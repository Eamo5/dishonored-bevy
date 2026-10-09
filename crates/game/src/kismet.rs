//! Runtime for the original level scripts (UE3 Kismet), cooked into `scene.kismet`.
//!
//! The graph is executed as in the original: events (level loaded, touch, use, death,
//! pickups, remote events, ...) activate output links; actions run when one of their inputs
//! is activated and activate their outputs (latent ones like `SeqAct_Delay` later). Ops with
//! no meaning here (sound, streaming, AI tuning, ...) pass straight through to their first
//! output so the flow they sit in carries on.
//!
//! The VM itself doesn't touch the world: actions queue [`Effect`]s that a separate system
//! applies (spawning NPCs, doors, visibility, teleports, objectives, fades, map travel).
//! `DH_KISMET_TRACE=1` logs every activation.

use crate::gameplay::{HudMessages, PlayerStats};
use crate::interact::{Door, Interaction, Pickup, Usable};
use crate::level::{LevelInfo, LevelInstance, LevelLight, LevelSpawnSet};
use crate::npc::{Alert, FromSpawner, Mode, Npc, NpcSpawned, SpawnRequest};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::Collider;
use dhcook::format::{KVal, Kismet as Graph};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

pub struct KismetPlugin;

impl Plugin for KismetPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(GameState::InGame), setup.after(LevelSpawnSet).after(crate::npc::spawn_npcs))
            .add_systems(
                Update,
                (world_events, more_events, destroyed_events, run, apply_effects, update_ui, talk_prompts, speakers_look)
                    .chain()
                    .after(crate::interact::use_focus)
                    .run_if(in_state(GameState::InGame).and_then(resource_exists::<Vm>)),
            );
    }
}

/// `SF_Global` flag the campaign sets when a real game starts (maps give their debug
/// loadout without it).
pub const REAL_GAME_STARTED: &str = "366014ed4b88bdb681e5d7a6cdb7791a";

/// A runtime value of a sequence variable.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Val {
    None,
    Bool(bool),
    Int(i32),
    Float(f32),
    Str(String),
    /// index into `kismet.actors`
    Actor(u32),
    /// a sequence op (events can be toggled through variables)
    Op(u32),
    Player,
    List(Vec<Val>),
    /// a vector (`SeqVar_Vector`), in the original's units and axes
    Vec3([f32; 3]),
}

impl Val {
    fn from_k(v: &KVal) -> Val {
        match v {
            KVal::Bool(b) => Val::Bool(*b),
            KVal::Int(i) => Val::Int(*i),
            KVal::Float(f) => Val::Float(*f),
            KVal::Str(s) => Val::Str(s.clone()),
            KVal::Vec3(v) => Val::Vec3(*v),
            KVal::Actor(a) => Val::Actor(*a),
            KVal::Op(o) => Val::Op(*o),
            KVal::Var(_) => Val::None,
            KVal::List(l) => Val::List(l.iter().map(Val::from_k).collect()),
        }
    }
    fn as_f32(&self) -> Option<f32> {
        match self {
            Val::Float(f) => Some(*f),
            Val::Int(i) => Some(*i as f32),
            Val::Bool(b) => Some(*b as i32 as f32),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Val::Bool(b) => Some(*b),
            Val::Int(i) => Some(*i != 0),
            Val::Float(f) => Some(*f != 0.0),
            _ => None,
        }
    }
    /// Flatten object lists.
    fn items(self) -> Vec<Val> {
        match self {
            Val::List(l) => l.into_iter().flat_map(|v| v.items()).collect(),
            Val::None => Vec::new(),
            v => vec![v],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DoorCmd {
    Open(bool),
    Close,
    Lock,
    Unlock,
}

/// The scripts' switches as the world reads them (kept from the VM's).
#[derive(Resource, Default)]
pub struct ScriptSwitches(pub Switches);

/// The scripts' switches over Corvo's means (saved with the game).
#[derive(Clone, Default, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Switches {
    /// Blood Thirst's adrenaline off (`DisSeqAct_AdrenalineToggle`: the Prison's opening)
    pub adrenaline_off: bool,
    /// the journal shut (`DisSeqAct_ToggleJournal`: the Tower's opening)
    pub journal_off: bool,
    /// no choking from behind (`DisSeqAct_ToggleChoke`)
    pub choke_off: bool,
    /// the game's own tutorials held back (`DisSeqAct_EnableSystemicTutorials`: the dream)
    pub systemic_off: bool,
    /// achievements not judged (`DisSeqAct_ToggleAchievementEval`)
    pub achievements_off: bool,
}

/// A choice put to the player by a conversation (its speaker's actor in the low bits) rather
/// than by a script op.
pub const CONV_CHOICE: u32 = 0x8000_0000;

/// Who walked into a trigger volume: Corvo (or Corvo inside a creature, UE3's possession
/// proxy pawn), or a character.
#[derive(Clone, Copy, Debug)]
enum Toucher {
    Player { proxy: bool },
    Npc,
}

/// World changes requested by the scripts.
#[derive(Clone, Debug)]
pub enum Effect {
    /// the whole HUD shown (`Some(true)`), hidden, or toggled (`SeqAct_ToggleHUD`)
    HudAll(Option<bool>),
    Spawn(u32),
    /// a movie played over everything (or stopped: None): the Tower's title card, the credits
    Movie(Option<String>),
    /// the scripts' post-processing on (its op) or off, over seconds
    /// (`DisSeqAct_UberPostProcess`)
    Post(Option<u32>, f32),
    /// hide (true) / show (false) / toggle (None)
    Hide(Val, Option<bool>),
    Destroy(Val),
    Light(Val, Option<bool>),
    /// put somewhere, facing so (`None`: as it faces)
    Teleport(Val, Vec3, f32),
    /// its velocity set (m/s: `SeqAct_SetVelocity`)
    Velocity(Val, Vec3),
    /// put somewhere, keeping its facing unless a yaw is given (`SeqAct_SetLocation`)
    Place(Val, Option<Vec3>, Option<f32>),
    /// a map started outright (`SeqAct_ConsoleCommand` "start" / "open": the maps' own way on
    /// when played without the campaign)
    StartMap(String),
    Door(Val, DoorCmd),
    Damage(Val, f32),
    Message(String),
    /// a tutorial (`m_fTutorialDuration`: it stays longer)
    Tutorial(String),
    /// an objective added, updated, completed or failed (the objectives popup)
    Objective(crate::objnotify::ObjectiveNotice),
    /// a choice for the player (`DisSeqAct_DialogScriptedChoice`): the op, its options
    Choice(u32, Vec<(usize, String)>),
    Location(String),
    /// fade the screen to this opacity
    Fade { to: f32, time: f32 },
    GoTo(Val, Vec3),
    Travel(String),
    /// subtitle line and how long it shows
    Subtitle(String, f32),
    /// a streamed sublevel came in (true) or went out
    Stream(String, bool),
    /// a conversation's line: text, seconds, the speaking actor (named when a character)
    Line(String, f32, Option<u32>),
    /// back from black (a skipped matinee's director fade)
    FadeIn(f32),
    /// post a Wwise event (at an actor)
    Sound(String, Option<Val>),
    /// a conversation's voice (at the speaking actor)
    Voice(String, Option<Val>),
    /// start / stop a placed ambient sound actor
    Ambient(Val, bool),
    /// give (level > 0) or take (0) one of Corvo's powers
    Power(String, u8),
    /// open a store (its tweak object's name)
    Store(String),
    /// save the game (`DisSeqAct_AutoSave`, at mission starts and milestones)
    AutoSave,
    /// a key onto Corvo's ring (`DisSeqAct_AddKey`: its `m_Name`)
    Key(String),
    /// an abstract item (note, invitation) taken from Corvo: its object path
    TakeItem(String),
    /// a part of the HUD shown (true) or hidden
    Hud(String, bool),
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OpState {
    disabled: bool,
    fired: u32,
    init: bool,
    gate_open: bool,
    index: i32,
    and_seen: u64,
    paused: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
enum Latent {
    Delay { op: u32, left: f32 },
    /// a playing matinee: position, length, direction x play rate, cooked matinee and the
    /// actors bound to each of its groups
    Interp { op: u32, t: f32, len: f32, rate: f32, looping: bool, paused: bool, matinee: Option<u32>, binds: Vec<(u32, Vec<u32>)> },
    Spawn { op: u32, left: f32 },
    After { op: u32, out: usize, left: f32 },
    /// a conversation started on a dialogue actor (input name)
    Dialog { actor: u32, input: String, left: f32 },
    /// a door the scripts move, telling its scripts when it has swung (opened?)
    DoorSwung { actor: u32, opened: bool, left: f32 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum TaskState {
    Inactive,
    Active,
    Completed,
    Failed,
}

/// The level script interpreter.
/// The scripts' runtime state in a save game.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct VmSave {
    ops: usize,
    vals: Vec<Val>,
    st: Vec<OpState>,
    queue: Vec<(u32, u32)>,
    timers: Vec<(f32, u32, u32)>,
    latent: Vec<Latent>,
    flags: HashMap<String, bool>,
    objectives: Vec<String>,
    tasks: HashMap<String, (TaskState, bool)>,
    travel: Option<String>,
    convs: Vec<(u32, usize, usize, usize, f32)>,
    time: f32,
    interp_pos: HashMap<u32, f32>,
    events_of: HashMap<u32, Vec<u32>>,
    touching: Vec<u32>,
    /// conversations already played (for those that play once)
    #[serde(default)]
    fired_convs: Vec<String>,
    /// streamed sublevels not loaded
    #[serde(default)]
    inert: Option<Vec<(u32, u32, String)>>,
    /// tasks' marker targets
    #[serde(default)]
    task_targets: HashMap<String, (u32, String)>,
    /// the scripts' switches (and the wheel's and tutorials')
    #[serde(default)]
    switches: Option<(Switches, bool, bool)>,
    /// music boxes the scripts force or forbid (by spawner)
    #[serde(default)]
    music_box: HashMap<u32, bool>,
    /// properties the scripts set on spawners (`SeqAct_ModifyProperty`)
    #[serde(default)]
    spawn_props: HashMap<u32, Vec<(String, String)>>,
    /// physics joints the scripts destroyed
    #[serde(default)]
    joints_broken: Vec<String>,
}

#[derive(Resource)]
pub struct Vm {
    pub g: Arc<Graph>,
    vals: Vec<Val>,
    st: Vec<OpState>,
    queue: VecDeque<(u32, u32)>,
    timers: Vec<(f32, u32, u32)>,
    latent: Vec<Latent>,
    remote: HashMap<String, Vec<u32>>,
    named: HashMap<String, Vec<u32>>,
    /// actor -> events it originates
    events_of: HashMap<u32, Vec<u32>>,
    /// number of distinct inputs linked into each op (for AND gates)
    linked_inputs: Vec<u32>,
    pub effects: Vec<Effect>,
    flags: HashMap<String, bool>,
    /// active objectives (path) and task states (path)
    pub objectives: Vec<String>,
    pub tasks: HashMap<String, (TaskState, bool)>,
    /// where each task's marker points (`DisSeqAct_UpdateTaskTarget`): actor, label
    pub task_targets: HashMap<String, (u32, String)>,
    pub travel: Option<String>,
    trace: bool,
    /// spawner index -> actor
    spawner_actor: HashMap<u32, u32>,
    pickup_actor: HashMap<u32, u32>,
    instance_actor: HashMap<u32, Vec<u32>>,
    /// trigger volumes of touch events: (actor, convex hulls)
    touch: Vec<(u32, Vec<Collider>)>,
    touching: HashSet<u32>,
    /// triggers the scripts attached to a character (`SeqAct_AttachToActor`): actor ->
    /// (its spawner, where the character was when it was attached, once known)
    pub touch_follow: HashMap<u32, (u32, Option<Vec3>)>,
    /// conversations being played: (speaker actor, dialogue tree, conversation, next step,
    /// time left)
    convs: Vec<(u32, usize, usize, usize, f32)>,
    /// dialogue trees by object path
    tree_by_path: HashMap<String, usize>,
    /// conversations played (object paths), sequential branches' turns, time limits' last
    /// passes
    fired_convs: HashSet<String>,
    branch_turn: HashMap<(usize, u32), u32>,
    node_time: HashMap<(usize, u32), f32>,
    /// speakers and the tree they answer with (for the player approaching / loitering),
    /// their positions and how long the player has lingered by each
    speaker_tree: HashMap<u32, usize>,
    pub speaker_pos: HashMap<u32, Vec3>,
    pub player_pos: Option<Vec3>,
    linger: HashMap<u32, f32>,
    hook_time: HashMap<(u32, u32), f32>,
    /// NPC states last frame (for death / alert events)
    npc_seen: HashMap<Entity, (Mode, Alert, f32, bool)>,
    /// how many of Corvo's noises each character had heard (`DisSeqEvent_PlayerHeard`)
    npc_heard: HashMap<Entity, u32>,
    /// Corvo is in a rat
    in_rat: bool,
    /// the actors whose destruction the scripts were told of (`SeqEvent_Destroyed`)
    destroyed: HashSet<u32>,
    player_combat: bool,
    player_crouch: bool,
    pub time: f32,
    /// matinee positions kept between plays (movers stay where they stopped)
    interp_pos: HashMap<u32, f32>,
    /// the campaign's scripts merged in: their first op and variable
    pub campaign: Option<(u32, u32)>,
    /// the campaign's chaos level (`DisSeqVar_DarknessLevel`) and this mission's share of it
    pub darkness: i32,
    pub mission_chaos: i32,
    /// `SeqAct_PrepareMapChange`'s map
    pending_map: Option<String>,
    pending_levels: Vec<String>,
    /// ops of streamed sublevels that aren't loaded: (first, end, level name)
    inert: Vec<(u32, u32, String)>,
    /// each package's ops and whether the scripts stream it
    level_ops: Vec<(String, bool, u32, u32)>,
    /// what the campaign scripts asked for
    pub campaign_fx: Vec<crate::campaign::CampaignFx>,
    /// a travel destination in no map here: travel there unless the campaign changes map
    pub goto_fallback: Option<(String, f32)>,
    /// rat spawners the scripts started (`DisSeqAct_ActivateRatSpawner`)
    pub rat_spawns: Vec<u32>,
    /// launchers the scripts fire (`DisSeqAct_ActivateProjectileLauncher`): `scene.traps`
    pub launches: Vec<u32>,
    /// what the scripts do to the characters and the world (`script_world`)
    pub ai_fx: Vec<crate::script_world::AiFx>,
    /// the spawner of the character Corvo possesses
    pub possessed: Option<u32>,
    /// usable objects the scripts move (`DisSeqAct_ActivateUsable`): actor, stage (else the next)
    pub usable_stages: Vec<(u32, Option<i32>)>,
    /// river krusts told to spit at something (`DisSeqAct_RiverKrustSpitAtTarget`): krust, where
    pub krust_spits: Vec<(u32, Vec3)>,
    /// screen effects the scripts started (`DisSeqAct_PostProcess` `m_Effect`: "Epp_Knocked",
    /// "Epp_Weepers"...), for the post-process graph (`ppgraph`)
    pub post_effects: Vec<String>,
    /// watch towers told to fire at something (`DisSeqAct_WatchTowerShootAtTarget`): the
    /// tower (actor name), where
    pub tower_shots: Vec<(String, Vec3)>,
    /// actors the scripts highlight (`DisSeqAct_Highlight`: the golden rim, `highlight`)
    pub highlights: HashSet<u32>,
    /// the light range the scripts set for Corvo's visibility (`DisSeqAct_SetPlayerVisSettings`;
    /// darkest, brightest), till they clear it
    pub player_vis: Option<[f32; 2]>,
    /// the HUD's target cards to show (`DisSeqAct_ShowTargetNotification`): name, portrait,
    /// assassinated (0) / neutralized / rescued / spared
    pub target_cards: Vec<(String, String, u8)>,
    /// characters whose awareness of Corvo shows though they're no enemy
    /// (`DisSeqAct_OverrideAwarenessDisplay`: Emily seeking him at hide-and-seek)
    pub awareness_shown: HashSet<u32>,
    /// the journal the scripts open, on a tab (`DisSeqAct_OpenJournal` `m_Tab`)
    pub open_journal: Option<String>,
    /// Corvo's things put away (`DisSeqAct_BackupAndClearInventory`: true, and whether his
    /// upgrades too) and given back (`DisSeqAct_RestoreInventoryFromBackup`: false)
    pub inventory_ops: Vec<(bool, bool)>,
    /// the journal closes seen, and Corvo's mana last frame (for their events)
    journal_viewed: u32,
    mana_was: Option<f32>,
    /// `DisSeqEvent_TaskActivated` ops that fired (their objective came up)
    task_events: HashSet<u32>,
    rat_bites: u32,
    /// objectives plus active tasks last seen (`DisSeqEvent_TaskActivated` looks again on a change)
    active_count: usize,
    /// where each spawner's character stands (for `SeqAct_GetDistance`)
    pub npc_pos: HashMap<u32, Vec3>,
    /// the character possessed last frame (`DisSeqEvent_Possessed` Started / Finished)
    possessed_was: Option<u32>,
    /// nav points with `DisSeqEvent_PatrolPointReached` (actor, where) and the characters at
    /// them now (actor, spawner)
    patrol_points: Option<Vec<(u32, Vec3)>>,
    at_points: HashSet<(u32, u32)>,
    /// `SeqEvent_LOS` ops Corvo is looking at
    looking: HashSet<u32>,
    /// usable objects the scripts lock (true) or unlock (`DisSeqAct_Lock`): actor
    pub usable_locks: Vec<(u32, bool)>,
    /// material parameters set (`SeqAct_SetMatInstScalarParam`): material instance path,
    /// parameter, value
    pub mat_params: Vec<(String, String, f32)>,
    /// materials put on things (`SeqAct_SetMaterial`): targets, scene material
    pub set_materials: Vec<(Vec<Val>, u32)>,
    /// `SeqAct_Timer`s running: when each started
    timer_start: HashMap<u32, f32>,
    /// Corvo's abstract items by `Group.Name` (`DisSeqAct_GetAbstractItemQuantity`), kept up
    /// to date by the world
    pub item_counts: HashMap<String, u32>,
    /// the scripts stop Corvo's active power (`DisSeqAct_CancelPlayerActivePower`)
    pub cancel_powers: bool,
    /// scripted bent time (`DisSeqAct_BendTime`): the world's dilation, or the end of it
    pub bend_time: Vec<Option<f32>>,
    /// characters to vanish once out of sight (`DisSeqAct_NPCMarkForVanish`): targets, op,
    /// marked (else cancelled)
    pub vanish: Vec<(Vec<Val>, u32, bool)>,
    /// Corvo's view held on something (`DisSeqAct_PlayerTrackTarget`): target, blend time
    pub player_track: Option<(Vec<Val>, f32)>,
    /// pickups hung on characters (`DisSeqAct_AttachPickup`): targets, pickups, attach (else
    /// detach)
    pub attach_pickups: Vec<(Vec<Val>, Vec<Val>, bool)>,
    /// Corvo is inside a creature (the possession proxy)
    player_proxy: bool,
    /// characters standing in trigger volumes (volume actor, spawner)
    npc_touching: HashSet<(u32, u32)>,
    /// volumes whose touch events characters can set off (`bPlayerOnly` false)
    npc_touch_actors: Option<Vec<u32>>,
    /// Corvo's health / mana set (`DisSeqAct_SetPlayerHealth` / `Mana`): percent of the most
    pub vitals: Vec<(Option<f32>, Option<f32>)>,
    /// elixirs given or taken (`DisSeqAct_ModifyElixirCount`): mana (else health), how many
    pub elixirs: Vec<(bool, i32)>,
    /// upgrades given (`DisSeqAct_GiveUpgrade`: tweak names) and keys taken (`DisSeqAct_RemoveKey`)
    pub upgrades: Vec<String>,
    pub keys_taken: Vec<String>,
    /// Corvo's ammunition changed (`DisSeqAct_ModifyAmmo`): add (0), set (1) or remove (2),
    /// and (type, amount)
    pub ammo_mods: Vec<(u8, Vec<(u8, i32)>)>,
    /// the quick-access wheel shut (`DisSeqAct_TogglePowerWheel`)
    pub wheel_off: bool,
    /// characters dressed differently (`DisSeqAct_NPCSetMaterials`): targets, op, set (else
    /// back to their own)
    pub npc_materials: Vec<(Vec<Val>, u32, bool)>,
    /// projectiles the scripts fire (`DisSeqAct_FireProjectile`): from, to, a flare
    pub projectiles: Vec<(Vec3, Vec3, bool)>,
    /// blasts the scripts set off (`DisSeqAct_TriggerExplosion`): where, and the op (its
    /// cooked blast)
    pub explosions: Vec<(Vec3, u32)>,
    /// the speaker whose tree is being walked (its dialogue outputs)
    walk_speaker: Option<u32>,
    /// characters speaking for a one-shot dialogue actor (`DisSeqAct_DialogInputs` Target with a
    /// DialogOneShot): talking to the character uses the one-shot's tree (the Empress)
    pawn_oneshot: HashMap<u32, u32>,
    /// doors open (their actors): swung clockwise (`DisSeqCond_IsDoorOpen`)
    door_open: HashMap<u32, bool>,
    /// characters' pawn tweaks by spawner (`DisSeqCond_CompareTweaks`)
    pub npc_pawn: HashMap<u32, String>,
    /// physics set on things (`SeqAct_SetPhysics`): targets, `newPhysics`
    pub set_physics: Vec<(Vec<Val>, String)>,
    /// possession put on characters (`DisSeqAct_OverridePossess`): targets, the op (else
    /// cleared), where Corvo comes out
    pub possess_overrides: Vec<(Vec<Val>, Option<u32>, Option<Vec3>)>,
    /// the rain box set (`DisSeqAct_SetRainEmitter`): op, emitter, drops, start delay
    pub rain: Option<(u32, Vec<Val>, u32, f32)>,
    /// tutorial messages held back (`DisSeqAct_ToggleTutorial`)
    pub tutorials_off: bool,
    /// items put in Corvo's hands (`DisSeqAct_EquipItemType`): targets, item class
    pub equip: Vec<(Vec<Val>, String)>,
    /// camera lens effects (`DisSeqAct_SpawnCameraLensEffect`): op, and to start (particle
    /// system, looping, life span) or else stop looping
    pub lens: Vec<(u32, Option<(u32, bool, f32)>)>,
    /// the scripts' switches over Corvo's means
    pub switches: Switches,
    /// limbs the scripts cut (`DisSeqAct_SeverLimb`): targets, the joint, with gore
    pub severs: Vec<(Vec<Val>, String, bool)>,
    /// music boxes the scripts force (true) or forbid (false), by spawner
    /// (`DisSeqAct_PlayMusicBox`; absent: as the Overseer wishes)
    pub music_box: HashMap<u32, bool>,
    /// properties the scripts set on spawners before they spawn (`SeqAct_ModifyProperty`:
    /// `m_bAwareOfPlayerUponStartup`), by spawner
    pub spawn_props: HashMap<u32, Vec<(String, String)>>,
    /// bone charm effects the scripts give the charms their factories make
    /// (`DisSeqAct_SetBoneCharmEffect`): where the charm stands, the effect's id
    pub charm_effects: Vec<(Vec3, i32)>,
    /// watch towers the scripts work (`DisSeqAct_WatchTower`): targets, the input (off, on,
    /// toggle, explore, alert, switch polarity), what to track
    pub tower_cmds: Vec<(Vec<Val>, u8, Vec<Val>)>,
    /// who the scripts hold to have done the damage to things (`SeqAct_SetDamageInstigator`)
    pub instigators: HashMap<u32, Val>,
    /// Corvo's statistics by the original's names (`ePlayerStat_*`), kept up to date by the
    /// world (`DisSeqAct_GetPlayerStat`), and those the scripts reset
    pub stat_values: HashMap<String, u32>,
    pub stat_resets: Vec<String>,
    /// velocities the scripts give things (`SeqAct_SetVelocity`): the actor, m/s
    pub velocities: Vec<(u32, Vec3)>,
    /// placed particle systems' actors (`scene.particles` -> actor)
    particle_actor: HashMap<u32, u32>,
    /// characters the scripts draw in the foreground group (`DisSeqAct_OutsiderConfig`: the
    /// Outsider, never cut by the walls about him), by spawner
    pub foreground: Vec<u32>,
    /// the map's state to keep for a return to it (`DisSeqAct_SaveLevelState`): partial
    pub level_state_save: Option<bool>,
    /// levels whose kept state the scripts drop (`DisSeqAct_DiscardLevelState`; "*": all,
    /// `DisSeqAct_DiscardAllLevelStates`)
    pub level_state_discards: Vec<String>,
    /// forbidden zones' factions set (or cleared) by the scripts
    /// (`DisSeqAct_ForbiddenZoneOverride`): zones, owners, forbidden, set
    pub zone_overrides: Vec<(Vec<Val>, Vec<Val>, Vec<Val>, bool)>,
    /// characters told to protect the neutral or not (`DisSeqAct_AIProtectNeutralsOverride`)
    pub protect_overrides: Vec<(Vec<Val>, Option<bool>)>,
    /// characters' flags: 0 ignoring physics' blows (`DisSeqAct_NPCIgnoreRBDamages`), 1 no
    /// teleport back onto the navmesh (`DisSeqAct_NPCDisableTeleportOnNavmesh`)
    pub npc_flags: Vec<(Vec<Val>, u8, bool)>,
    /// ambushes (`DisSeqAct_AIAmbush`): who, at which point, start (else stop)
    pub ambush_cmds: Vec<(Vec<Val>, Vec<Val>, bool)>,
    /// the damage each `SeqEvent_TakeDamage` has taken towards its threshold
    damage_acc: HashMap<u32, f32>,
    /// physics joints the scripts destroyed (`SeqAct_Destroy` on an `RB_ConstraintActor`):
    /// what hung by them falls
    pub joints_broken: std::collections::HashSet<String>,
    /// damage the scripts deal to things (`SeqAct_ModifyHealth` on a prop): its instances, how much
    pub prop_damage: Vec<(Vec<u32>, f32)>,
    /// doorways the scripts muffle (`DisSeqAct_SetAudioOcclusion`): their names, for Corvo, for
    /// the AI
    pub portal_occlusion: Vec<(Vec<String>, f32, f32)>,
    /// whale oil tanks the scripts fill (`DisSeqAct_RefillWhaleOilBattery`): their instances,
    /// to what percentage
    pub tank_refills: Vec<(Vec<u32>, f32)>,
    /// Dunwall City Trials: what the scripts ask of the challenge (`challenge.rs`), whether it
    /// is in expert mode, whether this map's clockwork doll was found before
    pub challenge_fx: Vec<crate::challenge::ChallengeFx>,
    pub expert: bool,
    pub doll_found: bool,
    /// the damage type of the last death the challenge saw (`DisSeqAct_DLC05_GetDeathInfo`)
    pub last_death: Option<String>,
    /// movers' navigation meshes the scripts join to the rest (true) or part from it
    /// (`ArkSeqAct_ChangePylonConnection`): the pylon's actor
    pub pylon_links: Vec<(String, bool)>,
    /// physics bursts the scripts set off (`RB_RadialImpulseActor` toggled): where, its
    /// impulse (reach, strength, speed change, falloff)
    pub impulses: Vec<(Vec3, [f32; 4])>,
    /// achievements the scripts judge (`DisSeqAct_EvalAchievement`), and whether the mission's
    /// statistics reset after (`m_bResetStats`)
    pub achievement_evals: Vec<(String, bool)>,
    /// the sounds factories made (`ActorFactoryAkAmbientSound`), by the actor standing for
    /// them (its spawn point), and the emitters made for them
    factory_sounds: HashMap<u32, String>,
    factory_ambients: HashMap<u32, usize>,
    /// characters struck by a typed blow lately (when): their health's drop was that blow
    pub recent_hits: HashMap<Entity, f32>,
}

impl Vm {
    fn new(g: Arc<Graph>, scene_volumes: &[dhcook::format::Volume]) -> Vm {
        let n_ops = g.ops.len();
        let mut vm = Vm {
            vals: g.vars.iter().map(initial_value).collect(),
            st: vec![OpState::default(); n_ops],
            queue: VecDeque::new(),
            timers: Vec::new(),
            latent: Vec::new(),
            remote: HashMap::new(),
            named: HashMap::new(),
            events_of: HashMap::new(),
            linked_inputs: vec![0; n_ops],
            effects: Vec::new(),
            flags: HashMap::new(),
            objectives: Vec::new(),
            tasks: HashMap::new(),
            task_targets: HashMap::new(),
            travel: None,
            trace: std::env::var("DH_KISMET_TRACE").is_ok(),
            spawner_actor: HashMap::new(),
            pickup_actor: HashMap::new(),
            instance_actor: HashMap::new(),
            touch: Vec::new(),
            touching: HashSet::new(),
            touch_follow: HashMap::new(),
            convs: Vec::new(),
            tree_by_path: g.dialog_trees.iter().enumerate().map(|(i, t)| (t.path.clone(), i)).collect(),
            fired_convs: HashSet::new(),
            branch_turn: HashMap::new(),
            node_time: HashMap::new(),
            // one-shot dialogue actors answer from where they stand
            speaker_tree: g.dialogs.iter().map(|d| (d.actor, d.tree as usize)).collect(),
            speaker_pos: g.dialogs.iter().filter_map(|d| g.actors.get(d.actor as usize).map(|a| (d.actor, Vec3::from(a.position)))).collect(),
            player_pos: None,
            linger: HashMap::new(),
            hook_time: HashMap::new(),
            npc_seen: HashMap::new(),
            npc_heard: HashMap::new(),
            in_rat: false,
            destroyed: HashSet::new(),
            player_combat: false,
            player_crouch: false,
            time: 0.0,
            interp_pos: HashMap::new(),
            campaign: None,
            darkness: 0,
            mission_chaos: 0,
            pending_map: None,
            pending_levels: Vec::new(),
            inert: Vec::new(),
            level_ops: Vec::new(),
            campaign_fx: Vec::new(),
            goto_fallback: None,
            rat_spawns: Vec::new(),
            launches: Vec::new(),
            ai_fx: Vec::new(),
            possessed: None,
            usable_stages: Vec::new(),
            krust_spits: Vec::new(),
            post_effects: Vec::new(),
            tower_shots: Vec::new(),
            highlights: HashSet::new(),
            player_vis: None,
            target_cards: Vec::new(),
            awareness_shown: HashSet::new(),
            open_journal: None,
            inventory_ops: Vec::new(),
            journal_viewed: 0,
            mana_was: None,
            task_events: HashSet::new(),
            rat_bites: 0,
            active_count: 0,
            npc_pos: HashMap::new(),
            possessed_was: None,
            patrol_points: None,
            at_points: HashSet::new(),
            looking: HashSet::new(),
            usable_locks: Vec::new(),
            mat_params: Vec::new(),
            set_materials: Vec::new(),
            timer_start: HashMap::new(),
            item_counts: HashMap::new(),
            cancel_powers: false,
            bend_time: Vec::new(),
            vanish: Vec::new(),
            player_track: None,
            lens: Vec::new(),
            equip: Vec::new(),
            tutorials_off: false,
            rain: None,
            possess_overrides: Vec::new(),
            set_physics: Vec::new(),
            player_proxy: false,
            npc_touching: HashSet::new(),
            npc_touch_actors: None,
            npc_pawn: HashMap::new(),
            door_open: HashMap::new(),
            // the characters answering for one-shot dialogue actors, from the scripts' links
            walk_speaker: None,
            pawn_oneshot: {
                let mut m = HashMap::new();
                let actor_of = |v: &u32| match g.vars.get(*v as usize).and_then(|v| v.props.get("ObjValue")) {
                    Some(KVal::Actor(a)) => Some(*a),
                    _ => None,
                };
                for op in g.ops.iter().filter(|o| o.class == "DisSeqAct_DialogInputs" && !o.props.contains_key("m_pDialogTree")) {
                    let shots: Vec<u32> = op.vars.iter().filter(|l| l.desc == "DialogOneShot").flat_map(|l| l.vars.iter()).filter_map(actor_of).collect();
                    for p in op.vars.iter().filter(|l| l.desc == "Target").flat_map(|l| l.vars.iter()).filter_map(actor_of) {
                        if let Some(o) = shots.first() {
                            m.insert(p, *o);
                        }
                    }
                }
                m
            },
            vitals: Vec::new(),
            elixirs: Vec::new(),
            upgrades: Vec::new(),
            keys_taken: Vec::new(),
            ammo_mods: Vec::new(),
            wheel_off: false,
            switches: Switches::default(),
            severs: Vec::new(),
            music_box: HashMap::new(),
            spawn_props: HashMap::new(),
            charm_effects: Vec::new(),
            tower_cmds: Vec::new(),
            instigators: HashMap::new(),
            stat_values: HashMap::new(),
            stat_resets: Vec::new(),
            velocities: Vec::new(),
            particle_actor: HashMap::new(),
            foreground: Vec::new(),
            level_state_save: None,
            level_state_discards: Vec::new(),
            zone_overrides: Vec::new(),
            protect_overrides: Vec::new(),
            npc_flags: Vec::new(),
            ambush_cmds: Vec::new(),
            damage_acc: HashMap::new(),
            joints_broken: Default::default(),
            prop_damage: Vec::new(),
            portal_occlusion: Vec::new(),
            tank_refills: Vec::new(),
            challenge_fx: Vec::new(),
            expert: false,
            doll_found: false,
            last_death: None,
            pylon_links: Vec::new(),
            impulses: Vec::new(),
            achievement_evals: Vec::new(),
            factory_sounds: HashMap::new(),
            factory_ambients: HashMap::new(),
            recent_hits: HashMap::new(),
            explosions: Vec::new(),
            projectiles: Vec::new(),
            npc_materials: Vec::new(),
            attach_pickups: Vec::new(),
            g: g.clone(),
        };
        for (i, op) in g.ops.iter().enumerate() {
            let i = i as u32;
            if op.props.get("bEnabled") == Some(&KVal::Bool(false)) {
                vm.st[i as usize].disabled = true;
            }
            if op.class == "SeqEvent_RemoteEvent" {
                if let Some(KVal::Str(n)) = op.props.get("EventName") {
                    vm.remote.entry(n.to_ascii_lowercase()).or_default().push(i);
                }
            }
            if let Some(KVal::Actor(a)) = op.props.get("Originator") {
                vm.events_of.entry(*a).or_default().push(i);
            }
            let mut seen = HashSet::new();
            for o in &op.outputs {
                for &(t, inp, _) in &o.links {
                    let _ = seen.insert((t, inp));
                }
            }
            for (t, _) in seen {
                if let Some(c) = vm.linked_inputs.get_mut(t as usize) {
                    *c += 1;
                }
            }
        }
        for (i, v) in g.vars.iter().enumerate() {
            if !v.name.is_empty() && !v.class.contains("Named") && !v.class.contains("External") {
                vm.named.entry(v.name.to_ascii_lowercase()).or_default().push(i as u32);
            }
        }
        for (i, a) in g.actors.iter().enumerate() {
            let i = i as u32;
            if let Some(s) = a.spawner {
                vm.spawner_actor.insert(s, i);
            }
            if let Some(p) = a.pickup {
                vm.pickup_actor.insert(p, i);
            }
            for &inst in &a.instances {
                vm.instance_actor.entry(inst).or_default().push(i);
            }
            for &p in &a.particles {
                vm.particle_actor.insert(p, i);
            }
            let touched = vm.events_of.get(&i).map(|evs| evs.iter().any(|&e| g.ops[e as usize].class.contains("Touch"))).unwrap_or(false);
            if let (true, Some(vol)) = (touched, a.volume.and_then(|v| scene_volumes.get(v as usize))) {
                let hulls: Vec<Collider> = vol
                    .hulls
                    .iter()
                    .filter_map(|h| Collider::convex_hull(&h.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>()))
                    .collect();
                if !hulls.is_empty() {
                    vm.touch.push((i, hulls));
                }
            }
        }
        vm
    }

    // ------------------------------------------------------------ properties and variables

    /// A story flag (GUID as the level scripts write it).
    pub fn flag(&self, f: &str) -> bool {
        *self.flags.get(f).unwrap_or(&false)
    }
    pub fn flags(&self) -> &HashMap<String, bool> {
        &self.flags
    }

    fn prop(&self, op: u32, k: &str) -> Option<&KVal> {
        self.g.ops[op as usize].props.get(k)
    }
    fn pf(&self, op: u32, k: &str) -> Option<f32> {
        match self.prop(op, k)? {
            KVal::Float(f) => Some(*f),
            KVal::Int(i) => Some(*i as f32),
            _ => None,
        }
    }
    fn pi(&self, op: u32, k: &str) -> Option<i32> {
        match self.prop(op, k)? {
            KVal::Int(i) => Some(*i),
            KVal::Float(f) => Some(*f as i32),
            _ => None,
        }
    }
    fn pb(&self, op: u32, k: &str) -> Option<bool> {
        match self.prop(op, k)? {
            KVal::Bool(b) => Some(*b),
            KVal::Int(i) => Some(*i != 0),
            _ => None,
        }
    }
    fn ps(&self, op: u32, k: &str) -> Option<String> {
        match self.prop(op, k)? {
            KVal::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The variables a (named / external) variable stands for.
    fn storage(&self, v: u32, depth: usize, out: &mut Vec<u32>) {
        let Some(var) = self.g.vars.get(v as usize) else { return };
        if depth > 8 {
            return;
        }
        if var.class.contains("SeqVar_Named") {
            if let Some(KVal::Str(n)) = var.props.get("FindVarName") {
                // the nearest enclosing sequence that has one by that name (each prefab's
                // distraction sequence has its own `DistractedPawn`), else any
                let all: Vec<u32> = self.named.get(&n.to_ascii_lowercase()).cloned().unwrap_or_default();
                let mut scope = var.parent;
                let mut found: Vec<u32> = Vec::new();
                let mut hops = 0;
                while let Some(sq) = scope {
                    found = all.iter().copied().filter(|w| self.g.vars.get(*w as usize).is_some_and(|x| x.parent == Some(sq))).collect();
                    if !found.is_empty() || hops > 16 {
                        break;
                    }
                    hops += 1;
                    scope = self.g.ops.get(sq as usize).and_then(|o| o.parent);
                }
                if found.is_empty() {
                    found = all;
                }
                for w in found {
                    self.storage(w, depth + 1, out);
                }
            }
        } else if var.class.contains("SeqVar_External") {
            let (Some(KVal::Str(label)), Some(parent)) = (var.props.get("VariableLabel"), var.parent) else { return };
            if let Some(op) = self.g.ops.get(parent as usize) {
                for l in &op.vars {
                    if l.desc.eq_ignore_ascii_case(label) {
                        for &w in &l.vars {
                            self.storage(w, depth + 1, out);
                        }
                    }
                }
            }
        } else {
            out.push(v);
        }
    }

    fn value(&self, v: u32) -> Val {
        let class = self.g.vars[v as usize].class.as_str();
        match class {
            "SeqVar_Player" | "DisSeqVar_PlayerPawn" => Val::Player,
            "DisSeqVar_DarknessLevel" => Val::Int(self.darkness),
            "SeqVar_RandomFloat" => {
                let p = &self.g.vars[v as usize].props;
                let get = |k: &str, d: f32| match p.get(k) {
                    Some(KVal::Float(f)) => *f,
                    _ => d,
                };
                let (lo, hi) = (get("Min", 0.0), get("Max", 1.0));
                Val::Float(lo + (hi - lo) * rand::random::<f32>())
            }
            "SeqVar_RandomInt" => {
                let p = &self.g.vars[v as usize].props;
                let get = |k: &str, d: i32| match p.get(k) {
                    Some(KVal::Int(i)) => *i,
                    _ => d,
                };
                let (lo, hi) = (get("Min", 0), get("Max", 100));
                Val::Int(lo + (rand::random::<u32>() % ((hi - lo).max(0) as u32 + 1)) as i32)
            }
            _ => self.vals[v as usize].clone(),
        }
    }

    fn link_vars(&self, op: u32, desc: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for l in &self.g.ops[op as usize].vars {
            if l.desc.eq_ignore_ascii_case(desc) {
                for &v in &l.vars {
                    self.storage(v, 0, &mut out);
                }
            }
        }
        out
    }

    /// Values of an op's variable link (object lists flattened).
    fn read(&self, op: u32, desc: &str) -> Vec<Val> {
        self.link_vars(op, desc).into_iter().flat_map(|v| self.value(v).items()).collect()
    }
    fn read_f(&self, op: u32, desc: &str, prop: &str, default: f32) -> f32 {
        self.read(op, desc).iter().find_map(|v| v.as_f32()).or_else(|| self.pf(op, prop)).unwrap_or(default)
    }
    fn read_b(&self, op: u32, desc: &str, prop: &str, default: bool) -> bool {
        self.read(op, desc).iter().find_map(|v| v.as_bool()).or_else(|| self.pb(op, prop)).unwrap_or(default)
    }
    /// Write an op's linked variables (what the world measures: a challenge timer's time).
    pub fn write_var(&mut self, op: u32, desc: &str, v: Val) {
        self.write(op, desc, v);
    }

    fn write(&mut self, op: u32, desc: &str, v: Val) {
        for w in self.link_vars(op, desc) {
            self.vals[w as usize] = v.clone();
        }
    }

    // ------------------------------------------------------------ activation

    fn output_index(&self, op: u32, desc: &str) -> Option<usize> {
        self.g.ops[op as usize].outputs.iter().position(|o| o.desc.eq_ignore_ascii_case(desc))
    }

    /// The player's answer to a choice: carry on down that output.
    pub fn choose(&mut self, op: u32, out: usize) {
        if op & CONV_CHOICE != 0 {
            self.answer_conversation(op & !CONV_CHOICE, out);
        } else {
            self.fire(op, out);
        }
    }

    /// Activate the links of an op's output.
    fn fire(&mut self, op: u32, out: usize) {
        let Some(o) = self.g.ops[op as usize].outputs.get(out) else { return };
        for &(t, i, d) in &o.links {
            if d > 0.0 {
                self.timers.push((d, t, i));
            } else {
                self.queue.push_back((t, i));
            }
        }
    }

    fn fire_desc(&mut self, op: u32, desc: &str) {
        if let Some(i) = self.output_index(op, desc) {
            self.fire(op, i);
        }
    }

    /// Trigger an event op (respects enabling and its trigger count).
    fn event(&mut self, ev: u32, out: usize, instigator: Option<Val>) -> bool {
        if self.is_inert(ev) {
            return false;
        }
        let max = self.pi(ev, "MaxTriggerCount").unwrap_or(1);
        let s = &mut self.st[ev as usize];
        if s.disabled || max > 0 && s.fired >= max as u32 {
            return false;
        }
        s.fired += 1;
        if self.trace {
            let op = &self.g.ops[ev as usize];
            info!("kismet: event {}#{ev} ({}) -> {}", op.name, op.class, op.outputs.get(out).map(|o| o.desc.as_str()).unwrap_or("?"));
        }
        if let Some(v) = instigator {
            self.write(ev, "Instigator", v);
        }
        self.fire(ev, out);
        true
    }

    /// Where a script value is: Corvo, a spawner's character, an actor.
    fn pos_of(&self, v: &Val) -> Option<Vec3> {
        match v {
            Val::Player => self.player_pos,
            Val::Actor(a) => {
                let ka = self.g.actors.get(*a as usize)?;
                ka.spawner.and_then(|s| self.npc_pos.get(&s).copied()).or(Some(Vec3::from(ka.position)))
            }
            Val::List(l) => l.iter().find_map(|x| self.pos_of(x)),
            _ => None,
        }
    }

    /// A character (its spawner) began a behaviour (`DisSeqEvent_BehaviorStarted` m_pBehavior:
    /// `DisBehaviorPanic`...): the scripts listening hear it, with it as the instigator.
    pub fn behavior_started(&mut self, spawner: u32, behavior: &str) {
        let Some(&a) = self.spawner_actor.get(&spawner) else { return };
        for ev in self.events_of.get(&a).cloned().unwrap_or_default() {
            if self.g.ops[ev as usize].class != "DisSeqEvent_BehaviorStarted" {
                continue;
            }
            let want = self.ps(ev, "m_pBehavior").unwrap_or_default();
            if want.rsplit('.').next().is_some_and(|w| w.eq_ignore_ascii_case(behavior)) {
                self.event(ev, 0, Some(Val::Actor(a)));
            }
        }
    }

    /// Whether a touch event answers this toucher: characters only where `bPlayerOnly` is off,
    /// and only the kinds its `ClassProximityTypes` names (the possession proxy: a rat's or a
    /// fish's ways), none its `IgnoredClassProximityTypes` names.
    fn touch_accepts(&self, ev: u32, who: Toucher) -> bool {
        let classes = |k: &str| -> Vec<String> {
            match self.prop(ev, k) {
                Some(KVal::List(l)) => l.iter().filter_map(|v| if let KVal::Str(s) = v { Some(s.rsplit('.').next().unwrap_or(s).to_ascii_lowercase()) } else { None }).collect(),
                _ => Vec::new(),
            }
        };
        let mine: &[&str] = match who {
            Toucher::Player { proxy: false } => &["dishonoredplayerpawn", "dishonoredpawn", "pawn", "actor"],
            Toucher::Player { proxy: true } => &["dispossessionproxypawn", "dishonoredplayerpawn", "dishonoredpawn", "pawn", "actor"],
            Toucher::Npc => &["dishonorednpcpawn", "dishonoredpawn", "pawn", "actor"],
        };
        if matches!(who, Toucher::Npc) && self.pb(ev, "bPlayerOnly").unwrap_or(true) {
            return false;
        }
        let only = classes("ClassProximityTypes");
        if !only.is_empty() && !only.iter().any(|c| mine.contains(&c.as_str())) {
            return false;
        }
        !classes("IgnoredClassProximityTypes").iter().any(|c| mine.contains(&c.as_str()))
    }

    /// A volume touched (0) or left (1) by someone.
    fn touch_event(&mut self, actor: u32, out: usize, who: Toucher, instigator: Val) {
        for ev in self.events_of.get(&actor).cloned().unwrap_or_default() {
            if matches!(self.g.ops[ev as usize].class.as_str(), "SeqEvent_Touch" | "DisSeqEvent_Touch") && self.touch_accepts(ev, who) {
                self.event(ev, out, Some(instigator.clone()));
            }
        }
    }

    /// Turn an event on (0), off (1) or over. A touch event turned on while Corvo already
    /// stands in its volume fires at once (UE3's `bForceOverlapping`, on by default): Emily
    /// sets off for the gazebo when its triggers come on around him.
    fn toggle_event(&mut self, ev: u32, input: u32) {
        let Some(s) = self.st.get_mut(ev as usize) else { return };
        let was = s.disabled;
        s.disabled = match input {
            0 => false,
            1 => true,
            _ => !s.disabled,
        };
        if !(was && !s.disabled) {
            return;
        }
        let class = self.g.ops[ev as usize].class.as_str();
        if !matches!(class, "SeqEvent_Touch" | "DisSeqEvent_Touch") || self.pb(ev, "bForceOverlapping") == Some(false) {
            return;
        }
        if let Some(KVal::Actor(a)) = self.prop(ev, "Originator").cloned() {
            if self.touching.contains(&a) && self.touch_accepts(ev, Toucher::Player { proxy: self.player_proxy }) {
                self.event(ev, 0, Some(Val::Player));
            }
            let npcs: Vec<u32> = self.npc_touching.iter().filter(|(v, _)| *v == a).map(|(_, s)| *s).collect();
            for s in npcs {
                if self.touch_accepts(ev, Toucher::Npc) {
                    let who = self.spawner_actor.get(&s).map(|x| Val::Actor(*x));
                    self.event(ev, 0, who);
                }
            }
        }
    }

    /// Fire one of an op's outputs from outside the scripts (a character vanished).
    pub fn signal(&mut self, op: u32, out: usize) {
        self.fire(op, out);
    }

    /// The actors speaking now.
    pub fn speakers(&self) -> impl Iterator<Item = u32> + '_ {
        self.convs.iter().map(|c| c.0)
    }

    /// Whether the character `actor` answers being talked to (its tree's "player used" hook).
    pub fn talkable(&self, actor: u32) -> bool {
        let actor = self.pawn_oneshot.get(&actor).copied().filter(|o| self.speaker_tree.contains_key(o) && !self.speaker_tree.contains_key(&actor)).unwrap_or(actor);
        let Some(t) = self.speaker_tree.get(&actor).and_then(|t| self.g.dialog_trees.get(*t)) else { return false };
        !self.convs.iter().any(|c| c.0 == actor)
            && t.nodes.iter().any(|n| matches!(&n.kind, dhcook::format::KDialogNodeKind::Hook { hook, .. } if hook == "DDH_PLAYER_USED") && n.outs.iter().any(|o| *o >= 0))
    }

    /// The player talks to `actor`: its tree's "player used" hook picks what it says.
    pub fn talk(&mut self, actor: u32) -> bool {
        let actor = self.pawn_oneshot.get(&actor).copied().filter(|o| self.speaker_tree.contains_key(o) && !self.speaker_tree.contains_key(&actor)).unwrap_or(actor);
        let Some(tree) = self.speaker_tree.get(&actor).copied() else { return false };
        let g = self.g.clone();
        let Some(t) = g.dialog_trees.get(tree) else { return false };
        self.walk_speaker = Some(actor);
        for (ni, n) in t.nodes.iter().enumerate() {
            if matches!(&n.kind, dhcook::format::KDialogNodeKind::Hook { hook, .. } if hook == "DDH_PLAYER_USED") {
                if let Some(c) = self.walk_tree(tree, ni as i32, 0) {
                    self.start_conversation(actor, tree, c);
                    return true;
                }
                if self.trace {
                    let path: Vec<String> = self.trace_walk(tree, ni as i32, 0);
                    info!("kismet: talk to {} ({}): nothing to say along {:?}", actor, t.path, path);
                }
            }
        }
        false
    }

    /// Whether `actor` has a use event that can still fire (characters the player can talk to).
    pub fn usable(&self, actor: u32) -> bool {
        self.events_of.get(&actor).is_some_and(|evs| {
            evs.iter().any(|&ev| {
                let op = &self.g.ops[ev as usize];
                let max = self.pi(ev, "MaxTriggerCount").unwrap_or(1);
                let s = &self.st[ev as usize];
                matches!(op.class.as_str(), "DisSeqEvent_Used" | "SeqEvent_Used") && !s.disabled && (max <= 0 || s.fired < max as u32)
            })
        })
    }

    /// Fire the events of class `classes` that `actor` originates, on output `out`.
    pub fn actor_event(&mut self, actor: u32, classes: &[&str], out: OutSel, instigator: Option<Val>) {
        let evs = self.events_of.get(&actor).cloned().unwrap_or_default();
        for ev in evs {
            let class = self.g.ops[ev as usize].class.clone();
            if !classes.iter().any(|c| class == *c) {
                continue;
            }
            let idx = match out {
                OutSel::Index(i) => Some(i),
                OutSel::Desc(d) => self.output_index(ev, d),
            };
            if let Some(i) = idx {
                self.event(ev, i, instigator.clone());
            }
        }
    }

    /// Something took damage (UE3 `SeqEvent_TakeDamage.HandleDamage`): each of its damage
    /// events taking that type (one of its `DamageTypes`, else none of its
    /// `IgnoreDamageTypes`, by class lineage) and at least its `MinDamageAmount` adds it up,
    /// firing once the sum reaches its `DamageThreshold` (100) and adding the sum to its
    /// "Damage Taken". Only Corvo's blows count where `bPlayerOnly` (the default).
    pub fn take_damage(&mut self, actor: u32, class: &str, amount: f32, by_player: bool) {
        // (whoever the scripts made answerable for its damage: Corvo, for a PA speaker he
        // shot down and its fall, `SeqAct_SetDamageInstigator`)
        let by_player = by_player || matches!(self.instigators.get(&actor), Some(Val::Player));
        for ev in self.events_of.get(&actor).cloned().unwrap_or_default() {
            let op = &self.g.ops[ev as usize];
            if op.class != "SeqEvent_TakeDamage" || self.is_inert(ev) || self.st[ev as usize].disabled {
                continue;
            }
            if amount < self.pf(ev, "MinDamageAmount").unwrap_or(0.0) || !by_player && self.pb(ev, "bPlayerOnly").unwrap_or(true) {
                continue;
            }
            let types = |k: &str| -> Vec<&str> {
                match op.props.get(k) {
                    Some(KVal::List(l)) => l.iter().filter_map(|v| if let KVal::Str(s) = v { s.rsplit('.').next() } else { None }).collect(),
                    _ => Vec::new(),
                }
            };
            let allow = types("DamageTypes");
            let valid = if allow.is_empty() {
                !types("IgnoreDamageTypes").iter().any(|t| crate::worlddamage::is_a(class, t))
            } else {
                allow.iter().any(|t| crate::worlddamage::is_a(class, t))
            };
            if !valid {
                continue;
            }
            let sum = *self.damage_acc.entry(ev).and_modify(|d| *d += amount).or_insert(amount);
            if sum >= self.pf(ev, "DamageThreshold").unwrap_or(100.0) && self.event(ev, 0, by_player.then_some(Val::Player)) {
                let taken = self.read(ev, "Damage Taken").iter().find_map(|v| v.as_f32()).unwrap_or(0.0);
                self.write(ev, "Damage Taken", Val::Float(taken + sum));
                self.damage_acc.insert(ev, 0.0);
            }
            if std::env::var("DH_DAMAGE_LOG").is_ok() {
                info!("damage: {} {class} {amount:.1} -> {} ({sum:.1} of {:.1})", self.g.actors.get(actor as usize).map(|a| a.name.as_str()).unwrap_or("?"), self.g.ops[ev as usize].name, self.pf(ev, "DamageThreshold").unwrap_or(100.0));
            }
        }
    }

    /// The actors with damage events (`SeqEvent_TakeDamage`).
    pub fn damage_listeners(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self.events_of.iter().filter(|(_, evs)| evs.iter().any(|&e| self.g.ops[e as usize].class == "SeqEvent_TakeDamage")).map(|(&a, _)| a).collect();
        out.sort_unstable();
        out
    }
    pub fn actor_of_spawner(&self, spawner: u32) -> Option<u32> {
        self.spawner_actor.get(&spawner).copied()
    }

    /// Corvo used something: the scripts listening to everything he uses
    /// (`DisSeqEvent_Interact` without an originator: the Prison's "Take a weapon") hear
    /// what.
    fn interacted(&mut self, actor: u32) {
        let evs: Vec<u32> = self.g.ops.iter().enumerate().filter(|(_, o)| o.class == "DisSeqEvent_Interact" && !o.props.contains_key("Originator")).map(|(i, _)| i as u32).collect();
        for ev in evs {
            self.write(ev, "InteractedObject (Output)", Val::Actor(actor));
            self.event(ev, 0, Some(Val::Player));
        }
    }

    /// Run queued activations (bounded, in case of loops).
    fn pump(&mut self) {
        let mut steps = 0;
        while let Some((op, input)) = self.queue.pop_front() {
            steps += 1;
            if steps > 20000 {
                warn!("kismet: too many activations in one frame, dropping {}", self.queue.len());
                self.queue.clear();
                break;
            }
            if self.is_inert(op) {
                continue;
            }
            self.exec(op, input);
        }
    }

    /// An op of a streamed sublevel that isn't loaded.
    /// A streamed level that's out (its scripts asleep).
    pub fn level_out(&self, name: &str) -> bool {
        self.inert.iter().any(|(_, _, n)| n.eq_ignore_ascii_case(name))
    }

    fn is_inert(&self, op: u32) -> bool {
        self.inert.iter().any(|(a, b, _)| op >= *a && op < *b)
    }

    /// Stream a sublevel in: its scripts wake (their level-loaded events fire) and its
    /// characters appear.
    pub fn load_level(&mut self, name: &str) {
        let Some(i) = self.inert.iter().position(|(_, _, n)| n.eq_ignore_ascii_case(name)) else { return };
        let (a, b, n) = self.inert.remove(i);
        info!("kismet: level {n} streams in");
        for op in a..b {
            if self.g.ops[op as usize].class == "SeqEvent_LevelLoaded" {
                self.event(op, 0, None);
                self.st[op as usize].fired = 0;
                self.event(op, 1, None);
            }
        }
        self.effects.push(Effect::Stream(n, true));
    }

    /// Stream a sublevel out.
    pub fn unload_level(&mut self, name: &str) {
        if self.inert.iter().any(|(_, _, n)| n.eq_ignore_ascii_case(name)) {
            return;
        }
        let Some((n, _, a, b)) = self.level_ops.iter().find(|(n, s, _, _)| *s && n.eq_ignore_ascii_case(name)).cloned() else { return };
        info!("kismet: level {n} streams out");
        self.inert.push((a, b, n.clone()));
        self.effects.push(Effect::Stream(n, false));
    }

    fn exec(&mut self, op: u32, input: u32) {
        let Some(o) = self.g.ops.get(op as usize) else { return };
        let class = o.class.clone();
        if self.trace {
            info!("kismet: {}#{op} ({class}) <- {}", o.name, o.inputs.get(input as usize).map(|i| i.desc.as_str()).unwrap_or("?"));
        }
        let g = self.g.clone();
        let opd = &g.ops[op as usize];
        match class.as_str() {
            // ---------------------------------------------------- structure
            "Sequence" | "PrefabSequence" | "PrefabSequenceContainer" => {
                let Some(inp) = opd.inputs.get(input as usize) else { return };
                let ev = inp.linked.or_else(|| {
                    g.ops.iter().position(|e| {
                        e.class == "SeqEvent_SequenceActivated"
                            && e.parent == Some(op)
                            && matches!(e.props.get("InputLabel"), Some(KVal::Str(l)) if l.eq_ignore_ascii_case(&inp.desc))
                    })
                    .map(|i| i as u32)
                });
                if let Some(ev) = ev {
                    self.event(ev, 0, None);
                }
            }
            "SeqAct_FinishSequence" => {
                let Some(parent) = opd.parent else { return };
                let label = self.ps(op, "OutputLabel").unwrap_or_default();
                let outs = &g.ops[parent as usize].outputs;
                let idx = outs.iter().position(|o| o.linked == Some(op)).or_else(|| outs.iter().position(|o| o.desc.eq_ignore_ascii_case(&label)));
                if let Some(i) = idx {
                    self.fire(parent, i);
                }
            }
            "SeqAct_ActivateRemoteEvent" => {
                // (what it names as the instigator, the events take as theirs: a challenge's
                // spawn point for the wave's spawn)
                let instigator = self.read(op, "Instigator").into_iter().find(|v| !matches!(v, Val::None));
                if let Some(name) = self.ps(op, "EventName") {
                    for ev in self.remote.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default() {
                        self.event(ev, 0, instigator.clone());
                    }
                }
                self.fire(op, 0);
            }

            // ---------------------------------------------------- flow control
            "SeqAct_Delay" => match input {
                0 => {
                    let d = self.read_f(op, "Duration", "Duration", 1.0).max(0.0);
                    self.latent.retain(|l| !matches!(l, Latent::Delay { op: o, .. } if *o == op));
                    self.st[op as usize].paused = false;
                    self.latent.push(Latent::Delay { op, left: d });
                }
                1 => {
                    self.latent.retain(|l| !matches!(l, Latent::Delay { op: o, .. } if *o == op));
                    self.fire(op, 1);
                }
                _ => {
                    let s = &mut self.st[op as usize];
                    s.paused = !s.paused;
                }
            },
            "SeqAct_Gate" => {
                let open_default = self.pb(op, "bOpen").unwrap_or(true);
                let auto_close = self.pi(op, "AutoCloseCount").unwrap_or(0);
                let s = &mut self.st[op as usize];
                if !s.init {
                    s.init = true;
                    s.gate_open = open_default;
                }
                match input {
                    0 => {
                        if s.gate_open {
                            s.index += 1;
                            if auto_close > 0 && s.index >= auto_close {
                                s.gate_open = false;
                                s.index = 0;
                            }
                            self.fire(op, 0);
                        }
                    }
                    1 => s.gate_open = true,
                    2 => s.gate_open = false,
                    _ => s.gate_open = !s.gate_open,
                }
            }
            "SeqAct_Switch" => {
                let count = self.pi(op, "LinkCount").unwrap_or(opd.outputs.len() as i32).max(1);
                let inc = self.pi(op, "IncrementAmount").unwrap_or(1);
                let looping = self.pb(op, "bLooping").unwrap_or(false);
                let from_var = self.read(op, "Index").iter().find_map(|v| v.as_f32()).map(|f| f as i32);
                let s = &mut self.st[op as usize];
                if !s.init {
                    s.init = true;
                    s.index = from_var.unwrap_or(1);
                }
                if input == 1 {
                    s.index = 1;
                    return;
                }
                let idx = s.index;
                let mut next = idx + inc;
                if next > count {
                    next = if looping { 1 } else { count + 1 };
                }
                s.index = next;
                if idx >= 1 && idx <= count {
                    self.fire(op, (idx - 1) as usize);
                }
            }
            "SeqAct_RandomSwitch" => {
                let n = self.pi(op, "LinkCount").unwrap_or(opd.outputs.len() as i32).max(1) as u32;
                self.fire(op, (rand::random::<u32>() % n) as usize);
            }
            "SeqAct_AndGate" => {
                let need = self.linked_inputs[op as usize].max(1);
                let s = &mut self.st[op as usize];
                s.and_seen |= 1u64 << (input.min(63));
                s.fired += 1;
                if s.fired >= need {
                    s.fired = 0;
                    s.and_seen = 0;
                    self.fire(op, 0);
                }
            }
            "SeqAct_Toggle" => {
                for t in self.read(op, "Target") {
                    // (a physics burst fires on any input: `RB_RadialImpulseActor.OnToggle`)
                    if let Some(ka) = if let Val::Actor(a) = &t { g.actors.get(*a as usize) } else { None } {
                        if let Some(imp) = ka.impulse {
                            self.impulses.push((Vec3::from(ka.position), imp));
                            continue;
                        }
                    }
                    match t {
                        Val::Op(ev) => self.toggle_event(ev, input),
                        v @ Val::Actor(_) => {
                            let on = match input {
                                0 => Some(true),
                                1 => Some(false),
                                _ => None,
                            };
                            self.effects.push(Effect::Light(v, on));
                        }
                        _ => {}
                    }
                }
                // events attached through the Event link
                if let Some(KVal::List(evs)) = self.prop(op, "EventLinks").cloned() {
                    for e in evs {
                        if let KVal::Op(ev) = e {
                            self.toggle_event(ev, input);
                        }
                    }
                }
                for w in self.link_vars(op, "Bool") {
                    let cur = self.vals[w as usize].as_bool().unwrap_or(false);
                    self.vals[w as usize] = Val::Bool(match input {
                        0 => true,
                        1 => false,
                        _ => !cur,
                    });
                }
                self.fire(op, 0);
            }

            // ---------------------------------------------------- variables
            "SeqAct_SetBool" => {
                let v = self.read_b(op, "Value", "DefaultValue", true);
                self.write(op, "Target", Val::Bool(v));
                self.fire(op, 0);
            }
            "SeqAct_SetInt" => {
                let v = self.read_f(op, "Value", "DefaultValue", 0.0) as i32;
                self.write(op, "Target", Val::Int(v));
                self.fire(op, 0);
            }
            "SeqAct_SetFloat" => {
                let v = self.read_f(op, "Value", "DefaultValue", 0.0);
                self.write(op, "Target", Val::Float(v));
                self.fire(op, 0);
            }
            "SeqAct_SetObject" | "SeqAct_SetString" => {
                let v = self.read(op, "Value").into_iter().next().or_else(|| self.prop(op, "DefaultValue").map(Val::from_k)).unwrap_or(Val::None);
                self.write(op, "Target", v);
                self.fire(op, 0);
            }
            "SeqAct_AddInt" | "SeqAct_SubtractInt" | "SeqAct_MultiplyInt" | "SeqAct_DivideInt" | "SeqAct_AddFloat" | "SeqAct_SubtractFloat"
            | "SeqAct_MultiplyFloat" | "SeqAct_DivideFloat" => {
                let a = self.read_f(op, "A", "ValueA", 0.0);
                let b = self.read_f(op, "B", "ValueB", 0.0);
                let r = if class.contains("Add") {
                    a + b
                } else if class.contains("Subtract") {
                    a - b
                } else if class.contains("Multiply") {
                    a * b
                } else if b != 0.0 {
                    a / b
                } else {
                    0.0
                };
                self.write(op, "IntResult", Val::Int(r as i32));
                self.write(op, "FloatResult", Val::Float(r));
                self.fire(op, 0);
            }
            "SeqAct_CastToInt" | "SeqAct_CastToFloat" => {
                let v = self.read_f(op, if class.ends_with("Int") { "Float" } else { "Int" }, "", 0.0);
                self.write(op, "Result", if class.ends_with("Int") { Val::Int(v as i32) } else { Val::Float(v) });
                self.write(op, "IntResult", Val::Int(v as i32));
                self.write(op, "FloatResult", Val::Float(v));
                self.fire(op, 0);
            }

            // ---------------------------------------------------- conditions
            "SeqCond_CompareBool" => {
                let vals = self.read(op, "Bool");
                let all = !vals.is_empty() && vals.iter().all(|v| v.as_bool().unwrap_or(false));
                self.fire(op, if all { 0 } else { 1 });
            }
            "SeqCond_CompareInt" | "SeqCond_CompareFloat" | "SeqCond_Increment" | "SeqCond_IncrementFloat" => {
                let mut a = self.read_f(op, if class.contains("Increment") { "Counter" } else { "A" }, "ValueA", 0.0);
                if class.contains("Increment") {
                    a += self.pf(op, "IncrementAmount").unwrap_or(1.0);
                    let wv = if class.ends_with("Float") { Val::Float(a) } else { Val::Int(a as i32) };
                    self.write(op, "Counter", wv);
                }
                let b = self.read_f(op, if class.contains("Increment") { "Comparison" } else { "B" }, "ValueB", 0.0);
                for (i, o) in opd.outputs.iter().enumerate() {
                    let d = o.desc.replace(' ', "");
                    let hit = match d.as_str() {
                        "A<=B" => a <= b,
                        "A>B" => a > b,
                        "A==B" => (a - b).abs() < 1e-4,
                        "A<B" => a < b,
                        "A>=B" => a >= b,
                        "A!=B" => (a - b).abs() >= 1e-4,
                        _ => false,
                    };
                    if hit {
                        self.fire(op, i);
                    }
                }
            }
            "SeqCond_CompareObject" => {
                let a = self.read(op, "A");
                let b = self.read(op, "B");
                let eq = !a.is_empty() && a == b;
                self.fire_desc(op, if eq { "A == B" } else { "A != B" });
            }
            "SeqCond_IsAlive" => self.fire(op, 0),
            "DisSeqCond_CheckStoryFlag" => {
                let f = self.ps(op, "m_StoryFlag").unwrap_or_default();
                let v = *self.flags.get(&f).unwrap_or(&false);
                self.fire(op, if v { 0 } else { 1 });
            }
            "DisSeqCond_CompareBoolExtended" => {
                let vals = self.read(op, "Bool");
                let all = !vals.is_empty() && vals.iter().all(|v| v.as_bool().unwrap_or(false));
                self.fire(op, if all { 0 } else { 1 });
            }
            "DisSeqCond_IsPlayerInCombat" => self.fire(op, if self.player_combat { 0 } else { 1 }),
            // not the automated test harness
            "DisSeqCond_IsSentinel" => self.fire(op, 1),
            "DisSeqCond_IsDLCUnlocked" => {
                let dlc = self.ps(op, "m_DLCType").unwrap_or_default();
                self.fire(op, if crate::script_world::dlc_installed(&dlc) { 0 } else { 1 });
            }
            "DisSeqCond_PawnIsPossessed" => {
                let possessed = self.possessed;
                let yes = self.read(op, "Pawn").iter().any(|v| matches!(v, Val::Actor(a) if possessed.is_some() && g.actors.get(*a as usize).and_then(|a| a.spawner) == possessed));
                self.fire(op, if yes { 0 } else { 1 });
            }
            "DisSeqAct_AISetPatrol" => {
                let start = self.read(op, "StartActor").into_iter().find_map(|v| if let Val::Actor(a) = v { Some(a) } else { None });
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Patrol { targets, start, stop: input == 1 });
                self.fire(op, 0);
            }
            "DisSeqAct_AISetSenses" => {
                let f = |k: &str| self.pb(op, k).unwrap_or(false);
                let (blind, deaf, numb) = (f("m_bBlind"), f("m_bDeaf"), f("m_bNumb"));
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Senses { targets, blind, deaf, numb });
                self.fire(op, 0);
            }
            "DisSeqAct_SetDisposition" => {
                // set, clear, clear all (the class defaults: `Relationship_Enemy`, reciprocated)
                let disp = self.ps(op, "m_Disposition").unwrap_or_else(|| "Relationship_Enemy".into());
                let (targets, recipients) = (self.read(op, "Target"), self.read(op, "Recipients"));
                let reciprocate = self.pb(op, "m_bReciprocate").unwrap_or(true);
                self.ai_fx.push(crate::script_world::AiFx::Disposition { targets, recipients, input: input.min(2) as u8, hostile: disp.contains("Enemy"), reciprocate });
                self.fire(op, 0);
            }
            "DisSeqAct_LimitPawnMinHealth" => {
                let min = self.pi(op, "m_MinimumHealth").unwrap_or(1) as f32;
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::MinHealth { targets, min });
                self.fire(op, 0);
            }
            "SeqAct_ChangeCollision" => {
                let ct = self.ps(op, "CollisionType").unwrap_or_else(|| "COLLIDE_BlockAll".into());
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Collision { targets, block: ct.contains("Block") });
                self.fire(op, 0);
            }
            "DisSeqAct_WallofLightControl" | "DisSeqAct_DefenceTower" => {
                // (arc pylons take the same commands: off, on, toggle, switch polarity)
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Wall { targets, cmd: input as usize });
                self.fire(op, 0);
            }
            "DisSeqAct_AlarmBell" => {
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Bell { targets, on: input == 1 });
                self.fire(op, 0);
            }
            "DisSeqAct_PlugWhaleOilBattery" => {
                let receptacles = self.read(op, "Receptacle");
                self.ai_fx.push(crate::script_world::AiFx::Plug { receptacles, plug: input == 0 });
                self.fire(op, 0);
            }
            "DisSeqAct_ActivateUsable" => {
                let stage = self.read(op, "m_TargetStageIndex").into_iter().find_map(|v| if let Val::Int(i) = v { Some(i) } else { None });
                for t in self.read(op, "Target") {
                    if let Val::Actor(a) = t {
                        self.usable_stages.push((a, stage));
                    }
                }
                self.fire(op, 0);
            }
            "SeqAct_AttachToActor" => {
                let detach = self.pb(op, "bDetach").unwrap_or(false);
                let (targets, attachments) = (self.read(op, "Target"), self.read(op, "Attachment"));
                // a trigger attached to a character moves with it
                let spawner = targets.iter().find_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize).and_then(|a| a.spawner) } else { None });
                for at in &attachments {
                    if let Val::Actor(b) = at {
                        if self.touch.iter().any(|(a, _)| a == b) {
                            match (detach, spawner) {
                                (false, Some(s)) => {
                                    self.touch_follow.insert(*b, (s, None));
                                }
                                _ => {
                                    self.touch_follow.remove(b);
                                }
                            }
                        }
                    }
                }
                self.ai_fx.push(crate::script_world::AiFx::Attach { targets, attachments, detach });
                self.fire(op, 0);
            }
            "SeqAct_ToggleCinematicMode" | "DisSeqAct_DLC05_ToggleCinematicMode" => {
                let f = |k: &str| self.pb(op, k).unwrap_or(true);
                let (hide_hud, hold) = (f("bHideHUD"), f("bDisableMovement") && f("bDisableInput"));
                let on = match input {
                    0 => Some(true),
                    1 => Some(false),
                    _ => None,
                };
                let hide_player = f("bHidePlayer");
                self.ai_fx.push(crate::script_world::AiFx::Cinematic { on, hide_hud, hold, hide_player });
                self.fire(op, 0);
            }
            "SeqAct_ActorFactory" | "SeqAct_ActorFactoryEx" => {
                // what it makes stands at its spawn point: the scripts' later actions on
                // "Spawned" (a PA speaker's announcements) happen there
                if input == 0 {
                    if let Some(at) = self.read(op, "Spawn Point").into_iter().find(|v| matches!(v, Val::Actor(_))) {
                        // (a sound it makes plays there when started)
                        if let (Val::Actor(a), Some(ev)) = (&at, self.ps(op, "ambient_event")) {
                            self.factory_sounds.insert(*a, ev);
                        }
                        self.write(op, "Spawned", at.clone());
                        self.write(op, "Spawned 1", at);
                    }
                }
                // a pickup the factory makes, at its spawn point (or the place it's given:
                // `SpawnLocations`, the Ex's)
                if input == 0 {
                    if let Some(KVal::Int(p)) = self.prop(op, "factory_pickup").cloned() {
                        let at = self
                            .read(op, "Spawn Point")
                            .into_iter()
                            .find_map(|v| if let Val::Actor(a) = v { g.actors.get(a as usize).map(|a| Vec3::from(a.position)) } else { None })
                            .or_else(|| self.read(op, "SpawnLocations").into_iter().find_map(|v| if let Val::Vec3(p) = v { Some(Vec3::from(dhcook::xform::ue_point(p))) } else { None }));
                        if let Some(at) = at {
                            self.ai_fx.push(crate::script_world::AiFx::SpawnPickup { pickup: p as u32, at });
                        }
                    }
                }
                // (the Ex: enable / disable / toggle only gate it)
                if class == "SeqAct_ActorFactoryEx" && input == 0 {
                    if let Some(i) = self.output_index(op, "Spawned 1") {
                        self.fire(op, i);
                    }
                }
                if input == 0 || class == "SeqAct_ActorFactory" {
                    self.fire(op, 0);
                }
            }
            "DisSeqAct_GivePickup" => {
                self.ai_fx.push(crate::script_world::AiFx::GivePickup(op));
                self.fire(op, 0);
            }
            "DisSeqAct_AINoise" => {
                let loud = self.ps(op, "m_eNoiseLoudness").and_then(|l| l.trim_start_matches("EAINoiseLoudness").parse::<usize>().ok()).unwrap_or(1);
                let radius = [3.0, 6.0, 10.0, 16.0, 24.0, 40.0][loud.min(5)];
                let combat = self.ps(op, "m_eNoiseContext").is_some_and(|c| c.contains("Danger"));
                let (maker, at) = (self.read(op, "NoiseMaker"), self.read(op, "NoiseLocation"));
                self.ai_fx.push(crate::script_world::AiFx::Noise { maker, at, radius, combat });
                self.fire(op, 0);
            }
            "DisSeqAct_AIStartDistraction" => {
                let (targets, to) = (self.read(op, "Target"), self.read(op, "Distractor"));
                self.ai_fx.push(crate::script_world::AiFx::Distract { targets, to });
                self.fire(op, 0);
            }
            "DisSeqAct_AISetSuspicionLevel" => {
                let suspecting = self.ps(op, "m_SuspicionLevel").is_some_and(|l| l != "DAISL_None");
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Suspicion { targets, suspecting });
                self.fire(op, 0);
            }
            "DisSeqAct_AIDoSearch" => {
                let (targets, around) = (self.read(op, "Target"), self.read(op, "Search Start"));
                self.ai_fx.push(crate::script_world::AiFx::Search { targets, around, abort: input == 1 });
                self.fire(op, 0);
            }
            "DisSeqAct_AIGuard" => {
                let targets = self.read(op, "Target");
                let mut home = self.read(op, "Home Actor");
                if let Some(KVal::Actor(a)) = self.prop(op, "m_pHomeActor") {
                    home.push(Val::Actor(*a));
                }
                self.ai_fx.push(crate::script_world::AiFx::Guard { targets, home, stop: input == 1 });
                self.fire(op, 0);
            }
            "DisSeqAct_NPCDoTeleportSpell" => {
                if input == 0 {
                    let (targets, to) = (self.read(op, "Target"), self.read(op, "Destination"));
                    let set_home = self.pb(op, "m_bSetNewHomeActor").unwrap_or(false);
                    self.ai_fx.push(crate::script_world::AiFx::Teleport { targets, to, set_home });
                    self.fire(op, 0);
                    self.fire(op, 1);
                } else {
                    self.fire(op, 2);
                }
            }
            "DisSeqAct_AIClearAttention" => {
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::ClearAttention { targets });
                self.fire(op, 0);
            }
            "DisSeqAct_AIPsychicAttention" => {
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Psychic { targets, on: input == 0 });
                self.fire(op, 0);
            }
            "DisSeqAct_SetRatSwarmCustomBehavior" => {
                let (swarms, to) = (self.read(op, "Target"), self.read(op, "Action Target"));
                self.ai_fx.push(crate::script_world::AiFx::RatsGo { swarms, to });
                self.fire(op, 0);
                self.latent.push(Latent::After { op, out: 1, left: 4.0 });
            }
            "DisSeqAct_AIShoot" => {
                if input == 0 {
                    let (shooters, target) = (self.read(op, "Target"), self.read(op, "ShotTarget"));
                    let shots = self.pi(op, "m_iMaxNumShots").unwrap_or(1).max(1) as u32;
                    let accuracy = if self.pb(op, "m_bOverrideAccuracy").unwrap_or(false) { self.pf(op, "m_fOverrideAccuracyPercentage").unwrap_or(0.5) } else { 0.8 };
                    self.ai_fx.push(crate::script_world::AiFx::Shoot { shooters, target, shots, accuracy });
                    self.fire(op, 0);
                    self.latent.push(Latent::After { op, out: 1, left: 0.6 * shots as f32 + 0.3 });
                } else {
                    self.fire(op, 3);
                }
            }
            "DisSeqAct_AIDoFollow" => {
                let (targets, leader) = (self.read(op, "Target"), self.read(op, "BehaviorTarget"));
                self.ai_fx.push(crate::script_world::AiFx::Follow { targets, leader, stop: input == 1 });
                self.fire(op, 0);
            }
            "DisSeqAct_GameOver" => {
                let reason = self.ps(op, "m_Reason").unwrap_or_default();
                self.ai_fx.push(crate::script_world::AiFx::GameOver(reason));
                self.fire(op, 0);
            }
            "DisSeqCond_TaskState" => {
                let task = self.read(op, "Task").into_iter().find_map(|v| if let Val::Str(s) = v { Some(s) } else { None });
                let state = task.and_then(|t| self.tasks.get(&t).map(|s| s.0)).unwrap_or(TaskState::Inactive);
                let name = format!("{state:?}");
                match self.output_index(op, &name) {
                    Some(i) => self.fire(op, i),
                    None => self.fire(op, 0),
                }
            }
            "DisSeqCond_IsObjectiveComplete" | "DisSeqCond_HasObjective" => {
                let obj = self.read(op, "Objective").into_iter().find_map(|v| if let Val::Str(s) = v { Some(s) } else { None }).unwrap_or_default();
                let has = self.objectives.contains(&obj);
                let complete = self.tasks.get(&obj).map(|t| t.0 == TaskState::Completed).unwrap_or(false);
                let yes = if class.contains("Complete") { complete } else { has };
                self.fire(op, if yes { 0 } else { 1 });
            }
            "DisSeqAct_SetPlayerHealth" | "DisSeqAct_SetPlayerMana" => {
                // (the variable over the property; percent of the most)
                let health = class.ends_with("Health");
                let key = if health { "m_Health" } else { "m_Mana" };
                let var = self.read(op, if health { "Health" } else { "Mana" }).into_iter().find_map(|v| match v {
                    Val::Int(i) => Some(i as f32),
                    Val::Float(f) => Some(f),
                    _ => None,
                });
                let v = var.or(self.pi(op, key).map(|i| i as f32)).unwrap_or(100.0);
                self.vitals.push(if health { (Some(v), None) } else { (None, Some(v)) });
                self.fire(op, 0);
            }
            "DisSeqAct_ModifyElixirCount" => {
                let mana = self.ps(op, "m_ElixirType").is_some_and(|t| t.contains("Mana"));
                self.elixirs.push((mana, self.pi(op, "m_Value").unwrap_or(1)));
                self.fire(op, 0);
            }
            "DisSeqAct_GiveUpgrade" => {
                if let Some(u) = self.ps(op, "m_pUpgrade") {
                    self.upgrades.push(u.rsplit('.').next().unwrap_or(&u).to_string());
                }
                self.fire(op, 0);
            }
            "DisSeqAct_BodyShadowKill" => {
                self.ai_fx.push(crate::script_world::AiFx::AshOnDeath { targets: self.read(op, "Target") });
                self.fire(op, 0);
            }
            "DisSeqAct_AISetBrainFlags" => {
                // enable (0) / disable (1) / toggle (2) the flags on the targets
                // (`EDisAIBrainFlags` order)
                const FLAGS: [&str; 6] = ["Combat_DontAttack", "Combat_NoPrimaryAttacks", "Combat_NoRangedAttacks", "Panic_ForbidDespawn", "Panic_Civilian_Disable", "Overseer_DontUseGrenade"];
                let flags: Vec<u8> = match self.prop(op, "m_FlagsToModify") {
                    Some(KVal::List(l)) => l
                        .iter()
                        .filter_map(|v| if let KVal::Str(s) = v { FLAGS.iter().position(|f| s.trim_start_matches("EDisAIBrainFlags_") == *f).map(|i| i as u8) } else { None })
                        .collect(),
                    _ => Vec::new(),
                };
                self.ai_fx.push(crate::script_world::AiFx::BrainFlags { targets: self.read(op, "Target"), flags, input: input as u8 });
                self.fire(op, 0);
            }
            "DisSeqAct_SetPlayerVisSettings" => {
                if let Some(KVal::List(l)) = self.prop(op, "pvs") {
                    if let [KVal::Float(a), KVal::Float(b)] = l.as_slice() {
                        self.player_vis = Some([*a, *b]);
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_OverrideAwarenessDisplay" => {
                // start (0) / stop (1) showing the characters' awareness
                for v in self.read(op, "NPC") {
                    if let Val::Actor(a) = v {
                        if input == 0 {
                            self.awareness_shown.insert(a);
                        } else {
                            self.awareness_shown.remove(&a);
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_ClearPlayerVisSettings" => {
                self.player_vis = None;
                self.fire(op, 0);
            }
            "DisSeqAct_Highlight" => {
                // highlight (0) / unhighlight (1) the targets
                for v in self.read(op, "Target") {
                    if let Val::Actor(a) = v {
                        if input == 0 {
                            self.highlights.insert(a);
                        } else {
                            self.highlights.remove(&a);
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_SpawnStealable" => {
                // something put in a character's pocket for Corvo to steal (a key, elixirs)
                if let Some(KVal::Int(p)) = self.prop(op, "factory_pickup").cloned() {
                    self.ai_fx.push(crate::script_world::AiFx::SpawnStealable { pickup: p as u32, targets: self.read(op, "Target") });
                }
                self.fire(op, 0);
            }
            "DisSeqAct_AIDoSimpleBehaviors" => {
                // start (0) / abort (1) a simple behaviour: beg (the default) or panic
                let panic = self.ps(op, "m_eSimpleBehaviorToRun").is_some_and(|b| b.ends_with("Panic"));
                self.ai_fx.push(crate::script_world::AiFx::Simple { targets: self.read(op, "Target"), panic, start: input == 0 });
                self.fire(op, if input == 0 { 0 } else { 1 });
            }
            "DisSeqAct_WatchTowerShootAtTarget" => {
                // a tower's volley at a target (a character, an actor, Corvo)
                let towers: Vec<String> = self.read(op, "Target").iter().filter_map(|v| if let Val::Actor(a) = v { self.g.actors.get(*a as usize).map(|x| x.name.clone()) } else { None }).collect();
                let at = self.read(op, "Attack Target").iter().find_map(|v| self.position_of(v));
                if let Some(at) = at {
                    for t in towers {
                        self.tower_shots.push((t, at));
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_OpenJournal" => {
                // the journal opens on a tab (the Outsider's dream: the powers after a rune)
                self.open_journal = Some(self.ps(op, "m_Tab").unwrap_or_default());
                self.fire(op, 0);
            }
            "DisSeqAct_BackupAndClearInventory" => {
                // Corvo's things put away (the dream: all but his coins)
                self.inventory_ops.push((true, self.pb(op, "m_bAlsoApplyToUpgrades").unwrap_or(false)));
                self.fire(op, 0);
            }
            "DisSeqAct_RestoreInventoryFromBackup" => {
                self.inventory_ops.push((false, false));
                self.fire(op, 0);
            }
            "SeqAct_ToggleHUD" => {
                // show (0) / hide (1) / toggle (2) the whole HUD
                self.effects.push(Effect::HudAll(match input {
                    0 => Some(true),
                    1 => Some(false),
                    _ => None,
                }));
                self.fire(op, 0);
            }
            "DisSeqAct_PostProcess" => {
                // a screen effect of the post-process manager: start (0) / stop (1)
                if let Some(e) = self.ps(op, "m_Effect") {
                    self.post_effects.retain(|x| *x != e);
                    if input == 0 {
                        self.post_effects.push(e);
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_UberPostProcess" => {
                // start (0) / stop (1), fading in or out
                if input == 0 {
                    self.effects.push(Effect::Post(Some(op), self.read_f(op, "m_fFadeInTime", "m_fFadeInTime", 0.0)));
                } else {
                    self.effects.push(Effect::Post(None, self.read_f(op, "m_fFadeOutTime", "m_fFadeOutTime", 0.0)));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_ModifyAmmo" => {
                // (Corvo's: the Tower's opening empties his pouches, the Prison's too)
                if self.read(op, "Targets").iter().chain(self.read(op, "Target").iter()).any(|v| matches!(v, Val::Player)) {
                    let how = match self.ps(op, "m_AmmoOp").as_deref() {
                        Some("eDisAmmoOp_SetAmmo") => 1,
                        Some("eDisAmmoOp_RemoveAmmo") => 2,
                        _ => 0,
                    };
                    let list = match self.prop(op, "ammo") {
                        Some(KVal::List(l)) => l
                            .iter()
                            .filter_map(|e| match e {
                                KVal::List(p) => match (p.first(), p.get(1)) {
                                    (Some(KVal::Int(t)), Some(KVal::Int(n))) => Some((*t as u8, *n)),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    self.ammo_mods.push((how, list));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_RemoveKey" => {
                if let Some(k) = self.ps(op, "m_Name") {
                    self.keys_taken.push(k);
                }
                self.fire(op, 0);
            }
            "DisSeqAct_SetObjectiveHidden" => {
                for v in self.read(op, "Objective") {
                    if let Val::Str(path) = v {
                        if let Some(t) = self.tasks.get_mut(&path) {
                            t.1 = true;
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_TogglePowerWheel" => {
                // enable (0), disable (1), toggle (2)
                self.wheel_off = match input {
                    0 => false,
                    1 => true,
                    _ => !self.wheel_off,
                };
                self.fire(op, 0);
            }
            "SeqAct_AccessObjectList" => {
                // random (0), first (1), last (2), at index (3)
                let list: Vec<Val> = self.read(op, "Object List").into_iter().flat_map(|v| if let Val::List(l) = v { l } else { vec![v] }).filter(|v| !matches!(v, Val::None)).collect();
                if !list.is_empty() {
                    let i = match input {
                        0 => rand::random::<u32>() as usize % list.len(),
                        1 => 0,
                        2 => list.len() - 1,
                        _ => {
                            let i = self.read(op, "Index").into_iter().find_map(|v| if let Val::Int(i) = v { Some(i) } else { None }).unwrap_or(0);
                            (i.max(0) as usize).min(list.len() - 1)
                        }
                    };
                    self.write(op, "Output Object", list[i].clone());
                }
                self.fire(op, 0);
            }
            "DisSeqAct_NPCSetMaterials" => {
                // set (0) / clear (1)
                let targets = self.read(op, "NPC(s)");
                self.npc_materials.push((targets, op, input == 0));
                self.fire(op, 0);
            }
            "DisSeqAct_FireProjectile" => {
                // a bolt (or a flare arrow) from one point to another: the streets' crossfire
                let from = self.read(op, "Source").iter().find_map(|v| self.pos_of(v));
                let to = self.read(op, "Target").iter().find_map(|v| self.pos_of(v));
                if let (Some(a), Some(b)) = (from, to) {
                    let flare = self.ps(op, "m_pProjectileTweak").is_some_and(|t| t.to_ascii_lowercase().contains("flare"));
                    self.projectiles.push((a, b, flare));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_TriggerExplosion" => {
                for t in self.read(op, "Target") {
                    if let Some(p) = self.pos_of(&t) {
                        self.explosions.push((p, op));
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqCond_IsDoorOpen" => {
                // closed (0), open clockwise (1), counter-clockwise (2)
                let door = self.read(op, "Door").into_iter().find_map(|v| if let Val::Actor(a) = v { Some(a) } else { None });
                let out = match door.and_then(|a| self.door_open.get(&a)) {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                };
                self.fire(op, out);
            }
            "DisSeqCond_CompareTweaks" => {
                // a character's pawn tweak (or a tweak named outright) against another: the
                // same, one a variant of the other (`Pwn_LadyEmily_TowerEmpress` of
                // `Pwn_LadyEmily`), or not
                // (a thing's: its tweak and those it falls back on, the Prison's sword an
                // `InventoryPickupSwordBase`)
                let lineage = |vm: &Self, v: &Val| -> Vec<String> {
                    match v {
                        Val::Str(s) => vec![s.clone()],
                        Val::Actor(x) => match vm.g.actors.get(*x as usize) {
                            Some(a) => match a.spawner {
                                Some(s) => vm.npc_pawn.get(&s).cloned().into_iter().collect(),
                                None => a.tweaks.clone(),
                            },
                            None => Vec::new(),
                        },
                        _ => Vec::new(),
                    }
                };
                let a = self.read(op, "A").iter().map(|v| lineage(self, v)).find(|l| !l.is_empty()).unwrap_or_default();
                let b = self.read(op, "B").iter().map(|v| lineage(self, v)).find(|l| !l.is_empty()).unwrap_or_default();
                let short = |s: &str| s.rsplit('.').next().unwrap_or(s).to_ascii_lowercase();
                let same = |x: &String, y: &String| x.eq_ignore_ascii_case(y) || short(x) == short(y);
                let out = match (a.first(), b.first()) {
                    (Some(x), Some(y)) if same(x, y) => 0,
                    (Some(x), Some(y)) if a.iter().any(|t| same(t, y)) || b.iter().any(|t| same(t, x)) => 1,
                    (Some(x), Some(y)) if short(x).starts_with(&short(y)) || short(y).starts_with(&short(x)) => 1,
                    _ => 2,
                };
                if self.trace {
                    info!("kismet: compare tweaks {a:?} with {b:?}: {out}");
                }
                self.fire(op, out);
            }
            c if c.starts_with("DisSeqCond") || c.starts_with("SeqCond") => {
                // unknown condition: take the negative branch when there is one
                self.fire(op, if opd.outputs.len() > 1 { 1 } else { 0 });
            }
            "DisSeqAct_AddPower" => {
                let name = self.ps(op, "m_PowerName").unwrap_or_default();
                let level = self.pi(op, "m_PowerLevel").unwrap_or(1).clamp(1, 2) as u8;
                self.effects.push(Effect::Power(name, level));
                self.fire(op, 0);
            }
            "DisSeqAct_RemovePower" => {
                let name = self.ps(op, "m_PowerName").unwrap_or_default();
                self.effects.push(Effect::Power(name, 0));
                self.fire(op, 0);
            }
            "SeqAct_ModifyObjectList" => {
                // add (0), remove (1), empty (2); the count written back
                let refs = self.read(op, "ObjectRef");
                let lists = self.link_vars(op, "ObjectListVar");
                let mut n = 0;
                for w in lists {
                    let mut items = match &self.vals[w as usize] {
                        Val::List(l) => l.clone(),
                        Val::None => Vec::new(),
                        v => vec![v.clone()],
                    };
                    match input {
                        0 => {
                            for r in &refs {
                                if !matches!(r, Val::None) && !items.contains(r) {
                                    items.push(r.clone());
                                }
                            }
                        }
                        1 => items.retain(|x| !refs.contains(x)),
                        _ => items.clear(),
                    }
                    n = items.len();
                    self.vals[w as usize] = Val::List(items);
                }
                self.write(op, "ListEntriesCount", Val::Int(n as i32));
                self.fire(op, 0);
            }
            "SeqAct_IsInObjectList" => {
                let tests = self.read(op, "Object(s)ToTest");
                let list = self.read(op, "ObjectListVar");
                let all = self.pb(op, "bCheckForAllObjects").unwrap_or(false);
                let found = |v: &Val| list.contains(v);
                let ok = if all { !tests.is_empty() && tests.iter().all(found) } else { tests.iter().any(found) };
                self.fire(op, if ok { 0 } else { 1 });
            }
            "SeqAct_Timer" => {
                // start (0) / stop (1): the time between, written out
                if input == 0 {
                    self.timer_start.insert(op, self.time);
                } else if let Some(t0) = self.timer_start.remove(&op) {
                    self.write(op, "Time", Val::Float(self.time - t0));
                    self.fire(op, 0);
                }
            }
            "DisSeqAct_GetAbstractItemQuantity" => {
                let item = self.ps(op, "m_pItem").unwrap_or_default();
                let short = item.split_once('.').map(|x| x.1.to_string()).unwrap_or(item);
                let n = self.item_counts.get(&short).copied().unwrap_or(0);
                self.write(op, "Quantity", Val::Int(n as i32));
                self.fire(op, 0);
            }
            "DisSeqAct_CancelPlayerActivePower" => {
                self.cancel_powers = true;
                self.fire(op, 0);
            }
            "DisSeqAct_BendTime" => {
                // start (0) / stop (1): the world slowed (stopped by default), Corvo not
                self.bend_time.push((input == 0).then(|| self.pf(op, "m_fWorldTimeDilation").unwrap_or(0.0)));
                self.fire(op, 0);
            }
            "DisSeqAct_EquipItemType" => {
                let targets = self.read(op, "Target");
                let item = self.ps(op, "m_pItemTypeToEquip").unwrap_or_default();
                self.equip.push((targets, item));
                self.fire(op, 0);
            }
            "DisSeqAct_SpawnCameraLensEffect" => {
                // spawn (0) / stop looping (1); "Finished" (0) when it is over
                if input == 0 {
                    match self.pi(op, "lens_system") {
                        Some(sys) => {
                            let looping = self.pb(op, "lens_looping").unwrap_or(false);
                            let life = self.pf(op, "lens_life").unwrap_or(0.0);
                            self.lens.push((op, Some((sys as u32, looping, life))));
                        }
                        None => self.fire(op, 1),
                    }
                } else {
                    self.lens.push((op, None));
                }
            }
            "DisSeqAct_PlayerTrackTarget" => {
                // start (0) / stop (1)
                self.player_track = if input == 0 {
                    let target = self.read(op, "TrackedTarget");
                    (!target.is_empty()).then(|| (target, self.pf(op, "m_fBlendTime").unwrap_or(1.0)))
                } else {
                    None
                };
                self.fire(op, 0);
            }
            "DisSeqAct_NPCMarkForVanish" => {
                let targets = self.read(op, "Target");
                self.vanish.push((targets, op, input == 0));
                self.fire(op, 0);
            }
            "SeqAct_SetMaterial" => {
                if let Some(id) = self.pi(op, "material_id") {
                    let targets = self.read(op, "Target");
                    self.set_materials.push((targets, id as u32));
                }
                self.fire(op, 0);
            }
            "SeqAct_SetMatInstScalarParam" => {
                if let (Some(mi), Some(param)) = (self.ps(op, "MatInst"), self.ps(op, "ParamName")) {
                    let v = self.read_f(op, "ScalarValue", "ScalarValue", 0.0);
                    self.mat_params.push((mi, param, v));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_ToggleHUDElement" => {
                // enable (0) / disable (1); `EDisHudElement` starts with the crosshair
                let el = self.ps(op, "m_HudElement").unwrap_or_else(|| "DHE_Crosshair".into());
                self.effects.push(Effect::Hud(el, input == 0));
                self.fire(op, 0);
            }
            "DisSeqAct_AddKey" => {
                // onto Corvo's ring, by its name
                if let Some(name) = self.ps(op, "m_Name").filter(|n| !n.is_empty()) {
                    self.effects.push(Effect::Key(name));
                }
                self.fire(op, 0);
            }
            "SeqAct_GetDistance" => {
                // (in Unreal units, as the scripts compare it)
                let (a, b) = (self.read(op, "A"), self.read(op, "B"));
                let d = match (a.iter().find_map(|v| self.pos_of(v)), b.iter().find_map(|v| self.pos_of(v))) {
                    (Some(a), Some(b)) => a.distance(b) * 100.0,
                    _ => 0.0,
                };
                if self.trace {
                    info!("kismet: {} distance {d:.0} between {a:?} and {b:?}", self.g.ops[op as usize].name);
                }
                self.write(op, "Distance", Val::Float(d));
                self.fire(op, 0);
            }
            "DisSeqAct_AddInventoryItem" | "DisSeqAct_RemoveInventoryItem" => {
                let add = class == "DisSeqAct_AddInventoryItem";
                let item = self.ps(op, if add { "m_pItemToAdd" } else { "m_pRemoveAllOfType" }).unwrap_or_default();
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::Arm { targets, item, add });
                // (removal: done at once, and unequipped)
                self.fire(op, 0);
                if !add {
                    self.fire(op, 1);
                }
            }
            "DisSeqAct_NPCTrackTarget" => {
                // start (0), stop (1), reset (2): the character's head follows the target
                let (npcs, target) = (self.read(op, "NPC"), self.read(op, "TrackedTarget"));
                self.ai_fx.push(crate::script_world::AiFx::Track { npcs, target, on: input == 0 });
                self.fire(op, 0);
            }
            "DisSeqAct_SetIgnoreDeath" => {
                // kept from dying (as `DisSeqAct_LimitPawnMinHealth` at 1)
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::MinHealth { targets, min: 1.0 });
                self.fire(op, 0);
            }
            "DisSeqAct_AIRingAlarm" => {
                let targets = self.read(op, "Target");
                self.ai_fx.push(crate::script_world::AiFx::RingAlarm { targets });
                self.fire(op, 0);
            }
            "DisSeqAct_RemoveAbstractItem" => {
                // a note or an invitation handed over
                if let Some(item) = self.ps(op, "m_pItemToRemove") {
                    self.effects.push(Effect::TakeItem(item));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_AutoSave" => {
                // the save goes through: Out, then Accepted
                self.effects.push(Effect::AutoSave);
                self.fire(op, 0);
                self.fire(op, 1);
            }
            "DisSeqAct_OpenCraftsmanStore" => {
                let tweak = self.ps(op, "m_pTweaks").unwrap_or_default();
                let tweak = tweak.rsplit('.').next().unwrap_or(&tweak).to_string();
                self.effects.push(Effect::Store(tweak));
                self.fire(op, 0);
            }
            "DisSeqAct_SetStoryFlag" => {
                let f = self.ps(op, "m_StoryFlag").unwrap_or_default();
                let cur = *self.flags.get(&f).unwrap_or(&false);
                self.flags.insert(
                    f,
                    match input {
                        0 => true,
                        1 => false,
                        _ => !cur,
                    },
                );
                self.fire(op, 0);
            }

            // ---------------------------------------------------- world
            "DisSeqAct_StartSpawn" => {
                let spawners: Vec<u32> = self
                    .read(op, "Target")
                    .into_iter()
                    .filter_map(|v| if let Val::Actor(a) = v { g.actors.get(a as usize).and_then(|a| a.spawner) } else { None })
                    .collect();
                for &s in &spawners {
                    self.effects.push(Effect::Spawn(s));
                }
                self.fire(op, 0);
                self.latent.push(Latent::Spawn { op, left: 0.25 });
            }
            "SeqAct_Destroy" => {
                for t in self.read(op, "Target") {
                    // a joint: what hung by it falls
                    if let Some(ka) = if let Val::Actor(a) = t { g.actors.get(a as usize) } else { None } {
                        if ka.class.starts_with("RB_") {
                            self.joints_broken.insert(ka.name.clone());
                        }
                    }
                    self.effects.push(Effect::Destroy(t));
                }
                self.fire(op, 0);
            }
            "SeqAct_ToggleHidden" => {
                let h = match input {
                    0 => Some(true),
                    1 => Some(false),
                    _ => None,
                };
                for t in self.read(op, "Target") {
                    self.effects.push(Effect::Hide(t, h));
                }
                self.fire(op, 0);
            }
            "SeqAct_Teleport" => {
                let dest = self.read(op, "Destination").into_iter().find_map(|v| if let Val::Actor(a) = v { g.actors.get(a as usize) } else { None });
                if let Some(d) = dest {
                    for t in self.read(op, "Target") {
                        self.effects.push(Effect::Teleport(t, Vec3::from(d.position), d.yaw));
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_Door" | "DisSeqAct_Lock" => {
                let cmd = if class == "DisSeqAct_Lock" {
                    if input == 0 { DoorCmd::Lock } else { DoorCmd::Unlock }
                } else {
                    match input {
                        0 => DoorCmd::Open(true),
                        1 => DoorCmd::Open(false),
                        2 => DoorCmd::Close,
                        3 => DoorCmd::Lock,
                        _ => DoorCmd::Unlock,
                    }
                };
                for t in self.read(op, "Target") {
                    // (usable objects lock too: chests, cabinets)
                    if let (Val::Actor(a), DoorCmd::Lock | DoorCmd::Unlock) = (&t, cmd) {
                        if self.g.actors.get(*a as usize).is_some_and(|ka| ka.usable.is_some()) {
                            self.usable_locks.push((*a, matches!(cmd, DoorCmd::Lock)));
                        }
                    }
                    // (a door that moves tells its scripts: the Prison cell's locks behind
                    // Corvo once closed)
                    if let Val::Actor(a) = t {
                        match cmd {
                            DoorCmd::Open(cw) => {
                                if self.door_open.insert(a, cw).is_none() {
                                    self.latent.push(Latent::DoorSwung { actor: a, opened: true, left: DOOR_SWING });
                                }
                            }
                            DoorCmd::Close => {
                                if self.door_open.remove(&a).is_some() {
                                    self.latent.push(Latent::DoorSwung { actor: a, opened: false, left: DOOR_SWING });
                                }
                            }
                            _ => {}
                        }
                    }
                    self.effects.push(Effect::Door(t, cmd));
                }
                self.fire(op, 0);
            }
            "SeqAct_ModifyHealth" => {
                let amount = self.read_f(op, "Amount", "Amount", 0.0);
                let heal = self.pb(op, "bHeal").unwrap_or(false);
                for t in self.read(op, "Target") {
                    // a thing (a PA speaker): worn down, broken
                    if let Some(ka) = if let Val::Actor(a) = t { g.actors.get(a as usize) } else { None } {
                        if ka.spawner.is_none() && !ka.instances.is_empty() {
                            if !heal {
                                self.prop_damage.push((ka.instances.clone(), amount));
                            }
                            continue;
                        }
                    }
                    self.effects.push(Effect::Damage(t, if heal { -amount } else { amount }));
                }
                self.fire(op, 0);
            }
            "SeqAct_AkPostEvent" => {
                if let Some(ev) = self.ps(op, "Event") {
                    let targets = self.read(op, "Target");
                    if targets.is_empty() {
                        self.effects.push(Effect::Sound(ev.clone(), None));
                    }
                    for t in targets {
                        self.effects.push(Effect::Sound(ev.clone(), Some(t)));
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_RBConstraint" => {
                // a joint given nothing to bind (`ConstraintActor1` empty: the Tower's) lets go
                if !self.read(op, "ConstraintActor1").iter().any(|v| matches!(v, Val::Actor(_))) {
                    for t in self.read(op, "Targets") {
                        if let Some(ka) = if let Val::Actor(a) = t { g.actors.get(a as usize) } else { None } {
                            if ka.class.starts_with("RB_") {
                                self.joints_broken.insert(ka.name.clone());
                            }
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_EvalAchievement" => {
                if let Some(a) = self.ps(op, "m_Achievement") {
                    let reset = self.pb(op, "m_bResetStats").unwrap_or(false);
                    self.achievement_evals.push((a, reset));
                }
                self.fire(op, 0);
            }
            // ------------------------------------------------- Dunwall City Trials
            "DisSeqAct_DLC05_SendChallengeEvent" => {
                // (unset: the challenge's end, the enum's first)
                let ev = self.ps(op, "m_eEventToSend").unwrap_or_else(|| "ECE_Challenge_End".into());
                self.challenge_fx.push(crate::challenge::ChallengeFx::Event(ev));
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_StreamLevels" => {
                let names: Vec<String> = match self.prop(op, "Levels") {
                    Some(KVal::List(l)) => l.iter().filter_map(|v| if let KVal::Str(s) = v { Some(s.clone()) } else { None }).collect(),
                    _ => Vec::new(),
                };
                for n in names {
                    if input == 0 {
                        self.load_level(&n);
                    } else {
                        self.unload_level(&n);
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_Timer" => {
                // Start, Stop, Add Modifier, Pause, Resume -> Started, Stopped, (Completed), Paused, Resumed
                let modifier = self.read(op, "Modifier").iter().find_map(|v| v.as_f32()).unwrap_or(0.0);
                let p = crate::challenge::TimerParams {
                    initial: self.pf(op, "m_fInitialTime").unwrap_or(0.0),
                    target: self.pb(op, "m_bUseTargetTime").unwrap_or(false).then(|| self.pf(op, "m_fTargetTime").unwrap_or(0.0)),
                    increment: self.pb(op, "m_bIncrement").unwrap_or(true),
                    reset_on_stop: self.pb(op, "m_bResetOnStop").unwrap_or(false),
                    kind: self.ps(op, "m_TimerType").unwrap_or_else(|| "DDHT_DefaultTimer".into()),
                };
                self.challenge_fx.push(crate::challenge::ChallengeFx::Timer { op, input, params: p, modifier });
                match input {
                    0 => self.fire(op, 0),
                    1 => self.fire(op, 1),
                    3 => self.fire(op, 3),
                    4 => self.fire(op, 4),
                    _ => {}
                }
            }
            "DisSeqAct_DLC05_SetScoringRules" => {
                if let Some(t) = self.ps(op, "m_pRuleSetTweak") {
                    self.challenge_fx.push(crate::challenge::ChallengeFx::Rules(t));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_TriggerCustomScoringRule" => {
                if let Some(r) = self.ps(op, "m_RuleToTrigger") {
                    self.challenge_fx.push(crate::challenge::ChallengeFx::CustomRule(r));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_ShowHUDItem" => {
                // Show, Hide, Reset -> Shown, Hidden, Reset
                let item = self.ps(op, "m_Item").or_else(|| self.ps(op, "m_LinkSetup")).unwrap_or_default();
                let initial = self.read(op, "Initial Count").iter().find_map(|v| v.as_f32()).map(|f| f as i32);
                let max = self.pi(op, "m_MaxValue");
                self.challenge_fx.push(crate::challenge::ChallengeFx::HudItem { item, input, initial, max });
                self.fire(op, (input as usize).min(2));
            }
            "DisSeqAct_DLC05_ShowWaveNumber" => {
                let number = self.read(op, "Number").iter().find_map(|v| v.as_f32()).map(|f| f as i32);
                let text = self.ps(op, "m_CustomText").filter(|t| !t.is_empty());
                self.challenge_fx.push(crate::challenge::ChallengeFx::Wave { number, text });
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_ShowCountdown" => {
                // Out now; Finished once the count is down (`challenge.rs`)
                let go = self.pb(op, "m_bShowGO").unwrap_or(true);
                self.challenge_fx.push(crate::challenge::ChallengeFx::Countdown { op, go });
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_ShowPhaseResults" => {
                let n = |s: &Self, d: &str| s.read(op, d).iter().find_map(|v| v.as_f32()).unwrap_or(0.0) as i32;
                let fx = crate::challenge::ChallengeFx::PhaseResults {
                    name: self.ps(op, "m_PhaseName").unwrap_or_default(),
                    last: self.pb(op, "m_bWasLastPhase").unwrap_or(false),
                    possible: n(self, "Possible Kills"),
                    required: n(self, "Required Kills"),
                    effective: n(self, "Effective Kills"),
                };
                self.challenge_fx.push(fx);
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_ShowEquipmentUnlock" => {
                self.challenge_fx.push(crate::challenge::ChallengeFx::EquipmentUnlock);
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_SetDifficulty" => {
                self.challenge_fx.push(crate::challenge::ChallengeFx::Difficulty(self.ps(op, "m_Difficulty").unwrap_or_else(|| "EDifficulty_Normal".into())));
                self.fire(op, 0);
            }
            "DisSeqCond_DLC05_IsExpertMode" => {
                // Normal, Expert
                self.fire(op, self.expert as usize);
            }
            "DisSeqAct_DLC05_PlayerResurrect" => {
                self.challenge_fx.push(crate::challenge::ChallengeFx::Resurrect);
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_Heal" => {
                let pct = self.read(op, "m_MaxHealthPercentage").iter().find_map(|v| v.as_f32()).or_else(|| self.pi(op, "m_MaxHealthPercentage").map(|i| i as f32)).unwrap_or(100.0);
                self.challenge_fx.push(crate::challenge::ChallengeFx::Heal(pct));
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_InfiniteAmmo" => {
                // Enable, Disable -> Enabled, Disabled
                self.challenge_fx.push(crate::challenge::ChallengeFx::InfiniteAmmo(input == 0));
                self.fire(op, (input as usize).min(1));
            }
            "DisSeqAct_DLC05_DollCollected" => {
                self.doll_found = true;
                self.challenge_fx.push(crate::challenge::ChallengeFx::Doll);
                self.fire(op, 0);
            }
            "DisSeqCond_DLC05_IsDollAlreadyCollected" => {
                // No, Yes
                self.fire(op, self.doll_found as usize);
            }
            "DisSeqAct_DLC05_UnlockAchievement" => {
                // eDLC05Achievement_TimeManager -> eAchievement_DLC05_TimeManager
                if let Some(a) = self.ps(op, "m_Achievement") {
                    let name = format!("eAchievement_DLC05_{}", a.trim_start_matches("eDLC05Achievement_"));
                    self.achievement_evals.push((name, false));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_DLC05_Teleport" => {
                // Success, Fail
                let dest = self.read(op, "Destination").into_iter().find_map(|v| if let Val::Actor(a) = v { g.actors.get(a as usize) } else { None });
                match dest {
                    Some(d) => {
                        let targets = self.read(op, "Target");
                        let targets = if targets.is_empty() { vec![Val::Player] } else { targets };
                        for t in targets {
                            self.effects.push(Effect::Teleport(t, Vec3::from(d.position), d.yaw));
                        }
                        self.fire(op, 0);
                    }
                    None => self.fire(op, 1),
                }
            }
            "DisSeqAct_DLC05_NpcWave" => {
                // Add Npc: a moment of slowed time on the newcomers (`m_fWorldTimeDilation` for
                // `m_fDuration`); Stop BendTime
                if input == 0 {
                    self.fire(op, 0);
                    let dur = self.pf(op, "m_fDuration").unwrap_or(1.5);
                    self.bend_time.push(Some(self.pf(op, "m_fWorldTimeDilation").unwrap_or(0.2)));
                    self.challenge_fx.push(crate::challenge::ChallengeFx::WaveBendTime { op, secs: dur });
                    self.fire(op, 1);
                } else {
                    self.bend_time.push(None);
                    self.fire(op, 2);
                }
            }
            "SeqAct_DrawText" => {
                // Show, Hide
                let text = self.read(op, "String").into_iter().find_map(|v| if let Val::Str(s) = v { Some(s) } else { None });
                self.challenge_fx.push(crate::challenge::ChallengeFx::Text(if input == 0 { text.or_else(|| self.ps(op, "DrawText")) } else { None }));
                self.fire(op, 0);
            }
            "SeqAct_ConvertToString" => {
                let parts: Vec<String> = self
                    .read(op, "Inputs")
                    .into_iter()
                    .map(|v| match v {
                        Val::Str(s) => s,
                        Val::Int(i) => i.to_string(),
                        Val::Float(f) => format!("{f}"),
                        Val::Bool(b) => b.to_string(),
                        _ => String::new(),
                    })
                    .collect();
                let sep = self.ps(op, "VarSeparator").unwrap_or_default();
                self.write(op, "Output", Val::Str(parts.join(&sep)));
                self.fire(op, 0);
            }
            "SeqAct_GetVectorComponents" => {
                if let Some(Val::Vec3(v)) = self.read(op, "Input Vector").into_iter().next() {
                    self.write(op, "X", Val::Float(v[0]));
                    self.write(op, "Y", Val::Float(v[1]));
                    self.write(op, "Z", Val::Float(v[2]));
                }
                self.fire(op, 0);
            }
            "SeqAct_SetVectorComponents" => {
                let c = |s: &Self, d: &str| s.read(op, d).iter().find_map(|v| v.as_f32());
                let mut v = match self.read(op, "Output Vector").into_iter().next() {
                    Some(Val::Vec3(v)) => v,
                    _ => [0.0; 3],
                };
                for (k, d) in ["X", "Y", "Z"].iter().enumerate() {
                    if let Some(x) = c(self, d) {
                        v[k] = x;
                    }
                }
                self.write(op, "Output Vector", Val::Vec3(v));
                self.fire(op, 0);
            }
            "SeqCond_SwitchClass" => {
                // the output named after the object's class, else "Default"
                let class_of = self.read(op, "Object").into_iter().find_map(|v| if let Val::Str(s) = v { Some(s) } else { None }).unwrap_or_default();
                let cls = class_of.rsplit('.').next().unwrap_or(&class_of).to_string();
                let out = self.output_index(op, &cls).or_else(|| self.output_index(op, "Default")).unwrap_or(0);
                self.fire(op, out);
            }
            "DisSeqAct_DLC05_GetDeathInfo" => {
                // the last death the challenge saw: its damage type (Out), or none (Error)
                match self.last_death.clone() {
                    Some(kind) => {
                        self.write(op, "Damage Type", Val::Str(kind));
                        self.fire(op, 0);
                    }
                    None => self.fire(op, 1),
                }
            }
            "SeqAct_AkStopAll" => {
                self.challenge_fx.push(crate::challenge::ChallengeFx::StopAllSounds);
                self.fire(op, 0);
            }
            "ArkSeqAct_ChangePylonConnection" => {
                // (Connect, Disconnect)
                for v in self.read(op, "Target") {
                    if let Some(a) = if let Val::Actor(a) = v { g.actors.get(a as usize) } else { None } {
                        self.pylon_links.push((a.name.clone(), input == 0));
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_RefillWhaleOilBattery" => {
                let pct = self.pf(op, "m_PercentageCharge").unwrap_or(100.0);
                let insts: Vec<u32> = self.read(op, "Battery").iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize) } else { None }).flat_map(|a| a.instances.iter().copied()).collect();
                self.tank_refills.push((insts, pct));
                self.fire(op, 0);
            }
            "DisSeqAct_SetAudioOcclusion" => {
                // (a door shut or opened: its doorway muffles what passes)
                let names: Vec<String> = self.read(op, "Target").iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize).map(|a| a.name.clone()) } else { None }).collect();
                let player = self.read(op, "Occlusion For Player").iter().find_map(|v| v.as_f32()).unwrap_or(0.0);
                let ai = self.read(op, "Occlusion For AI").iter().find_map(|v| v.as_f32()).unwrap_or(0.0);
                self.portal_occlusion.push((names, player, ai));
                self.fire(op, 0);
            }
            "SeqAct_AkStartAmbientSound" => {
                // (Start All, Stop All, Start Target(s), Stop Target(s))
                for t in self.read(op, "Target") {
                    self.effects.push(Effect::Ambient(t, input % 2 == 0));
                }
                self.fire(op, 0);
            }
            "SeqAct_ControlGameMovie" => {
                // play (0) / stop (1): the movie's length in black (its cooked Bink header),
                // then "Movie Completed" (the Tower's title card, then the Prison)
                if input == 0 {
                    let secs = self.pf(op, "movie_secs").unwrap_or(2.0).min(600.0);
                    self.effects.push(Effect::Fade { to: 1.0, time: 0.2 });
                    self.effects.push(Effect::Movie(self.ps(op, "MovieName")));
                    self.latent.push(Latent::After { op, out: 1, left: secs });
                } else {
                    self.effects.push(Effect::Movie(None));
                }
                self.fire(op, 0);
            }
            "SeqAct_CameraFade" => {
                // Dishonored's fade: towards FadeOpacity (default black) over FadeTime
                let to = match self.prop(op, "FadeAlpha") {
                    Some(KVal::List(l)) if l.len() == 2 => match l[1] {
                        KVal::Float(x) => x,
                        _ => 1.0,
                    },
                    _ => self.pf(op, "FadeOpacity").unwrap_or(1.0),
                };
                let time = self.pf(op, "FadeTime").unwrap_or(1.0);
                self.effects.push(Effect::Fade { to, time });
                self.fire(op, 0);
                self.latent.push(Latent::After { op, out: 1, left: time });
            }
            "DisSeqAct_ShowLocationDiscovery" => {
                if let Some(n) = self.ps(op, "m_LocationName") {
                    self.effects.push(Effect::Location(n));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_AttachPickup" => {
                let targets = self.read(op, "Target");
                let items = self.read(op, "Pickup");
                self.attach_pickups.push((targets, items, input == 0));
                self.fire(op, 0);
            }
            "SeqAct_SetPhysics" => {
                let targets = self.read(op, "Target");
                let mode = self.ps(op, "newPhysics").unwrap_or_else(|| "PHYS_None".into());
                self.set_physics.push((targets, mode));
                self.fire(op, 0);
            }
            "DisSeqAct_OverridePossess" => {
                // override (0) / clear (1)
                let targets = self.read(op, "Target");
                let exit = self.read(op, "Exit Point").iter().find_map(|v| self.pos_of(v));
                self.possess_overrides.push((targets, (input == 0).then_some(op), exit));
                self.fire(op, 0);
            }
            "DisSeqAct_SetRainEmitter" => {
                // (its "Rain Start" / "Rain Stop" come from the world as Corvo walks in and out)
                let target = self.read(op, "Target");
                let drops = self.pi(op, "m_NumRainDrops").unwrap_or(40).max(1) as u32;
                let delay = self.pf(op, "m_fRainStartDelay").unwrap_or(0.0);
                self.rain = Some((op, target, drops, delay));
            }
            "DisSeqAct_ToggleTutorial" => {
                // enable (0), disable (1), toggle (2)
                self.tutorials_off = match input {
                    0 => false,
                    1 => true,
                    _ => !self.tutorials_off,
                };
                self.fire(op, 0);
            }
            "DisSeqAct_ShowTargetNotification" => {
                // the HUD's target card (`TargetNotification`): who, the portrait, and what became
                // of them (`EDisChapterTargetState`: assassinated, neutralized, rescued, spared;
                // "active" shows nothing)
                if input == 0 {
                    let state = self.ps(op, "m_TargetNotification").unwrap_or_else(|| "DTSN_TargetEliminated".into());
                    let kind = ["DTSN_TargetEliminated", "DTSN_TargetNeutralized", "DTSN_TargetRescued", "DTSN_TargetSpared"].iter().position(|s| *s == state);
                    if let Some(kind) = kind {
                        self.target_cards.push((self.ps(op, "target_name").unwrap_or_default(), self.ps(op, "target_portrait").unwrap_or_default(), kind as u8));
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_SetTutorialMessage" | "DisSeqAct_ShowHUDMessage" => {
                if input == 0 && !(self.tutorials_off && class == "DisSeqAct_SetTutorialMessage") {
                    let text = g.ops[op as usize].props.iter().find_map(|(k, v)| match v {
                        KVal::Str(s) if k.contains("Message") || k.contains("Text") => Some(s.clone()),
                        _ => None,
                    });
                    if let Some(t) = text {
                        let t = clean_text(&t);
                        self.effects.push(if class == "DisSeqAct_SetTutorialMessage" { Effect::Tutorial(t) } else { Effect::Message(t) });
                    }
                }
                self.fire(op, 0);
            }

            // ---------------------------------------------------- objectives
            "DisSeqAct_AddObjective" => {
                for v in self.read(op, "Objective") {
                    if let Val::Str(path) = v {
                        if !self.objectives.contains(&path) {
                            if let Some(o) = g.objectives.iter().find(|o| o.path == path) {
                                for t in &o.tasks {
                                    self.tasks.entry(t.path.clone()).or_insert((TaskState::Active, t.hidden));
                                }
                                // (its tasks to do, shown one after another)
                                let tasks = o.tasks.iter().filter(|t| self.tasks.get(&t.path).is_some_and(|s| s.0 == TaskState::Active && !s.1)).map(|t| (t.text.clone(), crate::objnotify::ObjState::Added)).collect();
                                self.effects.push(Effect::Objective(crate::objnotify::ObjectiveNotice { name: o.text.clone(), state: crate::objnotify::ObjState::Added, tasks }));
                            }
                            self.objectives.push(path.clone());
                            self.tasks.insert(path, (TaskState::Active, false));
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_CompleteObjective" | "DisSeqAct_FailObjective" | "DisSeqAct_RemoveObjective" => {
                for v in self.read(op, "Objective") {
                    if let Val::Str(path) = v {
                        if class.contains("Remove") {
                            self.objectives.retain(|o| *o != path);
                        } else {
                            let s = if class.contains("Complete") { TaskState::Completed } else { TaskState::Failed };
                            if let Some(o) = g.objectives.iter().find(|o| o.path == path) {
                                let state = if s == TaskState::Completed { crate::objnotify::ObjState::Completed } else { crate::objnotify::ObjState::Failed };
                                self.effects.push(Effect::Objective(crate::objnotify::ObjectiveNotice { name: o.text.clone(), state, tasks: Vec::new() }));
                            }
                            self.tasks.insert(path.clone(), (s, false));
                            for ev in 0..g.ops.len() as u32 {
                                if g.ops[ev as usize].class == "DisSeqEvent_ObjectiveCompleted"
                                    && matches!(g.ops[ev as usize].props.get("m_pObjective"), Some(KVal::Str(p)) if *p == path)
                                {
                                    self.event(ev, 0, None);
                                }
                            }
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_UpdateTaskTarget" => {
                let target = self.read(op, "Target").into_iter().find_map(|v| if let Val::Actor(a) = v { Some(a) } else { None });
                let label = self.ps(op, "m_TargetName").unwrap_or_default();
                // (a task can have several: the three walkway keys; the others are `path#index`)
                let index = self.pi(op, "m_TargetIndex").unwrap_or(0);
                for v in self.read(op, "Task") {
                    let Val::Str(path) = v else { continue };
                    let key = if index > 0 { format!("{path}#{index}") } else { path };
                    match (input, target) {
                        (0, Some(a)) => {
                            self.task_targets.insert(key, (a, label.clone()));
                        }
                        (2, Some(a)) if !self.task_targets.contains_key(&key) => {
                            self.task_targets.insert(key, (a, label.clone()));
                        }
                        _ => {
                            self.task_targets.remove(&key);
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_SetTaskState" | "DisSeqAct_SetTaskHidden" => {
                let state = match self.ps(op, "m_TaskState").as_deref() {
                    Some("DTS_Completed") => TaskState::Completed,
                    Some("DTS_Failed") => TaskState::Failed,
                    Some("DTS_Inactive") => TaskState::Inactive,
                    _ => TaskState::Active,
                };
                for v in self.read(op, "Task") {
                    let Val::Str(path) = v else { continue };
                    if class == "DisSeqAct_SetTaskHidden" {
                        let hide = self.pb(op, "m_bHidden").unwrap_or(input == 0);
                        let e = self.tasks.entry(path.clone()).or_insert((TaskState::Active, hide));
                        let shown = e.1 && !hide && e.0 == TaskState::Active;
                        e.1 = hide;
                        // a task coming out: the objective updated
                        if shown {
                            if let Some((o, t)) = g.objectives.iter().find_map(|o| o.tasks.iter().find(|t| t.path == path).map(|t| (o, t))) {
                                if self.objectives.contains(&o.path) {
                                    self.effects.push(Effect::Objective(crate::objnotify::ObjectiveNotice { name: o.text.clone(), state: crate::objnotify::ObjState::Updated, tasks: vec![(t.text.clone(), crate::objnotify::ObjState::Added)] }));
                                }
                            }
                        }
                        continue;
                    }
                    let prev = self.tasks.get(&path).map(|t| t.0);
                    self.tasks.insert(path.clone(), (state, false));
                    if prev != Some(state) {
                        // the objective updated with the task (one shown, not hidden)
                        let ev = match state {
                            TaskState::Completed => Some(crate::objnotify::ObjState::Completed),
                            TaskState::Failed => Some(crate::objnotify::ObjState::Failed),
                            TaskState::Active if prev.is_some_and(|p| p == TaskState::Inactive) => Some(crate::objnotify::ObjState::Added),
                            _ => None,
                        };
                        if let Some(ev) = ev {
                            if let Some((o, t)) = g.objectives.iter().find_map(|o| o.tasks.iter().find(|t| t.path == path).map(|t| (o, t))) {
                                if self.objectives.contains(&o.path) && !t.hidden {
                                    self.effects.push(Effect::Objective(crate::objnotify::ObjectiveNotice { name: o.text.clone(), state: crate::objnotify::ObjState::Updated, tasks: vec![(t.text.clone(), ev)] }));
                                }
                            }
                        }
                        for ev in 0..g.ops.len() as u32 {
                            let e = &g.ops[ev as usize];
                            if e.class == "DisSeqEvent_TaskStateChanged" && matches!(e.props.get("m_pTask"), Some(KVal::Str(p)) if *p == path) {
                                if let Some(i) = self.output_index(ev, &format!("{state:?}")) {
                                    self.event(ev, i, None);
                                }
                            }
                        }
                    }
                }
                self.fire(op, 0);
            }

            // ---------------------------------------------------- timed
            "SeqAct_Interp" | "DisSeqAct_DLC05_Interp" => {
                // inputs: Play, Reverse, Stop, Pause, Change Dir
                let existing = self.latent.iter().position(|l| matches!(l, Latent::Interp { op: o, .. } if *o == op));
                match input {
                    0 | 1 | 4 => {
                        let forward = match input {
                            0 => true,
                            1 => false,
                            _ => match existing.map(|i| &self.latent[i]) {
                                Some(Latent::Interp { rate, .. }) => *rate < 0.0,
                                _ => true,
                            },
                        };
                        let play_rate = self.pf(op, "PlayRate").unwrap_or(1.0).max(0.01);
                        if let Some(i) = existing {
                            if let Latent::Interp { rate, paused, .. } = &mut self.latent[i] {
                                *rate = if forward { play_rate } else { -play_rate };
                                *paused = false;
                            }
                        } else {
                            let data = self.link_vars(op, "Data");
                            let prop = |k: &str| data.iter().find_map(|&v| g.vars[v as usize].props.get(k).cloned());
                            let len = match prop("InterpLength") {
                                Some(KVal::Float(f)) => f,
                                _ => 0.0,
                            };
                            let matinee = match prop("Matinee") {
                                Some(KVal::Int(i)) => Some(i as u32),
                                _ => None,
                            };
                            // the actors linked to each group (variable links named after it)
                            let binds = matinee
                                .and_then(|m| g.matinees.get(m as usize))
                                .map(|m| {
                                    m.groups
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, gr)| !gr.name.is_empty())
                                        .map(|(gi, gr)| {
                                            let actors = self.read(op, &gr.name).into_iter().filter_map(|v| if let Val::Actor(a) = v { Some(a) } else { None }).collect();
                                            (gi as u32, actors)
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            // movers continue from where they were left
                            let mut t = self.interp_pos.get(&op).copied().unwrap_or(0.0);
                            if forward && t >= len {
                                t = 0.0;
                            } else if !forward && t <= 0.0 {
                                t = len;
                            }
                            // without cooked tracks (or for quick tests) the matinee only times out
                            let len = if matinee.is_none() && !full_matinee() { len.min(0.5) } else { len };
                            let looping = self.pb(op, "bLooping").unwrap_or(false);
                            self.latent.push(Latent::Interp { op, t, len, rate: if forward { play_rate } else { -play_rate }, looping, paused: false, matinee, binds });
                        }
                        self.fire_desc(op, "Started");
                    }
                    2 => {
                        if let Some(i) = existing {
                            if let Latent::Interp { t, .. } = self.latent[i] {
                                self.interp_pos.insert(op, t);
                            }
                            self.latent.remove(i);
                        }
                        self.fire_desc(op, "Stopped");
                    }
                    3 => {
                        if let Some(i) = existing {
                            if let Latent::Interp { paused, .. } = &mut self.latent[i] {
                                *paused = !*paused;
                            }
                        }
                    }
                    _ => {}
                }
            }
            "SeqAct_AttachToEvent" => {
                let attachees = self.read(op, "Attachee");
                if let Some(KVal::List(evs)) = self.prop(op, "EventLinks").cloned() {
                    for e in evs {
                        let KVal::Op(ev) = e else { continue };
                        for a in &attachees {
                            if let Val::Actor(actor) = a {
                                let list = self.events_of.entry(*actor).or_default();
                                if !list.contains(&ev) {
                                    list.push(ev);
                                }
                            }
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_RiverKrustSpitAtTarget" => {
                let at = self.read(op, "SpitTarget").into_iter().find_map(|v| if let Val::Actor(a) = v { g.actors.get(a as usize).map(|a| Vec3::from(a.position)) } else { None });
                if let Some(at) = at {
                    for t in self.read(op, "RiverKrust") {
                        if let Val::Actor(a) = t {
                            if let Some(k) = g.actors.get(a as usize).and_then(|a| a.krust) {
                                self.krust_spits.push((k, at));
                            }
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_ActivateProjectileLauncher" => {
                for t in self.read(op, "Target") {
                    if let Val::Actor(a) = t {
                        if let Some(tr) = g.actors.get(a as usize).and_then(|a| a.trap) {
                            self.launches.push(tr);
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_ActivateRatSpawner" => {
                for t in self.read(op, "Target") {
                    if let Val::Actor(a) = t {
                        if let Some(r) = g.actors.get(a as usize).and_then(|a| a.rat_spawner) {
                            self.rat_spawns.push(r);
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_SetPlayerTravelDestination" => {
                self.travel = self.ps(op, "m_Tag");
                self.fire(op, 0);
            }
            "DisSeqAct_GotoPlayerTravelDestination" => {
                if let Some(t) = self.travel.clone() {
                    self.effects.push(Effect::Travel(t));
                }
                self.fire(op, 0);
            }

            // ---------------------------------------------------- the campaign
            "SeqAct_MultiLevelStreaming" | "SeqAct_LevelStreaming" => {
                let names: Vec<String> = match self.prop(op, "Levels") {
                    Some(KVal::List(l)) => l.iter().filter_map(|v| if let KVal::Str(s) = v { Some(s.clone()) } else { None }).collect(),
                    _ => self.ps(op, "LevelName").into_iter().collect(),
                };
                for n in names {
                    if input == 0 {
                        self.load_level(&n);
                    } else {
                        self.unload_level(&n);
                    }
                }
                self.fire(op, 0);
            }
            "SeqAct_PrepareMapChange" => {
                self.pending_map = self.ps(op, "MainLevelName").filter(|m| !m.is_empty() && m != "None");
                self.pending_levels = match self.prop(op, "InitiallyLoadedSecondaryLevelNames") {
                    Some(KVal::List(l)) => l.iter().filter_map(|v| if let KVal::Str(s) = v { Some(s.clone()) } else { None }).collect(),
                    _ => Vec::new(),
                };
                if let Some(i) = self.output_index(op, "Finished") {
                    self.fire(op, i);
                } else {
                    self.fire(op, 0);
                }
            }
            "SeqAct_CommitMapChange" => {
                // latent: carries on in the next map (see `campaign::resume`)
                match self.pending_map.take() {
                    Some(map) => self.campaign_fx.push(crate::campaign::CampaignFx::ChangeMap { map, tag: self.travel.clone(), op, levels: std::mem::take(&mut self.pending_levels) }),
                    None => self.fire(op, 0),
                }
            }
            "DisSeqAct_AddDarkness" => {
                self.darkness = (self.darkness + self.pi(op, "m_Value").unwrap_or(0)).clamp(0, 100);
                self.campaign_fx.push(crate::campaign::CampaignFx::Darkness(self.darkness));
                self.fire(op, 0);
            }
            "DisSeqAct_MissionStatsTracking" => {
                let tweak = self.ps(op, "m_pMissionStatsTweaks").unwrap_or_default();
                let tweak = tweak.rsplit('.').next().unwrap_or(&tweak).to_string();
                if input == 0 {
                    self.mission_chaos = 0;
                    self.campaign_fx.push(crate::campaign::CampaignFx::MissionBegin(tweak));
                } else {
                    // the mission's chaos joins the campaign's
                    self.darkness += self.mission_chaos;
                    self.mission_chaos = 0;
                    self.campaign_fx.push(crate::campaign::CampaignFx::MissionEnd { tweak, darkness: self.darkness });
                }
                self.fire(op, 0);
            }
            "DisSeqAct_ShowMissionStats" => {
                let tweak = self.ps(op, "m_pMissionStatsTweaks").unwrap_or_default();
                let tweak = tweak.rsplit('.').next().unwrap_or(&tweak).to_string();
                self.campaign_fx.push(crate::campaign::CampaignFx::ShowStats { tweak, op });
            }
            "DisSeqAct_ApplyPlayerLoadout" => {
                let l = self.ps(op, "m_pPlayerLoadout").unwrap_or_default();
                self.campaign_fx.push(crate::campaign::CampaignFx::Loadout(l.rsplit('.').next().unwrap_or(&l).to_string()));
                self.fire(op, 0);
            }
            "DisSeqAct_AddAbstractItem" => {
                if let Some(i) = self.ps(op, "m_pItemToAdd") {
                    // `ChapterNotes_twk.HUB.ChapterLog_Hub_02` -> `HUB.ChapterLog_Hub_02`
                    self.campaign_fx.push(crate::campaign::CampaignFx::AbstractItem(i.split_once('.').map(|x| x.1.to_string()).unwrap_or(i)));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_SetCurrentChapter" => {
                if let Some(c) = self.ps(op, "m_ChapterTag") {
                    self.campaign_fx.push(crate::campaign::CampaignFx::Chapter(c));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_IncrementPlayerStat" => {
                if let Some(s) = self.ps(op, "m_StatToIncrement") {
                    self.campaign_fx.push(crate::campaign::CampaignFx::IncrementStat(s));
                }
                self.fire(op, 0);
            }
            "DisSeqAct_AIGoToActor" => {
                if input == 0 {
                    let dest = self.read(op, "Destination").into_iter().find_map(|v| if let Val::Actor(a) = v { g.actors.get(a as usize) } else { None });
                    if let Some(d) = dest {
                        for t in self.read(op, "Target") {
                            self.effects.push(Effect::GoTo(t, Vec3::from(d.position)));
                        }
                    }
                    self.fire(op, 0);
                    self.latent.push(Latent::After { op, out: 1, left: 4.0 });
                }
            }

            // ---------------------------------------------------- dialogue
            "DisSeqAct_DialogInputs" => {
                let name = opd.inputs.get(input as usize).map(|i| i.desc.clone()).unwrap_or_default();
                // a character's tree named on the op (spoken by its target), else the one-shot
                // dialogue actor's own
                let named_tree = match self.prop(op, "m_pDialogTree") {
                    Some(KVal::Str(p)) if !p.is_empty() => self.tree_by_path.get(p).copied(),
                    _ => None,
                };
                let mut speakers: Vec<(u32, Option<usize>)> = Vec::new();
                for v in self.read(op, "DialogOneShot") {
                    if let Val::Actor(a) = v {
                        speakers.push((a, g.dialogs.iter().find(|d| d.actor == a).map(|d| d.tree as usize)));
                        // (its character answers for it)
                        if named_tree.is_none() {
                            for t in self.read(op, "Target") {
                                if let Val::Actor(p) = t {
                                    self.pawn_oneshot.insert(p, a);
                                }
                            }
                        }
                    }
                }
                if let Some(t) = named_tree {
                    for v in self.read(op, "Target") {
                        if let Val::Actor(a) = v {
                            speakers.push((a, Some(t)));
                        }
                    }
                }
                for (actor, tree) in speakers {
                    self.walk_speaker = Some(actor);
                    let conv = tree.and_then(|t| {
                        self.speaker_tree.insert(actor, t);
                        self.enter_tree(t, &name).map(|c| (t, c))
                    });
                    match conv {
                        Some((t, ci)) => self.start_conversation(actor, t, ci),
                        None => self.latent.push(Latent::Dialog { actor, input: name.clone(), left: 2.0 }),
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_DialogScriptedChoice" => {
                // the player answers (choice.rs), down the output chosen
                let options: Vec<(usize, String)> = opd.outputs.iter().enumerate().filter(|(_, o)| !o.desc.eq_ignore_ascii_case("Player Busy")).map(|(i, o)| (i, o.desc.clone())).collect();
                if options.is_empty() {
                    self.fire(op, 0);
                } else {
                    self.effects.push(Effect::Choice(op, options));
                }
            }

            // ---------------------------------------------------- switches over Corvo's means
            "DisSeqAct_AdrenalineToggle" | "DisSeqAct_ToggleJournal" | "DisSeqAct_ToggleChoke" | "DisSeqAct_EnableSystemicTutorials" => {
                // enable (0), disable (1), toggle (2)
                let sw = &mut self.switches;
                let flag = match class.as_str() {
                    "DisSeqAct_AdrenalineToggle" => &mut sw.adrenaline_off,
                    "DisSeqAct_ToggleJournal" => &mut sw.journal_off,
                    "DisSeqAct_ToggleChoke" => &mut sw.choke_off,
                    _ => &mut sw.systemic_off,
                };
                *flag = match input {
                    0 => false,
                    1 => true,
                    _ => !*flag,
                };
                self.fire(op, 0);
            }
            "DisSeqAct_ToggleAchievementEval" => {
                // `eDisKismetToggleAchievementEval_Enable` / `_Disable` / `_Toggle`
                let how = self.ps(op, "m_eToggle").or_else(|| self.ps(op, "m_Toggle")).unwrap_or_default();
                let off = &mut self.switches.achievements_off;
                *off = if how.ends_with("Disable") || (how.is_empty() && input == 1) {
                    true
                } else if how.ends_with("Toggle") || (how.is_empty() && input == 2) {
                    !*off
                } else {
                    false
                };
                self.fire(op, 0);
            }
            // ---------------------------------------------------- characters and things
            "DisSeqAct_SeverLimb" => {
                let joint = self.ps(op, "m_JointName").unwrap_or_else(|| "neck_jnt".into());
                let gore = self.pb(op, "m_bShowGore").unwrap_or(true);
                self.severs.push((self.read(op, "Target"), joint, gore));
                self.fire(op, 0);
            }
            "DisSeqAct_PlayMusicBox" => {
                // force play (0), as it wishes (1), prevent play (2)
                for t in self.read(op, "Target") {
                    if let Some(s) = actor_spawner(&g, &t) {
                        match input {
                            0 => {
                                self.music_box.insert(s, true);
                            }
                            2 => {
                                self.music_box.insert(s, false);
                            }
                            _ => {
                                self.music_box.remove(&s);
                            }
                        }
                    }
                }
                self.fire(op, 0);
            }
            "SeqAct_ModifyProperty" => {
                // (the spawners' own: what they spawn takes them)
                let props: Vec<(String, String)> = match self.prop(op, "Properties") {
                    Some(KVal::List(l)) => l
                        .iter()
                        .filter_map(|p| match p {
                            KVal::List(kv) => match (kv.first(), kv.get(1)) {
                                (Some(KVal::Str(k)), Some(KVal::Str(v))) => Some((k.clone(), v.clone())),
                                _ => None,
                            },
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                for t in self.read(op, "Target") {
                    if let Some(s) = actor_spawner(&g, &t) {
                        let e = self.spawn_props.entry(s).or_default();
                        for (k, v) in &props {
                            e.retain(|(k2, _)| !k2.eq_ignore_ascii_case(k));
                            e.push((k.clone(), v.clone()));
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_SetBoneCharmEffect" => {
                let id = self.pi(op, "m_Effect").unwrap_or(0);
                for t in self.read(op, "Target") {
                    if let Some(at) = self.pos_of(&t) {
                        self.charm_effects.push((at, id));
                    }
                }
                self.fire(op, 0);
            }
            // ---------------------------------------------------- the AI's places
            "DisSeqAct_ForbiddenZoneOverride" => {
                // set (0), clear (1)
                self.zone_overrides.push((self.read(op, "Target"), self.read(op, "Owning Factions"), self.read(op, "Forbidden Factions"), input == 0));
                self.fire(op, 0);
            }
            "DisSeqAct_AIProtectNeutralsOverride" => {
                // protect (0), don't care (1)
                self.protect_overrides.push((self.read(op, "Target"), Some(input == 0)));
                self.fire(op, 0);
            }
            "DisSeqAct_NPCIgnoreRBDamages" => {
                // allow (0: they hurt it), disallow (1)
                self.npc_flags.push((self.read(op, "Target"), 0, input == 1));
                self.fire(op, 0);
            }
            "DisSeqAct_NPCDisableTeleportOnNavmesh" => {
                // start (0), stop (1), reset (2)
                self.npc_flags.push((self.read(op, "NPC"), 1, input == 0));
                self.fire(op, 0);
            }
            "DisSeqAct_AIAmbush" => {
                // start (0), stop (1)
                self.ambush_cmds.push((self.read(op, "Target"), self.read(op, "Ambush Point"), input == 0));
                if let Some(i) = self.output_index(op, "Out") {
                    self.fire(op, i);
                }
            }
            // ---------------------------------------------------- level states
            "DisSeqAct_SaveLevelState" => {
                self.level_state_save = Some(self.pb(op, "m_bPartial").unwrap_or(false));
                self.fire(op, 0);
            }
            "DisSeqAct_DiscardLevelState" => {
                if let Some(n) = self.ps(op, "m_LevelName").filter(|n| !n.is_empty() && n != "None") {
                    self.level_state_discards.push(n);
                }
                self.fire(op, 0);
            }
            "DisSeqAct_DiscardAllLevelStates" => {
                self.level_state_discards.push("*".into());
                self.fire(op, 0);
            }
            "DisSeqAct_OutsiderConfig" => {
                // `m_DepthPriorityGroup` SDPG_Foreground (its lighting channels uninitialised:
                // as they were)
                if self.ps(op, "m_DepthPriorityGroup").is_none_or(|g| g.ends_with("Foreground")) {
                    for t in self.read(op, "Target") {
                        if let Some(s) = actor_spawner(&g, &t) {
                            if !self.foreground.contains(&s) {
                                self.foreground.push(s);
                            }
                        }
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_WatchTower" => {
                self.tower_cmds.push((self.read(op, "Target"), input as u8, self.read(op, "Track")));
                self.fire(op, 0);
            }
            "SeqAct_SetDamageInstigator" => {
                // (the PA speakers' prefab: shot, the player is held to have done it)
                let by = self.read(op, "Damage Instigator").into_iter().next().unwrap_or(Val::None);
                for t in self.read(op, "Target") {
                    if let Val::Actor(a) = t {
                        self.instigators.insert(a, by.clone());
                    }
                }
                self.fire(op, 0);
            }
            "DisSeqAct_GetPlayerStat" => {
                // `m_Stat`: `DisTweaks_PlayerStats.EDisPlayerStat`
                let name = PLAYER_STATS.get(self.pi(op, "m_Stat").unwrap_or(-1).max(0) as usize).copied().unwrap_or("");
                let v = self.stat_values.get(name).copied().unwrap_or(0);
                self.write(op, "Result", Val::Int(v as i32));
                if self.pb(op, "m_bResetStat").unwrap_or(false) && !name.is_empty() {
                    self.stat_resets.push(name.to_string());
                    self.stat_values.insert(name.to_string(), 0);
                }
                self.fire(op, 0);
            }
            // ---------------------------------------------------- places and motion
            "SeqAct_SetVelocity" => {
                // `Velocity Dir` scaled to `Velocity Mag` where one is given (else as it is)
                let dir = self.read(op, "Velocity Dir").into_iter().find_map(|v| if let Val::Vec3(d) = v { Some(d) } else { None });
                let mag = self.read(op, "Velocity Mag").into_iter().find_map(|v| v.as_f32()).or_else(|| self.pf(op, "VelocityMag")).unwrap_or(0.0);
                if let Some(d) = dir.or_else(|| match self.prop(op, "VelocityDir") {
                    Some(KVal::Vec3(v)) => Some(*v),
                    _ => None,
                }) {
                    let ue = Vec3::from(d);
                    let ue = if mag > 0.0 { ue.normalize_or_zero() * mag } else { ue };
                    let v = Vec3::from(dhcook::xform::ue_point(ue.to_array()));
                    for t in self.read(op, "Target") {
                        self.effects.push(Effect::Velocity(t, v));
                    }
                }
                self.fire(op, 0);
            }
            "SeqAct_SetLocation" => {
                let vec = |vals: Vec<Val>| vals.into_iter().find_map(|v| if let Val::Vec3(d) = v { Some(d) } else { None });
                let at = vec(self.read(op, "Location")).or_else(|| match self.prop(op, "LocationValue") {
                    Some(KVal::Vec3(v)) if self.pb(op, "bSetLocation").unwrap_or(true) => Some(*v),
                    _ => None,
                });
                let rot = vec(self.read(op, "Rotation")).or_else(|| match self.prop(op, "RotationValue") {
                    Some(KVal::Vec3(v)) if self.pb(op, "bSetRotation").unwrap_or(false) => Some(*v),
                    _ => None,
                });
                let pos = at.map(|p| Vec3::from(dhcook::xform::ue_point(p)));
                let yaw = rot.map(|r| dhcook::xform::ue_yaw_to_bevy(r[1] as i32));
                for t in self.read(op, "Target") {
                    self.effects.push(Effect::Place(t, pos, yaw));
                }
                self.fire(op, 0);
            }
            "SeqAct_GetLocationAndRotation" => {
                if let Some(t) = self.read(op, "Target").into_iter().next() {
                    if let Some(p) = self.pos_of(&t) {
                        self.write(op, "Location", Val::Vec3([p.x / dhcook::xform::UNIT, p.z / dhcook::xform::UNIT, p.y / dhcook::xform::UNIT]));
                    }
                    let yaw = match &t {
                        Val::Actor(a) => g.actors.get(*a as usize).map(|a| a.yaw),
                        _ => None,
                    };
                    if let Some(y) = yaw {
                        // (UE's rotation units, 65536 a turn; the cooked yaw is the game's)
                        let ue = -(y - std::f32::consts::FRAC_PI_2) * 32768.0 / std::f32::consts::PI;
                        self.write(op, "Rotation Vector", Val::Vec3([0.0, ue, 0.0]));
                    }
                }
                self.fire(op, 0);
            }
            "SeqAct_ConsoleCommand" => {
                // the maps' own way on when played without the campaign ("start L_Tower_P")
                let cmds: Vec<String> = match self.prop(op, "Commands") {
                    Some(KVal::List(l)) => l.iter().filter_map(|c| if let KVal::Str(s) = c { Some(s.clone()) } else { None }).collect(),
                    _ => Vec::new(),
                };
                for c in cmds {
                    let mut w = c.split_whitespace();
                    if let (Some(verb), Some(map)) = (w.next(), w.next()) {
                        if verb.eq_ignore_ascii_case("start") || verb.eq_ignore_ascii_case("open") {
                            self.effects.push(Effect::StartMap(map.to_string()));
                        }
                    }
                }
                self.fire(op, 0);
            }
            // ---------------------------------------------------- the editor's own
            // (logging, debug console triggers, texture streaming hints, the physics bones'
            // sync, the sound capture box: nothing in the game; `SeqAct_WaitForLevelsVisible`:
            // the streamed levels are in as soon as they're loaded)
            "SeqAct_Log" | "SeqAct_StreamInTextures" | "SeqAct_WaitForLevelsVisible" | "SeqAct_UpdatePhysBonesFromAnim" | "DisSeqAct_SetActiveSoundCaptureBox" => {
                if !opd.outputs.is_empty() {
                    self.fire(op, 0);
                }
            }

            // events only fire from the world
            c if c.starts_with("SeqEvent") || c.starts_with("DisSeqEvent") || c.starts_with("SeqEvt") || c.starts_with("DisSeqEvt") => {}
            // everything else passes through
            _ => {
                if !opd.outputs.is_empty() {
                    self.fire(op, 0);
                }
            }
        }
    }

    /// Raise a dialogue actor's output named `name` (`DisSeqEvent_DialogOutputs`); whether it
    /// has one.
    fn dialog_output(&mut self, actor: Option<u32>, name: &str) -> bool {
        let Some(actor) = actor else { return false };
        let mut hit = false;
        for ev in self.events_of.get(&actor).cloned().unwrap_or_default() {
            if self.g.ops[ev as usize].class == "DisSeqEvent_DialogOutputs" {
                if let Some(i) = self.output_index(ev, name) {
                    self.event(ev, i, None);
                    hit = true;
                }
            }
        }
        hit
    }

    /// The player answered a conversation's choice (its speaker's): on down that option.
    fn answer_conversation(&mut self, actor: u32, step: usize) {
        if self.trace {
            info!("kismet: conversation answer: actor {actor} step {step} ({} conversations)", self.convs.len());
        }
        if let Some(c) = self.convs.iter_mut().find(|c| c.0 == actor) {
            c.3 = step;
            c.4 = 0.0;
        }
    }

    /// Advance the conversations: subtitles, matinees and the events they raise.
    fn tick_conversations(&mut self, dt: f32) {
        let g = self.g.clone();
        let mut convs = std::mem::take(&mut self.convs);
        for c in convs.iter_mut() {
            c.4 -= dt;
            let Some(steps) = g.dialog_trees.get(c.1).and_then(|t| t.conversations.get(c.2)).map(|cv| &cv.steps) else {
                c.3 = usize::MAX;
                continue;
            };
            let mut voice_len = None;
            while c.4 <= 0.0 && c.3 < steps.len() {
                match &steps[c.3] {
                    dhcook::format::KDialogStep::Voice(event) => {
                        // the speaker's voice; its length times the line
                        self.effects.push(Effect::Voice(event.clone(), Some(Val::Actor(c.0))));
                        voice_len = crate::audio::event_length(event);
                    }
                    dhcook::format::KDialogStep::Line(text, speaker) => {
                        let dur = voice_len.take().map(|l| l + 0.25).unwrap_or(1.2 + text.chars().count() as f32 / 15.0);
                        // slot 0 is the tree's own speaker
                        self.effects.push(Effect::Line(text.clone(), dur, (*speaker == 0).then_some(c.0)));
                        c.4 = dur;
                    }
                    dhcook::format::KDialogStep::SetFlag(f) => {
                        self.flags.insert(f.clone(), true);
                    }
                    // the player answers (or, not answering, the conversation ends)
                    dhcook::format::KDialogStep::Choice(options) => {
                        let opts: Vec<(usize, String)> = options.iter().map(|(t, at)| (*at as usize, t.clone())).collect();
                        if self.trace {
                            info!("kismet: conversation choice: actor {} {:?}", c.0, opts);
                        }
                        self.effects.push(Effect::Choice(CONV_CHOICE | c.0, opts));
                        c.3 = steps.len();
                        c.4 = 30.0;
                        continue;
                    }
                    dhcook::format::KDialogStep::End => {
                        c.3 = steps.len();
                        continue;
                    }
                    dhcook::format::KDialogStep::Matinee(guid) => {
                        for (i, op) in g.ops.iter().enumerate() {
                            if op.class == "SeqAct_Interp" && matches!(op.props.get("m_MatineeGUID"), Some(KVal::Str(x)) if x == guid) {
                                self.queue.push_back((i as u32, 0));
                            }
                        }
                        c.4 = 0.6;
                    }
                    dhcook::format::KDialogStep::Remote(name) => {
                        // the dialogue actor's own outputs first, else a level remote event
                        let mut hit = false;
                        for ev in self.events_of.get(&c.0).cloned().unwrap_or_default() {
                            if g.ops[ev as usize].class == "DisSeqEvent_DialogOutputs" {
                                if let Some(i) = self.output_index(ev, name) {
                                    self.event(ev, i, None);
                                    hit = true;
                                }
                            }
                        }
                        if !hit {
                            for ev in self.remote.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default() {
                                self.event(ev, 0, None);
                            }
                        }
                    }
                }
                c.3 += 1;
            }
        }
        convs.retain(|c| c.3 < g.dialog_trees.get(c.1).and_then(|t| t.conversations.get(c.2)).map(|cv| cv.steps.len()).unwrap_or(0) || c.4 > 0.0);
        self.convs.extend(convs);
    }

    /// Start a conversation of a tree, spoken by `actor` (it stops what it was saying).
    fn start_conversation(&mut self, actor: u32, tree: usize, conv: u32) {
        if let Some(c) = self.g.dialog_trees.get(tree).and_then(|t| t.conversations.get(conv as usize)) {
            self.fired_convs.insert(c.path.clone());
            if self.trace {
                info!("kismet: conversation {} ({})", c.label, self.g.dialog_trees[tree].path);
            }
        }
        self.convs.retain(|c| c.0 != actor);
        self.convs.push((actor, tree, conv as usize, 0, 0.0));
    }

    /// The conversation a Kismet input leads to through a tree's hooks (else one labelled
    /// after it).
    fn enter_tree(&mut self, tree: usize, input: &str) -> Option<u32> {
        let g = self.g.clone();
        let t = g.dialog_trees.get(tree)?;
        let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_ascii_lowercase();
        let want = norm(input);
        for (ni, n) in t.nodes.iter().enumerate() {
            let dhcook::format::KDialogNodeKind::Hook { hook, inputs } = &n.kind else { continue };
            if hook != "DDH_KISMET_ACTIVATED" {
                continue;
            }
            let Some(k) = inputs.iter().position(|i| norm(i) == want) else { continue };
            let out = n.outs.get(k).copied().unwrap_or(-1);
            let out = if out < 0 { n.outs.iter().copied().find(|o| *o >= 0).unwrap_or(-1) } else { out };
            if let Some(c) = self.walk_tree(tree, out, 0) {
                return Some(c);
            }
            let _ = ni;
        }
        t.conversations.iter().position(|c| norm(&c.label) == want).map(|c| c as u32)
    }

    /// The nodes a walk would visit (for tracing), without side effects.
    fn trace_walk(&self, tree: usize, node: i32, depth: u32) -> Vec<String> {
        let Some(n) = (node >= 0 && depth < 12).then(|| self.g.dialog_trees.get(tree).and_then(|t| t.nodes.get(node as usize))).flatten() else { return Vec::new() };
        let mut out = vec![format!("{:?}", n.kind)];
        for o in n.outs.iter().filter(|o| **o >= 0).take(2) {
            out.extend(self.trace_walk(tree, *o, depth + 1).into_iter().map(|s| format!("  {s}")));
        }
        out
    }

    /// Follow a tree's graph from a node to the conversation it picks now.
    fn walk_tree(&mut self, tree: usize, node: i32, depth: u32) -> Option<u32> {
        use dhcook::format::KDialogNodeKind as K;
        if node < 0 || depth > 40 {
            return None;
        }
        let g = self.g.clone();
        let t = g.dialog_trees.get(tree)?;
        let n = t.nodes.get(node as usize)?;
        let out = |i: usize| n.outs.get(i).copied().unwrap_or(-1);
        let linked: Vec<i32> = n.outs.iter().copied().filter(|o| *o >= 0).collect();
        match &n.kind {
            K::Conversation(c) => {
                let cv = t.conversations.get(*c as usize)?;
                (!(cv.once && self.fired_convs.contains(&cv.path))).then_some(*c)
            }
            // (the nodes' outputs are the negative one first: a flag unset, a conversation not
            // had yet — the Regent's "Three days late" plays once, the letter is given once)
            K::StoryFlag(f) => {
                let set = self.flag(f);
                self.walk_tree(tree, out(if set { 1 } else { 0 }), depth + 1)
            }
            K::SetStoryFlag(f) => {
                self.flags.insert(f.clone(), true);
                self.walk_tree(tree, out(0), depth + 1)
            }
            K::Fired(p) => {
                let fired = self.fired_convs.contains(p);
                self.walk_tree(tree, out(if fired { 1 } else { 0 }), depth + 1)
            }
            K::TimeLimit(s) => {
                let key = (tree, node as u32);
                let recent = self.node_time.get(&key).is_some_and(|t0| self.time - t0 < *s);
                if recent {
                    self.walk_tree(tree, out(1), depth + 1)
                } else {
                    self.node_time.insert(key, self.time);
                    self.walk_tree(tree, out(0), depth + 1)
                }
            }
            K::Random => {
                if linked.is_empty() {
                    return None;
                }
                let first = rand::random_range(0..linked.len());
                (0..linked.len()).find_map(|k| self.walk_tree(tree, linked[(first + k) % linked.len()], depth + 1))
            }
            K::Sequential => {
                if linked.is_empty() {
                    return None;
                }
                let turn = self.branch_turn.entry((tree, node as u32)).or_insert(0);
                let k = *turn as usize % linked.len();
                *turn += 1;
                self.walk_tree(tree, linked[k], depth + 1)
            }
            K::Remote(name) => {
                // the speaker's dialogue outputs first (the Empress's "Give_Letter"), else a
                // level remote event
                if !self.dialog_output(self.walk_speaker, name) {
                    for ev in self.remote.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default() {
                        self.event(ev, 0, None);
                    }
                }
                self.walk_tree(tree, out(0), depth + 1)
            }
            K::Hook { .. } | K::Pass => linked.into_iter().find_map(|o| self.walk_tree(tree, o, depth + 1)),
        }
    }

    /// The player near a speaker: its tree's "player approaches" hook, and "player loiters"
    /// after a few seconds there.
    fn proximity_hooks(&mut self, dt: f32) {
        let Some(player) = self.player_pos else { return };
        let g = self.g.clone();
        let speakers: Vec<(u32, usize)> = self.speaker_tree.iter().map(|(a, t)| (*a, *t)).collect();
        for (actor, tree) in speakers {
            let Some(pos) = self.speaker_pos.get(&actor).copied() else { continue };
            let d = pos.distance(player);
            let near = d < 4.0;
            let lingered = if d < 3.0 { *self.linger.entry(actor).and_modify(|l| *l += dt).or_insert(0.0) } else {
                self.linger.remove(&actor);
                0.0
            };
            if !near || self.convs.iter().any(|c| c.0 == actor) {
                continue;
            }
            let Some(t) = g.dialog_trees.get(tree) else { continue };
            for (ni, n) in t.nodes.iter().enumerate() {
                let dhcook::format::KDialogNodeKind::Hook { hook, .. } = &n.kind else { continue };
                let due = match hook.as_str() {
                    "DDH_PLAYER_APPROACHES" => true,
                    "DDH_PLAYER_LOITERS" => lingered > 5.0,
                    _ => false,
                };
                // a hook answers once per approach (again after a while)
                let key = (actor, ni as u32);
                if !due || self.hook_time.get(&key).is_some_and(|t0| self.time - t0 < 30.0) {
                    continue;
                }
                self.hook_time.insert(key, self.time);
                self.walk_speaker = Some(actor);
                if let Some(c) = self.walk_tree(tree, ni as i32, 0) {
                    self.start_conversation(actor, tree, c);
                    break;
                }
            }
        }
    }

    /// A conversation we can't play has ended: fire the dialogue actor's outcome that
    /// best matches ("<input>..Done/Finished", else any Done/Finished, else the first).
    fn dialog_finished(&mut self, actor: u32, input: &str) {
        let evs: Vec<u32> = self
            .events_of
            .get(&actor)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|&e| self.g.ops[e as usize].class == "DisSeqEvent_DialogOutputs")
            .collect();
        let input = input.to_ascii_lowercase();
        let ends = |d: &str| (d.contains("done") || d.contains("finish") || d.contains("end")) && !d.contains("couldn") && !d.contains("fail");
        for ev in evs {
            let outs: Vec<String> = self.g.ops[ev as usize].outputs.iter().map(|o| o.desc.to_ascii_lowercase()).collect();
            let pick = outs
                .iter()
                .position(|d| !input.is_empty() && d.contains(&input) && ends(d))
                .or_else(|| outs.iter().position(|d| ends(d)))
                .or(if outs.len() == 1 { Some(0) } else { None });
            if let Some(i) = pick {
                self.event(ev, i, None);
            }
        }
    }

    /// Advance timers and latent actions.
    /// The scripts' runtime state, for save games.
    pub fn save_state(&self) -> VmSave {
        VmSave {
            ops: self.st.len(),
            vals: self.vals.clone(),
            st: self.st.clone(),
            queue: self.queue.iter().copied().collect(),
            timers: self.timers.clone(),
            latent: self.latent.clone(),
            flags: self.flags.clone(),
            objectives: self.objectives.clone(),
            tasks: self.tasks.clone(),
            travel: self.travel.clone(),
            convs: self.convs.clone(),
            time: self.time,
            interp_pos: self.interp_pos.clone(),
            events_of: self.events_of.clone(),
            touching: self.touching.iter().copied().collect(),
            fired_convs: self.fired_convs.iter().cloned().collect(),
            inert: Some(self.inert.clone()),
            task_targets: self.task_targets.clone(),
            switches: Some((self.switches.clone(), self.wheel_off, self.tutorials_off)),
            music_box: self.music_box.clone(),
            spawn_props: self.spawn_props.clone(),
            joints_broken: self.joints_broken.iter().cloned().collect(),
        }
    }

    /// Restore a saved state (of the same cooked scripts).
    pub fn restore_state(&mut self, s: VmSave) -> bool {
        if s.ops != self.st.len() || s.vals.len() != self.vals.len() {
            return false;
        }
        self.vals = s.vals;
        self.st = s.st;
        self.queue = s.queue.into_iter().collect();
        self.timers = s.timers;
        self.latent = s.latent;
        self.flags = s.flags;
        self.objectives = s.objectives;
        self.tasks = s.tasks;
        self.task_targets = s.task_targets;
        self.travel = s.travel;
        self.convs = s.convs;
        self.time = s.time;
        self.interp_pos = s.interp_pos;
        self.events_of = s.events_of;
        self.touching = s.touching.into_iter().collect();
        self.fired_convs = s.fired_convs.into_iter().collect();
        if let Some((sw, wheel, tut)) = s.switches {
            self.switches = sw;
            self.wheel_off = wheel;
            self.tutorials_off = tut;
        }
        self.music_box = s.music_box;
        self.spawn_props = s.spawn_props;
        self.joints_broken = s.joints_broken.into_iter().collect();
        if let Some(i) = s.inert {
            self.inert = i;
        }
        self.convs.retain(|c| self.g.dialog_trees.get(c.1).is_some_and(|t| c.2 < t.conversations.len()));
        self.effects.clear();
        self.npc_seen.clear();
        true
    }

    /// A map's kept state put back on a return to it (`DisSeqAct_SaveLevelState`): its own
    /// scripts as they were, the story so far (flags, objectives, tasks, the campaign's
    /// scripts, the travel destination, the switches) as it is now.
    pub fn restore_level_state(&mut self, s: VmSave) -> bool {
        let (flags, objectives, tasks, task_targets, travel, switches) =
            (self.flags.clone(), self.objectives.clone(), self.tasks.clone(), self.task_targets.clone(), self.travel.clone(), self.switches.clone());
        let campaign = self.campaign_state();
        if !self.restore_state(s) {
            return false;
        }
        self.flags = flags;
        self.objectives = objectives;
        self.tasks = tasks;
        self.task_targets = task_targets;
        self.travel = travel;
        self.switches = switches;
        if let Some((vals, st)) = campaign {
            self.restore_campaign(&vals, &st);
        }
        true
    }

    /// The mission statistics screen of a `DisSeqAct_ShowMissionStats` was closed.
    pub fn stats_closed(&mut self, op: u32) {
        match self.output_index(op, "Closed") {
            Some(i) => self.fire(op, i),
            None => self.fire(op, 0),
        }
        self.pump();
    }

    /// The campaign part's state (variables and op states), to carry into the next map.
    pub fn campaign_state(&self) -> Option<(Vec<Val>, Vec<OpState>)> {
        let (oo, vo) = self.campaign?;
        Some((self.vals[vo as usize..].to_vec(), self.st[oo as usize..].to_vec()))
    }

    fn restore_campaign(&mut self, vals: &[Val], st: &[OpState]) {
        let Some((oo, vo)) = self.campaign else { return };
        if vals.len() == self.vals.len() - vo as usize {
            self.vals[vo as usize..].clone_from_slice(vals);
        }
        if st.len() == self.st.len() - oo as usize {
            self.st[oo as usize..].clone_from_slice(st);
        }
    }

    /// Fire the level scripts' remote events of this name (as SeqAct_ActivateRemoteEvent).
    /// Where something is now: a character (by its spawner's actor), an actor, Corvo.
    pub fn position_of(&self, v: &Val) -> Option<Vec3> {
        match v {
            Val::Player => self.player_pos,
            Val::Actor(a) => self
                .spawner_actor
                .iter()
                .find(|(_, x)| **x == *a)
                .and_then(|(s, _)| self.npc_pos.get(s).copied())
                .or_else(|| self.g.actors.get(*a as usize).map(|x| Vec3::from(x.position))),
            Val::List(l) => l.iter().find_map(|x| self.position_of(x)),
            _ => None,
        }
    }

    /// Activates an op's input (testing: `kop OP [INPUT]`).
    /// Raise the challenge's events (`DisSeqEvent_DLC05_Challenge`: Started 0, Ended 1,
    /// Failed 2, Reset 3).
    pub fn challenge_event(&mut self, out: usize) {
        for i in 0..self.g.ops.len() {
            if self.g.ops[i].class == "DisSeqEvent_DLC05_Challenge" {
                self.event(i as u32, out, None);
            }
        }
    }

    /// The player died in a challenge (`DisSeqEvent_DLC05_PlayerDeath` "Died"): whether the
    /// scripts take it from here.
    pub fn challenge_death(&mut self) -> bool {
        let mut any = false;
        for i in 0..self.g.ops.len() {
            if self.g.ops[i].class == "DisSeqEvent_DLC05_PlayerDeath" && !self.is_inert(i as u32) {
                any |= self.event(i as u32, 0, None);
            }
        }
        any
    }

    /// The challenge's opening, which the game plays itself: the level's matinee nothing in the
    /// scripts starts (the briefing's fly-through, its dialogue).
    pub fn challenge_intro(&mut self) -> usize {
        let intros: Vec<u32> = (0..self.g.ops.len() as u32)
            .filter(|&i| matches!(self.g.ops[i as usize].class.as_str(), "SeqAct_Interp" | "DisSeqAct_DLC05_Interp") && self.linked_inputs[i as usize] == 0 && !self.is_inert(i))
            .collect();
        for &i in &intros {
            self.activate_op(i, 0);
        }
        intros.len()
    }

    pub fn activate_op(&mut self, op: u32, input: u32) {
        if (op as usize) < self.g.ops.len() {
            self.queue.push_back((op, input));
        }
    }

    pub fn remote_event(&mut self, name: &str) {
        for ev in self.remote.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default() {
            self.event(ev, 0, None);
        }
    }

    /// Matinees playing now: (op, cooked matinee, position, actors bound per group).
    pub fn matinees(&self) -> Vec<(u32, u32, f32, &[(u32, Vec<u32>)])> {
        self.latent
            .iter()
            .filter_map(|l| match l {
                Latent::Interp { op, t, matinee: Some(m), binds, .. } => Some((*op, *m, *t, binds.as_slice())),
                _ => None,
            })
            .collect()
    }

    /// Skip the playing cut-scenes (matinees with director cuts) to their end.
    pub fn skip_cinematics(&mut self) {
        let g = self.g.clone();
        for l in &mut self.latent {
            if let Latent::Interp { t, len, rate, matinee: Some(m), .. } = l {
                if *rate > 0.0 && g.matinees.get(*m as usize).is_some_and(|m| !m.cuts.is_empty()) {
                    *t = (*len - 1e-3).max(0.0);
                }
            }
        }
    }

    fn tick(&mut self, dt: f32) {
        self.proximity_hooks(dt);
        self.time += dt;
        // DH_INTERP_LOG: the matinees running, once a second
        if std::env::var("DH_INTERP_LOG").is_ok() && (self.time - dt).floor() != self.time.floor() {
            for l in &self.latent {
                if let Latent::Interp { op, t, len, rate, paused, .. } = l {
                    info!("kismet: interp {}#{op} t={t:.1}/{len:.1} rate {rate} paused {paused}", self.g.ops[*op as usize].name);
                }
            }
        }
        self.tick_conversations(dt);
        let mut due = Vec::new();
        self.timers.retain_mut(|t| {
            t.0 -= dt;
            if t.0 <= 0.0 {
                due.push((t.1, t.2));
                false
            } else {
                true
            }
        });
        for d in due {
            self.queue.push_back(d);
        }
        let mut done: Vec<(u32, usize)> = Vec::new();
        let mut dialogs: Vec<(u32, String)> = Vec::new();
        let mut swung: Vec<(u32, bool)> = Vec::new();
        let mut interps_done = false;
        let mut named_outputs: Vec<(u32, String)> = Vec::new();
        let mut sounds: Vec<(String, Option<u32>)> = Vec::new();
        let mut ended: Vec<(u32, f32)> = Vec::new();
        let mut skipped = false;
        let mut subtitles: Vec<(String, f32)> = Vec::new();
        let paused: Vec<bool> = self.st.iter().map(|s| s.paused).collect();
        let g = self.g.clone();
        self.latent.retain_mut(|l| match l {
            Latent::Delay { op, left } => {
                if !paused[*op as usize] {
                    *left -= dt;
                }
                if *left <= 0.0 {
                    done.push((*op, 0));
                    false
                } else {
                    true
                }
            }
            Latent::Interp { op, t, len, rate, looping, paused, matinee, binds } => {
                if *paused {
                    return true;
                }
                let prev = *t;
                *t = (*t + dt * *rate).clamp(0.0, *len);
                let (lo, hi) = if *rate >= 0.0 { (prev, *t) } else { (*t, prev) };
                let crossed = |k: f32| if *rate >= 0.0 { k > lo && k <= hi || (prev == 0.0 && k == 0.0) } else { k >= lo && k < hi };
                if let Some(m) = matinee.and_then(|m| g.matinees.get(m as usize)) {
                    // event tracks fire the outputs named after their keys; sound tracks
                    // play at the group's actor
                    for (gi, gr) in m.groups.iter().enumerate() {
                        let actor = binds.iter().find(|b| b.0 == gi as u32).and_then(|b| b.1.first().copied());
                        for tr in &gr.tracks {
                            match tr {
                                dhcook::format::KTrack::Event(keys) => {
                                    for (k, name) in keys {
                                        if crossed(*k) {
                                            named_outputs.push((*op, name.clone()));
                                        }
                                    }
                                }
                                dhcook::format::KTrack::Sound(keys) => {
                                    for (k, name) in keys {
                                        if crossed(*k) {
                                            sounds.push((name.clone(), actor));
                                        }
                                    }
                                }
                                dhcook::format::KTrack::Dialog(keys) => {
                                    for (k, event, text) in keys {
                                        if crossed(*k) {
                                            if !event.is_empty() {
                                                sounds.push((event.clone(), actor));
                                            }
                                            if !text.is_empty() {
                                                let dur = crate::audio::event_length(event).map(|l| l + 0.25).unwrap_or(1.2 + text.chars().count() as f32 / 15.0);
                                                subtitles.push((text.clone(), dur));
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                let finished = if *rate >= 0.0 { *t >= *len } else { *t <= 0.0 };
                if finished && *looping && *rate > 0.0 {
                    *t = 0.0;
                    return true;
                }
                if finished {
                    skipped |= matinee.is_none();
                    if matinee.is_none() {
                        // no cooked tracks: the event outputs (after the standard ones) at the end
                        for i in 6..g.ops[*op as usize].outputs.len() {
                            done.push((*op, i));
                        }
                    }
                    ended.push((*op, *t));
                    done.push((*op, if *rate >= 0.0 { 0 } else { 1 }));
                    interps_done = true;
                    false
                } else {
                    true
                }
            }
            Latent::Spawn { op, left, .. } => {
                *left -= dt;
                if *left <= 0.0 {
                    done.push((*op, 2));
                    done.push((*op, 1));
                    false
                } else {
                    true
                }
            }
            Latent::After { op, out, left } => {
                *left -= dt;
                if *left <= 0.0 {
                    done.push((*op, *out));
                    false
                } else {
                    true
                }
            }
            Latent::Dialog { actor, input, left } => {
                *left -= dt;
                if *left <= 0.0 {
                    dialogs.push((*actor, input.clone()));
                    false
                } else {
                    true
                }
            }
            Latent::DoorSwung { actor, opened, left } => {
                *left -= dt;
                if *left <= 0.0 {
                    swung.push((*actor, *opened));
                    false
                } else {
                    true
                }
            }
        });
        for (a, opened) in swung {
            self.actor_event(a, &["DisSeqEvent_Door"], OutSel::Desc(if opened { "Opened" } else { "Closed" }), None);
        }
        for (op, name) in named_outputs {
            self.fire_desc(op, &name);
        }
        for (name, actor) in sounds {
            self.effects.push(Effect::Sound(name, actor.map(Val::Actor)));
        }
        for (op, t) in ended {
            self.interp_pos.insert(op, t);
        }
        for (text, dur) in subtitles {
            self.effects.push(Effect::Subtitle(text, dur));
        }
        for (op, out) in done {
            self.fire(op, out);
        }
        if interps_done && !full_matinee() && skipped {
            self.effects.push(Effect::FadeIn(0.8));
        }
        for (actor, input) in dialogs {
            self.dialog_finished(actor, &input);
        }
    }
}

/// How long a door takes to swing (`interact::animate_doors`).
const DOOR_SWING: f32 = 0.65;

/// Which output of an event to fire.
#[derive(Clone, Copy)]
pub enum OutSel {
    Index(usize),
    Desc(&'static str),
}

fn initial_value(v: &dhcook::format::KVar) -> Val {
    let p = &v.props;
    let get = |k: &str| p.get(k).map(Val::from_k);
    match v.class.as_str() {
        "SeqVar_Bool" => Val::Bool(get("bValue").and_then(|x| x.as_bool()).unwrap_or(false)),
        "SeqVar_Int" => Val::Int(get("IntValue").and_then(|x| x.as_f32()).unwrap_or(0.0) as i32),
        "SeqVar_Float" => Val::Float(get("FloatValue").and_then(|x| x.as_f32()).unwrap_or(0.0)),
        "SeqVar_String" => get("StrValue").unwrap_or(Val::Str(String::new())),
        "SeqVar_ObjectList" => get("ObjList").unwrap_or(Val::List(Vec::new())),
        "DisSeqVar_Task" => get("m_pDummyTask").or_else(|| get("m_pObjective")).unwrap_or(Val::None),
        "InterpData" => get("InterpLength").unwrap_or(Val::None),
        _ => get("ObjValue").unwrap_or(Val::None),
    }
}

fn full_matinee() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("DH_FULL_MATINEE").is_ok())
}

/// Strip input glyph markup ("Press `GBA_Use` on ...").
pub fn clean_text(s: &str) -> String {
    let mut out = String::new();
    let mut parts = s.split('`');
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for (i, p) in parts.enumerate() {
        if i % 2 == 0 {
            // the original PC bindings (DefaultInput.ini)
            use crate::bindings::{hint, Act};
            let key = match p.to_ascii_lowercase().as_str() {
                "gba_use" => hint(Act::Use),
                "gba_crouch" | "gba_sneak" => hint(Act::Crouch),
                "gba_jump" => hint(Act::Jump),
                "gba_sprint" => hint(Act::Sprint),
                "gba_block" => hint(Act::Block),
                "gba_fire" | "gba_primaryattack" | "gba_primary" => "[LMB]".into(),
                "gba_altfire" | "gba_secondaryattack" | "gba_secondary" => "[RMB]".into(),
                "gba_journal" => hint(Act::Journal),
                "gba_wheel" => "[MMB]".into(),
                "gba_leanleft" => hint(Act::LeanLeft),
                "gba_leanright" => hint(Act::LeanRight),
                "gba_lean_gamepad" => format!("{} {}", hint(Act::LeanLeft), hint(Act::LeanRight)),
                "gba_healthelixir" => hint(Act::HealthElixir),
                "gba_manaelixir" => hint(Act::ManaElixir),
                "gba_zoom" => hint(Act::Zoom),
                "gba_moveforward" => hint(Act::Forward),
                "gba_movebackward" => hint(Act::Back),
                "gba_strafeleft" => hint(Act::Left),
                "gba_straferight" => hint(Act::Right),
                // (the console's sticks: the PC's movement keys and mouse)
                "key_leftstick" => format!("{}{}{}{}", hint(Act::Forward), hint(Act::Left), hint(Act::Back), hint(Act::Right)),
                "key_rightstick" => "[Mouse]".into(),
                _ => "[key]".into(),
            };
            out.push_str(&key);
        } else {
            out.push_str(p);
        }
    }
    out
}

// ---------------------------------------------------------------- systems

#[derive(Component)]
pub(crate) struct ObjectivesText;
#[derive(Component)]
struct FadeOverlay;
#[derive(Component)]
pub(crate) struct SubtitleText;

/// `ScriptUi::hud_hidden`'s entry for the whole HUD (`SeqAct_ToggleHUD`).
pub const HUD_ALL: &str = "ALL";

/// Current screen fade (from, to, time, elapsed) and location banner.
#[derive(Resource, Default)]
pub struct ScriptUi {
    /// the HUD's parts the scripts hid (`DisSeqAct_ToggleHUDElement`: `DHE_Health`,
    /// `DHE_Mana`, `DHE_Equipment`, `DHE_Crosshair`, `DHE_StealthGem`...)
    pub hud_hidden: HashSet<String>,
    fade: Option<(f32, f32, f32, f32)>,
    alpha: f32,
    /// a location to announce (`location` shows it)
    pub location: Option<String>,
    subtitle: Option<(String, f32)>,
    /// time spent fully black
    black: f32,
    /// seconds since the objectives last changed
    objectives_age: f32,
}

impl ScriptUi {
    /// A save's HUD: the parts the scripts had hidden, and no fade or banner left over from the
    /// level's start-up.
    pub fn restore(&mut self, hidden: HashSet<String>) {
        *self = ScriptUi { hud_hidden: hidden, ..Default::default() };
    }
}

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    mut campaign: ResMut<crate::gameplay::Campaign>,
    data: Res<crate::gamedata::Data>,
    script: Res<crate::campaign::CampaignScript>,
    stats: Res<PlayerStats>,
) {
    let Some(level) = level else { return };
    // the campaign's scripts (the persistent level's) run alongside the map's
    let mut graph = level.scene.kismet.clone();
    let merged = script.0.as_ref().filter(|_| !level.scene.name.eq_ignore_ascii_case(crate::menu::MENU_MAP)).map(|c| crate::campaign::merge(&mut graph, c));
    let g = Arc::new(graph);
    let mut vm = Vm::new(g.clone(), &level.scene.volumes);
    vm.campaign = merged;
    // the doors left open
    for (i, a) in g.actors.iter().enumerate() {
        if let Some(cw) = a.instances.first().and_then(|&inst| level.scene.door(&a.name, inst)).and_then(|d| d.open_start).filter(|_| a.class == "DisDoor") {
            vm.door_open.insert(i as u32, cw);
        }
    }
    // streamed sublevels: only those the map change named start loaded
    let loaded = crate::campaign::initial_levels(&level.scene, &campaign, &script);
    for l in &level.scene.levels {
        vm.level_ops.push((l.name.clone(), l.streamed, l.ops.0, l.ops.1));
        if l.streamed && !loaded.contains(&l.name.to_ascii_lowercase()) {
            vm.inert.push((l.ops.0, l.ops.1, l.name.clone()));
        }
    }
    if !loaded.is_empty() {
        info!("streamed sublevels loaded: {loaded:?}");
    }
    // characters answer from their voice's dialogue tree
    for (ai, a) in g.actors.iter().enumerate() {
        let tree = a
            .spawner
            .and_then(|s| level.scene.spawners.get(s as usize))
            .and_then(|sp| sp.npc_type)
            .and_then(|t| level.scene.npc_types.get(t as usize))
            .and_then(|t| t.voices.first())
            .and_then(|v| level.scene.barks.get(*v as usize))
            .and_then(|b| vm.tree_by_path.get(&b.tree).copied());
        if let Some(t) = tree {
            vm.speaker_tree.entry(ai as u32).or_insert(t);
        }
    }
    vm.darkness = stats.chaos_level;
    if let Some((vals, st)) = campaign.script_state.take() {
        vm.restore_campaign(&vals, &st);
    }
    if campaign.travel.is_some() {
        vm.travel = campaign.travel.clone();
    }
    // the story so far (global story flags carry from map to map)
    vm.flags = campaign.flags.clone();
    if vm.flags.is_empty() {
        // a map started directly: the game counts as started properly (`Global_RealGameStarted`:
        // the maps' debug loadouts stay off; the start loadout is the campaign's)
        vm.flags.insert(REAL_GAME_STARTED.into(), true);
        // and the missions before it are done (the flags the Heart's lines are labelled with)
        let idx = crate::save::mission_index(&level.scene.name);
        for n in data.0.heart.nodes.iter().filter(|n| n.class == "CheckStoryFlag" && !n.flag.is_empty()) {
            let m = match n.label.trim().to_ascii_uppercase().as_str() {
                "HUB" => 2,
                "HIGH OVERSEER" | "OVERSEER RETURN" => 3,
                "BROTHEL" => 4,
                "BRIDGE" => 5,
                "BOYLE" => 6,
                "TOWER RETURN" => 7,
                "FLOODED" => 8,
                "HUB ATTACK" => 9,
                "LIGHTHOUSE" => 10,
                _ => continue,
            };
            if m < idx {
                vm.flags.insert(n.flag.clone(), true);
            }
        }
    }
    info!(
        "level scripts: {} ops, {} variables, {} actors, {} touch volumes, {} objectives",
        g.ops.len(),
        g.vars.len(),
        g.actors.len(),
        vm.touch.len(),
        g.objectives.len()
    );
    // the level has loaded (the campaign's own level events ran when the game started)
    let level_ops = merged.map(|m| m.0 as usize).unwrap_or(g.ops.len());
    for (i, op) in g.ops.iter().enumerate().take(level_ops) {
        match op.class.as_str() {
            "SeqEvent_LevelLoaded" => {
                vm.event(i as u32, 0, None);
                vm.st[i].fired = 0;
                vm.event(i as u32, 1, None);
            }
            "SeqEvent_LevelBeginning" | "SeqEvent_LevelStartup" => {
                vm.event(i as u32, 0, None);
            }
            _ => {}
        }
    }
    // a map change the campaign made: its scripts carry on here
    if let (Some((oo, _)), Some(r)) = (merged, campaign.resume.take()) {
        vm.fire(oo + r, 0);
    }
    commands.insert_resource(vm);
    commands.insert_resource(ScriptUi::default());
    // UI: objectives (top right), location banner (top centre), fade overlay
    commands.spawn((
        FadeOverlay,
        Node { width: percent(100), height: percent(100), position_type: PositionType::Absolute, ..default() },
        BackgroundColor(Color::NONE),
        GlobalZIndex(5),
        DespawnOnExit(GameState::InGame),
    ));
    commands.spawn((
        ObjectivesText,
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(22.0), ..default() },
        TextColor(Color::srgba(0.93, 0.86, 0.66, 0.9)),
        TextLayout::justify(Justify::Right),
        Node { position_type: PositionType::Absolute, right: px(28), top: px(24), width: px(520), ..default() },
        DespawnOnExit(GameState::InGame),
    ));
    commands.spawn((
        SubtitleText,
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(28.0), ..default() },
        TextColor(Color::srgba(1.0, 1.0, 1.0, 0.95)),
        TextLayout::justify(Justify::Center),
        TextShadow::default(),
        Node { position_type: PositionType::Absolute, width: percent(70), left: percent(15), bottom: percent(14), ..default() },
        DespawnOnExit(GameState::InGame),
    ));
    // usable objects that are neither doors nor pickups get a prompt
    let mut n = 0;
    for (i, a) in g.actors.iter().enumerate() {
        let used = vm_events(&g, i as u32).any(|c| c == "DisSeqEvent_Used" || c == "SeqEvent_Used");
        // (characters carry their own prompt: `talk_prompts`)
        // (animated usable objects have their own: `usables`)
        if !used || a.pickup.is_some() || a.class == "DisDoor" || a.spawner.is_some() || crate::usables::animated(&level, a.usable) {
            continue;
        }
        let label = if a.class.contains("Usable") || a.class.contains("Lever") || a.class.contains("Button") { "Use" } else { "Interact" };
        commands.spawn((
            Usable { actor: i as u32, label: label.into() },
            Transform::from_translation(Vec3::from(a.position) + Vec3::Y * 0.2),
            DespawnOnExit(GameState::InGame),
        ));
        n += 1;
    }
    if n > 0 {
        info!("{n} scripted usable objects");
    }
}

/// What leads to another map (a level's exits, mostly `DishonoredUsableObject` doors): its
/// "Used" event goes on (through sub-sequences and remote events) to setting or going to the
/// player's travel destination (the HUD's `CHS_OVER_DOOR_LEVEL_TRANSITION`,
/// `crosshair_travelDoor`). By script actor.
pub fn travel_actors(g: &dhcook::format::Kismet) -> std::collections::HashSet<u32> {
    use std::collections::{HashSet, VecDeque};
    let mut out = HashSet::new();
    for op in &g.ops {
        if !matches!(op.class.as_str(), "DisSeqEvent_Used" | "SeqEvent_Used") {
            continue;
        }
        let Some(KVal::Actor(a)) = op.props.get("Originator") else { continue };
        if g.actors.get(*a as usize).is_none() {
            continue;
        }
        let mut seen: HashSet<u32> = HashSet::new();
        let mut queue: VecDeque<u32> = op.outputs.iter().flat_map(|o| o.links.iter().map(|l| l.0)).collect();
        let mut found = false;
        while let Some(i) = queue.pop_front() {
            if !seen.insert(i) || seen.len() > 400 {
                continue;
            }
            let Some(o) = g.ops.get(i as usize) else { continue };
            match o.class.as_str() {
                "DisSeqAct_GotoPlayerTravelDestination" | "DisSeqAct_SetPlayerTravelDestination" => {
                    found = true;
                    break;
                }
                // into a sub-sequence: its activated events
                "Sequence" | "PrefabSequence" | "PrefabSequenceContainer" => {
                    queue.extend(g.ops.iter().enumerate().filter(|(_, e)| e.parent == Some(i) && e.class == "SeqEvent_SequenceActivated").map(|(k, _)| k as u32));
                }
                // a remote event: its listeners
                "SeqAct_ActivateRemoteEvent" => {
                    if let Some(name) = o.props.get("EventName") {
                        queue.extend(g.ops.iter().enumerate().filter(|(_, e)| e.class == "SeqEvent_RemoteEvent" && e.props.get("EventName") == Some(name)).map(|(k, _)| k as u32));
                    }
                }
                _ => {}
            }
            queue.extend(o.outputs.iter().flat_map(|x| x.links.iter().map(|l| l.0)));
        }
        if found {
            out.insert(*a);
        }
    }
    out
}

fn vm_events(g: &Graph, actor: u32) -> impl Iterator<Item = &str> + '_ {
    g.ops.iter().filter(move |o| o.props.get("Originator") == Some(&KVal::Actor(actor))).map(|o| o.class.as_str())
}

/// Characters speaking near the player look at him.
#[derive(Component)]
struct TalkLook;

fn speakers_look(
    mut commands: Commands,
    vm: Res<Vm>,
    player: Query<&Transform, With<Player>>,
    npcs: Query<(Entity, &Npc, &FromSpawner, &Transform, Option<&TalkLook>, Option<&crate::npc::SceneLook>), Without<Player>>,
) {
    let Ok(pt) = player.single() else { return };
    let speaking: HashSet<u32> = vm.speakers().filter_map(|a| vm.g.actors.get(a as usize).and_then(|ka| ka.spawner)).collect();
    for (e, npc, from, t, talk, look) in &npcs {
        let near = speaking.contains(&from.0) && !npc.is_down() && t.translation.distance(pt.translation) < 6.0;
        match (near, talk.is_some()) {
            (true, _) if look.is_none() || talk.is_some() => {
                commands.entity(e).try_insert((TalkLook, crate::npc::SceneLook { target: pt.translation, weight: 1.0 }));
            }
            (false, true) => {
                commands.entity(e).try_remove::<(TalkLook, crate::npc::SceneLook)>();
            }
            _ => {}
        }
    }
}

/// Characters the scripts let the player use (talk to, give orders) get a prompt naming them.
fn talk_prompts(
    mut commands: Commands,
    time: Res<Time>,
    mut next: Local<f32>,
    vm: Res<Vm>,
    data: Res<crate::gamedata::Data>,
    npcs: Query<(Entity, &Npc, &FromSpawner, Option<&Usable>)>,
) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 0.4;
    for (e, npc, from, usable) in &npcs {
        let actor = vm.g.actors.iter().position(|a| a.spawner == Some(from.0)).map(|a| a as u32);
        let can = actor.filter(|&a| (vm.usable(a) || vm.talkable(a)) && !npc.is_down() && !npc.hostile() && npc.alert != crate::npc::Alert::Combat);
        match (can, usable) {
            (Some(a), None) => {
                let (package, object) = npc.pawn.split_once('.').unwrap_or((&npc.pawn, &npc.pawn));
                let label = match data.0.pawn_names.get(object).or(data.0.pawn_names.get(package)) {
                    Some(n) => format!("Talk to {n}"),
                    None => "Talk".to_string(),
                };
                commands.entity(e).try_insert(Usable { actor: a, label });
            }
            (None, Some(_)) => {
                commands.entity(e).try_remove::<Usable>();
            }
            _ => {}
        }
    }
}

/// Turn world happenings into script events.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
/// Things destroyed (`SeqEvent_Destroyed`): river krusts killed (the Hound Pits' story
/// flags for clearing the shore).
fn destroyed_events(mut vm: ResMut<Vm>, krusts: Option<Res<crate::krust::KrustLog>>) {
    let Some(krusts) = krusts else { return };
    if !krusts.is_changed() {
        return;
    }
    let dead: Vec<u32> = vm.g.actors.iter().enumerate().filter(|(_, a)| a.krust.is_some_and(|k| krusts.dead.contains_key(&k))).map(|(i, _)| i as u32).collect();
    for a in dead {
        if vm.destroyed.insert(a) {
            vm.actor_event(a, &["SeqEvent_Destroyed"], OutSel::Index(0), Some(Val::Player));
        }
    }
}

fn world_events(
    mut vm: ResMut<Vm>,
    mut used: MessageReader<Interaction>,
    mut spawned: MessageReader<NpcSpawned>,
    (mut power_used, mut power_equipped, possession, mut hits): (MessageReader<crate::powers::PowerUsed>, MessageReader<crate::powers::PowerEquipped>, Res<crate::possession::Possession>, MessageReader<crate::gameplay::NpcHit>),
    player: Query<(&Transform, &Player)>,
    npcs: Query<(Entity, &Npc, &FromSpawner, &Transform), Without<Player>>,
    (cam, rapier): (Query<&GlobalTransform, With<crate::player::PlayerCamera>>, bevy_rapier3d::prelude::ReadRapierContext),
    (journal, stats, bites, hosts): (Res<crate::journal::Journal>, Res<crate::gameplay::PlayerStats>, Res<crate::swarm::RatBites>, Query<&crate::possession::Host, Without<Npc>>),
) {
    let vm = &mut *vm;
    // Corvo going into a rat and out (`DisSeqEvent_RatPossess`: the Lighthouse's sewers)
    let rat = possession.host.and_then(|h| hosts.get(h).ok()).is_some_and(|h| !h.fish && !h.rooted);
    if rat != vm.in_rat {
        vm.in_rat = rat;
        for ev in class_ops(&vm.g, "DisSeqEvent_RatPossess") {
            vm.event(ev, if rat { 0 } else { 1 }, Some(Val::Player));
        }
    }
    // rats biting Corvo (`DisSeqEvent_AttackedByRats`)
    if bites.0 != vm.rat_bites {
        vm.rat_bites = bites.0;
        for ev in class_ops(&vm.g, "DisSeqEvent_AttackedByRats") {
            vm.event(ev, 0, Some(Val::Player));
        }
    }
    // the journal closed after being looked at (`DisSeqEvent_JournalViewed`: the dream goes
    // on once the powers page has been seen)
    if journal.viewed != vm.journal_viewed {
        vm.journal_viewed = journal.viewed;
        for ev in class_ops(&vm.g, "DisSeqEvent_JournalViewed") {
            vm.event(ev, 0, Some(Val::Player));
        }
    }
    // an objective coming up (`DisSeqEvent_TaskActivated` `m_pObjective`: the Bridge's markers)
    let active_count = vm.objectives.len() + vm.tasks.values().filter(|s| s.0 == TaskState::Active).count();
    let task_evs = if active_count != vm.active_count {
        vm.active_count = active_count;
        class_ops(&vm.g, "DisSeqEvent_TaskActivated")
    } else {
        Vec::new()
    };
    for ev in task_evs {
        let Some(t) = vm.ps(ev, "m_pObjective") else { continue };
        let active = vm.objectives.contains(&t) || vm.tasks.get(&t).is_some_and(|s| s.0 == TaskState::Active);
        if active && vm.task_events.insert(ev) {
            vm.event(ev, 0, Some(Val::Player));
        }
    }
    // Corvo's mana crossing a threshold (`DisSeqEvent_PlayerManaThreshold`: above, below)
    if let Some(was) = vm.mana_was.replace(stats.mana).filter(|w| *w != stats.mana) {
        for ev in class_ops(&vm.g, "DisSeqEvent_PlayerManaThreshold") {
            let t = vm.pi(ev, "m_ManaThreshold").unwrap_or(0) as f32;
            if was < t && stats.mana >= t {
                vm.event(ev, 0, Some(Val::Player));
            } else if was >= t && stats.mana < t {
                vm.event(ev, 1, Some(Val::Player));
            }
        }
    }
    // the character Corvo is in: its spawner's possession events start and finish
    vm.possessed = possession.host.and_then(|h| npcs.get(h).ok()).map(|(_, _, f, _)| f.0);
    if vm.possessed != vm.possessed_was {
        if let Some(a) = vm.possessed_was.and_then(|s| vm.spawner_actor.get(&s).copied()) {
            vm.actor_event(a, &["DisSeqEvent_Possessed"], OutSel::Desc("Finished"), Some(Val::Player));
        }
        if let Some(a) = vm.possessed.and_then(|s| vm.spawner_actor.get(&s).copied()) {
            vm.actor_event(a, &["DisSeqEvent_Possessed"], OutSel::Desc("Started"), Some(Val::Player));
        }
        vm.possessed_was = vm.possessed;
    }
    // where everyone is (`SeqAct_GetDistance`), and who they are
    vm.npc_pos.clear();
    for (_, n, f, t) in &npcs {
        if !n.is_down() {
            vm.npc_pos.insert(f.0, t.translation);
        }
        if !vm.npc_pawn.contains_key(&f.0) {
            vm.npc_pawn.insert(f.0, n.pawn.clone());
        }
    }
    // Corvo inside a creature is the possession proxy (its own triggers)
    vm.player_proxy = possession.body.is_some();
    // patrollers reaching their nav points
    if vm.patrol_points.is_none() {
        let pts: Vec<(u32, Vec3)> = vm
            .events_of
            .iter()
            .filter(|(_, evs)| evs.iter().any(|&e| vm.g.ops[e as usize].class == "DisSeqEvent_PatrolPointReached"))
            .filter_map(|(a, _)| vm.g.actors.get(*a as usize).map(|ka| (*a, Vec3::from(ka.position))))
            .collect();
        vm.patrol_points = Some(pts);
    }
    if let Some(pts) = vm.patrol_points.clone().filter(|p| !p.is_empty()) {
        let mut now = HashSet::new();
        for (a, at) in &pts {
            for (s, p) in &vm.npc_pos {
                if (p.xz() - at.xz()).length() < 1.2 && (p.y - at.y).abs() < 2.5 {
                    now.insert((*a, *s));
                }
            }
        }
        let arrived: Vec<(u32, u32)> = now.difference(&vm.at_points).copied().collect();
        vm.at_points = now;
        for (a, s) in arrived {
            let who = vm.spawner_actor.get(&s).map(|x| Val::Actor(*x));
            vm.actor_event(a, &["DisSeqEvent_PatrolPointReached"], OutSel::Index(0), who);
        }
    }
    // what Corvo looks at (`SeqEvent_LOS`: within its distance and so many pixels of the
    // screen's centre, on a 1280-wide view)
    if let Ok(c) = cam.single() {
        let (eye, fwd) = (c.translation(), c.forward().as_vec3());
        let ctx = rapier.single().ok();
        let walls = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
        let g = vm.g.clone();
        for (ev, op) in g.ops.iter().enumerate().filter(|(_, o)| o.class == "SeqEvent_LOS") {
            let ev = ev as u32;
            let Some(KVal::Actor(a)) = op.props.get("Originator") else { continue };
            let Some(ka) = g.actors.get(*a as usize) else { continue };
            let at = ka.spawner.and_then(|s| vm.npc_pos.get(&s).copied()).unwrap_or(Vec3::from(ka.position));
            let to = at - eye;
            let d = to.length();
            let range = vm.pf(ev, "TriggerDistance").unwrap_or(0.0) * 0.01;
            let px = vm.pf(ev, "ScreenCenterDistance").unwrap_or(50.0);
            let cosang = to.normalize_or_zero().dot(fwd);
            let tan = (1.0 - cosang * cosang).max(0.0).sqrt() / cosang.max(1e-3);
            let mut seen = cosang > 0.0 && tan * 640.0 <= px && (range <= 0.0 || d <= range);
            if seen && vm.pb(ev, "bCheckForObstructions").unwrap_or(true) {
                if let Some(ctx) = &ctx {
                    seen = d < 0.5 || ctx.cast_ray(eye, to / d, d - 0.3, true, walls).is_none();
                }
            }
            let was = vm.looking.contains(&ev);
            if seen && !was {
                vm.looking.insert(ev);
                vm.event(ev, 0, Some(Val::Player));
            } else if !seen && was {
                vm.looking.remove(&ev);
                vm.event(ev, 1, Some(Val::Player));
            }
        }
    }
    // where the player and the speaking characters are (for the dialogue hooks)
    vm.player_pos = player.single().ok().map(|(t, _)| t.translation);
    let speaking: Vec<u32> = vm.speaker_tree.keys().copied().collect();
    for a in speaking {
        let Some(s) = vm.g.actors.get(a as usize).and_then(|ka| ka.spawner) else { continue };
        if let Some((_, _, _, t)) = npcs.iter().find(|(_, n, f, _)| f.0 == s && !n.is_down()) {
            vm.speaker_pos.insert(a, t.translation);
        } else {
            vm.speaker_pos.remove(&a);
        }
    }
    // touch volumes (those attached to a character move with it)
    let mut moved: HashMap<u32, Vec3> = HashMap::new();
    for (a, (spawner, origin)) in vm.touch_follow.iter_mut() {
        if let Some((_, _, _, t)) = npcs.iter().find(|(_, n, f, _)| f.0 == *spawner && !n.is_down()) {
            let o = *origin.get_or_insert(t.translation);
            moved.insert(*a, t.translation - o);
        }
    }
    if let Ok((pt, p)) = player.single() {
        let probe = pt.translation;
        // (Corvo's whole capsule touches, as UE3's collision cylinder does: a flat trigger on
        // the floor is walked through below his middle)
        use bevy_rapier3d::parry;
        let capsule = parry::shape::Capsule::new_y(if p.crouched { crate::player::CROUCH_HALF } else { crate::player::STAND_HALF }, crate::player::RADIUS);
        let mut inside = HashSet::new();
        for (actor, hulls) in &vm.touch {
            let at = probe - moved.get(actor).copied().unwrap_or(Vec3::ZERO);
            let pose = parry::math::Pose::translation(at.x, at.y, at.z);
            if hulls.iter().any(|h| parry::query::intersection_test(&parry::math::Pose::IDENTITY, h.raw.as_ref(), &pose, &capsule).unwrap_or_else(|_| h.contains_point(Vec3::ZERO, Quat::IDENTITY, at))) {
                inside.insert(*actor);
            }
        }
        let entered: Vec<u32> = inside.difference(&vm.touching).copied().collect();
        let left: Vec<u32> = vm.touching.difference(&inside).copied().collect();
        vm.touching = inside;
        let who = Toucher::Player { proxy: vm.player_proxy };
        for a in entered {
            vm.touch_event(a, 0, who, Val::Player);
        }
        for a in left {
            vm.touch_event(a, 1, who, Val::Player);
        }
    // characters walking into and out of the volumes whose events they can set off
    if vm.npc_touch_actors.is_none() {
        let g = vm.g.clone();
        let list: Vec<u32> = vm
            .touch
            .iter()
            .map(|(a, _)| *a)
            .filter(|a| {
                vm.events_of.get(a).is_some_and(|evs| {
                    evs.iter().any(|&ev| matches!(g.ops[ev as usize].class.as_str(), "SeqEvent_Touch" | "DisSeqEvent_Touch") && matches!(g.ops[ev as usize].props.get("bPlayerOnly"), Some(KVal::Bool(false))))
                })
            })
            .collect();
        vm.npc_touch_actors = Some(list);
    }
    let watched = vm.npc_touch_actors.clone().unwrap_or_default();
    if !watched.is_empty() {
        let mut now: HashSet<(u32, u32)> = HashSet::new();
        for (_, n, f, t) in &npcs {
            if n.is_down() {
                continue;
            }
            for &a in &watched {
                let Some(hulls) = vm.touch.iter().find(|(x, _)| *x == a).map(|(_, h)| h) else { continue };
                let at = t.translation - moved.get(&a).copied().unwrap_or(Vec3::ZERO);
                if hulls.iter().any(|h| h.contains_point(Vec3::ZERO, Quat::IDENTITY, at)) {
                    now.insert((a, f.0));
                }
            }
        }
        let entered: Vec<(u32, u32)> = now.difference(&vm.npc_touching).copied().collect();
        let left: Vec<(u32, u32)> = vm.npc_touching.difference(&now).copied().collect();
        vm.npc_touching = now;
        for (a, s) in entered {
            if vm.trace {
                info!("kismet: character of spawner {s} ({}) touches {}", vm.npc_pawn.get(&s).map(|x| x.as_str()).unwrap_or("?"), vm.g.actors.get(a as usize).map(|x| x.name.as_str()).unwrap_or("?"));
            }
            if let Some(&sa) = vm.spawner_actor.get(&s) {
                vm.touch_event(a, 0, Toucher::Npc, Val::Actor(sa));
            }
        }
        for (a, s) in left {
            if let Some(&sa) = vm.spawner_actor.get(&s) {
                vm.touch_event(a, 1, Toucher::Npc, Val::Actor(sa));
            }
        }
    }
        if p.crouched != vm.player_crouch {
            vm.player_crouch = p.crouched;
            for ev in class_ops(&vm.g, "DisSeqEvent_PlayerCrouch") {
                let i = if p.crouched { 2 } else { 0 };
                vm.event(ev, i, None);
            }
        }
    }
    // powers: the Outsider's tutorial waits for Blink to be equipped and used
    for _ in power_used.read() {
        for ev in class_ops(&vm.g, "DisSeqEvent_PowerUsed") {
            vm.event(ev, 0, Some(Val::Player));
        }
    }
    for _ in power_equipped.read() {
        for ev in class_ops(&vm.g, "DisSeqEvent_PowerEquipped") {
            vm.event(ev, 0, Some(Val::Player));
        }
    }
    // Windblast striking someone (`DisSeqEvent_Windblasted`: pushed, or thrown hard)
    for h in hits.read() {
        if h.kind != crate::gameplay::HitKind::Windblast {
            continue;
        }
        let Some(&a) = npcs.get(h.npc).ok().and_then(|(_, _, f, _)| vm.spawner_actor.get(&f.0)) else { continue };
        vm.actor_event(a, &["DisSeqEvent_Windblasted"], OutSel::Desc(if h.damage > 30.0 { "Thrown" } else { "Pushed" }), Some(Val::Player));
    }
    // spawns
    for s in spawned.read() {
        if let Some(&a) = vm.spawner_actor.get(&s.spawner) {
            vm.actor_event(a, &["DisSeqEvent_Spawned"], OutSel::Index(0), Some(Val::Actor(a)));
        }
    }
    // NPC deaths, knock-outs, alerts, damage
    let mut combat = false;
    let mut seen = HashMap::new();
    for (e, npc, from, _) in &npcs {
        if npc.alert == Alert::Combat && !npc.is_down() {
            combat = true;
        }
        let now = (npc.mode, npc.alert, npc.health, npc.sees_player);
        seen.insert(e, now);
        let Some(&(mode0, alert0, health0, sees0)) = vm.npc_seen.get(&e) else { continue };
        let Some(&actor) = vm.spawner_actor.get(&from.0) else { continue };
        let who = Some(Val::Player);
        // Corvo seen (`DisSeqEvent_PawnSighted`: the Tower's return, the Overseers' outpost)
        if npc.sees_player && !sees0 && !npc.is_down() {
            vm.actor_event(actor, &["DisSeqEvent_PawnSighted"], OutSel::Index(0), who.clone());
        }
        // and heard (`DisSeqEvent_PlayerHeard`: the Bridge's guards, the kennel's dogs)
        let heard = vm.npc_heard.insert(e, npc.heard_player).unwrap_or(npc.heard_player);
        if npc.heard_player > heard && !npc.is_down() {
            vm.actor_event(actor, &["DisSeqEvent_PlayerHeard"], OutSel::Index(0), who.clone());
            for ev in class_ops(&vm.g, "DisSeqEvent_PlayerHeard").into_iter().filter(|&ev| !vm.g.ops[ev as usize].props.contains_key("Originator")).collect::<Vec<_>>() {
                vm.event(ev, 0, Some(Val::Actor(actor)));
            }
        }
        if npc.mode != mode0 {
            // choked from behind (`DisSeqEvt_Choke`: started, won when they go limp, lost when
            // they break free), and looking into something (`DisSeqEvent_Investigate`)
            let choke = match (mode0, npc.mode) {
                (_, Mode::Choked) => Some("Started"),
                (Mode::Choked, Mode::Unconscious | Mode::Dead) => Some("Win"),
                (Mode::Choked, _) => Some("Lose"),
                _ => None,
            };
            if let Some(out) = choke {
                vm.actor_event(actor, &["DisSeqEvt_Choke"], OutSel::Desc(out), Some(Val::Actor(actor)));
                for ev in class_ops(&vm.g, "DisSeqEvt_Choke").into_iter().filter(|&ev| !vm.g.ops[ev as usize].props.contains_key("Originator")).collect::<Vec<_>>() {
                    if let Some(i) = vm.output_index(ev, out) {
                        vm.event(ev, i, Some(Val::Actor(actor)));
                    }
                }
            }
            if npc.mode == Mode::Investigate {
                vm.actor_event(actor, &["DisSeqEvent_Investigate"], OutSel::Index(0), who.clone());
            }
            match npc.mode {
                Mode::Dead => {
                    vm.actor_event(actor, &["SeqEvent_Death"], OutSel::Index(0), who.clone());
                    vm.actor_event(actor, &["DisSeqEvent_NPCIncapacitated"], OutSel::Desc("Dead"), who.clone());
                    if alert0 == Alert::Unaware {
                        vm.actor_event(actor, &["DisSeqEvent_Assassinated"], OutSel::Index(0), who.clone());
                    }
                }
                Mode::Unconscious => {
                    vm.actor_event(actor, &["DisSeqEvent_NPCIncapacitated"], OutSel::Desc("Asleep"), who.clone());
                    vm.actor_event(actor, &["DisSeqEvent_KnockedOut"], OutSel::Index(0), who.clone());
                }
                _ => {}
            }
        }
        if npc.alert != alert0 {
            if npc.alert == Alert::Combat {
                vm.actor_event(actor, &["DisSeqEvent_Combat"], OutSel::Index(0), who.clone());
            }
            // calming down (`DisSeqEvent_AttentionDecreasedTo`: to its level, or all the way)
            let rank = |a: Alert| match a {
                Alert::Unaware => 0,
                Alert::Suspicious => 1,
                Alert::Combat => 2,
            };
            if rank(npc.alert) < rank(alert0) {
                let to: &[&str] = match npc.alert {
                    Alert::Unaware => &["DAL_Unaware", "DAL_None", "DAL_Calm"],
                    Alert::Suspicious => &["DAL_Investigate", "DAL_Suspicious", "DAL_Curious"],
                    Alert::Combat => &[],
                };
                for ev in vm.events_of.get(&actor).cloned().unwrap_or_default() {
                    let op = &vm.g.ops[ev as usize];
                    if op.class != "DisSeqEvent_AttentionDecreasedTo" {
                        continue;
                    }
                    let fits = match op.props.get("m_eAttentionLevel") {
                        Some(KVal::Str(l)) => to.contains(&l.as_str()),
                        _ => npc.alert == Alert::Unaware,
                    };
                    if fits {
                        vm.event(ev, 0, who.clone());
                    }
                }
            }
            let level = match npc.alert {
                Alert::Combat => Some(["DAL_Busted", "DAL_Combat", "DAL_Alerted"]),
                Alert::Suspicious => Some(["DAL_Investigate", "DAL_Suspicious", "DAL_Curious"]),
                Alert::Unaware => None,
            };
            if let Some(levels) = level {
                for ev in vm.events_of.get(&actor).cloned().unwrap_or_default() {
                    let op = &vm.g.ops[ev as usize];
                    if op.class == "DisSeqEvent_AttentionIncreasedTo"
                        && matches!(op.props.get("m_eAttentionLevel"), Some(KVal::Str(l)) if levels.contains(&l.as_str()))
                    {
                        vm.event(ev, 0, who.clone());
                    }
                }
            }
        }
        // hurt by something other than a blow of known type (a fall, a trap): of no
        // particular type
        if npc.health < health0 - 0.1 && vm.recent_hits.get(&e).is_none_or(|t| vm.time - t > 0.5) {
            vm.take_damage(actor, "DishonoredDamageType", health0 - npc.health, true);
        }
    }
    vm.npc_seen = seen;
    if combat != vm.player_combat {
        vm.player_combat = combat;
        for ev in class_ops(&vm.g, "DisSeqEvent_PlayerCombat") {
            vm.event(ev, if combat { 0 } else { 1 }, None);
        }
    }
    // the player used something
    for u in used.read() {
        match u {
            Interaction::Corpse { spawner, what } => {
                if let Some(&a) = vm.spawner_actor.get(spawner) {
                    vm.actor_event(a, &["DisSeqEvent_Corpse"], OutSel::Index(*what as usize), Some(Val::Player));
                }
            }
            Interaction::Pickup(p) => {
                if let Some(&a) = vm.pickup_actor.get(p) {
                    vm.actor_event(a, &["DisSeqEvent_PickupPickedUp"], OutSel::Index(0), Some(Val::Player));
                    vm.interacted(a);
                }
            }
            Interaction::Door { instance, opened, cw } => {
                for a in vm.instance_actor.get(instance).cloned().unwrap_or_default() {
                    if *opened {
                        vm.door_open.insert(a, *cw);
                    } else {
                        vm.door_open.remove(&a);
                    }
                    let d = match (opened, cw) {
                        (true, true) => "OpenCW",
                        (true, false) => "OpenCCW",
                        (false, true) => "CloseCW",
                        (false, false) => "CloseCCW",
                    };
                    vm.actor_event(a, &["DisSeqEvent_Used"], OutSel::Desc(d), Some(Val::Player));
                    vm.actor_event(a, &["DisSeqEvent_Door"], OutSel::Desc(if *opened { "Opened" } else { "Closed" }), Some(Val::Player));
                    vm.interacted(a);
                }
            }
            Interaction::Keyhole { instance, used } => {
                for a in vm.instance_actor.get(instance).cloned().unwrap_or_default() {
                    let out = if *used { OutSel::Index(0) } else { OutSel::Desc("Unused") };
                    vm.actor_event(a, &["DisSeqEvent_KeyHoleUsed"], out, Some(Val::Player));
                }
            }
            Interaction::Tripwire(t) => {
                let actors: Vec<u32> = vm.g.actors.iter().enumerate().filter(|(_, a)| a.trap == Some(*t)).map(|(i, _)| i as u32).collect();
                for a in actors {
                    vm.actor_event(a, &["DisSeqEvent_Tripwire"], OutSel::Desc("Tripped"), Some(Val::Player));
                }
            }
            Interaction::DoorLocked(instance) => {
                for a in vm.instance_actor.get(instance).cloned().unwrap_or_default() {
                    vm.actor_event(a, &["DisSeqEvent_Used"], OutSel::Desc("Used But Locked"), Some(Val::Player));
                    vm.actor_event(a, &["DisSeqEvent_KeyHoleUsed"], OutSel::Index(0), Some(Val::Player));
                }
            }
            Interaction::Usable(a) => {
                vm.actor_event(*a, &["DisSeqEvent_Used", "SeqEvent_Used", "DisSeqEvent_Interact"], OutSel::Index(0), Some(Val::Player));
                vm.interacted(*a);
                // characters answer from their dialogue
                vm.talk(*a);
            }
            Interaction::UsableLock { actor, locked } => {
                vm.actor_event(*actor, &["DisSeqEvent_Lock"], OutSel::Desc(if *locked { "Locked" } else { "Unlocked" }), Some(Val::Player));
            }
            Interaction::Knocked { instance, speed } => {
                for a in vm.instance_actor.get(instance).cloned().unwrap_or_default() {
                    for ev in vm.events_of.get(&a).cloned().unwrap_or_default() {
                        // (a thing thrown against a wall: `SeqEvent_HitWall`)
                        if vm.g.ops[ev as usize].class == "SeqEvent_HitWall" {
                            vm.event(ev, 0, None);
                            continue;
                        }
                        if vm.g.ops[ev as usize].class != "SeqEvent_RigidBodyCollision" {
                            continue;
                        }
                        // (UE units)
                        let min = vm.pf(ev, "MinCollisionVelocity").unwrap_or(0.0) / 100.0;
                        if *speed >= min {
                            vm.event(ev, 0, None);
                        }
                    }
                }
            }
            Interaction::Movable { instance, broken } => {
                for a in vm.instance_actor.get(instance).cloned().unwrap_or_default() {
                    if *broken {
                        vm.actor_event(a, &["DisSeqEvent_BreakableBroken"], OutSel::Index(0), Some(Val::Player));
                        // a whale oil tank blown up (the scripts count them; the Hound Pits' comment on it)
                        vm.actor_event(a, &["DisSeqEvent_WhaleOilBattery"], OutSel::Desc("Destroyed"), Some(Val::Player));
                    } else {
                        vm.actor_event(a, &["DisSeqEvent_MovablePickedUp"], OutSel::Index(0), Some(Val::Player));
                    }
                }
            }
            Interaction::Distracted { distractor, at, spawner, start } => {
                let a = vm
                    .g
                    .actors
                    .iter()
                    .enumerate()
                    .filter(|(_, ka)| ka.name == *distractor)
                    .min_by(|x, y| Vec3::from(x.1.position).distance(*at).total_cmp(&Vec3::from(y.1.position).distance(*at)))
                    .map(|(i, _)| i as u32);
                if let Some(a) = a {
                    let pawn = vm.spawner_actor.get(spawner).map(|x| Val::Actor(*x));
                    let out = if *start { "Distracted Start" } else { "Distracted End" };
                    for ev in vm.events_of.get(&a).cloned().unwrap_or_default() {
                        if vm.g.ops[ev as usize].class != "DisSeqEvent_Distracted" {
                            continue;
                        }
                        if let Some(p) = &pawn {
                            vm.write(ev, "DistractedPawn", p.clone());
                        }
                        if let Some(i) = vm.output_index(ev, out) {
                            vm.event(ev, i, pawn.clone());
                        }
                    }
                }
            }
            Interaction::Receptacle { actor, at, plugged } => {
                // (sublevels reuse names: the nearest of that name)
                let a = vm
                    .g
                    .actors
                    .iter()
                    .enumerate()
                    .filter(|(_, ka)| ka.name == *actor)
                    .min_by(|x, y| Vec3::from(x.1.position).distance(*at).total_cmp(&Vec3::from(y.1.position).distance(*at)))
                    .map(|(i, _)| i as u32);
                if let Some(a) = a {
                    vm.actor_event(a, &["DisSeqEvent_WhaleOilReceptacle"], OutSel::Desc(if *plugged { "Plugged" } else { "Unplugged" }), Some(Val::Player));
                }
            }
        }
    }
}

/// Events nothing in the game sets off, as in the original: `SeqEvent_Console` (the
/// developers' console) and `SeqEvent_AnimNotify` (the maps' two only reach `SeqAct_Log`).
pub const UNFIRED_EVENTS: [&str; 2] = ["SeqEvent_Console", "SeqEvent_AnimNotify"];

/// `DisTweaks_PlayerStats.EDisPlayerStat`, in its order.
pub const PLAYER_STATS: [&str; 41] = [
    "ePlayerStat_NumKills",
    "ePlayerStat_NumAssassinations",
    "ePlayerStat_NumFlees",
    "ePlayerStat_NumAlarmsTriggered",
    "ePlayerStat_NumCorpsesDiscovered",
    "ePlayerStat_NumExplosions",
    "ePlayerStat_NumRatsSummoned",
    "ePlayerStat_LongestDistanceFallen",
    "ePlayerStat_AssassinationTargetKilled",
    "ePlayerStat_AmountStolen",
    "ePlayerStat_NPCsAlerted",
    "ePlayerStat_PowersAcquired",
    "ePlayerStat_UnawareKill",
    "ePlayerStat_SameFactionKill",
    "ePlayerStat_DropKill",
    "ePlayerStat_MercyKill",
    "ePlayerStat_Suicide",
    "ePlayerStat_TimePossessing",
    "ePlayerStat_DistanceTravelled",
    "ePlayerStat_UpgradesAcquired",
    "ePlayerStat_ItemsCollected",
    "ePlayerStat_MaxPursuersCountEscaped",
    "ePlayerStat_MaxSimultaneousEnemyCountKilled",
    "ePlayerStat_FishPossessed",
    "ePlayerStat_GrenadeThrowbackKill",
    "ePlayerStat_RatTunnelUsed",
    "ePlayerStat_ChaosScore",
    "ePlayerStat_ChaosLevel",
    "ePlayerStat_HostileKill",
    "ePlayerStat_HostilePutToSleep",
    "ePlayerStat_CivilianKill",
    "ePlayerStat_CivilianPutToSleep",
    "ePlayerStat_NPCsEscapedFrom",
    "ePlayerStat_GoldFound",
    "ePlayerStat_RuneFound",
    "ePlayerStat_BoneCharmFound",
    "ePlayerStat_OutsiderShrineFound",
    "ePlayerStat_SokolovPaintingFound",
    "ePlayerStat_ArcMineBasedOnRatKill",
    "ePlayerStat_CrackedCharmFound",
    "ePlayerStat_DamageTakenByPullLiftShield",
];

/// The spawner a target names (a spawner actor, or the pawn it made).
fn actor_spawner(g: &Graph, v: &Val) -> Option<u32> {
    match v {
        Val::Actor(a) => g.actors.get(*a as usize)?.spawner,
        Val::List(l) => l.iter().find_map(|x| actor_spawner(g, x)),
        _ => None,
    }
}

/// What `more_events` remembers between frames.
#[derive(Default)]
struct MoreState {
    asleep: HashSet<u32>,
    sheathed: bool,
    has: HashMap<u32, bool>,
    climbing: Option<u32>,
    held: Option<Entity>,
    walls: HashMap<String, (bool, bool)>,
    actor_by_name: HashMap<String, u32>,
}

/// Whether an event answers for an actor: its own (`Originator`, or attached to it), or one
/// listening to everyone (neither).
fn event_for(vm: &Vm, ev: u32, actor: Option<u32>) -> bool {
    let own = vm.events_of.iter().any(|(_, evs)| evs.contains(&ev));
    match actor {
        Some(a) => !own || vm.events_of.get(&a).is_some_and(|evs| evs.contains(&ev)),
        None => !own,
    }
}

/// More of the world the scripts hear: characters put to sleep (`DisSeqEvent_PutToSleep`),
/// Corvo holstering his weapon (`DisSeqEvent_PlayerHolsterWeapon`) and his weapons changing
/// (`DisSeqEvent_PlayerInventoryChanged`), his climbs (`DisSeqEvent_Climb`), props he drops
/// (`DisSeqEvent_MovableDropped`), Whalers' teleports (`DisSeqEvent_TeleportSpell`), particle
/// events (`SeqEvent_ParticleEvent`: the fireworks' launches and bursts) and walls of light
/// (`DisSeqEvent_WallOfLight`).
#[allow(clippy::too_many_arguments)]
fn more_events(
    mut vm: ResMut<Vm>,
    mut st: Local<MoreState>,
    npcs: Query<(&Npc, &FromSpawner)>,
    stats: Res<crate::gameplay::PlayerStats>,
    (climb, climbables): (Res<crate::climb::Climb>, Res<crate::climb::Climbables>),
    (held, props, level): (Res<crate::props::Held>, Query<&crate::props::Prop>, Option<Res<LevelInfo>>),
    (mut teleports, mut pfx, mut fled): (MessageReader<crate::assassin::Teleported>, MessageReader<crate::particles::ParticleEventFired>, MessageReader<crate::aiworld::FleePointReached>),
    mut devices: ResMut<crate::security::Devices>,
) {
    let vm = &mut *vm;
    // put to sleep: asleep now, not before
    let mut asleep = HashSet::new();
    for (n, f) in &npcs {
        if n.mode == crate::npc::Mode::Unconscious {
            asleep.insert(f.0);
        }
    }
    let evs = class_ops(&vm.g, "DisSeqEvent_PutToSleep");
    for s in asleep.difference(&st.asleep).copied().collect::<Vec<_>>() {
        let a = vm.spawner_actor.get(&s).copied();
        for &ev in &evs {
            if event_for(vm, ev, a) {
                if let Some(a) = a {
                    vm.write(ev, "Sleeping Pawn", Val::Actor(a));
                }
                vm.event(ev, 0, a.map(Val::Actor));
            }
        }
    }
    st.asleep = asleep;
    // the weapon holstered
    if stats.sheathed && !st.sheathed {
        for ev in class_ops(&vm.g, "DisSeqEvent_PlayerHolsterWeapon") {
            vm.event(ev, 0, Some(Val::Player));
        }
    }
    st.sheathed = stats.sheathed;
    // his weapons: received, lost (`m_pItemType`)
    for ev in class_ops(&vm.g, "DisSeqEvent_PlayerInventoryChanged") {
        let ty = vm.ps(ev, "m_pItemType").unwrap_or_default().to_ascii_lowercase();
        let has = if ty.contains("sword") {
            !stats.unarmed
        } else if ty.contains("crossbow") {
            stats.weapons && !stats.no_crossbow
        } else if ty.contains("pistol") {
            stats.weapons
        } else {
            let short = ty.rsplit('.').next().unwrap_or(&ty).to_string();
            stats.items.iter().any(|(k, n)| *n > 0 && k.to_ascii_lowercase().contains(&short))
        };
        match st.has.insert(ev, has) {
            Some(was) if was != has => {
                vm.event(ev, if has { 0 } else { 1 }, Some(Val::Player));
            }
            _ => {}
        }
    }
    // climbing: started, finished
    let on = climb.on.and_then(|i| climbables.instance(i));
    if on != st.climbing {
        if let Some(inst) = st.climbing {
            for a in vm.instance_actor.get(&inst).cloned().unwrap_or_default() {
                vm.actor_event(a, &["DisSeqEvent_Climb"], OutSel::Desc("Finish"), Some(Val::Player));
            }
        }
        if let Some(inst) = on {
            for a in vm.instance_actor.get(&inst).cloned().unwrap_or_default() {
                vm.actor_event(a, &["DisSeqEvent_Climb"], OutSel::Desc("Start"), Some(Val::Player));
            }
        }
        st.climbing = on;
    }
    // a prop let go of
    if held.0 != st.held {
        if let Some(e) = st.held.filter(|_| held.0.is_none()) {
            let inst = props.get(e).ok().and_then(|p| level.as_ref()?.scene.movables.get(p.index).map(|m| m.instance));
            let actor = inst.and_then(|i| vm.instance_actor.get(&i).and_then(|v| v.first().copied()));
            for ev in class_ops(&vm.g, "DisSeqEvent_MovableDropped") {
                if event_for(vm, ev, actor) {
                    vm.event(ev, 0, Some(Val::Player));
                }
            }
        }
        st.held = held.0;
    }
    // Whalers' teleports: gone, back
    for t in teleports.read() {
        let a = vm.spawner_actor.get(&t.spawner).copied();
        for ev in class_ops(&vm.g, "DisSeqEvent_TeleportSpell") {
            if event_for(vm, ev, a) {
                if let Some(i) = vm.output_index(ev, "Disappearance") {
                    vm.event(ev, i, a.map(Val::Actor));
                }
                if let Some(i) = vm.output_index(ev, "Reappearance") {
                    vm.event(ev, i, a.map(Val::Actor));
                }
            }
        }
    }
    // a fleeing character at a flee point (`DisSeqEvent_FleepointReached`, the point's)
    for f in fled.read() {
        let Some(&a) = st.actor_by_name.get(&f.point) else { continue };
        let who = vm.spawner_actor.get(&f.spawner).copied().map(Val::Actor);
        vm.actor_event(a, &["DisSeqEvent_FleepointReached"], OutSel::Index(0), who);
    }
    // particle events, by name
    let pfx_log = std::env::var("DH_PFX_EVENT_LOG").is_ok();
    for e in pfx.read() {
        let Some(&a) = vm.particle_actor.get(&e.index) else { continue };
        if pfx_log {
            info!("particle event {} on {}", e.name, vm.g.actors[a as usize].name);
        }
        for ev in vm.events_of.get(&a).cloned().unwrap_or_default() {
            if vm.g.ops[ev as usize].class == "SeqEvent_ParticleEvent" {
                if let Some(i) = vm.output_index(ev, &e.name) {
                    vm.event(ev, i, None);
                }
            }
        }
    }
    // walls of light: on, off, their polarity, a strike
    if st.actor_by_name.is_empty() {
        st.actor_by_name = vm.g.actors.iter().enumerate().map(|(i, a)| (a.name.clone(), i as u32)).collect();
    }
    let walls: Vec<String> = devices.list.iter().filter(|d| d.wall_face().is_some()).map(|d| d.def.actor.clone()).collect();
    for name in walls {
        let Some(now) = devices.state(&name) else { continue };
        let Some(&a) = st.actor_by_name.get(&name) else { continue };
        match st.walls.insert(name, now) {
            Some(was) if was != now => {
                if was.0 != now.0 {
                    vm.actor_event(a, &["DisSeqEvent_WallOfLight"], OutSel::Desc(if now.0 { "Activated" } else { "Deactivated" }), Some(Val::Player));
                }
                if was.1 != now.1 {
                    vm.actor_event(a, &["DisSeqEvent_WallOfLight"], OutSel::Desc(if now.1 { "Reversed Polarity" } else { "Default Polarity" }), Some(Val::Player));
                }
            }
            _ => {}
        }
    }
    for name in std::mem::take(&mut devices.wall_kills) {
        if let Some(&a) = st.actor_by_name.get(&name) {
            vm.actor_event(a, &["DisSeqEvent_WallOfLight"], OutSel::Desc("Kill Effect Activated"), None);
            vm.actor_event(a, &["DisSeqEvent_WallOfLight"], OutSel::Desc("Kill Effect Deactivated"), None);
        }
    }
}

fn class_ops(g: &Graph, class: &str) -> Vec<u32> {
    g.ops.iter().enumerate().filter(|(_, o)| o.class == class).map(|(i, _)| i as u32).collect()
}

fn run(time: Res<Time>, mut vm: ResMut<Vm>) {
    vm.pump();
    vm.tick(time.delta_secs());
    vm.pump();
}

/// Apply what the scripts asked for.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_effects(
    mut commands: Commands,
    mut vm: ResMut<Vm>,
    mut ui: ResMut<ScriptUi>,
    mut msgs: ResMut<HudMessages>,
    mut stats: ResMut<PlayerStats>,
    mut spawn: MessageWriter<SpawnRequest>,
    mut player: Query<(&mut Transform, &mut Player)>,
    mut npcs: Query<(Entity, &mut Npc, &FromSpawner, &mut Transform), Without<Player>>,
    mut instances: Query<(&LevelInstance, &mut Visibility, Option<&mut Door>), (Without<Player>, Without<Npc>)>,
    mut lights: Query<(&LevelLight, &mut Visibility), (Without<LevelInstance>, Without<Player>, Without<Npc>)>,
    pickups: Query<(Entity, &Pickup)>,
    (mut sounds, mut ambients, mut travel, mut store): (MessageWriter<crate::audio::PostEvent>, ResMut<crate::audio::Ambients>, MessageWriter<crate::mission::TravelRequest>, MessageWriter<crate::store::OpenStore>),
    level: Option<Res<LevelInfo>>,
    data: Option<Res<crate::gamedata::Data>>,
    (stream_cols, mut fog, mut autosave, mut choice, mut emitters): (
        Query<(&LevelInstance, &crate::level::InstanceCollider)>,
        ResMut<crate::fog::FogState>,
        MessageWriter<crate::save::SaveRequest>,
        ResMut<crate::choice::Choice>,
        Query<&mut crate::particles::ParticleEmitter>,
    ),
) {
    if vm.effects.is_empty() {
        return;
    }
    let effects = std::mem::take(&mut vm.effects);
    let g = vm.g.clone();
    for e in effects {
        let actor = |v: &Val| if let Val::Actor(a) = v { g.actors.get(*a as usize) } else { None };
        let voice = matches!(e, Effect::Voice(..));
        match e {
            Effect::Spawn(s) => {
                spawn.write(SpawnRequest(s));
            }
            Effect::Post(op, secs) => {
                // the level's settings with the op's fields
                let post = op.and_then(|op| {
                    let level = level.as_ref()?;
                    let mut p = level.scene.post;
                    if let Some(KVal::List(fields)) = g.ops.get(op as usize).and_then(|o| o.props.get("pp_fields")) {
                        let fields: Vec<(String, Vec<f32>)> = fields
                            .iter()
                            .filter_map(|f| {
                                let KVal::List(l) = f else { return None };
                                let Some(KVal::Str(name)) = l.first() else { return None };
                                Some((name.clone(), l[1..].iter().filter_map(|x| if let KVal::Float(x) = x { Some(*x) } else { None }).collect()))
                            })
                            .collect();
                        crate::postfx::apply_fields(&mut p, fields.iter().map(|(n, v)| (n.as_str(), v.as_slice())));
                    }
                    Some((1000 + op, p))
                });
                commands.queue(move |w: &mut World| {
                    let mut sp = w.resource_mut::<crate::postfx::ScriptPost>();
                    sp.0 = post;
                    sp.1 = secs.max(0.0);
                });
            }
            Effect::Movie(name) => {
                commands.queue(move |w: &mut World| match name {
                    Some(name) => {
                        w.write_message(crate::movie::PlayMovie { name, looping: false, skippable: false, layer: crate::movie::MovieLayer::Fullscreen });
                    }
                    None => {
                        w.write_message(crate::movie::StopMovie);
                    }
                });
            }
            Effect::Sound(ev, at) | Effect::Voice(ev, at) => {
                if std::env::var("DH_AUDIO_LOG").is_ok() {
                    info!("audio: script posts {ev}");
                }
                // a character speaks from where it is now
                let pos = at.as_ref().and_then(|v| actor(v)).map(|a| {
                    a.spawner
                        .and_then(|s| npcs.iter().find(|(_, _, f, _)| f.0 == s).map(|(_, _, _, t)| t.translation + Vec3::Y * 0.6))
                        .unwrap_or(Vec3::from(a.position))
                });
                // a character's line: its jaw follows
                let speaker = at.as_ref().and_then(|v| actor(v)).and_then(|a| a.spawner).and_then(|s| npcs.iter().find(|(_, _, f, _)| f.0 == s).map(|x| x.0));
                let post = crate::audio::PostEvent::named(&ev, pos);
                let post = if voice { post.voiced() } else { post };
                sounds.write(match speaker {
                    Some(e) => post.spoken_by(e),
                    None => post,
                });
            }
            Effect::Ambient(v, on) => {
                if let Some(i) = actor(&v).and_then(|a| a.sound) {
                    ambients.set_enabled(i as usize, on);
                } else if let Val::Actor(a) = v {
                    // a factory's sound (a PA speaker's hum), where it was made
                    if let Some(ev) = vm.factory_sounds.get(&a).cloned() {
                        let i = match vm.factory_ambients.get(&a) {
                            Some(&i) => i,
                            None => {
                                let i = ambients.add(&ev, Vec3::from(g.actors[a as usize].position));
                                vm.factory_ambients.insert(a, i);
                                i
                            }
                        };
                        ambients.set_enabled(i, on);
                        if std::env::var("DH_AUDIO_LOG").is_ok() {
                            info!("audio: factory sound {ev} at {} {}", g.actors[a as usize].name, if on { "on" } else { "off" });
                        }
                    }
                }
            }
            Effect::Power(name, level) => {
                info!("level scripts: power {name} -> level {level}");
                if level == 0 {
                    stats.powers.remove(&name);
                } else if stats.power(&name) < level {
                    stats.powers.insert(name, level);
                }
            }
            Effect::Store(tweak) => {
                store.write(crate::store::OpenStore(tweak));
            }
            Effect::Choice(op, options) => {
                choice.ask(op, options);
            }
            Effect::Hud(el, on) => {
                if on {
                    ui.hud_hidden.remove(&el);
                } else {
                    ui.hud_hidden.insert(el);
                }
            }
            Effect::HudAll(show) => {
                let show = show.unwrap_or(ui.hud_hidden.contains(HUD_ALL));
                if show {
                    ui.hud_hidden.remove(HUD_ALL);
                } else {
                    ui.hud_hidden.insert(HUD_ALL.to_string());
                }
            }
            Effect::Key(name) => {
                if !stats.keys.contains(&name) {
                    stats.keys.push(name.clone());
                    msgs.push(name);
                }
            }
            Effect::TakeItem(path) => {
                // (journal notes are kept as `Group.Name`)
                let short = path.split_once('.').map(|x| x.1.to_string()).unwrap_or(path.clone());
                stats.notes.retain(|n| *n != short && *n != path);
                if let Some(c) = stats.items.get_mut(&short) {
                    *c = c.saturating_sub(1);
                }
            }
            Effect::AutoSave => {
                // (test runs leave the player's saves alone)
                if std::env::var("DH_SCRIPT").is_err() || std::env::var("DH_AUTOSAVE").is_ok() {
                    autosave.write(crate::save::SaveRequest(crate::save::AUTOSAVE_SLOT));
                }
            }
            Effect::Hide(v, h) => {
                let Some(a) = actor(&v) else { continue };
                for (li, mut vis, _) in &mut instances {
                    if a.instances.contains(&li.index) {
                        let hide = h.unwrap_or(*vis != Visibility::Hidden);
                        *vis = if hide { Visibility::Hidden } else { Visibility::Inherited };
                    }
                }
            }
            Effect::Destroy(v) => {
                let Some(a) = actor(&v) else { continue };
                if let Some(s) = a.spawner {
                    for (ne, _, from, _) in &npcs {
                        if from.0 == s {
                            commands.entity(ne).try_despawn();
                        }
                    }
                }
                // gone, its collision with it; and nothing to use any more (a lever, the outer
                // door once the bomb is set)
                for (li, mut vis, _) in &mut instances {
                    if a.instances.contains(&li.index) {
                        *vis = Visibility::Hidden;
                    }
                }
                for (li, col) in &stream_cols {
                    if a.instances.contains(&li.index) {
                        commands.entity(col.0).insert(bevy_rapier3d::prelude::ColliderDisabled);
                    }
                }
                // an emitter goes out (a fire blown out)
                if !a.particles.is_empty() {
                    for mut em in &mut emitters {
                        if a.particles.contains(&em.index) {
                            em.active = false;
                        }
                    }
                }
                if let Val::Actor(ai) = v {
                    commands.queue(move |w: &mut World| {
                        let rigs: Vec<Entity> = w.query::<(Entity, &crate::usables::UsableRig)>().iter(w).filter(|(_, r)| r.actor() == Some(ai)).map(|(e, _)| e).collect();
                        for e in rigs {
                            w.entity_mut(e).remove::<crate::usables::UsableRig>();
                        }
                        let usables: Vec<Entity> = w.query::<(Entity, &crate::interact::Usable)>().iter(w).filter(|(_, u)| u.actor == ai).map(|(e, _)| e).collect();
                        for e in usables {
                            w.entity_mut(e).remove::<crate::interact::Usable>();
                        }
                    });
                }
                if let Some(p) = a.pickup {
                    for (pe, pk) in &pickups {
                        if pk.index == p {
                            for &m in &pk.entities {
                                commands.entity(m).try_despawn();
                            }
                            commands.entity(pe).try_despawn();
                        }
                    }
                }
            }
            Effect::Light(v, on) => {
                let Some(a) = actor(&v) else { continue };
                // fog layers switch too (`DisFog.OnToggle`)
                if let Some(f) = a.fog.and_then(|f| fog.enabled.get_mut(f as usize)) {
                    *f = on.unwrap_or(!*f);
                }
                for (ll, mut vis) in &mut lights {
                    if a.lights.contains(&ll.0) {
                        let show = on.unwrap_or(*vis == Visibility::Hidden);
                        *vis = if show { Visibility::Inherited } else { Visibility::Hidden };
                    }
                }
                // an emitter switches on (`ActivateSystem`: fireworks, a puff of smoke) or off
                if !a.particles.is_empty() {
                    for mut em in &mut emitters {
                        if a.particles.contains(&em.index) {
                            em.active = on.unwrap_or(!em.active);
                        }
                    }
                }
            }
            Effect::Teleport(v, pos, yaw) => match v {
                Val::Player => {
                    if let Ok((mut t, mut p)) = player.single_mut() {
                        // (UE3 finds the pawn a spot out of the floor: points sit at or a little
                        // under it - the Prison cell's is 0.26 m down; the feet start 0.35 m up)
                        t.translation = pos + Vec3::Y * (crate::player::STAND_HALF + crate::player::RADIUS + 0.35);
                        p.yaw = yaw;
                        p.velocity = Vec3::ZERO;
                    }
                }
                _ => {
                    let Some(s) = actor(&v).and_then(|a| a.spawner) else { continue };
                    for (_, mut npc, from, mut t) in &mut npcs {
                        if from.0 == s {
                            t.translation = pos + Vec3::Y * (crate::npc::NPC_CENTER + 0.05);
                            npc.yaw = yaw;
                        }
                    }
                }
            },
            Effect::Door(v, cmd) => {
                let Some(a) = actor(&v) else { continue };
                for (li, _, door) in &mut instances {
                    let Some(mut door) = door else { continue };
                    if !a.instances.contains(&li.index) {
                        continue;
                    }
                    match cmd {
                        DoorCmd::Open(cw) => {
                            door.locked = false;
                            door.dir = if cw { 1.0 } else { -1.0 };
                            door.target = 1.0;
                        }
                        DoorCmd::Close => door.target = 0.0,
                        DoorCmd::Lock => door.locked = true,
                        DoorCmd::Unlock => door.locked = false,
                    }
                }
            }
            Effect::Damage(v, amount) => match v {
                Val::Player => {
                    if amount >= 0.0 {
                        stats.take_damage(amount);
                    } else {
                        stats.health = (stats.health - amount).clamp(0.0, stats.max_health);
                    }
                }
                _ => {
                    let Some(s) = actor(&v).and_then(|a| a.spawner) else { continue };
                    for (_, mut npc, from, _) in &mut npcs {
                        if from.0 == s && !npc.is_down() {
                            npc.health -= amount;
                            if npc.health <= 0.0 {
                                npc.mode = Mode::Dead;
                            }
                        }
                    }
                }
            },
            Effect::Message(m) => msgs.push(m),
            Effect::Tutorial(m) => msgs.tutorial(m, 10.0),
            Effect::Objective(n) => commands.queue(move |w: &mut World| {
                if let Some(mut q) = w.get_resource_mut::<crate::objnotify::ObjectiveQueue>() {
                    q.push(n);
                }
            }),
            Effect::Location(l) => ui.location = Some(l),
            Effect::Subtitle(t, d) => ui.subtitle = Some((t, d)),
            Effect::Stream(name, load) => {
                // the sublevel's characters come or go
                let Some(l) = level.as_ref().and_then(|l| l.scene.levels.iter().find(|r| r.name.eq_ignore_ascii_case(&name)).cloned()) else { continue };
                // its things appear (their colliders with them) or go
                for (li, mut vis, _) in &mut instances {
                    if li.index >= l.instances.0 && li.index < l.instances.1 {
                        *vis = if load { Visibility::Inherited } else { Visibility::Hidden };
                    }
                }
                for (li, col) in &stream_cols {
                    if li.index >= l.instances.0 && li.index < l.instances.1 {
                        if load {
                            commands.entity(col.0).remove::<bevy_rapier3d::prelude::ColliderDisabled>();
                        } else {
                            commands.entity(col.0).insert(bevy_rapier3d::prelude::ColliderDisabled);
                        }
                    }
                }
                if load {
                    for s in l.spawners.0..l.spawners.1 {
                        if level.as_ref().and_then(|lv| lv.scene.spawners.get(s as usize)).is_some_and(|sp| sp.spawn_on_begin_play) {
                            spawn.write(SpawnRequest(s));
                        }
                    }
                } else {
                    for (e, _, from, _) in &npcs {
                        if from.0 >= l.spawners.0 && from.0 < l.spawners.1 {
                            commands.entity(e).try_despawn();
                        }
                    }
                }
            }
            Effect::Line(t, d, speaker) => {
                // "Admiral Havelock: ..." when a character speaks
                let name = speaker.and_then(|a| g.actors.get(a as usize)).and_then(|ka| ka.spawner).and_then(|s| {
                    let (_, npc, _, _) = npcs.iter().find(|(_, _, f, _)| f.0 == s)?;
                    let (package, object) = npc.pawn.split_once('.').unwrap_or((&npc.pawn, &npc.pawn));
                    let names = &data.as_ref()?.0.pawn_names;
                    names.get(object).or(names.get(package)).cloned()
                });
                ui.subtitle = Some((name.map(|n| format!("{n}: {t}")).unwrap_or(t), d));
            }
            Effect::FadeIn(time) => {
                if ui.alpha > 0.01 && ui.fade.map(|f| f.1 > 0.5).unwrap_or(true) {
                    ui.fade = Some((ui.alpha, 0.0, time, 0.0));
                }
            }
            Effect::Fade { to, time } => ui.fade = Some((ui.alpha, to, time.max(0.01), 0.0)),
            Effect::GoTo(v, pos) => {
                let Some(s) = actor(&v).and_then(|a| a.spawner) else { continue };
                for (_, mut npc, from, _) in &mut npcs {
                    if from.0 == s && !npc.is_down() {
                        npc.target = Some(pos);
                        npc.home = pos;
                    }
                }
            }
            // the travel destination: a player start of this map (where a map change the
            // campaign made arrives); else, without the campaign's scripts, the map that has it
            Effect::Velocity(v, vel) => match v {
                Val::Player => {
                    if let Ok((_, mut p)) = player.single_mut() {
                        p.velocity = vel;
                        p.grounded = false;
                    }
                }
                Val::Actor(a) => vm.velocities.push((a, vel)),
                _ => {}
            },
            Effect::Place(v, pos, yaw) => match v {
                Val::Player => {
                    if let Ok((mut t, mut p)) = player.single_mut() {
                        if let Some(pos) = pos {
                            t.translation = pos + Vec3::Y * (crate::player::STAND_HALF + crate::player::RADIUS);
                            p.velocity = Vec3::ZERO;
                        }
                        if let Some(y) = yaw {
                            p.yaw = y;
                        }
                    }
                }
                _ => {
                    let Some(s) = actor(&v).and_then(|a| a.spawner) else { continue };
                    for (_, mut npc, from, mut t) in &mut npcs {
                        if from.0 == s {
                            if let Some(pos) = pos {
                                t.translation = pos;
                            }
                            if let Some(y) = yaw {
                                npc.yaw = y;
                            }
                        }
                    }
                }
            },
            Effect::StartMap(map) => {
                // (the campaign takes Corvo on itself)
                if vm.campaign.is_none() {
                    match crate::campaign::start_in(&map, None) {
                        Some((name, start)) => {
                            info!("level scripts: on to {name}");
                            travel.write(crate::mission::TravelRequest { map: name, start });
                        }
                        None => warn!("level scripts: map {map} is not cooked"),
                    }
                }
            }
            Effect::Travel(tag) => {
                let here = level.as_ref().and_then(|l| l.scene.player_starts.iter().find(|s| s.tag.eq_ignore_ascii_case(&tag)).cloned());
                match here {
                    Some(s) => {
                        if let Ok((mut t, mut p)) = player.single_mut() {
                            let pos = Vec3::from(s.position) + Vec3::Y * 0.15;
                            if t.translation.distance(pos) > 2.0 {
                                info!("level scripts: to the travel destination {tag}");
                                t.translation = pos;
                                p.yaw = s.yaw;
                                p.velocity = Vec3::ZERO;
                            }
                        }
                    }
                    None if vm.campaign.is_some() => vm.goto_fallback = Some((tag, 1.5)),
                    None => match find_start(&tag) {
                        Some((map, start)) => {
                            info!("level scripts: travel to {map} (start {tag})");
                            travel.write(crate::mission::TravelRequest { map, start });
                        }
                        None => warn!("level scripts: travel destination {tag} not found in the cooked maps"),
                    },
                }
            }
        }
    }
}

/// The cooked map that has a player start tagged `tag`.
pub fn find_start(tag: &str) -> Option<(String, usize)> {
    #[derive(serde::Deserialize)]
    struct Starts {
        name: String,
        player_starts: Vec<dhcook::format::PlayerStart>,
    }
    let dir = crate::loading::cache_dir().join("maps");
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(d) = std::fs::read(&p) else { continue };
        let Ok(s) = serde_json::from_slice::<Starts>(&d) else { continue };
        if let Some(i) = s.player_starts.iter().position(|ps| ps.tag.eq_ignore_ascii_case(tag)) {
            return Some((s.name, i));
        }
    }
    None
}


fn update_ui(
    time: Res<Time>,
    vm: Res<Vm>,
    mut ui: ResMut<ScriptUi>,
    mut barks: MessageReader<crate::barks::Subtitle>,
    cinematic_fade: Res<crate::matinee::CinematicFade>,
    settings: Res<crate::settings::Settings>,
    mut objectives: Query<(&mut Text, &mut TextColor), With<ObjectivesText>>,
    mut fade: Query<&mut BackgroundColor, With<FadeOverlay>>,
    mut subtitle: Query<&mut Text, (With<SubtitleText>, Without<ObjectivesText>)>,
) {
    let dt = time.delta_secs();
    // AI barks show when no dialogue is on screen
    for b in barks.read() {
        // (`SubtitlesMode_Dialogs`: the barks unsubtitled)
        if settings.subtitle_mode == 1 && b.ambient {
            continue;
        }
        if ui.subtitle.is_none() || b.priority {
            ui.subtitle = Some((b.text.clone(), b.secs));
        }
    }
    let line = match &mut ui.subtitle {
        Some((t, left)) => {
            *left -= dt;
            if *left <= 0.0 { String::new() } else { t.clone() }
        }
        None => String::new(),
    };
    if line.is_empty() {
        ui.subtitle = None;
    }
    let line = if settings.subtitle_mode > 0 { line } else { String::new() };
    if let Ok(mut t) = subtitle.single_mut() {
        if t.0 != line {
            t.0 = line;
        }
    }
    if vm.is_changed() {
        if let Ok((mut t, mut c)) = objectives.single_mut() {
            let mut s = String::new();
            for path in &vm.objectives {
                let Some(o) = vm.g.objectives.iter().find(|o| o.path == *path) else { continue };
                let done = vm.tasks.get(path).map(|t| t.0 == TaskState::Completed).unwrap_or(false);
                if done {
                    continue;
                }
                s.push_str(&o.text);
                s.push('\n');
                for task in &o.tasks {
                    if let Some((state, hidden)) = vm.tasks.get(&task.path) {
                        if !hidden && *state == TaskState::Active {
                            s.push_str("   ");
                            s.push_str(&task.text);
                            s.push('\n');
                        }
                    }
                }
            }
            if t.0 != s {
                t.0 = s;
                ui.objectives_age = 0.0;
            }
            // (the original shows changes as they happen, `objnotify`, and keeps the list in the
            // journal; `DH_OBJECTIVES=1` keeps it up here)
            ui.objectives_age += dt;
            let a = if std::env::var("DH_OBJECTIVES").is_ok() { 1.0 } else { 0.0 };
            let want = c.0.with_alpha(0.9 * a);
            if c.0 != want {
                c.0 = want;
            }
        }
    }
    if let Some(a) = cinematic_fade.0 {
        ui.alpha = a;
        ui.black = 0.0;
    } else if let Some((from, to, len, el)) = &mut ui.fade {
        *el += dt;
        let k = (*el / *len).min(1.0);
        let a = *from + (*to - *from) * k;
        let finished = k >= 1.0;
        let to_v = *to;
        ui.alpha = a;
        if finished {
            ui.fade = None;
            ui.alpha = to_v;
        }
    }
    // never leave the player blind for long (fade-ins we can't reproduce)
    if ui.alpha > 0.99 && ui.fade.is_none() {
        ui.black += dt;
        if ui.black > 8.0 {
            ui.fade = Some((1.0, 0.0, 1.0, 0.0));
        }
    } else {
        ui.black = 0.0;
    }
    if let Ok(mut bg) = fade.single_mut() {
        let want = Color::srgba(0.0, 0.0, 0.0, ui.alpha.clamp(0.0, 1.0));
        if bg.0 != want {
            bg.0 = want;
        }
    }
}
