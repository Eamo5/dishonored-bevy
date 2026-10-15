//! NPCs: spawned from the level's DishonoredSpawners using the original skinned models,
//! animated with the pawn's original animation sets (procedural fallback for characters
//! without them), with perception and a stealth/combat state machine.

use crate::anim::{Animator, CharAnims, ClipId};
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
            .add_systems(OnEnter(GameState::InGame), spawn_npcs.after(LevelSpawnSet))
            .add_systems(First, log_jumps.run_if(in_state(GameState::InGame)))
            .add_systems(Update, sticky_struck.before(npc_select_anim).run_if(in_state(GameState::InGame)))
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
                    age_corpses,
                    ash_marked,
                    burn_ashes,
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
    /// a riposte due in (s); how long Corvo has crowded it
    pub act: (u8, u32),
    pub riposte: f32,
    pub crowded: f32,
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
    pub route: Vec<Vec3>,
    pub route_idx: usize,
    pub route_dir: i32,
    pub route_pingpong: bool,
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
    pub leap_cd: [f32; 2],
    pub trail_idx: usize,
    pub saw_body: bool,
    /// Pre-placed corpse: lies dead from the start and doesn't alarm guards.
    pub corpse: bool,
    /// Ground speeds: walk, alert walk, run (from the animations' root motion).
    pub gait: [f32; 3],
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
    /// out of a plague carrier's cloud it's in (`DisBehaviorEscapePlague`'s `GetOuttaHere`):
    /// where it makes for, and how long yet
    pub plague_escape: Option<(Vec3, f32)>,
    /// robbed while it stood: how long until it misses what was taken (`DisBehaviorStolen`)
    pub robbed: Option<f32>,
    /// caught panicking with Corvo upon it: how long it has begged, and how long he has been
    /// back off; then the seconds left getting up again
    pub caught: Option<(f32, f32)>,
    pub rising: f32,
    /// a watch point it checks while searching: the point and seconds left watching it; those
    /// checked this search
    pub watch: Option<(usize, f32)>,
    pub watched: Vec<usize>,
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
        Arms { melee: 15.0, ranged: 20.0, accuracy: [0.35, 0.1], parry: [0.5, 0.25, 3.0], block_break: [0.3, 0.6], regen: [5.0, 5.0, 15.0], big_hit: 10.0, dodge: 0.0, grenades: Vec::new(), moves: Vec::new(), tunes: None, fire: None, taunt: None, plague: None, whiskey: None, throwing: None, outer_only: Vec::new(), outer_place: None }
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
fn fire_context(npc: &Npc) -> dhcook::format::FireContext {
    npc.arms.fire.clone().unwrap_or(dhcook::format::FireContext {
        range: [2.5, 30.0, 6.5],
        angle: 25.0,
        cooldown: if npc.ranged == 3 { [3.0, 6.0] } else { [10.0, 20.0] },
        cooldown_unreachable: if npc.ranged == 3 { [2.0, 5.0] } else { [1.0, 2.0] },
    })
}

/// A shot's chance to hit `d` m away: its accuracy, `m_MaxAccuracy` close, falling to
/// `m_MinAccuracy` at the end of its reach (less `miss`, the difficulty's for guns).
fn shot_chance(npc: &Npc, ctx: &dhcook::format::FireContext, d: f32, miss: f32) -> f32 {
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

/// What a character's weapons are: a blade, a firearm or bow (and which: 1 pistol, 2 a
/// tallboy's bow, 3 an assassin's wrist bow).
pub fn weapon_kinds(st: &dhcook::format::NpcStats) -> (bool, u8) {
    let has = |c: &str| st.weapons.iter().any(|w| w.class == c);
    let blade = has("DisTweaks_WepSword") || has("DisTweaks_WepSwordOfAssassin");
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
        let blade = ["DisTweaks_WepSword", "DisTweaks_WepSwordOfAssassin", "DisTweaks_WepTallboyBow", "DisTweaks_WepWhiskeyBottle"].iter().find_map(|c| st.weapons.iter().find(|w| w.class == *c));
        if let Some(w) = blade {
            a.melee = w.melee[d];
        }
        let gun = ["DisTweaks_WepPistol", "DisTweaks_WepTallboyBow", "DisTweaks_WepSwordOfAssassin"].iter().find_map(|c| st.weapons.iter().find(|w| w.class == *c));
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

/// A fight's pacing (the elite guard's `DisAISubProcessManageAttacks`, as roshan's r614/blink
/// research decoded it): flurries of 2-3 blows (`m_FlurryAttacks`), then 4-5 s to recover
/// (`m_StaminaRecovery`); aggression rolled in [0, 1) as the fight starts, regained at
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
    if npc.aggression < 0.0 {
        npc.aggression = rand::random::<f32>();
        npc.flurry = rand::random_range(FLURRY[0]..=FLURRY[1]);
        npc.stamina = 0.0;
    }
    npc.aggression = (npc.aggression + AGGRESSION_REGEN * dt).min(1.0);
    if npc.flurry < 1 {
        npc.stamina -= dt;
        if npc.stamina < 0.0 {
            npc.flurry = rand::random_range(FLURRY[0]..=FLURRY[1]);
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
    (mut stats, mut msgs, settings): (ResMut<crate::gameplay::PlayerStats>, ResMut<crate::gameplay::HudMessages>, Res<crate::settings::Settings>),
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
                        crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, dmg);
                        stats.knockdown = (stats.knockdown.0 + 1, crate::combat::side_of(t.translation, pt.translation, p.yaw));
                        stats.knocked = KNOCKED_DOWN;
                    }
                    info!("wolfhound jump attack: {} {} ({d:.2} m)", npc.name, if hit { "knocks Corvo down" } else { "misses" });
                    npc.grab = Some((if hit && stats.dead { GrabPhase::Kill } else if hit { GrabPhase::Release } else { GrabPhase::Miss }, 0.0));
                } else if d < if hound { HOUND_HOLD } else { GRAB_DODGE } && grabbed.is_none() && !npc.is_down() {
                    info!("weeper grab: {} holds Corvo", npc.name);
                    npc.grab = Some((GrabPhase::Hold, 0.0));
                    commands.entity(pe).insert(Grabbed { by: e, presses: 0, hound });
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
                    crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, HOUND_BITE);
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
            }
            _ => {}
        }
    }
    // the weeper went away (despawned)
    if let Some(g) = grabbed {
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
#[derive(Component, Default)]
pub struct HeadTrack {
    yaw: f32,
    pitch: f32,
    weight: f32,
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
    npcs: Query<(Entity, &Transform, &Npc, &NpcRig, Option<&mut HeadTrack>, Has<crate::audio::Speaking>, Option<&crate::anim::Animator>), (Without<SceneLook>, Without<crate::player::Player>)>,
    mut joints: Query<&mut Transform, (Without<Npc>, Without<crate::player::Player>)>,
) {
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    let eye = pt.translation + Vec3::Y * 0.6;
    let mut npcs = npcs;
    for (e, t, npc, rig, track, speaking, anim) in &mut npcs {
        let Some(mut track) = track else {
            commands.entity(e).try_insert(HeadTrack::default());
            continue;
        };
        // (a rig not posed this frame keeps last frame's pose: nothing laid on it)
        if anim.is_none_or(|a| a.frozen) {
            continue;
        }
        let head = t.translation + Vec3::Y * 0.75;
        let to = eye - head;
        let flat = to.with_y(0.0).length();
        let yaw = ((-to.x).atan2(-to.z) - npc.yaw + PI).rem_euclid(TAU) - PI;
        let near = flat < 6.0;
        let wants = !npc.is_down()
            && npc.mode != Mode::Combat
            && flat > 0.3
            && yaw.abs() < LOOK_YAW
            && ((npc.attn_level == 1 && (npc.sees_player || npc.last_seen.is_some())) || (speaking && near));
        let rate = 4.0 * dt;
        track.weight = if wants { (track.weight + rate).min(1.0) } else { (track.weight - rate).max(0.0) };
        if wants {
            let pitch = to.y.atan2(flat).clamp(-LOOK_DOWN, LOOK_UP);
            let k = (8.0 * dt).min(1.0);
            track.yaw += (yaw - track.yaw) * k;
            track.pitch += (pitch - track.pitch) * k;
        }
        if track.weight <= 0.0 {
            continue;
        }
        if std::env::var("DH_LOOK_LOG").is_ok() {
            info!("look: {} weight {:.2} yaw {:.0} pitch {:.0} (attention {}, speaking {speaking})", npc.name, track.weight, track.yaw.to_degrees(), track.pitch.to_degrees(), npc.attn_level);
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
        if face {
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
}

/// How far a missed bullet flies on (m).
const MISS_REACH: f32 = 80.0;

/// Gun and bow shots: the sound, and a gun's flash and its bullet's tracer (its pistol's
/// `Twk_Proj_Bullet`, Corvo's own: `m_pBulletTracerFX`). A missed bullet flies on past its mark,
/// to one side, and strikes the world as Corvo's do (its effect, its hole).
#[allow(clippy::too_many_arguments)]
fn npc_shots(
    mut shots: MessageReader<NpcShot>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut gizmos: Gizmos,
    mut recent: Local<Vec<(NpcShot, f32)>>,
    time: Res<Time>,
    (level, rapier, surfaces, mut decals): (Option<Res<LevelInfo>>, ReadRapierContext, Query<&crate::footsteps::ColliderSurfaces>, MessageWriter<crate::decals::SpawnDecal>),
) {
    for s in shots.read() {
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
        if !sp.spawn_on_begin_play && !all || crate::campaign::spawner_unloaded(scene, &loaded, si as u32) {
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
        if done.contains(&r.0) {
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
    let route: Vec<Vec3> = sp
        .route
        .and_then(|r| scene.routes.get(r as usize))
        .map(|r| r.points.iter().map(|p| Vec3::from(*p)).collect())
        .unwrap_or_default();
    let pingpong = sp
        .route
        .and_then(|r| scene.routes.get(r as usize))
        .map(|r| r.kind.contains("PingPong"))
        .unwrap_or(true);
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
    let (has_sword, ranged) = match stats.filter(|s| !s.weapons.is_empty()).map(weapon_kinds) {
        Some((blade, gun)) => (blade, if enemy { gun } else { 0 }),
        None => (has_sword, ranged),
    };
    let arms = stats.map(|s| Arms::of(s, difficulty, si as u64)).unwrap_or_default();
    let corpse = crate::level::is_corpse_type(&vis.name);
    let mode = if corpse {
        Mode::Dead
    } else if kind == Kind::Story {
        Mode::Idle
    } else if route.len() >= 2 {
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
        crowded: 0.0,
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
        route,
        route_idx: 0,
        route_dir: 1,
        route_pingpong: pingpong,
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
        wish: Vec3::ZERO,
        path: None,
        halted: false,
        floored: false,
        rise: None,
        leg_hit: None,
        small_hit: false,
        shot_hit: false,
        dusted: false,
        flank: None,
        taunt_hold: 0.0,
        search_run: false,
        witnessed_death: false,
        cough_cd: rand::random::<f32>() * 3.0,
        cough_hold: 0.0,
        hit_from: None,
        foe: None,
        feud_scan: rand::random::<f32>(),
        grab: None,
        grab_len: 0.0,
        grab_drift: 0.0,
        grab_jump: false,
        leap_cd: [0.0; 2],
        trail_idx: 0,
        saw_body: false,
        corpse,
        gait: [WALK, ALERT_WALK, RUN],
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
        plague_escape: None,
        robbed: None,
        rising: 0.0,
        watch: None,
        watched: Vec::new(),
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
        let e = commands.spawn(Transform::from_translation(Vec3::from(b.translation)).with_rotation(q)).id();
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
    let hand_r = socket("RightHandWpn", &["handAttachment_R_jnt", "hand_R_jnt"], Transform::IDENTITY);
    let holster = socket("HolsterWpn", &["Holster_jnt"], HOLSTERED);
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
        blade = crate::trails::blade_of(scene, prop);
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
    let clips = vis.anims.as_ref().and_then(|lib| NpcClips::resolve(lib).map(|c| (lib.clone(), c)));
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
        npc.gait = [
            speed(clips.walk, 0.8, 2.0, WALK),
            speed(clips.walk_alert.or(clips.walk), 0.8, 2.6, ALERT_WALK),
            speed(clips.run, 2.5, 6.0, RUN),
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
                way: 0,
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
                rise_next: None,
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
    mut msgs: ResMut<crate::gameplay::HudMessages>,
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
                stats.killed(&npc.pawn, !npc.hostile());
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
        // those who can fight turn on Corvo when he hurts them (the Tower's guards too)
        if !npc.enemy && npc.fighter() && !matches!(h.kind, HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::Rats | HitKind::Assassinate | HitKind::Fatality | HitKind::Choke | HitKind::SleepDart | HitKind::WallOfLight | HitKind::StunMine) {
            npc.enemy = true;
            stances.wronged(npc.spawner);
        }
        let pos = t.translation;
        let unaware = npc.alert != Alert::Combat;
        // kept alive by the scripts: no killing blow takes it
        if npc.min_health > 0.0 && matches!(h.kind, HitKind::Assassinate | HitKind::Fatality | HitKind::Fire | HitKind::WallOfLight) {
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
                msgs.push(format!("{} rendered unconscious", npc.name));
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
                msgs.push("Sleep dart hit");
                noise.write(Noise { pos, radius: 3.0, kind: NoiseKind::Anomaly });
            }
            // a stun mine's lightning: asleep, falling limp where they stood
            HitKind::StunMine => {
                npc.set_mode(Mode::Unconscious);
                stats.knockouts += 1;
                commands.entity(e).try_insert(crate::ragdoll::GoLimp::now((pos - h.from).with_y(0.0).normalize_or_zero() * 0.5, None));
                noise.write(Noise { pos, radius: 3.0, kind: NoiseKind::Anomaly });
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
                // `NPCImmolationTime`: they burn, then nothing is left
                npc.health = 0.0;
                npc.set_mode(Mode::Dead);
                stats.killed(&npc.pawn, !npc.hostile());
                commands.entity(e).try_insert(Ashes(-0.9));
                sfx.write(crate::audio::PostEvent::named("Amb_Fire_Alcohol_Immolation", Some(pos + Vec3::Y)));
                fx.write(crate::particles::SpawnEffect { follow: Some(e), secs: 1.6, ..crate::particles::SpawnEffect::at("immolation", Vec3::ZERO) });
                noise.write(Noise { pos, radius: 14.0, kind: NoiseKind::Danger });
            }
            HitKind::Assassinate | HitKind::Fatality | HitKind::WallOfLight => {
                npc.health = 0.0;
                npc.set_mode(Mode::Dead);
                stats.killed(&npc.pawn, !npc.hostile());
                // (a wall of light burns its dead to ash, `DisWallOfLight`'s kill)
                if h.kind == HitKind::WallOfLight {
                    commands.entity(e).try_insert(Ashes(-0.2));
                }
                if h.kind == HitKind::Assassinate {
                    msgs.push(format!("{} assassinated", npc.name));
                }
                noise.write(Noise { pos, radius: 3.0, kind: NoiseKind::DeathRattle });
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
                }
                noise.write(Noise { pos, radius: 8.0, kind: NoiseKind::Threatening });
            }
            _ => {
                npc.health = (npc.health - h.damage).max(npc.min_health);
                npc.hurt_t = 0.0;
                npc.hit_react = 0.35;
                npc.attack_t = None;
                // (slid into: its strong reaction, stumbling through it)
                npc.stagger = npc.stagger.max(if h.kind == HitKind::Slide { 0.9 } else { 0.25 });
                if npc.aggression >= 0.0 && h.damage > 0.0 {
                    npc.aggression = (npc.aggression + AGGRESSION_PER_DAMAGE).clamp(0.0, 1.0);
                }
                if npc.mode != Mode::Combat && npc.hostile() {
                    npc.alert = Alert::Combat;
                    npc.awareness = 1.0;
                    npc.last_seen = Some(h.from);
                    npc.set_mode(Mode::Combat);
                }
                // a civilian struck panics (`TweaksBehavior_Panic_Civilian`: attacked)
                if npc.health > 0.0 && civilian_may_panic(&npc) {
                    npc.last_seen = Some(h.from);
                    npc.set_mode(Mode::Flee);
                }
                if npc.health <= 0.0 {
                    npc.set_mode(Mode::Dead);
                    stats.killed(&npc.pawn, !npc.hostile());
                    noise.write(Noise { pos, radius: 8.0, kind: NoiseKind::Threatening });
                    noise.write(Noise { pos, radius: 3.0, kind: NoiseKind::DeathRattle });
                } else {
                    noise.write(Noise { pos, radius: 8.0, kind: NoiseKind::Threatening });
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
        if (stats.kills > kills0 || ko) && !matches!(h.kind, HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::Rats) {
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
        if npc.mode == Mode::Dead && shadow > 0 && (unaware || shadow >= 2) && !matches!(h.kind, HitKind::Rats | HitKind::Fire | HitKind::ByOthers | HitKind::EnemyExplosion) {
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
    mut npcs: Query<(&mut Npc, &Transform)>,
    level: Option<Res<LevelInfo>>,
    player: Query<&Transform, (With<Player>, Without<Npc>)>,
    rooms: Res<crate::audiorooms::AudioRooms>,
) {
    let corvo = player.single().ok().map(|t| t.translation);
    for n in noises.read() {
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
            if his {
                npc.heard_player += 1;
            }
            if !npc.hostile() {
                // a scary noise (to them) sets them running; the armed stand their ground
                let scary = level.as_ref().and_then(|l| npc.npc_type.and_then(|t| l.scene.npc_types.get(t as usize))).and_then(|t| t.sight.as_ref()).and_then(|s| s.scary.clone());
                if n.panics(scary.as_deref()) && !npc.fighter() && npc.may_panic() {
                    if npc.can_flee {
                        npc.set_mode(Mode::Flee);
                    }
                    npc.last_seen = Some(n.pos);
                }
                continue;
            }
            // what it hears raises its attention (the original's increase for the noise's
            // context: an anomaly, a threat, danger, a death rattle, a cry for help, an alarm),
            // at least to a level but never to busted: hearing alone can't find Corvo;
            // investigating, it goes to look
            if let Some(att) = level.as_ref().and_then(|l| attention_of(&npc, &l.scene)) {
                if npc.alert == Alert::Combat {
                    continue;
                }
                let (amount, lvl) = att.heard[n.kind.slot()];
                let cap = (att.levels[3][0] - 0.001).min(att.max);
                let floor = match lvl {
                    0 => 0.0,
                    1..=4 => att.levels[lvl as usize - 1][0],
                    _ => att.max,
                }
                .min(cap);
                npc.awareness = npc.awareness.max((npc.awareness + amount).max(floor).min(cap));
                npc.last_seen = Some(n.pos);
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
pub fn attention_of<'a>(npc: &Npc, scene: &'a dhcook::format::Scene) -> Option<&'a dhcook::format::Attention> {
    let s = npc.npc_type.and_then(|t| scene.npc_types.get(t as usize)).and_then(|t| t.sight.as_ref())?;
    Some(if !npc.hostile() {
        &s.neutral
    } else if npc.alert != Alert::Unaware {
        &s.suspecting
    } else {
        &s.unsuspecting
    })
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
    let within = |c: &[f32; 4]| {
        let half = (c[0] * 0.5).to_radians();
        d <= c[1] && h <= half && v <= half * c[2] && -v <= half * c[3]
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
    (froms, mut events): (Query<&FromSpawner>, MessageWriter<crate::interact::Interaction>),
    (level, pvis, settings, notarget): (Option<Res<LevelInfo>>, Res<crate::stealth::PlayerVisibility>, Res<crate::settings::Settings>, Option<Res<crate::script::NoTarget>>),
) {
    let (dt_world, dt_own) = (time.delta_secs() * tc.world_scale(), time.delta_secs() * tc.own_scale());
    // inside a host, Corvo is the host
    // (`DH_NOTARGET`, `notarget`: unseen, for watching the characters in tests)
    let hidden = possession.host.is_some() || std::env::var("DH_NOTARGET").is_ok() || notarget.is_some();
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
    let head = pt.translation + Vec3::Y * p.eye_height;
    let chest = pt.translation;
    // (characters without the original's tweaks: darker = harder to see, 0.35 .. 1.0)
    let light = plit.map(|l| l.brightness).unwrap_or(0.4);
    let light_factor = (0.35 + light * 1.6).clamp(0.35, 1.0);
    let stance = if p.crouched { 0.55 } else if p.sprinting { 1.35 } else { 1.0 };
    let difficulty = settings.difficulty.min(3) as usize;
    let log = std::env::var("DH_ATTN_LOG").is_ok();
    let body_positions: Vec<(Entity, Vec3)> = npcs.iter().filter(|(_, n, _)| n.is_down() && !n.corpse).map(|(e, _, t)| (e, t.translation)).collect();
    // (deaths just now: the bodies of the last two seconds)
    let fresh: Vec<(Entity, Vec3)> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Dead && !n.corpse && n.corpse_age < 2.0).map(|(e, _, t)| (e, t.translation)).collect();
    // (for the civilians: the guards fighting, the others panicking)
    let fighting: Vec<Vec3> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Combat && n.hostile() && !n.is_down()).map(|(_, _, t)| t.translation).collect();
    let panicking: Vec<Vec3> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Flee && !n.fighter() && !n.is_down()).map(|(_, _, t)| t.translation).collect();
    let mut newly_detected = false;
    for (e, mut npc, t) in &mut npcs {
        if npc.is_down() || npc.kind == Kind::Story || npc.mode == Mode::Choked || stats.dead || hidden || Some(e) == possession.host || npc.blind {
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
        let fwd = npc.forward();
        let sight = npc.npc_type.and_then(|i| level.as_ref()?.scene.npc_types.get(i as usize)?.sight.as_ref());
        let mut visible = false;
        let mut dist = f32::MAX;
        let mut range = 30.0 * light_factor;
        for target in [head, chest] {
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
            if ctx.cast_ray(eye, dir, d - 0.1, true, filter).is_none() {
                visible = true;
                dist = d;
                range = r;
                break;
            }
        }
        // (an enemy he is in sight of keeps him there without a clear line while he stays within
        // its attention's `m_fDistanceToLoseSight`, 3 m: round a corner, behind a crate, as
        // roshan's r614/blink research decoded it)
        if !visible && npc.sees_player && npc.hostile() {
            let keep = level.as_ref().and_then(|l| attention_of(&npc, &l.scene)).map_or(0.0, |a| a.lose_sight);
            let off = pt.translation - t.translation;
            if off.with_y(0.0).length() <= keep && off.y.abs() <= keep {
                visible = true;
                dist = off.length();
            }
        }
        npc.sees_player = visible;
        if visible {
            npc.last_seen = Some(pt.translation);
        }
        let touching = (pt.translation - t.translation).with_y(0.0).length() < 0.9 && (pt.translation.y - t.translation.y).abs() < 1.5;
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
            // a death before its eyes (fighting too)
            if !npc.witnessed_death {
                for (_, b) in &fresh {
                    let d = b.distance(eye);
                    if d < 14.0 && (*b - eye).normalize_or_zero().dot(fwd) > 0.3 {
                        let dir = (*b + Vec3::Y * 0.2 - eye).normalize_or_zero();
                        let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                        if ctx.cast_ray(eye, dir, d - 0.3, true, filter).is_none() {
                            npc.witnessed_death = true;
                            break;
                        }
                    }
                }
            }
            // discover bodies
            if !npc.saw_body && npc.alert != Alert::Combat {
                for (body, b) in &body_positions {
                    let d = b.distance(eye);
                    if d < 14.0 && (*b - eye).normalize_or_zero().dot(fwd) > 0.5 {
                        let dir = (*b + Vec3::Y * 0.2 - eye).normalize_or_zero();
                        let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                        if ctx.cast_ray(eye, dir, d - 0.3, true, filter).is_none() {
                            npc.saw_body = true;
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
                            break;
                        }
                    }
                }
            }
        } else if civilian_may_panic(&npc) {
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
            let why = body_positions
                .iter()
                .map(|(_, b)| *b)
                .find(|b| in_sight(*b))
                .or_else(|| fighting.iter().copied().find(|f| f.distance(pos) < GROUP_DISTANCE && in_sight(*f + Vec3::Y * 0.5)))
                .or_else(|| panicking.iter().copied().find(|f| f.distance(pos) < GROUP_DISTANCE && f.distance(pos) > 0.05));
            if let Some(from) = why {
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
    (stances, settings, mut grab_inhibit, mut inhibited): (Res<crate::script_world::SpawnerOverrides>, Res<crate::settings::Settings>, Local<f32>, Local<std::collections::HashMap<String, (f32, Entity)>>),
    (mut grenades, mut bottles, mut whiskey_fx, mut throws): (MessageWriter<crate::gadgets::NpcGrenade>, MessageWriter<crate::whiskey::NpcBottle>, MessageWriter<crate::particles::SpawnEffect>, MessageWriter<crate::thrown::NpcThrow>),
    (mut powers_used, finishing, mut finishers, mut tallboy_shots): (MessageReader<crate::powers::PowerUsed>, Query<(), With<FinisherClip>>, MessageWriter<NpcFinisher>, MessageWriter<crate::tallboy::TallboyShot>),
) {
    let (dt_world, dt_own) = (time.delta_secs() * tc.world_scale(), time.delta_secs() * tc.own_scale());
    let max_attackers = INNER_SKIRMISH_CAPS[(settings.difficulty as usize).min(3)];
    if dt_world <= 0.0 && (dt_own <= 0.0 || !npcs.iter().any(|(_, n, _)| n.out_of_bend)) {
        return;
    }
    let Ok((pe, pt, psword, pl)) = player.single() else { return };
    let ppos = pt.translation;
    // (his feet: his capsule's centre above them, standing or crouched)
    let pfeet = ppos.y - if pl.crouched { crate::player::CROUCH_HALF } else { crate::player::STAND_HALF } - crate::player::RADIUS;
    let blocking = psword.is_some_and(|s| s.blocking);
    let Ok(ctx) = rapier.single() else { return };
    // the others, for feuds between factions
    let others: Vec<(Entity, Vec3, String, bool, u32, f32)> = npcs.iter().map(|(e, n, t)| (e, t.translation, n.faction.clone(), n.is_down() || n.kind == Kind::Story, n.spawner, n.health)).collect();
    let factions = level.as_ref().map(|l| &l.scene.factions);
    let wall_filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    // NPCs that just entered combat alert their neighbours
    let mut alerts: Vec<(Vec3, Vec3)> = Vec::new();
    // attack tokens: swings already in progress count against the limit
    let mut attackers = npcs.iter().filter(|(_, n, _)| n.attack_t.is_some()).count();
    // Corvo gone before their eyes by Blink: those who saw him, not yet fighting him, are taken
    // aback (the original's `DisBehaviorMagicResponse`, its `Magic_OutOfSight` clip; their line
    // is the barks'; in a fight, our reading, they keep at him)
    if powers_used.read().any(|u| u.0 == crate::powers::Power::Blink) {
        for (_, mut npc, _) in &mut npcs {
            if npc.sees_player && npc.hostile() && npc.alert != Alert::Combat && !npc.is_down() && npc.attack_t.is_none() && npc.aim.is_none() && npc.grab.is_none() {
                npc.gesture(20);
            }
        }
    }
    // the ring at Corvo: of those fighting him, as many as the cap, the nearest; the others wait
    // outside it
    let ring: Vec<Entity> = {
        let mut fighting: Vec<(Entity, f32)> = npcs.iter().filter(|(_, n, _)| n.mode == Mode::Combat && n.hostile() && !n.is_down()).map(|(e, _, t)| (e, t.translation.distance_squared(ppos))).collect();
        fighting.sort_by(|a, b| a.1.total_cmp(&b.1));
        fighting.into_iter().take(max_attackers).map(|(e, _)| e).collect()
    };
    // one weeper at a time grabs Corvo
    let grab_active = npcs.iter().any(|(_, n, _)| n.grab.is_some());
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
                        noise.write(Noise { pos, radius: 8.0, kind: NoiseKind::Threatening });
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
        // Corvo in its way, not an enemy: a shove (`DisTweaks_NPCPush`, within 2 m, every 1 s at
        // most) with its "personal space" bark
        if !npc.enemy && npc.alert != Alert::Combat && npc.has_move("Push") && npc.attack_t.is_none() {
            let to = (ppos - pos).with_y(0.0);
            let close = to.length() < 1.1 && (ppos.y - pos.y).abs() < 1.2 && to.normalize_or_zero().dot(npc.forward()) > 0.3;
            npc.crowded = if close { npc.crowded + dt } else { 0.0 };
            if npc.crowded > 0.8 {
                npc.crowded = -0.2;
                npc.gesture(3);
                npc.bark_req = (npc.bark_req.0.wrapping_add(1), "PATROL_PERSONAL_SPACE");
                hit_writer.write(PlayerHit { npc: e, from: pos, damage: 0.0, kick: false, big: false, push: true, unblockable: false, missile: false });
            }
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
                if npc.route.is_empty() {
                    npc.set_mode(Mode::Idle);
                    continue;
                }
                let goal = npc.route[npc.route_idx];
                let away = Vec2::new(pos.x - goal.x, pos.z - goal.z).length();
                if away < 0.6 || (npc.halted && away < UNACCURACY) {
                    npc.target = None;
                    npc.wait -= dt;
                    if npc.wait <= 0.0 {
                        npc.wait = 1.0 + rand::random::<f32>() * 2.5;
                        let n = npc.route.len() as i32;
                        let mut next = npc.route_idx as i32 + npc.route_dir;
                        if next >= n || next < 0 {
                            if npc.route_pingpong {
                                npc.route_dir = -npc.route_dir;
                                next = npc.route_idx as i32 + npc.route_dir;
                            } else {
                                next = next.rem_euclid(n);
                            }
                        }
                        npc.route_idx = next.clamp(0, n - 1) as usize;
                        // (on to the next: not there till it gets there)
                        npc.halted = false;
                    }
                } else {
                    npc.target = Some(goal);
                }
            }
            Mode::Investigate => {
                let goal = npc.target.or(npc.last_seen).unwrap_or(npc.home);
                npc.target = Some(goal);
                if pos.distance(goal) < 1.6 || npc.mode_timer > 14.0 {
                    // (come to a body it found: it looks it over, `Sword_CorpseCheck`)
                    if std::mem::take(&mut npc.body_check) && pos.distance(goal) < 1.6 {
                        npc.gesture(19);
                    }
                    npc.set_mode(Mode::Search);
                }
            }
            Mode::Search => {
                // the search lasts while its attention holds above the Investigate stop (out
                // of sight it leaks: about 12 s unsuspecting, half a minute and more suspecting,
                // as roshan's r614/blink research measured), its crumbs' worth at most
                let given_up = match level.as_ref().and_then(|l| attention_of(&npc, &l.scene)) {
                    Some(a) => npc.mode_timer > 3.0 && (npc.awareness < a.levels[2][1] || npc.mode_timer > 40.0 + npc.watched.len() as f32 * 4.0),
                    None => npc.mode_timer > 10.0 + (npc.alert == Alert::Combat) as u8 as f32 * 6.0 + npc.watched.len() as f32 * 4.0,
                };
                if given_up {
                    npc.alert = Alert::Unaware;
                    npc.awareness = npc.awareness.min(0.2);
                    npc.saw_body = false;
                    npc.watch = None;
                    npc.watched.clear();
                    npc.set_mode(Mode::Return);
                } else if let Some((w, left)) = npc.watch {
                    // at a watch point: watching for its time, turning about
                    let at = ai.watch_point(w).map(|p| Vec3::from(p.position)).unwrap_or(pos);
                    if pos.distance(at) < 1.2 {
                        npc.target = None;
                        npc.yaw += dt * 0.8 * (left * 1.7).sin().signum();
                        npc.watch = if left - dt <= 0.0 { None } else { Some((w, left - dt)) };
                    } else if npc.stuck > 1.5 {
                        npc.watch = None;
                    } else {
                        npc.target = Some(at);
                    }
                } else if npc.target.map(|g| pos.distance(g) < 1.0).unwrap_or(true) || npc.stuck > 1.0 {
                    let center = npc.last_seen.unwrap_or(pos);
                    // last seen in a hideout: looked into from its access point; else the watch
                    // points about where he was lost; else about there
                    if let Some((at, _)) = ai.hideout_access(center).filter(|(at, _)| pos.distance(*at) > 1.2 && !npc.watched.contains(&usize::MAX)) {
                        npc.watched.push(usize::MAX);
                        npc.target = Some(at);
                    } else if let Some(w) = ai.watch_near(center, 14.0, &npc.watched) {
                        let p = ai.watch_point(w).cloned().unwrap_or_default();
                        npc.watched.push(w);
                        npc.watch = Some((w, p.time[0] + rand::random::<f32>() * (p.time[1] - p.time[0]).max(0.0)));
                        npc.target = Some(Vec3::from(p.position));
                    } else {
                        let a = rand::random::<f32>() * TAU;
                        let r = 2.0 + rand::random::<f32>() * 4.0;
                        npc.target = Some(center + Vec3::new(a.cos() * r, 0.0, a.sin() * r));
                    }
                    npc.stuck = 0.0;
                }
            }
            Mode::Return => {
                let goal = if npc.route.len() >= 2 { npc.route[npc.route_idx] } else { npc.home };
                npc.target = Some(goal);
                if pos.distance(goal) < 1.0 {
                    let m = if npc.route.len() >= 2 { Mode::Patrol } else { Mode::Idle };
                    npc.set_mode(m);
                }
            }
            Mode::Flee => {
                let from = npc.last_seen.unwrap_or(ppos);
                // caught: Corvo upon it (within the panic's `m_fCowerReactionDistance`, 350 uu),
                // a civilian gives up running and begs where it is, facing him
                // (`m_eCaughtReaction`: `EReaction_Beg`), its cry for help heard 15 m; once he
                // has backed off past the reach a moment, it gets up and runs on
                if !npc.fighter() {
                    let near = (ppos - pos).length() < BEG_REACH;
                    match npc.caught {
                        None if npc.rising <= 0.0 && near && (npc.sees_player || (ppos - pos).length() < 2.0) => {
                            npc.caught = Some((0.0, 0.0));
                            noise.write(Noise { pos, radius: 15.0, kind: NoiseKind::Help });
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
                    noise.write(Noise { pos, radius: 15.0, kind: NoiseKind::Help });
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
                        grenades.write(crate::gadgets::NpcGrenade { owner: e, from, vel, fuse: g.fuse, blast: g.blast.clone() });
                        noise.write(Noise { pos, radius: 20.0, kind: NoiseKind::Threatening });
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
                    if d > 2.5 && npc.stagger <= 0.0 && npc.hit_react <= 0.0 {
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
                        let chance = shot_chance(&npc, &ctx, d, if npc.ranged == 1 { attrs.npc_gun_miss } else { 0.0 });
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
                            shots.write(NpcShot { from: pos + Vec3::Y * 0.5, to: ppos, bow: npc.ranged >= 2, miss: !hit });
                        }
                        npc.shots += 1;
                        if npc.ranged == 1 {
                            npc.gun_hold = GUN_HOLD;
                            npc.target = None;
                            noise.write(Noise { pos, radius: 40.0, kind: NoiseKind::Danger });
                            continue;
                        }
                        noise.write(Noise { pos, radius: 40.0, kind: NoiseKind::Danger });
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
                    if d < 1.4 {
                        npc.target = None;
                    }
                } else if npc.has_sword || npc.enemy {
                    for c in npc.melee_cd.iter_mut() {
                        *c -= dt;
                    }
                    // its parry's counter (`DisTweaks_NPCRiposte`): a quick blow straight after
                    if npc.riposte > 0.0 {
                        npc.riposte -= dt;
                        if npc.riposte <= 0.0 && npc.attack_t.is_none() && npc.stagger <= 0.0 && d < 2.6 {
                            npc.attack_t = Some(0.0);
                            npc.swing = Some(Swing { kind: MoveKind::Short, big: false, len: 0.85, hit: 0.4, reach: 2.6, lunge: 0.0, no_versus: false, more: Vec::new() });
                        }
                    }
                    // Corvo's blow coming: a step aside or back out of it (its blade's
                    // `DisTweaks_NPCSideStep` / `BackStep`, within its reach and on its cooldown;
                    // how readily, by its `m_DodgeLevel`: a tenth a level)
                    let swinging = psword.is_some_and(|s| s.swing > 0.0 && s.swing < 0.1 && !s.hit_done);
                    if swinging && npc.attack_t.is_none() && npc.stagger <= 0.0 && npc.retreat.is_none() && npc.sees_player {
                        let moves = npc.arms.moves.clone();
                        if npc.melee_cd.len() != moves.len() {
                            npc.melee_cd = vec![0.0; moves.len()];
                        }
                        let dodge = moves.iter().enumerate().find(|(i, m)| matches!(m.kind.as_str(), "SideStep" | "BackStep") && npc.melee_cd[*i] <= 0.0 && d <= m.range[1] && inhibited.get(&m.kind).is_none_or(|(_, by)| *by == e));
                        if let Some((i, m)) = dodge {
                            if rand::random::<f32>() < npc.arms.dodge * 0.1 {
                                inhibited.insert(m.kind.clone(), (inhibition(&m.kind), e));
                                let toward = (ppos - pos).with_y(0.0).normalize_or_zero();
                                let away = if m.kind == "SideStep" {
                                    let side = Vec3::new(-toward.z, 0.0, toward.x);
                                    if rand::random::<bool>() { side } else { -side }
                                } else {
                                    -toward
                                };
                                npc.retreat = Some((pos + away * m.step.max(1.0), 0.45));
                                // (its step's clip: left, right or back)
                                let right = Vec3::new(-npc.forward().z, 0.0, npc.forward().x);
                                npc.gesture(if m.kind != "SideStep" { 17 } else if away.dot(right) > 0.0 { 16 } else { 15 });
                                if std::env::var("DH_MELEE_LOG").is_ok() {
                                    info!("melee: {} dodges ({}) at {d:.1} m", npc.name, m.kind);
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
                        let blows = std::iter::once(hit_at).chain(npc.swing.as_ref().map(|s| s.more.clone()).unwrap_or_default());
                        for b in blows {
                            if !(t_att < b && t_new >= b) {
                                continue;
                            }
                            // an assassin's bolt, as its accuracy has it
                            if kind == Some(MoveKind::Ranged) {
                                let hit = npc.sees_player && rand::random::<f32>() < shot_chance(&npc, &fire_context(&npc), d, 0.0);
                                info!("{} shoots {d:.1} m ({})", npc.name, if hit { "hit" } else { "miss" });
                                if hit {
                                    hit_writer.write(PlayerHit { npc: e, from: pos, damage: npc.arms.ranged, kick: false, big: false, push: false, unblockable: false, missile: false });
                                }
                                shots.write(NpcShot { from: pos + Vec3::Y * 0.5, to: ppos, bow: true, miss: !hit });
                                npc.shots += 1;
                                noise.write(Noise { pos, radius: 40.0, kind: NoiseKind::Danger });
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
                                    noise.write(Noise { pos, radius: 8.0, kind: NoiseKind::Threatening });
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
                                shots.write(NpcShot { from: pos + Vec3::Y * 0.2, to: ppos, bow: false, miss: !struck });
                                noise.write(Noise { pos, radius: 40.0, kind: NoiseKind::Danger });
                            } else {
                                noise.write(Noise { pos, radius: 8.0, kind: NoiseKind::Threatening });
                            }
                        }
                        npc.attack_t = if t_new > len { None } else { Some(t_new) };
                        if npc.attack_t.is_none() && kind == Some(MoveKind::Under) {
                            npc.under_cd[0] = UNDER_SWORD.0 + rand::random::<f32>() * UNDER_SWORD.1;
                        }
                        if npc.attack_t.is_none() {
                            npc.swing = None;
                        }
                    } else if npc.attack_cd <= 0.0 && npc.stagger <= 0.0 && npc.sees_player && (attackers < max_attackers || (!npc.arms.outer_only.is_empty() && (!ring.contains(&e) || (ppos.y - pos.y).abs() > 1.6))) && npc.may_melee() && pace_ready(&npc) {
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
                                    Swing { kind, big: rand::random::<f32>() < m.big, len, hit, reach: if kind == MoveKind::Bash { m.range[1] + 0.3 } else { 2.4 }, lunge, no_versus: false, more: Vec::new() }
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
                                npc.melee_cd[i] = (cd[0] + rand::random::<f32>() * (cd[1] - cd[0]).max(0.0)) * ((1.0 - npc.aggression.clamp(0.0, 1.0)) * COOLDOWN_FACTOR + 1.0);
                            }
                            // a blow of its flurry; spent, it recovers its stamina
                            npc.flurry -= 1;
                            if npc.flurry < 1 {
                                npc.stamina = STAMINA_RECOVERY[0] + rand::random::<f32>() * (STAMINA_RECOVERY[1] - STAMINA_RECOVERY[0]);
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

/// A civilian that may panic now: unarmed, not barred from it, able to run, not already
/// running, not of Corvo's own people.
fn civilian_may_panic(npc: &Npc) -> bool {
    !npc.hostile() && !npc.fighter() && npc.may_panic() && npc.can_flee && npc.mode != Mode::Flee && !npc.is_down() && !dhcook::format::is_player_faction(&npc.faction)
}

/// Whether a noise sends a searcher running (`m_SearchFromNoiseTweaks[4..7].m_bShouldRun`:
/// danger, death rattles, cries for help, alarms).
fn runs_to(kind: NoiseKind) -> bool {
    matches!(kind, NoiseKind::Danger | NoiseKind::DeathRattle | NoiseKind::Help | NoiseKind::Alarm)
}

/// Steering + kinematic movement.
fn npc_move(
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<
        (&mut Npc, &mut Transform, Option<&mut KinematicCharacterController>, Option<&KinematicCharacterControllerOutput>),
        (Without<Player>, Without<ScriptedAnim>, Without<crate::possession::Possessed>, Without<crate::carry::Falling>),
    >,
    mut steps: MessageWriter<crate::footsteps::Footfall>,
    (grid, mut budget, level): (Option<Res<crate::navmesh::NavGrid>>, ResMut<crate::navmesh::PathBudget>, Option<Res<LevelInfo>>),
) {
    let (dt_world, dt_own) = (time.delta_secs().min(0.05) * tc.world_scale(), time.delta_secs().min(0.05) * tc.own_scale());
    // (fallen out of the world: below the level's kill height, `WorldInfo.KillZ`)
    let kill_y = level.as_ref().map(|l| l.scene.kill_y).unwrap_or(-200.0);
    let ppos = player.single().map(|t| t.translation).ok();
    let ctx = rapier.single().ok();
    let ground_filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, crate::level::GROUP_WORLD));
    for (mut npc, mut t, kcc, out) in &mut npcs {
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
            Mode::Combat | Mode::Flee => npc.gait[2],
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
                let steer = crate::navmesh::steer(grid.as_deref(), &mut budget, &mut nav, pos - Vec3::Y * NPC_CENTER, goal, dt);
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
        // stuck handling: sidestep perpendicular to the desired direction
        if wish.length_squared() > 0.01 && !paced {
            npc.stuck += dt;
            if npc.stuck > 0.8 {
                if pos.distance(npc.progress_pos) < 0.3 {
                    npc.sidestep = 0.7 * if rand::random::<bool>() { 1.0 } else { -1.0 };
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
        let new_h = if paced { wish } else { horiz + (wish - horiz) * (if npc.grab.is_some() { 20.0 } else { 8.0 } * dt).min(1.0) };
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
        let face = match (npc.mode, ppos) {
            (Mode::Combat, Some(p)) if npc.sees_player && p.distance(pos) < 6.0 => Some((p - pos).with_y(0.0)),
            (Mode::Combat, _) if npc.last_seen.map(|l| l.distance(pos) < 6.0).unwrap_or(false) && new_h.length() < 1.0 => {
                npc.last_seen.map(|l| (l - pos).with_y(0.0))
            }
            _ if new_h.length() > 0.3 => Some(new_h),
            _ if npc.attn_level == 2 && npc.alert == Alert::Unaware && npc.target.is_none() => npc.last_seen.map(|l| (l - pos).with_y(0.0)),
            (Mode::Idle, _) if npc.target.is_none() => Some(Quat::from_rotation_y(npc.home_yaw) * Vec3::NEG_Z),
            _ => None,
        };
        // (turning with its clip: faced the new way already, shown turning: `npc_select_anim`)
        let face = face.filter(|_| npc.pace.is_none_or(|p| p.turn.is_none()));
        if let Some(f) = face {
            if f.length_squared() > 1e-4 {
                let target_yaw = (-f.x).atan2(-f.z);
                let mut dy = (target_yaw - npc.yaw + PI).rem_euclid(TAU) - PI;
                let max = 6.0 * dt;
                dy = dy.clamp(-max, max);
                npc.yaw += dy;
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
        // a foot down every half cycle (near the player only)
        let crossed = npc.anim_phase < before || (before < PI && npc.anim_phase >= PI);
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
            steps.write(Footfall { pos: t.translation, gait, who });
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
    idle_fidgets: Vec<ClipId>,
    walk: Option<ClipId>,
    idle_alert: Option<ClipId>,
    walk_alert: Option<ClipId>,
    run: Option<ClipId>,
    /// the alert walk's and the run's other ways (`AnimNodeBlendDirectional`: east (right),
    /// south, west), each with its rate's sign: a set without a south or west of its own plays
    /// its `_Revert` clip backwards (the guards' sword sets)
    walk_dirs: [Option<(ClipId, f32)>; 3],
    run_dirs: [Option<(ClipId, f32)>; 3],
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
    /// lying poses (last frames) to settle into after a reaction that ends mid-fall
    lie_back: Option<ClipId>,
    lie_front: Option<ClipId>,
    assassinated: Vec<ClipId>,
    choke_in: Option<ClipId>,
    choke_loop: Option<ClipId>,
    choke_out: Option<ClipId>,
    corpse: Vec<ClipId>,
    search: Vec<ClipId>,
    surprised: Option<ClipId>,
    /// startled by a live explosive, sword sheathed or drawn (`Empty_Surprised_Front`,
    /// `Sword_Surprised_Front`)
    startled: [Option<ClipId>; 2],
    equip: Option<ClipId>,
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
    /// turning where it stands, left then right, by 45, 90, 135 and 180 degrees: relaxed
    /// (`Sword_Patrol_Turn*`, `Empty_Turn*`) and on edge (`Sword_Turn*`)
    turn: [[[Option<ClipId>; 4]; 2]; 2],
    /// its relaxed walk begun where it stands (`Sword_Patrol_Walk_Start`, `Empty_Walk_Start`),
    /// ended as its left or right foot lands (`_Walk_StopLeft`, `_StopRight`), and begun turning
    /// into it, left then right, by 45, 90, 135 and 180 degrees (`_TurnToWalkLeft45`...): its idle
    /// speed mode's start, stop and turn props
    walk_start: Option<ClipId>,
    walk_stop: [Option<ClipId>; 2],
    turn_to_walk: [[Option<ClipId>; 4]; 2],
    /// turning round on the move, walking (its relaxed walk's) and running, left then right
    /// (`Sword_Patrol_Walk_TurnLeft180`, `Empty_Run_TurnRight180`...: its walk and run speed
    /// modes' turn props)
    move_turn: [[Option<ClipId>; 2]; 2],
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
    /// its aim offsets (`ADD_Sword_Gun_AimIdle_*`): up, centre and down rows, left to right
    gun_aim: [Option<ClipId>; 9],
}

impl NpcClips {
    /// None unless the character can at least stand and walk.
    pub fn resolve(lib: &CharAnims) -> Option<NpcClips> {
        let c = NpcClips {
            // humans first, then the wolfhound's own sets (Patrol_ / Attack_)
            idle: lib.first(&["Sword_Patrol_Idle", "Empty_Idle", "Sword_Idle", "Patrol_Idle"]),
            // (the ambient idles of the set it stands in, all it has: a guard's patrol stance,
            // empty hands (a tall thug's are named without the underscore), a wolfhound's)
            idle_fidgets: {
                let sets: [(&str, &[&str]); 3] = [
                    ("Sword_Patrol_Idle", &["Sword_Patrol_Idle_01", "Sword_Patrol_Idle_02", "Sword_Patrol_Idle_03", "Sword_Patrol_Idle_04"]),
                    (
                        "Empty_Idle",
                        &["Empty_Idle_01", "Empty_Idle_02", "Empty_Idle_03", "Empty_Idle_04", "Empty_Idle_05", "Empty_Idle_06", "Empty_Idle_07", "Empty_Idle_08", "Empty_Idle02", "Empty_Idle05", "Empty_Idle07"],
                    ),
                    ("Patrol_Idle", &["Patrol_Idle_01", "Patrol_Idle_02", "Patrol_Idle_03", "Patrol_Idle_04"]),
                ];
                sets.iter().find(|(base, _)| lib.first(&[base]).is_some()).map(|(_, all)| lib.all(all)).unwrap_or_default()
            },
            walk: lib.first(&["Sword_Patrol_WalkN", "Empty_Walk_Patrol", "Empty_WalkN", "Sword_WalkN", "Patrol_WalkN"]),
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
            lie_back: lib.first(&["Generic_DeathFrontA", "Corpses_DeathPose_OnBack1", "Generic_DeathFront_A", "Generic_DeathA"]),
            lie_front: lib.first(&["Generic_DeathBackA", "Generic_DeathFrontA", "Generic_DeathBack_A", "Generic_DeathB"]),
            assassinated: lib.all(&["Generic_Assassination_Back_Slave", "Generic_Assassination_FastBack_Slave"]),
            choke_in: lib.find("Generic_Choke_In_Slave"),
            choke_loop: lib.find("Generic_Choke_Loop_Slave"),
            choke_out: lib.find("Generic_Choke_Win_Slave"),
            corpse: lib.all(&["Corpses_DeathPose_OnBack1", "Corpses_DeathPose_OnBack2", "Corpses_DeathPose_OnBack3"]),
            search: lib.all(&["Sword_OutOfSight", "Empty_OutOfSight", "Sword_PlayerCheck", "Empty_PlayerCheck", "Patrol_IdleSearch"]),
            surprised: lib.first(&["Sword_Surprised_Front", "Empty_Surprised_Front", "Attack_IdleBark_Short", "Empty_Taunt"]),
            startled: [lib.find("Empty_Surprised_Front"), lib.find("Sword_Surprised_Front")],
            equip: lib.first(&["Sword_Patrol_Equip", "Sword_Equip"]),
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
            turn: [["Sword_Patrol_Turn", "Empty_Turn", "Patrol_Turn"], ["Sword_Turn", "Empty_Turn", "Sword_Patrol_Turn"]].map(|sets| {
                ["Left", "Right"].map(|side| ["45", "90", "135", "180"].map(|a| sets.iter().find_map(|s| lib.find(&format!("{s}{side}{a}")))))
            }),
            trigger_alarm: lib.all(&["Sword_TriggerAlarm"]),
            corpse_check: lib.all(&["Sword_CorpseCheck"]),
            under: [lib.find("Sword_Attack_UnderTable"), lib.find("Sword_Gun_AttackUnderTable")],
            // (a wolfhound's: its barks, `Attack_IdleBark01`-`04`, each with its own bark)
            taunt: lib.all(&["Sword_TauntA", "Sword_TauntB", "Sword_TauntC", "Attack_IdleBark01", "Attack_IdleBark02", "Attack_IdleBark03", "Attack_IdleBark04"]),
            cough: lib.all(&["Empty_Idle_Cough"]),
            check_pockets: lib.all(&["Empty_CheckPockets"]),
            beg: [lib.find("Empty_Beg_Kneeling_In"), lib.find("Empty_Beg_Kneeling_Loop"), lib.find("Empty_Beg_Kneeling_Out")],
            music_box: [lib.find("Musicbox_Idle"), lib.find("MusicBox_PlayMusic_In"), lib.find("MusicBox_PlayMusic_Loop"), lib.find("MusicBox_PlayMusic_Out")],
            cower: lib.find("Empty_Idle_Panic_Recover"),
            gun_aim: ["UpLeft", "Up", "UpRight", "CenterLeft", "Center", "CenterRight", "DownLeft", "Down", "DownRight"].map(|d| lib.find(&format!("ADD_Sword_Gun_AimIdle_{d}"))),
            walk_dirs: [None; 3],
            run_dirs: [None; 3],
            walk_start: None,
            walk_stop: [None; 2],
            turn_to_walk: [[None; 4]; 2],
            move_turn: [[None; 2]; 2],
        };
        let dirs = |n: Option<ClipId>, base: &str| -> [Option<(ClipId, f32)>; 3] {
            let Some(prefix) = n.and_then(|n| lib.clip(n).name.strip_suffix(&format!("{base}N")).map(str::to_string)) else { return [None; 3] };
            let way = |d: &str| lib.find(&format!("{prefix}{base}{d}")).map(|c| (c, 1.0)).or_else(|| lib.find(&format!("{prefix}{base}{d}_Revert")).map(|c| (c, -1.0)));
            [lib.find(&format!("{prefix}{base}E")).map(|c| (c, 1.0)), way("S"), way("W")]
        };
        // (its relaxed walk's start, stops and turns into it, by its walk's prefix:
        // `Sword_Patrol_`, `Empty_`)
        let relaxed = c.walk.and_then(|w| lib.clip(w).name.strip_suffix("WalkN").map(str::to_string));
        let relaxed = |n: &str| relaxed.as_ref().and_then(|p| lib.find(&format!("{p}{n}")));
        let walk_start = relaxed("Walk_Start");
        let walk_stop = ["Left", "Right"].map(|s| relaxed(&format!("Walk_Stop{s}")));
        let turn_to_walk = ["Left", "Right"].map(|s| ["45", "90", "135", "180"].map(|a| relaxed(&format!("TurnToWalk{s}{a}"))));
        let run_prefix = c.run.and_then(|r| lib.clip(r).name.strip_suffix("RunN").map(str::to_string));
        let move_turn = [
            ["Left", "Right"].map(|s| relaxed(&format!("Walk_Turn{s}180"))),
            ["Left", "Right"].map(|s| run_prefix.as_ref().and_then(|p| lib.find(&format!("{p}Run_Turn{s}180")))),
        ];
        let c = NpcClips { walk_dirs: dirs(c.walk_alert, "Walk"), run_dirs: dirs(c.run, "Run"), walk_start, walk_stop, turn_to_walk, move_turn, ..c };
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
    /// the way it last went relative to its facing (0 ahead, 1 right, 2 back, 3 left)
    way: u8,
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
    /// up again from where it lay: its stun's recovery to come
    rise_next: Option<ClipId>,
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
    mut last: Local<std::collections::HashMap<Entity, (Option<ClipId>, Option<ClipId>)>>,
) {
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for (e, npc, anim, t, scripted) in &npcs {
        if t.translation.distance(ppos) > 8.0 {
            continue;
        }
        let cur = anim.current().map(|p| p.clip);
        let upper = anim.upper_clip();
        if last.get(&e) != Some(&(cur, upper)) {
            last.insert(e, (cur, upper));
            info!(
                "npc clip: {} -> {} upper {} (mode {:?} alert {:?} hit {:.2} stagger {:.2} attack {:?} scripted {scripted})",
                npc.name,
                cur.map(|c| anim.lib.clip(c).name.as_str()).unwrap_or("-"),
                upper.map(|c| anim.lib.clip(c).name.as_str()).unwrap_or("-"),
                npc.mode,
                npc.alert,
                npc.hit_react,
                npc.stagger,
                npc.attack_t
            );
        }
    }
}

/// Choose each NPC's clip from its state (locomotion, attacks, reactions, deaths).
fn npc_select_anim(
    time: Res<Time>,
    tc: Res<TimeControl>,
    mut commands: Commands,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<(Entity, &mut Npc, &mut NpcAnim, &mut Animator, &Transform, &Children, Option<&DropKilled>, Option<&AssassinClip>, Option<&mut FinisherClip>, Option<&crate::carry::Falling>, (Has<crate::ragdoll::Ragdoll>, Has<crate::ragdoll::GoLimp>, Has<crate::musicbox::TunePlaying>)), Without<ScriptedAnim>>,
    mut visuals: Query<&mut Transform, (With<NpcVisual>, Without<Npc>, Without<Player>)>,
    mut swarmed: MessageReader<crate::swarm::SwarmOn>,
) {
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    let swarmed: std::collections::HashSet<Entity> = swarmed.read().map(|s| s.0).collect();
    for (e, mut npc, mut st, mut anim, t, children, dropped, assassin, finisher, falling, (limp, going_limp, tune)) in &mut npcs {
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
        let clips = st.clips.clone();
        let lib = anim.lib.clone();

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
        let fidget = anim.upper_clip().is_some_and(|c| clips.idle_fidgets.contains(&c));
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
        let drawn = npc.alert != Alert::Unaware
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
                st.rise_next = pick(&clips.recovery[hands]).or_else(|| pick(&clips.recovery[1 - hands]));
                anim.restart(c, false, 1.0, 0.0);
                npc.stagger = npc.stagger.max(lib.duration(c) + st.rise_next.map_or(0.0, |r| lib.duration(r)));
                st.oneshot = true;
                st.hit_seen = npc.hit_react;
                st.last_mode = npc.mode;
                st.last_alert = npc.alert;
                continue;
            }
        }
        if let Some(c) = st.rise_next.filter(|_| anim.finished()) {
            st.rise_next = None;
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
                let (clip, settle) = if npc.corpse {
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
            if st.ragdoll && !far {
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
                if let Some(c) = clips.dodge_attack[side].filter(|_| near && npc.mode == Mode::Combat) {
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
                    MoveKind::WhiskeyFire => clips.bottle_fire[0].or(clips.bottle_fire[1]),
                    MoveKind::ThrowWhiskey => clips.bottle_throw,
                    MoveKind::Throw => pick(&clips.throw_rock),
                    MoveKind::Attract => clips.attract[2].or_else(|| pick(&clips.attack_short_big)),
                    MoveKind::Ranged => clips.wrist_shot,
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
            anim.additive.clear();
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
        if npc.alert == Alert::Combat && st.last_alert != Alert::Combat && !reacted {
            if let Some(c) = clips.equip.or(clips.surprised) {
                // (on the move, its sword drawn on its upper body, its legs keeping their stride:
                // the NPC tree's upper-body slot)
                if npc.anim_speed >= 0.6 && clips.equip.is_some() && !musical {
                    anim.restart_upper(c, false, 1.4, 0.15);
                } else {
                    anim.restart(c, false, 1.4, 0.15);
                    st.oneshot = true;
                }
            }
        } else if npc.mode == Mode::Search && st.last_mode != Mode::Search && !reacted {
            // (searching for what struck it: the flinch shows first)
            if let Some(c) = pick(&clips.search) {
                anim.restart(c, false, 1.0, 0.25);
                st.oneshot = true;
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
                if let Some(c) = pick(&clips.idle_fidgets) {
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
                clips.walk_start.map(|c| (c, None))
            } else {
                let i = ((off.abs().to_degrees() / 45.0).round() as usize).clamp(1, 4) - 1;
                clips.turn_to_walk[(off < 0.0) as usize][i].map(|c| (c, Some((st.shown_yaw, off))))
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
        let walking = anim.current().filter(|c| relaxed && npc.pace.is_none() && sp > 0.6 && Some(c.clip) == clips.walk);
        match (walking, npc.path) {
            (Some(cur), Some((left, _))) => {
                let ph = (cur.t / lib.duration(cur.clip)).rem_euclid(1.0);
                let was = std::mem::replace(&mut st.walk_phase, ph);
                let foot = if was < 0.5 && ph >= 0.5 { Some(0) } else if ph < was { Some(1) } else { None };
                let way = (-npc.wish.x).atan2(-npc.wish.z);
                let ahead = npc.wish.length_squared() > 0.01 && ((way - npc.yaw + PI).rem_euclid(TAU) - PI).abs() <= 22.5f32.to_radians();
                let next = npc.stride * 0.5;
                if let Some(c) = foot.and_then(|f| clips.walk_stop[f]).filter(|_| ahead && left < UNACCURACY && left - WALK_STRAIGHT < next * 0.5) {
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
        let fore = anim.current().and_then(|c| if Some(c.clip) == clips.walk { Some(0) } else if Some(c.clip) == clips.run { Some(1) } else { None });
        if let Some(gait) = fore.filter(|_| npc.pace.is_none() && sp >= 0.4 && !far && st.turn.is_none() && npc.stagger <= 0.0 && npc.attack_t.is_none() && st.way == 0 && npc.wish.length_squared() > 0.01) {
            let way = (-npc.wish.x).atan2(-npc.wish.z);
            let off = (way - npc.yaw + PI).rem_euclid(TAU) - PI;
            if let Some(c) = clips.move_turn[gait][(off < 0.0) as usize].filter(|_| off.abs() >= 135f32.to_radians()) {
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
                    let edge = (npc.alert != Alert::Unaware || st.drawn) as usize;
                    let i = ((off.abs().to_degrees() / 45.0).round() as usize).clamp(1, 4) - 1;
                    // (a turn to the left is a rising yaw)
                    let side = (off < 0.0) as usize;
                    if let Some(c) = clips.turn[edge][side][i].or(clips.turn[1 - edge][side][i]) {
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
            let c = if npc.mode == Mode::Flee && !npc.fighter() { clips.cower.or(clips.idle_alert).or(clips.idle) } else if alert { clips.idle_alert.or(clips.idle) } else { clips.idle };
            if let Some(c) = c {
                anim.play(c, true, 1.0, 0.3);
            }
        } else {
            let running = npc.mode != Mode::Flee && sp > (npc.gait[1] * 1.3 + npc.gait[2]) * 0.5;
            let c = if npc.mode == Mode::Flee {
                clips.flee.or(clips.run)
            } else if running {
                clips.run.or(clips.walk_alert)
            } else if alert {
                clips.walk_alert.or(clips.walk)
            } else {
                clips.walk
            };
            // alert, it goes the way it goes facing where it faces: the walk or run for that way
            // (the quarter its going falls in, held 10 degrees past the edge, not to flicker)
            let (c, sign) = match c {
                Some(c) if alert && npc.mode != Mode::Flee => {
                    let v = npc.velocity.with_y(0.0);
                    let right = Quat::from_rotation_y(npc.yaw) * Vec3::X;
                    let a = v.dot(right).atan2(v.dot(npc.forward())).to_degrees();
                    let centre = [0.0f32, 90.0, 180.0, -90.0][st.way as usize % 4];
                    let off = (a - centre + 540.0).rem_euclid(360.0) - 180.0;
                    if off.abs() > 55.0 {
                        st.way = (((a + 405.0) / 90.0).floor() as i32).rem_euclid(4) as u8;
                        if std::env::var("DH_LOCO_LOG").is_ok() {
                            info!("loco: {} goes {} ({a:.0} deg off its facing at {sp:.1} m/s)", npc.name, ["ahead", "right", "back", "left"][st.way as usize]);
                        }
                    }
                    let dirs = if running { &clips.run_dirs } else { &clips.walk_dirs };
                    match st.way {
                        0 => (Some(c), 1.0),
                        w => dirs[w as usize - 1].map(|(d, s)| (Some(d), s)).unwrap_or((Some(c), 1.0)),
                    }
                }
                c => (c, 1.0),
            };
            if let Some(c) = c {
                let natural = lib.root_speed(c).max(0.3);
                anim.play(c, true, sign * (sp / natural).clamp(0.5, 1.8), 0.25);
                let stride = lib.root_speed(c) * lib.clip(c).duration;
                if (stride - npc.stride).abs() > 0.01 && std::env::var("DH_LOCO_LOG").is_ok() {
                    let k = lib.clip(c);
                    info!("loco: {} strides {stride:.2} m a loop ({} {:.2} s, {:?})", npc.name, k.name, k.duration, k.sounds);
                }
                npc.stride = stride;
            }
        }
    }
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
        NpcAttachment { socket: "Helmet".into(), prop: String::new(), health: 0.0, instant: vec![], immune: vec![], breaks: None, chance, group, weight, knock_off: vec![], absorb: vec![], absorb_head: false, absorb_side: 0 }
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
