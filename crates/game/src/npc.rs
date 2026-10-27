//! NPCs: spawned from the level's DishonoredSpawners using the original skinned models,
//! animated with the pawn's original animation sets (procedural fallback for characters
//! without them), with perception and a stealth/combat state machine.

use crate::anim::{Animator, CharAnims, ClipId, LocoMix};
use crate::gameplay::{HitKind, Noise, NoiseKind, NpcHit, NpcStagger, PlayerHit, PlayerStats, TimeControl};
use crate::level::{GameAssets, LevelInfo, LevelSpawnSet, GROUP_NPC, GROUP_PLAYER, GROUP_PROP, GROUP_WORLD};
use crate::player::Player;
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

pub struct NpcPlugin;

pub(crate) fn age_corpses(time: Res<Time>, tc: Res<TimeControl>, mut npcs: Query<&mut Npc>) {
    let dt = time.delta_secs() * tc.world_scale().max(0.0);
    for mut npc in &mut npcs {
        npc.corpse_age = if npc.mode == Mode::Dead { npc.corpse_age + dt } else { 0.0 };
    }
}

impl Plugin for NpcPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerTrail>()
            .init_resource::<ArrivalTimer>()
            .add_message::<SpawnRequest>()
            .add_message::<NpcFinisher>()
            .add_message::<NpcSpawned>()
            .add_message::<NpcShot>()
            .add_message::<AllyCall>()
            .add_systems(OnEnter(GameState::InGame), spawn_npcs.after(LevelSpawnSet))
            .init_resource::<HandledBodies>()
            .add_systems(OnEnter(GameState::InGame), |mut h: ResMut<HandledBodies>| h.0.clear())
            .add_systems(Update, body_lights.before(npc_perception).run_if(in_state(GameState::InGame)))
            .add_systems(First, log_jumps.run_if(in_state(GameState::InGame)))
            .add_systems(Update, sticky_struck.before(npc_select_anim).run_if(in_state(GameState::InGame)))
            .add_systems(Update, npc_faces.after(npc_select_anim).before(npc_animate).run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, model_details.run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, log_jumps.after(bevy_rapier3d::plugin::PhysicsSet::Writeback).run_if(in_state(GameState::InGame)))
            .add_systems(
                PostUpdate,
                (scene_head_look, head_track, speaking_jaw).after(crate::anim::AnimPose).before(bevy::transform::TransformSystems::Propagate).run_if(in_state(GameState::InGame)),
            )
            .add_systems(
                Update,
                (
                    record_trail,
                    spawn_requested,
                    npc_hits,
                    last_survivors,
                    age_corpses,
                    (ash_marked, electrocute, burn_ashes).chain(),
                    npc_shots,
                    npc_hearing,
                    npc_perception.run_if(|| !skip("perception")),
                    (attention_look, escape_explosions).chain(),
                    npc_brain.run_if(|| !skip("brain")),
                    npc_finishers,
                    weeper_grab,
                    hold_scripted,
                    scripted_upper,
                    npc_move.run_if(|| !skip("move")),
                    npc_select_anim.run_if(|| !skip("animate")),
                    npc_animate.run_if(|| !skip("animate")),
                    drop_swords,
                    log_npc_clips.run_if(|| std::env::var("DH_NPC_CLIP_LOG").is_ok()),
                )
                    .chain()
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

/// A sticky grenade stuck to someone (`gadgets`): they flail at it till it goes off.
#[derive(Component)]
pub struct StuckGrenade;

/// Someone a sticky grenade has stuck to flails at it (`Generic_StickyWeapon`), standing for it.
fn sticky_struck(mut commands: Commands, mut npcs: Query<(Entity, &mut Npc), With<StuckGrenade>>) {
    for (e, mut npc) in &mut npcs {
        commands.entity(e).try_remove::<StuckGrenade>();
        if !npc.is_down() && !npc.floored {
            npc.attack_t = None;
            npc.swing = None;
            npc.aim = None;
            npc.gesture(21);
        }
    }
}

/// Profiling aid: `DH_SKIP=move,animate` disables the named NPC systems.
fn skip(name: &str) -> bool {
    static SKIP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SKIP.get_or_init(|| std::env::var("DH_SKIP").unwrap_or_default()).split(',').any(|s| s == name)
}

pub const NPC_HALF: f32 = 0.6; // capsule half segment
pub const NPC_RADIUS: f32 = 0.34;
pub const NPC_CENTER: f32 = 0.97; // capsule centre above the feet (UE3 Origin)
/// How far a character's gunshot is heard (`DisTweaks_NPCFireGun.m_AINoiseLoudnessWhenFiring`,
/// `EAINoiseLoudness4`: 6000 uu).
const GUN_NOISE: f32 = 60.0;
const WALK: f32 = 1.45;
const ALERT_WALK: f32 = 2.2;
const RUN: f32 = 4.6;
/// Time since the level appeared (reset on every spawn / restart).
#[derive(Resource, Default)]
pub struct ArrivalTimer(pub f32);

/// Seconds after the level appears before NPCs can perceive the player.
const ARRIVAL_GRACE: f32 = 3.0;
/// How many NPCs may be in an attack swing at the same time (others wait their turn).
/// The most blades at Corvo at once, by difficulty (`DefaultAI.ini` `m_InnerSkirmishCaps`:
/// easy 1, normal 2, hard and very hard 3).
const INNER_SKIRMISH_CAPS: [usize; 4] = [1, 2, 3, 3];
/// Beyond this distance from the player, unaware NPCs move without the character controller.
const FAR_MOVE: f32 = 25.0;
/// The most such an NPC steps up (m), as its controller's autostep and ground snap.
const FAR_STEP: f32 = 0.45;
/// A relaxed walk's start, stop and turn props (the idle speed modes of the
/// `ArkComponentLocomotionConfig`s: `m_fStraightDist` 70 uu, the path it needs ahead to start and
/// the mark it stops short of), and where a walk's goal counts as reached once it has stopped
/// with its clip (`m_fUnaccuracyRadius`, 120 uu), m.
const WALK_STRAIGHT: f32 = 1.4;
const UNACCURACY: f32 = 2.4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Guard,
    Thug,
    Civilian,
    Story,
    /// wolfhounds and other animals
    Creature,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Alert {
    Unaware,
    Suspicious,
    Combat,
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Idle,
    Patrol,
    Investigate,
    Search,
    Combat,
    Return,
    Flee,
    Choked,
    Unconscious,
    Dead,
}

#[derive(Component)]
pub struct Npc {
    /// the spawner it came from
    pub spawner: u32,
    pub kind: Kind,
    pub name: String,
    /// the pawn type (`Twk_Pawn_AntonSokolov`...)
    pub pawn: String,
    /// its health, in the original's units (Corvo has 70), and its full health
    /// (`m_HealthMax`)
    pub health: f32,
    pub max_health: f32,
    /// how it fights (its type's attributes and weapons)
    pub arms: Arms,
    /// Corvo's blows it has parried in a row; seconds since it was last hurt
    pub parries: u8,
    pub hurt_t: f32,
    /// seconds before each of its grenade throws may come again; a leap back from Corvo
    /// (where to, for how long)
    pub grenade_cd: Vec<f32>,
    pub retreat: Option<(Vec3, f32)>,
    /// the blow under way (with `attack_t`), and each move's cooldown
    pub swing: Option<Swing>,
    pub melee_cd: Vec<f32>,
    /// a gesture to play once (1 its parry won, 2 its blow parried, 3 a shove; 12 a taunt, 13 a
    /// weeper's cough) and its count;
    /// a riposte due in (s)
    pub act: (u8, u32),
    pub riposte: f32,
    /// its personal space (`DisAISubProcessPersonalSpace`): warning Corvo off (s left), and till
    /// when it won't warn him, or shove him, again
    pub warn: Option<f32>,
    pub warn_cd: f32,
    pub push_cd: f32,
    /// stopped on its walk by something it head-tracks behind it (`DisAISubProcessHeadTrackPause`):
    /// the heading it walked (`m_OriginalPauseYaw`)
    pub track_pause: Option<f32>,
    /// a bark asked for (its count, the hook)
    pub bark_req: (u32, &'static str),
    /// its blade locked with Corvo's (`combat::Versus`): it holds
    pub versus: bool,
    /// lying in wait for him round a corner (`DisTweaks_NPCAmbush`: how long), and till when
    /// it won't again
    pub ambush: Option<f32>,
    pub ambush_cd: f32,
    pub mode: Mode,
    pub alert: Alert,
    /// Detection meter 0..1 (1 = fully detected).
    pub awareness: f32,
    pub yaw: f32,
    pub velocity: Vec3,
    pub grounded: bool,
    pub home: Vec3,
    pub home_yaw: f32,
    /// its patrol (`patrol`): the route it walks, where on it, what it guards
    pub patrol: crate::patrol::Patrol,
    pub wait: f32,
    pub target: Option<Vec3>,
    pub last_seen: Option<Vec3>,
    pub sees_player: bool,
    pub mode_timer: f32,
    pub attack_cd: f32,
    /// Seconds into the current melee attack.
    pub attack_t: Option<f32>,
    pub stagger: f32,
    /// a sleep dart taking hold (seconds left: `DisDamageType_Arrow_Sleep.m_fTimeBeforeSleep`,
    /// for those in a fight)
    pub sleep_in: Option<f32>,
    pub stuck: f32,
    pub sidestep: f32,
    pub progress_pos: Vec3,
    pub anim_phase: f32,
    /// its locomotion loops' sync position (0..1) and its feet's on-ground share, while they play
    /// (`npc_select_anim`): its footfalls as its feet come down
    pub sync: Option<(f32, f32)>,
    foot_sync: f32,
    pub anim_speed: f32,
    /// how far its walk or run takes it in a loop of the clip, two steps (m: the clip's root
    /// motion; 0 before it plays one)
    pub stride: f32,
    pub hit_react: f32,
    pub down_t: f32,
    /// World-time seconds since death; separate from the short fall blend.
    pub corpse_age: f32,
    pub look: f32,
    pub has_sword: bool,
    /// ranged weapon: 0 none, 1 pistol (elite guards), 2 bow (tallboys)
    pub ranged: u8,
    /// seconds before the next shot (`m_ContextCooldown`)
    pub shoot_cd: f32,
    /// its pistol being aimed: seconds to the shot, seconds Corvo has been out of its sight
    pub aim: Option<(f32, f32)>,
    /// the pistol shots it has fired (its animation follows them)
    pub shots: u32,
    /// seconds it stands after a pistol shot, for the shot and putting the gun away
    pub gun_hold: f32,
    /// until it strikes under a hideout again, with its blade and with its pistol
    pub under_cd: [f32; 2],
    /// until it may taunt again, and how long its taunt holds it
    pub taunt_cd: f32,
    /// on its way to a body it found, to look it over
    pub body_check: bool,
    /// Corvo has just come out of it (its possession over): reeling to come
    pub unpossessed: bool,
    /// casting its Attract spell, Corvo drawn in to it (a whaler's pull, `assassin.rs`)
    pub casting: bool,
    /// knocked or blown off its feet: the push to go limp with, where its clips can get it up,
    /// and its throw
    pub knock: Option<(Vec3, Throw)>,
    /// thrown, in the air on its push before it goes limp (the original's animated throw, its
    /// `PHYS_Flying` moment)
    pub flying: bool,
    /// its walk begun or ended with its clip; what it wished to go this frame (its path's way, at
    /// its pace), and its path: how far to its goal and to the path's next point
    pub pace: Option<Pace>,
    /// turning where it stands with its turn's clip (`npc_select_anim`): it sets off after
    pub turning: bool,
    pub wish: Vec3,
    pub path: Option<(f32, f32)>,
    /// stopped with its stop's clip, short of its goal (reached, within `UNACCURACY`)
    pub halted: bool,
    /// knocked off its feet, limp till it lies still (`ragdoll`), then getting up
    pub floored: bool,
    /// lying still after it, getting up: from its back (or its front)
    pub rise: Option<bool>,
    /// the blow it last took was a shot to a leg: its right (or left)
    pub leg_hit: Option<bool>,
    /// the blow it last took was a thing thrown at it (a bottle, a brick: `DisDamageType_Impact`)
    pub small_hit: bool,
    /// a weak hit taken (Corvo's sword: its flinch laid over what it does, nothing stopped),
    /// from the side struck (front, left, right, back); its set has the flinch's clips
    pub weak_hit: Option<usize>,
    pub weak_hits: bool,
    /// dodging Corvo's blow (s left: his blade passes it by), and its set has the dodge's clips
    /// (`Sword_DodgeLeftIn` / `Out`)
    pub dodging: f32,
    pub dodges: bool,
    /// Corvo's blows that threatened it lately, and how long since the last; how long his
    /// firearm or bow has been aimed at it, and how long it bears that before stepping aside
    pub incoming: (u32, f32),
    pub ranged_threat: (f32, f32),
    /// the blow it last took was a bolt (a bullet knocks it down)
    pub shot_hit: bool,
    /// choking in a cloud of chokedust (its stun's length: `stagger`)
    pub dusted: bool,
    /// its bearing about Corvo while it waits outside the ring of those at him (radians)
    pub flank: Option<f32>,
    pub taunt_hold: f32,
    /// investigating or searching at a run (`TweaksBehavior_Search_Default`'s `m_bShouldRun`:
    /// danger, help, alarm and death-rattle noises, corpses, sightings), else at a walk
    pub search_run: bool,
    /// it saw one of its own die (the alarm's `m_fAlarmChanceUponWitnessDeath`)
    pub witnessed_death: bool,
    /// the bodies it has found (each once), and the one it goes to look over
    pub found_bodies: Vec<Entity>,
    /// the allies in its sight, what each was doing when it saw them (fighting 1, searching 2,
    /// panicking 3) and when it last saw them so: a new sighting is reacted to once, one out of
    /// its sight a second (`m_fLostContactTimeOut`) is lost (0x00B435F0)
    pub allies_seen: Vec<(Entity, u8, f32)>,
    pub checking: Option<Entity>,
    /// the walls of light and arc pylons it saw kill an ally, rewired (`distrust`; indices in
    /// `security::Devices`)
    pub distrust: Vec<usize>,
    /// Corvo's magic seen (`magic`): the while after it (seconds left, busted when it began),
    /// its reaction to come (seconds, he vanished), and the line it says this frame (he vanished)
    pub magic_pending: Option<(f32, bool)>,
    pub magic_react: Option<(f32, bool)>,
    pub magic_bark: Option<bool>,
    /// what it says this frame of a body Corvo carries (busted, not his enemy: the body one of
    /// its side's 0, neutral to it 1, its enemy's 2)
    pub carry_bark: Option<u8>,
    /// its brain's suspicion level (`m_SuspicionLevel`): Suspecting once a body, a fight, an
    /// alarm, a blow or Corvo seen up close has set it; nothing but the level scripts takes it
    /// back (decoded for this rewrite: no timer, no search's end)
    pub suspecting: bool,
    /// its search's check clip's cooldown (10 s: a searcher come to a spot looks it over, and
    /// tells the others there, only so often)
    pub check_cd: f32,
    /// a reaction under way (`DisBehaviorReact`: a Harmless response's remark), for so long yet
    pub reacting: f32,
    /// searching at the scripts' bidding (`DisSeqAct_AIDoSearch`'s `DisBehaviorPatrolSearch`)
    pub scripted_search: bool,
    /// before setting off to look into something, its search's reaction (`m_SearchReactionDelay`,
    /// 0-1 s: it turns to it first)
    pub react: f32,
    /// Corvo dealt its last blow (else another character or the world did)
    pub hit_by_player: bool,
    /// its panic behaviour's last-survivor count (`DisAIBrainProcessPanic.m_KillQueue`: when
    /// each of its own fell to Corvo), how often it has panicked, and, panicking, how near Corvo
    /// may come before it turns on him (a fighter's `EReaction_Fight`; 0: it never does) and how
    /// long it runs heedless first (`m_fFreeFlightTimer`)
    pub kill_queue: Vec<f32>,
    pub panics: u32,
    pub panic_fight: f32,
    pub free_flight: f32,
    /// a plague carrier's time until it coughs again, and how long its cough holds it
    pub cough_cd: f32,
    pub cough_hold: f32,
    /// where the last blow came from (its reaction and its death go by the side it struck)
    pub hit_from: Option<Vec3>,
    /// an NPC of an enemy faction it is fighting (`DisTweaks_Faction.m_EnemyFactions`), and
    /// when it next looks for one
    pub foe: Option<Entity>,
    pub feud_scan: f32,
    /// a weeper's arm grab (`DisTweaks_WeeperGrab`): phase and time in it
    pub grab: Option<(GrabPhase, f32)>,
    /// a wolfhound's leap, or its letting go: how long its clip lasts (s; 0 a weeper's own
    /// pace), and how fast the clip carries it forward meanwhile (m/s, back below 0)
    pub grab_len: f32,
    pub grab_drift: f32,
    /// a wolfhound's grab is its jump attack (not its arm attack); the time before each may come
    /// again (its arm attack's, its jump attack's)
    pub grab_jump: bool,
    /// a weeper running after Corvo (one of the few its kind allow), standing weeping after its
    /// grab (s left), and losing him far from its home (s left)
    pub weep_run: bool,
    pub weep_stand: f32,
    pub weep_lost: Option<f32>,
    pub leap_cd: [f32; 2],
    pub trail_idx: usize,
    pub saw_body: bool,
    /// Pre-placed corpse: lies dead from the start and doesn't alarm guards.
    pub corpse: bool,
    /// Ground speeds: walk, alert walk, run, panic run (from the animations' root motion).
    pub gait: [f32; 4],
    /// Voice (index into `scene.barks`).
    pub voice: Option<u32>,
    /// The faction (original `DisTweaks_Faction`) and whether it is Corvo's enemy.
    pub faction: String,
    pub enemy: bool,
    /// story group (`Twk_ID_CityGuard`): the Heart's secrets about them
    pub story_group: String,
    /// its type's own stance towards Corvo (the scripts' dispositions override `enemy`)
    pub enemy_default: bool,
    /// its squad (`m_Squad`), whether it can flee (`m_bCapableOfFleeing`), the volumes it keeps
    /// within (`m_TetherVolumes`: `aiworld`)
    pub squad: String,
    pub can_flee: bool,
    pub tether: Vec<u32>,
    /// the flee point it runs to (`aiworld::AiWorld::flee_target`)
    pub flee_to: Option<usize>,
    /// a comrade set upon by rats whose call for help it answers, and the rat on it to stamp
    /// on (`DisBehaviorCombatRatSwarm`)
    pub rat_help: Option<(Entity, Vec3)>,
    /// a live explosive it makes off from (`DisBehaviorEscapeExplosion`): where it is, and how
    /// long it keeps away yet
    pub escape: Option<(Vec3, f32)>,
    /// the danger zones' push on it as its brain steers it (`danger`): what bends its way,
    /// and its eased push and fade before that
    pub danger: Vec3,
    pub danger_smooth: Vec3,
    pub danger_fade: f32,
    /// the avoidance's push on it (`avoid`): what else bends its way, and its eased radius (m:
    /// below 0 before its first) and force
    pub avoid: Vec3,
    pub avoid_r: f32,
    pub avoid_f: f32,
    /// its speed mode a run (`npc_move`)
    pub running: bool,
    /// its stance's locomotion (`LOCO_*`, as `npc_select_anim` last picked it), and how fast it
    /// turns (rad/s, signed: `turn_toward`)
    pub stance: usize,
    pub yaw_v: f32,
    /// out of a plague carrier's cloud it's in (`DisBehaviorEscapePlague`'s `GetOuttaHere`):
    /// where it makes for, and how long yet
    pub plague_escape: Option<(Vec3, f32)>,
    /// robbed while it stood: how long until it misses what was taken (`DisBehaviorStolen`)
    pub robbed: Option<f32>,
    /// caught panicking with Corvo upon it: how long it has begged, and how long he has been
    /// back off; then the seconds left getting up again
    pub caught: Option<(f32, f32)>,
    pub rising: f32,
    /// an investigation it couldn't finish: seconds left standing, and whether it stares up at
    /// where Corvo is out of reach (`StareAtUnreachable`)
    pub hold: Option<(f32, bool)>,
    /// Corvo trespassing in its forbidden zone: it takes him for an enemy (what it thought
    /// before)
    pub trespass: Option<bool>,
    /// lying in wait at an ambush point (`DisSeqAct_AIAmbush`: `AiMarkers::ambush`)
    pub ambush_at: Option<usize>,
    /// protects the neutral (`m_bProtectNeutrals`): the scripts' word, else its brain's
    pub protect_neutrals: Option<bool>,
    pub protects_by_default: bool,
    /// the scripts' flags (`DisSeqAct_NPCIgnoreRBDamages`, `..NPCDisableTeleportOnNavmesh`)
    pub ignore_rb_damage: bool,
    pub no_navmesh_teleport: bool,
    /// senses the scripts took (`DisSeqAct_AISetSenses`): it sees nothing / hears nothing /
    /// feels nothing
    pub blind: bool,
    pub deaf: bool,
    /// the alarm bell it runs to ring (`security::alarms`)
    pub alarm_run: Option<Vec3>,
    /// the attachments it lost (armour knocked off: their indices)
    pub bare: Vec<u8>,
    /// its fight's pacing (`DisAISubProcessManageAttacks`): its aggression (0..1, rolled as it
    /// starts: below 0 not yet), the blows left of its flurry and the time before the next
    pub aggression: f32,
    pub flurry: i32,
    pub stamina: f32,
    pub numb: bool,
    /// the scripts keep it alive (`DisSeqAct_LimitPawnMinHealth`)
    pub min_health: f32,
    /// its path over the navigation mesh
    pub nav: crate::navmesh::NavState,
    /// the scripts have it follow someone (a spawner's character; `u32::MAX` Corvo)
    pub follow: Option<u32>,
    /// the scripts let it always know where Corvo is (`DisSeqAct_AIPsychicAttention`)
    pub psychic: bool,
    /// its type (`scene.npc_types`): its sight and attention tweaks
    pub npc_type: Option<u32>,
    /// the original's attention level (0 unaware, 1 head track, 2 turn to face, 3 investigate,
    /// 4 busted) and how far towards busted it is
    pub attn_level: u8,
    pub attn_fill: f32,
    /// noises of Corvo's it has heard (the scripts' `DisSeqEvent_PlayerHeard`)
    pub heard_player: u32,
    /// the scripts' brain flags (`DisSeqAct_AISetBrainFlags`, bits of `EDisAIBrainFlags`)
    pub brain_flags: u8,
    /// dead, its body turns to ash (`DisSeqAct_BodyShadowKill`: Daud's Whalers)
    pub ash_on_death: bool,
    /// bent time passes it by (its pawn's `m_bAlwaysOutOfBendTime`: the trials' Daud)
    pub out_of_bend: bool,
}

/// `EDisAIBrainFlags`: don't attack at all, no melee, no ranged attacks, (panicking: never
/// despawn), civilians don't panic, Overseers throw no grenades.
pub const BRAIN_DONT_ATTACK: u8 = 1 << 0;
pub const BRAIN_NO_PRIMARY: u8 = 1 << 1;
pub const BRAIN_NO_RANGED: u8 = 1 << 2;
pub const BRAIN_NO_CIVILIAN_PANIC: u8 = 1 << 4;

/// How a character fights, for the difficulty: its type's attributes and weapons
/// (`NpcStats`), in the original's units.
#[derive(Clone, Debug)]
pub struct Arms {
    /// a blow of its blade (`m_MeleeDamage`), a shot (`m_RangedDamage`, or its projectile's own)
    pub melee: f32,
    pub ranged: f32,
    /// its chance to hit with a shot, close and at the end of its reach (`m_MaxAccuracy`,
    /// `m_MinAccuracy`)
    pub accuracy: [f32; 2],
    /// parrying Corvo's blows: the chance of the first, of each next, and how many in a row
    /// (`m_ParryChanceOfStarting`, `m_ParryChanceOfChaining`, `m_ParryChainsAllowed`)
    pub parry: [f32; 3],
    /// a parry's chance to break its guard, from the first of a chain to the last
    /// (`m_BlockBreakRate_Min`, `_Max`)
    pub block_break: [f32; 2],
    /// regeneration: the delay after a hurt, health a second, the health it comes back to
    /// (`m_HealthRegenInitialDelay`, `m_HealthRegenAmount` every `m_HealthRegenRate` s,
    /// `m_HealthRegenLimit`)
    pub regen: [f32; 3],
    /// the damage of a big hit (`m_HitReact_High_Damage`)
    pub big_hit: f32,
    /// how readily it dodges (`m_DodgeLevel`)
    pub dodge: f32,
    /// the ways it throws grenades
    pub grenades: Vec<dhcook::format::GrenadeThrow>,
    /// its blade's moves
    pub moves: Vec<dhcook::format::MeleeMove>,
    /// a music box's tunes
    pub tunes: Option<dhcook::format::NpcTunes>,
    /// how its firearm or bow shoots (its fire context)
    pub fire: Option<dhcook::format::FireContext>,
    /// when it taunts (its taunt context)
    pub taunt: Option<dhcook::format::FireContext>,
    /// the plague it carries (a weeper's: its cough's cloud, its flies)
    pub plague: Option<dhcook::format::NpcPlague>,
    /// a thug's whiskey bottle (its fire breath, the bottle it throws)
    pub whiskey: Option<dhcook::format::NpcWhiskey>,
    /// a witch's spells (`witch`)
    pub witch: Option<dhcook::format::NpcWitch>,
    /// a whale butcher's saw: its running blade and its spray (`saw`)
    pub saw: Option<dhcook::format::NpcSaw>,
    /// a hound master's whistle for the wolfhounds (`aiworld::summon_hounds`)
    pub summon: Option<dhcook::format::NpcSummon>,
    /// how it paces its fight (its combat behaviour's)
    pub pacing: Option<dhcook::format::NpcPacing>,
    /// how danger zones steer it, and the one its aim lays (`danger`)
    pub danger: dhcook::format::NpcDanger,
    /// how its locomotion turns (`LocoTurn`)
    pub turning: LocoTurn,
    /// what it throws with its bare hand
    pub throwing: Option<dhcook::format::NpcThrowing>,
    /// the moves it uses only from outside the ring of those at Corvo (or with him out of
    /// reach): turned off by its attack pattern, not by its outer one (a thug's bottle thrown)
    pub outer_only: Vec<String>,
    /// where it keeps outside that ring: its outer pattern's reach (m) and flank (degrees)
    pub outer_place: Option<(f32, f32)>,
}

impl Default for Arms {
    fn default() -> Self {
        Arms { melee: 15.0, ranged: 20.0, accuracy: [0.35, 0.1], parry: [0.5, 0.25, 3.0], block_break: [0.3, 0.6], regen: [5.0, 5.0, 15.0], big_hit: 10.0, dodge: 0.0, grenades: Vec::new(), moves: Vec::new(), tunes: None, fire: None, taunt: None, plague: None, whiskey: None, witch: None, saw: None, summon: None, pacing: None, danger: Default::default(), turning: LocoTurn::default(), throwing: None, outer_only: Vec::new(), outer_place: None }
    }
}

/// A blade's move under way: which, a big hit or not, how long it lasts, when it lands, how
/// far it reaches, how fast it lunges at Corvo until then.
#[derive(Clone, Debug)]
pub struct Swing {
    pub kind: MoveKind,
    pub big: bool,
    pub len: f32,
    pub hit: f32,
    pub reach: f32,
    pub lunge: f32,
    /// no sword lock against it (`DisTweaks_MeleeAttack.m_bDisableVersus`: the whalers' coup de
    /// grace)
    pub no_versus: bool,
    /// its later blows (a clip striking twice), after `hit`
    pub more: Vec<f32>,
}

/// A gun's or bow's reach, facing and cooldowns (its `DisTweaks_NPCFire` context; without one,
/// a pistol's, an assassin's wrist bow's).
pub(crate) fn fire_context(npc: &Npc) -> dhcook::format::FireContext {
    npc.arms.fire.clone().unwrap_or(dhcook::format::FireContext {
        range: [2.5, 30.0, 6.5],
        angle: 25.0,
        cooldown: if npc.ranged == 3 { [3.0, 6.0] } else { [10.0, 20.0] },
        cooldown_unreachable: if npc.ranged == 3 { [2.0, 5.0] } else { [1.0, 2.0] },
        abort: None,
    })
}

/// A shot's chance to hit `d` m away: its accuracy, `m_MaxAccuracy` close, falling to
/// `m_MinAccuracy` at the end of its reach (less `miss`, the difficulty's for guns).
pub(crate) fn shot_chance(npc: &Npc, ctx: &dhcook::format::FireContext, d: f32, miss: f32) -> f32 {
    let [near, far, _] = ctx.range;
    let k = ((d - near) / (far - near).max(0.1)).clamp(0.0, 1.0);
    (npc.arms.accuracy[0] + (npc.arms.accuracy[1] - npc.arms.accuracy[0]) * k) * (1.0 - miss)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MoveKind {
    Short,
    /// a blow stepping back from someone too close (`Sword_Attack_StepBackA`)
    StepBack,
    /// a thug's fire breath and its thrown whiskey bottle (`Sword_Bottle_Fire_*`,
    /// `Sword_ThrowBottle`)
    WhiskeyFire,
    ThrowWhiskey,
    /// a thing thrown with the bare hand at Corvo out of reach (`Sword_ThrowRock_*`)
    Throw,
    /// a whaler's coup de grace on Corvo its Attract spell drew in (`Sword_Power_Attract_Attack`)
    Attract,
    /// an assassin's wrist bow shot (`Sword_RangedAttack`), its bolt loosed at the clip's cue
    Ranged,
    /// a witch's scream (`Sword_Attack_Scream`), her magic's volley (`Sword_Power_RangedAttack`:
    /// four, at its `DishonoredNotify_FireProjectile`s) and a tentacle called up
    /// (`Sword_Power_SummonTentacle`)
    Scream,
    Magic,
    Summon,
    Medium,
    Long,
    Bash,
    Jump,
    Left90,
    Right90,
    Left180,
    Right180,
    /// under a table: a stab (`Sword_Attack_UnderTable`), a pistol's shots
    /// (`Sword_Gun_AttackUnderTable`)
    Under,
    UnderGun,
    /// a whale butcher's saw spraying shrapnel at Corvo (`DisDLC06Tweaks_NPCFireSaw`: raised, held
    /// spraying a shot a moment, lowered)
    SawFire,
}

impl MoveKind {
    fn of(kind: &str) -> MoveKind {
        match kind {
            "StepBack" => MoveKind::StepBack,
            "WhiskeyFire" => MoveKind::WhiskeyFire,
            "ThrowWhiskey" => MoveKind::ThrowWhiskey,
            "Throw" => MoveKind::Throw,
            "Medium" => MoveKind::Medium,
            "Long" => MoveKind::Long,
            "Bash" => MoveKind::Bash,
            "Jump" => MoveKind::Jump,
            "Left90" => MoveKind::Left90,
            "Right90" => MoveKind::Right90,
            "Left180" => MoveKind::Left180,
            "Right180" => MoveKind::Right180,
            "SawFire" => MoveKind::SawFire,
            _ => MoveKind::Short,
        }
    }

    /// The side it strikes at (degrees from the facing, clockwise: right positive).
    fn side(self) -> f32 {
        match self {
            MoveKind::Left90 => -90.0,
            MoveKind::Right90 => 90.0,
            MoveKind::Left180 => -180.0,
            MoveKind::Right180 => 180.0,
            _ => 0.0,
        }
    }

    /// A blow (not a dodge, the kick that answers a block, a riposte or a shove).
    fn is_attack(kind: &str) -> bool {
        !matches!(kind, "SideStep" | "BackStep" | "Bash" | "Riposte" | "Push" | "Ambush")
    }
}

/// The weapons characters fight hand to hand with: a sword, an assassin's, a whale butcher's saw.
const BLADES: [&str; 3] = ["DisTweaks_WepSword", "DisTweaks_WepSwordOfAssassin", "DisDLC06Tweaks_WepSaw"];

/// What a character's weapons are: a blade, a firearm or bow (and which: 1 pistol, 2 a
/// tallboy's bow, 3 an assassin's wrist bow).
pub fn weapon_kinds(st: &dhcook::format::NpcStats) -> (bool, u8) {
    let has = |c: &str| st.weapons.iter().any(|w| w.class == c);
    let blade = BLADES.iter().any(|c| has(c));
    let ranged = if has("DisTweaks_WepTallboyBow") {
        2
    } else if has("DisTweaks_WepPistol") {
        1
    } else if has("DisTweaks_WepSwordOfAssassin") {
        3
    } else {
        0
    };
    (blade, ranged)
}

impl Arms {
    pub fn of(st: &dhcook::format::NpcStats, difficulty: u8, seed: u64) -> Arms {
        let d = difficulty.min(3) as usize;
        let mut a = Arms::default();
        let at = |n: &str| st.attributes.get(n).map(|v| v[d]);
        // the blade (a tallboy's bow has its own blows), then the firearm or bow
        let blade = ["DisTweaks_WepSword", "DisTweaks_WepSwordOfAssassin", "DisDLC06Tweaks_WepSaw", "DisTweaks_WepTallboyBow", "DisTweaks_WepWhiskeyBottle"].iter().find_map(|c| st.weapons.iter().find(|w| w.class == *c));
        if let Some(w) = blade {
            a.melee = w.melee[d];
        }
        let gun = ["DisTweaks_WepPistol", "DisTweaks_WepTallboyBow", "DisTweaks_WepSwordOfAssassin", "DisDLC06Tweaks_WepWitchHand", "DisDLC06Tweaks_WepSaw"].iter().find_map(|c| st.weapons.iter().find(|w| w.class == *c));
        if let Some(w) = gun {
            a.ranged = if w.projectile_damage > 0.0 { w.projectile_damage } else { w.ranged[d] * w.projectile_mult };
        }
        if let (Some(lo), Some(hi)) = (at("MinAccuracy"), at("MaxAccuracy")) {
            a.accuracy = [hi, lo];
        }
        a.parry = [at("ParryChanceOfStarting").unwrap_or(a.parry[0]), at("ParryChanceOfChaining").unwrap_or(a.parry[1]), at("ParryChainsAllowed").unwrap_or(a.parry[2])];
        a.block_break = [at("BlockBreakRate_Min").unwrap_or(a.block_break[0]), at("BlockBreakRate_Max").unwrap_or(a.block_break[1])];
        let step = at("HealthRegenRate").unwrap_or(1.0).max(0.01);
        a.regen = [at("HealthRegenInitialDelay").unwrap_or(a.regen[0]), at("HealthRegenAmount").map(|x| x / step).unwrap_or(0.0), at("HealthRegenLimit").unwrap_or(0.0)];
        a.big_hit = st.hit_react[1];
        a.dodge = at("DodgeLevel").unwrap_or(0.0);
        a.grenades = st.grenades.clone();
        a.moves = st.melee.clone();
        a.whiskey = st.whiskey.clone();
        a.witch = st.witch.clone();
        a.saw = st.saw.clone();
        a.summon = st.summon.clone();
        a.pacing = st.pacing.clone();
        a.danger = st.danger.clone();
        a.turning = LocoTurn::of(&st.loco);
        // (its outer pattern's place: the ideal reach of the move it names, a projectile's that of
        // its firearm, its bottle or its throw)
        let drawn = (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) as usize;
        if let Some(shape) = (!st.outer_shapes.is_empty()).then(|| &st.outer_shapes[drawn % st.outer_shapes.len()]) {
            let reach = if shape.ideal == "Projectile" {
                st.melee.iter().find(|m| matches!(m.kind.as_str(), "ThrowWhiskey" | "Throw")).map(|m| m.range[2]).or(st.fire.as_ref().map(|f| f.range[2]))
            } else {
                st.melee.iter().find(|m| m.kind == shape.ideal).map(|m| m.range[2])
            };
            a.outer_place = reach.filter(|r| *r > 1.5).map(|r| (r, shape.flank));
        }
        a.throwing = st.throwing.clone();
        // its attack pattern, one of its combat behaviour's: the moves it may use, and how soon
        // again (a pattern's 0-0 cooldown leaving the move's own, our reading); and its outer
        // one's, for the moves it turns off but the outer one doesn't (used from outside the
        // ring of those at Corvo)
        let draw = |all: &[Vec<dhcook::format::PatternOverride>]| (!all.is_empty()).then(|| all[drawn % all.len()].clone());
        let on = |p: &Option<Vec<dhcook::format::PatternOverride>>, kind: &str| p.as_ref().is_none_or(|p| p.iter().find(|o| o.kind == kind).is_none_or(|o| o.enabled));
        let outer = draw(&st.outer_patterns);
        if let Some(pattern) = draw(&st.attack_patterns) {
            let inner = Some(pattern.clone());
            a.outer_only = a.moves.iter().filter(|m| !on(&inner, &m.kind) && outer.is_some() && on(&outer, &m.kind)).map(|m| m.kind.clone()).collect();
            a.moves.retain(|m| on(&inner, &m.kind) || a.outer_only.contains(&m.kind));
            for m in &mut a.moves {
                if let Some(o) = pattern.iter().find(|o| o.kind == m.kind && o.cooldown[1] > 0.0) {
                    m.cooldown = o.cooldown;
                }
            }
        }
        a.tunes = st.tunes.clone();
        a.fire = st.fire.clone();
        a.taunt = st.taunt.clone();
        a.plague = st.plague.clone();
        a
    }
}

impl Npc {
    /// Whether its blade has a move (`Riposte`, `Push`...).
    pub fn has_move(&self, kind: &str) -> bool {
        self.arms.moves.iter().any(|m| m.kind == kind)
    }

    /// Play a gesture once (1 its parry won, 2 its blow parried, 3 a shove; a sword lock: 4 its
    /// struggle, 5 lost badly, 6 lost, 7 won; 8 a rat stamped on; an ambush: 9 into hiding,
    /// 10 springing out, 11 giving it up).
    pub fn gesture(&mut self, kind: u8) {
        self.act = (kind, self.act.1.wrapping_add(1));
    }

    /// Its chance to parry Corvo's next blow (none once the chain is spent).
    pub fn parry_chance(&self) -> f32 {
        if self.parries == 0 {
            self.arms.parry[0]
        } else if (self.parries as f32) < self.arms.parry[2] {
            self.arms.parry[1]
        } else {
            0.0
        }
    }

    /// Its chance that a parry breaks its guard: rising along the chain.
    pub fn block_break_chance(&self) -> f32 {
        let k = (self.parries as f32 / self.arms.parry[2].max(1.0)).clamp(0.0, 1.0);
        self.arms.block_break[0] + (self.arms.block_break[1] - self.arms.block_break[0]) * k
    }

    /// It may swing at Corvo (the scripts' brain flags).
    pub fn may_melee(&self) -> bool {
        self.brain_flags & (BRAIN_DONT_ATTACK | BRAIN_NO_PRIMARY) == 0
    }
    /// It may shoot at Corvo.
    pub fn may_shoot(&self) -> bool {
        self.brain_flags & (BRAIN_DONT_ATTACK | BRAIN_NO_RANGED) == 0
    }
    /// A civilian it may panic.
    pub fn may_panic(&self) -> bool {
        self.brain_flags & BRAIN_NO_CIVILIAN_PANIC == 0
    }
    pub fn is_down(&self) -> bool {
        matches!(self.mode, Mode::Unconscious | Mode::Dead)
    }
    pub fn hostile(&self) -> bool {
        self.enemy
    }
    /// One of the people, not of those against Corvo by their type (the statistics' civilians:
    /// what it is, however it stands to him now).
    pub fn civilian(&self) -> bool {
        !self.enemy_default
    }
    /// Armed: a sword or a gun, bow or wrist bow.
    pub fn fighter(&self) -> bool {
        self.has_sword || self.ranged > 0
    }
    pub fn forward(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw) * Vec3::NEG_Z
    }
    pub fn set_mode(&mut self, m: Mode) {
        // (a fight's pacing is rolled afresh for the next one)
        if self.mode == Mode::Combat && m != Mode::Combat {
            self.aggression = -1.0;
        }
        if self.mode != m {
            if m == Mode::Dead && std::env::var_os("DH_DEATH_LOG").is_some() {
                info!("death: {} ({}) from {:?} at health {:.1}\n{}", self.name, self.pawn, self.mode, self.health, std::backtrace::Backtrace::force_capture());
            }
            if m == Mode::Investigate {
                self.react = rand::random::<f32>();
            }
            self.mode = m;
            self.mode_timer = 0.0;
            self.stuck = 0.0;
        }
    }
}

/// Visual root (child of the NPC) carrying the skeleton; tilted when the body falls.
#[derive(Component)]
pub struct NpcVisual;

/// A corpse fully consumed by rats or hagfish, distinct from temporary hiding.
#[derive(Component)]
pub struct ConsumedBody;

/// Killed by a drop assassination: from which side (0 front, 1 back, 2 left, 3 right).
#[derive(Component, Clone, Copy)]
pub struct DropKilled(pub u8);

/// The shot that just struck was to the head (the challenges' Headshot).
#[derive(Component, Clone, Copy)]
pub struct HeadHit;

/// The shot that just struck was to a leg: its right (or left).
#[derive(Component, Clone, Copy)]
pub struct LegHit(pub bool);

/// A relaxed walk begun or ended with its clip (`npc_select_anim`), which carries it meanwhile
/// (`npc_move`): its clip, how far through, how long; its pace, linear across `ramp` (s) from
/// `speed[0]` to `speed[1]` (m/s); shown turning into its walk (from, by: radians); and a stop's
/// way, kept (a start goes its path's).
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    pub clip: ClipId,
    pub t: f32,
    pub len: f32,
    pub ramp: [f32; 2],
    pub speed: [f32; 2],
    pub turn: Option<(f32, f32)>,
    pub way: Option<Vec3>,
}

impl Pace {
    fn now(&self) -> f32 {
        let k = ((self.t - self.ramp[0]) / (self.ramp[1] - self.ramp[0]).max(0.05)).clamp(0.0, 1.0);
        self.speed[0] + (self.speed[1] - self.speed[0]) * k
    }
}

/// How a character is thrown off its feet (the original's `StateNPCMasterThrown`, as roshan's
/// r614/blink research decoded it): knocked down by a shot or a blast (`PushToRagdoll` group 0,
/// `Generic_PushToRagdoll_*`), blown away by Wind Blast (group 1, `Generic_WindBlast_*`), or a
/// push past the full ragdoll's threshold (limp at once).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Throw {
    Knock,
    Blast,
    Limp,
}

/// A drop assassination's fall, m (the challenges' fall height scoring).
#[derive(Component, Clone, Copy)]
pub struct DropHeight(pub f32);

/// One NPC's paired move on another (`DisNPCAnimDefinitions`' attacker / victim chains): the
/// fatality (`DisTweaks_NPCFatality`, `DisNPCAnim_NPCFatality`: the frontal impale,
/// `Generic_NpcVsNpc_FatalityStrike` over the victim's `..._FatalityDeath`) or the stomp
/// (`DisTweaks_NPCStomp`, `DisNPCAnim_NPCStomp`: `..._FatalityKick` over `..._FatalityKicked`, a
/// bash for the blow's damage).
#[derive(Message, Clone, Copy)]
pub struct NpcFinisher {
    pub killer: Entity,
    pub victim: Entity,
    pub stomp: bool,
    pub damage: f32,
}

/// The killer's part of a finisher: its clip, and whether it has begun.
#[derive(Component)]
pub struct FinisherClip(pub String, pub bool);

const NPC_FINISHER: (&str, &str) = ("Generic_NpcVsNpc_FatalityStrike", "Generic_NpcVsNpc_FatalityDeath");
const NPC_STOMP: (&str, &str) = ("Generic_NpcVsNpc_FatalityKick", "Generic_NpcVsNpc_FatalityKicked");

/// A finisher between NPCs set up: the victim put where its clip's `anchor_jnt` has the killer
/// (as Corvo's are), each given its clip, the victim struck down.
#[allow(clippy::type_complexity)]
fn npc_finishers(
    mut commands: Commands,
    mut msgs: MessageReader<NpcFinisher>,
    mut npcs: Query<(&mut Npc, &mut Transform, &Animator)>,
    mut hits: MessageWriter<NpcHit>,
) {
    for m in msgs.read() {
        let Ok([(_, kt, ka), (mut vn, mut vt, va)]) = npcs.get_many_mut([m.killer, m.victim]) else { continue };
        let pair = if m.stomp { NPC_STOMP } else { NPC_FINISHER };
        let damage = if m.stomp { m.damage } else { 9999.0 };
        let (Some(_), Some(slave)) = (ka.lib.find(pair.0), va.lib.find(pair.1)) else {
            // (without the clips: the blow lands as any other)
            hits.write(NpcHit { npc: m.victim, damage, kind: HitKind::ByOthers, from: kt.translation });
            continue;
        };
        let at = kt.translation;
        if let Some(an) = va.lib.bone_start(slave, "anchor_jnt") {
            let local = Vec3::new(an.z, 0.0, an.y);
            let to = (at - vt.translation).with_y(0.0);
            if local.length() > 0.05 && to.length() > 0.01 {
                let ang = |v: Vec3| v.x.atan2(v.z);
                let yaw = ang(to) - ang(local);
                vn.yaw = yaw;
                let p = at - Quat::from_rotation_y(yaw) * local;
                vt.translation = Vec3::new(p.x, vt.translation.y, p.z);
                vt.rotation = Quat::from_rotation_y(yaw);
            }
        }
        commands.entity(m.killer).try_insert(FinisherClip(pair.0.to_string(), false));
        if m.stomp {
            commands.entity(m.victim).try_insert(FinisherClip(pair.1.to_string(), false));
        } else {
            commands.entity(m.victim).try_insert(AssassinClip(pair.1.to_string()));
        }
        hits.write(NpcHit { npc: m.victim, damage, kind: HitKind::ByOthers, from: at });
        info!("npc finisher: {} on {}", pair.0, vn.name);
    }
}

/// An assassinated character's own part of the kill (`Generic_Assassination_*_Slave`).
#[derive(Component)]
pub struct AssassinClip(pub String);

#[derive(Default, Clone, Copy)]
struct RigIdx {
    root: Option<usize>,
    spine: [Option<usize>; 3],
    neck: Option<usize>,
    jaw: Option<usize>,
    upper_arm: [Option<usize>; 2],
    lower_arm: [Option<usize>; 2],
    thigh: [Option<usize>; 2],
    calf: [Option<usize>; 2],
}

#[derive(Component)]
pub struct NpcRig {
    joints: Vec<Entity>,
    names: Vec<String>,
    bind_t: Vec<Vec3>,
    bind_world: Vec<Quat>,
    parent: Vec<i32>,
    idx: RigIdx,
}

impl NpcRig {
    /// How many bones.
    pub fn len(&self) -> usize {
        self.joints.len()
    }
    /// A bone's parent, by index.
    pub fn parent_of(&self, i: usize) -> Option<usize> {
        self.parent.get(i).filter(|p| **p >= 0).map(|p| *p as usize)
    }
    /// A bone's name, by index.
    pub fn name_of(&self, i: usize) -> Option<&str> {
        self.names.get(i).map(|s| s.as_str())
    }
    /// A bone's index, by name.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }
    /// The joint of a bone, by index.
    pub fn joint_at(&self, i: usize) -> Option<Entity> {
        self.joints.get(i).copied()
    }
    /// The joint of a bone, by name.
    pub fn joint(&self, name: &str) -> Option<Entity> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name)).and_then(|i| self.joints.get(i).copied())
    }
}

/// Recent player positions, used by NPCs to follow the player around corners.
#[derive(Resource, Default)]
pub struct PlayerTrail {
    pub points: Vec<Vec3>,
}

pub fn npc_groups() -> CollisionGroups {
    CollisionGroups::new(GROUP_NPC, GROUP_WORLD | GROUP_PLAYER | GROUP_NPC | GROUP_PROP)
}

fn display_name(pawn: &str) -> &'static str {
    let p = pawn.to_ascii_lowercase();
    if p.contains("eliteguard") {
        "Elite Guard"
    } else if p.contains("cityguard") {
        "City Watch Guard"
    } else if p.contains("lowerguard") {
        "Prison Guard"
    } else if p.contains("guardcaptain") {
        "Guard Captain"
    } else if p.contains("executioner") {
        "Executioner"
    } else if p.contains("campbell") {
        "High Overseer Campbell"
    } else if p.contains("overseer") {
        "Overseer"
    } else if p.contains("thug") {
        "Thug"
    } else if p.contains("delilah") {
        "Delilah"
    } else if p.contains("witch") {
        "Witch"
    } else if p.contains("regent") {
        "Lord Regent"
    } else if p.contains("hound") {
        "Wolfhound"
    } else if p.contains("daud") {
        "Daud"
    } else if p.contains("assassin") {
        "Assassin"
    } else if p.contains("weeper") {
        "Weeper"
    } else if p.contains("tallboy") {
        "Tallboy"
    } else if p.contains("servant") {
        "Servant"
    } else if p.contains("emily") {
        // (before the Empress: `Pwn_LadyEmily_TowerEmpress`)
        "Lady Emily"
    } else if p.contains("empress") {
        "Empress"
    } else if p.contains("sokolov") {
        "Sokolov"
    } else if p.contains("butcher") {
        // (The Knife of Dunwall's: `DLC06_Pwn_SawButcher`, `_CleaverButcher`)
        "Whale Butcher"
    } else {
        "Citizen"
    }
}

fn kind_of(s: &str) -> Kind {
    match s {
        "guard" | "overseer" => Kind::Guard,
        "thug" => Kind::Thug,
        "story" => Kind::Story,
        "creature" => Kind::Creature,
        _ => Kind::Civilian,
    }
}

/// The materials a character was given in place of its type's (`material_swaps`): (of the
/// head?, the slot's material, its new one). Kept for the pieces it may lose and for the
/// scripts' undressing.
#[derive(Component, Clone, Default)]
pub struct NpcLook(pub Vec<(bool, u32, u32)>);

impl NpcLook {
    /// What a part with this material (of the head?, material) wears instead, if anything.
    pub fn mat<'a>(&self, assets: &'a GameAssets, pm: Option<&(bool, u32)>) -> Option<&'a crate::level::PartMat> {
        let pm = pm?;
        self.0.iter().find(|s| s.0 == pm.0 && s.1 == pm.1).and_then(|s| assets.npc_mat_swaps.get(&s.2))
    }
}

/// A character's spawn roll `k` in [0, 1): the same for the same spawner, so a character keeps
/// its look.
fn spawn_roll(seed: u64, k: u64) -> f32 {
    let mut x = seed.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k.wrapping_add(1).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 31;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 29;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// The first of some options whose running sum of chances passes a roll (the original's pawn
/// rolls: chances not normalized, so past them all, none).
fn first_past<T: Copy>(opts: &[(T, f32)], r: f32) -> Option<T> {
    let mut sum = 0.0;
    opts.iter().find(|(_, c)| {
        sum += c.max(0.0);
        r < sum
    }).map(|(t, _)| *t)
}

/// One of a type's heads (`NpcType::variants`), by their chances (as the original: a roll
/// times their total).
fn pick_variant(scene: &dhcook::format::Scene, tid: u32, seed: u64) -> u32 {
    let Some(t) = scene.npc_types.get(tid as usize).filter(|t| t.variants.len() > 1) else { return tid };
    let total: f32 = t.variants.iter().map(|v| v.1.max(0.0)).sum();
    first_past(&t.variants, spawn_roll(seed, 9000) * total).unwrap_or(tid)
}

/// A character's materials varied: per slot of its body and head, one roll; the option it
/// passes replaces the slot's material. Each (of the head?, the slot's material, its new one).
fn material_swaps(scene: &dhcook::format::Scene, tid: u32, seed: u64) -> Vec<(bool, u32, u32)> {
    let Some(t) = scene.npc_types.get(tid as usize) else { return Vec::new() };
    let mut out = Vec::new();
    for (head, var, slots, k0) in [(false, &t.body_variation, &t.body_slots, 9100), (true, &t.head_variation, &t.head_slots, 9300)] {
        for (slot, opts) in var.iter().enumerate() {
            let Some(m) = first_past(opts, spawn_roll(seed, k0 + slot as u64)).filter(|m| *m != u32::MAX) else { continue };
            if let Some(&orig) = slots.get(slot) {
                out.push((head, orig, m));
            }
        }
    }
    out
}

/// A fight's pacing (`DisAISubProcessManageAttacks`, as roshan's r614/blink research decoded
/// it), each fighter's own (`Arms::pacing`, its combat behaviour's), else the elite guard's:
/// flurries of 2-3 blows (`m_AttacksPerFlurry`), then 4-5 s to recover
/// (`m_StaminaRecoveryDuration`); aggression rolled in [0, 1) as the fight starts, regained at
/// `m_fAggressionRegenRate` 0.1 a second, `m_fAggressionDeltaPerDamage` -0.5 when struck,
/// stretching its moves' cooldowns by `(1 - aggression) x m_fCooldownFactorAtMinAggression` (1).
const FLURRY: [i32; 2] = [2, 3];
const STAMINA_RECOVERY: [f32; 2] = [4.0, 5.0];
const AGGRESSION_REGEN: f32 = 0.1;
const AGGRESSION_PER_DAMAGE: f32 = -0.5;
const COOLDOWN_FACTOR: f32 = 1.0;

/// A fighter's pacing goes on: its aggression and flurry rolled as the fight starts, its
/// aggression regained; between flurries, its stamina recovering.
fn pace_tick(npc: &mut Npc, dt: f32) {
    let flurry = npc.arms.pacing.as_ref().map_or(FLURRY, |p| [p.flurry[0] as i32, p.flurry[1].max(p.flurry[0]) as i32]);
    if npc.aggression < 0.0 {
        npc.aggression = rand::random::<f32>();
        npc.flurry = rand::random_range(flurry[0]..=flurry[1]);
        npc.stamina = 0.0;
    }
    npc.aggression = (npc.aggression + npc.arms.pacing.as_ref().map_or(AGGRESSION_REGEN, |p| p.aggression[1]) * dt).min(1.0);
    if npc.flurry < 1 {
        npc.stamina -= dt;
        if npc.stamina < 0.0 {
            npc.flurry = rand::random_range(flurry[0]..=flurry[1]);
        }
    }
}

/// Whether a fighter has a blow of its flurry left.
fn pace_ready(npc: &Npc) -> bool {
    npc.aggression < 0.0 || npc.flurry >= 1
}

/// What a character wears of its type's attachments: those always worn, and of each group (a
/// hat, a mask, a piece of armour) the group's chance, then one option by weight (the first
/// whose running sum passes the roll, as the original's pawn rolls). The same rolls for the same
/// spawner, so a character keeps its look.
fn worn_attachments(atts: &[dhcook::format::NpcAttachment], seed: u64) -> Vec<bool> {
    let roll = |k: u64| spawn_roll(seed, k);
    let mut worn: Vec<bool> = atts.iter().map(|a| a.group == 0).collect();
    let mut groups: Vec<u8> = atts.iter().map(|a| a.group).filter(|g| *g > 0).collect();
    groups.sort_unstable();
    groups.dedup();
    for g in groups {
        let idx: Vec<usize> = (0..atts.len()).filter(|&i| atts[i].group == g).collect();
        if roll(g as u64 * 2) >= atts[idx[0]].chance {
            continue;
        }
        let total: f32 = idx.iter().map(|&i| atts[i].weight.max(0.0)).sum();
        let mut r = roll(g as u64 * 2 + 1) * total;
        let pick = idx
            .iter()
            .copied()
            .find(|&i| {
                r -= atts[i].weight.max(0.0);
                r < 0.0
            })
            .unwrap_or(idx[idx.len() - 1]);
        worn[pick] = true;
    }
    worn
}

/// Shadow Kill: the body burns away (seconds since death); no corpse remains.
#[derive(Component)]
pub struct Ashes(pub f32);

/// Bodies the scripts marked (`DisSeqAct_BodyShadowKill`) turn to ash as they die, as the
/// Shadow Kill power's do.
fn ash_marked(
    mut commands: Commands,
    q: Query<(Entity, &Npc, &GlobalTransform), Without<Ashes>>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
) {
    for (e, npc, g) in &q {
        if npc.ash_on_death && npc.mode == Mode::Dead && !npc.corpse {
            let pos = g.translation();
            commands.entity(e).try_insert(Ashes(0.0));
            sfx.write(crate::audio::PostEvent::named("Snd_Power_Shadow_Kill", Some(pos)));
            fx.write(crate::particles::SpawnEffect { secs: 2.0, ..crate::particles::SpawnEffect::at("shadow_kill", pos) });
        }
    }
}

/// A wall of light's dead (`StateNPCMasterDead_Electrocuted`, with `DisTweaks_NPCPawn`'s
/// settings). Struck standing, it plays its death (`DisNPCAnim_DeathWallOfLight`:
/// `Generic_Electrified`), lightning on its limbs (`m_ElectrocutedFXs`), drawn at once with
/// `m_pElectrocutedMaterial` (`Electric_Vanish_inst`: black, its lightning crawling over it); from
/// `m_fElectrocutedFadeBegin` (0.5 s) its `m_ElectrocutedParameter` (`O_Opacity`) goes from the
/// material's 1 to `m_fElectrocutedFadeEnd` (0) over `m_fElectrocutedFadeDuration` (1.5 s), the
/// material's noise eating it away, and nothing is left. A body it burns goes the same way, lying
/// as it is. (Our reading of the settings: the fade's begin is a time, its end the parameter's
/// last value; the material's mask leaves nothing below 0.5, so the fade can't start lower.)
/// One set on fire standing (an incendiary bolt's): it burns with its own clip
/// (`DisNPCAnim_Immolate`: `Generic_Immolation`) until nothing is left.
#[derive(Component)]
pub struct Immolated;

#[derive(Component)]
pub struct Electrocuted {
    t: f32,
    /// struck standing (its death clip plays), else a body
    pub standing: bool,
    mat: Option<Handle<crate::ue3mat::Ue3Material>>,
}

impl Electrocuted {
    pub fn new(standing: bool) -> Self {
        Electrocuted { t: 0.0, standing, mat: None }
    }
}

/// `m_fElectrocutedFadeBegin`, `m_fElectrocutedFadeEnd`, `m_fElectrocutedFadeDuration`.
const ELECTROCUTED_FADE: (f32, f32, f32) = (0.5, 0.0, 1.5);
/// `Twk_Pawn_DefaultNPC.m_ElectrocutedFXs`: the bones the lightning plays on, and which
/// (`ps_electrified`, the hands' and feet's `_ArmLeft`...).
const ELECTROCUTED_FXS: [(&str, &str); 12] = [
    ("spine_1_jnt", "electrified"),
    ("spine_3_jnt", "electrified"),
    ("upper_arm_L_jnt", "electrified"),
    ("lower_arm_L_jnt", "electrified_arm_l"),
    ("upper_arm_R_jnt", "electrified"),
    ("lower_arm_R_jnt", "electrified_arm_r"),
    ("upper_leg_L_jnt", "electrified"),
    ("lower_leg_L_jnt", "electrified"),
    ("foot_L_jnt", "electrified_leg_l"),
    ("upper_leg_R_jnt", "electrified"),
    ("lower_leg_R_jnt", "electrified"),
    ("foot_R_jnt", "electrified_leg_r"),
];

#[allow(clippy::type_complexity)]
fn electrocute(
    mut commands: Commands,
    time: Res<Time>,
    assets: Option<Res<GameAssets>>,
    mut materials: ResMut<Assets<crate::ue3mat::Ue3Material>>,
    mut q: Query<(Entity, &mut Electrocuted, &NpcRig, &Npc)>,
    children: Query<&Children>,
    parts: Query<Option<&crate::fxlight::PartPasses>, With<NpcPart>>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
) {
    let dt = time.delta_secs();
    let vanish = assets.as_ref().and_then(|a| a.electrocuted_material.clone());
    let gone = ELECTROCUTED_FADE.0 + ELECTROCUTED_FADE.2;
    for (e, mut el, rig, npc) in &mut q {
        let t0 = el.t;
        el.t += dt;
        if t0 == 0.0 {
            // the lightning on its limbs until it is gone
            for (bone, name) in ELECTROCUTED_FXS {
                if let Some(j) = rig.joint(bone).or_else(|| rig.joint("spine_3_jnt")) {
                    fx.write(crate::particles::SpawnEffect { follow: Some(j), secs: gone, ..crate::particles::SpawnEffect::at(name, Vec3::ZERO) });
                }
            }
            // its parts take the material (each character its own, to fade)
            if let Some(m) = vanish.as_ref().and_then(|(base, _)| materials.get(base).cloned()) {
                let h = materials.add(m);
                for c in children.iter_descendants(e) {
                    if let Ok(passes) = parts.get(c) {
                        crate::fxlight::drop_light_passes(&mut commands, c, passes);
                        commands.entity(c).try_remove::<(MeshMaterial3d<crate::lightmap::WorldMaterial>, MeshMaterial3d<crate::ue3mat::Ue3Material>)>().try_insert(MeshMaterial3d(h.clone()));
                    }
                }
                el.mat = Some(h);
            }
            if std::env::var("DH_NPC_CLIP_LOG").is_ok() {
                info!("npc {} electrocuted ({}{})", npc.name, if el.standing { "standing" } else { "a body" }, if el.mat.is_some() { "" } else { ", no vanishing material" });
            }
        }
        // its parameter from the material's own (1) to the fade's end
        let k = ((el.t - ELECTROCUTED_FADE.0) / ELECTROCUTED_FADE.2).clamp(0.0, 1.0);
        if let (Some(mut m), Some((_, slots))) = (el.mat.as_ref().and_then(|h| materials.get_mut(h)), vanish.as_ref()) {
            let v = 1.0 + (ELECTROCUTED_FADE.1 - 1.0) * k;
            for &s in slots {
                if s < m.params.p.len() {
                    m.params.p[s] = Vec4::splat(v);
                }
            }
        }
        if el.t >= gone {
            // nothing left to find
            commands.entity(e).try_remove::<Electrocuted>().try_insert(Ashes(1.0));
        }
    }
}

fn burn_ashes(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Ashes, &mut Npc, &mut Visibility)>) {
    for (e, mut a, mut npc, mut vis) in &mut q {
        a.0 += time.delta_secs();
        if a.0 > 0.6 && *vis != Visibility::Hidden {
            *vis = Visibility::Hidden;
            // nothing left to find
            npc.corpse = true;
            commands.entity(e).try_remove::<Collider>();
        }
    }
}

/// A weeper's arm grab: the lunge, the hold (Corvo struggles free with
/// `m_NumAttackButtonPressToWin` attacks), the release, or a miss; a wolfhound's, Corvo dead in
/// its teeth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrabPhase {
    Try,
    Hold,
    Release,
    Miss,
    Kill,
}

/// Corvo held by a weeper (or pinned by a wolfhound).
#[derive(Component)]
pub struct Grabbed {
    pub by: Entity,
    pub presses: u32,
    pub hound: bool,
    /// a witch's tentacle has his arm (`tentacle`), and the presses it takes to break free
    pub tentacle: bool,
    pub need: u32,
}

/// `DisTweaks_WeeperGrab`: reach, dodge radius (UE units / 100), presses to break free.
const GRAB_RANGE: f32 = 4.0;
const GRAB_DODGE: f32 = 1.5;
pub const GRAB_PRESSES: u32 = 5;
/// After any weeper's grab, none grabs for this long (s: `DefaultAI.ini`'s
/// `DisGlobalCombatManager.m_fInhibitionTimeWeeperArmGrab`, as roshan's r614/blink research
/// pointed out).
const GRAB_INHIBIT: f32 = 20.0;
/// After one strikes Corvo with a move, no other uses the same on him for this long (s:
/// `DefaultAI.ini`'s `DisGlobalCombatManager.m_fInhibitionTime*Attack`, `..SideStep`,
/// `..BackStep`, as roshan's r614/blink research found them used).
fn inhibition(kind: &str) -> f32 {
    match kind {
        "Short" => 2.0,
        "StepBack" => 1.0,
        "Medium" => 1.0,
        "Long" => 0.8,
        "Jump" => 3.0,
        // (a wolfhound's: `m_fInhibitionTimeWHArmAttack`, `..WHJumpAttack`)
        "WHArmAttack" => 8.0,
        "WHJumpAttack" => 30.0,
        "SideStep" => 6.0,
        "BackStep" => 8.0,
        _ => 0.0,
    }
}

/// A wolfhound's two leaps (`Twk_Inv_WolfhoundBite`'s contexts):
/// - its arm attack (`DisTweaks_WHArmAttack`): from 2-4.5 m (`m_fMin/MaxContextRange`), within
///   35 degrees (`m_fAllowedAngle`) and 0.3 m of height feet to feet, every 8-12 s
///   (`m_ContextCooldown`). It has his arm if he is within `m_ArmGrabRange` 1.5 m as it lands,
///   then the struggle by difficulty (`m_LoopSettings`: presses to win 1/2/3/4, 5 damage every
///   0.75/0.5/0.25/0.125 s; `DisTweaks_Minigame`'s 3 presses, 0.5 s where it says none);
/// - its jump attack (`DisTweaks_WHJumpAttack`): from 5.5-8 m, within 15 degrees and 1 m, every
///   20-30 s, knocking him down for `m_DamageOnKnockDown` 7/10/15/20 by difficulty
///   (`DisDamageType_WolfhoundJumpAttack`), then stepping back.
const HOUND_ARM: (f32, f32) = (2.0, 4.5);
const HOUND_ARM_CD: (f32, f32) = (8.0, 12.0);
const HOUND_HOLD: f32 = 1.5;
const HOUND_LOOP: [(u32, f32); 4] = [(1, 0.75), (2, 0.5), (3, 0.25), (4, 0.125)];
const HOUND_BITE: f32 = 5.0;
const HOUND_JUMP: (f32, f32) = (5.5, 8.0);
const HOUND_JUMP_CD: (f32, f32) = (20.0, 30.0);
const HOUND_KNOCKDOWN: [f32; 4] = [7.0, 10.0, 15.0, 20.0];
/// Corvo down from a wolfhound's jump attack (s: his knockdown clips', `Sword_KnockdownBack`...)
const KNOCKED_DOWN: f32 = 0.7;

/// A weeper's weep behaviour (`TweakBehaviors_Weep_Default` over `DisTweaks_AIBehavior_Weep`'s
/// defaults, as roshan's r614/blink research decoded the behaviour): following, it walks, running
/// only while fewer than `m_iSimultaneousRunnerAllowed` (3) of its kind run (after
/// `m_TimeBeforeRunning`, 0); after its grab it stands weeping `m_fStandStillWeeping_Min`..`Max`
/// (2-5 s), until he is 220 uu off; with him beyond `m_fHomeRadius` (1500 uu) of its home for
/// `m_fLostTimer_Min`..`Max` (1-3 s) it gives up and goes home.
const WEEP_RUNNERS: usize = 3;
const WEEP_STAND: (f32, f32) = (2.0, 5.0);
const WEEP_RESUME: f32 = 2.2;
const WEEP_HOME: f32 = 15.0;
const WEEP_LOST: (f32, f32) = (1.0, 3.0);

/// An ambush's longest wait, and the time before another (s).
const AMBUSH_WAIT: f32 = 8.0;
const AMBUSH_CD: f32 = 12.0;

/// Is this a wolfhound (its leap and pin rather than a weeper's grab)?
fn is_hound(npc: &Npc) -> bool {
    npc.name.contains("Wolfhound") || npc.pawn.to_ascii_lowercase().contains("wolfhound")
}

/// The weeper grab's turns: a lunge that lands (or not), the struggle, the release.
#[allow(clippy::too_many_arguments)]
fn weeper_grab(
    mut commands: Commands,
    time: Res<Time>,
    mut player: Query<(Entity, &Transform, &mut Player, Option<&Grabbed>)>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), Without<Player>>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    (mut stats, settings): (ResMut<crate::gameplay::PlayerStats>, Res<crate::settings::Settings>),
) {
    let Ok((pe, pt, mut p, grabbed)) = player.single_mut() else { return };
    for (e, mut npc, t) in &mut npcs {
        let Some((phase, el)) = npc.grab else { continue };
        let el = el + time.delta_secs();
        npc.grab = Some((phase, el));
        match phase {
            GrabPhase::Try if el >= if npc.grab_len > 0.0 { npc.grab_len } else { 0.6 } => {
                let d = (pt.translation - t.translation).with_y(0.0).length();
                let hound = is_hound(&npc);
                if npc.grab_jump {
                    // a wolfhound's jump attack lands: him in its reach, knocked down (its kill if
                    // that kills him), then it steps back
                    let hit = d < HOUND_HOLD + 0.5 && grabbed.is_none() && !npc.is_down() && !stats.dead;
                    if hit {
                        let dmg = HOUND_KNOCKDOWN[(settings.difficulty as usize).min(3)];
                        crate::gameplay::hurt_player(&mut stats, &mut sfx, dmg);
                        stats.knockdown = (stats.knockdown.0 + 1, crate::combat::side_of(t.translation, pt.translation, p.yaw));
                        stats.knocked = KNOCKED_DOWN;
                    }
                    info!("wolfhound jump attack: {} {} ({d:.2} m)", npc.name, if hit { "knocks Corvo down" } else { "misses" });
                    npc.grab = Some((if hit && stats.dead { GrabPhase::Kill } else if hit { GrabPhase::Release } else { GrabPhase::Miss }, 0.0));
                } else if d < if hound { HOUND_HOLD } else { GRAB_DODGE } && grabbed.is_none() && !npc.is_down() {
                    info!("weeper grab: {} holds Corvo", npc.name);
                    npc.grab = Some((GrabPhase::Hold, 0.0));
                    commands.entity(pe).insert(Grabbed { by: e, presses: 0, hound, tentacle: false, need: 0 });
                    p.locked = true;
                    p.velocity = Vec3::ZERO;
                    sfx.write(crate::audio::PostEvent::named("Snd_P_Clothes_Grab_01_Cue_ak", Some(pt.translation)));
                } else {
                    info!("weeper grab: {} missed ({d:.2} m)", npc.name);
                    npc.grab = Some((GrabPhase::Miss, 0.0));
                }
            }
            GrabPhase::Hold => {
                // Corvo is turned to face it
                let to = (t.translation - pt.translation).with_y(0.0);
                if to.length() > 0.1 {
                    let want = (-to.x).atan2(-to.z);
                    let dy = (want - p.yaw + PI).rem_euclid(TAU) - PI;
                    p.yaw += dy * (8.0 * time.delta_secs()).min(1.0);
                    p.pitch *= 1.0 - (6.0 * time.delta_secs()).min(1.0);
                }
                let hound = is_hound(&npc);
                // the wolfhound's teeth while it pins him
                let (presses, tick) = HOUND_LOOP[(settings.difficulty as usize).min(3)];
                if hound && grabbed.is_some_and(|g| g.by == e) && (el / tick).floor() > ((el - time.delta_secs()) / tick).floor() {
                    crate::gameplay::hurt_player(&mut stats, &mut sfx, HOUND_BITE);
                    sfx.write(crate::audio::PostEvent::named("Snd_Wolfhound_Minigame_Bite", Some(t.translation + Vec3::Y * 0.5)));
                }
                let need = if hound { presses } else { GRAB_PRESSES };
                let broke = grabbed.is_none_or(|g| g.by != e || g.presses >= need) || stats.dead;
                if broke || npc.is_down() || npc.stagger > 0.5 {
                    // (a wolfhound with Corvo dead in its teeth: its kill)
                    let kill = hound && stats.dead && !npc.is_down();
                    info!("weeper grab: {} {}", npc.name, if kill { "kills Corvo" } else { "lets go" });
                    npc.grab = Some((if kill { GrabPhase::Kill } else { GrabPhase::Release }, 0.0));
                    npc.stagger = npc.stagger.max(1.2);
                    if grabbed.is_some_and(|g| g.by == e) {
                        commands.entity(pe).remove::<Grabbed>();
                        p.locked = false;
                    }
                }
            }
            GrabPhase::Release | GrabPhase::Miss | GrabPhase::Kill if el >= if npc.grab_len > 0.0 { npc.grab_len } else { 0.9 } => {
                npc.grab = None;
                npc.attack_cd = npc.attack_cd.max(3.0);
                // (a weeper stands weeping after its grab)
                if !is_hound(&npc) {
                    npc.weep_stand = WEEP_STAND.0 + rand::random::<f32>() * (WEEP_STAND.1 - WEEP_STAND.0);
                    info!("weeper grab: {} stands weeping {:.1} s", npc.name, npc.weep_stand);
                }
            }
            _ => {}
        }
    }
    // the weeper went away (despawned; a tentacle's hold is its own)
    if let Some(g) = grabbed.filter(|g| !g.tentacle) {
        if npcs.get(g.by).is_err() {
            commands.entity(pe).remove::<Grabbed>();
            p.locked = false;
        }
    }
}

/// A scene has the character look at something (`InterpTrackLookAt`): where, and how much.
#[derive(Component)]
pub struct SceneLook {
    pub target: Vec3,
    pub weight: f32,
}

/// The head turns to what the scene has the character look at, over its animation.
fn scene_head_look(npcs: Query<(&Transform, &Npc, &NpcRig, &SceneLook)>, mut joints: Query<&mut Transform, (Without<Npc>, Without<SceneLook>)>) {
    for (t, npc, rig, look) in &npcs {
        let to = (look.target - (t.translation + Vec3::Y * 0.7)).with_y(0.0);
        if to.length() < 0.1 {
            continue;
        }
        let want = (-to.x).atan2(-to.z);
        let yaw = ((want - npc.yaw + PI).rem_euclid(TAU) - PI).clamp(-1.3, 1.3) * look.weight.clamp(0.0, 1.0);
        // most of it in the neck, the rest in the upper back
        for (bone, share) in [(rig.idx.neck, 0.65), (rig.idx.spine[2], 0.35)] {
            let Some(i) = bone else { continue };
            let Some(&je) = rig.joints.get(i) else { continue };
            let Ok(mut jt) = joints.get_mut(je) else { continue };
            let p = rig.parent[i];
            let pw = if p >= 0 { rig.bind_world[p as usize] } else { Quat::IDENTITY };
            jt.rotation = pw.inverse() * Quat::from_rotation_y(yaw * share) * pw * jt.rotation;
        }
    }
}

/// A character's head turned to what holds its attention, eased in and out: its yaw and pitch
/// from its body, and how far it has turned.
/// (tests) A character made to watch Corvo whatever its attention (`lookhere`).
#[derive(Component)]
pub struct ForceLook;

#[derive(Component, Default)]
pub struct HeadTrack {
    yaw: f32,
    pitch: f32,
    weight: f32,
    /// its look is laid by its look-at grids (`look_grids`), not turned here
    grids: bool,
    /// its torso's share of the look (its pattern's `m_fMaxTorsoInfluence`; all of it watching
    /// Corvo)
    torso: f32,
    /// its look about while nothing holds its eye (`LOOK_PATTERNS`)
    idle: Option<IdleLook>,
    /// till its next glance at Corvo, and how long the glance under way lasts (s)
    gaze: (f32, f32),
}

/// A glance (`TweaksBehavior_Idle_Default`'s `DisTweaks_AISubProcess_FleetingGaze`, the
/// patrol's, search's and its search patrol's alike): how near (m), how far off its facing
/// (degrees), how often and how long (s).
const GAZE_RADIUS: f32 = 5.0;
const GAZE_ANGLE: f32 = 80.0;
const GAZE_INTERVAL: [f32; 2] = [8.0, 15.0];
const GAZE_DURATION: [f32; 2] = [1.5, 3.0];

/// Personal space (`TweaksBehavior_TakePosition_Default`'s and the search patrol's
/// `DisTweaks_AISubProcess_PersonalSpace`): how long its warning runs (`m_fWarnDuration`), and
/// till it warns again (`m_fWarnCooldown`) or shoves again (`m_fPushCooldown`) (s); out of its way
/// beyond the shove's reach (`DisTweaks_NPCPush`'s 250 uu), shoved within its ideal 150 (m).
const SPACE_WARN: f32 = 2.5;
const SPACE_WARN_CD: f32 = 2.5;
const SPACE_PUSH_CD: f32 = 5.0;
const SPACE_LEFT: f32 = 2.5;
const SPACE_PUSH: f32 = 1.5;

/// A walk paused for what it head-tracks behind it (`TweaksBehavior_Patrol_Default`'s
/// `DisTweaks_AISubProcess_HeadTrackPause`): within `m_fRearHalfAngleWidthToPause_Deg` of its
/// back, and on again within `m_fFrontHalfAngleWidthToResume_Deg` of its heading (degrees).
const PAUSE_REAR: f32 = 45.0;
const PAUSE_RESUME: f32 = 90.0;

/// A look-about under way: its pattern, sequence and rule, the rule's time and repeats left,
/// where it goes (yaw left, pitch up, radians) and how fast.
#[derive(Clone, Copy, Debug)]
struct IdleLook {
    pattern: usize,
    seq: usize,
    rule: usize,
    left: f32,
    repeats: u32,
    target: (f32, f32),
    speed: f32,
}

/// One step of a look-about (`ArkComponentLookatConfig`'s `m_ProcPatterns` rule): a target
/// about its centre within its corners (degrees across, right positive, and up; absolute, from
/// the facing, or from where the look is), held `dur` s, `repeat` more times, gone to at `speed`
/// (our reading: the share of the way a 30th of a second).
struct LookRule {
    absolute: bool,
    center: (f32, f32),
    corners: [(f32, f32); 2],
    repeat: (u32, u32),
    dur: (f32, f32),
    speed: (f32, f32),
}

/// A look-about's sequence: its rules in turn, drawn by its weight (`m_fProbability`).
struct LookSeq {
    weight: f32,
    rules: &'static [LookRule],
}

/// The look-about patterns of `DisLookatConfig.DefaultLookatConfig` (as roshan's r614/blink
/// research read the config): a patrol's (the head alone glancing about), a search's empty-handed
/// and sword drawn (head and torso, left and right in turn). The sequences' small movements
/// within them (`m_ParentSeqIdx`) are left out.
struct LookPattern {
    torso: f32,
    seqs: &'static [LookSeq],
}

const LOOK_FORWARD: LookRule = LookRule { absolute: true, center: (0.0, 0.0), corners: [(0.0, 0.0), (0.0, 0.0)], repeat: (0, 0), dur: (10.0, 10.0), speed: (0.2, 0.25) };

const LOOK_PATTERNS: [LookPattern; 3] = [
    // Patrol
    LookPattern {
        torso: 0.0,
        seqs: &[
            LookSeq { weight: 1.0, rules: &[LOOK_FORWARD] },
            LookSeq { weight: 0.9, rules: &[LookRule { absolute: true, center: (0.0, 0.0), corners: [(-32.0, -20.0), (32.0, 5.0)], repeat: (0, 1), dur: (5.0, 6.0), speed: (0.03, 0.1) }] },
            LookSeq { weight: 0.4, rules: &[LookRule { absolute: false, center: (0.0, 0.0), corners: [(-8.0, -5.0), (8.0, 5.0)], repeat: (0, 0), dur: (1.0, 2.0), speed: (0.1, 0.15) }] },
        ],
    },
    // SearchEmpty
    LookPattern {
        torso: 1.0,
        seqs: &[
            LookSeq { weight: 1.0, rules: &[LOOK_FORWARD] },
            LookSeq {
                weight: 1.0,
                rules: &[
                    LookRule { absolute: true, center: (-90.0, 0.0), corners: [(-30.0, -40.0), (30.0, 25.0)], repeat: (0, 0), dur: (2.5, 4.0), speed: (0.05, 0.1) },
                    LookRule { absolute: true, center: (90.0, 0.0), corners: [(-30.0, -40.0), (30.0, 25.0)], repeat: (0, 0), dur: (2.5, 4.0), speed: (0.05, 0.1) },
                ],
            },
        ],
    },
    // SearchSword
    LookPattern {
        torso: 1.0,
        seqs: &[
            LookSeq { weight: 1.0, rules: &[LOOK_FORWARD] },
            LookSeq {
                weight: 1.0,
                rules: &[
                    LookRule { absolute: true, center: (-90.0, 0.0), corners: [(-35.0, -40.0), (35.0, 35.0)], repeat: (0, 0), dur: (3.0, 4.0), speed: (0.05, 0.12) },
                    LookRule { absolute: true, center: (90.0, 0.0), corners: [(-35.0, -40.0), (35.0, 35.0)], repeat: (0, 0), dur: (3.0, 4.0), speed: (0.05, 0.12) },
                ],
            },
        ],
    },
];

/// A look-about's next rule (`rule` of `seq`, or the sequence's first when a new one is drawn),
/// from where the look is.
fn idle_rule(pattern: usize, seq: usize, rule: usize, from: (f32, f32)) -> IdleLook {
    let roll = |r: (f32, f32)| r.0 + rand::random::<f32>() * (r.1 - r.0).max(0.0);
    let p = &LOOK_PATTERNS[pattern];
    let r = &p.seqs[seq].rules[rule];
    let (h, v) = (roll((r.corners[0].0, r.corners[1].0)) + r.center.0, roll((r.corners[0].1, r.corners[1].1)) + r.center.1);
    // (right positive across, as the config has it: the look's yaw is left positive)
    let target = if r.absolute { (-h.to_radians(), v.to_radians()) } else { (from.0 - h.to_radians(), from.1 + v.to_radians()) };
    IdleLook { pattern, seq, rule, left: roll(r.dur), repeats: rand::random_range(r.repeat.0..=r.repeat.1), target, speed: roll(r.speed) }
}

/// A look-about's next sequence, drawn by weight.
fn idle_seq(pattern: usize, from: (f32, f32)) -> IdleLook {
    let seqs = LOOK_PATTERNS[pattern].seqs;
    let total: f32 = seqs.iter().map(|s| s.weight).sum();
    let mut x = rand::random::<f32>() * total;
    let seq = seqs.iter().position(|s| {
        x -= s.weight;
        x < 0.0
    });
    idle_rule(pattern, seq.unwrap_or(0), 0, from)
}

/// The look-at modes of `DisLookatConfig.DefaultLookatConfig` (`ArkComponentLookatConfig`, as
/// roshan's r614/blink research read it): by stance (empty-handed, sword drawn, a search's patrol
/// stance) and gait (idle, walk, run), the torso's and the head's reach right, left, up and down
/// (degrees). Each has its 3x3 grids of additive poses, `ADD_<stance>_AimTorso<gait>_<dir>` and
/// `..AimHead..`; a search runs on its walk's.
const LOOK_MODES: [[([f32; 4], [f32; 4]); 3]; 3] = [
    // EmptyIdle, EmptyWalk, EmptyRun
    [([60.0, -60.0, 35.0, -45.0], [65.0, -65.0, 60.0, -35.0]), ([60.0, -60.0, 35.0, -45.0], [65.0, -65.0, 60.0, -35.0]), ([60.0, -60.0, 35.0, -35.0], [65.0, -65.0, 40.0, -40.0])],
    // SwordIdle, SwordWalk, SwordRun
    [([70.0, -80.0, 45.0, -35.0], [50.0, -90.0, 45.0, -35.0]), ([70.0, -80.0, 45.0, -35.0], [50.0, -90.0, 45.0, -35.0]), ([65.0, -65.0, 45.0, -35.0], [50.0, -50.0, 45.0, -45.0])],
    // SearchIdle, SearchWalk (and run)
    [([60.0, -60.0, 35.0, -35.0], [75.0, -75.0, 50.0, -35.0]), ([60.0, -60.0, 35.0, -35.0], [65.0, -65.0, 45.0, -35.0]), ([60.0, -60.0, 35.0, -35.0], [65.0, -65.0, 45.0, -35.0])],
];
/// The share of a look the head and torso take, across and up (`m_HeadAndTorso_HorizontalPercentage`
/// 90, `..VerticalPercentage` 100).
const LOOK_SHARE: [f32; 2] = [0.9, 1.0];

/// A look's grid weights, the torso's and the head's: its yaw (radians, left of the facing
/// positive) and pitch over a mode's reaches, the torso's share of its reach (`share`) and the
/// head's together (our reading of `Start_Torso` 0, `End_Head` 1: both across the whole look, so
/// each turns its own share; a look of the head alone, the head the whole of it), as 3x3 weights
/// up-left .. down-right.
fn look_weights(yaw: f32, pitch: f32, torso: [f32; 4], head: [f32; 4], share: f32) -> [[f32; 9]; 2] {
    let right = -yaw.to_degrees() * LOOK_SHARE[0];
    let up = pitch.to_degrees() * LOOK_SHARE[1];
    let x = if right >= 0.0 { right / (torso[0] * share + head[0]).max(1.0) } else { right / (torso[1] * share + head[1]).abs().max(1.0) };
    let y = if up >= 0.0 { up / (torso[2] * share + head[2]).max(1.0) } else { up / (torso[3] * share + head[3]).abs().max(1.0) };
    let split = |v: f32| if v < 0.0 { [-v, 1.0 + v, 0.0] } else { [0.0, 1.0 - v, v] };
    let grid = |x: f32, y: f32| -> [f32; 9] {
        let (cols, rows) = (split(x.clamp(-1.0, 1.0)), split(-y.clamp(-1.0, 1.0)));
        std::array::from_fn(|i| rows[i / 3] * cols[i % 3])
    };
    [grid(x * share, y * share), grid(x, y)]
}

/// How far a head turns (radians: `ArkComponentLookatConfig`'s idle look-at modes, the head's
/// 65 degrees left and right with the torso's 60 behind it, 60 up and 35 down, as roshan's
/// r614/blink research read them); beyond the turn it doesn't look.
const LOOK_YAW: f32 = 1.9;
const LOOK_UP: f32 = 1.05;
const LOOK_DOWN: f32 = 0.6;

/// Characters look at Corvo as they first notice him (at the head-tracking level of their
/// attention alone: the original posts its look-at request there and clears it at any other
/// level, as roshan's r614/blink research decoded it; from turning to face him on the whole body
/// turns) and as they speak with him close by: the head takes most of the turn, the upper back
/// the rest, over the body's animation (`ArkComponentLookatConfig`).
#[allow(clippy::type_complexity)]
fn head_track(
    mut commands: Commands,
    time: Res<Time>,
    player: Query<&Transform, With<crate::player::Player>>,
    npcs: Query<(Entity, &Transform, &Npc, &NpcRig, Option<&mut HeadTrack>, (Has<crate::audio::Speaking>, Has<ForceLook>, Has<ScriptedAnim>, Option<&NpcAnim>), Option<&crate::anim::Animator>), (Without<SceneLook>, Without<crate::player::Player>)>,
    mut joints: Query<&mut Transform, (Without<Npc>, Without<crate::player::Player>)>,
) {
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    let eye = pt.translation + Vec3::Y * 0.6;
    let mut npcs = npcs;
    for (e, t, npc, rig, track, (speaking, forced, scripted, st), anim) in &mut npcs {
        let Some(mut track) = track else {
            commands.entity(e).try_insert(HeadTrack::default());
            continue;
        };
        let grids = track.grids;
        // (a rig not posed this frame keeps last frame's pose: nothing laid on it)
        if anim.is_none_or(|a| a.frozen) {
            continue;
        }
        let head = t.translation + Vec3::Y * 0.75;
        let to = eye - head;
        let flat = to.with_y(0.0).length();
        let yaw = ((-to.x).atan2(-to.z) - npc.yaw + PI).rem_euclid(TAU) - PI;
        let near = flat < 6.0;
        // now and then a glance at Corvo, not its enemy, seen close by before it
        // (`DisAISubProcessFleetingGaze`: its top noted proxy within `m_fGazeRadius` 500 uu and
        // `m_fGazeAngleDegrees` 80 of its facing, every `m_GazeInterval` 8-15 s for
        // `m_GazeDuration` 1.5-3 s; the player alone, and the angle either side, our reading)
        track.gaze.0 = (track.gaze.0 - dt).max(0.0);
        track.gaze.1 = (track.gaze.1 - dt).max(0.0);
        let can_gaze = !npc.hostile()
            && npc.sees_player
            && flat < GAZE_RADIUS
            && yaw.abs() < GAZE_ANGLE.to_radians()
            && matches!(npc.mode, Mode::Idle | Mode::Patrol | Mode::Return | Mode::Search)
            && !scripted;
        if !can_gaze {
            track.gaze.1 = 0.0;
        } else if track.gaze.0 <= 0.0 {
            track.gaze = (GAZE_INTERVAL[0] + rand::random::<f32>() * (GAZE_INTERVAL[1] - GAZE_INTERVAL[0]), GAZE_DURATION[0] + rand::random::<f32>() * (GAZE_DURATION[1] - GAZE_DURATION[0]));
        }
        let gazing = track.gaze.1 > 0.0;
        // a clip it plays (the body's, the upper body's or a gesture's) turning its look off
        // (`DisNotify_NPCDisableLookAt`, from its
        // `m_LookAtFilter` down: from the eyes or the head, the whole look; from the torso, the
        // torso's share; our reading of the filter, by the clips that carry it: a victim's and a
        // hit's from the eyes, an attack's from the head, a draw's, a parry's or a taunt's from
        // the torso)
        let look_off = match (anim, st) {
            (Some(a), Some(st)) => [a.current(), a.upper_playing(), a.overlay_playing()]
                .into_iter()
                .flatten()
                .filter_map(|p| st.marks.get(&p.clip).map(|m| (p, m)))
                .flat_map(|(p, m)| m.look_off.iter().filter(move |w| p.t >= w.0 && p.t <= w.0 + w.1).map(|w| w.2))
                .min(),
            _ => None,
        };
        let wants = !npc.is_down()
            && look_off.is_none_or(|f| f >= 2)
            && npc.mode != Mode::Combat
            && flat > 0.3
            && yaw.abs() < LOOK_YAW
            && ((npc.attn_level == 1 && (npc.sees_player || npc.last_seen.is_some())) || (speaking && near) || forced || npc.warn.is_some() || gazing);
        // nothing holding its eye, a patrolling or searching character looks about (its pattern:
        // a patrol's, or a search's with its sword drawn or not), near enough to be seen doing it
        let pattern = match npc.mode {
            // (not in a scene the scripts play: its clips have their own looks)
            _ if wants || !grids || npc.is_down() || flat > 30.0 || speaking || scripted || look_off.is_some_and(|f| f < 2) => None,
            Mode::Patrol | Mode::Idle | Mode::Return => Some(0),
            Mode::Search | Mode::Investigate => Some(if npc.has_sword { 2 } else { 1 }),
            _ => None,
        };
        let rate = 4.0 * dt;
        track.weight = if wants || pattern.is_some() { (track.weight + rate).min(1.0) } else { (track.weight - rate).max(0.0) };
        let torso_off = look_off == Some(2);
        if wants {
            track.idle = None;
            track.torso = if torso_off { (track.torso - rate).max(0.0) } else { (track.torso + rate).min(1.0) };
            let pitch = to.y.atan2(flat).clamp(-LOOK_DOWN, LOOK_UP);
            let k = (8.0 * dt).min(1.0);
            track.yaw += (yaw - track.yaw) * k;
            track.pitch += (pitch - track.pitch) * k;
        } else if let Some(p) = pattern {
            let from = (track.yaw, track.pitch);
            let mut idle = match track.idle {
                Some(i) if i.pattern == p => i,
                _ => idle_seq(p, from),
            };
            idle.left -= dt;
            if idle.left <= 0.0 {
                let rules = LOOK_PATTERNS[p].seqs[idle.seq].rules.len();
                idle = if idle.repeats > 0 {
                    IdleLook { repeats: idle.repeats - 1, ..idle_rule(p, idle.seq, idle.rule, from) }
                } else if idle.rule + 1 < rules {
                    idle_rule(p, idle.seq, idle.rule + 1, from)
                } else {
                    idle_seq(p, from)
                };
            }
            let k = 1.0 - (1.0 - idle.speed.clamp(0.0, 1.0)).powf(dt * 30.0);
            track.yaw += (idle.target.0 - track.yaw) * k;
            track.pitch += (idle.target.1 - track.pitch) * k;
            let to_torso = if torso_off { 0.0 } else { LOOK_PATTERNS[p].torso };
            track.torso += (to_torso - track.torso).clamp(-rate, rate);
            track.idle = Some(idle);
        } else {
            track.idle = None;
        }
        if track.weight <= 0.0 {
            continue;
        }
        if std::env::var("DH_LOOK_LOG").is_ok() {
            info!("look: {} weight {:.2} torso {:.2} yaw {:.0} pitch {:.0} (attention {}, speaking {speaking}, off {look_off:?}, gaze {gazing})", npc.name, track.weight, track.torso, track.yaw.to_degrees(), track.pitch.to_degrees(), npc.attn_level);
        }
        // (laid by its look-at grids, `look_grids`; else turned here)
        if grids {
            continue;
        }
        let w = track.weight * track.weight * (3.0 - 2.0 * track.weight);
        for (bone, share) in [(rig.idx.neck, 0.65), (rig.idx.spine[2], 0.35)] {
            let Some(i) = bone else { continue };
            let Some(&je) = rig.joints.get(i) else { continue };
            let Ok(mut jt) = joints.get_mut(je) else { continue };
            let p = rig.parent[i];
            let pw = if p >= 0 { rig.bind_world[p as usize] } else { Quat::IDENTITY };
            let turn = Quat::from_rotation_y(track.yaw * share * w) * Quat::from_rotation_x(track.pitch * share * w);
            jt.rotation = pw.inverse() * turn * pw * jt.rotation;
        }
    }
}

/// A speaking character's jaw opens with its line's loudness (when its face doesn't play the
/// line's FaceFX animation).
pub fn speaking_jaw(
    mut commands: Commands,
    time: Res<Time>,
    mut npcs: Query<(Entity, &NpcRig, &mut crate::audio::Speaking, Has<crate::facefx::FaceAnim>)>,
    mut joints: Query<&mut Transform, Without<Npc>>,
) {
    for (e, rig, mut sp, face) in &mut npcs {
        sp.t += time.delta_secs();
        let Some(env) = crate::audio::envelope(sp.media) else {
            commands.entity(e).try_remove::<crate::audio::Speaking>();
            continue;
        };
        let f = sp.t * dhcook::audio::ENVELOPE_RATE;
        let i = f as usize;
        if i + 1 >= env.len() {
            commands.entity(e).try_remove::<crate::audio::Speaking>();
            continue;
        }
        let k = f - i as f32;
        let v = (env[i] as f32 * (1.0 - k) + env[i + 1] as f32 * k) / 255.0;
        if face || std::env::var_os("DH_NO_JAW").is_some() {
            continue;
        }
        let Some(ji) = rig.idx.jaw else { continue };
        let Some(&je) = rig.joints.get(ji) else { continue };
        let Ok(mut jt) = joints.get_mut(je) else { continue };
        let p = rig.parent[ji];
        let pw = if p >= 0 { rig.bind_world[p as usize] } else { Quat::IDENTITY };
        // about the head's sideways axis (+Z in the character's space, facing +X)
        jt.rotation = pw.inverse() * Quat::from_rotation_z(-0.3 * v.powf(0.7)) * pw * jt.rotation;
    }
}

/// Level spawner an NPC came from (index into `scene.spawners`).
#[derive(Component)]
pub struct FromSpawner(pub u32);

/// A matinee (scripted scene) drives this NPC's animation and placement; its AI waits.
#[derive(Component)]
pub struct ScriptedAnim;

/// `DH_NPC_JUMP_LOG`: characters that move more than 1.5 m between two looks (debug).
fn log_jumps(q: Query<(Entity, &Npc, &Transform, Has<ScriptedAnim>)>, mut last: Local<std::collections::HashMap<Entity, Vec3>>, time: Res<Time>) {
    if std::env::var("DH_NPC_JUMP_LOG").is_err() {
        return;
    }
    for (e, n, t, scripted) in &q {
        if let Some(p) = last.insert(e, t.translation) {
            if p.distance(t.translation) > 1.5 {
                info!("npc jump: {e} {} {:.2} -> {:.2} mode {:?} scripted {scripted} (t {:.2})", n.name, p, t.translation, n.mode, time.elapsed_secs());
            }
        }
    }
}

/// A scene, a distraction or the scripts playing a character's clips take its upper body
/// back from an ambient idle.
fn scripted_upper(mut q: Query<&mut Animator, (With<ScriptedAnim>, With<Npc>)>) {
    for mut a in &mut q {
        if a.upper_on() {
            a.stop_upper(0.2);
        }
    }
}

/// Characters a scene holds don't go on with their last step (the controller keeps its
/// translation until it is cleared).
fn hold_scripted(mut q: Query<&mut KinematicCharacterController, With<ScriptedAnim>>) {
    for mut k in &mut q {
        if k.translation.is_some() {
            k.translation = None;
        }
    }
}

/// A part of a character's body or head (its index in its type's parts): the scripts may
/// dress it differently (`DisSeqAct_NPCSetMaterials`).
#[derive(Component)]
pub struct NpcPart {
    pub index: u32,
    pub npc_type: u32,
    /// the LOD it belongs to (0 the best; `GORE_LOD` the dismemberment one)
    pub lod: u8,
}

/// `NpcPart::lod` of the dismemberment LOD's parts.
pub const GORE_LOD: u8 = u8::MAX;

/// `PSI_GraphicsPC_ModelDetails`: on Normal a character's lesser LODs take over with distance
/// as UE3 picks them (LOD n once its `DisplayFactor` exceeds the screen radius / 320, the
/// screen radius the bounds' radius at the view's focal length: `SkeletalLODDistanceFactorMultiplier`
/// 1); on High (0), never.
#[allow(clippy::type_complexity)]
fn model_details(
    mut commands: Commands,
    settings: Res<crate::settings::Settings>,
    assets: Option<Res<GameAssets>>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cam: Query<&Projection, With<crate::player::PlayerCamera>>,
    new: Query<(), Added<NpcPart>>,
    mut parts: Query<(Entity, &NpcPart, &mut Visibility)>,
) {
    if !settings.is_changed() && new.is_empty() {
        return;
    }
    let Some(assets) = assets else { return };
    let fov = cam.single().ok().and_then(|p| if let Projection::Perspective(pp) = p { Some(pp.fov) } else { None }).unwrap_or(75f32.to_radians());
    let half_h = window.single().map(|w| w.physical_height() as f32 * 0.5).unwrap_or(540.0);
    let focal = half_h / (fov * 0.5).tan().max(1e-3);
    for (e, p, mut v) in &mut parts {
        if p.lod == GORE_LOD {
            continue;
        }
        let Some(Some(vis)) = assets.npc_types.get(p.npc_type as usize) else { continue };
        if settings.model_details >= 1 || vis.lods.is_empty() {
            commands.entity(e).try_remove::<bevy::camera::visibility::VisibilityRange>();
            if p.lod > 0 && *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
            continue;
        }
        // LOD n from the distance its display factor is reached at, to where the next one's is
        // (`DH_LOD_SCALE` for tests: the distances scaled)
        let scale = std::env::var("DH_LOD_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        let at = |n: usize| focal * vis.radius / (320.0 * vis.lods[n - 1].0) * scale;
        let n = p.lod as usize;
        let start = if n == 0 { 0.0 } else { at(n) };
        let end = if n < vis.lods.len() { at(n + 1) } else { 1.0e6 };
        commands.entity(e).try_insert(bevy::camera::visibility::VisibilityRange { start_margin: start..start, end_margin: end..end, use_aabb: false });
        if std::env::var("DH_LOD_LOG").is_ok() && p.index == 0 {
            info!("model details: {} LOD {n} from {start:.1} to {end:.1} m", vis.name);
        }
        if n > 0 && *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
    }
}

/// A level script asks a spawner to produce its NPC.
#[derive(Message)]
pub struct SpawnRequest(pub u32);

/// An NPC fired at Corvo (or at whom its script names): from where, at what, and whether it
/// missed.
#[derive(Message, Clone, Copy)]
pub struct NpcShot {
    pub from: Vec3,
    pub to: Vec3,
    pub bow: bool,
    pub miss: bool,
    /// a whale butcher's saw's shrapnel (`DLC06_Proj_SawProjectile`: unseen but for its trail)
    pub saw: bool,
}

/// A piece of a saw's shrapnel in flight: where it is going, how fast (m/s).
#[derive(Component)]
pub struct Shrapnel {
    to: Vec3,
    speed: f32,
    /// arrived: a moment before it is gone, its trail fading
    linger: f32,
}

/// The shrapnel flies to its mark (its trail following it), and is gone.
pub(crate) fn fly_shrapnel(mut commands: Commands, time: Res<Time>, tc: Res<crate::gameplay::TimeControl>, mut q: Query<(Entity, &mut Transform, &mut Shrapnel)>) {
    let dt = time.delta_secs() * tc.world_scale();
    for (e, mut t, mut s) in &mut q {
        let left = s.to - t.translation;
        let step = s.speed * dt;
        if left.length() <= step {
            t.translation = s.to;
            s.linger -= dt;
            if s.linger <= 0.0 {
                commands.entity(e).try_despawn();
            }
        } else {
            t.translation += left.normalize_or_zero() * step;
        }
    }
}

/// How far a missed bullet flies on (m).
const MISS_REACH: f32 = 80.0;

/// Gun and bow shots: the sound, and a gun's flash and its bullet's tracer (its pistol's
/// `Twk_Proj_Bullet`, Corvo's own: `m_pBulletTracerFX`). A missed bullet flies on past its mark,
/// to one side, and strikes the world as Corvo's do (its effect, its hole).
#[allow(clippy::too_many_arguments)]
fn npc_shots(
    mut commands: Commands,
    mut shots: MessageReader<NpcShot>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut gizmos: Gizmos,
    mut recent: Local<Vec<(NpcShot, f32)>>,
    time: Res<Time>,
    (level, rapier, surfaces, mut decals): (Option<Res<LevelInfo>>, ReadRapierContext, Query<&crate::footsteps::ColliderSurfaces>, MessageWriter<crate::decals::SpawnDecal>),
) {
    for s in shots.read() {
        // a saw's shrapnel: its spray's sound and sparks, and a piece flying on its trail to its
        // mark, or past it to where it strikes
        if s.saw {
            sfx.write(crate::audio::PostEvent::named("AI_Butcher_Range_Fire", Some(s.from)));
            let dir = (s.to + Vec3::Y * 0.6 - s.from).normalize_or(Vec3::NEG_Z);
            fx.write(crate::particles::SpawnEffect { rot: Quat::from_rotation_arc(Vec3::X, dir), secs: 0.2, ..crate::particles::SpawnEffect::at("saw_sparks", s.from) });
            let mut end = s.to + Vec3::Y * 0.6;
            if s.miss {
                let side = dir.cross(Vec3::Y).normalize_or(Vec3::X) * (rand::random::<f32>() - 0.5) * 2.0;
                let way = (end + side * (0.5 + rand::random::<f32>() * 0.5) + Vec3::Y * (rand::random::<f32>() - 0.5) * 0.8 - s.from).normalize_or(dir);
                end = s.from + way * MISS_REACH;
                if let (Some(level), Ok(ctx)) = (level.as_ref(), rapier.single()) {
                    let world = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
                    if let Some((_, toi, surface, normal)) = crate::footsteps::ray_surface_normal(&ctx, &surfaces, s.from, way, MISS_REACH, world) {
                        end = s.from + way * toi;
                        let struck = crate::impacts::struck(surface);
                        crate::impacts::spawn(&mut fx, &level.scene, crate::impacts::BULLET, struck, end, normal);
                    }
                }
            }
            let speed = 100.0;
            let piece = commands.spawn((Transform::from_translation(s.from), Shrapnel { to: end, speed, linger: 0.3 }, DespawnOnExit(GameState::InGame))).id();
            fx.write(crate::particles::SpawnEffect { follow: Some(piece), secs: s.from.distance(end) / speed + 0.6, ..crate::particles::SpawnEffect::at("saw_shrapnel", s.from) });
            continue;
        }
        let ev = if s.bow { "Snd_AI_TBoy_Bow_Fire" } else { "Snd_Weapon_Gun_Elite_Fire_cue_ak" };
        sfx.write(crate::audio::PostEvent::named(ev, Some(s.from)));
        if s.bow {
            recent.push((*s, 0.06));
            continue;
        }
        let dir = (s.to - s.from).normalize_or(Vec3::NEG_Z);
        fx.write(crate::particles::SpawnEffect { rot: Quat::from_rotation_arc(Vec3::X, dir), secs: 0.5, ..crate::particles::SpawnEffect::at("npc_pistol_muzzle", s.from + dir * 0.3) });
        let mut end = s.to;
        if s.miss {
            // (wide of its mark by half a metre or more, to either side, and a little high or low)
            let side = dir.cross(Vec3::Y).normalize_or(Vec3::X) * if rand::random::<bool>() { 1.0 } else { -1.0 };
            let past = s.to + side * (0.5 + rand::random::<f32>() * 0.7) + Vec3::Y * (rand::random::<f32>() - 0.4) * 0.8;
            let way = (past - s.from).normalize_or(dir);
            end = s.from + way * MISS_REACH;
            if let (Some(level), Ok(ctx)) = (level.as_ref(), rapier.single()) {
                let world = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
                if let Some((_, toi, surface, normal)) = crate::footsteps::ray_surface_normal(&ctx, &surfaces, s.from, way, MISS_REACH, world) {
                    end = s.from + way * toi;
                    let struck = crate::impacts::struck(surface);
                    crate::impacts::spawn(&mut fx, &level.scene, crate::impacts::BULLET, struck, end, normal);
                    crate::impacts::mark(&mut decals, &level.scene, crate::impacts::BULLET, struck, end, normal, way, f32::INFINITY);
                }
            }
        }
        fx.write(crate::particles::SpawnEffect { beam_to: Some(end), rot: Quat::from_rotation_arc(Vec3::X, dir), secs: 0.6, ..crate::particles::SpawnEffect::at("pistol_tracer", s.from + dir * 0.3) });
    }
    for (s, t) in recent.iter_mut() {
        *t -= time.delta_secs();
        gizmos.line(s.from, s.to, if s.bow { Color::srgb(1.0, 0.55, 0.2) } else { Color::srgb(1.0, 0.9, 0.6) });
    }
    recent.retain(|(_, t)| *t > 0.0);
}

/// An NPC appeared from a spawner.
#[derive(Message)]
pub struct NpcSpawned {
    pub spawner: u32,
    pub entity: Entity,
}

/// Spawners that start with the level. The others wait for the level scripts
/// (`DisSeqAct_StartSpawn`), unless `DH_ALL_NPCS` is set.
pub fn spawn_npcs(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<WorldLighting>>,
    mut spawned: MessageWriter<NpcSpawned>,
    (campaign, script): (Res<crate::gameplay::Campaign>, Res<crate::campaign::CampaignScript>),
    settings: Res<crate::settings::Settings>,
) {
    commands.insert_resource(ArrivalTimer(0.0));
    let (Some(level), Some(assets)) = (level, assets) else { return };
    let scene = &level.scene;
    let max = std::env::var("DH_MAX_NPCS").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let all = std::env::var("DH_ALL_NPCS").is_ok();
    // streamed sublevels not loaded keep their characters
    let loaded = crate::campaign::initial_levels(scene, &campaign, &script);
    let mut n = 0;
    for (si, sp) in scene.spawners.iter().enumerate() {
        if n >= max {
            break;
        }
        // (a grave hound that starts as its skull lies there: `gravehound`)
        if !sp.spawn_on_begin_play && !all || sp.start_as_skull || crate::campaign::spawner_unloaded(scene, &loaded, si as u32) {
            continue;
        }
        if let Some(entity) = spawn_npc(&mut commands, &assets, scene, wl.as_deref_mut(), si, settings.difficulty) {
            spawned.write(NpcSpawned { spawner: si as u32, entity });
            n += 1;
        }
    }
    info!("spawned {n} NPCs");
}

/// Spawn requests from the level scripts (a spawner makes one character at a time: another
/// once its last is down, as the challenges' spawn points do wave after wave). What the scripts
/// set on the spawner makes it (`SeqAct_ModifyProperty` `m_pPawnTweaks`: the wave's kind).
#[allow(clippy::too_many_arguments)]
fn spawn_requested(
    mut commands: Commands,
    mut requests: MessageReader<SpawnRequest>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<WorldLighting>>,
    existing: Query<(&FromSpawner, &Npc)>,
    mut spawned: MessageWriter<NpcSpawned>,
    (settings, vm): (Res<crate::settings::Settings>, Option<Res<crate::kismet::Vm>>),
) {
    let (Some(level), Some(assets)) = (level, assets) else {
        requests.clear();
        return;
    };
    let mut done: Vec<u32> = existing.iter().filter(|(_, n)| !n.is_down()).map(|(f, _)| f.0).collect();
    for r in requests.read() {
        // (a sleeping grave hound's skull is woken instead: `gravehound`)
        if done.contains(&r.0) || level.scene.spawners.get(r.0 as usize).is_some_and(|s| s.start_as_skull) {
            continue;
        }
        done.push(r.0);
        // (the pawn the scripts gave the spawner: "DisTweaks_NPCPawn'Pkg.Group.Pwn_X'")
        let tid = vm.as_ref().and_then(|v| v.spawn_props.get(&r.0)).and_then(|p| p.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case("m_pPawnTweaks"))).and_then(|(_, v)| {
            let path = v.split('\'').nth(1).unwrap_or(v);
            level.scene.npc_types.iter().position(|t| t.name.eq_ignore_ascii_case(path)).map(|i| i as u32)
        });
        if let Some(entity) = spawn_npc_as(&mut commands, &assets, &level.scene, wl.as_deref_mut(), r.0 as usize, settings.difficulty, tid) {
            info!("script spawned {}", level.scene.spawners[r.0 as usize].name);
            spawned.write(NpcSpawned { spawner: r.0, entity });
        }
    }
}

/// Spawn the NPC of spawner `si`.
pub fn spawn_npc(commands: &mut Commands, assets: &GameAssets, scene: &dhcook::format::Scene, wl: Option<&mut WorldLighting>, si: usize, difficulty: u8) -> Option<Entity> {
    spawn_npc_as(commands, assets, scene, wl, si, difficulty, None)
}

/// Spawn spawner `si`'s NPC, or one of another type it was given.
pub fn spawn_npc_as(commands: &mut Commands, assets: &GameAssets, scene: &dhcook::format::Scene, mut wl: Option<&mut WorldLighting>, si: usize, difficulty: u8, as_type: Option<u32>) -> Option<Entity> {
    let sp = scene.spawners.get(si)?;
    let tid = as_type.or(sp.npc_type)?;
    // (one of its heads, and its materials varied: the same for the same spawner)
    let tid = pick_variant(scene, tid, si as u64);
    let swaps = material_swaps(scene, tid, si as u64);
    let swapped = |pm: &(bool, u32)| swaps.iter().find(|s| s.0 == pm.0 && s.1 == pm.1).and_then(|s| assets.npc_mat_swaps.get(&s.2));
    let Some(Some(vis)) = assets.npc_types.get(tid as usize) else { return None };
    let kind = kind_of(&vis.kind);
    let pos = Vec3::from(sp.position) + Vec3::Y * (NPC_CENTER + 0.05);
    let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
    // its health and arms: its type's attributes and weapons (types without them: the
    // default NPC's 40, an elite's 50, a tallboy's)
    let stats = scene.npc_types.get(tid as usize).and_then(|t| t.stats.as_ref());
    let cooked_health = stats.and_then(|s| s.attributes.get("HealthMax")).map(|v| v[difficulty.min(3) as usize]).filter(|h| *h > 0.0);
    let health = match kind {
        Kind::Guard if vis.name.contains("Elite") => 50.0,
        _ => 40.0,
    };
    // the original faction decides who fights Corvo (assassins and weepers are not
    // "guards" but are his enemies); types without one fall back to their kind
    // (the spawner may put it in another: `m_pFactionTweakOverride`)
    let ty = scene.npc_types.get(tid as usize);
    let (faction, enemy) = match (&sp.faction, ty) {
        (Some((f, h)), _) => (f.clone(), *h),
        (None, Some(t)) if !t.faction.is_empty() => (t.faction.clone(), t.hostile),
        (None, t) => (t.map(|t| t.faction.clone()).unwrap_or_default(), matches!(kind, Kind::Guard | Kind::Thug)),
    };
    let assassin = faction.contains("Assassin");
    let tallboy = vis.name.to_ascii_lowercase().contains("tallboy");
    let has_sword = (matches!(kind, Kind::Guard | Kind::Thug) || assassin) && !tallboy;
    // the officers carry the elite guard pistol (`Twk_Inv_PistolEliteGuard`), tallboys bows
    // (and Daud's assassins their wrist bow, `Twk_Inv_AssassinHand`)
    let ranged = if tallboy && enemy {
        2
    } else if enemy && (vis.name.contains("EliteGuard") || vis.name.contains("Executioner")) {
        1
    } else if enemy && assassin {
        3
    } else {
        0
    };
    let health = cooked_health.unwrap_or(if tallboy { 80.0 } else { health });
    // (what it carries, where its type says)
    // (what its type's loadout holds, if cooked, even nothing: a weeping thug's bare hands, the
    // Coldridge prisoners'; else its kind's)
    let (has_sword, ranged) = match stats.map(weapon_kinds) {
        Some((blade, gun)) => (blade, if enemy { gun } else { 0 }),
        None => (has_sword, ranged),
    };
    let arms = stats.map(|s| Arms::of(s, difficulty, si as u64)).unwrap_or_default();
    let corpse = crate::level::is_corpse_type(&vis.name);
    // (its spawner sets it out on patrol, `m_bPatrolUponStartup`: it takes up a route as it
    // starts; the others wait for the scripts)
    let patrol = crate::patrol::Patrol { on: sp.patrol && !corpse, dir: 1, ..Default::default() };
    let mode = if corpse {
        Mode::Dead
    } else if patrol.on {
        Mode::Patrol
    } else {
        Mode::Idle
    };
    let npc = Npc {
        spawner: si as u32,
        kind,
        name: display_name(&vis.name).to_string(),
        pawn: vis.name.clone(),
        health: if corpse { 0.0 } else { health },
        max_health: health,
        arms,
        parries: 0,
        hurt_t: 99.0,
        grenade_cd: Vec::new(),
        retreat: None,
        swing: None,
        melee_cd: Vec::new(),
        act: (0, 0),
        riposte: 0.0,
        warn: None,
        warn_cd: 0.0,
        push_cd: 0.0,
        track_pause: None,
        bark_req: (0, ""),
        versus: false,
        ambush: None,
        ambush_cd: 0.0,
        mode,
        alert: Alert::Unaware,
        awareness: 0.0,
        yaw: sp.yaw,
        velocity: Vec3::ZERO,
        grounded: false,
        home: pos,
        home_yaw: sp.yaw,
        patrol,
        wait: rand::random::<f32>() * 2.0,
        target: None,
        last_seen: None,
        sees_player: false,
        mode_timer: 0.0,
        attack_cd: 0.0,
        attack_t: None,
        stagger: 0.0,
        sleep_in: None,
        stuck: 0.0,
        sidestep: 0.0,
        progress_pos: pos,
        anim_phase: rand::random::<f32>() * TAU,
        sync: None,
        foot_sync: 0.0,
        stride: 0.0,
        anim_speed: 0.0,
        hit_react: 0.0,
        down_t: if corpse { 1.0 } else { 0.0 },
        corpse_age: if corpse { 1_000_000.0 } else { 0.0 },
        look: 0.0,
        has_sword,
        ranged,
        shoot_cd: 2.0,
        aim: None,
        shots: 0,
        gun_hold: 0.0,
        under_cd: [0.0; 2],
        taunt_cd: 5.0 + rand::random::<f32>() * 10.0,
        body_check: false,
        unpossessed: false,
        casting: false,
        knock: None,
        flying: false,
        pace: None,
        turning: false,
        wish: Vec3::ZERO,
        path: None,
        halted: false,
        floored: false,
        rise: None,
        leg_hit: None,
        small_hit: false,
        weak_hit: None,
        weak_hits: false,
        dodging: 0.0,
        dodges: false,
        incoming: (0, 0.0),
        ranged_threat: (0.0, -1.0),
        shot_hit: false,
        dusted: false,
        flank: None,
        taunt_hold: 0.0,
        search_run: false,
        witnessed_death: false,
        found_bodies: Vec::new(),
        allies_seen: Vec::new(),
        checking: None,
        distrust: Vec::new(),
        magic_pending: None,
        magic_react: None,
        magic_bark: None,
        carry_bark: None,
        suspecting: false,
        check_cd: 0.0,
        reacting: 0.0,
        scripted_search: false,
        react: 0.0,
        hit_by_player: false,
        kill_queue: Vec::new(),
        panics: 0,
        panic_fight: 0.0,
        free_flight: 0.0,
        cough_cd: rand::random::<f32>() * 3.0,
        cough_hold: 0.0,
        hit_from: None,
        foe: None,
        feud_scan: rand::random::<f32>(),
        grab: None,
        grab_len: 0.0,
        grab_drift: 0.0,
        grab_jump: false,
        weep_run: false,
        weep_stand: 0.0,
        weep_lost: None,
        leap_cd: [0.0; 2],
        trail_idx: 0,
        saw_body: false,
        corpse,
        gait: [WALK, ALERT_WALK, RUN, RUN],
        voice: scene.npc_types.get(tid as usize).and_then(|t| (!t.voices.is_empty()).then(|| t.voices[rand::random_range(0..t.voices.len())])),
        faction: faction.clone(),
        enemy,
        story_group: if sp.story_group.is_empty() { ty.map(|t| t.story_group.clone()).unwrap_or_default() } else { sp.story_group.clone() },
        enemy_default: enemy,
        squad: sp.squad.clone(),
        can_flee: sp.can_flee,
        tether: sp.tether.clone(),
        flee_to: None,
        caught: None,
        rat_help: None,
        escape: None,
        danger: Vec3::ZERO,
        danger_smooth: Vec3::ZERO,
        danger_fade: 0.0,
        avoid: Vec3::ZERO,
        avoid_r: -1.0,
        avoid_f: 1.0,
        running: false,
        stance: LOCO_EMPTY,
        yaw_v: 0.0,
        plague_escape: None,
        robbed: None,
        rising: 0.0,
        hold: None,
        trespass: None,
        ambush_at: None,
        protect_neutrals: None,
        protects_by_default: ty.and_then(|t| t.sight.as_ref()).is_some_and(|s| s.protect_neutrals),
        ignore_rb_damage: false,
        no_navmesh_teleport: false,
        blind: false,
        deaf: false,
        alarm_run: None,
        bare: Vec::new(),
        aggression: -1.0,
        flurry: 0,
        stamina: 0.0,
        numb: false,
        min_health: 0.0,
        nav: Default::default(),
        follow: None,
        psychic: false,
        npc_type: Some(tid),
        attn_level: 0,
        attn_fill: 0.0,
        heard_player: 0,
        brain_flags: 0,
        ash_on_death: false,
        out_of_bend: ty.is_some_and(|t| t.out_of_bend),
    };

    // skeleton joints
    let bones = &vis.skeleton.bones;
    let mut joints: Vec<Entity> = Vec::with_capacity(bones.len());
    let mut bind_world: Vec<Quat> = Vec::with_capacity(bones.len());
    for b in bones {
        let q = Quat::from_array(b.rotation).normalize();
        // (seen as their character is: what hangs from them, a hat, goes with it when it is
        // hidden)
        let e = commands.spawn((Transform::from_translation(Vec3::from(b.translation)).with_rotation(q), Visibility::default())).id();
        let w = if b.parent >= 0 && (b.parent as usize) < bind_world.len() { bind_world[b.parent as usize] * q } else { q };
        bind_world.push(w);
        joints.push(e);
    }
    let visual = commands.spawn((NpcVisual, Transform::from_rotation(Quat::from_rotation_y(FRAC_PI_2)), Visibility::default())).id();
    for (i, b) in bones.iter().enumerate() {
        let parent = if b.parent >= 0 && (b.parent as usize) < i { joints[b.parent as usize] } else { visual };
        commands.entity(parent).add_child(joints[i]);
    }
    let find = |names: &[&str]| -> Option<usize> {
        names.iter().find_map(|n| bones.iter().position(|b| b.name.eq_ignore_ascii_case(n)))
    };
    let idx = RigIdx {
        root: find(&["Root_jnt"]),
        spine: [find(&["spine_1_jnt"]), find(&["spine_2_jnt"]), find(&["spine_3_jnt"])],
        neck: find(&["neck_jnt"]),
        jaw: find(&["jaw_jnt"]),
        upper_arm: [find(&["upper_arm_L_jnt"]), find(&["upper_arm_R_jnt"])],
        lower_arm: [find(&["lower_arm_L_jnt"]), find(&["lower_arm_R_jnt"])],
        thigh: [find(&["upper_leg_L_jnt"]), find(&["upper_leg_R_jnt"])],
        calf: [find(&["lower_leg_L_jnt"]), find(&["lower_leg_R_jnt"])],
    };
    // weapon sockets of the mesh (bone + offset), with fallbacks for meshes without them
    let socket = |name: &str, fallback: &[&str], default: Transform| -> Option<(Entity, Transform)> {
        match vis.skeleton.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(name)) {
            Some(s) => find(&[s.bone.as_str()])
                .map(|i| (joints[i], Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize()))),
            None => find(fallback).map(|i| (joints[i], default)),
        }
    };
    // (its blade held and put away where its weapon says, `m_Sockets`: a butcher's saw in his left
    // hand, put away nowhere)
    let blade_wep = stats.and_then(|s| BLADES.iter().find_map(|c| s.weapons.iter().find(|w| w.class == *c)));
    let hand_r = blade_wep
        .filter(|w| !w.socket.is_empty() && !w.socket.eq_ignore_ascii_case("RightHandWpn"))
        .and_then(|w| socket(&w.socket, &[], Transform::IDENTITY))
        .or_else(|| socket("RightHandWpn", &["handAttachment_R_jnt", "hand_R_jnt"], Transform::IDENTITY));
    let holster = match blade_wep.map(|w| w.holster.as_str()) {
        Some(h) if !h.is_empty() && !h.eq_ignore_ascii_case("HolsterWpn") => socket(h, &[], Transform::IDENTITY),
        _ => socket("HolsterWpn", &["Holster_jnt"], HOLSTERED),
    };
    for (pi, (mesh, mat)) in vis.parts.parts.iter().enumerate() {
        if skip("visuals") {
            break;
        }
        let mut ec = commands.spawn((
            Mesh3d(mesh.clone()),
            MeshTag(slot),
            SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: joints.clone() },
            DynamicSkinnedMeshBounds,
            crate::fxlight::LitPart,
            NpcPart { index: pi as u32, npc_type: tid, lod: 0 },
        ));
        vis.parts.mats.get(pi).and_then(&swapped).unwrap_or(mat).apply(&mut ec);
        let m = ec.id();
        commands.entity(visual).add_child(m);
    }
    // the lesser LODs, shown by the model details (`model_details`)
    for (li, (_, lp)) in vis.lods.iter().enumerate() {
        if skip("visuals") {
            break;
        }
        for (pi, (mesh, mat)) in lp.parts.iter().enumerate() {
            let mut ec = commands.spawn((
                Mesh3d(mesh.clone()),
                MeshTag(slot),
                SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: joints.clone() },
                DynamicSkinnedMeshBounds,
                crate::fxlight::LitPart,
                NpcPart { index: pi as u32, npc_type: tid, lod: li as u8 + 1 },
                Visibility::Hidden,
            ));
            lp.mats.get(pi).and_then(&swapped).unwrap_or(mat).apply(&mut ec);
            let m = ec.id();
            commands.entity(visual).add_child(m);
        }
    }
    // weapon in the right hand (its blade, what its swings trail)
    let mut swords = Vec::new();
    let mut blade = None;
    let mut saw_edge = None;
    if has_sword {
        let prop = if assassin {
            "assassin_sword"
        } else if vis.name.contains("Overseer") {
            "overseer_sword"
        } else if vis.name.contains("Elite") || vis.name.contains("Executioner") {
            "elite_sword"
        } else if kind == Kind::Thug {
            "thug_sword"
        } else {
            "city_sword"
        };
        let prop = if assets.props.contains_key(prop) { prop } else { "city_sword" };
        // (its own, where cooked: a butcher's cleaver, his saw)
        let prop = blade_wep.map(|w| w.mesh.as_str()).filter(|m| assets.props.contains_key(*m)).unwrap_or(prop);
        blade = crate::trails::blade_of(scene, prop);
        saw_edge = scene.props.iter().find(|p| p.name == prop).and_then(|p| p.edge).map(Vec3::from);
        if let (Some((hand, at)), Some(parts)) = (hand_r, assets.props.get(prop)) {
            for (mesh, mat) in &parts.parts {
                let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), at));
                mat.apply(&mut ec);
                let s = ec.id();
                commands.entity(hand).add_child(s);
                swords.push(s);
            }
        }
    }
    // a musical Overseer's music box, on its `MusicBox` socket (`DisWepMusicBox`): its own rig
    // where the map has it cooked, else the box at rest
    let mut music_rig = None;
    if ty.is_some_and(|t| t.stats.as_ref().is_some_and(|s| s.tunes.is_some())) || ty.is_some_and(|t| t.name.contains("MusicOverseer")) {
        let rig = scene.music_box.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref());
        if let (Some((j, at)), Some(rv)) = (socket("MusicBox", &[], Transform::IDENTITY), rig) {
            music_rig = Some(crate::musicbox::spawn_rig(commands, rv, j, at, slot));
        } else if let (Some((j, at)), Some(parts)) = (socket("MusicBox", &[], Transform::IDENTITY), assets.props.get("musicbox")) {
            for (mesh, mat) in &parts.parts {
                let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), at));
                mat.apply(&mut ec);
                let b = ec.id();
                commands.entity(j).add_child(b);
            }
        }
    }
    // its pistol on the hip, drawn into the left hand to shoot (the officers'
    // `Twk_Inv_PistolEliteGuard`: `Wpn_EliteGun` on `HolsterGun`, then `LeftHandWpn`)
    let gun_hand = socket("LeftHandWpn", &["handAttachment_L_jnt", "hand_L_jnt"], Transform::IDENTITY);
    let gun_holster = socket("HolsterGun", &[], Transform::IDENTITY);
    let mut gun = Vec::new();
    if ranged == 1 {
        if let (Some((j, at)), Some(parts)) = (gun_holster.or(gun_hand), assets.props.get("elite_gun")) {
            for (mesh, mat) in &parts.parts {
                let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), at));
                mat.apply(&mut ec);
                let g = ec.id();
                commands.entity(j).add_child(g);
                gun.push(g);
            }
        }
    }
    // a thug's whiskey bottle on its hip (`Twk_Inv_WhiskeyBottleThug`: `Wpn_ThugBottle` on
    // `HolsterBottle`, drawn into `LeftHandWpn` for its fire breath and its throw)
    let bottle_holster = socket("HolsterBottle", &[], Transform::IDENTITY);
    let mut bottle = Vec::new();
    if stats.is_some_and(|s| s.whiskey.is_some()) {
        if let (Some((j, at)), Some(parts)) = (bottle_holster.or(gun_hand), assets.props.get("thug_bottle")) {
            for (mesh, mat) in &parts.parts {
                let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), at));
                mat.apply(&mut ec);
                let b = ec.id();
                commands.entity(j).add_child(b);
                bottle.push(b);
            }
        }
    }
    // what it carries on its sockets (a tallboy's whale oil tanks and shields); the breakable
    // ones can be struck (a tank, burst by a bullet or a bolt)
    let mut breakable_parts = Vec::new();
    let atts = ty.map(|t| t.attachments.as_slice()).unwrap_or(&[]);
    let worn = worn_attachments(atts, si as u64);
    for (ai, a) in atts.iter().enumerate() {
        if std::env::var("DH_ACC_LOG").is_ok() && a.group > 0 {
            info!("accessory of {}: {} on {} worn {} (socket {}, mesh {})", vis.name, a.prop, a.socket, worn[ai], socket(&a.socket, &[], Transform::IDENTITY).is_some(), assets.props.contains_key(&a.prop));
        }
        if !worn[ai] {
            continue;
        }
        // (armour with nothing of its own to draw, an overseer's mask: still worn)
        let parts = assets.props.get(&a.prop);
        let Some((joint, at)) = socket(&a.socket, &[], Transform::IDENTITY).filter(|_| parts.is_some() || !a.absorb.is_empty()) else { continue };
        let holder = commands.spawn((at, Visibility::default())).id();
        commands.entity(joint).add_child(holder);
        for (mesh, mat) in parts.iter().flat_map(|p| &p.parts) {
            let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), Transform::IDENTITY));
            mat.apply(&mut ec);
            let m = ec.id();
            commands.entity(holder).add_child(m);
        }
        if a.breaks.is_some() || !a.instant.is_empty() {
            commands.entity(holder).insert((Collider::ball(0.3), Sensor, CollisionGroups::new(GROUP_PROP, Group::ALL), crate::gameplay::Strikeable(holder)));
            breakable_parts.push((holder, ai, a.health));
        } else if !a.knock_off.is_empty() || !a.absorb.is_empty() {
            // (armour: knocked off by its damage types, `npcparts::knock_off`; taking those it
            // absorbs, `npc_hits`)
            breakable_parts.push((holder, ai, a.health));
        }
    }
    // original animations
    let mut npc = npc;
    // (its fighting stance's clips by its blade's prefix, `m_ItemPrefix`)
    let prefix = blade_wep.map(|w| w.prefix.as_str()).unwrap_or("");
    let clips = vis.anims.as_ref().and_then(|lib| NpcClips::resolve(lib, prefix).map(|c| (lib.clone(), c)));
    // DH_CLIP_CENSUS: each type's stances as resolved (what it would stand, fight and strike with)
    if std::env::var_os("DH_CLIP_CENSUS").is_some() {
        static SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
        if SEEN.lock().is_ok_and(|mut s| s.get_or_insert_with(std::collections::HashSet::new).insert(vis.name.clone())) {
            let name = |c: Option<ClipId>| match (vis.anims.as_ref(), c) {
                (Some(lib), Some(c)) => lib.clip(c).name.clone(),
                _ => "-".into(),
            };
            match &clips {
                Some((_, c)) => info!(
                    "clip census {}: prefix {prefix:?} blade {has_sword} ranged {ranged} idle {} walk {} alert {} {} run {} attacks {} equip {}",
                    vis.name,
                    name(c.idle),
                    name(c.walk),
                    name(c.idle_alert),
                    name(c.walk_alert),
                    name(c.run),
                    c.attack.len(),
                    name(c.equip)
                ),
                None => info!("clip census {}: no clips (sets {})", vis.name, vis.anims.as_ref().map_or(0, |l| l.set_count())),
            }
        }
    }
    let rig = NpcRig {
        joints: joints.clone(),
        names: bones.iter().map(|b| b.name.clone()).collect(),
        bind_t: bones.iter().map(|b| Vec3::from(b.translation)).collect(),
        bind_world,
        parent: bones.iter().map(|b| b.parent).collect(),
        idx,
    };
    let e = commands
        .spawn((
            rig,
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(sp.yaw)),
            Visibility::default(),
            Collider::capsule_y(NPC_HALF, NPC_RADIUS),
            npc_groups(),
            KinematicCharacterController {
                up: Vec3::Y,
                offset: CharacterLength::Absolute(0.03),
                slide: true,
                autostep: Some(CharacterAutostep {
                    max_height: CharacterLength::Absolute(0.4),
                    min_width: CharacterLength::Absolute(0.1),
                    include_dynamic_bodies: false,
                }),
                max_slope_climb_angle: 50f32.to_radians(),
                min_slope_slide_angle: 60f32.to_radians(),
                snap_to_ground: Some(CharacterLength::Absolute(0.4)),
                filter_groups: Some(npc_groups()),
                // (rapier's pushing of dynamic bodies by a character is unreliable)
                apply_impulse_to_dynamic_bodies: false,
                ..default()
            },
            LitActor { slot, probe_height: 0.4, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
            DespawnOnExit(GameState::InGame),
        ))
        .id();
    if let (Some(&s0), Some(blade)) = (swords.first(), blade) {
        commands.entity(s0).insert(crate::trails::BladeTrail::new(blade, e, false));
    }
    // (a whale butcher's saw: its running blade, reckoned from its edge)
    if let (Some(&s0), true) = (swords.first(), stats.is_some_and(|s| s.saw.is_some())) {
        commands.entity(e).insert(crate::saw::SawBlade::new(s0, saw_edge.unwrap_or(Vec3::new(0.9, -0.4, 0.0))));
    }
    if let Some(r) = music_rig {
        commands.entity(r).insert(crate::musicbox::MusicBoxRig { owner: e });
    }
    for (holder, index, health) in breakable_parts {
        commands.entity(holder).insert(crate::npcparts::NpcAttached { npc: e, npc_type: tid, index, health: health.max(1.0) });
    }
    if !swaps.is_empty() {
        commands.entity(e).insert(NpcLook(swaps.clone()));
    }
    if let Some((lib, clips)) = clips {
        let lib: std::sync::Arc<CharAnims> = lib;
        let speed = |c: Option<ClipId>, lo: f32, hi: f32, def: f32| c.map(|c| lib.root_speed(c)).filter(|v| *v > 0.1).map(|v| v.clamp(lo, hi)).unwrap_or(def);
        // (each gait at its loop's authored speed, as the locomotion component asks its speed
        // mode's, times its modifier's `m_fSpeedMultiplier`: a guard's sword run 7.7 m/s, a
        // wolfhound's 9.9, a panicking civilian's own panic run 5)
        npc.gait = [
            // (a guard's patrol walk at `EmptyPatrol`'s `m_fSpeedMultiplier` 0.92)
            speed(clips.walk, 0.8, 2.0, WALK) * if clips.walk.is_some_and(|c| lib.clip(c).name.eq_ignore_ascii_case("Empty_Walk_Patrol")) { 0.92 } else { 1.0 },
            speed(clips.walk_alert.or(clips.walk), 0.8, 2.6, ALERT_WALK),
            speed(clips.run, 2.5, 10.0, RUN),
            speed(clips.flee.or(clips.run), 2.5, 10.0, RUN),
        ];
        let mut animator = Animator::new(lib.clone(), &vis.skeleton, joints.clone());
        // (its upper-body actions from `spine_0_jnt` up: `NPC_Human_at`'s `UpperBodyFilter`)
        animator.upper_mask = upper_body(&vis.skeleton.bones);
        commands.entity(e).insert((
            animator,
            NpcAnim {
                clips: std::sync::Arc::new(clips),
                last_mode: npc.mode,
                last_alert: npc.alert,
                begging: false,
                down: false,
                attacking: false,
                oneshot: false,
                hit_seen: 0.0,
                // (its behaviour's ambient idles start counting down from a random share of
                // their longest wait)
                idle_time: rand::random::<f32>() * AMBIENT_IDLE[1],
                blink_in: rand::random::<f32>() * BLINK_EVERY[1],
                settle: None,
                drawn: true,
                sword: swords.clone(),
                hand: hand_r,
                holster,
                grab_seen: None,
                act_seen: 0,
                escaping: false,
                loco_v: Vec2::ZERO,
                marks: std::sync::Arc::new(lib.marks(scene)),
                blade,
                gun,
                gun_hand,
                gun_holster,
                shown_yaw: f32::NAN,
                turn: None,
                swarm_t: f32::INFINITY,
                swarm_phase: 0,
                unpossess_next: None,
                dust_phase: 0,
                attract_phase: 0,
                bow_drawn: false,
                holster_in: None,
                limp_in: None,
                next_clip: None,
                lost_pending: false,
                flinch: None,
                faces: std::collections::HashMap::new(),
                walk_phase: f32::NAN,
                counter: None,
                gun_phase: GunPhase::Away,
                gun_t: 0.0,
                gun_out: false,
                shots_seen: 0,
                bottle,
                bottle_hand: gun_hand,
                bottle_holster,
                bottle_stage: None,
            saw_stage: None,
                ragdoll: ty.and_then(|t| t.ragdoll).and_then(|r| scene.ragdolls.get(r as usize)).is_some_and(|r| r.bodies.iter().filter(|b| bones.iter().any(|x| x.name.eq_ignore_ascii_case(&b.bone))).count() >= 2),
            },
        ));
    }
    commands.entity(e).insert(npc);
    if corpse {
        // lying on the floor: no controller, nothing to bump into
        commands.entity(e).remove::<KinematicCharacterController>().insert(CollisionGroups::new(Group::NONE, Group::NONE));
    }
    commands.entity(e).add_child(visual);
    commands.entity(e).insert(FromSpawner(si as u32));
    Some(e)
}

fn record_trail(mut trail: ResMut<PlayerTrail>, player: Query<&Transform, With<Player>>) {
    let Ok(t) = player.single() else {
        trail.points.clear();
        return;
    };
    let p = t.translation;
    if trail.points.last().map(|l| l.distance(p) > 0.8).unwrap_or(true) {
        trail.points.push(p);
        if trail.points.len() > 80 {
            trail.points.remove(0);
        }
    }
}

/// A blade in flesh: the blood spurting along the blow, held to the body
/// (`m_bAttachToStruckActor`, `DCO_LINE_BETWEEN_ACTORS`), and by chance on the lens.
pub fn blade_blood(fx: &mut MessageWriter<crate::particles::SpawnEffect>, vm: Option<&mut crate::kismet::Vm>, b: &dhcook::format::BladeBlood, victim: Entity, pos: Vec3, from: Vec3) {
    let dir = (pos - from).with_y(0.0).normalize_or(Vec3::X);
    fx.write(crate::particles::SpawnEffect { follow: Some(victim), system: Some(b.system), rot: Quat::from_rotation_arc(Vec3::X, dir), secs: 1.5, ..crate::particles::SpawnEffect::at("", Vec3::Y * 1.3) });
    if let (Some((lens, near, chance)), Some(vm)) = (b.lens, vm) {
        if from.distance(pos) < near && rand::random::<f32>() < chance {
            vm.lens.push((crate::hudfx::GAMEPLAY_LENS + 0x2ff, Some((lens, false, 0.0))));
        }
    }
}

/// Apply damage / takedowns from the player.
fn npc_hits(
    mut hits: MessageReader<NpcHit>,
    mut staggers: MessageReader<NpcStagger>,
    mut noise: MessageWriter<Noise>,
    mut stats: ResMut<PlayerStats>,
    mut npcs: Query<(Entity, &mut Npc, &Transform)>,
    mut commands: Commands,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut stances: ResMut<crate::script_world::SpawnerOverrides>,
    (level, mut vm): (Option<Res<LevelInfo>>, Option<ResMut<crate::kismet::Vm>>),
    (mut takedowns, marks, tc): (MessageWriter<crate::gameplay::PlayerTakedown>, Query<(Option<&HeadHit>, Option<&DropHeight>, Option<&LegHit>)>, Res<crate::gameplay::TimeControl>),
    (data, mut knocks, attrs, parts): (Res<crate::gamedata::Data>, MessageWriter<crate::npcparts::KnockOff>, Res<crate::gamedata::Attrs>, Query<&crate::npcparts::NpcAttached>),
) {
    // how much sooner a sleep dart takes hold (`SleepDelayReduction`: the Combat Sleep Darts
    // upgrade)
    let sleep_cut: f32 = data.0.upgrades.iter().filter(|u| stats.upgrades.contains(&u.id)).flat_map(|u| &u.attributes).filter(|(n, _)| n == "SleepDelayReduction").map(|(_, v)| *v).sum();
    for h in hits.read() {
        let (head, drop, leg) = marks.get(h.npc).map(|(h, d, l)| (h.is_some(), d.map(|d| d.0), l.map(|l| l.0))).unwrap_or((false, None, None));
        if head {
            commands.entity(h.npc).try_remove::<HeadHit>();
        }
        if leg.is_some() {
            commands.entity(h.npc).try_remove::<LegHit>();
        }
        let Ok((e, mut npc, t)) = npcs.get_mut(h.npc) else { continue };
        npc.hit_by_player = !matches!(h.kind, HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::Rats | HitKind::EnemyWallOfLight);
        npc.leg_hit = leg;
        npc.small_hit = h.kind == HitKind::Impact;
        npc.shot_hit = h.kind == HitKind::Bolt;
        if npc.is_down() {
            // the knocked out can still be killed where they lie (a blade, a bolt, a shot, a
            // blast, fire, the rats eating them): a kill, no longer a knockout
            let lethal = matches!(
                h.kind,
                HitKind::Sword | HitKind::Bolt | HitKind::Bullet | HitKind::ExplosiveBullet | HitKind::Explosion | HitKind::GrenadeThrowback | HitKind::StickyGrenade | HitKind::ExplosiveBolt | HitKind::SpringRazor | HitKind::Fire | HitKind::Rats | HitKind::ArcMine
            );
            if npc.mode == Mode::Unconscious && lethal && h.damage > 0.0 && npc.min_health <= 0.0 {
                npc.health = 0.0;
                npc.set_mode(Mode::Dead);
                stats.knockouts = stats.knockouts.saturating_sub(1);
                stats.killed(&npc.pawn, npc.civilian());
                if h.kind == HitKind::Fire {
                    commands.entity(e).try_insert(Ashes(-0.9));
                }
            }
            continue;
        }
        // (the scripts may spare someone physics' blows: `DisSeqAct_NPCIgnoreRBDamages`)
        if h.kind == HitKind::Impact && npc.ignore_rb_damage {
            continue;
        }
        if std::env::var("DH_HIT_LOG").is_ok() {
            info!("hit {} ({}): {:?} {:.0} at health {:.0}", npc.name, npc.pawn, h.kind, h.damage, npc.health);
        }
        npc.hit_from = Some(h.from);
        // (its armour may come off: a shot or a bolt to the head, Wind Blast, a flare)
        knocks.write(crate::npcparts::KnockOff { npc: e, kind: h.kind, head, from: h.from });
        // its armour takes the blow in its place (`DisTweaks_Armor`'s `m_DamageTypesToAbsorb`,
        // on its region, from its side: an overseer's mask a bolt, a dart or a bullet in the
        // face, a music box anything to the chest from the front; U-500 in roshan's r614/blink
        // research): no harm, no sleep, but it knows it was struck
        let kind = crate::worlddamage::hit_type(h.kind, h.from.distance(t.translation), &attrs);
        let front = (h.from - t.translation).with_y(0.0).dot(npc.forward()) > 0.0;
        let worn = level.as_ref().and_then(|l| npc.npc_type.and_then(|i| l.scene.npc_types.get(i as usize)));
        let absorbed = worn.is_some_and(|ty| {
            parts.iter().filter(|p| p.npc == e).filter_map(|p| ty.attachments.get(p.index)).any(|a| {
                a.absorb_head == head
                    && (a.absorb_side == 0 || (a.absorb_side == 1) == front)
                    && a.absorb.iter().any(|(t, exact)| if *exact { kind.eq_ignore_ascii_case(t) } else { crate::worlddamage::is_a(kind, t) })
            })
        });
        let mut h = *h;
        if absorbed {
            if std::env::var("DH_HIT_LOG").is_ok() {
                info!("hit {}: its armour takes the {:?}", npc.name, h.kind);
            }
            if h.kind == HitKind::SleepDart {
                npc.hit_react = 0.35;
                npc.last_seen = Some(h.from);
                if npc.hostile() && npc.mode != Mode::Combat {
                    npc.alert = Alert::Combat;
                    npc.awareness = 1.0;
                    npc.set_mode(Mode::Combat);
                }
                continue;
            }
            h.damage = 0.0;
        }
        // those who can fight turn on Corvo when he hurts them (the Tower's guards too); any other
        // neutral to him takes him for an enemy once he does it harm (`IncomingDamage`'s
        // hatred, 0x00B23780, decoded for this rewrite)
        let corvo_side = dhcook::format::is_player_faction(&npc.faction) || level.as_ref().and_then(|l| l.scene.allies.get(&npc.faction)).is_some_and(|a| a.iter().any(|f| dhcook::format::is_player_faction(f)));
        if !npc.enemy && (npc.fighter() || h.damage > 0.0 && !corvo_side) && !matches!(h.kind, HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::Rats | HitKind::Assassinate | HitKind::Fatality | HitKind::Choke | HitKind::SleepDart | HitKind::WallOfLight | HitKind::EnemyWallOfLight | HitKind::StunMine) {
            npc.enemy = true;
            stances.wronged(npc.spawner);
        }
        // (struck by Corvo or another: it suspects, its attention reactions' `AttackedBy`)
        if !matches!(h.kind, HitKind::Rats | HitKind::WallOfLight | HitKind::EnemyWallOfLight) {
            npc.suspecting = true;
        }
        // struck, those whose panic has `m_AttackedMelee` / `m_AttackedRanged` (every shipped
        // tweak both or neither) run from whoever struck them, friend or foe (0x00B61390)
        let struck_panic = h.damage > 0.0 && may_flee(&npc) && level.as_ref().is_some_and(|l| panic_rule(Some(&l.scene), &npc, "AttackedMelee") || panic_rule(Some(&l.scene), &npc, "AttackedRanged"));
        let pos = t.translation;
        let unaware = npc.alert != Alert::Combat;
        // kept alive by the scripts: no killing blow takes it
        if npc.min_health > 0.0 && matches!(h.kind, HitKind::Assassinate | HitKind::Fatality | HitKind::Fire | HitKind::WallOfLight | HitKind::EnemyWallOfLight) {
            npc.hit_react = 0.35;
            continue;
        }
        // Blood Thirst: melee builds adrenaline (a blow's damage twice over:
        // `m_fAdrenalineMultOnHit`)
        if stats.power("BloodThirsty") > 0 {
            stats.gain_adrenaline(match h.kind {
                HitKind::Sword => h.damage * 2.0,
                HitKind::Assassinate => 60.0,
                _ => 0.0,
            });
        }
        // (a kill counted by the blow's type and the victim, for the achievements' statistics)
        let kills0 = stats.kills;
        if matches!(h.kind, HitKind::Sword | HitKind::Assassinate) {
            sfx.write(crate::audio::PostEvent::named("Snd_Imp_Sword_on_Body_cue_ak", Some(pos + Vec3::Y)));
            sfx.write(crate::audio::PostEvent::named("Snd_Phys_Flesh_Blood", Some(pos + Vec3::Y)));
        }
        // the blade's blood (the contact system's sword against a body), and now and then on
        // the lens when Corvo is near
        if matches!(h.kind, HitKind::Sword) {
            if let Some(b) = level.as_ref().and_then(|l| l.scene.blade_blood.clone()) {
                blade_blood(&mut fx, vm.as_deref_mut(), &b, e, pos, h.from);
            }
        }
        match h.kind {
            HitKind::Choke => {
                npc.set_mode(Mode::Unconscious);
                stats.knockouts += 1;
            }
            HitKind::SleepDart => {
                // (someone fighting goes under after `m_fTimeBeforeSleep` 1.5 s, unless Corvo
                // has the Combat Sleep Darts: as roshan's r614/blink research decoded it)
                let delay = (1.5 - sleep_cut).max(0.0);
                if npc.mode == Mode::Combat && delay > 0.0 {
                    npc.sleep_in = Some(npc.sleep_in.map_or(delay, |t| t.min(delay)));
                } else {
                    npc.set_mode(Mode::Unconscious);
                }
                stats.knockouts += 1;
                noise.write(Noise { corvo: false, pos, radius: 3.0, kind: NoiseKind::Anomaly });
            }
            // a stun mine's lightning: asleep, falling limp where they stood
            HitKind::StunMine => {
                npc.set_mode(Mode::Unconscious);
                stats.knockouts += 1;
                commands.entity(e).try_insert(crate::ragdoll::GoLimp::now((pos - h.from).with_y(0.0).normalize_or_zero() * 0.5, None));
                noise.write(Noise { corvo: false, pos, radius: 3.0, kind: NoiseKind::Anomaly });
            }
            // chokedust: choking where they stand a while, unharmed; then after whoever threw it
            HitKind::Chokedust => {
                npc.stagger = npc.stagger.max(crate::gadgets::CHOKEDUST_STUN);
                npc.dusted = true;
                npc.attack_t = None;
                npc.hit_react = 0.35;
                npc.last_seen = Some(h.from);
                if npc.hostile() && npc.mode != Mode::Combat {
                    npc.set_mode(Mode::Search);
                }
                sfx.write(crate::audio::PostEvent::named("Snd_DLC06_AI_Cough_Male", Some(pos + Vec3::Y)));
            }
            HitKind::Fire => {
                // `NPCImmolationTime`: they burn, flailing in the flames (`DisNPCAnim_Immolate`),
                // then nothing is left
                npc.health = 0.0;
                npc.set_mode(Mode::Dead);
                stats.killed(&npc.pawn, npc.civilian());
                commands.entity(e).try_insert((Ashes(-0.9), Immolated));
                sfx.write(crate::audio::PostEvent::named("Amb_Fire_Alcohol_Immolation", Some(pos + Vec3::Y)));
                fx.write(crate::particles::SpawnEffect { follow: Some(e), secs: 1.6, ..crate::particles::SpawnEffect::at("immolation", Vec3::ZERO) });
                noise.write(Noise { corvo: false, pos, radius: 14.0, kind: NoiseKind::Danger });
            }
            HitKind::Assassinate | HitKind::Fatality | HitKind::WallOfLight | HitKind::EnemyWallOfLight => {
                npc.health = 0.0;
                npc.set_mode(Mode::Dead);
                // (a wall's kill is his only if he rewired it: the stats credit the player's side,
                // 0x00B73990)
                if h.kind != HitKind::EnemyWallOfLight {
                    stats.killed(&npc.pawn, npc.civilian());
                }
                // (a wall of light electrocutes its dead, who vanish: `DisWallOfLight`'s kill)
                let wall = matches!(h.kind, HitKind::WallOfLight | HitKind::EnemyWallOfLight);
                if wall {
                    commands.entity(e).try_insert(Electrocuted::new(true));
                }
                noise.write(Noise { corvo: !wall, pos, radius: 3.0, kind: NoiseKind::DeathRattle });
            }
            HitKind::ByOthers | HitKind::EnemyExplosion => {
                // another NPC's blow (or the level's rats): it fights back
                npc.health = (npc.health - h.damage).max(npc.min_health);
                npc.hurt_t = 0.0;
                npc.hit_react = 0.35;
                npc.attack_t = None;
                npc.stagger = npc.stagger.max(0.25);
                if npc.health <= 0.0 {
                    npc.set_mode(Mode::Dead);
                } else if struck_panic {
                    npc.last_seen = Some(h.from);
                    npc.set_mode(Mode::Flee);
                }
                noise.write(Noise { corvo: false, pos, radius: 8.0, kind: NoiseKind::Threatening });
            }
            _ => {
                npc.health = (npc.health - h.damage).max(npc.min_health);
                npc.hurt_t = 0.0;
                // Corvo's blade, leaving it standing: its weak reaction, the additive flinch over
                // what it does, nothing stopped (the reaction chooser: no `m_ExpectedHitReaction`
                // on the blade's damage type and `m_bDisableWeakHitReaction` false in every combat
                // tweak, as roshan's r614/blink research decoded it), where its set has the clips
                let weak = h.kind == HitKind::Sword && npc.health > 0.0 && npc.weak_hits;
                if weak {
                    npc.weak_hit = side_struck(&npc, pos).or(Some(0));
                } else {
                    npc.hit_react = 0.35;
                    npc.attack_t = None;
                    // (slid into: its strong reaction, stumbling through it)
                    npc.stagger = npc.stagger.max(if h.kind == HitKind::Slide { 0.9 } else { 0.25 });
                }
                if npc.aggression >= 0.0 && h.damage > 0.0 {
                    npc.aggression = (npc.aggression + npc.arms.pacing.as_ref().map_or(AGGRESSION_PER_DAMAGE, |p| p.aggression[0])).clamp(0.0, 1.0);
                }
                if npc.mode != Mode::Combat && npc.hostile() {
                    npc.alert = Alert::Combat;
                    npc.awareness = 1.0;
                    npc.last_seen = Some(h.from);
                    npc.set_mode(Mode::Combat);
                }
                if npc.health > 0.0 && struck_panic {
                    npc.last_seen = Some(h.from);
                    npc.set_mode(Mode::Flee);
                }
                if npc.health <= 0.0 {
                    npc.set_mode(Mode::Dead);
                    stats.killed(&npc.pawn, npc.civilian());
                    noise.write(Noise { corvo: false, pos, radius: 8.0, kind: NoiseKind::Threatening });
                    // (a death rattle's maker is the killer: Corvo, but for the rats)
                    noise.write(Noise { corvo: h.kind != HitKind::Rats, pos, radius: 3.0, kind: NoiseKind::DeathRattle });
                } else {
                    noise.write(Noise { corvo: false, pos, radius: 8.0, kind: NoiseKind::Threatening });
                }
            }
        }
        // blown off their feet (Wind Blast's push, an explosion's): the body flies limp
        if npc.is_down() {
            let blast = matches!(h.kind, HitKind::Explosion | HitKind::EnemyExplosion | HitKind::GrenadeThrowback | HitKind::StickyGrenade | HitKind::ExplosiveBullet | HitKind::ExplosiveBolt);
            let vel = if h.kind == HitKind::Windblast {
                npc.velocity
            } else if blast {
                let away = pos - h.from;
                let d = away.length();
                away.with_y(0.0).normalize_or_zero() * (7.0 - d).clamp(1.5, 4.5) + Vec3::Y * (4.0 - d * 0.5).clamp(1.0, 3.0)
            } else {
                Vec3::ZERO
            };
            if vel.length() > 2.0 {
                commands.entity(e).try_insert(crate::ragdoll::GoLimp::now(vel, None));
            }
        } else if matches!(h.kind, HitKind::Bullet | HitKind::Explosion | HitKind::EnemyExplosion | HitKind::GrenadeThrowback | HitKind::ExplosiveBullet) && h.damage > 0.0 {
            // a shot or a grenade's blast that leaves it standing knocks it down (the bullet's
            // and the grenade's damage types' reaction, Knockdown: roshan's r614/blink research,
            // seen in the original's `combat/pistol_hit` capture): thrown on the hit's push, at
            // least the throw's least momentum (800 uu: 4 m/s across, away from the hit)
            let away = (pos - h.from).with_y(0.0).normalize_or(-npc.forward());
            let blast = h.kind != HitKind::Bullet;
            let d = pos.distance(h.from);
            let push = if blast { (7.0 - d).clamp(4.0, 6.0) } else { 4.0 };
            let lift = if blast { (4.0 - d * 0.5).clamp(1.0, 3.0) } else { 0.0 };
            npc.knock = Some((away * push + Vec3::Y * lift, Throw::Knock));
        }
        // (Corvo's own takedowns, for the challenges' scoring)
        // (an arc mine's dead are ash: `m_bAshDeadPawns`)
        if h.kind == HitKind::ArcMine && npc.mode == Mode::Dead {
            commands.entity(e).try_insert(Ashes(-0.4));
        }
        let ko = matches!(h.kind, HitKind::Choke | HitKind::SleepDart | HitKind::StunMine) && npc.mode == Mode::Unconscious;
        if (stats.kills > kills0 || ko) && !matches!(h.kind, HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::Rats | HitKind::EnemyWallOfLight) {
            takedowns.write(crate::gameplay::PlayerTakedown {
                npc: h.npc,
                faction: npc.faction.clone(),
                pawn: npc.pawn.clone(),
                story_group: npc.story_group.clone(),
                kind: h.kind,
                lethal: stats.kills > kills0,
                unaware,
                head,
                drop: if h.kind == HitKind::Assassinate { drop } else { None },
                bent: tc.bend_remaining > 0.0 || tc.scripted.is_some(),
                hostile: npc.hostile(),
                at: pos,
            });
        }
        if stats.kills > kills0 {
            if h.kind == HitKind::GrenadeThrowback {
                *stats.counters.entry("ePlayerStat_GrenadeThrowbackKill".into()).or_default() += 1;
            }
            // (`<stat>|<damage type>|<pawn tweak>`: the statistics filter on either)
            let kind = crate::worlddamage::hit_type(h.kind, h.from.distance(pos), &crate::gamedata::Attrs::default());
            *stats.counters.entry(format!("ePlayerStat_NumKills|{kind}|{}", npc.pawn)).or_default() += 1;
            if npc.hostile() {
                *stats.counters.entry(format!("ePlayerStat_HostileKill|{kind}|{}", npc.pawn)).or_default() += 1;
            }
            if unaware {
                *stats.counters.entry("ePlayerStat_UnawareKill".into()).or_default() += 1;
                *stats.counters.entry(format!("ePlayerStat_UnawareKill|{kind}|{}", npc.pawn)).or_default() += 1;
            }
        }
        // Shadow Kill: the unaware dead (every one at level 2) turn to ash
        let shadow = stats.power("ShadowKill");
        if npc.mode == Mode::Dead && shadow > 0 && (unaware || shadow >= 2) && !matches!(h.kind, HitKind::Rats | HitKind::Fire | HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::EnemyWallOfLight) {
            commands.entity(e).try_insert(Ashes(0.0));
            sfx.write(crate::audio::PostEvent::named("Snd_Power_Shadow_Kill", Some(pos)));
            fx.write(crate::particles::SpawnEffect { secs: 2.0, ..crate::particles::SpawnEffect::at("shadow_kill", pos) });
        }
        if npc.is_down() {
            npc.attack_t = None;
            // bodies no longer block movement
            commands.entity(e).try_remove::<KinematicCharacterController>();
            commands.entity(e).try_insert(CollisionGroups::new(Group::NONE, Group::NONE));
        }
    }
    for s in staggers.read() {
        if let Ok((_, mut npc, _)) = npcs.get_mut(s.npc) {
            npc.stagger = npc.stagger.max(s.secs);
            npc.attack_t = None;
            npc.swing = None;
            npc.attack_cd = npc.attack_cd.max(s.secs);
            if s.parried {
                npc.gesture(2);
            }
        }
    }
}

fn npc_hearing(
    mut noises: MessageReader<Noise>,
    mut calls: MessageReader<AllyCall>,
    mut npcs: Query<(&mut Npc, &Transform)>,
    level: Option<Res<LevelInfo>>,
    player: Query<&Transform, (With<Player>, Without<Npc>)>,
    rooms: Res<crate::audiorooms::AudioRooms>,
    rapier: ReadRapierContext,
) {
    let corvo = player.single().ok().map(|t| t.translation);
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let log = std::env::var_os("DH_HEAR_LOG").is_some();
    // (not in one room with it, heard only with a clear line from it past the level's walls, as
    // the noise's dispatch traces outside the rooms, 0x00C5A760 -> 0x00BB50D0, as roshan's
    // r614/blink research found; from a little way out of it, as an impact's noise lies on what
    // was struck)
    let walled = |at: Vec3, from: Vec3| {
        let room = rooms.cell_at(at);
        if room.is_some() && room == rooms.cell_at(from) {
            return false;
        }
        let to = at + Vec3::Y * 0.72 - from;
        let len = to.length();
        len > 0.35 && ctx.as_ref().is_some_and(|c| c.cast_ray(from + to / len * 0.3, to / len, len - 0.3, true, walls).is_some())
    };
    let scene = level.as_ref().map(|l| &l.scene);
    let calls: Vec<AllyCall> = calls.read().cloned().collect();
    // (one of the caller's side, or a neutral it protects, hears a call as one: 0x00B0F0F0)
    let heeds = |npc: &Npc, c: &AllyCall| {
        let rel = relation(scene, &npc.faction, &c.faction);
        rel == 1 || rel == 0 && npc.npc_type.and_then(|t| scene?.npc_types.get(t as usize)).and_then(|t| t.sight.as_ref()).is_some_and(|s| s.protect_neutrals)
    };
    // (a cry for help a line carries is heard as any other, but taken up only by those of its
    // side)
    let mut heard: Vec<(Noise, Option<&AllyCall>)> = noises.read().map(|n| (*n, None)).collect();
    heard.extend(calls.iter().filter(|c| c.kind == CallKind::Help).map(|c| (Noise { corvo: false, pos: c.from, radius: 25.0, kind: NoiseKind::Help }, Some(c))));
    for (n, call) in &heard {
        // (his own noises: where he is)
        let his = corvo.is_some_and(|p| p.distance(n.pos) < 2.0);
        for (mut npc, t) in &mut npcs {
            if npc.is_down() || npc.kind == Kind::Story || matches!(npc.mode, Mode::Choked) {
                continue;
            }
            // (from another room: through the doorways, muffled by each)
            let (d, through) = match rooms.route(t.translation, n.pos, true) {
                Some(r) => (r.dist, r.gain),
                None => (t.translation.distance(n.pos), 1.0),
            };
            if d > n.radius * through || npc.deaf {
                continue;
            }
            if walled(t.translation, n.pos) {
                if log {
                    info!("hearing: {} doesn't hear {:?} {:.1} m off through the walls", npc.name, n.kind, d);
                }
                continue;
            }
            if log {
                info!("hearing: {} hears {:?} {:.1} m off (at {:.1}, Corvo's {})", npc.name, n.kind, d, n.pos, n.corvo);
            }
            if his {
                npc.heard_player += 1;
            }
            // a scary noise sets those whose panic has `m_HearNoise` running (0x00C58560, 0x00B616E0,
            // 0x00B61560, 0x00B615E0, decoded for this rewrite): one its list names
            // (`m_ScaryNoises`); a cry for help or an alarm bell whoever raised it, any other only
            // if its maker is its enemy (Corvo, to those that take him for one)
            let scene = level.as_ref().map(|l| &l.scene);
            if panic_rule(scene, &npc, "HearNoise") && may_flee(&npc) {
                let scary = npc.npc_type.and_then(|t| scene?.npc_types.get(t as usize)).and_then(|t| t.sight.as_ref()).and_then(|s| s.scary.clone());
                let listed = scary.as_deref().map_or(SCARY_NOISES.contains(&n.kind.context()), |l| l.iter().any(|c| c.eq_ignore_ascii_case(n.kind.context())));
                let maker = matches!(n.kind, NoiseKind::Help | NoiseKind::Alarm) || n.corvo && npc.hostile();
                if listed && maker {
                    npc.set_mode(Mode::Flee);
                    npc.last_seen = Some(n.pos);
                    continue;
                }
            }
            if !npc.hostile() || call.is_some_and(|c| !heeds(&npc, c)) {
                continue;
            }
            // what it hears raises its attention (the original's increase for the noise's
            // context: an anomaly, a threat, danger, a death rattle, a cry for help, an alarm),
            // by its amount, at least to its level; investigating, it goes to look. Busted only
            // where the noise's reason may take it there (the tweak's `m_ReasonSettings`
            // `m_bCanIncreaseToBusted`, as roshan's r614/blink research found the increase
            // applied: added, then floored at the level): a guard's for a cry for help
            // (`DACRT_HeardHelpRequest`), not for sounds (`DACRT_Heard`), death rattles
            // (`DACRT_HeardDeath`) or alarms (`DACRT_Alarm`)
            if let Some(att) = level.as_ref().and_then(|l| attention_of(&npc, &l.scene)) {
                if npc.alert == Alert::Combat {
                    continue;
                }
                // (a death heard, unsuspecting, its attention at investigating at most: taken
                // aback, `DART_Surprise`'s `DDH_COMBAT_SURPRISED`)
                if n.kind == NoiseKind::DeathRattle && !npc.suspecting && npc.attn_level <= 3 && npc.alert != Alert::Combat {
                    npc.bark_req = (npc.bark_req.0.wrapping_add(1), "COMBAT_SURPRISED");
                }
                // (an alarm makes it suspect, its attention reactions' `Alarm`)
                if n.kind == NoiseKind::Alarm {
                    npc.suspecting = true;
                }
                let (amount, lvl) = att.heard[n.kind.slot()];
                let reason = match n.kind {
                    NoiseKind::DeathRattle => 2,
                    NoiseKind::Alarm => 3,
                    NoiseKind::Help => 6,
                    _ => 7,
                };
                let busted = att.to_busted & (1 << reason) != 0;
                let cap = if busted { att.max } else { (att.levels[3][0] - 0.001).min(att.max) };
                let floor = match lvl {
                    0 => 0.0,
                    1..=4 => att.levels[lvl as usize - 1][0],
                    _ => att.max,
                }
                .min(cap);
                npc.awareness = npc.awareness.max((npc.awareness + amount).max(floor).min(cap));
                npc.last_seen = Some(n.pos);
                if busted && npc.awareness >= att.levels[3][0] {
                    // (busted by the call: after Corvo where it came from)
                    if std::env::var("DH_NPC_LOG").is_ok() {
                        info!("{}: busted by what it heard ({:?}) at {:.1}", npc.name, n.kind, n.pos);
                    }
                    npc.alert = Alert::Combat;
                    npc.set_mode(Mode::Combat);
                    continue;
                }
                if npc.awareness >= att.levels[2][0] {
                    npc.alert = Alert::Suspicious;
                    npc.target = Some(n.pos);
                    if npc.mode != Mode::Investigate || runs_to(n.kind) {
                        npc.search_run = runs_to(n.kind);
                    }
                    npc.set_mode(Mode::Investigate);
                }
                continue;
            }
            if n.combat() {
                if npc.alert != Alert::Combat {
                    npc.alert = Alert::Combat;
                    npc.awareness = npc.awareness.max(0.9);
                    npc.last_seen = Some(n.pos);
                    npc.set_mode(Mode::Combat);
                }
            } else if npc.mode != Mode::Combat {
                npc.alert = Alert::Suspicious;
                npc.awareness = npc.awareness.max(0.4);
                npc.target = Some(n.pos);
                npc.last_seen = Some(n.pos);
                npc.search_run = runs_to(n.kind);
                npc.set_mode(Mode::Investigate);
            }
        }
    }
    // a search request (to 25 m) or a notice (to 8 m) from one of its side: its attention at
    // least at investigating (searching itself, at a walk, where the caller thinks Corvo is) or
    // at turning to look there (0x00B438C0, 0x00B3F7F0, decoded for this rewrite)
    for c in calls.iter().filter(|c| c.kind != CallKind::Help) {
        let reach = if c.kind == CallKind::Search { 25.0 } else { 8.0 };
        for (mut npc, t) in &mut npcs {
            if npc.is_down() || !npc.hostile() || npc.kind == Kind::Story || npc.deaf || npc.alert == Alert::Combat || t.translation.distance(c.from) < 0.05 || !heeds(&npc, c) {
                continue;
            }
            let (d, through) = match rooms.route(t.translation, c.from, true) {
                Some(r) => (r.dist, r.gain),
                None => (t.translation.distance(c.from), 1.0),
            };
            // (the line's call is a noise as any other, 0x00C9BB50 -> 0x00C5A760)
            if d > reach * through || walled(t.translation, c.from) {
                continue;
            }
            // (one already looking there, within 2.5 m of the spot, is on it: as a searcher come
            // to a spot others look into is, `SearchReachedProxy`)
            if matches!(npc.mode, Mode::Search | Mode::Investigate) && npc.last_seen.is_some_and(|l| (l - c.spot).abs().max_element() < 2.5) {
                continue;
            }
            let Some(att) = scene.and_then(|s| attention_of(&npc, s)) else { continue };
            let floor = att.levels[if c.kind == CallKind::Search { 2 } else { 1 }][0];
            if npc.awareness >= floor {
                continue;
            }
            npc.awareness = floor;
            npc.last_seen = Some(c.spot);
            if c.kind == CallKind::Search && !matches!(npc.mode, Mode::Investigate | Mode::Flee) {
                if std::env::var("DH_NPC_LOG").is_ok() {
                    info!("{}: joins a search at {:.1} (called {d:.1} m off)", npc.name, c.spot);
                }
                npc.alert = Alert::Suspicious;
                npc.target = Some(c.spot);
                npc.search_run = false;
                npc.set_mode(Mode::Investigate);
                // (answering the call: `SEARCH_FROM_ALLY`, the search's ally response)
                npc.bark_req = (npc.bark_req.0.wrapping_add(1), "SEARCH_FROM_ALLY");
            }
        }
    }
}

/// The attention level (0 unaware, 1 head track, 2 turn to face, 3 investigate, 4 busted) an
/// attention value holds, from the current one: levels start at their start threshold and end
/// below their stop one.
pub fn attention_level(att: &dhcook::format::Attention, a: f32, cur: u8) -> u8 {
    let mut l = cur.min(4);
    while l < 4 && a >= att.levels[l as usize][0] {
        l += 1;
    }
    while l > 0 && a < att.levels[l as usize - 1][1] {
        l -= 1;
    }
    l
}

/// The attention tweak a character uses towards Corvo: an enemy it doesn't yet suspect, one it
/// suspects, or no enemy.
/// Its attention raised for a reason (`DisAttentionIncrease`: the amount added, then at least the
/// level's start, as roshan's r614/blink research found), busted only where the reason may take
/// it there (`m_ReasonSettings[reason].m_bCanIncreaseToBusted`).
pub(crate) fn raise_attention(npc: &mut Npc, att: &dhcook::format::Attention, (amount, lvl): (f32, u8), reason: u32) {
    let busted = att.to_busted & (1 << reason) != 0;
    let cap = if busted { att.max } else { (att.levels[3][0] - 0.001).min(att.max) };
    let floor = match lvl {
        0 => 0.0,
        1..=4 => att.levels[lvl as usize - 1][0],
        _ => att.max,
    }
    .min(cap);
    npc.awareness = npc.awareness.max((npc.awareness + amount).max(floor).min(cap));
}

pub fn attention_of<'a>(npc: &Npc, scene: &'a dhcook::format::Scene) -> Option<&'a dhcook::format::Attention> {
    let s = npc.npc_type.and_then(|t| scene.npc_types.get(t as usize)).and_then(|t| t.sight.as_ref())?;
    // (its suspicion level picks the profile against an enemy, `m_pAttentionTweaks_Enemy_
    // Unsuspecting` / `_Suspecting`: a search heard of keeps the unsuspecting one)
    Some(if !npc.hostile() {
        &s.neutral
    } else if npc.suspecting {
        &s.suspecting
    } else {
        &s.unsuspecting
    })
}

/// A body's light (0..1), sampled now and then as it lies (`stealth::body_light`).
#[derive(Component)]
pub struct BodyLight(pub f32, f32);

/// The bodies someone has looked over (`m_bCorpseHandled`): the others pass them by, but those
/// that react to handled bodies (`m_bReactToHandledCorpses`, the civilians).
#[derive(Resource, Default)]
pub struct HandledBodies(pub std::collections::HashSet<Entity>);

/// A character's side's stance to another's (by their factions): 1 its own or an ally, 2 an
/// enemy, 0 neutral.
fn relation(scene: Option<&dhcook::format::Scene>, me: &str, them: &str) -> u8 {
    if !me.is_empty() && (me == them || scene.and_then(|s| s.allies.get(me)).is_some_and(|a| a.iter().any(|f| f == them))) {
        1
    } else if scene.and_then(|s| s.factions.get(me)).is_some_and(|en| en.iter().any(|f| f == them)) {
        2
    } else {
        0
    }
}

/// A call a character's line carries to the others of its side (the dialog hooks' flags,
/// decoded for this rewrite: 0x00C9BB50): a cry for help (the combat lines', 25 m), a search
/// request (the search lines', 25 m: they search where it thinks Corvo is), a notice (8 m:
/// they look there).
#[derive(Message, Clone)]
pub struct AllyCall {
    pub from: Vec3,
    pub spot: Vec3,
    pub faction: String,
    pub kind: CallKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CallKind {
    Help,
    Search,
    Notice,
}

/// The marks a body is seen at (`Twk_Attn_Simple_*`): a body, a death as it happens.
const SEE_CORPSE: f32 = 0.35;
const SEE_DEATH: f32 = 0.025;

/// What nearness in a cone gives a body's visibility (`m_CorpseVisTuning`: (0.55, 0),
/// (0.875, 0.8), (0.96, 2.0) over 1 - distance / the cone's range).
fn corpse_tuning(x: f32) -> f32 {
    const P: [(f32, f32); 3] = [(0.55, 0.0), (0.875, 0.8), (0.96, 2.0)];
    if x <= P[0].0 {
        return P[0].1;
    }
    for w in P.windows(2) {
        if x <= w[1].0 {
            return w[0].1 + (w[1].1 - w[0].1) * (x - w[0].0) / (w[1].0 - w[0].0);
        }
    }
    P[2].1
}

/// Whether a body at `at` (its light `light`) is seen from the eyes: in a vision cone, its
/// nearness's tuning plus its light at `mark` or more (0x00B50030).
fn body_seen(s: &dhcook::format::Sight, eye: Vec3, fwd: Vec3, at: Vec3, light: f32, mark: f32) -> bool {
    let to = at + Vec3::Y * 0.2 - eye;
    let d = to.length();
    let Some(range) = vision_range(s, fwd, to / d.max(1e-3), d, false) else { return false };
    corpse_tuning(1.0 - d / range.max(1e-3)) + light >= mark
}

/// The bodies' light, each sampled every second or so as it lies.
fn body_lights(
    mut commands: Commands,
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    wl: Option<Res<crate::world_light::WorldLighting>>,
    rapier: ReadRapierContext,
    mut bodies: Query<(Entity, &Npc, &Transform, Option<&mut BodyLight>)>,
) {
    let Some(level) = level else { return };
    let ctx = rapier.single().ok();
    let now = time.elapsed_secs();
    for (e, n, t, l) in &mut bodies {
        if !n.is_down() {
            if l.is_some() {
                commands.entity(e).try_remove::<BodyLight>();
            }
            continue;
        }
        if l.as_ref().is_some_and(|l| now < l.1) {
            continue;
        }
        let v = crate::stealth::body_light(&level, wl.as_deref(), t.translation + Vec3::Y * 0.3, ctx.as_ref());
        let next = now + 0.8 + rand::random::<f32>() * 0.4;
        match l {
            Some(mut l) => *l = BodyLight(v, next),
            None => {
                commands.entity(e).try_insert(BodyLight(v, next));
            }
        }
    }
}

/// Whether a direction from the eyes falls in the character's focused or peripheral vision
/// (`DisTweaks_Vision`: horizontal angle, the vertical one a share of it up and down, and range):
/// that cone's range. Fighting, it follows Corvo all around close by and up and down.
fn vision_range(s: &dhcook::format::Sight, fwd: Vec3, dir: Vec3, d: f32, combat: bool) -> Option<f32> {
    let flat = dir.with_y(0.0).normalize_or_zero();
    let h = flat.dot(fwd).clamp(-1.0, 1.0).acos();
    let v = dir.y.clamp(-1.0, 1.0).asin();
    if combat {
        return (d <= s.inner[1] && (h <= (s.outer[0] * 0.5).to_radians() || d < 7.0)).then_some(s.inner[1]);
    }
    // (an elliptical cone, 0x00BEC910: its horizontal half angle across, its up or down scale of
    // that upright, the bounds between them along the ellipse; as roshan's r614/blink research
    // decoded it)
    let _ = (h, v);
    let side = fwd.cross(Vec3::Y).normalize_or(Vec3::X);
    let (x, y, z) = (dir.dot(fwd), dir.dot(side), dir.y);
    let within = |c: &[f32; 4]| {
        let half = (c[0] * 0.5).to_radians();
        if d > c[1] || x < half.cos() {
            return false;
        }
        let vert = half * if z >= 0.0 { c[2] } else { c[3] };
        let c2v = (2.0 * vert).cos();
        let cfov = (2.0 * half).cos();
        let k = (2.0 * (half.sin() * z).atan2(vert.sin() * y)).cos();
        2.0 * x * x - 1.0 >= (c2v + k * (cfov - c2v) + cfov) * 0.5
    };
    if within(&s.inner) {
        Some(s.inner[1])
    } else if within(&s.outer) {
        Some(s.outer[1])
    } else {
        None
    }
}

/// Vision: the characters' cones and line of sight; what they see of Corvo raises their
/// attention (his visibility plus what nearness gives, `DisTweaks_PawnAttention`), which
/// otherwise leaks away, and its level decides what they do.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn npc_perception(
    mut arrival: ResMut<ArrivalTimer>,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut stats: ResMut<PlayerStats>,
    player: Query<(Entity, &Transform, &Player, Option<&LitActor>)>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), (Without<Player>, Without<ConsumedBody>)>,
    possession: Res<crate::possession::Possession>,
    mut found: Local<std::collections::HashSet<Entity>>,
    (froms, mut events, swarms): (Query<&FromSpawner>, MessageWriter<crate::interact::Interaction>, Query<(&Transform, &crate::swarm::Swarm), Without<Npc>>),
    (level, pvis, settings, notarget): (Option<Res<LevelInfo>>, Res<crate::stealth::PlayerVisibility>, Res<crate::settings::Settings>, Option<Res<crate::script::NoTarget>>),
    (body_lights, carry, handled, heads): (Query<&BodyLight>, Res<crate::carry::Carry>, Res<HandledBodies>, Query<&HeadTrack>),
) {
    // (rat swarms about that would attack: Corvo's, or a level's grown to its
    // `m_InitialRatCountToBeAggressive`; a stray rat or two only scurries off)
    let swarm_at: Vec<Vec3> = swarms.iter().filter(|(_, s)| !s.rats.is_empty() && s.wild.as_ref().is_none_or(|w| s.rats.len() >= w.aggressive)).map(|(t, _)| t.translation).collect();
    let (dt_world, dt_own) = (time.delta_secs() * tc.world_scale(), time.delta_secs() * tc.own_scale());
    // inside a host, Corvo is the host
    // (`DH_NOTARGET`, `notarget`: unseen, for watching the characters in tests)
    let hidden = possession.host.is_some() || notarget.is_some();
    // (no time at all: paused)
    if dt_world <= 0.0 && dt_own <= 0.0 {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let Ok((pe, pt, p, plit)) = player.single() else { return };
    // arrival grace while the level fades in
    arrival.0 += time.delta_secs();
    if arrival.0 < ARRIVAL_GRACE {
        return;
    }
    // his feet, his cylinder as the original's (`CollisionHeight` 87.5 uu standing, 65 sneaking),
    // and the strip the onlookers' lines of sight go to (`Twk_DefaultVisRegion`: standing 30-150
    // uu above the feet, sneaking 50-90, crouched by the world 30-60; 35 uu out towards the
    // onlooker, 20 wide), as roshan's r614/blink research decoded them
    let feet = pt.translation.y - p.body().depth();
    let cylinder = match p.body() {
        crate::player::Body::Stand => 0.875,
        crate::player::Body::Sneak => 0.65,
        crate::player::Body::Crawl => 0.33,
    };
    let region = if p.crawl {
        (0.3, 0.6)
    } else if p.crouched {
        (0.5, 0.9)
    } else {
        (0.3, 1.5)
    };
    // (characters without the original's tweaks: darker = harder to see, 0.35 .. 1.0)
    let light = plit.map(|l| l.brightness).unwrap_or(0.4);
    let light_factor = (0.35 + light * 1.6).clamp(0.35, 1.0);
    let stance = if p.crouched { 0.55 } else if p.sprinting { 1.35 } else { 1.0 };
    let difficulty = settings.difficulty.min(3) as usize;
    let log = std::env::var("DH_ATTN_LOG").is_ok();
    // (the bodies to be found: the dead and the knocked out alike, not one Corvo carries; their
    // light, their faction, whether one has looked them over)
    let bodies: Vec<(Entity, Vec3, f32, String, bool)> = npcs
        .iter()
        .filter(|(e, n, _)| n.is_down() && !n.corpse && carry.body != Some(*e))
        .map(|(e, n, t)| (e, t.translation, body_lights.get(e).map_or(0.0, |l| l.0), n.faction.clone(), handled.0.contains(&e)))
        .collect();
    // (the body Corvo carries: whose side it was on)
    let carried: Option<String> = carry.body.and_then(|b| npcs.get(b).ok()).map(|(_, n, _)| n.faction.clone());
    // (deaths just now: the bodies of the last two seconds)
    let fresh: Vec<(Entity, Vec3)> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Dead && !n.corpse && n.corpse_age < 2.0).map(|(e, _, t)| (e, t.translation)).collect();
    // (for the civilians: the guards fighting, the others panicking)
    let fighting: Vec<Vec3> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Combat && n.hostile() && !n.is_down()).map(|(_, _, t)| t.translation).collect();
    // (those about, for one seeing an ally: what each is doing, fighting 1, searching 2,
    // panicking 3, and where it thinks Corvo is)
    let doing: Vec<(Entity, Vec3, String, u8, Option<Vec3>)> = npcs
        .iter()
        .filter(|(_, n, _)| !n.is_down() && n.kind != Kind::Story)
        .filter_map(|(e, n, t)| {
            let what = match n.mode {
                Mode::Combat if n.alert == Alert::Combat && n.foe.is_none() => 1,
                Mode::Search | Mode::Investigate if n.alert != Alert::Unaware => 2,
                Mode::Flee if !n.fighter() => 3,
                _ => return None,
            };
            Some((e, t.translation, n.faction.clone(), what, n.last_seen))
        })
        .collect();
    let panicking: Vec<Vec3> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Flee && !n.fighter() && !n.is_down()).map(|(_, _, t)| t.translation).collect();
    let mut newly_detected = false;
    for (e, mut npc, t) in &mut npcs {
        npc.carry_bark = None;
        if npc.is_down() || npc.kind == Kind::Story || npc.mode == Mode::Choked || stats.dead || Some(e) == possession.host || npc.blind {
            npc.sees_player = false;
            continue;
        }
        // (the world stopped: those in it see nothing; those bent time passes by go on)
        let dt = if npc.out_of_bend { dt_own } else { dt_world };
        if dt <= 0.0 {
            npc.sees_player = false;
            continue;
        }
        let eye = t.translation + Vec3::Y * 0.72;
        // its sight along its head (the vision component's eye update, 0x00C676A0, as roshan's
        // r614/blink research decoded it): its facing turned by its look-at's aim while it looks
        // at something (Corvo it has noticed, its looks about), unless that is within
        // `m_fVision_BoneNullzone_Deg` 5 degrees of its facing
        let head = heads.get(e).map_or(0.0, |h| h.yaw * h.weight * h.weight * (3.0 - 2.0 * h.weight));
        let fwd = if head.abs() > 5f32.to_radians() { Quat::from_rotation_y(npc.yaw + head) * Vec3::NEG_Z } else { npc.forward() };
        let sight = npc.npc_type.and_then(|i| level.as_ref()?.scene.npc_types.get(i as usize)?.sight.as_ref());
        let mut visible = false;
        let mut dist = f32::MAX;
        let mut range = 30.0 * light_factor;
        // (Corvo unseen, in a host or for tests: what they think of him only leaks away, but
        // the bodies about are seen as ever)
        // the point the cone and nearness take: where the eye's level meets his cylinder (`target
        // vslot 0`, 0x00C669C0); seen when one of the strip's five points (its corners and middle)
        // is in a clear line from the eye (0x00C77FC0)
        let aim = Vec3::new(pt.translation.x, eye.y.clamp(feet, feet + 2.0 * cylinder), pt.translation.z);
        for target in [aim].into_iter().filter(|_| !hidden) {
            let to = target - eye;
            let d = to.length();
            let dir = to / d.max(1e-3);
            let r = match sight {
                Some(s) => vision_range(s, fwd, dir, d, npc.alert == Alert::Combat),
                None => {
                    // in combat they track you all around at close range; otherwise a ~110
                    // degree cone
                    let flat = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
                    let cone = if npc.alert == Alert::Combat {
                        if d < 7.0 { -1.0 } else { 0.0 }
                    } else if d < 4.0 {
                        0.17
                    } else {
                        0.57
                    };
                    (d <= 30.0 * light_factor && flat.dot(fwd) >= cone && dir.y.abs() <= 0.8).then_some(30.0 * light_factor)
                }
            };
            let Some(r) = r else { continue };
            let filter = QueryFilter::default()
                .exclude_collider(e)
                .exclude_collider(pe)
                .groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
            let toward = (eye - pt.translation).with_y(0.0).normalize_or(Vec3::Z);
            let across = toward.cross(Vec3::Y).normalize_or(Vec3::X) * 0.1;
            let base = Vec3::new(pt.translation.x, feet, pt.translation.z) + toward * 0.35;
            let (lo, hi) = region;
            let points = [base + Vec3::Y * hi + across, base + Vec3::Y * hi - across, base + Vec3::Y * lo + across, base + Vec3::Y * lo - across, base + Vec3::Y * (lo + hi) * 0.5];
            let clear = |p: &Vec3| {
                let to = *p - eye;
                let l = to.length();
                l < 0.05 || ctx.cast_ray(eye, to / l, l, true, filter).is_none()
            };
            if points.iter().any(clear) {
                visible = true;
                dist = d;
                range = r;
                break;
            }
        }
        // (an enemy he is in sight of keeps him there without a clear line while he stays within
        // its attention's `m_fDistanceToLoseSight`, 3 m: round a corner, behind a crate, as
        // roshan's r614/blink research decoded it)
        if !visible && !hidden && npc.sees_player && npc.hostile() {
            let keep = level.as_ref().and_then(|l| attention_of(&npc, &l.scene)).map_or(0.0, |a| a.lose_sight);
            let off = pt.translation - t.translation;
            if off.with_y(0.0).length() <= keep && off.y.abs() <= keep {
                visible = true;
                dist = off.length();
            }
        }
        // Corvo come into its sight carrying a body (0x00B304D0, decoded for this rewrite):
        // it suspects (`m_bEscalateSuspicionIfSeeAnyCorpse`: its suspecting profile, held as a
        // search's is), and those with `m_bBecomeEnemyIfSeeCorpseCarry` (the civilians) take him
        // for an enemy unless on his side; its line of sight is enough
        let scene = level.as_ref().map(|l| &l.scene);
        let corvo_side = dhcook::format::is_player_faction(&npc.faction) || scene.and_then(|s| s.allies.get(&npc.faction)).is_some_and(|a| a.iter().any(|f| dhcook::format::is_player_faction(f)));
        if visible && !npc.sees_player && carried.is_some() {
            let flags = sight.map(|s| s.corpses).unwrap_or_default();
            if flags.escalate {
                npc.suspecting = true;
            }
            if flags.enemy_if_carried && !npc.hostile() && !corvo_side {
                npc.enemy = true;
                if log {
                    info!("{} sees Corvo carry a body: an enemy now", npc.name);
                }
            }
        }
        npc.sees_player = visible;
        if visible {
            npc.last_seen = Some(pt.translation);
        }
        let touching = !hidden && (pt.translation - t.translation).with_y(0.0).length() < 0.9 && (pt.translation.y - t.translation.y).abs() < 1.5;
        let att = level.as_ref().and_then(|l| attention_of(&npc, &l.scene));
        if let Some(att) = att {
            // the original's attention: what he gives away plus what nearness adds, by
            // difficulty; out of sight it leaks
            let mut rate = 0.0;
            if visible {
                let closeness = 1.0 - dist / range.max(0.1);
                // (clamped to 0..1 before the difficulty's scale: `m_bClampTargetVisibility`, as
                // roshan's r614/blink research decoded it)
                rate = att.distance.apply(pvis.value, closeness).clamp(0.0, 1.0) * att.see[difficulty];
            }
            if touching {
                rate += att.touch;
            }
            if rate > 0.0 {
                npc.awareness = (npc.awareness + rate * dt).min(att.max);
            } else {
                npc.awareness = (npc.awareness - att.leak * dt).max(0.0);
            }
            let was = npc.attn_level;
            npc.attn_level = attention_level(att, npc.awareness, was);
            npc.attn_fill = (npc.awareness / att.levels[3][0].max(0.05)).clamp(0.0, 1.0);
            if log && npc.attn_level != was {
                info!("attention: {} {:.2} level {} -> {} (sees {visible} at {:.1} m of {:.0}, vis {:.2})", npc.name, npc.awareness, was, npc.attn_level, dist.min(99.0), range, pvis.value);
            }
            // (its attention reactions, `DART_EscalateSuspicion`, while unsuspecting: its enemy
            // seen or touched at investigating or more makes it suspect, 0x00B36680)
            if npc.hostile() && !npc.suspecting && (visible || touching) && npc.attn_level >= 3 {
                npc.suspecting = true;
                // (touched by him, it is taken aback too, `DART_Surprise`'s
                // `DDH_COMBAT_SURPRISED`)
                if touching && npc.alert != Alert::Combat {
                    npc.bark_req = (npc.bark_req.0.wrapping_add(1), "COMBAT_SURPRISED");
                }
            }
            // busted with a body on his shoulder, by one neither his enemy nor of his side
            // (`CarryingCorpseOrBodyPart`, 0x00B42610..0x00B42730): a body of its side, or a
            // neutral's it protects, makes him its enemy (`m_CarryAllyCorpseResponse`, Hostile);
            // a neutral's or an enemy's it only remarks on (Harmless)
            if npc.attn_level >= 4 && was < 4 && !npc.hostile() && !corvo_side {
                if let Some(victim) = &carried {
                    let rel = relation(scene, &npc.faction, victim);
                    let protects = sight.is_some_and(|s| s.protect_neutrals);
                    let ally = rel == 1 || rel == 0 && protects;
                    npc.carry_bark = Some(if ally { 0 } else if rel == 0 { 1 } else { 2 });
                    // (a remark is its reaction behaviour's, `ReactionRequest`, 0x00B3EAB0)
                    if !ally {
                        npc.reacting = 3.0;
                    }
                    if ally {
                        npc.enemy = true;
                    }
                    if log {
                        info!("{} sees Corvo carry a body ({victim}): {}", npc.name, if ally { "an enemy now" } else { "remarks on it" });
                    }
                }
            }
            if visible && npc.hostile() {
                if npc.attn_level >= 4 && npc.alert != Alert::Combat {
                    npc.alert = Alert::Combat;
                    npc.set_mode(Mode::Combat);
                    newly_detected = true;
                } else if npc.attn_level >= 3 && npc.alert == Alert::Unaware {
                    npc.alert = Alert::Suspicious;
                    npc.target = npc.last_seen;
                    // (a sighting: it runs to look)
                    npc.search_run = true;
                    npc.set_mode(Mode::Investigate);
                }
            }
            // it busts one it takes for an enemy: those whose panic has `m_SeeEnemy` (servants,
            // civilians turned against him) run from where they believe he is, on the step up
            // to busted only (`EnemyBusted`, 0x00B22DA0 / 0x00B61680, decoded for this rewrite)
            if npc.attn_level >= 4 && was < 4 && npc.hostile() && may_flee(&npc) && panic_rule(scene, &npc, "SeeEnemy") {
                npc.last_seen = Some(pt.translation);
                npc.set_mode(Mode::Flee);
                if log {
                    info!("{} busts Corvo, its enemy: panics", npc.name);
                }
            }
            // what it heard rises to investigating (`npc_hearing`)
            if npc.hostile() && !visible && npc.attn_level >= 3 && npc.alert == Alert::Unaware {
                npc.alert = Alert::Suspicious;
                npc.target = npc.last_seen;
                npc.set_mode(Mode::Investigate);
            }
        } else if visible {
            if npc.hostile() {
                let view = 30.0 * light_factor;
                let closeness = (1.0 - dist / view).clamp(0.0, 1.0);
                let mut rate = (0.2 + 1.6 * closeness * closeness) * stance * (0.5 + 0.5 * light_factor);
                if npc.alert == Alert::Suspicious {
                    rate *= 1.6;
                }
                if dist < 2.5 {
                    rate += 1.5;
                }
                npc.awareness = (npc.awareness + rate * dt).min(1.0);
            }
        } else {
            let decay = match npc.alert {
                Alert::Unaware => 0.12,
                Alert::Suspicious => 0.05,
                Alert::Combat => 0.02,
            };
            npc.awareness = (npc.awareness - decay * dt).max(0.0);
        }
        if npc.hostile() {
            if att.is_none() {
                if npc.awareness >= 1.0 && npc.alert != Alert::Combat {
                    npc.alert = Alert::Combat;
                    npc.set_mode(Mode::Combat);
                    newly_detected = true;
                } else if npc.awareness >= 0.35 && npc.alert == Alert::Unaware {
                    npc.alert = Alert::Suspicious;
                    npc.target = npc.last_seen;
                    npc.set_mode(Mode::Investigate);
                }
                npc.attn_level = if npc.alert == Alert::Combat { 4 } else if npc.awareness >= 0.35 { 3 } else if npc.awareness > 0.05 { 1 } else { 0 };
                npc.attn_fill = npc.awareness;
            }
            // an ally newly in its sight, as what it is doing (0x00B435F0 / 0x00B0F5B0, decoded
            // for this rewrite): fighting, it suspects and joins in (`AllyCombat`, +1.0);
            // searching, its attention at least at turning to look (unsuspecting) or
            // investigating (suspecting) where the ally thinks Corvo is; panicking, it suspects
            // and looks there the same. Each sighting once, while the ally stays in its sight
            // doing that
            if npc.alert != Alert::Combat && npc.hostile() {
                let mine: Vec<(Entity, u8)> = doing
                    .iter()
                    .filter(|(o, p, f, _, _)| *o != e && p.distance(eye) < 35.0 && relation(scene, &npc.faction, f) == 1)
                    .filter(|(_, p, _, _, _)| {
                        let to = *p + Vec3::Y * 0.7 - eye;
                        let d = to.length();
                        let in_view = sight.is_none_or(|s| vision_range(s, fwd, to / d.max(1e-3), d, false).is_some());
                        in_view && ctx.cast_ray(eye, to / d.max(1e-3), (d - 0.4).max(0.0), true, QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD))).is_none()
                    })
                    .map(|(o, _, _, w, _)| (*o, *w))
                    .collect();
                let now = time.elapsed_secs();
                for (o, what) in &mine {
                    if let Some(seen) = npc.allies_seen.iter_mut().find(|x| x.0 == *o && x.1 == *what) {
                        seen.2 = now;
                        continue;
                    }
                    npc.allies_seen.push((*o, *what, now));
                    let Some((_, at, _, _, proxy)) = doing.iter().find(|x| x.0 == *o) else { continue };
                    let spot = proxy.unwrap_or(*at);
                    let Some(att) = level.as_ref().and_then(|l| attention_of(&npc, &l.scene)) else { continue };
                    if *what != 2 {
                        npc.suspecting = true;
                    }
                    if log {
                        info!("{} sees an ally {} (Corvo thought at {spot:.1})", npc.name, ["", "fighting", "searching", "panicking"][*what as usize]);
                    }
                    // (fighting: it joins in, as one seeing a fight start does)
                    if *what == 1 {
                        npc.alert = Alert::Combat;
                        npc.awareness = 1.0;
                        npc.last_seen = Some(spot);
                        npc.set_mode(Mode::Combat);
                        break;
                    }
                    let floor = if npc.suspecting { att.levels[2][0] } else { att.levels[1][0] };
                    if npc.awareness < floor {
                        npc.awareness = floor;
                        npc.last_seen = Some(spot);
                        if npc.awareness >= att.levels[2][0] && !matches!(npc.mode, Mode::Investigate | Mode::Search) {
                            npc.alert = Alert::Suspicious;
                            npc.target = Some(spot);
                            npc.search_run = false;
                            npc.set_mode(Mode::Investigate);
                        }
                    }
                }
                npc.allies_seen.retain(|x| now - x.2 < 1.0);
            }
            // a death before its eyes (fighting too): seen as a body is, at the lower mark
            // (`m_fCorpseVis_VisLevelToSeeDeath`)
            if !npc.witnessed_death {
                for (f, b) in &fresh {
                    let light = bodies.iter().find(|x| x.0 == *f).map_or(0.0, |x| x.2);
                    if sight.is_some_and(|s| body_seen(s, eye, fwd, *b, light, SEE_DEATH)) || sight.is_none() && b.distance(eye) < 14.0 && (*b - eye).normalize_or_zero().dot(fwd) > 0.3 {
                        let d = b.distance(eye);
                        let dir = (*b + Vec3::Y * 0.2 - eye).normalize_or_zero();
                        let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                        if ctx.cast_ray(eye, dir, d - 0.3, true, filter).is_none() {
                            npc.witnessed_death = true;
                            // (unsuspecting, its attention at investigating at most: taken
                            // aback, `DART_Surprise`)
                            if !npc.suspecting && npc.attn_level <= 3 {
                                npc.bark_req = (npc.bark_req.0.wrapping_add(1), "COMBAT_SURPRISED");
                            }
                            break;
                        }
                    }
                }
            }
            // discover bodies, as the original's simple attention to a body has it (0x00B50030,
            // 0x00B4BFC0, decoded for this rewrite): in its vision cones with a clear line, seen
            // once the curve of its nearness in that cone plus the body's own light reaches
            // `m_fCorpseVis_VisLevelToSeeCorpse` (0.35: in the dark within 10.8 m ahead, 5.2 m
            // aside; lit, as far as it sees), found at once. A body of an ally (or a neutral's,
            // protecting them), each once; not one already looked over, nor any with
            // `m_bIgnoreAllCorpses`; the knocked out as the dead
            let flags = sight.map(|s| s.corpses).unwrap_or_default();
            let protects = sight.is_some_and(|s| s.protect_neutrals);
            if !npc.saw_body && npc.alert != Alert::Combat && !flags.ignore {
                for (body, b, light, faction, was_handled) in &bodies {
                    if npc.found_bodies.contains(body) || *was_handled && !flags.handled_too {
                        continue;
                    }
                    let rel = relation(scene, &npc.faction, faction);
                    if !(rel == 1 || rel == 0 && protects) {
                        continue;
                    }
                    let seen = match sight {
                        Some(s) => body_seen(s, eye, fwd, *b, *light, SEE_CORPSE),
                        None => b.distance(eye) < 14.0 && (*b - eye).normalize_or_zero().dot(fwd) > 0.5,
                    };
                    let d = b.distance(eye);
                    if seen {
                        let dir = (*b + Vec3::Y * 0.2 - eye).normalize_or_zero();
                        let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                        if ctx.cast_ray(eye, dir, d - 0.3, true, filter).is_none() {
                            npc.found_bodies.push(*body);
                            npc.checking = Some(*body);
                            if log {
                                info!("{} finds a body {d:.1} m away (light {light:.2})", npc.name);
                            }
                            npc.saw_body = true;
                            if flags.escalate {
                                npc.suspecting = true;
                            }
                            // (one it saw die, not one it came upon)
                            npc.witnessed_death |= fresh.iter().any(|f| f.0 == *body);
                            // the mission statistics count each body found once
                            if found.insert(*body) {
                                stats.bodies_found += 1;
                                stats.deeds_pending.push(("ePlayerStat_NumCorpsesDiscovered", String::new()));
                                // the level scripts hear of it (`DisSeqEvent_Corpse` Discovered)
                                if let Ok(f) = froms.get(*body) {
                                    events.write(crate::interact::Interaction::Corpse { spawner: f.0, what: 2 });
                                }
                            }
                            npc.alert = Alert::Suspicious;
                            npc.awareness = npc.awareness.max(0.6);
                            npc.target = Some(*b);
                            npc.last_seen = Some(*b);
                            // (a body: it runs to it, and looks it over)
                            npc.search_run = true;
                            npc.body_check = true;
                            npc.set_mode(Mode::Investigate);
                            // (one that can't fight, a servant, runs from it instead, its panic's
                            // `m_WitnessCorpse`)
                            if !npc.fighter() && may_flee(&npc) && panic_rule(scene, &npc, "WitnessCorpse") {
                                npc.body_check = false;
                                npc.checking = None;
                                npc.set_mode(Mode::Flee);
                            }
                            break;
                        }
                    }
                }
            }
        }
        // a body found by one not against Corvo, as a guard finds one (each its own record of
        // each body, 0x00B4BFC0: seen in its cones by its nearness and the body's light; a death
        // as it happens at the lower mark; those looked over too for the civilians'
        // `m_bReactToHandledCorpses`; none for those that ignore bodies), whether or not it may
        // panic at it: one of its side, or a neutral it protects, makes the culprit, Corvo, its
        // enemy and counts as found (0x00B40740)
        let mut came_upon: Option<(Vec3, bool)> = None;
        if !npc.hostile() {
            let flags = sight.map(|s| s.corpses).unwrap_or_default();
            let seen_body = |b: Vec3, light: f32, mark: f32| {
                (match sight {
                    Some(s) => body_seen(s, eye, fwd, b, light, mark),
                    None => b.distance(eye) < 14.0 && (b - eye).normalize_or_zero().dot(fwd) > 0.5,
                }) && {
                    let d = b.distance(eye);
                    let dir = (b + Vec3::Y * 0.2 - eye).normalize_or_zero();
                    let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                    ctx.cast_ray(eye, dir, d - 0.3, true, filter).is_none()
                }
            };
            let body = bodies.iter().filter(|x| !flags.ignore && !npc.found_bodies.contains(&x.0) && (!x.4 || flags.handled_too)).find_map(|(be, b, light, victim, _)| {
                let dying = fresh.iter().any(|f| f.0 == *be);
                (seen_body(*b, *light, SEE_CORPSE) || dying && seen_body(*b, *light, SEE_DEATH)).then_some((*be, *b, victim.clone(), dying))
            });
            if let Some((be, b, victim, dying)) = body {
                npc.found_bodies.push(be);
                came_upon = Some((b, dying));
                if flags.escalate {
                    npc.suspecting = true;
                }
                let rel = relation(scene, &npc.faction, &victim);
                if rel == 1 || rel == 0 && sight.is_some_and(|s| s.protect_neutrals) {
                    if !corvo_side {
                        npc.enemy = true;
                    }
                    if log {
                        info!("{} finds a body of its side: Corvo its enemy now", npc.name);
                    }
                    if found.insert(be) {
                        stats.bodies_found += 1;
                        stats.deeds_pending.push(("ePlayerStat_NumCorpsesDiscovered", String::new()));
                        if let Ok(f) = froms.get(be) {
                            events.write(crate::interact::Interaction::Corpse { spawner: f.0, what: 2 });
                        }
                    }
                }
            }
        }
        // (an unarmed one on the guards' side, a servant, panics only at a swarm: what else it
        // sees it reports to them)
        let civilian = civilian_may_panic(&npc) || came_upon.is_some() && may_flee(&npc) && !npc.fighter();
        let servant = npc.hostile() && !npc.fighter() && npc.may_panic() && npc.can_flee && npc.mode != Mode::Flee && !npc.is_down();
        if civilian || servant {
            // a civilian panics (`TweaksBehavior_Panic_Civilian`, as roshan's r614/blink research
            // listed its triggers): a body in its sight (`WitnessCorpse`, `WitnessDeath`), a guard
            // fighting in its sight within the group distance (`WitnessAllyCombat`), another
            // panicking within it, seen or not (`WitnessAllyPanic`)
            let pos = t.translation;
            let in_sight = |b: Vec3| {
                let d = b.distance(eye);
                d < 14.0 && (b - eye).normalize_or_zero().dot(fwd) > 0.5 && {
                    let dir = (b + Vec3::Y * 0.2 - eye).normalize_or_zero();
                    let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                    ctx.cast_ray(eye, dir, d - 0.3, true, filter).is_none()
                }
            };
            // (each as its own panic behaviour has the rule on, where it lists any; a rat swarm
            // in sight, `SeeRatSwarm`, only where it does)
            let rules = level.as_ref().and_then(|l| npc.npc_type.and_then(|i| l.scene.npc_types.get(i as usize))).and_then(|ty| ty.sight.as_ref()).and_then(|s| s.panic.as_ref()).map(|p| p.rules.clone()).filter(|r| !r.is_empty());
            let on = |r: &str| rules.as_ref().is_none_or(|rs| rs.iter().any(|x| x == r));
            // (a body come upon just now: any, `WitnessCorpse`; one dying before its eyes,
            // `WitnessDeath`)
            let why = came_upon
                .filter(|(_, dying)| civilian && (on("WitnessCorpse") || *dying && on("WitnessDeath")))
                .map(|(b, _)| (b, "a body"))
                .or_else(|| fighting.iter().copied().find(|f| civilian && on("WitnessAllyCombat") && f.distance(pos) < GROUP_DISTANCE && in_sight(*f + Vec3::Y * 0.5)).map(|f| (f, "a fight")))
                .or_else(|| panicking.iter().copied().find(|f| civilian && on("WitnessAllyPanic") && f.distance(pos) < GROUP_DISTANCE && f.distance(pos) > 0.05).map(|f| (f, "another panicking")))
                .or_else(|| rules.as_ref().filter(|rs| rs.iter().any(|x| x == "SeeRatSwarm")).and_then(|_| swarm_at.iter().copied().find(|s| in_sight(*s))).map(|s| (s, "a swarm")));
            if let Some((from, what)) = why {
                if std::env::var("DH_NPC_LOG").is_ok() {
                    info!("{}: panics at {what} at {from:.1}", npc.name);
                }
                npc.last_seen = Some(from);
                npc.set_mode(Mode::Flee);
            }
        }
    }
    if newly_detected {
        stats.times_detected += 1;
    }
}

/// A character paying attention to Corvo looks his way (from the original's head-track level)
/// unless a scene moves its head.
#[derive(Component)]
struct AttnLook;

fn attention_look(mut commands: Commands, npcs: Query<(Entity, &Npc, Has<AttnLook>, Has<SceneLook>, Has<ScriptedAnim>)>) {
    for (e, npc, mine, look, scripted) in &npcs {
        let want = npc.attn_level >= 1 && npc.alert != Alert::Combat && !npc.is_down() && !scripted && npc.last_seen.is_some();
        if want && (mine || !look) {
            let target = npc.last_seen.unwrap_or_default();
            commands.entity(e).try_insert((AttnLook, SceneLook { target, weight: 1.0 }));
        } else if !want && mine {
            commands.entity(e).try_remove::<(AttnLook, SceneLook)>();
        }
    }
}

/// High level decisions: pick movement targets and attacks for each NPC.
/// A live explosive sets a character making off (`AI_BhvrTwk_EscapeExplosion`'s
/// `TweakBehaviors_EscapeExplosion_Default`): one within its threat radius (300 uu) and its
/// discovery angle (45 degrees) in its sight.
const ESCAPE_RADIUS: f32 = 3.0;
const ESCAPE_ANGLE: f32 = 45.0;
/// How far from it it makes for (m: `m_fExplosionClearance`, 600 uu).
const ESCAPE_CLEARANCE: f32 = 6.0;
/// How long it keeps away at most (`m_fEscapeEndTimer`), and once it has gone off
/// (`m_fEscapeThreatTimerAfterExplosion`) (s).
const ESCAPE_END: f32 = 5.0;
const ESCAPE_AFTER: f32 = 1.0;

/// Characters that see a live grenade by them make off from it (`DisBehaviorEscapeExplosion`):
/// not its thrower, nor one it's stuck to; they keep away while it's there (a thrown one
/// followed as it rolls), and a moment after it goes off.
fn escape_explosions(
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    grenades: Query<(&crate::gadgets::Grenade, &Transform)>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), (Without<crate::possession::Possessed>, Without<ScriptedAnim>)>,
) {
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    for (e, mut npc, t) in &mut npcs {
        let dt = time.delta_secs() * if npc.out_of_bend { tc.own_scale() } else { tc.world_scale() };
        if npc.is_down() || npc.kind == Kind::Story {
            npc.escape = None;
            npc.plague_escape = None;
            continue;
        }
        if let Some((to, left)) = npc.plague_escape {
            npc.plague_escape = (left - dt > 0.0).then_some((to, left - dt));
        }
        if let Some((at, left)) = npc.escape {
            // the one it keeps from, followed; gone off, a moment more
            let live = grenades.iter().map(|(_, gt)| gt.translation).filter(|p| p.distance(at) < 1.5).min_by(|a, b| a.distance(at).total_cmp(&b.distance(at)));
            let left = if live.is_some() { left - dt } else { left.min(ESCAPE_AFTER) - dt };
            npc.escape = (left > 0.0).then(|| (live.unwrap_or(at), left));
            continue;
        }
        if dt <= 0.0 {
            continue;
        }
        let eye = t.translation + Vec3::Y * 0.6;
        let fwd = npc.forward();
        let seen = grenades.iter().filter(|(g, _)| g.thrower() != Some(e) && g.stuck_on() != Some(e)).map(|(_, gt)| gt.translation).find(|p| {
            let to = *p - eye;
            let d = to.length();
            d < ESCAPE_RADIUS
                && to.with_y(0.0).normalize_or_zero().dot(fwd) >= ESCAPE_ANGLE.to_radians().cos()
                // (short of the floor it lies on)
                && ctx.cast_ray(eye, to / d.max(1e-3), (d - 0.25).max(0.0), true, walls).is_none()
        });
        if let Some(p) = seen {
            info!("{} makes off from a grenade {:.1} m away", npc.name, p.distance(t.translation));
            npc.escape = Some((p, ESCAPE_END));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn npc_brain(
    time: Res<Time>,
    tc: Res<TimeControl>,
    trail: Res<PlayerTrail>,
    stats: Res<PlayerStats>,
    attrs: Res<crate::gamedata::Attrs>,
    rapier: ReadRapierContext,
    mut hit_writer: MessageWriter<PlayerHit>,
    (mut noise, mut shots): (MessageWriter<Noise>, MessageWriter<NpcShot>),
    player: Query<(Entity, &Transform, Option<&crate::combat::Sword>, &Player)>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), (Without<Player>, Without<crate::possession::Possessed>)>,
    (level, mut npc_hits, ai, mut fled): (Option<Res<LevelInfo>>, MessageWriter<NpcHit>, Res<crate::aiworld::AiWorld>, MessageWriter<crate::aiworld::FleePointReached>),
    (stances, settings, mut grab_inhibit, mut inhibited, powers, mut last_swing): (
        Res<crate::script_world::SpawnerOverrides>,
        Res<crate::settings::Settings>,
        Local<f32>,
        Local<std::collections::HashMap<String, (f32, Entity)>>,
        Res<crate::powers::Powers>,
        Local<f32>,
    ),
    (mut grenades, mut bottles, mut whiskey_fx, mut throws, mut spells): (MessageWriter<crate::gadgets::NpcGrenade>, MessageWriter<crate::whiskey::NpcBottle>, MessageWriter<crate::particles::SpawnEffect>, MessageWriter<crate::thrown::NpcThrow>, MessageWriter<crate::witch::WitchSpell>),
    mut looked_over: Local<Vec<Vec3>>,
    (finishing, mut finishers, mut tallboy_shots, devices, mut patrols, mut handled, mut crumbs): (
        Query<(), With<FinisherClip>>,
        MessageWriter<NpcFinisher>,
        MessageWriter<crate::tallboy::TallboyShot>,
        Res<crate::security::Devices>,
        ResMut<crate::patrol::Patrols>,
        ResMut<HandledBodies>,
        ResMut<crate::crumbs::Crumbs>,
    ),
) {
    let (dt_world, dt_own) = (time.delta_secs() * tc.world_scale(), time.delta_secs() * tc.own_scale());
    // (the spots searchers looked over last frame, and those this frame)
    let spots_looked_over = std::mem::take(&mut *looked_over);
    let mut looked_now: Vec<Vec3> = Vec::new();
    let max_attackers = INNER_SKIRMISH_CAPS[(settings.difficulty as usize).min(3)];
    if dt_world <= 0.0 && (dt_own <= 0.0 || !npcs.iter().any(|(_, n, _)| n.out_of_bend)) {
        return;
    }
    let Ok((pe, pt, psword, pl)) = player.single() else { return };
    let ppos = pt.translation;
    let space_log = std::env::var("DH_SPACE_LOG").is_ok();
    // (his feet: his capsule's centre above them, standing or crouched)
    let pfeet = ppos.y - pl.body().depth();
    let blocking = psword.is_some_and(|s| s.blocking);
    // (Corvo's swing just begun: once a blow)
    let swing_began = psword.is_some_and(|s| s.swing > 0.0 && !s.hit_done && (*last_swing <= 0.0 || s.swing < *last_swing));
    *last_swing = psword.map_or(0.0, |s| s.swing);
    let Ok(ctx) = rapier.single() else { return };
    // the others, for feuds between factions
    let others: Vec<(Entity, Vec3, String, bool, u32, f32)> = npcs.iter().map(|(e, n, t)| (e, t.translation, n.faction.clone(), n.is_down() || n.kind == Kind::Story, n.spawner, n.health)).collect();
    // (the bodies lying about, for a searcher done looking one over)
    let downed: Vec<(Entity, Vec3, String)> = npcs.iter().filter(|(_, n, _)| n.is_down() && !n.corpse).map(|(e, n, t)| (e, t.translation, n.faction.clone())).collect();
    let factions = level.as_ref().map(|l| &l.scene.factions);
    let wall_filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    // NPCs that just entered combat alert their neighbours
    let mut alerts: Vec<(Vec3, Vec3)> = Vec::new();
    // attack tokens: swings already in progress count against the limit
    let mut attackers = npcs.iter().filter(|(_, n, _)| n.attack_t.is_some()).count();
    // the ring at Corvo: of those fighting him, as many as the cap, the nearest; the others wait
    // outside it
    let ring: Vec<Entity> = {
        let mut fighting: Vec<(Entity, f32)> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Combat && n.hostile() && !n.is_down()).map(|(e, _, t)| (e, t.translation.distance_squared(ppos))).collect();
        fighting.sort_by(|a, b| a.1.total_cmp(&b.1));
        fighting.into_iter().take(max_attackers).map(|(e, _)| e).collect()
    };
    // one weeper at a time grabs Corvo
    let grab_active = npcs.iter().any(|(_, n, _)| n.grab.is_some());
    // (the weepers running after Corvo)
    let mut weep_runners = npcs.iter().filter(|(_, n, _)| n.weep_run && !n.is_down() && n.mode == Mode::Combat).count();
    // (after any weeper's grab, none for a while: `DisGlobalCombatManager.m_fInhibitionTimeWeeperArmGrab`)
    *grab_inhibit = (*grab_inhibit - dt_world).max(0.0);
    // (the moves another has just used on Corvo, how long yet, and by whom)
    inhibited.retain(|_, (left, _)| {
        *left -= dt_world;
        *left > 0.0
    });
    for (e, mut npc, t) in &mut npcs {
        let pos = t.translation;
        // (its time: the world's, stopped, or its own)
        let dt = if npc.out_of_bend { dt_own } else { dt_world };
        if dt <= 0.0 {
            continue;
        }
        // (playing a finisher on a foe: held to it)
        if finishing.contains(e) {
            npc.target = None;
            npc.attack_t = None;
            continue;
        }
        npc.mode_timer += dt;
        npc.attack_cd = (npc.attack_cd - dt).max(0.0);
        npc.check_cd = (npc.check_cd - dt).max(0.0);
        npc.reacting = (npc.reacting - dt).max(0.0);
        if !matches!(npc.mode, Mode::Search | Mode::Investigate) {
            npc.scripted_search = false;
        }
        for c in npc.leap_cd.iter_mut() {
            *c = (*c - dt).max(0.0);
        }
        npc.stagger = (npc.stagger - dt).max(0.0);
        npc.hit_react = (npc.hit_react - dt).max(0.0);
        // a sleep dart's drug taking hold
        if let Some(t) = npc.sleep_in.as_mut() {
            *t -= dt;
            if *t <= 0.0 {
                npc.sleep_in = None;
                if !npc.is_down() {
                    npc.set_mode(Mode::Unconscious);
                }
            }
        }
        if npc.is_down() || npc.mode == Mode::Choked || npc.floored {
            npc.target = None;
            continue;
        }
        // its blade locked with Corvo's: it holds
        if npc.versus {
            continue;
        }
        // regeneration (`m_HealthRegen*`): a while after its last hurt, back up to its limit
        npc.hurt_t += dt;
        let limit = npc.arms.regen[2].min(npc.max_health);
        if npc.hurt_t > npc.arms.regen[0] && npc.health < limit {
            npc.health = (npc.health + npc.arms.regen[1] * dt).min(limit);
        }
        // robbed: a while later it misses what was taken (`DisBehaviorStolen`, in its reaction
        // slot: given up to anything setting it on edge), checks its pockets and says so
        // (`ROBBED`: "Hey, I got snaked.")
        if let Some(left) = npc.robbed {
            if npc.alert != Alert::Unaware || npc.mode == Mode::Combat {
                npc.robbed = None;
            } else if left - dt <= 0.0 {
                npc.robbed = None;
                npc.gesture(14);
                npc.bark_req = (npc.bark_req.0.wrapping_add(1), "ROBBED");
                info!("{} misses what was taken", npc.name);
            } else {
                npc.robbed = Some(left - dt);
            }
        }
        // a live explosive by it (`DisBehaviorEscapeExplosion`, a reflex over any fight): it
        // makes off, as far from it as its clearance
        if let Some((at, _)) = npc.escape {
            let away = (pos - at).with_y(0.0).normalize_or(-npc.forward());
            // (clear of it already: it holds there)
            npc.target = (pos.distance(at) < ESCAPE_CLEARANCE).then(|| at + away * ESCAPE_CLEARANCE);
            npc.attack_t = None;
            npc.aim = None;
            continue;
        }
        // in a plague carrier's cloud: out of it (`DisBehaviorEscapePlague`)
        if let Some((to, _)) = npc.plague_escape {
            npc.target = (pos.with_y(0.0).distance(to.with_y(0.0)) > 0.4).then_some(to);
            npc.attack_t = None;
            continue;
        }
        // feuds: an enemy faction's NPC in sight gets fought (Corvo comes first)
        if npc.mode != Mode::Combat && npc.kind != Kind::Story {
            npc.feud_scan -= dt;
            let foe_alive = npc.foe.and_then(|f| others.iter().find(|o| o.0 == f)).filter(|o| !o.3 && o.1.distance(pos) < 25.0);
            if foe_alive.is_none() {
                npc.foe = None;
            }
            if npc.foe.is_none() && npc.feud_scan <= 0.0 {
                npc.feud_scan = 0.6;
                let no_enemies = Vec::new();
                let enemies = factions.and_then(|f| f.get(&npc.faction)).unwrap_or(&no_enemies);
                // (the scripts' stances first: `DisSeqAct_SetDisposition`)
                let (me, my_faction) = (npc.spawner, npc.faction.clone());
                let foe_of = |o: &(Entity, Vec3, String, bool, u32, f32)| {
                    let them = [crate::script_world::Party::Npc(o.4), crate::script_world::Party::faction(&o.2)];
                    stances.hostile(me, &my_faction, &them, enemies.contains(&o.2))
                };
                npc.foe = others
                    .iter()
                    .filter(|o| o.0 != e && !o.3 && o.1.distance(pos) < 12.0 && foe_of(o))
                    .filter(|o| {
                        let to = o.1 - pos;
                        let d = to.length();
                        ctx.cast_ray(pos + Vec3::Y * 0.6, to / d.max(1e-3), d, true, wall_filter).is_none()
                    })
                    .min_by(|a, b| a.1.distance(pos).total_cmp(&b.1.distance(pos)))
                    .map(|o| o.0);
                if let Some(f) = npc.foe.and_then(|f| others.iter().find(|o| o.0 == f)) {
                    debug!("feud: {} ({}) attacks a {} {:.1} m away", npc.name, npc.faction, f.2, f.1.distance(pos));
                }
            }
            if let Some(fpos) = npc.foe.and_then(|f| others.iter().find(|o| o.0 == f)).map(|o| o.1) {
                let to = (fpos - pos).with_y(0.0);
                let d = to.length();
                npc.target = if d > 1.5 { Some(fpos) } else { None };
                if d > 0.1 {
                    npc.yaw = (-to.x).atan2(-to.z);
                }
                if let Some(t_att) = npc.attack_t {
                    let t_new = t_att + dt;
                    if t_att < 0.55 && t_new >= 0.55 && d < 2.3 {
                        let damage = npc.arms.melee;
                        npc_hits.write(NpcHit { npc: npc.foe.unwrap(), damage, kind: HitKind::ByOthers, from: pos });
                        noise.write(Noise { corvo: false, pos, radius: 8.0, kind: NoiseKind::Threatening });
                    }
                    npc.attack_t = if t_new > 0.95 { None } else { Some(t_new) };
                } else if d < 2.0 && npc.attack_cd <= 0.0 && npc.stagger <= 0.0 {
                    // a blow that kills: the sword's NPC fatality (`DisTweaks_NPCFatality`,
                    // `m_fChanceOfFatality` 1 on every NPC sword, against NPCs)
                    let foe_health = npc.foe.and_then(|f| others.iter().find(|o| o.0 == f)).map(|o| o.5).unwrap_or(f32::MAX);
                    if npc.has_sword && foe_health <= npc.arms.melee {
                        finishers.write(NpcFinisher { killer: e, victim: npc.foe.unwrap(), stomp: false, damage: npc.arms.melee });
                        npc.attack_cd = 3.0;
                    } else if npc.has_sword && rand::random::<f32>() < 0.3 {
                        // (now and then the stomp, `DisTweaks_NPCStomp`, among its blows)
                        finishers.write(NpcFinisher { killer: e, victim: npc.foe.unwrap(), stomp: true, damage: npc.arms.melee });
                        npc.attack_cd = 2.0;
                    } else {
                        npc.attack_t = Some(0.0);
                        npc.attack_cd = 1.2 + rand::random::<f32>() * 0.8;
                    }
                }
                continue;
            }
        }
        // Corvo in its way, not an enemy (`DisAISubProcessPersonalSpace`): it stops, turns to him
        // and warns him off (`PATROL_PERSONAL_SPACE_WARNING`); still in its way as the warning
        // runs out, a shove (`DisTweaks_NPCPush`) with its "personal space" bark
        npc.warn_cd = (npc.warn_cd - dt).max(0.0);
        npc.push_cd = (npc.push_cd - dt).max(0.0);
        if !npc.enemy && npc.alert != Alert::Combat && npc.has_move("Push") && npc.attack_t.is_none() && matches!(npc.mode, Mode::Idle | Mode::Patrol | Mode::Return | Mode::Search) {
            let to = (ppos - pos).with_y(0.0);
            let level = (ppos.y - pos.y).abs() < 1.2;
            let close = to.length() < 1.1 && level && to.normalize_or_zero().dot(npc.forward()) > 0.3;
            match npc.warn {
                None if close && npc.warn_cd <= 0.0 => {
                    if space_log {
                        info!("space: {} warns", npc.name);
                    }
                    npc.warn = Some(SPACE_WARN);
                    npc.bark_req = (npc.bark_req.0.wrapping_add(1), "PATROL_PERSONAL_SPACE_WARNING");
                }
                // (out of its space: over)
                Some(_) if !(level && to.length() < SPACE_LEFT) => {
                    if space_log {
                        info!("space: {} lets it go ({:.1} m)", npc.name, to.length());
                    }
                    npc.warn = None;
                    npc.warn_cd = SPACE_WARN_CD;
                }
                Some(left) if left > dt => npc.warn = Some(left - dt),
                Some(_) => {
                    npc.warn = None;
                    npc.warn_cd = SPACE_WARN_CD;
                    if space_log {
                        info!("space: {} warned ({:.1} m)", npc.name, to.length());
                    }
                    if to.length() < SPACE_PUSH && npc.push_cd <= 0.0 {
                        npc.push_cd = SPACE_PUSH_CD;
                        npc.gesture(3);
                        npc.bark_req = (npc.bark_req.0.wrapping_add(1), "PATROL_PERSONAL_SPACE");
                        hit_writer.write(PlayerHit { npc: e, from: pos, damage: 0.0, kick: false, big: false, push: true, unblockable: false, missile: false });
                    }
                }
                None => {}
            }
        } else {
            npc.warn = None;
        }
        // on its way to ring an alarm bell: nothing else (the bell's slot, Alert, is above
        // Combat's)
        // a comrade's call for help against rats answered (its rat-swarm combat, outranked by a
        // fight with Corvo): to the rat on it, the stamp coming as it's in reach
        if let Some((_, at)) = npc.rat_help.filter(|_| npc.mode != Mode::Combat) {
            npc.target = Some(at);
            continue;
        }
        if let Some(bell) = npc.alarm_run {
            npc.target = Some(bell);
            npc.attack_t = None;
            continue;
        }
        // what caught its eye (the turn-to-face level): it stops and turns to look
        if npc.attn_level == 2 && npc.alert == Alert::Unaware && matches!(npc.mode, Mode::Idle | Mode::Patrol | Mode::Return) {
            npc.target = None;
            npc.track_pause = None;
            continue;
        }
        // warning Corvo off: it stands
        if npc.warn.is_some() {
            npc.target = None;
            continue;
        }
        // on its walk with what it head-tracks behind it (within `m_fRearHalfAngleWidthToPause_Deg`
        // of its back): it stops and turns a quarter its way, the heading it walked kept, till
        // that is within `m_fFrontHalfAngleWidthToResume_Deg` of the heading, or gone
        // (`DisAISubProcessHeadTrackPause`, `TweaksBehavior_Patrol_Default`'s; its workings as
        // roshan's r614/blink research decoded them)
        let yaw_to = |p: Vec3| {
            let d = p - pos;
            (-d.x).atan2(-d.z)
        };
        let wrap = |a: f32| (a + PI).rem_euclid(TAU) - PI;
        let paused = npc.track_pause.is_some();
        let proxy = npc.last_seen.filter(|_| npc.attn_level == 1 && npc.alert == Alert::Unaware && matches!(npc.mode, Mode::Patrol | Mode::Return));
        npc.track_pause = match (proxy, npc.track_pause) {
            (Some(p), Some(h)) => (wrap(yaw_to(p) - h).abs() >= PAUSE_RESUME.to_radians()).then_some(h),
            // (as far off where it heads as it must come back within to walk on: not on a turn
            // towards it)
            (Some(p), None) => npc
                .target
                .filter(|t| t.with_y(0.0).distance(pos.with_y(0.0)) > 0.3)
                .filter(|_| wrap(yaw_to(p) - (npc.yaw + PI)).abs() <= PAUSE_REAR.to_radians())
                .map(yaw_to)
                .filter(|&h| wrap(yaw_to(p) - h).abs() >= PAUSE_RESUME.to_radians()),
            (None, _) => None,
        };
        if space_log && npc.track_pause.is_some() != paused {
            info!("pause: {} {} (proxy {:?}, attention {})", npc.name, if paused { "walks on" } else { "stops" }, proxy, npc.attn_level);
        }
        if npc.track_pause.is_some() {
            npc.target = None;
            continue;
        }
        // (begging is a panic's: out of it, it's over)
        if npc.mode != Mode::Flee {
            npc.caught = None;
            npc.rising = 0.0;
        }
        match npc.mode {
            Mode::Idle => {
                // (stopped short with its stop's clip, it is there)
                let away = pos.distance(npc.home);
                npc.target = if away > 0.6 && !(npc.halted && away < UNACCURACY) { Some(npc.home) } else { None };
                if npc.target.is_none() {
                    // occasionally glance around
                    npc.look = (npc.mode_timer * 0.4).sin() * 0.6;
                }
            }
            Mode::Patrol => {
                // its route walked as the original walks it (`patrol`); stopped short with its
                // stop's clip, it is there
                let Some(level) = level.as_ref() else { continue };
                if !npc.patrol.on {
                    npc.set_mode(Mode::Idle);
                    continue;
                }
                let squad = level.scene.spawners.get(npc.spawner as usize).map(|s| s.squad.as_str()).unwrap_or("");
                let mut patrol = std::mem::take(&mut npc.patrol);
                let at = if npc.halted { patrol.point(&level.scene.routes).filter(|g| (*g - pos).with_y(0.0).length() < UNACCURACY).unwrap_or(pos) } else { pos };
                let was = (patrol.route, patrol.idx, patrol.guard.is_some());
                let order = patrol.step(&level.scene.routes, &mut patrols, squad, at, dt);
                if std::env::var("DH_PATROL_LOG").is_ok() && was != (patrol.route, patrol.idx, patrol.guard.is_some()) {
                    let route = patrol.route.and_then(|r| level.scene.routes.get(r)).map_or("none", |r| r.name.as_str());
                    info!("patrol: {} ({squad}) {route} point {}{}", npc.name, patrol.idx, if patrol.guard.is_some() { ", guarding" } else { "" });
                }
                npc.patrol = patrol;
                if order.goal != npc.target {
                    // (on to the next: not there till it gets there)
                    npc.halted = false;
                }
                npc.target = order.goal;
            }
            Mode::Investigate => {
                let goal = npc.target.or(npc.last_seen).unwrap_or(npc.home);
                npc.target = Some(goal);
                // another searcher has looked this very spot over (`SearchReachedProxy`, to
                // those within 20 m of it; the two spots within 2.5 m on every axis): it gives
                // up its own look, stands a moment and searches on, a body it went to handled
                // all the same (0x00AFE1C0, 0x00B2E180, decoded for this rewrite)
                if spots_looked_over.iter().any(|r| r.with_y(0.0).distance(pos.with_y(0.0)) < 20.0 && (*r - goal).abs().max_element() < 2.5) {
                    if let Some(b) = npc.checking.take() {
                        handled.0.insert(b);
                    }
                    npc.body_check = false;
                    npc.hold = Some((0.5 + rand::random::<f32>() * 0.5, false));
                    npc.set_mode(Mode::Search);
                    continue;
                }
                // (its search's reaction first: turned to it, a moment)
                if npc.mode_timer < npc.react && pos.distance(goal) > 2.5 {
                    npc.target = None;
                    let to = (goal - pos).with_y(0.0);
                    if to.length() > 0.1 {
                        let want = (-to.x).atan2(-to.z);
                        let turn = (want - npc.yaw + PI).rem_euclid(TAU) - PI;
                        npc.yaw += turn.clamp(-4.0 * dt, 4.0 * dt);
                    }
                    continue;
                }
                // (come to it within 2.5 m, `m_fProximityThresholds`' 250 uu; to a body closer,
                // its 100 uu taken as 1.6 m, a body lying in the way)
                let reach = if npc.body_check { 1.6 } else { 2.5 };
                if pos.distance(goal) < reach || npc.mode_timer > 14.0 || npc.stuck > 1.5 {
                    // (not come to it, 0x00AFEB00: where Corvo is out of reach more than 5 m
                    // above, it stares up there, `StareAtUnreachable`, 5 s at most; else it
                    // stands a moment, 0.5-1 s; then it searches on)
                    if pos.distance(goal) >= reach {
                        npc.hold = Some(if goal.y - pos.y > 5.0 { (5.0, true) } else { (0.5 + rand::random::<f32>() * 0.5, false) });
                    } else if npc.check_cd <= 0.0 {
                        // (come to it, it looks it over, its check clip's 10 s cooldown
                        // permitting, and tells those about)
                        npc.check_cd = 10.0;
                        looked_now.push(goal);
                    }
                    // (come to a body it found: it looks it over, `Sword_CorpseCheck`, and from then
                    // on the others pass it by, `m_bCorpseHandled`)
                    if std::mem::take(&mut npc.body_check) && pos.distance(goal) < reach {
                        npc.gesture(19);
                        if let Some(b) = npc.checking.take() {
                            handled.0.insert(b);
                            // (and every other body of its side it sees then, 0x00B3B6E0: here
                            // those within the dark's sight, 10.8 m, with a clear line)
                            let eye = pos + Vec3::Y * 0.72;
                            for o in downed.iter().filter(|o| o.0 != b && o.1.distance(eye) < 10.8) {
                                let ally = relation(level.as_ref().map(|l| &l.scene), &npc.faction, &o.2) == 1;
                                let dir = (o.1 + Vec3::Y * 0.2 - eye).normalize_or_zero();
                                if ally && ctx.cast_ray(eye, dir, o.1.distance(eye) - 0.3, true, wall_filter).is_none() {
                                    handled.0.insert(o.0);
                                }
                            }
                        }
                    }
                    npc.set_mode(Mode::Search);
                }
            }
            Mode::Search => {
                // the search lasts while its attention holds above the Investigate stop (out
                // of sight it leaks: about 12 s unsuspecting, half a minute and more suspecting,
                // as roshan's r614/blink research measured); it has no timer of its own, its
                // crumbs never running out
                let given_up = match level.as_ref().and_then(|l| attention_of(&npc, &l.scene)) {
                    Some(a) => npc.mode_timer > 3.0 && npc.awareness < a.levels[2][1],
                    None => npc.mode_timer > 10.0 + (npc.alert == Alert::Combat) as u8 as f32 * 6.0,
                };
                if given_up {
                    npc.alert = Alert::Unaware;
                    npc.awareness = npc.awareness.min(0.2);
                    npc.saw_body = false;
                    npc.hold = None;
                    npc.set_mode(Mode::Return);
                } else if let Some((left, stare)) = npc.hold {
                    // (an investigation it couldn't finish: standing a moment, or staring up at
                    // where Corvo is out of reach, its time counted while he is unseen and
                    // ended once he is 5 m from there, 0x00AF4C40 / 0x00AF4440)
                    npc.target = None;
                    let at = npc.last_seen.unwrap_or(pos);
                    let to = (at - pos).with_y(0.0);
                    if stare && to.length() > 0.1 {
                        let want = (-to.x).atan2(-to.z);
                        let turn = (want - npc.yaw + PI).rem_euclid(TAU) - PI;
                        npc.yaw += turn.clamp(-4.0 * dt, 4.0 * dt);
                    }
                    let left = if stare && npc.sees_player { left } else { left - dt };
                    let gone = stare && ppos.distance(at) > 5.0;
                    npc.hold = (left > 0.0 && !gone).then_some((left, stare));
                } else if npc.target.map(|g| pos.distance(g) < 1.5).unwrap_or(true) || npc.stuck > 1.0 {
                    // tracking him (`TrackTarget`, 0x00B2EB20): on to a crumb Corvo's search
                    // proxy laid that no other searcher has taken, within its tether; with none,
                    // 5-10 m back the way of its post. A way that fails: the first of 8
                    // directions 45 degrees apart (from a random one) open for 2 m, up to 10 m
                    // (a point within reach of a step isn't one: it would be come to at once)
                    let open = |dir: Vec3, len: f32| {
                        let reach = ctx.cast_ray(pos, dir, len, true, wall_filter).map_or(len, |(_, toi)| toi - 0.5);
                        (reach > 2.0).then(|| pos + dir * reach)
                    };
                    let around = || {
                        let start = rand::random::<f32>() * TAU;
                        (0..8).find_map(|k| open(Quat::from_rotation_y(start + k as f32 * PI / 4.0) * Vec3::NEG_Z, 10.0))
                    };
                    let next = if npc.stuck > 1.0 {
                        around()
                    } else {
                        let tether = npc.tether.clone();
                        crumbs.next(|p| p.distance(pos) > 2.0 && ai.within_tether(&tether, p)).or_else(|| {
                            let home = (npc.home - pos).with_y(0.0).normalize_or_zero();
                            let way = if home == Vec3::ZERO { Quat::from_rotation_y(rand::random::<f32>() * TAU) * Vec3::NEG_Z } else { home };
                            (0..4).find_map(|_| open(Quat::from_rotation_y((rand::random::<f32>() * 2.0 - 1.0) * PI / 3.0) * way, 5.0 + rand::random::<f32>() * 5.0)).or_else(around)
                        })
                    };
                    if std::env::var_os("DH_SEARCH_LOG").is_some() {
                        info!("{} searches on to {next:.1?}", npc.name);
                    }
                    npc.target = next;
                    npc.stuck = 0.0;
                }
            }
            Mode::Return => {
                // back to its patrol where it left it, else home
                let on_patrol = npc.patrol.on;
                let goal = level.as_ref().and_then(|l| npc.patrol.point(&l.scene.routes)).filter(|_| on_patrol).unwrap_or(npc.home);
                npc.target = Some(goal);
                if pos.distance(goal) < 1.0 || (on_patrol && npc.patrol.route.is_none() && npc.patrol.first.is_none()) {
                    npc.set_mode(if on_patrol { Mode::Patrol } else { Mode::Idle });
                }
            }
            Mode::Flee => {
                let from = npc.last_seen.unwrap_or(ppos);
                // a fighter that panicked (its last survivor rule) runs heedless a moment
                // (`m_fFreeFlightTimer`); then, Corvo upon it (within its
                // `m_fCowerReactionDistance`), it turns and fights (`EReaction_Fight`)
                if npc.fighter() && npc.panic_fight > 0.0 {
                    if std::env::var("DH_NPC_LOG").is_ok() && (npc.mode_timer * 2.0).fract() < dt * 2.0 {
                        info!("{}: fleeing {:.1} s at {pos:.1}, {:.1} m from Corvo", npc.name, npc.mode_timer, (ppos - pos).length());
                    }
                    npc.free_flight = (npc.free_flight - dt).max(0.0);
                    if npc.free_flight <= 0.0 && (ppos - pos).length() < npc.panic_fight && npc.sees_player {
                        npc.panic_fight = 0.0;
                        npc.last_seen = Some(ppos);
                        npc.set_mode(Mode::Combat);
                        continue;
                    }
                }
                // caught: Corvo upon it (within the panic's `m_fCowerReactionDistance`, 350 uu),
                // a civilian gives up running and begs where it is, facing him
                // (`m_eCaughtReaction`: `EReaction_Beg`), its cry for help heard 15 m; once he
                // has backed off past the reach a moment, it gets up and runs on
                if !npc.fighter() {
                    let near = (ppos - pos).length() < BEG_REACH;
                    match npc.caught {
                        None if npc.rising <= 0.0 && near && (npc.sees_player || (ppos - pos).length() < 2.0) => {
                            npc.caught = Some((0.0, 0.0));
                            noise.write(Noise { corvo: false, pos, radius: 15.0, kind: NoiseKind::Help });
                        }
                        Some((t, away)) => {
                            let away = if (ppos - pos).length() > BEG_REACH + 1.5 { away + dt } else { 0.0 };
                            if away > 1.0 {
                                npc.caught = None;
                                npc.rising = BEG_RISE;
                                npc.flee_to = None;
                            } else {
                                npc.caught = Some((t + dt, away));
                            }
                        }
                        _ => {}
                    }
                    if npc.caught.is_some() || npc.rising > 0.0 {
                        npc.rising = (npc.rising - dt).max(0.0);
                        npc.target = None;
                        npc.mode_timer = 0.0;
                        let to = (ppos - pos).with_y(0.0);
                        if npc.caught.is_some() && to.length() > 0.1 {
                            let want = (-to.x).atan2(-to.z);
                            let turn = (want - npc.yaw + PI).rem_euclid(TAU) - PI;
                            npc.yaw += turn.clamp(-4.0 * dt, 4.0 * dt);
                        }
                        continue;
                    }
                }
                // a civilian starting to run cries for help: a 15 m call the guards hear (the
                // panic's help bark at `Loudness2`)
                if npc.mode_timer < dt * 1.5 && !npc.fighter() {
                    noise.write(Noise { corvo: false, pos, radius: 15.0, kind: NoiseKind::Help });
                }
                // its squad's flee point away from the danger, and on along their chain
                if npc.flee_to.is_none() && npc.mode_timer < dt * 1.5 {
                    npc.flee_to = ai.flee_target(&npc.squad, pos, from);
                }
                match npc.flee_to.and_then(|i| ai.flee_point(i).map(|p| (i, p.clone()))) {
                    Some((_, p)) if pos.distance(Vec3::from(p.position)) < 1.5 => {
                        fled.write(crate::aiworld::FleePointReached { point: p.name.clone(), spawner: npc.spawner });
                        npc.flee_to = p.next.map(|n| n as usize);
                        if npc.flee_to.is_none() {
                            // there: cowering a while, then back
                            npc.target = None;
                            npc.yaw = p.yaw;
                            if npc.mode_timer > 20.0 {
                                npc.set_mode(Mode::Return);
                            }
                        } else {
                            npc.mode_timer = 0.0;
                        }
                    }
                    Some((_, p)) => {
                        npc.target = Some(Vec3::from(p.position));
                        if npc.stuck > 3.0 {
                            npc.flee_to = None;
                        }
                    }
                    None => {
                        let away = (pos - from).with_y(0.0).normalize_or(Vec3::X);
                        npc.target = Some(pos + away * 6.0);
                        if npc.mode_timer > 12.0 {
                            npc.set_mode(Mode::Return);
                        }
                    }
                }
            }
            Mode::Combat => {
                pace_tick(&mut npc, dt);
                if stats.dead {
                    npc.set_mode(Mode::Return);
                    npc.alert = Alert::Suspicious;
                    continue;
                }
                if npc.mode_timer < dt * 1.5 {
                    alerts.push((pos, ppos));
                }
                let d = pos.distance(ppos);
                // casting its Attract spell (a whaler's pull, `assassin.rs`, which faces it to
                // Corvo): it stands for it, its blade and wrist bow waiting
                if npc.casting {
                    npc.target = None;
                    continue;
                }
                // lying in wait (`DisTweaks_NPCAmbush`): Corvo gone from its sight close by, it
                // holds round the corner (`Sword_Ambush_In` / `_Loop`) and springs at him
                // (`_Out`) as he comes within the context's reach, or gives it up (`_Cancel`)
                npc.ambush_cd -= dt;
                if let Some(w) = npc.ambush {
                    npc.ambush = Some(w + dt);
                    npc.target = None;
                    let reach = npc.arms.moves.iter().find(|m| m.kind == "Ambush").map(|m| m.range[1]).unwrap_or(2.5);
                    if npc.stagger > 0.0 || npc.hurt_t < 0.2 {
                        npc.ambush = None;
                        npc.ambush_cd = AMBUSH_CD;
                    } else if d < reach + 0.5 && (ppos.y - pos.y).abs() < 1.5 {
                        npc.ambush = None;
                        npc.ambush_cd = AMBUSH_CD;
                        let to = (ppos - pos).with_y(0.0);
                        npc.yaw = (-to.x).atan2(-to.z);
                        npc.gesture(10);
                        npc.attack_t = Some(0.0);
                        npc.swing = Some(Swing { kind: MoveKind::Short, big: true, len: 1.0, hit: 0.35, reach: reach + 0.8, lunge: 3.0, no_versus: false, more: Vec::new() });
                        info!("ambush: {} springs at {d:.1} m", npc.name);
                    } else if w > AMBUSH_WAIT || d > 10.0 {
                        npc.ambush = None;
                        npc.ambush_cd = AMBUSH_CD;
                        npc.gesture(11);
                    } else {
                        continue;
                    }
                } else if !npc.sees_player && npc.ambush_cd <= 0.0 && npc.attack_t.is_none() && npc.stagger <= 0.0 && npc.mode_timer > 1.8 && d < 7.0 && npc.has_move("Ambush") {
                    npc.ambush = Some(0.0);
                    npc.gesture(9);
                    info!("ambush: {} lies in wait ({d:.1} m)", npc.name);
                    continue;
                }
                // Corvo hiding in a hideout close by (under a table, a bed: `DisHideoutVolume`): it
                // comes to its way in and strikes under it (`DisTweaks_NPCAttackUnder`:
                // `Sword_Attack_UnderTable`, which can't be blocked or dodged; its tweak sets no
                // cooldown, so a 1-2 s pause between) or, its pistol's context run
                // (`DisTweaks_NPCAttackUnder_Gun`, every 10-20 s), shoots under it
                // (`Sword_Gun_AttackUnderTable`)
                for c in npc.under_cd.iter_mut() {
                    *c -= dt;
                }
                let can_under = (npc.has_sword || npc.ranged == 1) && npc.brain_flags & BRAIN_DONT_ATTACK == 0 && npc.attack_t.is_none();
                // (seen there, or last seen beside it: he went under)
                let knows = npc.sees_player || npc.last_seen.is_some_and(|ls| ls.distance(ppos) < 2.5);
                if let Some((way_in, _)) = ai.hideout_access(ppos).filter(|_| can_under && knows) {
                    let flat = (ppos - pos).with_y(0.0).length();
                    // (at the way in, or beside him: the blow reaches under to him)
                    if (flat < 1.9 || pos.with_y(0.0).distance(way_in.with_y(0.0)) < 0.9 && flat < 4.0) && (ppos.y - pos.y).abs() < 1.5 {
                        // (beside him: it keeps hold of him there, unseen)
                        npc.mode_timer = npc.mode_timer.min(1.0);
                        npc.last_seen = Some(ppos);
                        let gun = npc.ranged == 1 && npc.under_cd[1] <= 0.0;
                        if npc.stagger <= 0.0 && (gun || npc.has_sword && npc.under_cd[0] <= 0.0) {
                            let kind = if gun { MoveKind::UnderGun } else { MoveKind::Under };
                            if gun {
                                npc.under_cd[1] = 10.0 + rand::random::<f32>() * 10.0;
                            }
                            npc.attack_t = Some(0.0);
                            npc.swing = Some(Swing { kind, big: false, len: if gun { 4.03 } else { 2.7 }, hit: if gun { 1.39 } else { 0.95 }, reach: flat.max(2.0) + 0.5, lunge: 0.0, no_versus: true, more: Vec::new() });
                            info!("{} strikes under the hideout ({kind:?}, {flat:.1} m)", npc.name);
                        }
                        npc.target = None;
                        let to = (ppos - pos).with_y(0.0);
                        if to.length() > 0.1 {
                            npc.yaw = (-to.x).atan2(-to.z);
                        }
                        continue;
                    }
                    if npc.attack_t.is_none() {
                        npc.target = Some(way_in);
                        continue;
                    }
                }
                if npc.sees_player {
                    npc.target = Some(ppos);
                    // outside the ring of those at him (no blow of its own coming), it keeps its
                    // outer pattern's place: its ideal reach, off to its flank, a side each
                    // (`m_pIdealRangeSource`, `m_fFlankAngleDegrees`: our reading of how it
                    // skirmishes)
                    match npc.arms.outer_place.filter(|_| !ring.contains(&e) && npc.attack_t.is_none()) {
                        Some((reach, flank)) => {
                            let side = if npc.spawner % 2 == 0 { 1.0 } else { -1.0 };
                            if npc.flank.is_none() && std::env::var("DH_MELEE_LOG").is_ok() {
                                info!("melee: {} waits outside the ring, {reach:.1} m off, {:.0} degrees to its flank", npc.name, side * flank);
                            }
                            let bearing = *npc.flank.get_or_insert_with(|| {
                                let from = (pos - ppos).with_y(0.0);
                                from.x.atan2(from.z) + side * flank.to_radians()
                            });
                            npc.target = Some(ppos.with_y(pos.y) + Vec3::new(bearing.sin(), 0.0, bearing.cos()) * reach);
                        }
                        None => npc.flank = None,
                    }
                    // a wolfhound with Corvo out of its reach (above it or below) keeps where it can
                    // bark at him, just past its bark's least reach (our reading of its
                    // `DisNavMeshGoal_WolfhoundBarkPosition`)
                    if let Some(tc) = npc.arms.taunt.as_ref().filter(|_| is_hound(&npc) && (ppos.y - pos.y).abs() > 1.6) {
                        let away = (pos - ppos).with_y(0.0).normalize_or(-npc.forward());
                        npc.target = Some(ppos.with_y(pos.y) + away * (tc.range[0] + 1.5));
                    }
                    npc.trail_idx = trail.points.len();
                    npc.mode_timer = npc.mode_timer.min(1.0);
                } else if let Some(ls) = npc.last_seen {
                    // reached the last known position: follow the player's trail
                    if pos.distance(ls) < 1.5 {
                        let filter = QueryFilter::default().exclude_collider(e).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                        let newest_visible = trail.points.iter().enumerate().rev().find(|(_, p)| {
                            let to = **p - pos;
                            let dl = to.length();
                            dl < 18.0 && ctx.cast_ray(pos, to / dl.max(1e-3), dl, true, filter).is_none()
                        });
                        match newest_visible {
                            Some((i, p)) if i + 1 > npc.trail_idx.min(trail.points.len()) || p.distance(ls) > 1.0 => {
                                npc.last_seen = Some(*p);
                                npc.trail_idx = i + 1;
                            }
                            _ => {
                                npc.set_mode(Mode::Search);
                                continue;
                            }
                        }
                    }
                    npc.target = npc.last_seen;
                }
                // out of sight its attention leaks; below Busted's stop it has lost him
                // (`CombatToSearch`, as roshan's r614/blink research decoded it)
                let lost = !npc.sees_player && npc.mode_timer > 2.0 && level.as_ref().and_then(|l| attention_of(&npc, &l.scene)).is_some_and(|a| npc.awareness < a.levels[3][1]);
                if lost || npc.mode_timer > 20.0 {
                    npc.set_mode(Mode::Search);
                    continue;
                }
                // leaping back from Corvo after a grenade
                if let Some((to, left)) = npc.retreat {
                    npc.retreat = (left > dt).then_some((to, left - dt));
                    npc.target = Some(to);
                    continue;
                }
                // grenades (an overseer's grenade weapon): lobbed at Corvo
                // (`DisTweaks_NPCLobGrenadeAtUnreachable`), tossed leaping back from him close
                // (`DisTweaks_NPC_OverseerJumpAway`), dropped on him below
                // (`DisTweaks_NPCAttackUnder_Grenade`), each at its reach and cooldown
                if !npc.arms.grenades.is_empty() && npc.may_shoot() {
                    let throws = npc.arms.grenades.clone();
                    if npc.grenade_cd.len() != throws.len() {
                        npc.grenade_cd = throws.iter().map(|g| g.cooldown[0] * (0.3 + rand::random::<f32>() * 0.5)).collect();
                    }
                    for c in npc.grenade_cd.iter_mut() {
                        *c -= dt;
                    }
                    let flat = (ppos - pos).with_y(0.0).length();
                    let above = ppos.y - pos.y;
                    let unreachable = above.abs() > 1.6;
                    let ready = npc.sees_player && npc.attack_t.is_none() && npc.stagger <= 0.0;
                    let pick = throws.iter().enumerate().find(|(i, g)| {
                        ready
                            && npc.grenade_cd[*i] <= 0.0
                            && match g.kind.as_str() {
                                "DisTweaks_NPCAttackUnder_Grenade" => -above > 1.5 && flat < g.range[1],
                                _ if unreachable => d >= g.range_unreachable && d <= g.range[1],
                                _ => d >= g.range[0] && d <= g.range[1],
                            }
                    });
                    if let Some((i, g)) = pick {
                        let cd = if unreachable { g.cooldown_unreachable } else { g.cooldown };
                        npc.grenade_cd[i] = cd[0] + rand::random::<f32>() * (cd[1] - cd[0]).max(0.0);
                        let toward = (ppos - pos).with_y(0.0).normalize_or_zero();
                        let from = pos + Vec3::Y * 0.6 + toward * 0.5;
                        let vel = crate::gadgets::aim_throw(from, ppos - Vec3::Y * 0.8, g.speed);
                        info!("{} throws a grenade ({}) {d:.1} m away", npc.name, g.kind.trim_start_matches("DisTweaks_"));
                        grenades.write(crate::gadgets::NpcGrenade { owner: e, from, vel, fuse: g.fuse, blast: g.blast.clone(), target: ppos, danger: g.danger });
                        noise.write(Noise { corvo: false, pos, radius: 20.0, kind: NoiseKind::Threatening });
                        if g.jump > 0.0 {
                            // (`m_fJumpClearance`)
                            npc.retreat = Some((pos - toward * g.jump, 0.7));
                        }
                        npc.attack_cd = npc.attack_cd.max(0.8);
                        continue;
                    }
                }
                // taunting (its weapon's `DisTweaks_NPCTaunt`: Corvo seen within its reach and angle,
                // every 45-60 s, 20-30 s out of reach): a gesture with its line, standing for it,
                // unless he comes at it or it is struck
                npc.taunt_cd -= dt;
                if npc.taunt_hold > 0.0 {
                    npc.taunt_hold -= dt;
                    // (one whose taunt breaks off when its conditions fail stops it once it has
                    // gone on long enough: Corvo out of its reach, height or angle, or a danger zone
                    // pushing it, the taunt's check 0x00C27B70)
                    let broken = npc.arms.taunt.as_ref().and_then(|t| t.abort).is_some_and(|a| {
                        let to = (ppos - pos).with_y(0.0);
                        TAUNT_HOLD - npc.taunt_hold >= a.after
                            && (npc.danger.abs().max_element() >= 1e-4
                                || d < a.range[0]
                                || d > a.range[1]
                                || a.height >= 0.0 && (ppos.y - pos.y).abs() > a.height
                                || to.normalize_or_zero().dot(npc.forward()) < a.angle.to_radians().cos())
                    });
                    if broken {
                        info!("{} breaks off its taunt", npc.name);
                    }
                    if d > 2.5 && npc.stagger <= 0.0 && npc.hit_react <= 0.0 && !broken {
                        npc.target = None;
                        let to = (ppos - pos).with_y(0.0);
                        if to.length() > 0.1 {
                            npc.yaw = (-to.x).atan2(-to.z);
                        }
                        continue;
                    }
                    npc.taunt_hold = 0.0;
                }
                if let Some(tc) = npc.arms.taunt.clone().filter(|_| npc.taunt_cd <= 0.0 && npc.sees_player && npc.attack_t.is_none() && npc.aim.is_none() && npc.gun_hold <= 0.0 && npc.stagger <= 0.0) {
                    let to = (ppos - pos).with_y(0.0);
                    let facing = to.normalize_or_zero().dot(npc.forward()) > tc.angle.to_radians().cos();
                    if facing && d > tc.range[0] && d < tc.range[1] {
                        let unreachable = (ppos.y - pos.y).abs() > 1.6;
                        let cd = if unreachable { tc.cooldown_unreachable } else { tc.cooldown };
                        npc.taunt_cd = cd[0] + rand::random::<f32>() * (cd[1] - cd[0]).max(0.0);
                        npc.taunt_hold = TAUNT_HOLD;
                        npc.gesture(12);
                        npc.target = None;
                        info!("{} taunts Corvo {d:.1} m away", npc.name);
                        continue;
                    }
                }
                // shooting (its fire context, `DisTweaks_NPCFireGun` / `DisTweaks_NPCFireBow`: its
                // reach, within its allowed angle of where it faces, its cooldowns at Corvo within
                // reach and out of it; without one, 2.5-30 m, 25 degrees, 10-20 s and 1-2 s, the
                // wrist bow 3-6 s and 2-5 s)
                if npc.ranged > 0 && npc.may_shoot() {
                    npc.shoot_cd -= dt;
                    let to = ppos - pos;
                    let unreachable = (ppos.y - pos.y).abs() > 1.6 || d > 6.0;
                    let ctx = fire_context(&npc);
                    let [near, far, _] = ctx.range;
                    let facing = to.with_y(0.0).normalize_or_zero().dot(npc.forward()) > ctx.angle.to_radians().cos();
                    // a pistol is aimed before it fires (`DisAISubStateFirePistol`: the first shot
                    // 0.75-1.25 s after it starts aiming, given up once Corvo is lost for a moment)
                    // and, Corvo within its blade's reach, only from a standstill (as roshan's
                    // r614/blink research decoded it)
                    let mut aimed = false;
                    // (just fired: it stands for the shot and to put the gun away, unless Corvo
                    // comes at it or it is struck, or its next shot at him out of reach is due:
                    // the gun kept up)
                    if npc.ranged == 1 && npc.gun_hold > 0.0 {
                        npc.gun_hold -= dt;
                        // (a taunt due waits for the gun to be put away)
                        let taunt_due = npc.taunt_cd <= 0.0 && npc.arms.taunt.is_some();
                        let again = npc.shoot_cd <= 0.0 && unreachable && npc.sees_player && !taunt_due;
                        if d > 2.0 && npc.stagger <= 0.0 && npc.attack_t.is_none() && !again {
                            npc.target = None;
                            if to.with_y(0.0).length() > 0.1 {
                                npc.yaw = (-to.x).atan2(-to.z);
                            }
                            continue;
                        }
                        npc.gun_hold = 0.0;
                    }
                    if matches!(npc.ranged, 1 | 2) {
                        if let Some((left, unseen)) = npc.aim {
                            let unseen = if npc.sees_player { 0.0 } else { unseen + dt };
                            if unseen > 0.65 || d > far || npc.stagger > 0.0 || npc.attack_t.is_some() {
                                npc.aim = None;
                                npc.shoot_cd = npc.shoot_cd.max(1.0);
                            } else if left > dt {
                                npc.aim = Some((left - dt, unseen));
                                npc.target = None;
                                if to.with_y(0.0).length() > 0.1 {
                                    npc.yaw = (-to.x).atan2(-to.z);
                                }
                                continue;
                            } else {
                                npc.aim = None;
                                aimed = true;
                            }
                        }
                    }
                    // (a tallboy holds its arrow while one of its own stands within 7.5 m of Corvo: the
                    // burst would take them too)
                    let ally_by = npc.ranged == 2 && others.iter().any(|o| o.0 != e && !o.3 && o.2 == npc.faction && o.1.distance(ppos) < 7.5);
                    let ready = npc.shoot_cd <= 0.0 && npc.sees_player && npc.attack_t.is_none() && d > near && d < far && npc.stagger <= 0.0 && !ally_by;
                    if npc.ranged == 2 && std::env::var("DH_GUN_LOG").is_ok() && (time.elapsed_secs() % 1.0) < time.delta_secs() {
                        info!("bow: {} cd {:.1} sees {} attack {} d {d:.1} ({near}-{far}) stagger {:.1} ally {ally_by} facing {facing}", npc.name, npc.shoot_cd, npc.sees_player, npc.attack_t.is_some(), npc.stagger);
                    }
                    if npc.ranged == 1 && ready && !aimed && (unreachable || npc.velocity.with_y(0.0).length() < 0.3) {
                        npc.aim = Some((0.75 + rand::random::<f32>() * 0.5, 0.0));
                        info!("{} aims its pistol {d:.1} m away (from {pos:.2}, facing {:.2})", npc.name, npc.yaw);
                        npc.target = None;
                        continue;
                    }
                    // a tallboy draws its bow first (`Empty_Bow_Charged_In`, its telegraph cue
                    // `Snd_AI_Tboy_Telegraph` a warning), standing for it, and holds it a breath;
                    // Corvo lost a moment or the tallboy struck, it lets it down
                    if npc.ranged == 2 && ready && !aimed && facing {
                        npc.aim = Some((BOW_DRAW, 0.0));
                        info!("{} draws its bow {d:.1} m away", npc.name);
                        npc.target = None;
                        continue;
                    }
                    if ready && (aimed || (npc.ranged == 3 && facing)) {
                        let cd = if unreachable { ctx.cooldown_unreachable } else { ctx.cooldown };
                        npc.shoot_cd = cd[0] + rand::random::<f32>() * (cd[1] - cd[0]).max(0.0);
                        // an assassin's wrist bow: a shot of its own (`Sword_RangedAttack`), standing
                        // for it, its bolt loosed at the clip's cue (the blow's, below)
                        if npc.ranged == 3 {
                            npc.attack_t = Some(0.0);
                            npc.swing = Some(Swing { kind: MoveKind::Ranged, big: false, len: 1.37, hit: 0.73, reach: far, lunge: 0.0, no_versus: true, more: Vec::new() });
                            npc.target = None;
                            if to.with_y(0.0).length() > 0.1 {
                                npc.yaw = (-to.x).atan2(-to.z);
                            }
                            info!("{} raises its wrist bow {d:.1} m away", npc.name);
                            continue;
                        }
                        // (his charms' share: Unnerving Target's for guns, Shivering Silhouette's for
                        // bows too)
                        let chance = shot_chance(&npc, &ctx, d, if npc.ranged == 1 { attrs.npc_gun_miss } else { attrs.npc_arrow_miss });
                        let hit = rand::random::<f32>() < chance;
                        info!("{} shoots {d:.1} m ({})", npc.name, if hit { "hit" } else { "miss" });
                        if npc.ranged == 2 {
                            // a tallboy's arrow flies and bursts where it strikes (`tallboy`), from
                            // the rider's bow
                            tallboy_shots.write(crate::tallboy::TallboyShot { from: pos + Vec3::Y * 2.0, at: ppos, hit, source: e });
                        } else {
                            if hit {
                                hit_writer.write(PlayerHit { npc: e, from: pos, damage: npc.arms.ranged, kick: false, big: false, push: false, unblockable: false, missile: false });
                            }
                            shots.write(NpcShot { from: pos + Vec3::Y * 0.5, to: ppos, bow: npc.ranged >= 2, miss: !hit, saw: false });
                        }
                        npc.shots += 1;
                        // (a shot heard to `DisTweaks_NPCFireGun`'s `m_AINoiseLoudnessWhenFiring`,
                        // `EAINoiseLoudness4`: 60 m)
                        if npc.ranged == 1 {
                            npc.gun_hold = GUN_HOLD;
                            npc.target = None;
                            noise.write(Noise { corvo: false, pos, radius: GUN_NOISE, kind: NoiseKind::Danger });
                            continue;
                        }
                        noise.write(Noise { corvo: false, pos, radius: GUN_NOISE, kind: NoiseKind::Danger });
                    }
                    if npc.ranged == 2 {
                        // tallboys keep their distance and shoot
                        if d < 8.0 {
                            npc.target = None;
                        }
                        continue;
                    }
                }
                // a plague carrier coughs (its weep behaviour's `m_fCoughingCooldown`), standing for
                // the clip when Corvo is near enough to catch the cloud; the clip's notifies start
                // the cloud and the vomit (`plague`)
                if let Some((reach, cough)) = npc.arms.plague.as_ref().map(|p| (p.radius, p.cough)) {
                    npc.cough_cd -= dt;
                    if npc.cough_hold > 0.0 {
                        npc.cough_hold -= dt;
                        if npc.stagger <= 0.0 && npc.hit_react <= 0.0 && npc.grab.is_none() {
                            npc.target = None;
                            continue;
                        }
                        npc.cough_hold = 0.0;
                    }
                    if npc.cough_cd <= 0.0 && npc.grab.is_none() && npc.stagger <= 0.0 && npc.sees_player && d < reach + 1.5 {
                        npc.cough_cd = cough[0] + rand::random::<f32>() * (cough[1] - cough[0]).max(0.0) + COUGH_HOLD;
                        npc.cough_hold = COUGH_HOLD;
                        npc.gesture(13);
                        npc.target = None;
                        let to = (ppos - pos).with_y(0.0);
                        if to.length() > 0.1 {
                            npc.yaw = (-to.x).atan2(-to.z);
                        }
                        info!("{} coughs {d:.1} m from Corvo", npc.name);
                        continue;
                    }
                }
                // weepers seize Corvo by the arm (`DisTweaks_WeeperGrab`)
                if npc.grab.is_some() {
                    npc.target = None;
                    let to = (ppos - pos).with_y(0.0);
                    if to.length() > 0.1 {
                        npc.yaw = (-to.x).atan2(-to.z);
                    }
                    continue;
                }
                if npc.name.contains("Weeper") && !grab_active && *grab_inhibit <= 0.0 && npc.attack_cd <= 0.0 && npc.stagger <= 0.0 && npc.sees_player && d < GRAB_RANGE && d > 0.6 {
                    let to = (ppos - pos).with_y(0.0);
                    let facing = to.normalize_or_zero().dot(npc.forward()) > 0.0;
                    if facing && (ppos.y - pos.y).abs() < 0.8 && rand::random::<f32>() < dt * 3.0 {
                        *grab_inhibit = GRAB_INHIBIT;
                        npc.grab = Some((GrabPhase::Try, 0.0));
                        npc.target = None;
                        continue;
                    }
                }
                // a wolfhound leaps at him: its arm attack from close, its jump attack from further
                // (as soon as it may: its contexts come first in its slots, paced by their
                // cooldowns)
                if is_hound(&npc) && !grab_active && npc.attack_cd <= 0.0 && npc.stagger <= 0.0 && npc.sees_player {
                    let to = (ppos - pos).with_y(0.0);
                    let off = to.normalize_or_zero().dot(npc.forward()).clamp(-1.0, 1.0).acos().to_degrees();
                    let rise = (pfeet - (pos.y - NPC_CENTER)).abs();
                    // (and none other has just used the same on him: its inhibition)
                    let free = |kind: &str| inhibited.get(kind).is_none_or(|(_, by)| *by == e);
                    let arm = npc.leap_cd[0] <= 0.0 && free("WHArmAttack") && (HOUND_ARM.0..=HOUND_ARM.1).contains(&d) && off <= 35.0 && rise <= 0.3;
                    if std::env::var("DH_HOUND_LOG").is_ok() {
                        info!("hound: {} d {d:.2} off {off:.0} rise {rise:.2} cd {:?} attack_cd {:.2} arm {arm}", npc.name, npc.leap_cd, npc.attack_cd);
                    }
                    let jump = !arm && npc.leap_cd[1] <= 0.0 && free("WHJumpAttack") && (HOUND_JUMP.0..=HOUND_JUMP.1).contains(&d) && off <= 15.0 && rise <= 1.0;
                    if arm || jump {
                        npc.grab = Some((GrabPhase::Try, 0.0));
                        npc.grab_jump = jump;
                        let (i, cd, kind) = if jump { (1, HOUND_JUMP_CD, "WHJumpAttack") } else { (0, HOUND_ARM_CD, "WHArmAttack") };
                        npc.leap_cd[i] = cd.0 + rand::random::<f32>() * (cd.1 - cd.0);
                        inhibited.insert(kind.to_string(), (inhibition(kind), e));
                        npc.attack_cd = npc.attack_cd.max(1.0);
                        npc.target = None;
                        info!("wolfhound {}: {} from {d:.1} m", if jump { "jump attack" } else { "arm attack" }, npc.name);
                        continue;
                    }
                }
                // melee: blades, or teeth and claws for the unarmed (wolfhounds); a plague carrier
                // has no blow (its weep behaviour only grabs and coughs, as roshan's r614/blink
                // research found), and keeps the same distance
                if npc.arms.plague.is_some() {
                    // its weep behaviour: standing weeping after its grab, until he is off;
                    // following him, walking but for the few that run
                    if npc.weep_stand > 0.0 {
                        npc.weep_stand -= dt;
                        if npc.weep_stand <= 0.0 && d < WEEP_RESUME {
                            npc.weep_stand = 1e-3;
                        }
                        npc.target = None;
                        if npc.weep_run {
                            npc.weep_run = false;
                            weep_runners = weep_runners.saturating_sub(1);
                        }
                    } else if !npc.weep_run && weep_runners < WEEP_RUNNERS {
                        npc.weep_run = true;
                        weep_runners += 1;
                        if std::env::var("DH_NPC_CLIP_LOG").is_ok() {
                            info!("weeper {} runs after Corvo ({weep_runners} running)", npc.name);
                        }
                    }
                    if d < 1.4 {
                        npc.target = None;
                    }
                    // (Corvo far from its home a while: it gives up, home again)
                    if (ppos - npc.home).with_y(0.0).length() > WEEP_HOME {
                        let left = npc.weep_lost.unwrap_or(WEEP_LOST.0 + rand::random::<f32>() * (WEEP_LOST.1 - WEEP_LOST.0)) - dt;
                        npc.weep_lost = Some(left);
                        if left <= 0.0 {
                            npc.weep_lost = None;
                            npc.weep_run = false;
                            npc.alert = Alert::Unaware;
                            npc.awareness = npc.awareness.min(0.2);
                            npc.set_mode(Mode::Return);
                            info!("weeper {} gives Corvo up, home again (he is {:.1} m from it, it {:.1} m)", npc.name, (ppos - npc.home).with_y(0.0).length(), (pos - npc.home).with_y(0.0).length());
                        }
                    } else {
                        npc.weep_lost = None;
                    }
                } else if npc.has_sword || npc.enemy {
                    for c in npc.melee_cd.iter_mut() {
                        *c -= dt;
                    }
                    // its parry's counter (`DisTweaks_NPCRiposte`): a quick blow straight after
                    if npc.riposte > 0.0 {
                        npc.riposte -= dt;
                        if npc.riposte <= 0.0 && npc.attack_t.is_none() && npc.stagger <= 0.0 && d < 2.6 && !crate::distrust::strike_barred(&npc, &devices, pos, ppos) {
                            npc.attack_t = Some(0.0);
                            npc.swing = Some(Swing { kind: MoveKind::Short, big: false, len: 0.85, hit: 0.4, reach: 2.6, lunge: 0.0, no_versus: false, more: Vec::new() });
                        }
                    }
                    // Corvo's blows and aim, as its combat behaviour paces them
                    // (`DisAISubProcessManageAttacks`, as roshan's r614/blink research decoded it):
                    // a blow of his that threatens it (within `m_fAngleOfPerceivedMeleeThreat` of
                    // his facing) it dodges, by its `m_DodgeLevel` (a tenth a level), its blade
                    // passing it by; after `m_IncomingAttacksBeforeBackStep` of them (forgotten
                    // after `m_fTimeToForgiveIncomingAttacks` s) it steps back out of reach
                    // (`DisTweaks_NPCBackStep`); his firearm or bow aimed at it (within
                    // `m_fAngleOfPerceivedRangedThreat`) for `m_TimeForThreatToCauseSideStep` s,
                    // it steps aside (`DisTweaks_NPCSideStep`); each step within its reach and on
                    // its cooldown
                    let pace = npc.arms.pacing.clone().unwrap_or(dhcook::format::NpcPacing {
                        flurry: [2.0, 3.0],
                        stamina: [4.0, 5.0],
                        melee_threat: 30.0,
                        back_step_after: [1.0, 1.0],
                        forgive: 5.0,
                        ranged_threat: 10.0,
                        side_step_after: [2.0, 3.0],
                        aggression: [-0.5, 0.1, 1.0],
                        line_of_fire: None,
                        fire_from_head: false,
                    });
                    npc.dodging = (npc.dodging - dt).max(0.0);
                    npc.incoming.1 += dt;
                    if npc.incoming.1 > pace.forgive {
                        npc.incoming.0 = 0;
                    }
                    let facing = Vec3::new(-pl.yaw.sin(), 0.0, -pl.yaw.cos());
                    let to_it = (pos - ppos).with_y(0.0).normalize_or_zero();
                    let swinging = swing_began && d < 3.2 && facing.dot(to_it) >= pace.melee_threat.to_radians().cos();
                    let aim = Quat::from_euler(EulerRot::YXZ, pl.yaw, pl.pitch, 0.0) * Vec3::NEG_Z;
                    let to_chest = (pos + Vec3::Y * 0.4 - (ppos + Vec3::Y * 0.6)).normalize_or_zero();
                    if powers.selected.is_ranged() && npc.sees_player && aim.dot(to_chest) >= pace.ranged_threat.to_radians().cos() {
                        if npc.ranged_threat.1 < 0.0 {
                            npc.ranged_threat.1 = pace.side_step_after[0] + rand::random::<f32>() * (pace.side_step_after[1] - pace.side_step_after[0]).max(0.0);
                        }
                        let before = npc.ranged_threat.0;
                        npc.ranged_threat.0 += dt;
                        if before < npc.ranged_threat.1 && npc.ranged_threat.0 >= npc.ranged_threat.1 && std::env::var("DH_MELEE_LOG").is_ok() {
                            info!("melee: {} has borne Corvo's aim {:.1} s ({d:.1} m{})", npc.name, npc.ranged_threat.0, if npc.attack_t.is_some() { ", attacking" } else { "" });
                        }
                    } else {
                        npc.ranged_threat = (0.0, -1.0);
                    }
                    if swing_began && d < 4.0 && std::env::var("DH_MELEE_LOG").is_ok() {
                        info!(
                            "melee: Corvo's blow at {} ({d:.1} m, {:.0} degrees off his facing): {}",
                            npc.name,
                            facing.dot(to_it).clamp(-1.0, 1.0).acos().to_degrees(),
                            if !swinging { "no threat" } else if npc.attack_t.is_some() { "it is attacking" } else if npc.stagger > 0.0 { "it is staggered" } else if npc.retreat.is_some() { "it is stepping" } else if !npc.sees_player { "unseen" } else { "it answers" }
                        );
                    }
                    let mut step = None;
                    let mut danger_way = None;
                    if swinging && npc.attack_t.is_none() && npc.stagger <= 0.0 && npc.retreat.is_none() && npc.sees_player {
                        npc.incoming = (npc.incoming.0 + 1, 0.0);
                        let after = pace.back_step_after[0] + rand::random::<f32>() * (pace.back_step_after[1] - pace.back_step_after[0]).max(0.0);
                        // (a back step only when it has one to take: on its cooldown, within its
                        // reach, not held back; else it dodges as it would)
                        let back_ok = npc.arms.moves.iter().enumerate().any(|(i, m)| m.kind == "BackStep" && npc.melee_cd.get(i).is_none_or(|c| *c <= 0.0) && d <= m.range[1] && !inhibited.contains_key(&m.kind));
                        if back_ok && npc.incoming.0 as f32 >= after.round().max(1.0) {
                            step = Some("BackStep");
                        } else if npc.dodges && rand::random::<f32>() < npc.arms.dodge * 0.1 {
                            // (its dodge: aside out of the blade's way, leaning, then back or its
                            // riposte, `Sword_DodgeLeftIn` and after)
                            let right = Vec3::new(-npc.forward().z, 0.0, npc.forward().x);
                            let left_side = rand::random::<bool>();
                            npc.dodging = 0.6;
                            npc.retreat = Some((pos + (if left_side { -right } else { right }) * 0.4, 0.4));
                            npc.gesture(if left_side { 22 } else { 23 });
                            if std::env::var("DH_MELEE_LOG").is_ok() {
                                info!("melee: {} dodges Corvo's blow ({}) at {d:.1} m", npc.name, if left_side { "left" } else { "right" });
                            }
                            continue;
                        } else if !npc.dodges {
                            // (one without the dodge's clips steps aside out of it: an assassin's
                            // `Sword_SideStepLeft_DodgeIn`)
                            step = Some("SideStep");
                        }
                    } else if npc.ranged_threat.1 >= 0.0 && npc.ranged_threat.0 >= npc.ranged_threat.1 && npc.attack_t.is_none() && npc.stagger <= 0.0 && npc.retreat.is_none() {
                        npc.ranged_threat = (0.0, -1.0);
                        step = Some("SideStep");
                    } else if npc.attack_t.is_none() && npc.stagger <= 0.0 && npc.retreat.is_none() && facing_within(npc.forward(), ppos - pos, DANGER_STEP_ANGLE) {
                        // a danger zone's push (`danger`; ManageAttacks 0x00B3A300): tried each tick
                        // it is pushed, as its steps' cooldowns allow
                        if let Some(k) = danger_step(npc.forward(), npc.danger) {
                            step = Some(k);
                            danger_way = Some(npc.danger);
                        }
                    }
                    if let Some(kind) = step {
                        let moves = npc.arms.moves.clone();
                        if npc.melee_cd.len() != moves.len() {
                            npc.melee_cd = vec![0.0; moves.len()];
                        }
                        // (a step's inhibition holds it back too: an elite guard, stepping back
                        // after each blow, would else never dodge)
                        let dodge = moves.iter().enumerate().find(|(i, m)| m.kind == kind && npc.melee_cd[*i] <= 0.0 && d <= m.range[1] && !inhibited.contains_key(&m.kind));
                        if let Some((i, m)) = dodge {
                            // (a blow's step aside, one without the dodge's clips: as readily as it
                            // dodges)
                            if !(swinging && kind == "SideStep") || rand::random::<f32>() < npc.arms.dodge * 0.1 {
                                if kind == "BackStep" {
                                    npc.incoming.0 = 0;
                                }
                                // (a step from danger starts its threats' count afresh)
                                if danger_way.is_some() {
                                    npc.incoming = (0, 0.0);
                                    npc.ranged_threat = (0.0, -1.0);
                                }
                                inhibited.insert(m.kind.clone(), (inhibition(&m.kind), e));
                                let toward = (ppos - pos).with_y(0.0).normalize_or_zero();
                                let away = if m.kind == "SideStep" {
                                    let side = Vec3::new(-toward.z, 0.0, toward.x);
                                    // (from danger, the way it is pushed: 0x00C20690)
                                    match danger_way {
                                        Some(w) if w.dot(side).abs() > 1e-6 => side * w.dot(side).signum(),
                                        _ if rand::random::<bool>() => side,
                                        _ => -side,
                                    }
                                } else {
                                    -toward
                                };
                                npc.retreat = Some((pos + away * m.step.max(1.0), 0.45));
                                // (its step's clip: left, right or back)
                                let right = Vec3::new(-npc.forward().z, 0.0, npc.forward().x);
                                npc.gesture(if m.kind != "SideStep" { 17 } else if away.dot(right) > 0.0 { 16 } else { 15 });
                                if std::env::var("DH_MELEE_LOG").is_ok() {
                                    info!("melee: {} steps ({}) at {d:.1} m", npc.name, m.kind);
                                }
                                npc.melee_cd[i] = m.cooldown[0] + rand::random::<f32>() * (m.cooldown[1] - m.cooldown[0]).max(0.0);
                                continue;
                            }
                        }
                    }
                    if let Some(t_att) = npc.attack_t {
                        let t_new = t_att + dt;
                        // (a kick, and the charge's shoulder, knock him back: no damage,
                        // `m_fAttackDamageMultiplier` 0)
                        let (len, hit_at, reach, kick, big) = npc.swing.as_ref().map(|s| (s.len, s.hit, s.reach, matches!(s.kind, MoveKind::Bash | MoveKind::Jump), s.big)).unwrap_or((0.95, 0.55, 2.3, false, false));
                        let kind = npc.swing.as_ref().map(|s| s.kind);
                        let under = matches!(kind, Some(MoveKind::Under | MoveKind::UnderGun));
                        // (spraying, a butcher turns his saw after Corvo, as far as its
                        // `m_fAbortAllowedAngle` lets the spray go on: 90 degrees a second, our reading)
                        if kind == Some(MoveKind::SawFire) {
                            let want = (-(ppos - pos).x).atan2(-(ppos - pos).z);
                            let mut turn = (want - npc.yaw).rem_euclid(TAU);
                            if turn > PI {
                                turn -= TAU;
                            }
                            npc.yaw += turn.clamp(-FRAC_PI_2 * dt, FRAC_PI_2 * dt);
                        }
                        let blows = std::iter::once(hit_at).chain(npc.swing.as_ref().map(|s| s.more.clone()).unwrap_or_default());
                        for b in blows {
                            if !(t_att < b && t_new >= b) {
                                continue;
                            }
                            // a witch's spell at its cue (`witch`): her scream along her line,
                            // a bolt of her magic, a tentacle called up by Corvo
                            if let Some(k @ (MoveKind::Scream | MoveKind::Magic | MoveKind::Summon)) = kind {
                                let spell = match k {
                                    MoveKind::Scream => crate::witch::Spell::Scream,
                                    MoveKind::Magic => crate::witch::Spell::Magic,
                                    _ => crate::witch::Spell::Summon,
                                };
                                spells.write(crate::witch::WitchSpell { witch: e, spell, from: pos, facing: npc.forward() });
                                continue;
                            }
                            // an assassin's bolt, as its accuracy has it
                            if kind == Some(MoveKind::Ranged) {
                                let hit = npc.sees_player && rand::random::<f32>() < shot_chance(&npc, &fire_context(&npc), d, attrs.npc_arrow_miss);
                                info!("{} shoots {d:.1} m ({})", npc.name, if hit { "hit" } else { "miss" });
                                if hit {
                                    hit_writer.write(PlayerHit { npc: e, from: pos, damage: npc.arms.ranged, kick: false, big: false, push: false, unblockable: false, missile: false });
                                }
                                shots.write(NpcShot { from: pos + Vec3::Y * 0.5, to: ppos, bow: true, miss: !hit, saw: false });
                                npc.shots += 1;
                                // (a bow has no firing noise, as the crossbow: no
                                // `m_AINoiseLoudnessWhenFiring`)
                                continue;
                            }
                            // a whale butcher's saw's spray: a shot of shrapnel at each of its moments
                            // (`DLC06_Proj_SawProjectile`, an arrow's: its accuracy over the move's
                            // reach, as a bow's misses go)
                            if kind == Some(MoveKind::SawFire) {
                                let range = npc.arms.moves.iter().find(|m| m.kind == "SawFire").map_or([3.0, 35.0, 7.0], |m| m.range);
                                let ctx = dhcook::format::FireContext { range, ..fire_context(&npc) };
                                let hit = npc.sees_player && rand::random::<f32>() < shot_chance(&npc, &ctx, d, attrs.npc_arrow_miss);
                                if hit {
                                    hit_writer.write(PlayerHit { npc: e, from: pos, damage: npc.arms.ranged, kick: false, big: false, push: false, unblockable: true, missile: true });
                                }
                                shots.write(NpcShot { from: pos + Vec3::Y * 0.35 + npc.forward() * 0.9, to: ppos, bow: false, miss: !hit, saw: true });
                                continue;
                            }
                            // a thing thrown with the bare hand at him
                            if let (Some(MoveKind::Throw), Some(th)) = (kind, npc.arms.throwing.as_ref()) {
                                let from = pos + Vec3::Y * 0.8 + npc.forward() * 0.3;
                                let vel = crate::gadgets::aim_throw(from, ppos, th.speed);
                                throws.write(crate::thrown::NpcThrow { owner: e, from, vel, damage: th.damage[(settings.difficulty as usize).min(3)] });
                                if std::env::var("DH_MELEE_LOG").is_ok() {
                                    info!("melee: {} throws at {d:.1} m", npc.name);
                                }
                                continue;
                            }
                            // a thug's whiskey: its fire breath along its line (as far as
                            // `m_fDamageRange`, within `m_fDamageRadius` of it and Corvo's body
                            // about it; unblockable, its smoke where it strikes), or its bottle
                            // thrown at him
                            if let (Some(k @ (MoveKind::WhiskeyFire | MoveKind::ThrowWhiskey)), Some(w)) = (kind, npc.arms.whiskey.as_ref()) {
                                let f = npc.forward();
                                if k == MoveKind::WhiskeyFire {
                                    let mouth = pos + Vec3::Y * 0.75 + f * 0.25;
                                    let rel = ppos - mouth;
                                    let along = rel.dot(f);
                                    let off = (rel - f * along).with_y(0.0).length();
                                    let clear = ctx.cast_ray(mouth, rel.normalize_or_zero(), rel.length(), true, wall_filter).is_none();
                                    let struck = along > 0.0 && along < w.fire_range && off < w.fire_radius + 0.35 && (ppos.y - pos.y).abs() < 1.5 && clear;
                                    if struck {
                                        hit_writer.write(PlayerHit { npc: e, from: pos, damage: w.fire_damage, kick: false, big: false, push: false, unblockable: true, missile: true });
                                    }
                                    let at = if struck { ppos + Vec3::Y * 0.6 } else { mouth + f * along.clamp(0.5, w.fire_range) };
                                    whiskey_fx.write(crate::particles::SpawnEffect { secs: w.blind.max(1.0), ..crate::particles::SpawnEffect::at("whiskey_smoke", at) });
                                    noise.write(Noise { corvo: false, pos, radius: 8.0, kind: NoiseKind::Threatening });
                                } else {
                                    let from = pos + Vec3::Y * 0.7 + f * 0.4;
                                    let vel = crate::gadgets::aim_throw(from, ppos - Vec3::Y * 0.8, w.throw_speed);
                                    bottles.write(crate::whiskey::NpcBottle { owner: e, from, vel, blast: w.blast.clone() });
                                }
                                if std::env::var("DH_MELEE_LOG").is_ok() {
                                    info!("melee: {} {k:?} at {d:.1} m", npc.name);
                                }
                                continue;
                            }
                            let to = (ppos - pos).with_y(0.0);
                            let in_front = to.normalize_or_zero().dot(npc.forward()) > 0.5;
                            let shot = kind == Some(MoveKind::UnderGun);
                            let struck = to.length() < reach && (ppos.y - pos.y).abs() < 1.5 && in_front;
                            if struck {
                                let damage = if kick { 0.0 } else if shot { npc.arms.ranged } else { npc.arms.melee };
                                hit_writer.write(PlayerHit { npc: e, from: pos, damage, kick, big, push: false, unblockable: under, missile: false });
                            }
                            if shot {
                                shots.write(NpcShot { from: pos + Vec3::Y * 0.2, to: ppos, bow: false, miss: !struck, saw: false });
                                noise.write(Noise { corvo: false, pos, radius: GUN_NOISE, kind: NoiseKind::Danger });
                            } else {
                                noise.write(Noise { corvo: false, pos, radius: 8.0, kind: NoiseKind::Threatening });
                            }
                        }
                        npc.attack_t = if t_new > len { None } else { Some(t_new) };
                        if npc.attack_t.is_none() && kind == Some(MoveKind::Under) {
                            npc.under_cd[0] = UNDER_SWORD.0 + rand::random::<f32>() * UNDER_SWORD.1;
                        }
                        if npc.attack_t.is_none() {
                            npc.swing = None;
                        }
                    } else if npc.attack_cd <= 0.0
                        && npc.stagger <= 0.0
                        && npc.sees_player
                        && (attackers < max_attackers || (!npc.arms.outer_only.is_empty() && (!ring.contains(&e) || (ppos.y - pos.y).abs() > 1.6)))
                        && npc.may_melee()
                        && pace_ready(&npc)
                        // (not at Corvo in or across a device it no longer trusts)
                        && !crate::distrust::strike_barred(&npc, &devices, pos, ppos)
                    {
                        // (outside the ring of those at Corvo, or with him out of reach, only the
                        // moves for there)
                        let outer = !ring.contains(&e) || (!npc.arms.outer_only.is_empty() && (ppos.y - pos.y).abs() > 1.6 && (ppos - pos).with_y(0.0).length() > 2.5);
                        // the move for the distance and its facing (its blade's
                        // `DisTweaks_NPCAttackShort` / `Medium` / `Long`), a block answered with a
                        // kick (`DisTweaks_NPCBash`); without them, a blow at arm's length
                        let moves = npc.arms.moves.clone();
                        if npc.melee_cd.len() != moves.len() {
                            npc.melee_cd = vec![0.0; moves.len()];
                        }
                        // where Corvo is from its facing (degrees, clockwise)
                        let to = (ppos - pos).with_y(0.0).normalize_or_zero();
                        let f = npc.forward();
                        let right = Vec3::new(-f.z, 0.0, f.x);
                        let side = to.dot(right).atan2(to.dot(f)).to_degrees();
                        let fits = |i: usize, m: &dhcook::format::MeleeMove| {
                            let want = MoveKind::of(&m.kind).side();
                            let mut off = (side - want).abs();
                            if off > 180.0 {
                                off = 360.0 - off;
                            }
                            npc.melee_cd[i] <= 0.0 && d >= m.range[0] && d <= m.range[1] && off <= m.angle.max(10.0) && (ppos.y - pos.y).abs() <= m.height.max(1.5) && inhibited.get(&m.kind).is_none_or(|(_, by)| *by == e) && npc.arms.outer_only.contains(&m.kind) == outer
                        };
                        let pick = if moves.is_empty() {
                            (d < 2.0).then_some(None)
                        } else {
                            let bash = moves.iter().enumerate().find(|(i, m)| blocking && m.kind == "Bash" && fits(*i, *m));
                            bash.or_else(|| moves.iter().enumerate().find(|(i, m)| MoveKind::is_attack(&m.kind) && fits(*i, *m))).map(|(i, _)| Some(i))
                        };
                        if let Some(i) = pick {
                            if !outer {
                                attackers += 1;
                            }
                            npc.attack_t = Some(0.0);
                            npc.swing = Some(match i.map(|i| &moves[i]) {
                                Some(m) => {
                                    let kind = MoveKind::of(&m.kind);
                                    let (len, hit) = match kind {
                                        MoveKind::Short => (0.95, 0.5),
                                        MoveKind::Medium => (1.2, 0.7),
                                        MoveKind::Long => (1.5, 0.9),
                                        MoveKind::Bash => (0.9, 0.45),
                                        MoveKind::Jump => (1.3, 0.6),
                                        _ => (1.0, 0.6),
                                    };
                                    // a lunge (and the charge) closes to arm's length by the time it
                                    // lands
                                    let lunge = if matches!(kind, MoveKind::Medium | MoveKind::Long | MoveKind::Jump) { (d - 1.4).max(0.0) / hit } else { 0.0 };
                                    // (a turning blow turns it to him)
                                    if kind.side() != 0.0 {
                                        npc.yaw = (-(ppos - pos).x).atan2(-(ppos - pos).z);
                                    }
                                    // (a saw's cuts meet no blade: `DisDLC06Tweaks_NPCSawCut.m_bDisableVersus`)
                                    Swing { kind, big: rand::random::<f32>() < m.big, len, hit, reach: if kind == MoveKind::Bash { m.range[1] + 0.3 } else { 2.4 }, lunge, no_versus: npc.arms.saw.is_some(), more: Vec::new() }
                                }
                                None => Swing { kind: MoveKind::Short, big: false, len: 0.95, hit: 0.55, reach: 2.3, lunge: 0.0, no_versus: false, more: Vec::new() },
                            });
                            if let Some(i) = i {
                                let hold = inhibition(&moves[i].kind);
                                if hold > 0.0 {
                                    inhibited.insert(moves[i].kind.clone(), (hold, e));
                                }
                                // (the less aggressive, the longer: x1 at full aggression, x2 at none)
                                // (a bottle thrown at him out of reach, sooner again:
                                // `m_ContextCooldownWhenUnreachable`)
                                let away = (ppos.y - pos.y).abs() > 1.6;
                                let cd = match (&npc.arms.whiskey, &npc.arms.throwing) {
                                    (Some(w), _) if moves[i].kind == "ThrowWhiskey" && away => w.cooldown_unreachable,
                                    (_, Some(t)) if moves[i].kind == "Throw" && away => t.cooldown_unreachable,
                                    _ => moves[i].cooldown,
                                };
                                npc.melee_cd[i] = (cd[0] + rand::random::<f32>() * (cd[1] - cd[0]).max(0.0)) * ((1.0 - npc.aggression.clamp(0.0, 1.0)) * npc.arms.pacing.as_ref().map_or(COOLDOWN_FACTOR, |p| p.aggression[2]) + 1.0);
                            }
                            // a blow of its flurry; spent, it recovers its stamina
                            npc.flurry -= 1;
                            if npc.flurry < 1 {
                                let rest = npc.arms.pacing.as_ref().map_or(STAMINA_RECOVERY, |p| p.stamina);
                                npc.stamina = rest[0] + rand::random::<f32>() * (rest[1] - rest[0]);
                            }
                            if std::env::var("DH_MELEE_LOG").is_ok() {
                                info!("melee: {} {:?} at {d:.1} m (blocking {blocking})", npc.name, npc.swing);
                            }
                            npc.attack_cd = 0.5 + rand::random::<f32>() * 0.6;
                        }
                    }
                    // hold distance (a wolfhound its bite's ideal reach, backing off inside it:
                    // its MeleeChase / Engage, as roshan's r614/blink research decoded it)
                    let hound = is_hound(&npc);
                    let hold = if hound { npc.arms.moves.iter().find(|m| m.kind == "Medium").map_or(1.4, |m| m.range[2].max(1.4)) } else { 1.4 };
                    if d < hold {
                        npc.target = (hound && d < hold - 0.4 && npc.attack_t.is_none()).then(|| pos + (pos - ppos).with_y(0.0).normalize_or_zero() * (hold - d + 0.3));
                    }
                }
                let _ = pe;
            }
            Mode::Choked | Mode::Unconscious | Mode::Dead => {}
        }
    }
    *looked_over = looked_now;
    if !alerts.is_empty() {
        // an ally starting to fight, within the group distance
        // (`DishonoredGlobalAIManager.m_fGroupDistance` 1000 uu): one that sees it joins the
        // fight (`SeeAllyCombat` may reach busted); the others' attention rises
        // (`AllyCombatTarget`: +1.0, at least to investigating, never busted: they go to look),
        // as roshan's r614/blink research decoded it
        let ctx = rapier.single().ok();
        let world = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
        for (_, mut npc, t) in &mut npcs {
            if npc.is_down() || !npc.hostile() || npc.alert == Alert::Combat {
                continue;
            }
            let Some(&(at, foe)) = alerts.iter().find(|(p, _)| p.distance(t.translation) < GROUP_DISTANCE) else { continue };
            let sight = level.as_ref().and_then(|l| npc.npc_type.and_then(|ty| l.scene.npc_types.get(ty as usize))).and_then(|ty| ty.sight.clone());
            let eye = t.translation + Vec3::Y * 0.7;
            let to = at + Vec3::Y * 0.7 - eye;
            let d = to.length();
            let in_view = sight.as_ref().is_none_or(|s| vision_range(s, npc.forward(), to / d.max(1e-3), d, false).is_some());
            let clear = ctx.as_ref().is_some_and(|c| c.cast_ray(eye, to / d.max(1e-3), (d - 0.4).max(0.0), true, world).is_none());
            // (an ally seen fighting makes it suspect, `SeeAllyCombat`)
            npc.suspecting |= in_view && clear;
            if in_view && clear {
                npc.alert = Alert::Combat;
                npc.awareness = 1.0;
                npc.last_seen = Some(foe);
                npc.set_mode(Mode::Combat);
            } else if let Some(att) = level.as_ref().and_then(|l| attention_of(&npc, &l.scene)) {
                let cap = (att.levels[3][0] - 0.001).min(att.max);
                npc.awareness = npc.awareness.max((npc.awareness + 1.0).max(att.levels[2][0]).min(cap));
                npc.last_seen = Some(at);
                if npc.mode != Mode::Investigate {
                    npc.alert = Alert::Suspicious;
                    npc.target = Some(at);
                    npc.search_run = false;
                    npc.set_mode(Mode::Investigate);
                }
            }
        }
    }
}

/// How far an ally's fight carries (`DishonoredGlobalAIManager.m_fGroupDistance`, 1000 uu).
const GROUP_DISTANCE: f32 = 10.0;

/// The search's locomotion modifier (`SearchSword`, `SearchEmpty`).
const SEARCH_SPEED: f32 = 0.8;

/// The panic behaviour's last survivor rule (`DisAIBrainProcessPanic`'s kill handler,
/// 0x00B617C0, decoded for this rewrite): when one of a character's own falls to Corvo, it
/// counts the fall (the count forgets falls older than `m_fKillCountResetTimer`); with at
/// least `m_iKillCount` counted, Corvo within `m_fFearDistanceFromKiller`, it fighting him, and
/// no more than `m_iSurvivorCount` left fighting him, it panics and flees from him (at most
/// `m_PanicCountLimit` times).
fn last_survivors(
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), Without<Player>>,
    player: Query<&Transform, With<Player>>,
    mut seen: Local<std::collections::HashSet<Entity>>,
) {
    let (Some(level), Ok(pt)) = (level, player.single()) else { return };
    let ppos = pt.translation;
    // who fell to Corvo since last time (their side)
    let mut fallen: Vec<String> = Vec::new();
    for (e, npc, _) in &npcs {
        if npc.mode == Mode::Dead && seen.insert(e) && npc.hit_by_player {
            fallen.push(npc.faction.clone());
        }
    }
    seen.retain(|e| npcs.get(*e).is_ok_and(|(_, n, _)| n.mode == Mode::Dead));
    if fallen.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    let fighting: Vec<String> = npcs.iter().filter(|(_, n, _)| !n.is_down() && n.hostile() && n.mode == Mode::Combat).map(|(_, n, _)| n.faction.clone()).collect();
    for (_, mut npc, t) in &mut npcs {
        if npc.is_down() || !npc.hostile() {
            continue;
        }
        let Some(p) = npc.npc_type.and_then(|i| level.scene.npc_types.get(i as usize)).and_then(|ty| ty.sight.as_ref()).and_then(|s| s.panic.as_ref()) else { continue };
        if !p.rules.iter().any(|r| r == "LastSurvivor") {
            continue;
        }
        let fell = fallen.iter().filter(|f| **f == npc.faction).count();
        let left = fighting.iter().filter(|f| **f == npc.faction).count();
        let near = t.translation.distance(ppos);
        let fights = npc.mode == Mode::Combat;
        let panics = npc.panics;
        let go = last_survivor(p, &mut npc.kill_queue, now, fell, near, fights, left, panics);
        if std::env::var("DH_NPC_LOG").is_ok() && fell > 0 {
            info!("{}: one of its own fell ({} counted, {near:.1} m from Corvo, {:?}, {left} fighting him)", npc.name, npc.kill_queue.len(), npc.mode);
        }
        if go {
            if std::env::var("DH_NPC_LOG").is_ok() {
                info!("{}: last survivor ({} fallen, {left} left): panics", npc.name, npc.kill_queue.len());
            }
            npc.panics += 1;
            npc.kill_queue.clear();
            npc.last_seen = Some(ppos);
            npc.panic_fight = if p.caught == "Fight" { p.cower } else { 0.0 };
            npc.free_flight = p.free_flight;
            npc.flee_to = None;
            npc.set_mode(Mode::Flee);
        }
    }
}

/// The kill handler's count and test: each of its own that fell counted (falls older than
/// `m_fKillCountResetTimer` forgotten first); then whether it panics: `m_iKillCount` counted,
/// Corvo within `m_fFearDistanceFromKiller`, it fighting him, no more than `m_iSurvivorCount`
/// (itself among them) left fighting him, and its `m_PanicCountLimit` not used up.
#[allow(clippy::too_many_arguments)]
fn last_survivor(p: &dhcook::format::PanicTweak, queue: &mut Vec<f32>, now: f32, fell: usize, near: f32, fights: bool, left: usize, panics: u32) -> bool {
    let [survivors, kills, reset, fear] = p.survivor;
    for _ in 0..fell {
        queue.retain(|k| now - *k < reset);
        queue.push(now);
    }
    queue.len() as f32 >= kills && near <= fear && fights && left as f32 <= survivors && (p.count_limit == 0 || panics < p.count_limit)
}

/// A civilian that may panic now: unarmed, not barred from it, able to run, not already
/// running, not of Corvo's own people.
/// The noises a panic's `m_HearNoise` lists when its tweak names none (the class default).
const SCARY_NOISES: [&str; 5] = ["AlarmBell", "Help", "Danger", "Threatening", "DeathRattle"];

/// Whether a character's panic behaviour has a rule on (`DisPanicRule.m_bIsActive`).
fn panic_rule(scene: Option<&dhcook::format::Scene>, npc: &Npc, rule: &str) -> bool {
    npc.npc_type
        .and_then(|t| scene?.npc_types.get(t as usize))
        .and_then(|t| t.sight.as_ref())
        .and_then(|s| s.panic.as_ref())
        .is_some_and(|p| p.rules.iter().any(|r| r == rule))
}

/// One that may start panicking now: not barred from it (`Panic_Civilian_Disable`), able to run,
/// not already running or down, not of Corvo's side.
fn may_flee(npc: &Npc) -> bool {
    npc.may_panic() && npc.can_flee && npc.mode != Mode::Flee && !npc.is_down() && !dhcook::format::is_player_faction(&npc.faction)
}

fn civilian_may_panic(npc: &Npc) -> bool {
    !npc.hostile() && !npc.fighter() && npc.may_panic() && npc.can_flee && npc.mode != Mode::Flee && !npc.is_down() && !dhcook::format::is_player_faction(&npc.faction)
}

/// Whether a noise sends a searcher running (`m_SearchFromNoiseTweaks[4..7].m_bShouldRun`:
/// danger, death rattles, cries for help, alarms).
fn runs_to(kind: NoiseKind) -> bool {
    matches!(kind, NoiseKind::Danger | NoiseKind::DeathRattle | NoiseKind::Help | NoiseKind::Alarm)
}

/// How far off its target a fighter may face and still step from danger (the steps'
/// `m_fAllowedAngle`, 25 degrees for guards).
const DANGER_STEP_ANGLE: f32 = 25.0;

/// Whether `facing` is within `deg` degrees of `to` (on the ground).
fn facing_within(facing: Vec3, to: Vec3, deg: f32) -> bool {
    let to = to.with_y(0.0).normalize_or_zero();
    to != Vec3::ZERO && facing.with_y(0.0).normalize_or_zero().dot(to) >= deg.to_radians().cos()
}

/// The step a danger zone's push asks of a fighter (ManageAttacks 0x00B3A300, 0x00BBE6D0): where
/// the danger lies (against the push) by quadrants of its facing: ahead, a step back; at a
/// side, a step aside; behind, none.
pub(crate) fn danger_step(facing: Vec3, push: Vec3) -> Option<&'static str> {
    if push.abs().max_element() < 1e-4 {
        return None;
    }
    let fwd = facing.with_y(0.0).normalize_or(Vec3::NEG_Z);
    let right = Vec3::new(-fwd.z, 0.0, fwd.x);
    let to_danger = -push.with_y(0.0);
    let (x, y) = (to_danger.dot(fwd), to_danger.dot(right));
    if x.abs() > y.abs() {
        (x > 0.0).then_some("BackStep")
    } else if y.abs() > 0.0 {
        Some("SideStep")
    } else {
        None
    }
}

/// Steering + kinematic movement.
pub(crate) fn npc_move(
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<
        (Entity, &mut Npc, &mut Transform, Option<&mut KinematicCharacterController>, Option<&KinematicCharacterControllerOutput>),
        (Without<Player>, Without<ScriptedAnim>, Without<crate::possession::Possessed>, Without<crate::carry::Falling>),
    >,
    mut steps: MessageWriter<crate::footsteps::Footfall>,
    (grid, mut budget, level, devices): (Option<Res<crate::navmesh::NavGrid>>, ResMut<crate::navmesh::PathBudget>, Option<Res<LevelInfo>>, Res<crate::security::Devices>),
    data: Res<crate::gamedata::Data>,
) {
    let engage_turn = data.pawn("engage.m_fRotationSpeed", 600.0).to_radians();
    let (dt_world, dt_own) = (time.delta_secs().min(0.05) * tc.world_scale(), time.delta_secs().min(0.05) * tc.own_scale());
    // (fallen out of the world: below the level's kill height, `WorldInfo.KillZ`)
    let kill_y = level.as_ref().map(|l| l.scene.kill_y).unwrap_or(-200.0);
    let ppos = player.single().map(|t| t.translation).ok();
    let ctx = rapier.single().ok();
    let ground_filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, crate::level::GROUP_WORLD));
    for (me, mut npc, mut t, kcc, out) in &mut npcs {
        let dt = if npc.out_of_bend { dt_own } else { dt_world };
        // falling over (bodies lose their controller, so this comes first)
        if npc.is_down() {
            npc.down_t = (npc.down_t + dt * 1.6).min(1.0);
            // bodies rest on the floor (pre-placed corpses are placed above it, for their
            // ragdolls to drop)
            if !npc.grounded {
                if let Some((_, toi)) = ctx.as_ref().and_then(|c| c.cast_ray(t.translation, Vec3::NEG_Y, 40.0, true, ground_filter)) {
                    t.translation.y -= (toi - NPC_CENTER).max(0.0);
                    npc.grounded = true;
                }
            }
        }
        // knocked off its feet: its bodies carry it (`ragdoll`); thrown, it flies on its push
        // first, falling, against what is in its way
        if npc.floored {
            if let Some(mut k) = kcc {
                if npc.flying && dt > 0.0 {
                    // (on the ground it slides on, not gathering its fall)
                    let landed = out.is_some_and(|o| o.grounded) && npc.velocity.y <= 0.0;
                    npc.velocity.y = if landed { -1.0 } else { (npc.velocity.y - 19.0 * dt).max(-30.0) };
                    k.translation = Some(npc.velocity * dt);
                } else {
                    k.translation = None;
                }
            }
            continue;
        }
        // Distant, calm NPCs skip the character controller (its shape casts against the
        // level trimeshes dominate the frame) and just follow their route on a ground ray.
        let far = ppos.map(|p| p.distance(t.translation) > FAR_MOVE).unwrap_or(false)
            && npc.alert == Alert::Unaware
            && !npc.is_down();
        if let Some(o) = out {
            if !far {
                npc.grounded = o.grounded;
            }
        }
        let Some(mut kcc) = kcc else { continue };
        if dt <= 0.0 {
            kcc.translation = None;
            continue;
        }
        let pos = t.translation;
        let speed = match npc.mode {
            _ if npc.alarm_run.is_some() || npc.escape.is_some() || npc.plague_escape.is_some() => npc.gait[2],
            // (a weeper walks after Corvo, but for the few that run)
            Mode::Combat if npc.arms.plague.is_some() && !npc.weep_run => npc.gait[0],
            Mode::Combat => npc.gait[2],
            Mode::Flee => npc.gait[3],
            // searching, its walk or run slowed (`SearchSword` / `SearchEmpty`: x 0.8), as roshan's
            // r614/blink research read the locomotion modifiers
            Mode::Investigate | Mode::Search => {
                if npc.alert == Alert::Combat {
                    npc.gait[1] * 1.3
                } else if npc.search_run {
                    npc.gait[2] * SEARCH_SPEED
                } else {
                    npc.gait[1] * SEARCH_SPEED
                }
            }
            _ => npc.gait[0],
        };
        // (faster than an alert walk hurried: a run, as the avoidance takes its speed mode)
        npc.running = speed > npc.gait[1] * 1.3 + 0.01;
        let mut wish = Vec3::ZERO;
        // a lunging blow carries it at Corvo until it lands (a step back, away)
        let lunge = npc.swing.as_ref().filter(|s| s.lunge != 0.0 && npc.attack_t.is_some_and(|t| t < s.hit)).map(|s| s.lunge);
        let frozen = npc.stagger > 0.0 || (npc.attack_t.is_some() && lunge.is_none()) || npc.is_down() || npc.mode == Mode::Choked || npc.versus || npc.ambush.is_some();
        if let (Some(v), Some(p), false) = (lunge, ppos, frozen) {
            wish = (p - pos).with_y(0.0).normalize_or_zero() * v;
        }
        npc.path = None;
        if !frozen && lunge.is_none() {
            if let Some(goal) = npc.target {
                // along its path over the navigation mesh (straight at it off the mesh)
                let mut nav = std::mem::take(&mut npc.nav);
                // (kept out of the walls of light it shuns)
                let barred = devices.walls_barring(npc.arms.danger.walls, &npc.faction, npc.mode == Mode::Flee, &npc.distrust);
                let was = nav.barred;
                let steer = crate::navmesh::steer_barred(grid.as_deref(), &mut budget, &mut nav, pos - Vec3::Y * NPC_CENTER, goal, dt, &barred);
                if nav.barred && !was {
                    info!("{} waits: its only way runs through a wall of light it shuns", npc.name);
                }
                npc.nav = nav;
                let left = (goal - pos).with_y(0.0).length();
                let to = (steer - pos).with_y(0.0);
                let d = to.length();
                npc.path = Some((left, d));
                if left > 0.35 && d > 0.05 {
                    wish = to / d * speed * left.min(1.0).max(0.4);
                }
            }
        }
        npc.wish = wish;
        // standing, its way more than 15 degrees off its facing, it turns where it stands first
        // (faced at once, shown turning by its turn's clip, `npc_select_anim`), then sets off:
        // the locomotion component's static turn, below `m_fMaxSpeedForStaticTurn` 40 uu/s and
        // over 15 degrees, its path's speed 0 for an idle mode meanwhile, as roshan's r614/blink
        // research decoded it (fighting or fleeing, it goes as it faces)
        if matches!(npc.mode, Mode::Idle | Mode::Patrol | Mode::Investigate | Mode::Search) && npc.pace.is_none() && !frozen && lunge.is_none() && wish.length_squared() > 0.01 {
            let way = (-wish.x).atan2(-wish.z);
            let off = (way - npc.yaw + PI).rem_euclid(TAU) - PI;
            if npc.turning || (npc.velocity.with_y(0.0).length() < 0.4 && off.abs() > 15f32.to_radians()) {
                if !npc.turning {
                    npc.yaw = way;
                }
                wish = Vec3::ZERO;
            }
        }
        // its walk begun or ended with its clip (`npc_select_anim`): the clip's pace carries it,
        // a start its path's way, a stop on its way
        if frozen {
            npc.pace = None;
        }
        let paced = match npc.pace {
            Some(p) => {
                wish = p.way.unwrap_or_else(|| wish.normalize_or(npc.forward())) * p.now();
                let t = p.t + dt;
                npc.pace = (t < p.len).then_some(Pace { t, ..p });
                if t >= p.len && p.way.is_some() && p.turn.is_none() {
                    npc.halted = true;
                }
                true
            }
            None => false,
        };
        // a weeper's grab: the lunge closes on Corvo, then it stands its ground (a wolfhound's
        // leap lasts its clip and lands at his arm, `Attack_ArmAttack_GrabLoop_Minigame`'s
        // `anchor_jnt` 0.57 m on; its clip carries it as it lets go or misses)
        let (leap, close) = if npc.grab_len > 0.0 { (npc.grab_len - 0.05, if npc.grab_jump { 1.0 } else { 0.6 }) } else { (0.55, 0.9) };
        match (npc.grab, ppos) {
            (Some((GrabPhase::Try, el)), Some(p)) if el < leap && npc.stagger <= 0.0 => {
                let to = (p - pos).with_y(0.0);
                let d = to.length();
                // (a wolfhound's leap covers its 8 m)
                let cap = if is_hound(&npc) { 16.0 } else { 8.0 };
                wish = if d > close { to / d * ((d - close) / (leap - el).max(0.15)).min(cap) } else { Vec3::ZERO };
            }
            (Some((GrabPhase::Release | GrabPhase::Miss | GrabPhase::Kill, el)), _) if npc.grab_len > 0.0 && el < npc.grab_len => wish = npc.forward() * npc.grab_drift,
            (Some(_), _) => wish = Vec3::ZERO,
            _ => {}
        }
        // the danger zones' push and the avoidance's bend its way as it moves (`danger`, `avoid`;
        // the locomotion 0x0094A630: its heading and the pushes, together at most a full stride;
        // standing, it isn't moved)
        let pushed = (npc.danger + npc.avoid).with_y(0.0);
        if pushed.length_squared() > 1e-8 && npc.grab.is_none() {
            let s = wish.length();
            if s > 0.05 {
                wish = (wish / s + pushed.clamp_length_max(1.0)).clamp_length_max(1.0) * s;
            }
        }
        // stuck handling: sidestep perpendicular to the desired direction (counted only while
        // it already goes at some speed: one speeding up slowly isn't stuck)
        if wish.length_squared() > 0.01 && !paced && npc.velocity.with_y(0.0).length() > 0.4 {
            npc.stuck += dt;
            if npc.stuck > 0.8 {
                // (one at a time: a search's count runs on, and would pick a side each frame)
                if pos.distance(npc.progress_pos) < 0.3 && npc.sidestep == 0.0 {
                    npc.sidestep = 0.7 * if rand::random::<bool>() { 1.0 } else { -1.0 };
                    if std::env::var_os("DH_AVOID_LOG").is_some() {
                        info!("stuck: {} sidesteps at {pos:.1} ({:?})", npc.name, npc.mode);
                    }
                }
                npc.progress_pos = pos;
                npc.stuck = if npc.mode == Mode::Search { npc.stuck } else { 0.0 };
            }
        }
        if npc.sidestep.abs() > 0.0 {
            let side = Vec3::new(-wish.z, 0.0, wish.x).normalize_or_zero();
            wish = wish * 0.3 + side * npc.sidestep.signum() * speed;
            npc.sidestep = (npc.sidestep.abs() - dt).max(0.0) * npc.sidestep.signum();
        }
        let horiz = Vec3::new(npc.velocity.x, 0.0, npc.velocity.z);
        // (not lunging, held back by a blow, a hit or a hold, nor grabbing: those go and stop at once)
        let speedup = npc.arms.turning.speedup(npc.stance, npc.mode).filter(|_| npc.grab.is_none() && !frozen && lunge.is_none());
        let new_h = if paced {
            wish
        } else if let Some([up, down]) = speedup {
            // its speed eases at its modifier's acceleration and deceleration (`m_fAcceleration`,
            // `m_fDeceleration`), slowing in time to stop at its path's end; its way is its
            // path's at once (the locomotion's, 0x0094A630)
            let (s0, mut s1) = (horiz.length(), wish.length());
            if let Some((left, _)) = npc.path {
                s1 = s1.min((2.0 * down * left).sqrt());
            }
            let s = if s1 > s0 { (s0 + up * dt).min(s1) } else { (s0 - down * dt).max(s1) };
            let way = if wish.length_squared() > 1e-6 { wish.normalize() } else { horiz.normalize_or_zero() };
            way * s
        } else {
            horiz + (wish - horiz) * (if npc.grab.is_some() { 20.0 } else { 8.0 } * dt).min(1.0)
        };
        if new_h.length() > 0.5 && npc.pace.is_none_or(|p| p.way.is_none()) {
            npc.halted = false;
        }
        npc.velocity.x = new_h.x;
        npc.velocity.z = new_h.z;
        if npc.grounded {
            npc.velocity.y = -1.0;
        } else {
            npc.velocity.y = (npc.velocity.y - 19.0 * dt).max(-30.0);
        }
        if far {
            kcc.translation = None;
            let mut p = t.translation + Vec3::new(npc.velocity.x, 0.0, npc.velocity.z) * dt;
            // (the floor looked for from a step's height above its feet, as its controller
            // steps: a wall in its way is no floor to climb, and it stops at it)
            let top = p.y - NPC_CENTER + FAR_STEP;
            let hit = ctx.as_ref().and_then(|c| c.cast_ray(p.with_y(top), Vec3::NEG_Y, FAR_STEP + 2.4, true, ground_filter));
            match hit {
                Some((_, toi)) if toi <= 0.0 => {
                    p = t.translation;
                    npc.velocity.x = 0.0;
                    npc.velocity.z = 0.0;
                }
                Some((_, toi)) => {
                    let ground = top - toi + NPC_CENTER + 0.03;
                    p.y += (ground - p.y) * (12.0 * dt).min(1.0);
                    npc.grounded = true;
                    npc.velocity.y = 0.0;
                }
                None => {
                    // over a gap: let the controller take over again next frame
                    npc.grounded = false;
                }
            }
            t.translation = p;
        } else if npc.grounded && new_h.length_squared() < 1e-4 && wish == Vec3::ZERO {
            // standing still on the ground: nothing for the controller to resolve
            kcc.translation = None;
        } else {
            kcc.translation = Some(npc.velocity * dt);
        }
        // facing: movement direction, or the player when fighting at close range
        let (mut walking, mut engaging) = (false, false);
        let face = match (npc.mode, ppos) {
            (Mode::Combat, Some(p)) if npc.sees_player && p.distance(pos) < 6.0 => {
                engaging = true;
                Some((p - pos).with_y(0.0))
            }
            (Mode::Combat, _) if npc.last_seen.map(|l| l.distance(pos) < 6.0).unwrap_or(false) && new_h.length() < 1.0 => {
                npc.last_seen.map(|l| (l - pos).with_y(0.0))
            }
            _ if new_h.length() > 0.3 => {
                walking = true;
                Some(new_h)
            }
            _ if npc.attn_level == 2 && npc.alert == Alert::Unaware && npc.target.is_none() => npc.last_seen.map(|l| (l - pos).with_y(0.0)),
            (_, Some(p)) if npc.warn.is_some() => Some((p - pos).with_y(0.0)),
            // (paused: a quarter from the heading it walked, to the side what it watches is on)
            _ if npc.track_pause.is_some() => npc.last_seen.zip(npc.track_pause).map(|(l, h)| {
                let d = l - pos;
                let side = ((-d.x).atan2(-d.z) - (h + PI) + PI).rem_euclid(TAU) - PI;
                Quat::from_rotation_y(h + PI + FRAC_PI_2.copysign(side)) * Vec3::NEG_Z
            }),
            (Mode::Idle, _) if npc.target.is_none() => Some(Quat::from_rotation_y(npc.home_yaw) * Vec3::NEG_Z),
            // (guarding a point of its route: the point's way)
            (Mode::Patrol, _) if npc.target.is_none() => npc.patrol.face.map(|y| Quat::from_rotation_y(y) * Vec3::NEG_Z),
            _ => None,
        };
        // (turning with its clip: faced the new way already, shown turning: `npc_select_anim`)
        let face = face.filter(|_| npc.pace.is_none_or(|p| p.turn.is_none()));
        if let Some(f) = face {
            if f.length_squared() > 1e-4 {
                let target_yaw = (-f.x).atan2(-f.z);
                let dy = (target_yaw - npc.yaw + PI).rem_euclid(TAU) - PI;
                // walking, at its speed mode's turn rate (`LocoTurn`); facing Corvo close, at its
                // melee engagement's face-to rate (`DisTweaks_AISubState_MeleeEngage`'s
                // `m_fRotationSpeed`, 600 degrees a second); else as before
                let rate = if walking {
                    npc.arms.turning.rate(npc.stance, npc.mode, npc.running, npc.arms.plague.is_some())
                } else if engaging && npc.arms.turning.accel_mult > 0.0 {
                    Some(engage_turn)
                } else {
                    None
                };
                if let Some(rate) = rate {
                    let tw = npc.arms.turning;
                    // (a sharp turn on its path: faster with a wall ahead, faster still with one
                    // only to the side it turns)
                    let mut mult = 1.0;
                    if walking && npc.path.is_some() && dy.abs() > tw.wall_angle {
                        let wall = |dir: Vec3, len: f32| ctx.as_ref().is_some_and(|c| c.cast_ray(pos, dir, len, true, ground_filter).is_some());
                        let ahead = npc.forward();
                        let side = Quat::from_rotation_y(dy.signum() * FRAC_PI_2) * ahead;
                        mult = if wall(ahead, tw.wall_front) { tw.wall_mult } else if wall(side, tw.wall_side) { tw.side_mult } else { 1.0 };
                    }
                    let mut v = npc.yaw_v;
                    npc.yaw += turn_toward(dy, &mut v, rate * mult, rate * mult * tw.accel_mult, dt);
                    npc.yaw_v = v;
                } else {
                    let max = 6.0 * dt;
                    npc.yaw += dy.clamp(-max, max);
                    npc.yaw_v = 0.0;
                }
            }
        }
        if !npc.is_down() {
            t.rotation = Quat::from_rotation_y(npc.yaw);
        }
        let sp = new_h.length();
        npc.anim_speed += (sp - npc.anim_speed) * (10.0 * dt).min(1.0);
        // (a foot down twice a loop of its walk or run, or a man's stride before it plays one)
        let stride = if npc.stride > 0.5 { npc.stride } else if npc.anim_speed > 3.0 { 2.4 } else { 1.5 };
        let before = npc.anim_phase;
        npc.anim_phase = (npc.anim_phase + npc.anim_speed * dt * TAU / stride).rem_euclid(TAU);
        // a foot down every half cycle (near the player only); with its loops playing, as each
        // foot comes down on them: as the sync position enters a foot's on-ground share, around
        // its `m_fOnGroundPct` (left 0.44, right 0.94; the locomotion node's `m_bFootOnGround`,
        // 0x009114A0, as roshan's r614/blink research decoded it)
        let crossed = match npc.sync {
            Some((ph, share)) => {
                let was = std::mem::replace(&mut npc.foot_sync, ph);
                [0.44f32, 0.94].iter().any(|on| {
                    let down = (on - share * 0.5).rem_euclid(1.0);
                    if ph >= was { was < down && down <= ph } else { was < down || down <= ph }
                })
            }
            None => npc.anim_phase < before || (before < PI && npc.anim_phase >= PI),
        };
        let near = ppos.is_some_and(|p| p.distance(t.translation) < 30.0);
        if crossed && near && npc.anim_speed > 0.4 && npc.grounded && !npc.is_down() {
            use crate::footsteps::{Footfall, Gait, Walker};
            let n = npc.name.to_ascii_lowercase();
            let who = if n.contains("wolf") || n.contains("hound") {
                Walker::Wolfhound
            } else if n.contains("rat") && !n.contains("pirate") {
                Walker::Rat
            } else if n.contains("tallboy") {
                Walker::Tallboy
            } else {
                Walker::Guard
            };
            let gait = if npc.anim_speed > 3.0 { Gait::Sprint } else { Gait::Walk };
            steps.write(Footfall { pos: t.translation, gait, who, by: Some(me) });
        }
        if t.translation.y < kill_y {
            npc.set_mode(Mode::Dead);
        }
    }
}

/// Procedural animation on the original skeletons. Rotations are authored in the
/// skeleton's bind space (character facing +X, up +Y, right +Z) and converted to joint
/// local space as P⁻¹·R·W (P = parent bind world rotation, W = joint bind world rotation).
fn npc_animate(
    time: Res<Time>,
    tc: Res<TimeControl>,
    npcs: Query<(&Npc, &NpcRig, &Children), Without<Animator>>,
    mut visuals: Query<&mut Transform, (With<NpcVisual>, Without<Npc>)>,
    mut joints: Query<&mut Transform, (Without<NpcVisual>, Without<Npc>)>,
) {
    for (npc, rig, children) in &npcs {
        let t_now = time.elapsed_secs() * if tc.npc_scale(npc.out_of_bend) > 0.0 { 1.0 } else { 0.0 };
        // body orientation (falling down)
        for c in children.iter() {
            if let Ok(mut vt) = visuals.get_mut(c) {
                let face = Quat::from_rotation_y(FRAC_PI_2);
                if npc.is_down() {
                    let k = ease(npc.down_t);
                    let fall = Quat::from_rotation_x(FRAC_PI_2 * k);
                    let pivot = Vec3::new(0.0, -NPC_CENTER, 0.0);
                    vt.rotation = fall * face;
                    vt.translation = pivot + fall * (-pivot) + Vec3::Y * 0.12 * k;
                } else if npc.mode == Mode::Choked {
                    vt.rotation = Quat::from_rotation_x(-0.25) * face;
                    vt.translation = Vec3::new(0.0, -0.15, 0.0);
                } else {
                    vt.rotation = face;
                    vt.translation = Vec3::ZERO;
                }
            }
        }
        let sp = (npc.anim_speed / 3.5).clamp(0.0, 1.3);
        let ph = npc.anim_phase;
        let mut rots: Vec<(Option<usize>, Quat)> = Vec::with_capacity(16);
        let lower = 1.22; // bring arms down from the T-pose
        let swing = 0.55 * sp * ph.sin();
        let arm_swing = 0.5 * sp * ph.sin();
        let downed = npc.is_down();
        let breath = (t_now * 1.7 + ph).sin() * 0.02;
        // legs
        if !downed {
            rots.push((rig.idx.thigh[0], Quat::from_rotation_z(swing)));
            rots.push((rig.idx.thigh[1], Quat::from_rotation_z(-swing)));
            let kl = -(0.1 + 0.9 * sp * (ph + 0.6).cos().max(0.0));
            let kr = -(0.1 + 0.9 * sp * (ph + 0.6 + PI).cos().max(0.0));
            rots.push((rig.idx.calf[0], Quat::from_rotation_z(-kl)));
            rots.push((rig.idx.calf[1], Quat::from_rotation_z(-kr)));
        }
        // arms
        // arms swing opposite to the legs; elbows bend forward (+X)
        let mut right_arm = Quat::from_rotation_z(arm_swing) * Quat::from_rotation_x(lower);
        let left_arm = Quat::from_rotation_z(-arm_swing) * Quat::from_rotation_x(-lower);
        let mut right_elbow = Quat::from_rotation_y(0.3);
        let mut spine_twist = Quat::IDENTITY;
        if let Some(a) = npc.attack_t {
            // wind up (raise sword back), then strike down/forward
            let (raise, twist) = if a < 0.45 {
                let k = ease(a / 0.45);
                (1.9 * k, 0.5 * k)
            } else {
                let k = ease(((a - 0.45) / 0.2).min(1.0));
                (1.9 - 2.6 * k, 0.5 - 0.9 * k)
            };
            right_arm = Quat::from_rotation_z(raise) * Quat::from_rotation_x(lower * (1.0 - raise.clamp(0.0, 1.0) * 0.6));
            right_elbow = Quat::from_rotation_y(0.6);
            spine_twist = Quat::from_rotation_y(twist);
        } else if npc.mode == Mode::Combat && npc.sees_player {
            // guard stance: sword forward
            right_arm = Quat::from_rotation_z(0.9) * Quat::from_rotation_x(lower * 0.75);
            right_elbow = Quat::from_rotation_y(0.7);
        }
        if npc.hit_react > 0.0 {
            spine_twist *= Quat::from_rotation_z(0.35 * npc.hit_react / 0.35);
        }
        if npc.mode == Mode::Choked {
            right_arm = Quat::from_rotation_z(1.2 + (t_now * 9.0).sin() * 0.3) * Quat::from_rotation_x(lower * 0.5);
        }
        if !downed {
            rots.push((rig.idx.upper_arm[0], left_arm));
            rots.push((rig.idx.upper_arm[1], right_arm));
            rots.push((rig.idx.lower_arm[0], Quat::from_rotation_y(-0.3)));
            rots.push((rig.idx.lower_arm[1], right_elbow));
            let lean = if npc.anim_speed > 3.0 { -0.12 } else { 0.0 };
            rots.push((rig.idx.spine[1], Quat::from_rotation_z(lean + breath) * spine_twist));
            let look = if npc.mode == Mode::Search { (t_now * 0.9).sin() * 0.7 } else { npc.look };
            rots.push((rig.idx.neck, Quat::from_rotation_y(look * 0.6)));
        } else {
            // limp pose
            rots.push((rig.idx.upper_arm[0], Quat::from_rotation_x(-0.6)));
            rots.push((rig.idx.upper_arm[1], Quat::from_rotation_x(0.6)));
            rots.push((rig.idx.neck, Quat::from_rotation_z(0.3)));
        }
        for (i, r) in rots {
            let Some(i) = i else { continue };
            let Some(&je) = rig.joints.get(i) else { continue };
            let Ok(mut jt) = joints.get_mut(je) else { continue };
            let p = rig.parent[i];
            let pw = if p >= 0 { rig.bind_world[p as usize] } else { Quat::IDENTITY };
            jt.rotation = pw.inverse() * r * rig.bind_world[i];
        }
        // pelvis bob
        if let Some(ri) = rig.idx.root {
            if let Ok(mut jt) = joints.get_mut(rig.joints[ri]) {
                let bob = if downed { 0.0 } else { -(2.0 * ph).cos().abs() * 0.03 * sp };
                let p = rig.parent[ri];
                let pw = if p >= 0 { rig.bind_world[p as usize] } else { Quat::IDENTITY };
                jt.translation = rig.bind_t[ri] + pw.inverse() * (Vec3::Y * bob);
            }
        }
    }
}

/// The clips an NPC uses for each situation, looked up by the original sequence names.
pub struct NpcClips {
    idle: Option<ClipId>,
    walk: Option<ClipId>,
    idle_alert: Option<ClipId>,
    walk_alert: Option<ClipId>,
    run: Option<ClipId>,
    /// its stances' locomotion (`LOCO_EMPTY` ...)
    loco: [LocoSet; 5],
    flee: Option<ClipId>,
    attack: Vec<ClipId>,
    /// the moves' own: a short blow (small, big), a medium and a long lunge, a kick
    attack_short: Vec<ClipId>,
    attack_short_big: Vec<ClipId>,
    attack_medium: Vec<ClipId>,
    attack_long: Vec<ClipId>,
    attack_long_big: Vec<ClipId>,
    attack_step_back: Option<ClipId>,
    /// a thug's fire breath (its swig, the spitting, the end) and its bottle's throw
    bottle_fire: [Option<ClipId>; 3],
    bottle_throw: Option<ClipId>,
    /// a throw with the bare hand (a rock from the floor, from the pocket)
    throw_rock: Vec<ClipId>,
    kick: Option<ClipId>,
    /// the turning blows (left, right; at 90 and 180 degrees) and the charge
    attack_turn: [Vec<ClipId>; 4],
    jump_attack: Option<ClipId>,
    /// its parry won, its blow parried, a shove
    parry_win: Vec<ClipId>,
    parry_lost: Vec<ClipId>,
    push: Vec<ClipId>,
    /// a sword lock: the struggle, lost badly, lost, won
    versus: [Vec<ClipId>; 4],
    /// a rat stamped on
    stomp: Vec<ClipId>,
    /// an ambush: into hiding, waiting, springing out, giving up
    ambush: [Vec<ClipId>; 4],
    hit: Vec<ClipId>,
    hit_empty: Vec<ClipId>,
    /// the hit reactions and deaths by the side struck: front, left, right, back
    hit_dir: [Vec<ClipId>; 4],
    hit_dir_empty: [Vec<ClipId>; 4],
    /// shot in the left, right leg, sword drawn and empty-handed (`Sword_HitLeg_Left`,
    /// `Empty_HitLeg_Right`)
    hit_leg: [[Option<ClipId>; 2]; 2],
    /// struck by a thing thrown, by the side it struck: front, left, right, back
    /// (`Generic_SmallHit_Front`..., with their stone-on-body cue)
    small_hit: [Option<ClipId>; 4],
    /// a weak hit's flinch, laid over the pose (`HitReactSmall_Additive`:
    /// `ADD_Generic_SmallHit_Front`, `_Left`, `_Right`, `_Back`)
    weak_hit: [Option<ClipId>; 4],
    /// shot from ahead, ahead left, ahead right (`Generic_ProjectileHit_Front`, `_FrontLeft`,
    /// `_FrontRight`: the elite guards')
    shot_hit: [Option<ClipId>; 3],
    /// choking in chokedust: in, its loop, out (`Generic_SmokeBomb_In`...: Daud's elite guards'
    /// sets); without them, doubled over and up again, sword drawn and empty-handed
    /// (`Sword_Stun_Loop`, `Sword_RecoveryA` / `B`)
    dust: [Option<ClipId>; 3],
    stun_loop: [Option<ClipId>; 2],
    recovery: [Vec<ClipId>; 2],
    death_dir: [Option<ClipId>; 4],
    stun: Option<ClipId>,
    /// the eyelids' `Blink`
    blink: Option<ClipId>,
    death: Vec<ClipId>,
    death_fast: Option<ClipId>,
    /// its dodge (`DisNPCAnim_Dodge`): in, out, by side (left, right), and its riposte out of
    /// it (`DisNPCAnim_RiposteLeft`, `_Right`: `Sword_DodgeLeftAttack_Small`, `_Big`)
    dodge_in: [Vec<ClipId>; 2],
    dodge_out: [Option<ClipId>; 2],
    riposte: [Vec<ClipId>; 2],
    /// a wall of light's death (`DisNPCAnim_DeathWallOfLight`), and burning to death
    /// (`DisNPCAnim_Immolate`)
    electrified: Option<ClipId>,
    immolate: Option<ClipId>,
    /// lying poses (last frames) to settle into after a reaction that ends mid-fall
    lie_back: Option<ClipId>,
    lie_front: Option<ClipId>,
    assassinated: Vec<ClipId>,
    choke_in: Option<ClipId>,
    choke_loop: Option<ClipId>,
    choke_out: Option<ClipId>,
    corpse: Vec<ClipId>,
    search: Vec<ClipId>,
    /// Corvo lost in a fight (`DisNPCAnim_SearchLostTarget`): its sword lowered to its patrol
    /// stance (`Sword_SwordToPatrol`), and its look about, empty-handed and sword drawn
    /// (`Empty_OutOfSight`, `Sword_OutOfSight`)
    sword_to_patrol: Option<ClipId>,
    out_of_sight: [Option<ClipId>; 2],
    surprised: Option<ClipId>,
    /// startled by a live explosive, sword sheathed or drawn (`Empty_Surprised_Front`,
    /// `Sword_Surprised_Front`)
    startled: [Option<ClipId>; 2],
    /// its sword drawn: into its patrol stance, to search (`DisNPCAnim_EquipToSearch`:
    /// `Sword_Patrol_Equip`); into its fighting stance (`DisNPCAnim_EquipToCombat`:
    /// `Sword_Equip`); and, drawn in its patrol stance, raised to fight
    /// (`Sword_Patrol_PatrolToSword`)
    equip: Option<ClipId>,
    equip_combat: Option<ClipId>,
    patrol_to_sword: Option<ClipId>,
    /// its sword put back as it calms down (`Sword_Patrol_UnEquip`, `Sword_UnEquip`)
    unequip: Option<ClipId>,
    /// a weeper's arm grab
    grab_try: Option<ClipId>,
    grab_in: Option<ClipId>,
    grab_loop: Option<ClipId>,
    grab_out: Option<ClipId>,
    grab_miss: Option<ClipId>,
    /// a wolfhound's kill, Corvo dead in its teeth (`Attack_ArmAttack_TransToKill_Minigame`, then
    /// `Attack_KillPlayer_Minigame`)
    grab_kill: [Option<ClipId>; 2],
    /// a wolfhound's jump attack: the leap, its step back after, its kill
    /// (`Attack_JumpAttack_Jump_Minigame`, `_StepBack_`, `_TransToKill_Minigame`)
    jump: [Option<ClipId>; 3],
    /// killed from above, by side (front, back, left, right)
    drop_killed: [Option<ClipId>; 4],
    /// the pistol drawn, aimed, fired and put away (`Npc_EliteGuard_Gun_as`: `Sword_Gun_Draw`,
    /// `Sword_Gun_Idle`, `Sword_Gun_Fire`, `Sword_Gun_Undraw`)
    gun: [Option<ClipId>; 4],
    /// the pistol reloaded after each shot (`Sword_Gun_Reload`: `DisAIPistolState_Reloading`)
    gun_reload: Option<ClipId>,
    /// a dodge's step (left, right, back: `Sword_SideStep_*`), the alarm's lever pulled
    /// (`Sword_TriggerAlarm`), a body found looked over (`Sword_CorpseCheck`)
    dodge: [Vec<ClipId>; 3],
    /// Corvo out of it: thrown off as he comes out, then dazed or retching
    /// (`Empty_Unpossess_Eject`, `_Dizzy` with its line, `_Puke` with its sick)
    unpossess: [Option<ClipId>; 3],
    /// a whaler's Attract spell: its hand raised, held, its coup de grace, let go
    /// (`Sword_Power_Attract_In`, `_Loop`, `_Attack`, `_Cancel`)
    attract: [Option<ClipId>; 4],
    /// an assassin's wrist bow shot
    wrist_shot: Option<ClipId>,
    /// a witch's scream, her magic and a tentacle called up (`Sword_Attack_Scream`,
    /// `Sword_Power_RangedAttack`, `Sword_Power_SummonTentacle`)
    witch: [Option<ClipId>; 3],
    /// a tallboy's bow: drawn, held, loosed, let down (`Empty_Bow_Charged_In`, `_Charged`,
    /// `Empty_Bow_Fire`, `_Charged_Out`)
    bow: [Option<ClipId>; 4],
    /// getting up from where it lay, on its back and on its front (`Generic_RagdollToBack`,
    /// `_ToFront`)
    get_up: [Option<ClipId>; 2],
    /// thrown, pushed from the front, left, right, back: its moment before it goes limp, to its
    /// `DishonoredNotify_Ragdoll` (the original's thrown state, as roshan's r614/blink research
    /// read it): knocked down (`Generic_PushToRagdoll_Front`...), blown away
    /// (`Generic_WindBlast_Front`..., else the knock's)
    push_limp: [[Option<ClipId>; 4]; 2],
    /// an assassin's slash out of its dodge to the left, right (`Sword_SideStepLeft_DodgeAttack_Small`)
    dodge_attack: [Option<ClipId>; 2],
    /// Corvo gone before its eyes by magic (`Empty_Magic_OutOfSight`, `Sword_Magic_OutOfSight`:
    /// the original's `DisBehaviorMagicResponse`, its Awe slot)
    magic: [Vec<ClipId>; 2],
    /// a sticky grenade stuck to it, flailing at it (`Generic_StickyWeapon`)
    sticky: Vec<ClipId>,
    /// fighting off a swarm on it, empty-handed and sword drawn: in, its loop, out
    swarm: [[Option<ClipId>; 3]; 2],
    trigger_alarm: Vec<ClipId>,
    corpse_check: Vec<ClipId>,
    /// striking under a table, with the blade and with the pistol
    under: [Option<ClipId>; 2],
    /// its taunts (each cueing its line, `COMBAT_TAUNT_WITH_GESTURE`)
    taunt: Vec<ClipId>,
    /// begging on its knees: in, the loop, up again (`Empty_Beg_Kneeling_*`); else cowering where
    /// it stands (`Empty_Idle_Panic_Recover`, the cower sub-state's stance)
    beg: [Option<ClipId>; 3],
    cower: Option<ClipId>,
    /// a musical Overseer's arms: holding its music box (`Musicbox_Idle`), and cranking it
    /// (`MusicBox_PlayMusic_In`, `_Loop`, `_Out`)
    music_box: [Option<ClipId>; 4],
    /// a weeper's cough (`DisNPCAnim_Weeper_Cough`)
    cough: Vec<ClipId>,
    /// its pockets checked, robbed (`Empty_CheckPockets`: the guards')
    check_pockets: Vec<ClipId>,
    /// a whale butcher's saw spraying shrapnel: raised, held spraying, lowered
    /// (`Saw_RangeAttack_In`, `_Loop`, `_Out`)
    saw_fire: [Option<ClipId>; 3],
    /// its aim offsets (`ADD_Sword_Gun_AimIdle_*`): up, centre and down rows, left to right
    gun_aim: [Option<ClipId>; 9],
    /// its look-at grids (`LOOK_MODES`), by stance and gait: the torso's and the head's
    look: [[[[Option<ClipId>; 9]; 2]; 3]; 3],
}

impl NpcClips {
    /// None unless the character can at least stand and walk.
    pub fn resolve(lib: &CharAnims, prefix: &str) -> Option<NpcClips> {
        let c = NpcClips {
            // humans first, then the wolfhound's own sets (Patrol_ / Attack_)
            // (sheathed, as a guard patrols: its empty hands' idle and patrol walk, measured on the
            // original by roshan's r614/blink research at `EmptyPatrol`'s pace on `Empty_Walk_Patrol`)
            idle: lib.first(&["Empty_Idle", "Sword_Patrol_Idle", "Sword_Idle", "Patrol_Idle"]),
            walk: lib.first(&["Empty_Walk_Patrol", "Empty_WalkN", "Sword_Patrol_WalkN", "Sword_WalkN", "Patrol_WalkN"]),
            loco: Default::default(),
            // (a weeper weeps standing: `DisNPCAnim_Weep`)
            idle_alert: lib.first(&["Sword_Idle", "Empty_Weep_Idle", "Empty_Idle", "Sword_Patrol_Idle", "Attack_idle", "Patrol_Idle"]),
            walk_alert: lib.first(&["Sword_WalkN", "Empty_WalkN", "Attack_WalkN", "Patrol_TrotN"]),
            run: lib.first(&["Sword_RunN", "Empty_RunN", "Patrol_RunN"]),
            flee: lib.first(&["Empty_RunN_Panic1", "Sword_RunN_Panic", "Empty_RunN", "Sword_RunN", "Patrol_RunN"]),
            attack: lib.all(&[
                "Sword_Attack_ShortBigA",
                "Sword_Attack_ShortBigB",
                "Sword_Attack_ShortBigC",
                "Sword_Attack_ShortBigD",
                "Sword_Attack_ShortBigE",
                "Sword_Attack_FastA",
                "Sword_Attack_FastB",
                "Attack_Medium",
                "Empty_StompAttack",
            ]),
            attack_short: lib.all(&["Sword_Attack_ShortSmallA", "Sword_Attack_ShortSmallB", "Sword_Attack_ShortSmallC", "Sword_Attack_ShortSmallD", "Sword_Attack_ShortSmallE", "Sword_Attack_FastA", "Sword_Attack_FastB"]),
            attack_short_big: lib.all(&["Sword_Attack_ShortBigA", "Sword_Attack_ShortBigB", "Sword_Attack_ShortBigC", "Sword_Attack_ShortBigD", "Sword_Attack_ShortBigE"]),
            attack_medium: lib.all(&["Sword_Attack_MediumSmallA", "Sword_Attack_MediumSmallB", "Sword_Attack_MediumBigA", "Attack_Medium"]),
            attack_long: lib.all(&["Sword_Attack_LongSmallA", "Sword_Attack_LongSmallB"]),
            attack_long_big: lib.all(&["Sword_Attack_LongBigA"]),
            attack_step_back: lib.first(&["Sword_Attack_StepBackA"]),
            bottle_fire: [lib.find("Sword_Bottle_Fire_In"), lib.find("Sword_Bottle_Fire_Loop"), lib.find("Sword_Bottle_Fire_Out")],
            bottle_throw: lib.find("Sword_ThrowBottle"),
            throw_rock: lib.all(&["Sword_ThrowRock_Floor", "Sword_ThrowRock_Pocket"]),
            kick: lib.first(&["Sword_Attack_KickMedium"]),
            attack_turn: [
                lib.all(&["Sword_Attack_Left90BigA", "Sword_Attack_Left90SmallA"]),
                lib.all(&["Sword_Attack_Right90BigA", "Sword_Attack_Right90SmallA"]),
                lib.all(&["Sword_Attack_Left180BigA", "Sword_Attack_Left180SmallA"]),
                lib.all(&["Sword_Attack_Right180BigA", "Sword_Attack_Right180SmallA"]),
            ],
            jump_attack: lib.first(&["Sword_Attack_JumpSmallA"]),
            parry_win: lib.all(&["Sword_Ready_ParryWin_LeftLeft", "Sword_Ready_ParryWin_LeftRight", "Sword_Ready_ParryWin_RightLeft", "Sword_Ready_ParryWin_RightRight"]),
            parry_lost: lib.all(&["Sword_Ready_ParryLost_LeftLeft", "Sword_Ready_ParryLost_LeftRight", "Sword_Ready_ParryLost_RightLeft", "Sword_Ready_ParryLost_RightRight"]),
            push: lib.all(&["Sword_PushBack_A", "Sword_PushBack_B"]),
            stomp: lib.all(&["Sword_StompRat", "Sword_StompRatff", "Empty_StompRat"]),
            ambush: [
                lib.all(&["Sword_Ambush_In"]),
                lib.all(&["Sword_Ambush_Loop", "Sword_Ambush_LoopDD"]),
                lib.all(&["Sword_Ambush_Out", "Sword_Ambush_Out33"]),
                lib.all(&["Sword_Ambush_Cancel", "Sword_Ambush_Cancelfff"]),
            ],
            versus: [lib.all(&["Sword_Versus_MinigameUU"]), lib.all(&["Sword_Versus_BigLose"]), lib.all(&["Sword_Versus_MediumLoseDD"]), lib.all(&["Sword_Versus_MediumWin", "Sword_Versus_BigWin"])],
            // (with the sword in hand; the empty-handed ones flail their arms)
            hit: lib.all(&[
                "Sword_HitFront_A",
                "Sword_HitFront_Left",
                "Sword_HitFront_Right",
                "Sword_HitFront_Head",
                "Sword_HitFront_Stomach",
                "Attack_HitFront",
                "Attack_HitLeft",
                "Attack_HitRight",
            ]),
            hit_empty: lib.all(&["Empty_HitFront_A", "Empty_HitFront_Left", "Empty_HitFront_Right", "Empty_HitFront_Head", "Empty_HitFront_Stomach"]),
            // (by the side struck, as the clips are named for it: front, left, right, back)
            hit_dir: [
                lib.all(&["Sword_HitFront_A", "Sword_HitFront_Head", "Sword_HitFront_Stomach", "Sword_HitFront_Left", "Sword_HitFront_Right", "Attack_HitFront"]),
                lib.all(&["Sword_HitLeft_A", "Attack_HitLeft"]),
                lib.all(&["Sword_HitRight_A", "Attack_HitRight"]),
                lib.all(&["Sword_HitBack_A"]),
            ],
            hit_dir_empty: [
                lib.all(&["Empty_HitFront_A", "Empty_HitFront_Head", "Empty_HitFront_Stomach", "Empty_HitFront_Left", "Empty_HitFront_Right"]),
                lib.all(&["Empty_HitLeft_A"]),
                lib.all(&["Empty_HitRight_A"]),
                lib.all(&["Empty_HitBack_A"]),
            ],
            small_hit: ["Front", "Left", "Right", "Back"].map(|s| lib.find(&format!("Generic_SmallHit_{s}"))),
            weak_hit: ["Front", "Left", "Right", "Back"].map(|s| lib.find(&format!("ADD_Generic_SmallHit_{s}"))),
            shot_hit: ["Front", "FrontLeft", "FrontRight"].map(|s| lib.find(&format!("Generic_ProjectileHit_{s}"))),
            dust: ["In", "Loop", "Out"].map(|p| lib.find(&format!("Generic_SmokeBomb_{p}"))),
            stun_loop: [lib.find("Sword_Stun_Loop"), lib.find("Empty_Stun_Loop")],
            recovery: [lib.all(&["Sword_RecoveryA", "Sword_RecoveryB"]), lib.all(&["Empty_RecoveryA", "Empty_RecoveryB"])],
            hit_leg: [["Sword_", "Empty_"], ["Empty_", "Sword_"]].map(|sets| ["Left", "Right"].map(|s| sets.iter().find_map(|p| lib.find(&format!("{p}HitLeg_{s}"))))),
            death_dir: [
                lib.first(&["Generic_DeathFrontA", "Generic_DeathFront_A"]),
                lib.first(&["Generic_DeathLeftA", "Generic_DeathLeft_A"]),
                lib.first(&["Generic_DeathRightA", "Generic_DeathRight_A"]),
                lib.first(&["Generic_DeathBackA", "Generic_DeathBack_A"]),
            ],
            stun: lib.first(&["Sword_HitWindblast", "Empty_HitWindblast", "Patrol_Stun_Loop"]),
            blink: lib.find("Blink"),
            death: {
                let human = lib.all(&["Generic_DeathFrontA", "Generic_DeathBackA", "Generic_DeathLeftA", "Generic_DeathRightA"]);
                if human.is_empty() {
                    lib.all(&["Generic_DeathFront_A", "Generic_DeathBack_A", "Generic_DeathLeft_A", "Generic_DeathRight_A", "Generic_DeathA", "Generic_DeathB"])
                } else {
                    human
                }
            },
            death_fast: lib.find("Generic_DeathFast_Front"),
            electrified: lib.find("Generic_Electrified"),
            immolate: lib.find("Generic_Immolation"),
            dodge_in: ["Left", "Right"].map(|s| lib.all(&[&format!("Sword_Dodge{s}In")])),
            dodge_out: ["Left", "Right"].map(|s| lib.find(&format!("Sword_Dodge{s}Out"))),
            // (its riposte as its definition chains it: the small one, the big twice)
            riposte: ["Left", "Right"].map(|s| lib.all(&[&format!("Sword_Dodge{s}Attack_Small"), &format!("Sword_Dodge{s}Attack_Big"), &format!("Sword_Dodge{s}Attack_Big")])),
            lie_back: lib.first(&["Generic_DeathFrontA", "Corpses_DeathPose_OnBack1", "Generic_DeathFront_A", "Generic_DeathA"]),
            lie_front: lib.first(&["Generic_DeathBackA", "Generic_DeathFrontA", "Generic_DeathBack_A", "Generic_DeathB"]),
            assassinated: lib.all(&["Generic_Assassination_Back_Slave", "Generic_Assassination_FastBack_Slave"]),
            choke_in: lib.find("Generic_Choke_In_Slave"),
            choke_loop: lib.find("Generic_Choke_Loop_Slave"),
            choke_out: lib.find("Generic_Choke_Win_Slave"),
            corpse: lib.all(&["Corpses_DeathPose_OnBack1", "Corpses_DeathPose_OnBack2", "Corpses_DeathPose_OnBack3"]),
            // (what it noticed looked into, `DisNPCAnim_ReactToNoticing`: `PlayerCheck`)
            search: lib.all(&["Sword_PlayerCheck", "Empty_PlayerCheck", "Patrol_IdleSearch"]),
            sword_to_patrol: lib.find("Sword_SwordToPatrol"),
            out_of_sight: [lib.first(&["Empty_OutOfSight", "Sword_OutOfSight"]), lib.first(&["Sword_OutOfSight", "Empty_OutOfSight"])],
            surprised: lib.first(&["Sword_Surprised_Front", "Empty_Surprised_Front", "Attack_IdleBark_Short", "Empty_Taunt"]),
            startled: [lib.find("Empty_Surprised_Front"), lib.find("Sword_Surprised_Front")],
            equip: lib.first(&["Sword_Patrol_Equip", "Sword_Equip"]),
            equip_combat: lib.first(&["Sword_Equip", "Sword_Patrol_Equip"]),
            patrol_to_sword: lib.find("Sword_Patrol_PatrolToSword"),
            unequip: lib.first(&["Sword_Patrol_UnEquip", "Sword_UnEquip"]),
            // (a wolfhound's: its arm attack's, `DisNPCAnimDefinitions_Wolfhound`'s sync attack:
            // the leap, the bite on his arm, its hold, thrown off, or past him)
            grab_try: lib.first(&["Empty_ArmGrab_Try", "Attack_ArmAttack_JumpGlide_Minigame"]),
            grab_in: lib.first(&["Empty_ArmGrab_In", "Attack_ArmAttack_GrabArm_Minigame"]),
            grab_loop: lib.first(&["Empty_ArmGrab_Loop", "Attack_ArmAttack_GrabLoop_Minigame"]),
            grab_out: lib.first(&["Empty_ArmGrab_Out", "Attack_ArmAttack_GrabOut_Minigame"]),
            grab_miss: lib.first(&["Empty_ArmGrab_MissDD", "Attack_ArmAttack_GrabMiss_Minigame"]),
            grab_kill: [lib.find("Attack_ArmAttack_TransToKill_Minigame"), lib.find("Attack_KillPlayer_Minigame")],
            jump: ["Jump", "StepBack", "TransToKill"].map(|s| lib.find(&format!("Attack_JumpAttack_{s}_Minigame"))),
            drop_killed: [
                lib.find("Generic_Assassination_DropFront_Slave"),
                lib.find("Generic_Assassination_DropBack_Slave"),
                lib.find("Generic_Assassination_DropLeft_Slave"),
                lib.find("Generic_Assassination_DropRight_Slave"),
            ],
            gun: [lib.find("Sword_Gun_Draw"), lib.find("Sword_Gun_Idle"), lib.find("Sword_Gun_Fire"), lib.find("Sword_Gun_Undraw")],
            gun_reload: lib.find("Sword_Gun_Reload"),
            // (an assassin's: `Sword_SideStepLeft_DodgeIn`, its slash after, and `Sword_BackStep`)
            dodge: [lib.all(&["Sword_SideStep_Left", "Sword_SideStepLeft_DodgeIn"]), lib.all(&["Sword_SideStep_Right", "Sword_SideStepRight_DodgeIn"]), lib.all(&["Sword_SideStep_Back", "Sword_BackStep"])],
            unpossess: [lib.first(&["Empty_Unpossess_Eject", "Patrol_Unpossess_Eject"]), lib.first(&["Empty_Unpossess_Dizzy", "Patrol_Unpossess_Dizzy"]), lib.first(&["Empty_Unpossess_Puke", "Patrol_Unpossess_Puke"])],
            attract: ["In", "Loop", "Attack", "Cancel"].map(|p| lib.find(&format!("Sword_Power_Attract_{p}"))),
            wrist_shot: lib.find("Sword_RangedAttack"),
            witch: [lib.find("Sword_Attack_Scream"), lib.find("Sword_Power_RangedAttack"), lib.find("Sword_Power_SummonTentacle")],
            bow: [lib.find("Empty_Bow_Charged_In"), lib.find("Empty_Bow_Charged"), lib.find("Empty_Bow_Fire"), lib.find("Empty_Bow_Charged_Out")],
            get_up: [lib.find("Generic_RagdollToBack"), lib.find("Generic_RagdollToFront")],
            push_limp: [
                ["Front", "Left", "Right", "Back"].map(|s| lib.find(&format!("Generic_PushToRagdoll_{s}"))),
                ["Front", "Left", "Right", "Back"].map(|s| lib.find(&format!("Generic_WindBlast_{s}")).or_else(|| lib.find(&format!("Generic_PushToRagdoll_{s}")))),
            ],
            dodge_attack: [lib.find("Sword_SideStepLeft_DodgeAttack_Small"), lib.find("Sword_SideStepRight_DodgeAttack_Small")],
            magic: [lib.all(&["Empty_Magic_OutOfSight", "Sword_Magic_OutOfSight"]), lib.all(&["Sword_Magic_OutOfSight", "Empty_Magic_OutOfSight"])],
            sticky: lib.all(&["Generic_StickyWeapon"]),
            swarm: [["Empty_", "Sword_"], ["Sword_", "Empty_"]].map(|sets| ["In", "Loop", "Out"].map(|p| sets.iter().find_map(|s| lib.find(&format!("{s}SwarmAttack_{p}"))))),
            trigger_alarm: lib.all(&["Sword_TriggerAlarm"]),
            corpse_check: lib.all(&["Sword_CorpseCheck"]),
            under: [lib.find("Sword_Attack_UnderTable"), lib.find("Sword_Gun_AttackUnderTable")],
            saw_fire: [None; 3],
            // (a wolfhound's: its barks, `Attack_IdleBark01`-`04`, each with its own bark)
            taunt: lib.all(&["Sword_TauntA", "Sword_TauntB", "Sword_TauntC", "Attack_IdleBark01", "Attack_IdleBark02", "Attack_IdleBark03", "Attack_IdleBark04"]),
            cough: lib.all(&["Empty_Idle_Cough"]),
            check_pockets: lib.all(&["Empty_CheckPockets"]),
            beg: [lib.find("Empty_Beg_Kneeling_In"), lib.find("Empty_Beg_Kneeling_Loop"), lib.find("Empty_Beg_Kneeling_Out")],
            music_box: [lib.find("Musicbox_Idle"), lib.find("MusicBox_PlayMusic_In"), lib.find("MusicBox_PlayMusic_Loop"), lib.find("MusicBox_PlayMusic_Out")],
            cower: lib.find("Empty_Idle_Panic_Recover"),
            gun_aim: ["UpLeft", "Up", "UpRight", "CenterLeft", "Center", "CenterRight", "DownLeft", "Down", "DownRight"].map(|d| lib.find(&format!("ADD_Sword_Gun_AimIdle_{d}"))),
            look: ["Empty", "Sword", "Sword_Patrol"].map(|stance| {
                ["Idle", "Walk", "Run"].map(|gait| {
                    let gait = if stance == "Sword_Patrol" && gait == "Run" { "Walk" } else { gait };
                    ["Torso", "Head"].map(|part| {
                        ["UpLeft", "Up", "UpRight", "CenterLeft", "Center", "CenterRight", "DownLeft", "Down", "DownRight"].map(|d| lib.find(&format!("ADD_{stance}_Aim{part}{gait}_{d}")))
                    })
                })
            }),
        };
        // its stances' locomotion, as its locomotion config's speed modes list them: a gait's
        // loops by way (S, W, N, E: the `_Revert` loops S and W, played backwards)
        let ways = |base: &str| -> Option<[Option<(ClipId, bool)>; 4]> {
            let n = lib.find(&format!("{base}N"))?;
            let way = |d: &str| lib.find(&format!("{base}{d}_Revert")).map(|c| (c, true)).or_else(|| lib.find(&format!("{base}{d}")).map(|c| (c, false)));
            Some([way("S"), way("W"), Some((n, false)), lib.find(&format!("{base}E")).map(|c| (c, false))])
        };
        let first_ways = |bases: &[&str]| bases.iter().find_map(|b| ways(b)).unwrap_or([None; 4]);
        // (the ambient idles of its stance, `AmbientIdle`'s group for its hand: empty, holding
        // the sword; a tall thug's are named without the underscore; a wolfhound's)
        let fidgets: [(&str, &[&str]); 3] = [
            ("Sword_Patrol_Idle", &["Sword_Patrol_Idle_01", "Sword_Patrol_Idle_02", "Sword_Patrol_Idle_03", "Sword_Patrol_Idle_04"]),
            ("Empty_Idle", &["Empty_Idle_01", "Empty_Idle_02", "Empty_Idle_03", "Empty_Idle_04", "Empty_Idle_05", "Empty_Idle_06", "Empty_Idle_07", "Empty_Idle_08", "Empty_Idle02", "Empty_Idle05", "Empty_Idle07"]),
            ("Patrol_Idle", &["Patrol_Idle_01", "Patrol_Idle_02", "Patrol_Idle_03", "Patrol_Idle_04"]),
        ];
        // (its starts, stops and turns by its idle's prefix, `Empty_`, `Sword_Patrol_`,
        // `Sword_`: its idle speed mode's start, stop and turn props, its walk's and run's turns
        // round; turning where it stands, else the first of `turns` it has)
        let set = |idle: Option<ClipId>, walk: [Option<(ClipId, bool)>; 4], run: [Option<(ClipId, bool)>; 4], turns: &[&str]| -> LocoSet {
            let name = idle.map(|i| lib.clip(i).name.clone()).unwrap_or_default();
            let prefix = name.strip_suffix("Idle").map(str::to_string);
            let by = |n: &str| prefix.as_ref().and_then(|p| lib.find(&format!("{p}{n}")));
            let run_prefix = run[2].and_then(|(r, _)| lib.clip(r).name.strip_suffix("RunN").map(str::to_string));
            LocoSet {
                idle,
                modes: [walk, run],
                fidgets: fidgets.iter().find(|(base, _)| base.eq_ignore_ascii_case(&name)).map(|(_, all)| lib.all(all)).unwrap_or_default(),
                walk_start: by("Walk_Start"),
                walk_stop: ["Left", "Right"].map(|s| by(&format!("Walk_Stop{s}"))),
                turn_to_walk: ["Left", "Right"].map(|s| ["45", "90", "135", "180"].map(|a| by(&format!("TurnToWalk{s}{a}")))),
                move_turn: [
                    ["Left", "Right"].map(|s| by(&format!("Walk_Turn{s}180"))),
                    ["Left", "Right"].map(|s| run_prefix.as_ref().and_then(|p| lib.find(&format!("{p}Run_Turn{s}180")))),
                ],
                turn: ["Left", "Right"].map(|side| ["45", "90", "135", "180"].map(|a| by(&format!("Turn{side}{a}")).or_else(|| turns.iter().find_map(|s| lib.find(&format!("{s}{side}{a}")))))),
            }
        };
        let relaxed_turns = ["Empty_Turn", "Sword_Patrol_Turn", "Patrol_Turn", "Sword_Turn"];
        let edge_turns = ["Sword_Turn", "Empty_Turn", "Sword_Patrol_Turn", "Patrol_Turn"];
        let empty_walk = first_ways(&["Empty_Walk", "Patrol_Walk", "Sword_Patrol_Walk", "Sword_Walk"]);
        let empty_run = first_ways(&["Empty_Run", "Patrol_Run", "Sword_Run"]);
        let sword_run = first_ways(&["Sword_Run", "Empty_Run", "Patrol_Run"]);
        let single = |c: Option<ClipId>| c.map(|c| [None, None, Some((c, false)), None]);
        let loco = [
            set(c.idle, empty_walk, empty_run, &relaxed_turns),
            set(c.idle, single(lib.find("Empty_Walk_Patrol")).unwrap_or(empty_walk), empty_run, &relaxed_turns),
            set(lib.find("Sword_Patrol_Idle").or(c.idle), first_ways(&["Sword_Patrol_Walk", "Empty_Walk", "Patrol_Walk", "Sword_Walk"]), sword_run, &edge_turns),
            set(c.idle_alert, first_ways(&["Sword_Walk", "Empty_Walk", "Attack_Walk", "Patrol_Trot"]), sword_run, &edge_turns),
            set(c.idle_alert, empty_walk, single(c.flee).unwrap_or(sword_run), &edge_turns),
        ];
        let c = NpcClips { loco, ..c };
        // (a musical Overseer kicks, its hands on its music box: `MusicBox_Attack_KickMedium`,
        // its music box's set alone has it)
        let c = match lib.first(&["MusicBox_Attack_KickMedium"]) {
            Some(kick) => NpcClips {
                attack: vec![kick],
                attack_short: vec![kick],
                attack_short_big: vec![kick],
                attack_medium: vec![kick],
                attack_long: vec![kick],
                attack_long_big: vec![kick],
                attack_step_back: Some(kick),
                kick: Some(kick),
                ..c
            },
            None => c,
        };
        // (a whale butcher's saw: his fighting stance and blows are its own, by its prefix,
        // `m_ItemPrefix` `Saw_`: standing with it, walking, running, revving it up and down,
        // its short and medium cuts, its spray (`_RangeAttack_In`, `_Loop`, `_Out`) and the
        // rats' bite; the swordsmen's that it has no blade for, none: no parries, no dodges, no
        // blade locks, `m_bDisableVersus`)
        let c = match (prefix, lib.find(&format!("{prefix}Idle"))) {
            (p, Some(idle)) if !p.is_empty() && !p.eq_ignore_ascii_case("Sword_") && !p.eq_ignore_ascii_case("Empty_") => {
                let by = |n: &str| lib.find(&format!("{p}{n}"));
                let all = |ns: &[&str]| ns.iter().filter_map(|n| by(n)).collect::<Vec<_>>();
                let walk = by("WalkN");
                let run = by("RunN");
                let stance = LocoSet {
                    idle: Some(idle),
                    modes: [[None, None, walk.map(|w| (w, false)), None], [None, None, run.or(walk).map(|r| (r, false)), None]],
                    ..c.loco[3].clone()
                };
                let short = all(&["AttackShort", "AttackShortB"]);
                let medium = all(&["AttackMedium", "AttackMediumB"]);
                let mut loco = c.loco.clone();
                loco[2] = stance.clone();
                loco[3] = stance;
                NpcClips {
                    idle_alert: Some(idle),
                    walk_alert: walk.or(c.walk_alert),
                    run: run.or(c.run),
                    loco,
                    attack: [short.clone(), medium.clone()].concat(),
                    attack_short: short.clone(),
                    attack_short_big: short,
                    attack_medium: medium,
                    attack_long: Vec::new(),
                    attack_long_big: Vec::new(),
                    attack_step_back: None,
                    attack_turn: Default::default(),
                    jump_attack: by("JumpAttack"),
                    equip: by("Equip").or(c.equip),
                    equip_combat: by("Equip").or(c.equip_combat),
                    unequip: by("UnEquip").or(c.unequip),
                    patrol_to_sword: None,
                    sword_to_patrol: None,
                    parry_win: Vec::new(),
                    parry_lost: Vec::new(),
                    versus: Default::default(),
                    dodge_in: Default::default(),
                    dodge_out: [None; 2],
                    riposte: Default::default(),
                    dodge: Default::default(),
                    dodge_attack: [None; 2],
                    swarm: [c.swarm[0], ["SwarmAttack_In", "SwarmAttack_Loop", "SwarmAttack_Out"].map(|n| by(n))],
                    saw_fire: ["RangeAttack_In", "RangeAttack_Loop", "RangeAttack_Out"].map(|n| by(n)),
                    ..c
                }
            }
            _ => c,
        };
        (c.idle.is_some() && c.walk.is_some()).then_some(c)
    }
}

/// Animation state of an NPC driven by its original clips.
#[derive(Component)]
pub struct NpcAnim {
    clips: std::sync::Arc<NpcClips>,
    last_mode: Mode,
    last_alert: Alert,
    /// on its knees begging (or cowering)
    begging: bool,
    down: bool,
    attacking: bool,
    /// A one-shot (reaction, search, fidget) is playing.
    oneshot: bool,
    hit_seen: f32,
    /// standing time left before its next ambient idle
    idle_time: f32,
    /// time to its next blink
    blink_in: f32,
    /// Lying pose to blend into once the current (falling) clip ends.
    settle: Option<ClipId>,
    /// Sword in hand (else in its holster).
    drawn: bool,
    sword: Vec<Entity>,
    hand: Option<(Entity, Transform)>,
    holster: Option<(Entity, Transform)>,
    /// the grab phase being played
    grab_seen: Option<GrabPhase>,
    /// the gesture last played (its count)
    act_seen: u32,
    /// making off from an explosive (as last seen)
    escaping: bool,
    /// its locomotion's smoothed velocity in its own frame (x to its right, y ahead)
    loco_v: Vec2,
    /// what its clips' notifies mark (the blows' zones, when an attack may be broken off, when
    /// the sword falls from the hand)
    marks: std::sync::Arc<std::collections::HashMap<ClipId, dhcook::format::ClipMarks>>,
    /// its sword's blade, its ends in the sword's frame
    blade: Option<[Vec3; 2]>,
    /// it has a ragdoll (its pawn's physics asset)
    pub(crate) ragdoll: bool,
    /// its pistol's parts, the left hand and the hip they hang from, and where its drawing is
    gun: Vec<Entity>,
    gun_hand: Option<(Entity, Transform)>,
    gun_holster: Option<(Entity, Transform)>,
    /// the facing it is shown turned to (radians), and its turn where it stands: from, by how
    /// much, how far through, how long
    shown_yaw: f32,
    turn: Option<(f32, f32, f32, f32)>,
    /// how long since a swarm was last on it, and how far into fighting it off (0 not, 1 its
    /// start, 2 its loop)
    swarm_t: f32,
    swarm_phase: u8,
    /// its reeling after Corvo came out of it: the clip still to come
    unpossess_next: Option<ClipId>,
    /// how far into choking in chokedust (0 not, 1 its start, 2 its loop)
    dust_phase: u8,
    /// how far into casting its Attract spell (0 not, 1 its hand raised, 2 held)
    attract_phase: u8,
    /// putting its sword away: how long until it is in its holster
    holster_in: Option<f32>,
    /// knocked off its feet: how long until it goes limp, and its push
    limp_in: Option<(f32, Vec3)>,
    /// the clip to follow the one playing: up again from where it lay, its stun's recovery;
    /// Corvo lost, its look about
    next_clip: Option<ClipId>,
    /// Corvo lost in the fight, its lowering and look about yet to play (once it is free to)
    lost_pending: bool,
    /// a weak hit's flinch under way: its clip and how far through
    flinch: Option<(ClipId, f32)>,
    /// each body clip's face clip, as found
    faces: std::collections::HashMap<ClipId, Option<ClipId>>,
    /// how far through its relaxed walk's loop (0..1; NaN not walking it): its feet landing
    walk_phase: f32,
    /// its bow drawn (a tallboy's)
    bow_drawn: bool,
    /// dodging to a side (its clip) with its slash to come
    counter: Option<(usize, ClipId)>,
    gun_phase: GunPhase,
    gun_t: f32,
    gun_out: bool,
    shots_seen: u32,
    /// a thug's whiskey bottle's parts, the left hand and the hip they hang from, and how far
    /// through its fire breath's clips it is (0-2), or its throw (3)
    bottle: Vec<Entity>,
    bottle_hand: Option<(Entity, Transform)>,
    bottle_holster: Option<(Entity, Transform)>,
    bottle_stage: Option<u8>,
    /// a saw's spray under way: its clip (raised, held, lowered) and when it is lowered
    saw_stage: Option<(u8, f32)>,
}

/// The pause before a blade strikes under a hideout again, after its strike's clip (s:
/// `DisTweaks_NPCAttackUnder` sets no cooldown of its own): base and spread.
const UNDER_SWORD: (f32, f32) = (1.0, 1.0);

/// The side a character was struck from (0 front, 1 left, 2 right, 3 back), by where the last
/// blow came from; none without one.
fn side_struck(npc: &Npc, at: Vec3) -> Option<usize> {
    let to = (npc.hit_from? - at).with_y(0.0).normalize_or_zero();
    let f = npc.forward();
    let right = Vec3::new(-f.z, 0.0, f.x);
    let (ahead, side) = (to.dot(f), to.dot(right));
    Some(if ahead.abs() >= side.abs() {
        if ahead >= 0.0 { 0 } else { 3 }
    } else if side > 0.0 {
        2
    } else {
        1
    })
}

/// Time between blinks (s: `ArkComponentLookatConfig`'s `MinDurationBetweenBlink`,
/// `MaxDurationBetweenBlink`).
const BLINK_EVERY: [f32; 2] = [0.5, 6.0];

/// Standing time between ambient idles (s: the patrol and guard behaviours'
/// `m_fMinAmbientAnimFrequency`, `m_fMaxAmbientAnimFrequency`).
const AMBIENT_IDLE: [f32; 2] = [30.0, 60.0];

/// How long a cough holds a plague carrier (s: `Empty_Idle_Cough` runs 2.66 s).
const COUGH_HOLD: f32 = 2.6;

/// How long a taunt holds a character (s: its gestures, `Sword_TauntA/B/C`, run 2.4-3.0 s).
const TAUNT_HOLD: f32 = 2.7;

/// A panicking civilian is caught with Corvo this near (m: `TweaksBehavior_Panic_Civilian`'s
/// `m_fCowerReactionDistance`, 350 uu), and kneels to beg.
const BEG_REACH: f32 = 3.5;

/// How long getting up from begging holds a civilian (s: `Empty_Beg_Kneeling_Out`).
const BEG_RISE: f32 = 1.5;

/// How long a character stands after its pistol shot (`Sword_Gun_Fire` 1 s, then `Sword_Gun_Undraw`
/// 1 s).
const GUN_HOLD: f32 = 2.0;
/// A tallboy's draw before it looses, s: its draw's clip (`Empty_Bow_Charged_In`, 1.27 s) and a
/// breath held (our reading).
const BOW_DRAW: f32 = 1.5;

/// Where a character's pistol is: on its hip, coming out, aimed, firing, going back.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum GunPhase {
    #[default]
    Away,
    Drawing,
    Aiming,
    Firing,
    Reloading,
    Holstering,
}

/// A weapon's parts hung from a joint (its socket's offset).
fn hang(commands: &mut Commands, parts: &[Entity], at: Option<(Entity, Transform)>) {
    if let Some((j, t)) = at {
        for &p in parts {
            commands.entity(p).try_insert((t, ChildOf(j)));
        }
    }
}

/// A character's sword fallen from its hand as it went down (its clip's `DisNotify_DropItem`):
/// loose in the world.
#[derive(Component)]
pub struct DroppedSword;

/// A downed character lets its sword fall at its clip's drop (`DisNotify_DropItem`), else
/// soon after it falls: the sword's parts on a loose body (a box along its blade).
fn drop_swords(mut commands: Commands, mut npcs: Query<(&Npc, &mut NpcAnim, &Animator)>, parts: Query<&GlobalTransform>) {
    for (npc, mut st, anim) in &mut npcs {
        if !npc.is_down() || st.sword.is_empty() || !st.drawn {
            continue;
        }
        let due = match anim.current().and_then(|p| st.marks.get(&p.clip).and_then(|m| m.drops.first().copied()).map(|t| (p.t, t))) {
            Some((at, t)) => at >= t,
            None => npc.down_t >= 0.3,
        };
        if !due {
            continue;
        }
        let Some(g) = st.sword.first().and_then(|&p| parts.get(p).ok()) else { continue };
        let (_, rot, pos) = g.to_scale_rotation_translation();
        if std::env::var("DH_MELEE_LOG").is_ok() {
            info!("melee: {} lets its sword fall at {pos:.1}", npc.name);
        }
        let [a, b] = st.blade.unwrap_or([Vec3::new(-0.15, 0.0, 0.0), Vec3::new(0.85, 0.0, 0.0)]);
        let along = b - a;
        let shape = Collider::compound(vec![((a + b) * 0.5, Quat::from_rotation_arc(Vec3::X, along.normalize_or(Vec3::X)), Collider::cuboid(along.length() * 0.5, 0.012, 0.035))]);
        let root = commands
            .spawn((
                DroppedSword,
                Transform::from_translation(pos).with_rotation(rot),
                Visibility::default(),
                RigidBody::Dynamic,
                shape,
                CollisionGroups::new(GROUP_PROP, GROUP_WORLD.union(GROUP_PROP)),
                ColliderMassProperties::Mass(1.2),
                Damping { linear_damping: 0.3, angular_damping: 0.8 },
                DespawnOnExit(GameState::InGame),
            ))
            .id();
        for &p in &st.sword {
            commands.entity(p).try_insert((Transform::IDENTITY, ChildOf(root)));
        }
        st.sword.clear();
    }
}

impl NpcAnim {
    /// The weapon meshes in the character's hand or holster.
    pub fn swords(&self) -> &[Entity] {
        &self.sword
    }

    /// The lines a clip cues (`DishonoredNotify_FireDialogHook`): when, which dialog hook.
    pub fn clip_hooks(&self, clip: ClipId) -> &[(f32, String)] {
        self.marks.get(&clip).map(|m| m.hooks.as_slice()).unwrap_or(&[])
    }

    /// The effects a clip starts (`AnimNotify_PlayParticleEffect`).
    pub fn clip_fx(&self, clip: ClipId) -> &[dhcook::format::ClipFx] {
        self.marks.get(&clip).map(|m| m.fx.as_slice()).unwrap_or(&[])
    }

    /// When a clip starts its character's plague cloud (`DishonoredNotify_StartPlague`).
    pub fn plague_marks(&self, clip: ClipId) -> &[f32] {
        self.marks.get(&clip).map(|m| m.plague.as_slice()).unwrap_or(&[])
    }

    /// A clip's `DisNotify_FootPlacement`s (as it starts, in its time, as it ends): when, and
    /// whether the feet are placed from then on.
    pub fn feet_marks(&self, clip: ClipId) -> &[(f32, bool)] {
        self.marks.get(&clip).map(|m| m.feet.as_slice()).unwrap_or(&[])
    }
}

impl NpcAnim {
    /// The character's walk (or run) cycle, for scripted scenes.
    pub fn gait_clip(&self, run: bool) -> Option<ClipId> {
        if run {
            self.clips.run.or(self.clips.walk)
        } else {
            self.clips.walk
        }
    }
}

/// Sheathed sword relative to `Holster_jnt` for meshes without a holster socket.
const HOLSTERED: Transform = Transform {
    translation: Vec3::ZERO,
    rotation: Quat::from_xyzw(0.0, 1.0, 0.0, 0.0),
    scale: Vec3::ONE,
};

/// `DH_ANIM=<sequence>`: every NPC loops this clip (for checking animations).
fn forced_clip() -> Option<&'static str> {
    static F: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    F.get_or_init(|| std::env::var("DH_ANIM").ok()).as_deref()
}

fn pick(v: &[ClipId]) -> Option<ClipId> {
    if v.is_empty() {
        None
    } else {
        Some(v[rand::random::<u32>() as usize % v.len()])
    }
}

/// `DH_NPC_CLIP_LOG`: the clips the characters near Corvo switch to, with their state.
fn log_npc_clips(
    player: Query<&Transform, With<Player>>,
    npcs: Query<(Entity, &Npc, &Animator, &Transform, Has<ScriptedAnim>)>,
    mut last: Local<std::collections::HashMap<Entity, (Option<ClipId>, Option<ClipId>, Option<ClipId>)>>,
) {
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for (e, npc, anim, t, scripted) in &npcs {
        if t.translation.distance(ppos) > 8.0 {
            continue;
        }
        let cur = anim.current().map(|p| p.clip);
        let upper = anim.upper_clip();
        let face = anim.face_clip();
        if last.get(&e) != Some(&(cur, upper, face)) {
            last.insert(e, (cur, upper, face));
            info!(
                "npc clip: {} -> {} upper {} face {} (mode {:?} alert {:?} hit {:.2} stagger {:.2} attack {:?} scripted {scripted})",
                npc.name,
                cur.map(|c| anim.lib.clip(c).name.as_str()).unwrap_or("-"),
                upper.map(|c| anim.lib.clip(c).name.as_str()).unwrap_or("-"),
                face.map(|c| anim.lib.clip(c).name.as_str()).unwrap_or("-"),
                npc.mode,
                npc.alert,
                npc.hit_react,
                npc.stagger,
                npc.attack_t
            );
        }
    }
}

/// A character's look at what it watches (`HeadTrack`), as its look-at grids pose it
/// (`LOOK_MODES`): the mode by the stance of the clip it plays and its gait, the torso's and the
/// head's additive poses weighted by the look. Without the mode's grids, `head_track` turns it.
fn look_grids(anim: &mut Animator, clips: &NpcClips, lib: &CharAnims, speed: f32, track: Option<&mut HeadTrack>) {
    let all: Vec<ClipId> = clips.look.iter().flatten().flatten().flatten().flatten().copied().collect();
    anim.additive.retain(|(c, _, _)| !all.contains(c));
    let Some(track) = track else { return };
    let base = anim.current().map(|p| lib.clip(p.clip).name.as_str()).unwrap_or("");
    let stance = if base.starts_with("Sword_Patrol") { 2 } else if base.starts_with("Sword_") { 1 } else { 0 };
    let gait = if speed < 0.3 { 0 } else if speed < 2.6 { 1 } else { 2 };
    let grid = &clips.look[stance][gait];
    track.grids = grid.iter().all(|g| g[4].is_some());
    if !track.grids || track.weight <= 0.0 {
        return;
    }
    let w = track.weight * track.weight * (3.0 - 2.0 * track.weight);
    let (torso, head) = LOOK_MODES[stance][gait];
    let ws = look_weights(track.yaw, track.pitch, torso, head, track.torso);
    for (part, ws) in grid.iter().zip(ws) {
        for (i, c) in part.iter().enumerate() {
            if let Some(c) = c.filter(|_| ws[i] * w > 1e-3) {
                anim.additive.push((c, 0.0, ws[i] * w));
            }
        }
    }
}

/// A body clip's face clip (its `DisAnimation` steps' `m_FaceAnimName`, by the names they
/// follow: the clip's own `_Face`; its variants' (`Sword_Attack_ShortSmallB` ->
/// `Sword_Attack_ShortSmall_Face`); a big blow's `Sword_Attack_Big_Face`; a hit's small hit's
/// (`Empty_SmallHit_Face`, else `_Hit_Face`); a surprise's `_Surprised_Face`).
fn face_of(lib: &crate::anim::CharAnims, body: ClipId) -> Option<ClipId> {
    let name = lib.clip(body).name.clone();
    if name.ends_with("_Face") {
        return None;
    }
    let face = |n: &str| lib.find(&format!("{n}_Face"));
    if let Some(f) = face(&name) {
        return Some(f);
    }
    // (a variant: its trailing letter)
    let bytes = name.as_bytes();
    if bytes.len() > 2 && bytes[bytes.len() - 1].is_ascii_uppercase() && bytes[bytes.len() - 2].is_ascii_lowercase() {
        if let Some(f) = face(&name[..name.len() - 1]) {
            return Some(f);
        }
    }
    let prefix = name.split('_').next().unwrap_or("");
    let others = ["Sword", "Empty"];
    let any = |n: &str| std::iter::once(prefix).chain(others).find_map(|p| face(&format!("{p}_{n}")));
    if name.contains("Attack") && name.contains("Big") {
        return any("Attack_Big");
    }
    if name.contains("Surprised") {
        return any("Surprised");
    }
    if name.contains("Hit") && !name.contains("Attack") {
        return any("SmallHit").or_else(|| any("Hit"));
    }
    None
}

/// Each character's face plays its body's clip's face clip (`face_of`), held to the body's time:
/// its expression (an upper-body action's over the legs' clip; a weak hit's flinch over both).
/// Speaking, its line's face animation has it (`facefx`); far off, none.
fn npc_faces(player: Query<&Transform, With<Player>>, mut npcs: Query<(&mut NpcAnim, &mut Animator, &Transform, Has<crate::facefx::FaceAnim>), With<Npc>>) {
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for (mut st, mut anim, t, speaking) in &mut npcs {
        if speaking || t.translation.distance_squared(ppos) > 30.0 * 30.0 {
            anim.set_face(None, 0.2);
            continue;
        }
        let body = st.flinch.map(|(c, ft)| (c, ft, false)).or_else(|| anim.upper_playing().or(anim.current()).map(|p| (p.clip, p.t, p.looping)));
        let face = body.and_then(|(c, bt, looping)| {
            let lib = anim.lib.clone();
            let f = *st.faces.entry(c).or_insert_with(|| face_of(&lib, c));
            f.map(|f| (f, if looping { bt.rem_euclid(lib.duration(f)) } else { bt }))
        });
        anim.set_face(face, 0.2);
    }
}

/// Choose each NPC's clip from its state (locomotion, attacks, reactions, deaths).
fn npc_select_anim(
    time: Res<Time>,
    tc: Res<TimeControl>,
    mut commands: Commands,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<
        (
            Entity,
            &mut Npc,
            &mut NpcAnim,
            &mut Animator,
            &Transform,
            &Children,
            Option<&DropKilled>,
            Option<&AssassinClip>,
            Option<&mut FinisherClip>,
            Option<&crate::carry::Falling>,
            (Has<crate::ragdoll::Ragdoll>, Has<crate::ragdoll::GoLimp>, Has<crate::musicbox::TunePlaying>, Option<&Electrocuted>, Has<Immolated>, Option<&mut HeadTrack>),
        ),
        Without<ScriptedAnim>,
    >,
    mut visuals: Query<&mut Transform, (With<NpcVisual>, Without<Npc>, Without<Player>)>,
    mut swarmed: MessageReader<crate::swarm::SwarmOn>,
) {
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    let swarmed: std::collections::HashSet<Entity> = swarmed.read().map(|s| s.0).collect();
    for (e, mut npc, mut st, mut anim, t, children, dropped, assassin, finisher, falling, (limp, going_limp, tune, electrocuted, immolated, mut track)) in &mut npcs {
        let scale = tc.npc_scale(npc.out_of_bend).max(0.0);
        let dt = time.delta_secs() * scale;
        anim.time_scale = scale;
        // turning where it stands, it is shown turned as far as its turn has gone (in plain idle
        // only: anything else shows its facing as it is)
        let plain = !npc.is_down() && !npc.casting && !npc.floored && npc.rise.is_none() && npc.attack_t.is_none() && npc.aim.is_none() && npc.gun_hold <= 0.0 && npc.grab.is_none() && !st.oneshot && (npc.anim_speed < 0.3 || npc.pace.is_some());
        if !plain || !st.shown_yaw.is_finite() {
            st.turn = None;
            st.shown_yaw = npc.yaw;
        }
        npc.turning = st.turn.is_some();
        npc.sync = None;
        let lag = (st.shown_yaw - npc.yaw + PI).rem_euclid(TAU) - PI;
        // the clips carry the whole body motion (falls included)
        for c in children.iter() {
            if let Ok(mut vt) = visuals.get_mut(c) {
                let face = Quat::from_rotation_y(lag + FRAC_PI_2);
                if vt.rotation != face || vt.translation != Vec3::ZERO {
                    vt.rotation = face;
                    vt.translation = Vec3::ZERO;
                }
            }
        }
        let far = t.translation.distance_squared(ppos) > 80.0 * 80.0;
        let st = &mut *st;
        // its locomotion's velocity, in its own frame, smoothed toward its body's at up to 20 m/s
        // a second, less for a small difference (0x0090BC00: `m_fVelocityMaxBlendSpeed` 2000 uu/s
        // over `m_fVelocityMaxDif` 100, at least `m_fVelocityMinBlendSpeedRatio` 0.1 of it, as
        // roshan's r614/blink research decoded it)
        {
            let v = npc.velocity.with_y(0.0);
            let right = Quat::from_rotation_y(npc.yaw) * Vec3::X;
            let want = Vec2::new(v.dot(right), v.dot(npc.forward()));
            let d = (want - st.loco_v).length();
            if d > 1e-5 {
                st.loco_v += (want - st.loco_v) * (20.0 * (d / 1.0).clamp(0.1, 1.0) * dt / d).min(1.0);
            }
        }
        let clips = st.clips.clone();
        let lib = anim.lib.clone();
        // (Corvo lost in the fight: kept until it is free to show it)
        if npc.mode != Mode::Search {
            st.lost_pending = false;
        } else if st.last_mode == Mode::Combat {
            st.lost_pending = true;
        }
        // its look at what it watches (`head_track`'s), laid by its look-at grids for its stance
        // and gait: the clip it plays names its stance (a search's patrol stance, sword drawn,
        // else empty-handed)
        look_grids(&mut anim, &clips, &lib, npc.anim_speed, track.as_deref_mut());
        // (its own death, and no going limp: a wall of light's dead, struck standing; one burning)
        let own_death = electrocuted.filter(|el| el.standing).and(clips.electrified).or(clips.immolate.filter(|_| immolated));
        // a weak hit's flinch, laid over whatever it does (its clip played through once)
        npc.weak_hits = clips.weak_hit.iter().all(Option::is_some);
        npc.dodges = clips.dodge_in.iter().all(|v| !v.is_empty());
        if let Some(side) = npc.weak_hit.take() {
            st.flinch = clips.weak_hit[side.min(3)].map(|c| (c, 0.0));
            if let (Some((c, _)), true) = (st.flinch, std::env::var("DH_NPC_CLIP_LOG").is_ok()) {
                info!("npc flinch: {} {} over {}", npc.name, lib.clip(c).name, anim.current().map(|p| lib.clip(p.clip).name.as_str()).unwrap_or("-"));
            }
        }
        anim.additive.retain(|(c, _, _)| !clips.weak_hit.contains(&Some(*c)));
        if let Some((c, ft)) = st.flinch {
            if ft < lib.duration(c) && !npc.is_down() {
                anim.additive.push((c, ft, 1.0));
                st.flinch = Some((c, ft + dt));
            } else {
                st.flinch = None;
            }
        }

        // the eyelids: a blink every 0.5-6 s at 1-2 times its pace, on the eyelid bones alone
        // (`ArkComponentLookatConfig`'s `MinDurationBetweenBlink`..`Max`, `MinBlinkSpeed`..`Max`,
        // as roshan's r614/blink research read them); none for the down
        if let Some(b) = clips.blink {
            st.blink_in -= dt;
            if anim.overlay_is(Some(b)) && anim.overlay_finished() {
                anim.stop_overlay(0.02);
            }
            if st.blink_in <= 0.0 {
                st.blink_in = BLINK_EVERY[0] + rand::random::<f32>() * (BLINK_EVERY[1] - BLINK_EVERY[0]);
                if !npc.is_down() && !far {
                    anim.restart_overlay(b, false, 1.0 + rand::random::<f32>(), 0.02);
                }
            }
        }

        // (a fall, a blow, a gesture, a swing or anything setting it on edge takes the upper body
        // back from an ambient idle)
        let [box_idle, box_in, box_loop, box_out] = st.clips.music_box;
        let musical = box_idle.is_some() && (npc.arms.tunes.is_some() || npc.pawn.contains("MusicOverseer"));
        // (its ambient idle given up to anything setting it on edge, any upper-body action to a
        // blow, a gesture, a swing or being caught)
        let fidget = anim.upper_clip().is_some_and(|c| clips.loco.iter().any(|s| s.fidgets.contains(&c)));
        let interrupted = npc.hit_react > st.hit_seen + 1e-3 || npc.act.1 != st.act_seen || npc.attack_t.is_some() || npc.caught.is_some();
        if anim.upper_on() && (npc.is_down() || !musical && (interrupted || fidget && npc.alert != Alert::Unaware)) {
            anim.stop_upper(0.15);
        }
        // a live explosive discovered: startled (`m_fSurpriseChance` 1), on its upper body as its
        // legs make off
        if npc.escape.is_some() && !st.escaping && !npc.is_down() && !musical {
            let [empty, sword] = clips.startled;
            if let Some(c) = if st.drawn { sword.or(empty) } else { empty.or(sword) } {
                anim.restart_upper(c, false, 1.2, 0.1);
            }
        }
        st.escaping = npc.escape.is_some();
        // a musical Overseer's arms hold its music box, walking or standing (its pawn's walking
        // upper-body desire, `MusicBoxIdle`), and crank it while it plays (`PlayMusic`: in, the
        // loop, out), as roshan's r614/blink research read `DisWepMusicBox`
        if musical && !npc.is_down() && !limp {
            let on = |c: Option<ClipId>| c.is_some() && anim.upper_clip() == c;
            if tune {
                if !(on(box_in) || on(box_loop)) {
                    if let Some(c) = box_in.or(box_loop) {
                        anim.restart_upper(c, box_in.is_none(), 1.0, 0.2);
                    }
                } else if on(box_in) && anim.upper_finished() {
                    if let Some(c) = box_loop {
                        anim.restart_upper(c, true, 1.0, 0.15);
                    }
                }
            } else if on(box_in) || on(box_loop) {
                match box_out {
                    Some(c) => anim.restart_upper(c, false, 1.0, 0.15),
                    None => anim.stop_upper(0.2),
                }
            } else if !on(box_out) || anim.upper_finished() {
                if let Some(c) = box_idle.filter(|_| !on(box_idle)) {
                    anim.restart_upper(c, true, 1.0, 0.3);
                }
            }
        }

        // sword: drawn when alerted, holstered otherwise
        let mut drew = false;
        // (suspecting, a guard walks its rounds with its sword out, its patrol stance:
        // `EquippedPatrol` with `m_SuspectingEquippedPrimaryItemClass`, the sword)
        let drawn = npc.alert != Alert::Unaware
            || npc.suspecting && npc.hostile() && npc.has_sword && npc.mode != Mode::Flee
            || matches!(npc.mode, Mode::Combat | Mode::Search | Mode::Investigate)
            || npc.is_down()
            || npc.mode == Mode::Choked;
        if drawn != st.drawn && !st.sword.is_empty() {
            // calming down, it puts the sword away with its clip, into the holster at the clip's
            // unequip cue (`Snd_Sword_Enm_Unequip_cue_ak`), on its upper body as it walks
            let away = clips.unequip.filter(|_| !drawn && !npc.is_down() && st.hand.is_some());
            if let Some(c) = away {
                let cue = lib.clip(c).sounds.first().map_or(lib.duration(c) * 0.4, |s| s.0);
                st.holster_in = Some(cue);
                if npc.anim_speed >= 0.6 {
                    anim.restart_upper(c, false, 1.0, 0.15);
                } else {
                    anim.restart(c, false, 1.0, 0.15);
                    st.oneshot = true;
                }
            } else {
                st.holster_in = None;
                if let Some((j, at)) = if drawn { st.hand } else { st.holster.or(st.hand) } {
                    for &s in &st.sword {
                        commands.entity(s).try_insert((at, ChildOf(j)));
                    }
                }
                // drawn to search (not to fight: that is its fighting stance's draw, below), into
                // its patrol stance (`DisNPCAnim_EquipToSearch`)
                if let Some(c) = clips.equip.filter(|_| drawn && npc.alert != Alert::Combat && !npc.is_down() && npc.mode != Mode::Choked && !musical) {
                    drew = true;
                    if npc.anim_speed >= 0.6 {
                        anim.restart_upper(c, false, 1.2, 0.15);
                    } else {
                        anim.restart(c, false, 1.2, 0.15);
                        st.oneshot = true;
                    }
                }
            }
            st.drawn = drawn;
        }
        if let Some(t) = st.holster_in.as_mut() {
            *t -= dt;
            if *t <= 0.0 {
                st.holster_in = None;
                if let Some((j, at)) = st.holster.or(st.hand) {
                    for &s in &st.sword {
                        commands.entity(s).try_insert((at, ChildOf(j)));
                    }
                }
            }
        }
        if let Some(name) = forced_clip() {
            if let Some(c) = lib.find(name) {
                anim.play(c, true, 1.0, 0.2);
                continue;
            }
        }
        // finishing a foe: its part played through, then back to the rest
        if let Some(mut f) = finisher.filter(|_| !npc.is_down()) {
            match lib.find(&f.0) {
                Some(c) if !f.1 => {
                    anim.restart(c, false, 1.0, 0.15);
                    f.1 = true;
                    continue;
                }
                Some(_) if !anim.finished() => continue,
                _ => {
                    commands.entity(e).try_remove::<FinisherClip>();
                }
            }
        }

        // blown off its feet (Wind Blast), where its set can get it up again: limp (`ragdoll`),
        // out of everyone's way till it is
        if let Some((v, throw)) = npc.knock.take() {
            if clips.get_up.iter().all(Option::is_some) && !npc.is_down() && !limp && !going_limp {
                // (its push's moment first, by the side it came from, `Generic_PushToRagdoll_*`;
                // then limp)
                let f = npc.forward();
                let from = -v.with_y(0.0);
                let (ahead, side) = (from.dot(f), from.dot(Vec3::new(-f.z, 0.0, f.x)));
                let s = if ahead.abs() >= side.abs() { if ahead >= 0.0 { 0 } else { 3 } } else if side > 0.0 { 2 } else { 1 };
                let clip = match throw {
                    Throw::Knock => clips.push_limp[0][s],
                    Throw::Blast => clips.push_limp[1][s],
                    Throw::Limp => None,
                };
                match clip {
                    Some(c) => {
                        // (flying on its push meanwhile: roshan's r614/blink research saw the
                        // original's throw fly the pawn some 27 ticks before the ragdoll)
                        anim.restart(c, false, 1.0, 0.05);
                        let at = st.marks.get(&c).and_then(|m| m.ragdoll).unwrap_or(lib.duration(c));
                        st.limp_in = Some((at, v));
                        npc.flying = true;
                        npc.velocity = v;
                    }
                    None => {
                        commands.entity(e).try_insert((crate::ragdoll::GoLimp::now(v, None), ColliderDisabled));
                    }
                }
                npc.floored = true;
                npc.attack_t = None;
                npc.swing = None;
                npc.aim = None;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
        }
        if npc.floored && !npc.is_down() {
            if let Some((left, v)) = st.limp_in {
                if left > dt {
                    st.limp_in = Some((left - dt, v));
                } else {
                    st.limp_in = None;
                    npc.flying = false;
                    // (limp, flying on as it was)
                    let v = if npc.velocity.length() > 0.5 { npc.velocity } else { v };
                    commands.entity(e).try_insert((crate::ragdoll::GoLimp::now(v, None), ColliderDisabled));
                }
            }
            anim.frozen = limp && far;
            st.last_mode = npc.mode;
            st.last_alert = npc.alert;
            continue;
        }
        // lying still, up again (`Generic_RagdollToBack` / `_ToFront`: from where its bodies lay,
        // `ragdoll`), standing for it
        if let Some(back) = npc.rise.take() {
            anim.frozen = false;
            commands.entity(e).try_remove::<ColliderDisabled>();
            if let Some(c) = clips.get_up[(!back) as usize].filter(|_| !npc.is_down()) {
                // (then the stun's recovery, `Sword_RecoveryA` / `B` or `Empty_`: the original's
                // stun after a throw, as roshan's r614/blink research read it)
                let hands = (!st.drawn) as usize;
                st.next_clip = pick(&clips.recovery[hands]).or_else(|| pick(&clips.recovery[1 - hands]));
                anim.restart(c, false, 1.0, 0.0);
                npc.stagger = npc.stagger.max(lib.duration(c) + st.next_clip.map_or(0.0, |r| lib.duration(r)));
                st.oneshot = true;
                st.hit_seen = npc.hit_react;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
        }
        if let Some(c) = st.next_clip.filter(|_| anim.finished()) {
            st.next_clip = None;
            if !npc.is_down() {
                anim.restart(c, false, 1.0, 0.15);
                st.oneshot = true;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
        }
        if npc.is_down() {
            // limp (or about to be): its bodies pose its bones, the clip under them the rest
            if limp || going_limp {
                st.down = true;
                st.settle = None;
                anim.frozen = limp && far;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
            if falling.is_some() {
                if !st.down {
                    crate::carry::lay_down(&mut anim);
                    if std::env::var_os("DH_CARRY_LOG").is_some() {
                        info!("carry: restored flight pose for NPC {}", npc.spawner);
                    }
                }
                st.down = true;
                st.settle = None;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                anim.frozen = false;
                continue;
            }
            if !st.down {
                st.down = true;
                st.oneshot = false;
                let (clip, settle) = if let Some(c) = own_death {
                    (Some(c), None)
                } else if npc.corpse {
                    (pick(&clips.corpse).or(clips.lie_back), None)
                } else if let Some(d) = dropped.filter(|_| npc.mode == Mode::Dead) {
                    (clips.drop_killed[d.0 as usize % 4].or(pick(&clips.assassinated)), clips.lie_front)
                } else if let Some(a) = assassin.filter(|_| npc.mode == Mode::Dead) {
                    (lib.find(&a.0).or(pick(&clips.assassinated)), clips.lie_front)
                } else if st.last_mode == Mode::Choked {
                    (clips.choke_out, clips.lie_back)
                } else if st.last_alert == Alert::Unaware && npc.mode == Mode::Dead {
                    (pick(&clips.assassinated), clips.lie_front)
                } else if rand::random::<f32>() < 0.5 && clips.death_fast.is_some() {
                    (clips.death_fast, clips.lie_back)
                } else {
                    // (falling away from the blow, by the side it struck)
                    (side_struck(&npc, t.translation).and_then(|s| clips.death_dir[s]).or(pick(&clips.death)), None)
                };
                let (clip, settle) = match clip {
                    Some(c) => (Some(c), settle),
                    None => (settle.or(pick(&clips.death)), None),
                };
                st.settle = settle;
                if let Some(c) = clip {
                    anim.restart(c, false, 1.0, if npc.corpse { 0.0 } else { 0.15 });
                    if npc.corpse {
                        anim.seek(lib.duration(c));
                    }
                }
            }
            // its clip goes limp where it says (`DishonoredNotify_Ragdoll`), else at its end
            if st.ragdoll && !far && own_death.is_none() {
                let due = anim.finished() || anim.current().and_then(|p| st.marks.get(&p.clip).and_then(|m| m.ragdoll).map(|r| p.t >= r)).unwrap_or(true);
                if due {
                    commands.entity(e).try_insert(crate::ragdoll::GoLimp::soon());
                    anim.frozen = false;
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
            }
            if anim.finished() {
                if let Some(c) = st.settle.take() {
                    anim.restart(c, false, 1.0, 0.7);
                    anim.seek(lib.duration(c));
                }
            }
            anim.frozen = far && anim.finished() && st.settle.is_none();
            st.last_mode = npc.mode;
            st.last_alert = npc.alert;
            continue;
        }
        st.down = false;
        anim.frozen = far;

        if npc.mode == Mode::Choked {
            if st.last_mode != Mode::Choked {
                if let Some(c) = clips.choke_in.or(clips.choke_loop) {
                    anim.restart(c, false, 1.0, 0.15);
                }
            } else if anim.finished() {
                if let Some(c) = clips.choke_loop {
                    anim.play(c, true, 1.0, 0.1);
                }
            }
            st.last_mode = npc.mode;
            st.last_alert = npc.alert;
            continue;
        }

        // a weeper's grab: lunge, hold, release
        if let Some((phase, _)) = npc.grab {
            if st.grab_seen != Some(phase) {
                st.grab_seen = Some(phase);
                let c = match (phase, npc.grab_jump) {
                    (GrabPhase::Try, true) => clips.jump[0].or(clips.grab_try),
                    (GrabPhase::Release | GrabPhase::Miss, true) => clips.jump[1],
                    (GrabPhase::Kill, true) => clips.jump[2].or(clips.grab_kill[0]),
                    (GrabPhase::Try, _) => clips.grab_try,
                    (GrabPhase::Hold, _) => clips.grab_in.or(clips.grab_loop),
                    (GrabPhase::Release, _) => clips.grab_out,
                    (GrabPhase::Miss, _) => clips.grab_miss.or(clips.grab_out),
                    (GrabPhase::Kill, _) => clips.grab_kill[0].or(clips.grab_out),
                };
                let kill_in = c.filter(|&c| Some(c) == clips.grab_kill[0] || Some(c) == clips.jump[2]);
                if let Some(c) = c {
                    anim.restart(c, false, 1.0, 0.1);
                }
                // (a wolfhound's leap and letting go last their clips, carried by them)
                (npc.grab_len, npc.grab_drift) = match c.filter(|_| is_hound(&npc) && phase != GrabPhase::Hold) {
                    Some(c) => (lib.duration(c), lib.clip(c).root_motion[0] / lib.duration(c)),
                    None => (0.0, 0.0),
                };
                // (its kill through to its end)
                if let (GrabPhase::Kill, Some(k)) = (phase, clips.grab_kill[1].filter(|_| kill_in.is_some())) {
                    npc.grab_len += lib.duration(k);
                }
            } else if phase == GrabPhase::Hold && anim.finished() {
                if let Some(c) = clips.grab_loop {
                    anim.play(c, true, 1.0, 0.1);
                }
            } else if phase == GrabPhase::Kill && anim.finished() && anim.current().is_some_and(|p| Some(p.clip) == clips.grab_kill[0] || Some(p.clip) == clips.jump[2]) {
                if let Some(c) = clips.grab_kill[1] {
                    anim.restart(c, false, 1.0, 0.1);
                }
            }
            st.last_mode = npc.mode;
            st.last_alert = npc.alert;
            continue;
        }
        st.grab_seen = None;
        // an assassin's dodge to the side (`Sword_SideStepLeft_DodgeIn`): its slash out of it
        // (`_DodgeAttack_Small`) with Corvo still in its reach (its set has no way out of the dodge
        // without it, `DodgeLeftOut`, so our reading is every dodge ends in it)
        if let Some((side, dodged)) = st.counter {
            if npc.attack_t.is_some() {
                // (a blow straight out of it: the swing below takes the slash's clip)
            } else if anim.current().is_none_or(|p| p.clip != dodged) || npc.is_down() || npc.stagger > 0.0 {
                st.counter = None;
            } else if anim.finished() {
                st.counter = None;
                let near = (ppos - t.translation).with_y(0.0).length() < 3.0 && (ppos.y - t.translation.y).abs() < 1.5;
                let riposte = clips.dodge_attack[side].or_else(|| pick(&clips.riposte[side]));
                if let Some(c) = riposte.filter(|_| near && npc.mode == Mode::Combat) {
                    let hit = st.marks.get(&c).and_then(|m| m.zone).map(|(z, d)| (z + d - 0.12).max(z)).unwrap_or(0.3);
                    npc.attack_t = Some(0.0);
                    npc.swing = Some(Swing { kind: MoveKind::Short, big: false, len: lib.duration(c).max(hit + 0.1), hit, reach: 2.6, lunge: 0.0, no_versus: false, more: Vec::new() });
                    let to = (ppos - t.translation).with_y(0.0);
                    if to.length() > 0.1 {
                        npc.yaw = (-to.x).atan2(-to.z);
                    }
                    if std::env::var("DH_MELEE_LOG").is_ok() {
                        info!("melee: {} slashes out of its dodge: hit {hit:.2}", npc.name);
                    }
                    st.attacking = true;
                    anim.restart(c, false, 1.0, 0.05);
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                // (else back out of it, `Sword_DodgeLeftOut`)
                if let Some(c) = clips.dodge_out[side].filter(|_| Some(dodged) == clips.dodge_in[side].first().copied()) {
                    anim.restart(c, false, 1.0, 0.08);
                    st.oneshot = true;
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
            }
        }
        // a whaler's Attract spell: its hand raised (`Sword_Power_Attract_In`) and held while
        // Corvo is drawn in (`_Loop`), then its coup de grace (`_Attack`, the swing's below) or,
        // the spell broken off, let go (`_Cancel`), standing for it
        if clips.attract[1].is_some() {
            if npc.casting && !npc.is_down() {
                // (its blade waits: the coup de grace starts afresh)
                st.attacking = false;
                if st.attract_phase == 0 {
                    match clips.attract[0] {
                        Some(c) => {
                            anim.restart(c, false, 1.0, 0.12);
                            st.attract_phase = 1;
                        }
                        None => st.attract_phase = 1,
                    }
                } else if st.attract_phase == 1 && (anim.finished() || clips.attract[0].is_none()) {
                    if let Some(c) = clips.attract[1] {
                        anim.play(c, true, 1.0, 0.1);
                    }
                    st.attract_phase = 2;
                }
                st.hit_seen = npc.hit_react;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
            if std::mem::take(&mut st.attract_phase) > 0 && npc.attack_t.is_none() && !npc.is_down() {
                if let Some(c) = clips.attract[3] {
                    anim.restart(c, false, 1.0, 0.1);
                    npc.stagger = npc.stagger.max(lib.duration(c));
                    st.oneshot = true;
                    st.hit_seen = npc.hit_react;
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
            }
        }
        // melee swing: the move's clip, fitted to its duration
        if npc.attack_t.is_some() {
            let counter = st.counter.take();
            if !st.attacking {
                st.attacking = true;
                let (kind, big, len) = npc.swing.as_ref().map(|s| (s.kind, s.big, s.len)).unwrap_or((MoveKind::Short, false, 0.95));
                let own = match kind {
                    MoveKind::Short if big => pick(&clips.attack_short_big).or_else(|| pick(&clips.attack_short)),
                    MoveKind::Short => pick(&clips.attack_short).or_else(|| pick(&clips.attack_short_big)),
                    MoveKind::StepBack => clips.attack_step_back.or_else(|| pick(&clips.attack_short)),
                    MoveKind::Medium => pick(&clips.attack_medium),
                    MoveKind::Long if big => pick(&clips.attack_long_big).or_else(|| pick(&clips.attack_long)),
                    MoveKind::Long => pick(&clips.attack_long).or_else(|| pick(&clips.attack_long_big)),
                    MoveKind::Bash => clips.kick,
                    MoveKind::Jump => clips.jump_attack,
                    MoveKind::Left90 => pick(&clips.attack_turn[0]),
                    MoveKind::Right90 => pick(&clips.attack_turn[1]),
                    MoveKind::Left180 => pick(&clips.attack_turn[2]),
                    MoveKind::Right180 => pick(&clips.attack_turn[3]),
                    MoveKind::Under => clips.under[0],
                    MoveKind::UnderGun => clips.under[1],
                    MoveKind::SawFire => clips.saw_fire[0].or(clips.saw_fire[1]),
                    MoveKind::WhiskeyFire => clips.bottle_fire[0].or(clips.bottle_fire[1]),
                    MoveKind::ThrowWhiskey => clips.bottle_throw,
                    MoveKind::Throw => pick(&clips.throw_rock),
                    MoveKind::Attract => clips.attract[2].or_else(|| pick(&clips.attack_short_big)),
                    MoveKind::Ranged => clips.wrist_shot,
                    MoveKind::Scream => clips.witch[0],
                    MoveKind::Magic => clips.witch[1],
                    MoveKind::Summon => clips.witch[2],
                };
                // (a blade's blow straight out of a dodge to the side: its slash out of it,
                // `_DodgeAttack_Small`)
                let own = match counter {
                    Some((side, _)) if matches!(kind, MoveKind::Short | MoveKind::StepBack | MoveKind::Medium | MoveKind::Long) => clips.dodge_attack[side].or(own),
                    _ => own,
                };
                // an assassin's wrist bow shot, its bolt loosed at the clip's
                // `DishonoredNotify_FireProjectile` (or its fire sound's cue,
                // `AI_Assassin_Ranged_Attack_Fire`, 0.73 s)
                // (a witch's: her magic loosed at each of the clip's four
                // `DishonoredNotify_FireProjectile`s; her scream striking at its
                // `DisDLC07Notify_WitchScreamDamage`, 0.55 s; a tentacle called up as her hand's
                // effect starts, 0.34 s)
                if matches!(kind, MoveKind::Scream | MoveKind::Magic | MoveKind::Summon) {
                    if let (Some(c), Some(s)) = (own, npc.swing.as_mut()) {
                        let fires = st.marks.get(&c).map(|m| m.fires.clone()).unwrap_or_default();
                        match kind {
                            MoveKind::Magic if !fires.is_empty() => {
                                s.hit = fires[0];
                                s.more = fires[1..].to_vec();
                            }
                            MoveKind::Magic => s.hit = 0.41,
                            MoveKind::Scream => s.hit = 0.55,
                            _ => s.hit = 0.34,
                        }
                        s.len = lib.duration(c).max(s.hit + 0.1);
                        anim.restart(c, false, 1.0, 0.1);
                    }
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                if kind == MoveKind::Ranged {
                    if let (Some(c), Some(s)) = (own, npc.swing.as_mut()) {
                        s.hit = st.marks.get(&c).and_then(|m| m.fires.first().copied()).unwrap_or(0.73);
                        s.len = lib.duration(c).max(s.hit + 0.1);
                        anim.restart(c, false, 1.0, 0.1);
                    }
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                // a whaler's coup de grace: its clip as authored, carried at Corvo as far as the
                // clip steps (short of him) until its blow, which lands at its zone's close (or
                // where the spell's sound stops, `AI_Assassin_Pwr_Attract_Loop_Stop`, 0.59 s)
                if let (Some(c), Some(s)) = (own.filter(|_| kind == MoveKind::Attract), npc.swing.as_mut()) {
                    let hit = st.marks.get(&c).and_then(|m| m.zone).map(|(z, d)| (z + d - 0.12).max(z)).unwrap_or(0.59);
                    let room = (ppos - t.translation).with_y(0.0).length() - 1.1;
                    let step = (lib.root_speed(c) * lib.duration(c)).min(room).max(0.0);
                    s.hit = hit;
                    s.len = lib.duration(c).max(hit + 0.1);
                    s.lunge = step / hit.max(0.05);
                    if std::env::var("DH_MELEE_LOG").is_ok() {
                        info!("melee: coup de grace by its clip: hit {hit:.2}, len {:.2}, step {step:.2} m", s.len);
                    }
                    anim.restart(c, false, 1.0, 0.08);
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                // (a throw with the bare hand: let go at the clip's `DishonoredNotify_FireProjectile`)
                if let (Some(c), Some(s)) = (own.filter(|_| kind == MoveKind::Throw), npc.swing.as_mut()) {
                    s.hit = st.marks.get(&c).and_then(|m| m.fires.first().copied()).unwrap_or(lib.duration(c) * 0.6);
                    s.len = lib.duration(c).max(s.hit + 0.1);
                    anim.restart(c, false, 1.0, 0.1);
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                // a thug's whiskey, its bottle drawn into its left hand: its fire breath's swig,
                // spitting and end in turn, the flame striking at the spitting's
                // `DisNotify_WhiskeyFireDamage`; or its throw, let go at the clip's
                // `DishonoredNotify_FireProjectile`
                if let (Some(c), Some(s)) = (own.filter(|_| matches!(kind, MoveKind::WhiskeyFire | MoveKind::ThrowWhiskey)), npc.swing.as_mut()) {
                    let fired = |x: Option<ClipId>, d: f32| x.and_then(|x| st.marks.get(&x)).and_then(|m| m.fires.first().copied()).unwrap_or(d);
                    let dur = |x: Option<ClipId>| x.map_or(0.0, |x| lib.duration(x));
                    let [swig, spit, end] = clips.bottle_fire;
                    let (hit, len) = if kind == MoveKind::WhiskeyFire { (dur(swig) + fired(spit, 0.425), dur(swig) + dur(spit) + dur(end)) } else { (fired(Some(c), 0.4), lib.duration(c)) };
                    s.hit = hit;
                    s.len = len.max(hit + 0.1);
                    hang(&mut commands, &st.bottle, st.bottle_hand);
                    st.bottle_stage = Some(match kind {
                        MoveKind::ThrowWhiskey => 3,
                        _ if swig.is_some() => 0,
                        _ => 1,
                    });
                    anim.restart(c, false, 1.0, 0.1);
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                // a whale butcher's saw's spray: raised (`Saw_RangeAttack_In`), held spraying
                // (`_Loop`: a shot each `m_FiringDelay`, for the loop or `m_fAbortMaxFiringDuration`
                // at most), lowered (`_Out`)
                let saw = npc.arms.saw.clone();
                if let (Some(c), Some(s), Some(saw)) = (own.filter(|_| kind == MoveKind::SawFire), npc.swing.as_mut(), saw) {
                    let dur = |x: Option<ClipId>| x.map_or(0.0, |x| lib.duration(x));
                    let [up, hold, down] = clips.saw_fire;
                    let start = if up.is_some() { dur(up) } else { 0.0 };
                    let spray = dur(hold).clamp(0.5, saw.duration.max(0.5));
                    let mut shots = Vec::new();
                    let mut at = start;
                    while at < start + spray {
                        shots.push(at);
                        at += saw.delay[0] + rand::random::<f32>() * (saw.delay[1] - saw.delay[0]).max(0.0);
                    }
                    if let Some((first, rest)) = shots.split_first() {
                        s.hit = *first;
                        s.more = rest.to_vec();
                    }
                    s.len = start + spray + dur(down);
                    st.saw_stage = Some((if up.is_some() { 0 } else { 1 }, start + spray));
                    anim.restart(c, up.is_none(), 1.0, 0.1);
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                // (under a table: each of the clip's blows, its zones' ends or its shots, the clip
                // as authored)
                if let (Some(c), Some(s)) = (own.filter(|_| matches!(kind, MoveKind::Under | MoveKind::UnderGun)), npc.swing.as_mut()) {
                    let marks = st.marks.get(&c);
                    let blows: Vec<f32> = if kind == MoveKind::UnderGun {
                        marks.map(|m| m.fires.clone()).unwrap_or_default()
                    } else {
                        marks.map(|m| m.zones.iter().map(|(z, d)| (z + d - 0.12).max(*z)).collect()).unwrap_or_default()
                    };
                    if let Some((first, rest)) = blows.split_first() {
                        s.hit = *first;
                        s.more = rest.to_vec();
                    }
                    s.len = lib.duration(c);
                    if kind == MoveKind::UnderGun {
                        hang(&mut commands, &st.gun, st.gun_hand);
                        st.gun_out = true;
                    }
                    anim.restart(c, false, 1.0, 0.1);
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
                if let Some(c) = own.or_else(|| pick(&clips.attack)) {
                    // a blow with its zone (`DishonoredNotify_AttackZone`): its clip as authored,
                    // the blow landing in the zone's last `m_fDamageZoneMaxTime` (0.12 s: the
                    // original's zone handler opens the damage there, as roshan's r614/blink
                    // research decoded it), the attack over once it may be broken off
                    // (`DishonoredNotify_AttackInterruptable`); a lunge kept to its distance
                    let zone = st.marks.get(&c).and_then(|m| m.zone.map(|z| (z, m.interruptible))).filter(|((z, _), _)| *z > 0.05 && !matches!(kind, MoveKind::Bash | MoveKind::Jump));
                    let who = npc.name.clone();
                    match (zone, npc.swing.as_mut()) {
                        (Some(((z, d), intr)), Some(s)) => {
                            let hit = (z + d - 0.12).max(z);
                            if std::env::var("DH_MELEE_LOG").is_ok() {
                                info!("melee: {who} {:?} by its clip: hit {:.2} -> {hit:.2}, len {:.2} -> {:.2}", s.kind, s.hit, s.len, if intr > hit { intr } else { lib.duration(c) }.max(hit + 0.1));
                            }
                            s.lunge *= s.hit / hit.max(0.05);
                            // (stepping back: carried back as far as its clip goes by the blow)
                            if s.kind == MoveKind::StepBack {
                                s.lunge = -lib.root_speed(c) * lib.duration(c) / hit.max(0.05);
                            }
                            s.hit = hit;
                            s.len = if intr > hit { intr } else { lib.duration(c) }.max(hit + 0.1);
                            anim.restart(c, false, 1.0, 0.08);
                        }
                        _ => anim.restart(c, false, lib.duration(c) / len, 0.08),
                    }
                }
            } else if let Some((k, until)) = st.saw_stage.filter(|k| k.0 < 2) {
                // (the spray held, then the saw lowered)
                let t_att = npc.attack_t.unwrap_or(0.0);
                if k == 0 && anim.finished() {
                    if let Some(c) = clips.saw_fire[1] {
                        anim.restart(c, true, 1.0, 0.05);
                    }
                    st.saw_stage = Some((1, until));
                } else if k == 1 && t_att >= until {
                    if let Some(c) = clips.saw_fire[2] {
                        anim.restart(c, false, 1.0, 0.1);
                    }
                    st.saw_stage = Some((2, until));
                }
            } else if let Some(k) = st.bottle_stage.filter(|k| *k < 2) {
                // (the fire breath's next clip)
                if anim.finished() {
                    if let Some(c) = clips.bottle_fire[k as usize + 1] {
                        anim.restart(c, false, 1.0, 0.05);
                    }
                    st.bottle_stage = Some(k + 1);
                }
            }
            st.last_mode = npc.mode;
            st.last_alert = npc.alert;
            continue;
        }
        st.attacking = false;
        st.saw_stage = None;
        // (its bottle back on its hip)
        if st.bottle_stage.take().is_some() {
            hang(&mut commands, &st.bottle, st.bottle_holster.or(st.bottle_hand));
        }

        // a tallboy's bow: drawn as it takes aim (`Empty_Bow_Charged_In`, with its telegraph cue),
        // held (`_Charged`), loosed (`Empty_Bow_Fire`), or let down as the aim is given up
        // (`_Charged_Out`)
        if npc.ranged == 2 && clips.bow[1].is_some() {
            let [draw, held, loose, down] = clips.bow;
            let shot = npc.shots != st.shots_seen;
            st.shots_seen = npc.shots;
            if shot {
                st.bow_drawn = false;
                if let Some(c) = loose {
                    anim.restart(c, false, 1.0, 0.08);
                    st.oneshot = true;
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
            } else if npc.aim.is_some() && !npc.is_down() {
                if !st.bow_drawn {
                    st.bow_drawn = true;
                    if let Some(c) = draw.or(held) {
                        anim.restart(c, draw.is_none(), 1.0, 0.15);
                    }
                } else if anim.finished() {
                    if let Some(c) = held {
                        anim.play(c, true, 1.0, 0.1);
                    }
                }
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            } else if std::mem::take(&mut st.bow_drawn) && !npc.is_down() {
                if let Some(c) = down {
                    anim.restart(c, false, 1.0, 0.1);
                    st.oneshot = true;
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
            }
        }

        // the pistol: drawn into the left hand to aim (`Sword_Gun_Draw`, then `Sword_Gun_Idle`),
        // fired (`Sword_Gun_Fire`), put back on the hip (`Sword_Gun_Undraw`); moving, hurt or
        // down, it goes straight back
        if npc.ranged == 1 && clips.gun[1].is_some() {
            let [draw, idle, fire, undraw] = clips.gun;
            let shot = npc.shots != st.shots_seen;
            st.shots_seen = npc.shots;
            st.gun_t += dt;
            let span = |c: Option<ClipId>, k: f32| c.map(|c| lib.duration(c) * k).unwrap_or(0.0);
            let broken = !shot && ((npc.velocity.with_y(0.0).length() > 0.5 && npc.gun_hold <= 0.0) || npc.hit_react > st.hit_seen + 1e-3 || npc.stagger > 0.0);
            let mut to = st.gun_phase;
            if broken {
                to = GunPhase::Away;
            } else if shot {
                if let Some(c) = fire {
                    anim.restart(c, false, 1.0, 0.05);
                }
                to = GunPhase::Firing;
            } else {
                match st.gun_phase {
                    // (still in the hand: straight back to aiming)
                    GunPhase::Holstering if npc.aim.is_some() && st.gun_out && idle.is_some() => {
                        if let Some(c) = idle {
                            anim.play(c, true, 1.0, 0.15);
                        }
                        to = GunPhase::Aiming;
                    }
                    GunPhase::Away | GunPhase::Holstering if npc.aim.is_some() => {
                        match draw {
                            Some(c) => anim.restart(c, false, 1.0, 0.15),
                            None => {
                                if let Some(c) = idle {
                                    anim.play(c, true, 1.0, 0.15);
                                }
                            }
                        }
                        to = GunPhase::Drawing;
                    }
                    GunPhase::Drawing if draw.is_none() || anim.finished() => {
                        if let Some(c) = idle {
                            anim.play(c, true, 1.0, 0.1);
                        }
                        to = GunPhase::Aiming;
                    }
                    // (each shot reloaded: `DisAIPistolState_Reloading`, `Sword_Gun_Reload` with
                    // its sound and `DisNotify_ReloadAmmo`; no shot before it ends)
                    GunPhase::Firing if anim.finished() && clips.gun_reload.is_some() => {
                        if let Some(c) = clips.gun_reload {
                            anim.restart(c, false, 1.0, 0.1);
                            // (standing for it, as for the shot)
                            npc.shoot_cd = npc.shoot_cd.max(lib.duration(c));
                            npc.gun_hold = npc.gun_hold.max(lib.duration(c));
                        }
                        to = GunPhase::Reloading;
                    }
                    GunPhase::Aiming | GunPhase::Firing | GunPhase::Reloading if npc.aim.is_none() && (st.gun_phase == GunPhase::Aiming || anim.finished()) => {
                        match undraw {
                            Some(c) => {
                                anim.restart(c, false, 1.0, 0.1);
                                to = GunPhase::Holstering;
                            }
                            None => to = GunPhase::Away,
                        }
                    }
                    GunPhase::Firing | GunPhase::Reloading if anim.finished() => {
                        if let Some(c) = idle {
                            anim.play(c, true, 1.0, 0.1);
                        }
                        to = GunPhase::Aiming;
                    }
                    GunPhase::Holstering if anim.finished() => to = GunPhase::Away,
                    _ => {}
                }
            }
            if to != st.gun_phase {
                if std::env::var("DH_GUN_LOG").is_ok() {
                    info!("gun: {} {:?} -> {to:?} (aim {:?}, shot {shot}, broken {broken}: v {:.2} react {:.2}/{:.2} stagger {:.2}, hold {:.2})", npc.name, st.gun_phase, npc.aim.map(|a| a.0), npc.velocity.with_y(0.0).length(), npc.hit_react, st.hit_seen, npc.stagger, npc.gun_hold);
                }
                st.gun_phase = to;
                st.gun_t = 0.0;
            }
            // the gun in the hand from partway through the draw until partway through putting it back
            let out = match st.gun_phase {
                GunPhase::Away => false,
                GunPhase::Drawing => st.gun_t >= span(draw, 0.35),
                GunPhase::Holstering => st.gun_t < span(undraw, 0.6),
                _ => true,
            };
            if out != st.gun_out {
                st.gun_out = out;
                let at = if out { st.gun_hand } else { st.gun_holster.or(st.gun_hand) };
                hang(&mut commands, &st.gun, at);
            }
            // aimed and firing, the arm follows Corvo (the aim offsets blended by where he is
            // off its facing: across 90 degrees each way, up and down)
            anim.additive.retain(|(c, _, _)| !clips.gun_aim.contains(&Some(*c)));
            if matches!(st.gun_phase, GunPhase::Aiming | GunPhase::Firing) {
                let to = (ppos + Vec3::Y * 0.6 - (t.translation + Vec3::Y * 0.72)).normalize_or_zero();
                let fwd = npc.forward();
                let side = fwd.cross(Vec3::Y).normalize_or_zero();
                let x = to.with_y(0.0).normalize_or_zero().dot(side).clamp(-1.0, 1.0).asin() / std::f32::consts::FRAC_PI_2;
                let y = to.y.clamp(-1.0, 1.0).asin() / std::f32::consts::FRAC_PI_2;
                let split = |v: f32| if v < 0.0 { [-v, 1.0 + v, 0.0] } else { [0.0, 1.0 - v, v] };
                let (cols, rows) = (split(x.clamp(-1.0, 1.0)), split(-y.clamp(-1.0, 1.0)));
                for (i, c) in clips.gun_aim.iter().enumerate() {
                    if let Some(c) = c {
                        let w = rows[i / 3] * cols[i % 3];
                        if w > 1e-3 {
                            anim.additive.push((*c, 0.0, w));
                        }
                    }
                }
            }
            if st.gun_phase != GunPhase::Away {
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
        }

        // Corvo out of it: thrown off as he comes out, then dazed or retching (one or the other,
        // our reading), standing for it all
        if std::mem::take(&mut npc.unpossessed) && !npc.is_down() {
            // (without its clips, it stands reeling a while)
            npc.stagger = npc.stagger.max(3.0);
            if let Some(c) = clips.unpossess[0] {
                let then = if rand::random::<bool>() { clips.unpossess[1] } else { clips.unpossess[2] };
                anim.restart(c, false, 1.0, 0.1);
                npc.stagger = npc.stagger.max(lib.duration(c) + then.map_or(0.0, |t| lib.duration(t)));
                st.unpossess_next = then;
                st.oneshot = true;
                st.hit_seen = npc.hit_react;
                continue;
            }
        }
        if let Some(c) = st.unpossess_next.filter(|_| anim.finished()) {
            st.unpossess_next = None;
            if !npc.is_down() {
                anim.restart(c, false, 1.0, 0.15);
                st.oneshot = true;
                continue;
            }
        }
        // a swarm on it, eating it alive: it fights it off (`Sword_SwarmAttack_In`, `_Loop` while
        // they stay, `_Out` once they are gone; `Empty_` with its hands empty), above its flinches
        if swarmed.contains(&e) {
            st.swarm_t = 0.0;
        } else {
            st.swarm_t += dt;
        }
        let [swarm_in, swarm_loop, swarm_out] = if st.drawn { clips.swarm[1] } else { clips.swarm[0] };
        if !npc.is_down() && swarm_loop.is_some() {
            match (st.swarm_t < 0.4, st.swarm_phase) {
                (true, 0) => {
                    match swarm_in {
                        Some(c) => {
                            anim.restart(c, false, 1.0, 0.1);
                            st.swarm_phase = 1;
                        }
                        None => st.swarm_phase = 2,
                    }
                    st.hit_seen = npc.hit_react;
                    continue;
                }
                (true, 1) if anim.finished() => {
                    if let Some(c) = swarm_loop {
                        anim.play(c, true, 1.0, 0.1);
                    }
                    st.swarm_phase = 2;
                    st.hit_seen = npc.hit_react;
                    continue;
                }
                (true, _) => {
                    st.hit_seen = npc.hit_react;
                    continue;
                }
                (false, p) if p > 0 => {
                    st.swarm_phase = 0;
                    if let Some(c) = swarm_out {
                        anim.restart(c, false, 1.0, 0.15);
                        st.oneshot = true;
                        st.hit_seen = npc.hit_react;
                        continue;
                    }
                }
                _ => {}
            }
        }

        // choking in chokedust where it stands: its smoke clips (`Generic_SmokeBomb_In`, `_Loop`,
        // `_Out`) or, its set without them, doubled over (`Sword_Stun_Loop`) and up again
        // (`Sword_RecoveryA` / `B`: our reading of them), coming out of it as its stun ends
        if npc.dusted {
            let hands = (!st.drawn) as usize;
            let [d_in, d_loop, d_out] = if clips.dust[1].is_some() {
                clips.dust
            } else {
                [None, clips.stun_loop[hands].or(clips.stun_loop[1 - hands]), pick(&clips.recovery[hands]).or_else(|| pick(&clips.recovery[1 - hands]))]
            };
            let out_len = d_out.map_or(0.0, |c| lib.duration(c));
            match d_loop.filter(|_| !npc.is_down()) {
                // (without a loop, it reels as struck)
                None => {
                    npc.dusted = false;
                    st.dust_phase = 0;
                }
                Some(_) if st.dust_phase > 0 && npc.stagger <= out_len => {
                    npc.dusted = false;
                    st.dust_phase = 0;
                    if let Some(c) = d_out {
                        anim.restart(c, false, 1.0, 0.2);
                        st.oneshot = true;
                        st.hit_seen = npc.hit_react;
                        st.last_mode = npc.mode;
                        st.last_alert = npc.alert;
                        continue;
                    }
                }
                Some(l) => {
                    match (st.dust_phase, d_in) {
                        (0, Some(c)) => {
                            anim.restart(c, false, 1.0, 0.15);
                            st.dust_phase = 1;
                        }
                        (0, None) => {
                            anim.restart(l, true, 1.0, 0.2);
                            st.dust_phase = 2;
                        }
                        (1, _) if anim.finished() => {
                            anim.play(l, true, 1.0, 0.1);
                            st.dust_phase = 2;
                        }
                        _ => {}
                    }
                    st.hit_seen = npc.hit_react;
                    st.last_mode = npc.mode;
                    st.last_alert = npc.alert;
                    continue;
                }
            }
        }

        // reactions
        let mut reacted = false;
        if npc.hit_react > st.hit_seen + 1e-3 {
            // the reactions of the hands it has: sword drawn, or empty
            let pool = if (st.drawn && !st.sword.is_empty() || clips.hit_empty.is_empty()) && !clips.hit.is_empty() { &clips.hit } else if clips.hit_empty.is_empty() { &clips.hit } else { &clips.hit_empty };
            // (the side struck, where its clips have one)
            let dirs = if std::ptr::eq(pool, &clips.hit) { &clips.hit_dir } else { &clips.hit_dir_empty };
            let pool = match side_struck(&npc, t.translation) {
                Some(s) if !dirs[s].is_empty() => &dirs[s],
                _ => pool,
            };
            // (shot in a leg: its leg's)
            let leg = npc.leg_hit.take().and_then(|r| clips.hit_leg[(!std::ptr::eq(dirs, &clips.hit_dir)) as usize][r as usize]);
            // (struck by a thing thrown: its small hit, by the side it struck)
            let small = std::mem::take(&mut npc.small_hit).then(|| side_struck(&npc, t.translation).and_then(|s| clips.small_hit[s])).flatten();
            // (shot from ahead: its projectile hit, where its set has one)
            let shot = std::mem::take(&mut npc.shot_hit).then(|| side_struck(&npc, t.translation).and_then(|s| clips.shot_hit.get(s).copied().flatten())).flatten();
            let c = if npc.stagger > 1.0 { clips.stun.or(pick(pool)) } else { leg.or(small).or(shot).or_else(|| pick(pool)) };
            if let Some(c) = c {
                anim.restart(c, false, 1.3, 0.06);
                st.oneshot = true;
                reacted = true;
            }
        }
        st.hit_seen = npc.hit_react;
        // a gesture asked for: its parry won, its blow parried, a shove, a sword lock
        if npc.act.1 != st.act_seen {
            st.act_seen = npc.act.1;
            let pool = match npc.act.0 {
                1 => &clips.parry_win,
                2 => &clips.parry_lost,
                4..=7 => &clips.versus[npc.act.0 as usize - 4],
                8 => &clips.stomp,
                9 => &clips.ambush[0],
                10 => &clips.ambush[2],
                11 => &clips.ambush[3],
                12 => &clips.taunt,
                13 => &clips.cough,
                14 => &clips.check_pockets,
                15..=17 => &clips.dodge[npc.act.0 as usize - 15],
                22 | 23 => &clips.dodge_in[npc.act.0 as usize - 22],
                18 => &clips.trigger_alarm,
                19 => &clips.corpse_check,
                20 => &clips.magic[st.drawn as usize],
                21 => &clips.sticky,
                _ => &clips.push,
            };
            if let Some(c) = pick(pool) {
                // (its pockets checked where it stands; the alarm pulled, a body looked over, Corvo
                // gone by magic, a sticky grenade flailed at)
                if matches!(npc.act.0, 14 | 18 | 19 | 20 | 21) {
                    npc.stagger = npc.stagger.max(anim.lib.duration(c));
                }
                if matches!(npc.act.0, 15 | 16) && clips.dodge_attack[npc.act.0 as usize - 15].is_some() {
                    st.counter = Some((npc.act.0 as usize - 15, c));
                }
                if matches!(npc.act.0, 22 | 23) {
                    st.counter = Some((npc.act.0 as usize - 22, c));
                }
                // (the lock's struggle holds until it ends)
                let hold = npc.act.0 == 4;
                anim.restart(c, hold, if hold || npc.act.0 > 4 { 1.0 } else { 1.2 }, 0.08);
                st.oneshot = true;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
        }
        // locked blades: the struggle plays on
        if npc.versus {
            continue;
        }
        // lying in wait: in, then the wait's loop
        // caught begging: down on its knees, then the loop (or cowering where it stands); up
        // again as it lets go
        if npc.caught.is_some() {
            let [beg_in, beg_loop, _] = clips.beg;
            if !st.begging {
                st.begging = true;
                if let Some(c) = beg_in.filter(|_| beg_loop.is_some()) {
                    anim.restart(c, false, 1.0, 0.2);
                    continue;
                }
            }
            if anim.finished() || anim.current().is_none_or(|p| Some(p.clip) != beg_in && Some(p.clip) != beg_loop.or(clips.cower)) {
                if let Some(c) = beg_loop.or(clips.cower) {
                    anim.restart(c, true, 1.0, 0.25);
                }
            }
            continue;
        }
        if st.begging {
            st.begging = false;
            if let Some(c) = clips.beg[2].filter(|_| clips.beg[1].is_some()) {
                anim.restart(c, false, 1.0, 0.2);
                st.oneshot = true;
                continue;
            }
        }
        if npc.ambush.is_some() {
            if anim.finished() {
                if let Some(c) = pick(&clips.ambush[1]) {
                    anim.restart(c, true, 1.0, 0.1);
                }
            }
            continue;
        }
        // (turned on Corvo by the blow it is flinching from: the flinch shows, its sword in hand)
        let searching = |m: Mode| matches!(m, Mode::Search | Mode::Investigate);
        let to_fight = (npc.alert == Alert::Combat && st.last_alert != Alert::Combat) || (npc.mode == Mode::Combat && searching(st.last_mode));
        if to_fight && !reacted {
            st.next_clip = None;
            // (from its search, its sword already drawn in its patrol stance: raised to fight,
            // `Sword_Patrol_PatrolToSword`; else drawn to fight, `DisNPCAnim_EquipToCombat`)
            let raise = clips.patrol_to_sword.filter(|_| searching(st.last_mode));
            let draw = raise.or(clips.equip_combat);
            if let Some(c) = draw.or(clips.surprised) {
                // (on the move, its sword drawn on its upper body, its legs keeping their stride:
                // the NPC tree's upper-body slot)
                if npc.anim_speed >= 0.6 && draw.is_some() && !musical {
                    anim.restart_upper(c, false, 1.4, 0.15);
                } else {
                    anim.restart(c, false, 1.4, 0.15);
                    st.oneshot = true;
                }
            }
        } else if npc.mode == Mode::Search && (st.last_mode != Mode::Search || st.lost_pending) && !reacted {
            // (searching for what struck it: the flinch shows first) Corvo lost in the fight, it
            // lowers its sword to its patrol stance, then looks about
            // (`DisNPCAnim_SearchLostTarget`); else it looks into what it noticed
            let look = clips.out_of_sight[st.drawn as usize];
            if std::mem::take(&mut st.lost_pending) && look.is_some() {
                // (standing for it, as for Corvo gone by magic)
                if let Some(c) = clips.sword_to_patrol.filter(|_| st.drawn) {
                    anim.restart(c, false, 1.0, 0.2);
                    st.next_clip = look;
                    npc.stagger = npc.stagger.max(lib.duration(c) + look.map_or(0.0, |l| lib.duration(l)));
                } else if let Some(c) = look {
                    anim.restart(c, false, 1.0, 0.25);
                    npc.stagger = npc.stagger.max(lib.duration(c));
                }
                st.oneshot = true;
            } else if let Some(c) = pick(&clips.search).or(look) {
                // (after its draw, if it has just drawn)
                if drew {
                    st.next_clip = Some(c);
                } else {
                    anim.restart(c, false, 1.0, 0.25);
                    st.oneshot = true;
                }
            }
        }
        st.last_mode = npc.mode;
        st.last_alert = npc.alert;
        let sp = npc.anim_speed;
        if st.oneshot && !anim.finished() && (sp < 0.6 || npc.stagger > 0.0 || npc.retreat.is_some()) {
            continue;
        }
        st.oneshot = false;

        // locomotion, played at the rate that matches the ground speed
        let alert = npc.alert != Alert::Unaware || matches!(npc.mode, Mode::Combat | Mode::Search | Mode::Investigate);
        // (searching or looking into something, it keeps its patrol stance, its sword in hand: the
        // original's search stance is its patrol's, `DisNPCAnim_EquipToSearch` drawing into it)
        let search_stance = (matches!(npc.mode, Mode::Search | Mode::Investigate) || npc.suspecting && npc.alert == Alert::Unaware) && st.drawn;
        // its stance's locomotion, as its body stance picks its locomotion modifier (0x00B71000,
        // as roshan's r614/blink research decoded it): panicking; fighting; the sword drawn
        // otherwise, its patrol stance (`SwordPatrol`, `SearchSword`); sheathed, on its rounds
        // (`EmptyPatrol`) or not (`Empty`)
        let stance = if npc.mode == Mode::Flee {
            LOCO_PANIC
        } else if alert && !search_stance {
            LOCO_SWORD
        } else if st.drawn && !st.sword.is_empty() {
            LOCO_SWORD_PATROL
        } else if npc.mode == Mode::Patrol {
            LOCO_PATROL
        } else {
            LOCO_EMPTY
        };
        npc.stance = stance;
        let set = &clips.loco[stance];
        // a patrolling or guarding character's ambient idles (`DisTweaks_AISubProcess_AmbientAnims`
        // of the patrol and guard behaviours: every 30-60 s, `m_fMin/MaxAmbientAnimFrequency`, on
        // the upper body and walking too, `m_bCheckForMovement` false, as roshan's r614/blink
        // research found); the relaxed (civilians idling, wandering) have none
        if !musical && anim.upper_on() && anim.upper_finished() {
            anim.stop_upper(0.3);
        }
        if alert {
            // (a change of behaviour starts its ambient idles over)
            st.idle_time = rand::random::<f32>() * AMBIENT_IDLE[1];
            if anim.upper_on() && !musical && fidget {
                anim.stop_upper(0.2);
            }
        } else if npc.fighter() && !musical {
            st.idle_time -= dt;
            if st.idle_time <= 0.0 {
                st.idle_time = AMBIENT_IDLE[0] + rand::random::<f32>() * (AMBIENT_IDLE[1] - AMBIENT_IDLE[0]);
                if let Some(c) = pick(&set.fidgets) {
                    anim.restart_upper(c, false, 1.0, 0.3);
                }
            }
        }
        // a relaxed walk begun and ended with its clips: its idle speed mode's start, stop and turn
        // props (`m_StartAnimProps`, `m_StopAnimProps`, the `TurnToWalk` turns), by the
        // locomotion component's rules as roshan's r614/blink research decoded them; its clip
        // carries it meanwhile (`npc_move`), shown turning across a turn into the walk
        let relaxed = !alert && matches!(npc.mode, Mode::Idle | Mode::Patrol) && !far && !musical && npc.stagger <= 0.0 && npc.attack_t.is_none() && npc.grounded;
        if let Some(p) = npc.pace {
            let moving_turn = p.way.is_some() && p.turn.is_some() && !far && npc.stagger <= 0.0 && npc.attack_t.is_none();
            if (relaxed || moving_turn) && anim.current().is_some_and(|c| c.clip == p.clip) {
                match p.turn {
                    Some((from, by)) => st.shown_yaw = from + by * ease(p.t / p.len),
                    // (a stop: shown as it was going)
                    None if p.way.is_some() => {}
                    None => st.shown_yaw = npc.yaw,
                }
                continue;
            }
            npc.pace = None;
        }
        let loco_log = std::env::var("DH_LOCO_LOG").is_ok();
        // (from where it stands, the way within 15 degrees of its facing, its path straight ahead
        // further than the props' straight distance: its start; further round, its turn into the
        // walk by the nearest 45 degrees, as far round as it is)
        if relaxed && sp < 0.3 && st.turn.is_none() && npc.wish.length_squared() > 0.01 && npc.path.is_some_and(|(_, next)| next > WALK_STRAIGHT) {
            let way = (-npc.wish.x).atan2(-npc.wish.z);
            let off = (way - st.shown_yaw + PI).rem_euclid(TAU) - PI;
            let pick = if off.abs() <= 15f32.to_radians() {
                set.walk_start.map(|c| (c, None))
            } else {
                let i = ((off.abs().to_degrees() / 45.0).round() as usize).clamp(1, 4) - 1;
                set.turn_to_walk[(off < 0.0) as usize][i].map(|c| (c, Some((st.shown_yaw, off))))
            };
            if let Some((c, turn)) = pick {
                let len = lib.duration(c);
                let walk = npc.gait[0].max(0.3);
                // (still, then up to its walk's pace by the clip's end, as far as the clip goes)
                let hold = (len - 2.0 * lib.root_speed(c) * len / walk).clamp(0.0, len * 0.8);
                anim.restart(c, false, 1.0, 0.1);
                npc.pace = Some(Pace { clip: c, t: 0.0, len, ramp: [hold, len], speed: [0.0, walk], turn, way: None });
                if turn.is_some() {
                    npc.yaw = way;
                }
                st.walk_phase = f32::NAN;
                if loco_log {
                    info!("loco: {} starts walking with {} ({:.0} deg round, {hold:.2} s still)", npc.name, lib.clip(c).name, off.to_degrees());
                }
                continue;
            }
        }
        // (walking to where it stops, the way within 22.5 degrees of its facing: as a foot lands,
        // the left half through its walk's loop and the right at its start (the stops'
        // `m_fMoveAnimIn`), its stop for that foot, unless the next lands nearer the stop's mark,
        // the props' straight distance short of its goal)
        let walking = anim.current().filter(|c| relaxed && npc.pace.is_none() && sp > 0.6 && set.modes[0][2].is_some_and(|(n, _)| n == c.clip));
        match (walking, npc.path) {
            (Some(cur), Some((left, _))) => {
                let ph = (cur.t / lib.duration(cur.clip)).rem_euclid(1.0);
                let was = std::mem::replace(&mut st.walk_phase, ph);
                let foot = if was < 0.5 && ph >= 0.5 { Some(0) } else if ph < was { Some(1) } else { None };
                let way = (-npc.wish.x).atan2(-npc.wish.z);
                let ahead = npc.wish.length_squared() > 0.01 && ((way - npc.yaw + PI).rem_euclid(TAU) - PI).abs() <= 22.5f32.to_radians();
                let next = npc.stride * 0.5;
                if let Some(c) = foot.and_then(|f| set.walk_stop[f]).filter(|_| ahead && left < UNACCURACY && left - WALK_STRAIGHT < next * 0.5) {
                    let len = lib.duration(c);
                    let v0 = npc.velocity.with_y(0.0).length().max(0.3);
                    // (down from its pace to a stop as far as the clip goes)
                    let by = (2.0 * lib.root_speed(c) * len / v0).clamp(0.2, len);
                    anim.restart(c, false, 1.0, 0.1);
                    npc.pace = Some(Pace { clip: c, t: 0.0, len, ramp: [0.0, by], speed: [v0, 0.0], turn: None, way: Some(npc.forward()) });
                    st.walk_phase = f32::NAN;
                    if loco_log {
                        info!("loco: {} stops with {} {left:.2} m short of its goal (its walk {was:.2} -> {ph:.2}, t {:.2})", npc.name, lib.clip(c).name, cur.t);
                    }
                    continue;
                }
            }
            _ => st.walk_phase = f32::NAN,
        }
        // (on the move, facing its way, its way turned back on itself by 135 degrees or more: its
        // walk's or run's turn round, the speed mode's 180 degree turn props by side (two props
        // spanning 45 degrees each side of 180, as roshan's r614/blink research decoded the
        // locomotion component's moving turn). It slows to a stop over the turn's start, shown
        // turning round across the clip, then goes the new way.)
        let fore = anim.current().and_then(|c| (0..2).find(|&g| set.modes[g][2].is_some_and(|(n, _)| n == c.clip)));
        if let Some(gait) = fore.filter(|_| npc.pace.is_none() && sp >= 0.4 && !far && st.turn.is_none() && npc.stagger <= 0.0 && npc.attack_t.is_none() && npc.wish.length_squared() > 0.01) {
            let way = (-npc.wish.x).atan2(-npc.wish.z);
            let off = (way - npc.yaw + PI).rem_euclid(TAU) - PI;
            if let Some(c) = set.move_turn[gait][(off < 0.0) as usize].filter(|_| off.abs() >= 135f32.to_radians()) {
                let len = lib.duration(c);
                let v0 = npc.velocity.with_y(0.0).length();
                anim.restart(c, false, 1.0, 0.15);
                npc.pace = Some(Pace { clip: c, t: 0.0, len, ramp: [0.0, len * 0.3], speed: [v0, 0.0], turn: Some((st.shown_yaw, off)), way: Some(npc.forward()) });
                npc.yaw = way;
                if loco_log {
                    info!("loco: {} turns round with {} ({:.0} deg)", npc.name, lib.clip(c).name, off.to_degrees());
                }
                continue;
            }
        }
        // turning where it stands: a turn of more than 40 degrees with its turn's clip for the
        // nearest of 45, 90, 135 or 180 degrees, its body shown turning across it; less, gliding
        if sp < 0.3 {
            let off = (npc.yaw - st.shown_yaw + PI).rem_euclid(TAU) - PI;
            match st.turn {
                Some((from, by, t, len)) => {
                    let t = t + dt;
                    if t >= len {
                        st.turn = None;
                        st.shown_yaw = from + by;
                    } else {
                        st.turn = Some((from, by, t, len));
                        st.shown_yaw = from + by * ease(t / len);
                        continue;
                    }
                }
                None if off.abs() > 40f32.to_radians() => {
                    let i = ((off.abs().to_degrees() / 45.0).round() as usize).clamp(1, 4) - 1;
                    // (a turn to the left is a rising yaw)
                    let side = (off < 0.0) as usize;
                    if let Some(c) = set.turn[side][i].or_else(|| clips.loco.iter().find_map(|s| s.turn[side][i])) {
                        let len = lib.duration(c).max(0.2);
                        anim.restart(c, false, 1.0, 0.15);
                        st.turn = Some((st.shown_yaw, off, 0.0, len));
                        continue;
                    }
                    st.shown_yaw = npc.yaw;
                }
                None => st.shown_yaw += off.clamp(-6.0 * dt, 6.0 * dt),
            }
        }
        if sp < 0.3 {
            // (run as far as it goes, a civilian cowers there: the panic's stance)
            let c = if npc.mode == Mode::Flee && !npc.fighter() { clips.cower.or(set.idle).or(clips.idle) } else { set.idle.or(clips.idle) };
            if let Some(c) = c {
                anim.play(c, true, 1.0, 0.3);
            }
        } else if let Some((master, mix, rate, stride)) = loco_mix(&lib, set, st.loco_v) {
            // moving: its stance's loops blended by its way and speed, at one phase
            anim.play_mix(master, mix, rate, 0.25);
            // (its feet's on-ground share: the moving modes' `m_fFootOnGroundPct`, walk 0.4,
            // run 0.2, by their weights)
            let (mut walk, mut run) = (0.0, 0.0);
            for (c, w, _) in mix.loops.iter().flatten() {
                if set.modes[0].iter().flatten().any(|l| l.0 == *c) {
                    walk += w;
                } else if set.modes[1].iter().flatten().any(|l| l.0 == *c) {
                    run += w;
                }
            }
            let share = if walk + run > 1e-3 { (0.4 * walk + 0.2 * run) / (walk + run) } else { 0.4 };
            npc.sync = anim.current().map(|p| ((p.t / lib.duration(p.clip)).rem_euclid(1.0), share));
            if (stride - npc.stride).abs() > 0.05 && std::env::var("DH_LOCO_LOG").is_ok() {
                let names: Vec<String> = mix.loops.iter().flatten().map(|(c, w, back)| format!("{}{} {w:.2}", lib.clip(*c).name, if *back { " (back)" } else { "" })).collect();
                let heading = |v: Vec3| (-v.x).atan2(-v.z).to_degrees();
                info!(
                    "loco: {} at {:.2} m/s strides {stride:.2} m a loop, rate {rate:.2}: {} (facing {:.0}, going {:.0}, meaning {:.0}, {:?})",
                    npc.name,
                    st.loco_v.length(),
                    names.join(", "),
                    npc.yaw.to_degrees(),
                    heading(npc.velocity),
                    heading(npc.wish),
                    npc.mode
                );
            }
            npc.stride = stride;
        }
    }
}

/// The stances' locomotion sets (`NpcClips::loco`), as a body stance picks its locomotion
/// modifier: sheathed (`Empty`), sheathed on its rounds (`EmptyPatrol`), the sword drawn
/// relaxed (`SwordPatrol`, searching), fighting (`Sword`), panicking (`PanicEmpty`).
const LOCO_EMPTY: usize = 0;
const LOCO_PATROL: usize = 1;
const LOCO_SWORD_PATROL: usize = 2;
const LOCO_SWORD: usize = 3;
const LOCO_PANIC: usize = 4;

/// How a character's locomotion turns as it walks (its `NpcLoco`, as 0x00941330 turns it): its
/// rows `LOCO_*`, then searching empty-handed and a weeper's (the default config's `Search`
/// and `Weeper` speed modes), each gait's top rate (rad/s; walk, run; 0: its config has none,
/// and it turns as before); its acceleration over its rate; and on a sharp turn on its path
/// (over `wall_angle`), its rate's multiplier with a wall `wall_front` ahead, and with one only
/// `wall_side` to the side it turns.
#[derive(Clone, Copy, Default, Debug)]
pub struct LocoTurn {
    rates: [[f32; 2]; 7],
    /// each modifier's acceleration and deceleration (m/s²: `Empty`, `EmptyPatrol`,
    /// `SwordPatrol`, `Sword`, `PanicEmpty`, `SearchEmpty`, `SearchSword`; 0: none of its own)
    speedup: [[f32; 2]; 7],
    pub accel_mult: f32,
    pub wall_angle: f32,
    pub wall_front: f32,
    pub wall_side: f32,
    pub wall_mult: f32,
    pub side_mult: f32,
}

const LOCO_SEARCH: usize = 5;
const LOCO_WEEPER: usize = 6;

impl LocoTurn {
    pub fn of(l: &dhcook::format::NpcLoco) -> LocoTurn {
        // each row's speed modes by gait, the first its config has (a wolfhound's are its own)
        let names: [[&[&str]; 2]; 7] = [
            [&["EmptyWalk", "WalkNormal"], &["EmptyRun", "RunNormal"]],
            [&["EmptyWalkPatrol", "EmptyPatrolWalk", "EmptyWalk", "WalkNormal"], &["EmptyRun", "RunNormal"]],
            [&["SwordPatrolWalk", "SearchWalk", "SwordWalk", "WalkSearch"], &["SearchRun", "SwordRun", "RunNormal"]],
            [&["SwordWalk", "WalkAttack"], &["SwordRun", "RunAttack", "RunNormal"]],
            [&["PanicEmptyWalk", "PanicWalk", "EmptyWalk"], &["PanicEmptyRun", "PanicRun", "PanicSwordRun", "EmptyRun"]],
            [&["SearchWalk", "WalkSearch", "EmptyWalk"], &["SearchRun", "EmptyRun", "RunNormal"]],
            [&["WeeperWalk", "EmptyWalk"], &["WeeperRun", "EmptyRun"]],
        ];
        let rate = |all: &[&str]| all.iter().find_map(|n| l.modes.iter().find(|(m, _)| m.eq_ignore_ascii_case(n))).map_or(0.0, |(_, r)| r.to_radians());
        let modifier = |n: &str| l.modifiers.iter().find(|m| m.0.eq_ignore_ascii_case(n)).map_or([0.0; 2], |m| [m.1, m.2]);
        LocoTurn {
            rates: names.map(|row| row.map(rate)),
            speedup: ["Empty", "EmptyPatrol", "SwordPatrol", "Sword", "PanicEmpty", "SearchEmpty", "SearchSword"].map(modifier),
            accel_mult: l.accel_mult,
            wall_angle: l.wall_angle.to_radians(),
            wall_front: l.wall_front,
            wall_side: l.wall_side,
            wall_mult: l.wall_mult,
            side_mult: l.side_mult,
        }
    }

    /// How fast it speeds up and slows down in `stance` (m/s²), by its locomotion modifier: its
    /// stance's, searching `SearchEmpty` / `SearchSword` (as its body stance picks it, 0x00B71000).
    pub fn speedup(&self, stance: usize, mode: Mode) -> Option<[f32; 2]> {
        let searching = matches!(mode, Mode::Search | Mode::Investigate);
        let row = match stance {
            LOCO_EMPTY if searching => 5,
            LOCO_SWORD_PATROL if searching => 6,
            s => s,
        };
        self.speedup.get(row).copied().filter(|[up, down]| *up > 0.0 && *down > 0.0)
    }

    /// Its top turn rate walking or running in `stance` (rad/s), if its config gives one.
    pub fn rate(&self, stance: usize, mode: Mode, running: bool, weeper: bool) -> Option<f32> {
        let row = if weeper {
            LOCO_WEEPER
        } else if stance == LOCO_EMPTY && matches!(mode, Mode::Search | Mode::Investigate) {
            LOCO_SEARCH
        } else {
            stance
        };
        Some(self.rates.get(row)?[running as usize]).filter(|r| *r > 0.0 && self.accel_mult > 0.0)
    }
}

/// A turn as the locomotion makes it (0x004B2A30): its angular speed `v` (signed) speeds up at
/// `accel` to `rate` and slows in time to arrive; turning back, it flips at once; a step that
/// would pass the way it wants ends there. The yaw to add this frame, `dy` being what is left.
fn turn_toward(dy: f32, v: &mut f32, rate: f32, accel: f32, dt: f32) -> f32 {
    if dy == 0.0 {
        *v = 0.0;
        return 0.0;
    }
    if *v * dy < 0.0 {
        *v = -*v;
    }
    let left = dy.abs();
    let braking = 0.5 * *v * *v / accel >= left;
    let s = if braking { (v.abs() - accel * dt).max(0.0) } else { (v.abs() + accel * dt).min(rate) };
    *v = s * dy.signum();
    let step = *v * dt;
    if step.abs() >= left {
        *v = 0.0;
        dy
    } else {
        step
    }
}

/// A stance's locomotion, as its locomotion modifier's speed modes list it
/// (`ArkComponentLocomotionConfig`): its idle loop, its walk's and its run's loops by way (S, W,
/// N, E; S and W its `_Revert` loops, played backwards), its ambient idles, and its idle speed
/// mode's start, stops and turns into a walk (`m_StartAnimProps`, `m_StopAnimProps`,
/// `TurnToWalk`), its walk's and run's turns round, and its turns where it stands, left then
/// right by 45, 90, 135 and 180 degrees.
#[derive(Clone, Default)]
pub struct LocoSet {
    idle: Option<ClipId>,
    modes: [[Option<(ClipId, bool)>; 4]; 2],
    fidgets: Vec<ClipId>,
    walk_start: Option<ClipId>,
    walk_stop: [Option<ClipId>; 2],
    turn_to_walk: [[Option<ClipId>; 4]; 2],
    move_turn: [[Option<ClipId>; 2]; 2],
    turn: [[Option<ClipId>; 4]; 2],
}

/// The locomotion node's blend (`ArkAnimNodeLocomotion`: 0x009331D0, 0x00910E30, 0x009105B0,
/// 0x00910970, 0x00916950, as roshan's r614/blink research decoded them). The way it goes (`v`,
/// its smoothed velocity in its own frame, x to its right, y ahead) weighs each moving speed
/// mode's two loops either side of it (S, W, N, E a quarter apart; a weight from 0.98 takes
/// all; a lone loop takes every way). Its speed, stretched on a diagonal to the chord between
/// the two (`cos + sin` of the way past the first), weighs the two modes whose blended authored
/// speeds bracket it, the idle's 0. S and W play backwards. All keep one phase, the heaviest
/// leading, at the rate that matches the ground: its length times the speed over the loops'
/// authored strides summed by weight (1 standing). Returns the master, the mix, the master's
/// rate and the stride (the ground one loop covers).
fn loco_mix(lib: &CharAnims, set: &LocoSet, v: Vec2) -> Option<(ClipId, LocoMix, f32, f32)> {
    let speed = v.length();
    // (from ahead, N, round to the right, E)
    let angle = v.x.atan2(v.y).to_degrees();
    let ways = |m: &[Option<(ClipId, bool)>; 4]| -> ([(usize, f32); 2], bool) {
        if m.iter().filter(|l| l.is_some()).count() <= 1 {
            return ([(m.iter().position(Option::is_some).unwrap_or(2), 1.0), (0, 0.0)], false);
        }
        // (S 0, W 1, N 2, E 3 at 180, 270, 0 and 90 degrees)
        let q = (angle + 180.0).rem_euclid(360.0) / 90.0;
        let a = (q.floor() as usize) % 4;
        let f = q - q.floor();
        let (wa, wb) = if f <= 0.02 { (1.0, 0.0) } else if f >= 0.98 { (0.0, 1.0) } else { (1.0 - f, f) };
        // (a way the set lacks: ahead's)
        let pick = |k: usize| if m[k].is_some() { k } else { 2 };
        ([(pick(a), wa), (pick((a + 1) % 4), wb)], true)
    };
    let mut modes: Vec<(f32, Vec<(ClipId, f32, bool)>)> = Vec::new();
    if let Some(i) = set.idle {
        modes.push((0.0, vec![(i, 1.0, false)]));
    }
    let mut ratio = None;
    for m in &set.modes {
        if m.iter().all(Option::is_none) {
            continue;
        }
        let (w, multi) = ways(m);
        let mut s = 0.0;
        let mut loops: Vec<(ClipId, f32, bool)> = Vec::new();
        for (k, wk) in w {
            let Some((c, back)) = m[k].filter(|_| wk > 0.0) else { continue };
            s += wk * lib.root_speed(c);
            match loops.iter_mut().find(|l| l.0 == c) {
                Some(l) => l.1 += wk,
                None => loops.push((c, wk, back && multi)),
            }
        }
        // (the diagonal's stretch, by the first moving mode's two loops)
        if ratio.is_none() {
            let phi = w[1].1 * std::f32::consts::FRAC_PI_2;
            ratio = Some(if multi && w[0].1 > 0.0 && w[1].1 > 0.0 { phi.cos() + phi.sin() } else { 1.0 });
        }
        // (a mode no faster than the one below it brackets nothing)
        if modes.last().is_some_and(|(below, _)| s <= *below + 0.05) {
            continue;
        }
        modes.push((s, loops));
    }
    if modes.is_empty() {
        return None;
    }
    let sp = speed * ratio.unwrap_or(1.0);
    let mut weights = vec![0.0f32; modes.len()];
    match modes.iter().position(|(s, _)| *s >= sp) {
        Some(0) => weights[0] = 1.0,
        None => *weights.last_mut()? = 1.0,
        Some(i) => {
            let (lo, hi) = (modes[i - 1].0, modes[i].0);
            let w = ((sp - lo) / (hi - lo).max(1e-3)).clamp(0.0, 1.0);
            let w = if w >= 0.98 { 1.0 } else if w <= 0.02 { 0.0 } else { w };
            weights[i] = w;
            weights[i - 1] = 1.0 - w;
        }
    }
    let mut mix = LocoMix::default();
    let mut n = 0;
    let mut sum = 0.0;
    let mut master = None;
    let mut heaviest = 0.0;
    for (wm, (_, loops)) in weights.iter().zip(&modes) {
        for &(c, wl, back) in loops {
            let w = wm * wl;
            if w <= 0.0 || n >= mix.loops.len() {
                continue;
            }
            mix.loops[n] = Some((c, w, back));
            n += 1;
            sum += lib.root_speed(c) * lib.duration(c) * w;
            if w > heaviest {
                heaviest = w;
                master = Some(c);
            }
        }
    }
    let master = master?;
    let rate = if sp < 1e-3 || sum < 1e-3 { 1.0 } else { lib.duration(master) * sp / sum };
    Some((master, mix, rate, sum))
}

/// The joints from `spine_0_jnt` down its chain (a character's upper body), parents before
/// their children.
fn upper_body(bones: &[dhcook::format::BoneDef]) -> Option<std::sync::Arc<Vec<bool>>> {
    let root = bones.iter().position(|b| b.name.eq_ignore_ascii_case("spine_0_jnt"))?;
    let mut on = vec![false; bones.len()];
    for (i, b) in bones.iter().enumerate() {
        on[i] = i == root || (b.parent >= 0 && on.get(b.parent as usize).copied().unwrap_or(false));
    }
    Some(std::sync::Arc::new(on))
}

fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod wear_tests {
    use super::{first_past, worn_attachments};

    #[test]
    fn variations_not_normalized() {
        // a lone 0.5 option applies to half the rolls; past every option, the slot's own
        assert_eq!(first_past(&[(7u32, 0.5)], 0.3), Some(7));
        assert_eq!(first_past(&[(7u32, 0.5)], 0.7), None);
        // the first whose running sum passes the roll
        assert_eq!(first_past(&[(1u32, 0.0), (2, 0.5), (3, 0.5)], 0.6), Some(3));
        assert_eq!(first_past(&[(1u32, 0.0), (2, 0.5), (3, 0.5)], 0.0), Some(2));
    }
    use dhcook::format::NpcAttachment;

    fn att(group: u8, chance: f32, weight: f32) -> NpcAttachment {
        NpcAttachment { socket: "Helmet".into(), prop: String::new(), health: 0.0, instant: vec![], immune: vec![], breaks: None, chance, group, weight, knock_off: vec![], absorb: vec![], absorb_head: false, absorb_side: 0, tweaks: vec![] }
    }

    #[test]
    fn attack_patterns_turn_moves_off_and_pace_them() {
        use dhcook::format::{MeleeMove, NpcStats, PatternOverride};
        let mv = |kind: &str, cooldown: [f32; 2]| MeleeMove { kind: kind.into(), cooldown, ..Default::default() };
        let ov = |kind: &str, enabled: bool, cooldown: [f32; 2]| PatternOverride { kind: kind.into(), enabled, cooldown };
        let st = NpcStats {
            melee: vec![mv("Short", [0.25, 0.5]), mv("StepBack", [0.25, 0.5]), mv("WhiskeyFire", [1.0, 2.0]), mv("ThrowWhiskey", [10.0, 10.0])],
            // (a thug's: `TwkAttackPattern_FireBreather`, its outer one not naming the throw)
            attack_patterns: vec![vec![ov("ThrowWhiskey", false, [0.0, 0.0]), ov("Short", true, [1.0, 2.0]), ov("StepBack", true, [0.0, 0.0])]],
            outer_patterns: vec![vec![ov("Short", true, [3.0, 5.0])]],
            ..Default::default()
        };
        let a = super::Arms::of(&st, 1, 7);
        let cd = |k: &str| a.moves.iter().find(|m| m.kind == k).map(|m| m.cooldown);
        // its own cooldown set by the pattern; a 0-0 one leaves the move's own
        assert_eq!(cd("Short"), Some([1.0, 2.0]));
        assert_eq!(cd("StepBack"), Some([0.25, 0.5]));
        // the throw off in the ring, on outside it
        assert_eq!(a.outer_only, vec!["ThrowWhiskey".to_string()]);
        assert_eq!(cd("ThrowWhiskey"), Some([10.0, 10.0]));
        // without an outer pattern, off altogether
        let st = NpcStats { outer_patterns: Vec::new(), ..st };
        let a = super::Arms::of(&st, 1, 7);
        assert!(a.outer_only.is_empty() && !a.moves.iter().any(|m| m.kind == "ThrowWhiskey"));
    }

    #[test]
    fn outer_patterns_place_by_their_ideal_move() {
        use dhcook::format::{MeleeMove, NpcStats, PatternShape};
        let mv = |kind: &str, ideal: f32| MeleeMove { kind: kind.into(), range: [0.0, ideal * 2.0, ideal], ..Default::default() };
        // (a thug's `TwkAttackPattern_Ranged`: its throw's ideal reach; a flanker's, its long lunge's)
        let st = NpcStats { melee: vec![mv("Short", 1.5), mv("Long", 5.25), mv("ThrowWhiskey", 10.0)], outer_shapes: vec![PatternShape { ideal: "Projectile".into(), flank: 60.0 }], ..Default::default() };
        assert_eq!(super::Arms::of(&st, 1, 3).outer_place, Some((10.0, 60.0)));
        let st = NpcStats { outer_shapes: vec![PatternShape { ideal: "Long".into(), flank: 45.0 }], ..st };
        assert_eq!(super::Arms::of(&st, 1, 3).outer_place, Some((5.25, 45.0)));
        // (none it can keep: none)
        let st = NpcStats { outer_shapes: vec![PatternShape { ideal: "Jump".into(), flank: 45.0 }], ..st };
        assert_eq!(super::Arms::of(&st, 1, 3).outer_place, None);
    }

    #[test]
    fn last_survivor_panics_as_the_kill_handler() {
        // (a city guard's: 2 left, 2 counted, 20 s, 25 m; twice at most)
        let p = dhcook::format::PanicTweak { rules: vec!["LastSurvivor".into()], survivor: [2.0, 2.0, 20.0, 25.0], count_limit: 2, cower: 6.0, free_flight: 2.0, caught: "Fight".into() };
        let mut q = Vec::new();
        // one fall: not yet
        assert!(!super::last_survivor(&p, &mut q, 0.0, 1, 5.0, true, 2, 0));
        // a second within the reset, two left fighting, Corvo near: it panics
        assert!(super::last_survivor(&p, &mut q.clone(), 10.0, 1, 5.0, true, 2, 0));
        // three still fighting him, Corvo too far, it not fighting, or its panics used up: not
        assert!(!super::last_survivor(&p, &mut q.clone(), 10.0, 1, 5.0, true, 3, 0));
        assert!(!super::last_survivor(&p, &mut q.clone(), 10.0, 1, 30.0, true, 2, 0));
        assert!(!super::last_survivor(&p, &mut q.clone(), 10.0, 1, 5.0, false, 2, 0));
        assert!(!super::last_survivor(&p, &mut q.clone(), 10.0, 1, 5.0, true, 2, 2));
        // the first fall forgotten past the reset
        assert!(!super::last_survivor(&p, &mut q, 25.0, 1, 5.0, true, 2, 0));
        assert_eq!(q, vec![25.0]);
        // two at once
        assert!(super::last_survivor(&p, &mut Vec::new(), 0.0, 2, 5.0, true, 1, 0));
    }

    #[test]
    fn worn_by_group_chance_and_weight() {
        // a tank always; a sure hat of two; a helmet never
        let atts = [att(0, 1.0, 1.0), att(1, 1.0, 1.0), att(1, 1.0, 3.0), att(10, 0.0, 1.0)];
        for seed in 0..50 {
            let w = worn_attachments(&atts, seed);
            assert!(w[0]);
            assert_eq!(w[1] as u8 + w[2] as u8, 1, "one hat of the group");
            assert!(!w[3]);
            // the same spawner, the same look
            assert_eq!(w, worn_attachments(&atts, seed));
        }
        // a half chance: some wear it, some don't
        let half = [att(10, 0.5, 1.0)];
        let n = (0..200).filter(|s| worn_attachments(&half, *s)[0]).count();
        assert!((60..140).contains(&n), "{n}");
    }
}

#[cfg(test)]
mod tests {

    /// The elliptical cones (0x00BEC910): straight ahead and across within the half angle, upward
    /// only within its small up scale, downward further, never behind.
    #[test]
    fn vision_cones_are_ellipses() {
        let s = dhcook::format::Sight { inner: [45.0, 35.0, 0.25, 0.7], outer: [160.0, 17.0, 0.1, 0.7], ..Default::default() };
        let fwd = Vec3::NEG_Z;
        let at = |yaw: f32, pitch: f32, d: f32| {
            let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
            let dir = Vec3::new(-yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos());
            vision_range(&s, fwd, dir, d, false)
        };
        assert_eq!(at(0.0, 0.0, 30.0), Some(35.0));
        assert_eq!(at(25.0, 0.0, 30.0), None, "25 degrees across at 30 m: past the inner cone, the outer one 17 m");
        assert_eq!(at(20.0, 0.0, 10.0), Some(35.0));
        // up: the inner cone's 5.6 degrees, the outer's 8
        assert_eq!(at(0.0, 5.0, 10.0), Some(35.0));
        assert_eq!(at(0.0, 7.0, 10.0), Some(17.0));
        assert_eq!(at(0.0, 9.0, 10.0), None);
        // down: the inner cone's 15.75 degrees
        assert_eq!(at(0.0, -15.0, 10.0), Some(35.0));
        // a diagonal the old box let through: 20 across and 5 up is past the ellipse
        assert_eq!(at(20.0, 5.0, 10.0).filter(|r| *r > 20.0), None);
        assert_eq!(at(120.0, 0.0, 5.0), None);
    }

    use super::*;

    #[test]
    fn a_body_is_seen_by_its_nearness_in_a_cone_and_its_light() {
        // (the tuning's points, and between them)
        assert_eq!(corpse_tuning(0.3), 0.0);
        assert!((corpse_tuning(0.875) - 0.8).abs() < 1e-6);
        assert!((corpse_tuning(0.7125) - 0.4).abs() < 1e-6);
        assert_eq!(corpse_tuning(1.0), 2.0);
        let s = dhcook::format::Sight { inner: [45.0, 35.0, 0.25, 0.7], outer: [160.0, 17.0, 0.1, 0.7], ..Default::default() };
        let eye = Vec3::new(0.0, 0.7, 0.0);
        let ahead = |d: f32| Vec3::new(0.0, 0.0, -d);
        // in the dark: ahead within about 10.8 m, not beyond; lit, as far as it sees
        assert!(body_seen(&s, eye, Vec3::NEG_Z, ahead(10.0), 0.0, SEE_CORPSE));
        assert!(!body_seen(&s, eye, Vec3::NEG_Z, ahead(12.0), 0.0, SEE_CORPSE));
        assert!(body_seen(&s, eye, Vec3::NEG_Z, ahead(30.0), 0.4, SEE_CORPSE));
        // behind: never
        assert!(!body_seen(&s, eye, Vec3::NEG_Z, Vec3::new(0.0, 0.0, 3.0), 1.0, SEE_CORPSE));
    }

    /// A turn speeds up at its acceleration to its rate, slows to arrive and never passes it.
    #[test]
    fn turns_speed_up_and_arrive() {
        let (rate, accel, dt) = (90f32.to_radians(), 180f32.to_radians(), 1.0 / 60.0);
        let (mut left, mut v, mut t) = (FRAC_PI_2, 0.0, 0.0);
        let mut top: f32 = 0.0;
        while left > 1e-6 && t < 5.0 {
            let step = turn_toward(left, &mut v, rate, accel, dt);
            assert!(step <= left + 1e-6);
            left -= step;
            top = top.max(v);
            t += dt;
        }
        // a quarter turn at 90 degrees a second, half a second to full speed and to stop: 1.5 s
        assert!((t - 1.5).abs() < 0.1, "{t}");
        assert!((top - rate).abs() < 1e-4 && v == 0.0);
        // turning back flips at once
        let mut v = 1.0;
        turn_toward(-1.0, &mut v, rate, accel, dt);
        assert!(v < 0.0);
    }

    /// A guard's config: its patrol's walk turns at 90 degrees a second, its run at 550, and a
    /// default character searching empty-handed at its `SearchWalk`'s.
    #[test]
    fn turn_rates_by_stance() {
        let l = dhcook::format::NpcLoco {
            modes: [("EmptyWalk", 90.0), ("EmptyRun", 550.0), ("EmptyWalkPatrol", 90.0), ("SwordWalk", 360.0), ("SwordRun", 550.0), ("SearchWalk", 45.0)].map(|(n, r)| (n.to_string(), r)).to_vec(),
            accel_mult: 2.0,
            ..Default::default()
        };
        let t = LocoTurn::of(&l);
        let deg = |r: Option<f32>| r.map(f32::to_degrees).map(|d| d.round());
        assert_eq!(deg(t.rate(LOCO_PATROL, Mode::Patrol, false, false)), Some(90.0));
        assert_eq!(deg(t.rate(LOCO_SWORD, Mode::Combat, true, false)), Some(550.0));
        assert_eq!(deg(t.rate(LOCO_EMPTY, Mode::Search, false, false)), Some(45.0));
        // a weeper without its own walks as the empty-handed; no config, no rate (the old turn)
        assert_eq!(deg(t.rate(LOCO_EMPTY, Mode::Patrol, false, true)), Some(90.0));
        assert_eq!(LocoTurn::of(&Default::default()).rate(LOCO_SWORD, Mode::Combat, true, false), None);
    }
}
