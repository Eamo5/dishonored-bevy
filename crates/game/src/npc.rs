//! NPCs: spawned from the level's DishonoredSpawners using the original skinned models,
//! animated with the pawn's original animation sets (procedural fallback for characters
//! without them), with perception and a stealth/combat state machine.

use crate::anim::{Animator, CharAnims, ClipId};
use crate::gameplay::{HitKind, Noise, NpcHit, NpcStagger, PlayerHit, PlayerStats, TimeControl};
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
            .add_systems(PostUpdate, model_details.run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, log_jumps.after(bevy_rapier3d::plugin::PhysicsSet::Writeback).run_if(in_state(GameState::InGame)))
            .add_systems(
                PostUpdate,
                (scene_head_look, speaking_jaw).after(crate::anim::AnimPose).before(bevy::transform::TransformSystems::Propagate).run_if(in_state(GameState::InGame)),
            )
            .add_systems(
                Update,
                (
                    record_trail,
                    spawn_requested,
                    npc_hits,
                    ash_marked,
                    burn_ashes,
                    npc_shots,
                    npc_hearing,
                    npc_perception.run_if(|| !skip("perception")),
                    attention_look,
                    npc_brain.run_if(|| !skip("brain")),
                    npc_finishers,
                    weeper_grab,
                    hold_scripted,
                    npc_move.run_if(|| !skip("move")),
                    npc_select_anim.run_if(|| !skip("animate")),
                    npc_animate.run_if(|| !skip("animate")),
                    log_npc_clips.run_if(|| std::env::var("DH_NPC_CLIP_LOG").is_ok()),
                )
                    .chain()
                    .run_if(in_state(GameState::InGame)),
            );
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
const MAX_ATTACKERS: usize = 2;
/// Beyond this distance from the player, unaware NPCs move without the character controller.
const FAR_MOVE: f32 = 25.0;

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
    /// a gesture to play once (1 its parry won, 2 its blow parried, 3 a shove) and its count;
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
    pub stuck: f32,
    pub sidestep: f32,
    pub progress_pos: Vec3,
    pub anim_phase: f32,
    pub anim_speed: f32,
    pub hit_react: f32,
    pub down_t: f32,
    pub look: f32,
    pub has_sword: bool,
    /// ranged weapon: 0 none, 1 pistol (elite guards), 2 bow (tallboys)
    pub ranged: u8,
    /// seconds before the next shot (`m_ContextCooldown`)
    pub shoot_cd: f32,
    /// an NPC of an enemy faction it is fighting (`DisTweaks_Faction.m_EnemyFactions`), and
    /// when it next looks for one
    pub foe: Option<Entity>,
    pub feud_scan: f32,
    /// a weeper's arm grab (`DisTweaks_WeeperGrab`): phase and time in it
    pub grab: Option<(GrabPhase, f32)>,
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
}

impl Default for Arms {
    fn default() -> Self {
        Arms { melee: 15.0, ranged: 20.0, accuracy: [0.35, 0.1], parry: [0.5, 0.25, 3.0], block_break: [0.3, 0.6], regen: [5.0, 5.0, 15.0], big_hit: 10.0, dodge: 0.0, grenades: Vec::new(), moves: Vec::new(), tunes: None }
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
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MoveKind {
    Short,
    Medium,
    Long,
    Bash,
    Jump,
    Left90,
    Right90,
    Left180,
    Right180,
}

impl MoveKind {
    fn of(kind: &str) -> MoveKind {
        match kind {
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
    pub fn of(st: &dhcook::format::NpcStats, difficulty: u8) -> Arms {
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
        a.tunes = st.tunes.clone();
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

/// Killed by a drop assassination: from which side (0 front, 1 back, 2 left, 3 right).
#[derive(Component, Clone, Copy)]
pub struct DropKilled(pub u8);

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
/// `m_NumAttackButtonPressToWin` attacks), the release, or a miss.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrabPhase {
    Try,
    Hold,
    Release,
    Miss,
}

/// Corvo held by a weeper.
#[derive(Component)]
pub struct Grabbed {
    pub by: Entity,
    pub presses: u32,
}

/// `DisTweaks_WeeperGrab`: reach, dodge radius (UE units / 100), presses to break free.
const GRAB_RANGE: f32 = 4.0;
const GRAB_DODGE: f32 = 1.5;
pub const GRAB_PRESSES: u32 = 5;

/// A wolfhound's leap and pin (`Twk_Inv_WolfhoundBite`: `DisTweaks_WHJumpAttack` from 5.5 to
/// 8 m, knocking Corvo down for `m_DamageOnKnockDown` 7/10/15/20 by difficulty; then
/// `DisTweaks_WHArmAttack`'s struggle, within 1.5 m: `m_LoopSettings` 5 damage every 0.75 s,
/// `DisTweaks_Minigame`'s 3 presses to win).
const HOUND_LEAP: (f32, f32) = (5.5, 8.0);
const HOUND_HOLD: f32 = 1.5;
const HOUND_KNOCKDOWN: [f32; 4] = [7.0, 10.0, 15.0, 20.0];
const HOUND_BITE: (f32, f32) = (5.0, 0.75);
const HOUND_PRESSES: u32 = 3;

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
            GrabPhase::Try if el >= 0.6 => {
                let d = (pt.translation - t.translation).with_y(0.0).length();
                let hound = is_hound(&npc);
                if d < if hound { HOUND_HOLD } else { GRAB_DODGE } && grabbed.is_none() && !npc.is_down() {
                    // a wolfhound's leap knocks him down
                    if hound {
                        let dmg = HOUND_KNOCKDOWN[(settings.difficulty as usize).min(3)];
                        crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, dmg);
                    }
                    info!("weeper grab: {} holds Corvo", npc.name);
                    npc.grab = Some((GrabPhase::Hold, 0.0));
                    commands.entity(pe).insert(Grabbed { by: e, presses: 0 });
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
                if hound && grabbed.is_some_and(|g| g.by == e) && (el / HOUND_BITE.1).floor() > ((el - time.delta_secs()) / HOUND_BITE.1).floor() {
                    crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, HOUND_BITE.0);
                    sfx.write(crate::audio::PostEvent::named("Snd_Wolfhound_Minigame_Bite", Some(t.translation + Vec3::Y * 0.5)));
                }
                let need = if hound { HOUND_PRESSES } else { GRAB_PRESSES };
                let broke = grabbed.is_none_or(|g| g.by != e || g.presses >= need) || stats.dead;
                if broke || npc.is_down() || npc.stagger > 0.5 {
                    info!("weeper grab: {} lets go", npc.name);
                    npc.grab = Some((GrabPhase::Release, 0.0));
                    npc.stagger = npc.stagger.max(1.2);
                    if grabbed.is_some_and(|g| g.by == e) {
                        commands.entity(pe).remove::<Grabbed>();
                        p.locked = false;
                    }
                }
            }
            GrabPhase::Release | GrabPhase::Miss if el >= 0.9 => {
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

/// An NPC fired at Corvo (sound, tracer).
#[derive(Message, Clone, Copy)]
pub struct NpcShot {
    pub from: Vec3,
    pub to: Vec3,
    pub bow: bool,
}

/// Gun and bow shots: the sound and a brief tracer.
fn npc_shots(
    mut shots: MessageReader<NpcShot>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut gizmos: Gizmos,
    mut recent: Local<Vec<(NpcShot, f32)>>,
    time: Res<Time>,
) {
    for s in shots.read() {
        let ev = if s.bow { "Snd_AI_TBoy_Bow_Fire" } else { "Snd_Weapon_Gun_Elite_Fire_cue_ak" };
        sfx.write(crate::audio::PostEvent::named(ev, Some(s.from)));
        if !s.bow {
            let dir = (s.to - s.from).normalize_or(Vec3::NEG_Z);
            fx.write(crate::particles::SpawnEffect { rot: Quat::from_rotation_arc(Vec3::X, dir), secs: 0.5, ..crate::particles::SpawnEffect::at("npc_pistol_muzzle", s.from + dir * 0.3) });
        }
        recent.push((*s, 0.06));
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
    let arms = stats.map(|s| Arms::of(s, difficulty)).unwrap_or_default();
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
        stuck: 0.0,
        sidestep: 0.0,
        progress_pos: pos,
        anim_phase: rand::random::<f32>() * TAU,
        anim_speed: 0.0,
        hit_react: 0.0,
        down_t: if corpse { 1.0 } else { 0.0 },
        look: 0.0,
        has_sword,
        ranged,
        shoot_cd: 2.0,
        foe: None,
        feud_scan: rand::random::<f32>(),
        grab: None,
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
        mat.apply(&mut ec);
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
            mat.apply(&mut ec);
            let m = ec.id();
            commands.entity(visual).add_child(m);
        }
    }
    // weapon in the right hand
    let mut swords = Vec::new();
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
        if let (Some((hand, at)), Some(parts)) = (hand_r, assets.props.get(prop).or(assets.props.get("city_sword"))) {
            for (mesh, mat) in &parts.parts {
                let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), at));
                mat.apply(&mut ec);
                let s = ec.id();
                commands.entity(hand).add_child(s);
                swords.push(s);
            }
        }
    }
    // what it carries on its sockets (a tallboy's whale oil tanks and shields); the breakable
    // ones can be struck (a tank, burst by a bullet or a bolt)
    let mut breakable_parts = Vec::new();
    for (ai, a) in ty.map(|t| t.attachments.as_slice()).unwrap_or(&[]).iter().enumerate() {
        let (Some((joint, at)), Some(parts)) = (socket(&a.socket, &[], Transform::IDENTITY), assets.props.get(&a.prop)) else { continue };
        let holder = commands.spawn((at, Visibility::default())).id();
        commands.entity(joint).add_child(holder);
        for (mesh, mat) in &parts.parts {
            let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), Transform::IDENTITY));
            mat.apply(&mut ec);
            let m = ec.id();
            commands.entity(holder).add_child(m);
        }
        if a.breaks.is_some() || !a.instant.is_empty() {
            commands.entity(holder).insert((Collider::ball(0.3), Sensor, CollisionGroups::new(GROUP_PROP, Group::ALL), crate::gameplay::Strikeable(holder)));
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
    for (holder, index, health) in breakable_parts {
        commands.entity(holder).insert(crate::npcparts::NpcAttached { npc: e, npc_type: tid, index, health: health.max(1.0) });
    }
    if let Some((lib, clips)) = clips {
        let lib: std::sync::Arc<CharAnims> = lib;
        let speed = |c: Option<ClipId>, lo: f32, hi: f32, def: f32| c.map(|c| lib.root_speed(c)).filter(|v| *v > 0.1).map(|v| v.clamp(lo, hi)).unwrap_or(def);
        npc.gait = [
            speed(clips.walk, 0.8, 2.0, WALK),
            speed(clips.walk_alert.or(clips.walk), 0.8, 2.6, ALERT_WALK),
            speed(clips.run, 2.5, 6.0, RUN),
        ];
        let animator = Animator::new(lib.clone(), &vis.skeleton, joints.clone());
        commands.entity(e).insert((
            animator,
            NpcAnim {
                clips: std::sync::Arc::new(clips),
                last_mode: npc.mode,
                last_alert: npc.alert,
                down: false,
                attacking: false,
                oneshot: false,
                hit_seen: 0.0,
                idle_time: 0.0,
                settle: None,
                drawn: true,
                sword: swords.clone(),
                hand: hand_r,
                holster,
                grab_seen: None,
                act_seen: 0,
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
) {
    for h in hits.read() {
        let Ok((e, mut npc, t)) = npcs.get_mut(h.npc) else { continue };
        if npc.is_down() {
            continue;
        }
        // (the scripts may spare someone physics' blows: `DisSeqAct_NPCIgnoreRBDamages`)
        if h.kind == HitKind::Impact && npc.ignore_rb_damage {
            continue;
        }
        if std::env::var("DH_HIT_LOG").is_ok() {
            info!("hit {} ({}): {:?} {:.0} at health {:.0}", npc.name, npc.pawn, h.kind, h.damage, npc.health);
        }
        // those who can fight turn on Corvo when he hurts them (the Tower's guards too)
        if !npc.enemy && npc.fighter() && !matches!(h.kind, HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::Rats | HitKind::Assassinate | HitKind::Fatality | HitKind::Choke | HitKind::SleepDart | HitKind::WallOfLight) {
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
                npc.set_mode(Mode::Unconscious);
                stats.knockouts += 1;
                msgs.push("Sleep dart hit");
                noise.write(Noise { pos, radius: 4.0, combat: false });
            }
            HitKind::Fire => {
                // `NPCImmolationTime`: they burn, then nothing is left
                npc.health = 0.0;
                npc.set_mode(Mode::Dead);
                stats.kills += 1;
                if !npc.hostile() {
                    stats.civilians_killed += 1;
                }
                commands.entity(e).try_insert(Ashes(-0.9));
                sfx.write(crate::audio::PostEvent::named("Amb_Fire_Alcohol_Immolation", Some(pos + Vec3::Y)));
                fx.write(crate::particles::SpawnEffect { follow: Some(e), secs: 1.6, ..crate::particles::SpawnEffect::at("immolation", Vec3::ZERO) });
                noise.write(Noise { pos, radius: 14.0, combat: true });
            }
            HitKind::Assassinate | HitKind::Fatality | HitKind::WallOfLight => {
                npc.health = 0.0;
                npc.set_mode(Mode::Dead);
                stats.kills += 1;
                if !npc.hostile() {
                    stats.civilians_killed += 1;
                }
                if h.kind == HitKind::Assassinate {
                    msgs.push(format!("{} assassinated", npc.name));
                }
                noise.write(Noise { pos, radius: 5.0, combat: false });
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
                noise.write(Noise { pos, radius: 14.0, combat: true });
            }
            _ => {
                npc.health = (npc.health - h.damage).max(npc.min_health);
                npc.hurt_t = 0.0;
                npc.hit_react = 0.35;
                npc.attack_t = None;
                npc.stagger = npc.stagger.max(0.25);
                if npc.mode != Mode::Combat && npc.hostile() {
                    npc.alert = Alert::Combat;
                    npc.awareness = 1.0;
                    npc.last_seen = Some(h.from);
                    npc.set_mode(Mode::Combat);
                }
                if npc.health <= 0.0 {
                    npc.set_mode(Mode::Dead);
                    stats.kills += 1;
                    if !npc.hostile() {
                        stats.civilians_killed += 1;
                    }
                    noise.write(Noise { pos, radius: 14.0, combat: true });
                } else {
                    noise.write(Noise { pos, radius: 16.0, combat: true });
                }
            }
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
                // (the armed stand their ground)
                if n.combat && !npc.fighter() && npc.may_panic() {
                    if npc.can_flee {
                        npc.set_mode(Mode::Flee);
                    }
                    npc.last_seen = Some(n.pos);
                }
                continue;
            }
            // what it hears raises its attention (the original's increases: an anomaly, or
            // danger), at least to a level; investigating, it goes to look
            if let Some(att) = level.as_ref().and_then(|l| attention_of(&npc, &l.scene)) {
                if npc.alert == Alert::Combat {
                    continue;
                }
                let (amount, lvl) = if n.combat { att.heard[2] } else { att.heard[0] };
                let floor = match lvl {
                    0 => 0.0,
                    1..=4 => att.levels[lvl as usize - 1][0],
                    _ => att.max,
                };
                npc.awareness = (npc.awareness + amount).max(floor).min(att.max);
                npc.last_seen = Some(n.pos);
                if npc.awareness >= att.levels[2][0] {
                    npc.alert = Alert::Suspicious;
                    npc.target = Some(n.pos);
                    npc.set_mode(Mode::Investigate);
                }
                continue;
            }
            if n.combat {
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
    mut npcs: Query<(Entity, &mut Npc, &Transform), Without<Player>>,
    possession: Res<crate::possession::Possession>,
    mut found: Local<std::collections::HashSet<Entity>>,
    (froms, mut events): (Query<&FromSpawner>, MessageWriter<crate::interact::Interaction>),
    (level, pvis, settings): (Option<Res<LevelInfo>>, Res<crate::stealth::PlayerVisibility>, Res<crate::settings::Settings>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    // inside a host, Corvo is the host
    // (`DH_NOTARGET`: unseen, for watching the characters in tests)
    let hidden = possession.host.is_some() || std::env::var("DH_NOTARGET").is_ok();
    if dt <= 0.0 {
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
    let mut newly_detected = false;
    for (e, mut npc, t) in &mut npcs {
        if npc.is_down() || npc.kind == Kind::Story || npc.mode == Mode::Choked || stats.dead || hidden || Some(e) == possession.host || npc.blind {
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
                rate = att.distance.apply(pvis.value, closeness).max(0.0) * att.see[difficulty];
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
                    npc.set_mode(Mode::Investigate);
                }
            } else if visible && dist < 10.0 && npc.mode != Mode::Flee && stats.health < stats.max_health && !npc.fighter() && npc.may_panic() && !npc.faction.eq_ignore_ascii_case("Faction_Corvo_Default") {
                // civilians flee from an armed, bloodied stranger (not his own people)
                if npc.can_flee {
                        npc.set_mode(Mode::Flee);
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
            } else if dist < 10.0 && npc.mode != Mode::Flee && stats.health < stats.max_health && !npc.fighter() && !npc.faction.eq_ignore_ascii_case("Faction_Corvo_Default") {
                if npc.can_flee {
                        npc.set_mode(Mode::Flee);
                    }
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
            // discover bodies
            if !npc.saw_body && npc.alert != Alert::Combat {
                for (body, b) in &body_positions {
                    let d = b.distance(eye);
                    if d < 14.0 && (*b - eye).normalize_or_zero().dot(fwd) > 0.5 {
                        let dir = (*b + Vec3::Y * 0.2 - eye).normalize_or_zero();
                        let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
                        if ctx.cast_ray(eye, dir, d - 0.3, true, filter).is_none() {
                            npc.saw_body = true;
                            // the mission statistics count each body found once
                            if found.insert(*body) {
                                stats.bodies_found += 1;
                                // the level scripts hear of it (`DisSeqEvent_Corpse` Discovered)
                                if let Ok(f) = froms.get(*body) {
                                    events.write(crate::interact::Interaction::Corpse { spawner: f.0, what: 2 });
                                }
                            }
                            npc.alert = Alert::Suspicious;
                            npc.awareness = npc.awareness.max(0.6);
                            npc.target = Some(*b);
                            npc.last_seen = Some(*b);
                            npc.set_mode(Mode::Investigate);
                            break;
                        }
                    }
                }
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
    player: Query<(Entity, &Transform, Option<&crate::combat::Sword>), With<Player>>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), (Without<Player>, Without<crate::possession::Possessed>)>,
    (level, mut npc_hits, ai, mut fled): (Option<Res<LevelInfo>>, MessageWriter<NpcHit>, Res<crate::aiworld::AiWorld>, MessageWriter<crate::aiworld::FleePointReached>),
    stances: Res<crate::script_world::SpawnerOverrides>,
    mut grenades: MessageWriter<crate::gadgets::NpcGrenade>,
    (finishing, mut finishers): (Query<(), With<FinisherClip>>, MessageWriter<NpcFinisher>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    if dt <= 0.0 {
        return;
    }
    let Ok((pe, pt, psword)) = player.single() else { return };
    let ppos = pt.translation;
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
    // one weeper at a time grabs Corvo
    let grab_active = npcs.iter().any(|(_, n, _)| n.grab.is_some());
    for (e, mut npc, t) in &mut npcs {
        let pos = t.translation;
        // (playing a finisher on a foe: held to it)
        if finishing.contains(e) {
            npc.target = None;
            npc.attack_t = None;
            continue;
        }
        npc.mode_timer += dt;
        npc.attack_cd = (npc.attack_cd - dt).max(0.0);
        npc.stagger = (npc.stagger - dt).max(0.0);
        npc.hit_react = (npc.hit_react - dt).max(0.0);
        if npc.is_down() || npc.mode == Mode::Choked {
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
                        noise.write(Noise { pos, radius: 12.0, combat: true });
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
                hit_writer.write(PlayerHit { npc: e, from: pos, damage: 0.0, kick: false, big: false, push: true });
            }
        }
        // what caught its eye (the turn-to-face level): it stops and turns to look
        if npc.attn_level == 2 && npc.alert == Alert::Unaware && matches!(npc.mode, Mode::Idle | Mode::Patrol | Mode::Return) {
            npc.target = None;
            continue;
        }
        match npc.mode {
            Mode::Idle => {
                npc.target = if pos.distance(npc.home) > 0.6 { Some(npc.home) } else { None };
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
                if Vec2::new(pos.x - goal.x, pos.z - goal.z).length() < 0.6 {
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
                    }
                } else {
                    npc.target = Some(goal);
                }
            }
            Mode::Investigate => {
                let goal = npc.target.or(npc.last_seen).unwrap_or(npc.home);
                npc.target = Some(goal);
                if pos.distance(goal) < 1.6 || npc.mode_timer > 14.0 {
                    npc.set_mode(Mode::Search);
                }
            }
            Mode::Search => {
                if npc.mode_timer > 10.0 + (npc.alert == Alert::Combat) as u8 as f32 * 6.0 + npc.watched.len() as f32 * 4.0 {
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
                if stats.dead {
                    npc.set_mode(Mode::Return);
                    npc.alert = Alert::Suspicious;
                    continue;
                }
                if npc.mode_timer < dt * 1.5 {
                    alerts.push((pos, ppos));
                }
                let d = pos.distance(ppos);
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
                        npc.swing = Some(Swing { kind: MoveKind::Short, big: true, len: 1.0, hit: 0.35, reach: reach + 0.8, lunge: 3.0, no_versus: false });
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
                if npc.sees_player {
                    npc.target = Some(ppos);
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
                if npc.mode_timer > 20.0 {
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
                        noise.write(Noise { pos, radius: 20.0, combat: true });
                        if g.jump > 0.0 {
                            // (`m_fJumpClearance`)
                            npc.retreat = Some((pos - toward * g.jump, 0.7));
                        }
                        npc.attack_cd = npc.attack_cd.max(0.8);
                        continue;
                    }
                }
                // shooting (`DisTweaks_NPCFireGun`: 2.5-30 m, within 25° of where it faces;
                // every 1-2 s at a player out of reach, else now and then)
                if npc.ranged > 0 && npc.may_shoot() {
                    npc.shoot_cd -= dt;
                    let to = ppos - pos;
                    let unreachable = (ppos.y - pos.y).abs() > 1.6 || d > 6.0;
                    let facing = to.with_y(0.0).normalize_or_zero().dot(npc.forward()) > 25f32.to_radians().cos();
                    if npc.shoot_cd <= 0.0 && npc.sees_player && npc.attack_t.is_none() && d > 2.5 && d < 30.0 && facing && npc.stagger <= 0.0 {
                        npc.shoot_cd = if npc.ranged == 3 {
                            // the wrist bow (`DisTweaks_NPCFireBow` cooldowns 3-6 s, 2-5 s out of reach)
                            if unreachable { 2.0 + rand::random::<f32>() * 3.0 } else { 3.0 + rand::random::<f32>() * 3.0 }
                        } else if unreachable {
                            1.0 + rand::random::<f32>()
                        } else {
                            10.0 + rand::random::<f32>() * 10.0
                        };
                        // its accuracy: `m_MaxAccuracy` close, falling to `m_MinAccuracy` at the
                        // end of its reach
                        let k = ((d - 2.5) / 27.5).clamp(0.0, 1.0);
                        let chance = (npc.arms.accuracy[0] + (npc.arms.accuracy[1] - npc.arms.accuracy[0]) * k) * if npc.ranged == 1 { 1.0 - attrs.npc_gun_miss } else { 1.0 };
                        if rand::random::<f32>() < chance {
                            hit_writer.write(PlayerHit { npc: e, from: pos, damage: npc.arms.ranged, kick: false, big: false, push: false });
                        }
                        shots.write(NpcShot { from: pos + Vec3::Y * 0.5, to: ppos, bow: npc.ranged >= 2 });
                        noise.write(Noise { pos, radius: 40.0, combat: true });
                    }
                    if npc.ranged == 2 {
                        // tallboys keep their distance and shoot
                        if d < 8.0 {
                            npc.target = None;
                        }
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
                if npc.name.contains("Weeper") && !grab_active && npc.attack_cd <= 0.0 && npc.stagger <= 0.0 && npc.sees_player && d < GRAB_RANGE && d > 0.6 {
                    let to = (ppos - pos).with_y(0.0);
                    let facing = to.normalize_or_zero().dot(npc.forward()) > 0.0;
                    if facing && (ppos.y - pos.y).abs() < 0.8 && rand::random::<f32>() < dt * 3.0 {
                        npc.grab = Some((GrabPhase::Try, 0.0));
                        npc.target = None;
                        continue;
                    }
                }
                // a wolfhound leaps at him from its jump's reach (now and then)
                if is_hound(&npc) && !grab_active && npc.attack_cd <= 0.0 && npc.stagger <= 0.0 && npc.sees_player && d > HOUND_LEAP.0 && d < HOUND_LEAP.1 {
                    let to = (ppos - pos).with_y(0.0);
                    if to.normalize_or_zero().dot(npc.forward()) > 0.7 && (ppos.y - pos.y).abs() < 1.0 && rand::random::<f32>() < dt * 5.0 {
                        npc.grab = Some((GrabPhase::Try, 0.0));
                        npc.attack_cd = 6.0;
                        npc.target = None;
                        info!("wolfhound leap: {} from {d:.1} m", npc.name);
                        continue;
                    }
                }
                // melee: blades, or teeth and claws for the unarmed (wolfhounds, weepers)
                if npc.has_sword || npc.enemy {
                    for c in npc.melee_cd.iter_mut() {
                        *c -= dt;
                    }
                    // its parry's counter (`DisTweaks_NPCRiposte`): a quick blow straight after
                    if npc.riposte > 0.0 {
                        npc.riposte -= dt;
                        if npc.riposte <= 0.0 && npc.attack_t.is_none() && npc.stagger <= 0.0 && d < 2.6 {
                            npc.attack_t = Some(0.0);
                            npc.swing = Some(Swing { kind: MoveKind::Short, big: false, len: 0.85, hit: 0.4, reach: 2.6, lunge: 0.0, no_versus: false });
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
                        let dodge = moves.iter().enumerate().find(|(i, m)| matches!(m.kind.as_str(), "SideStep" | "BackStep") && npc.melee_cd[*i] <= 0.0 && d <= m.range[1]);
                        if let Some((i, m)) = dodge {
                            if rand::random::<f32>() < npc.arms.dodge * 0.1 {
                                let toward = (ppos - pos).with_y(0.0).normalize_or_zero();
                                let away = if m.kind == "SideStep" {
                                    let side = Vec3::new(-toward.z, 0.0, toward.x);
                                    if rand::random::<bool>() { side } else { -side }
                                } else {
                                    -toward
                                };
                                npc.retreat = Some((pos + away * m.step.max(1.0), 0.45));
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
                        if t_att < hit_at && t_new >= hit_at {
                            let to = (ppos - pos).with_y(0.0);
                            let in_front = to.normalize_or_zero().dot(npc.forward()) > 0.5;
                            if to.length() < reach && (ppos.y - pos.y).abs() < 1.5 && in_front {
                                hit_writer.write(PlayerHit { npc: e, from: pos, damage: if kick { 0.0 } else { npc.arms.melee }, kick, big, push: false });
                            }
                            noise.write(Noise { pos, radius: 12.0, combat: true });
                        }
                        npc.attack_t = if t_new > len { None } else { Some(t_new) };
                        if npc.attack_t.is_none() {
                            npc.swing = None;
                        }
                    } else if npc.attack_cd <= 0.0 && npc.stagger <= 0.0 && npc.sees_player && attackers < MAX_ATTACKERS && npc.may_melee() {
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
                            npc.melee_cd[i] <= 0.0 && d >= m.range[0] && d <= m.range[1] && off <= m.angle.max(10.0) && (ppos.y - pos.y).abs() <= m.height.max(1.5)
                        };
                        let pick = if moves.is_empty() {
                            (d < 2.0).then_some(None)
                        } else {
                            let bash = moves.iter().enumerate().find(|(i, m)| blocking && m.kind == "Bash" && fits(*i, *m));
                            bash.or_else(|| moves.iter().enumerate().find(|(i, m)| MoveKind::is_attack(&m.kind) && fits(*i, *m))).map(|(i, _)| Some(i))
                        };
                        if let Some(i) = pick {
                            attackers += 1;
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
                                    Swing { kind, big: rand::random::<f32>() < m.big, len, hit, reach: if kind == MoveKind::Bash { m.range[1] + 0.3 } else { 2.4 }, lunge, no_versus: false }
                                }
                                None => Swing { kind: MoveKind::Short, big: false, len: 0.95, hit: 0.55, reach: 2.3, lunge: 0.0, no_versus: false },
                            });
                            if let Some(i) = i {
                                let cd = moves[i].cooldown;
                                npc.melee_cd[i] = cd[0] + rand::random::<f32>() * (cd[1] - cd[0]).max(0.0);
                            }
                            if std::env::var("DH_MELEE_LOG").is_ok() {
                                info!("melee: {} {:?} at {d:.1} m (blocking {blocking})", npc.name, npc.swing);
                            }
                            npc.attack_cd = 0.5 + rand::random::<f32>() * 0.6;
                        }
                    }
                    if d < 1.4 {
                        npc.target = None; // hold distance
                    }
                }
                let _ = pe;
            }
            Mode::Choked | Mode::Unconscious | Mode::Dead => {}
        }
    }
    if !alerts.is_empty() {
        for (_, mut npc, t) in &mut npcs {
            if npc.is_down() || !npc.hostile() || npc.alert == Alert::Combat {
                continue;
            }
            if alerts.iter().any(|(p, _)| p.distance(t.translation) < 15.0) {
                npc.alert = Alert::Combat;
                npc.awareness = 1.0;
                npc.last_seen = Some(alerts[0].1);
                npc.set_mode(Mode::Combat);
            }
        }
    }
}

/// Steering + kinematic movement.
fn npc_move(
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<
        (&mut Npc, &mut Transform, Option<&mut KinematicCharacterController>, Option<&KinematicCharacterControllerOutput>),
        (Without<Player>, Without<ScriptedAnim>, Without<crate::possession::Possessed>),
    >,
    mut steps: MessageWriter<crate::footsteps::Footfall>,
    (grid, mut budget): (Option<Res<crate::navmesh::NavGrid>>, ResMut<crate::navmesh::PathBudget>),
) {
    let dt = time.delta_secs().min(0.05) * tc.world_scale();
    let ppos = player.single().map(|t| t.translation).ok();
    let ctx = rapier.single().ok();
    let ground_filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, crate::level::GROUP_WORLD));
    for (mut npc, mut t, kcc, out) in &mut npcs {
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
            Mode::Combat | Mode::Flee => npc.gait[2],
            Mode::Investigate | Mode::Search => {
                if npc.alert == Alert::Combat {
                    npc.gait[1] * 1.3
                } else {
                    npc.gait[1]
                }
            }
            _ => npc.gait[0],
        };
        let mut wish = Vec3::ZERO;
        // a lunging blow carries it at Corvo until it lands
        let lunge = npc.swing.as_ref().filter(|s| s.lunge > 0.0 && npc.attack_t.is_some_and(|t| t < s.hit)).map(|s| s.lunge);
        let frozen = npc.stagger > 0.0 || (npc.attack_t.is_some() && lunge.is_none()) || npc.is_down() || npc.mode == Mode::Choked || npc.versus || npc.ambush.is_some();
        if let (Some(v), Some(p), false) = (lunge, ppos, frozen) {
            wish = (p - pos).with_y(0.0).normalize_or_zero() * v;
        }
        if !frozen && lunge.is_none() {
            if let Some(goal) = npc.target {
                // along its path over the navigation mesh (straight at it off the mesh)
                let mut nav = std::mem::take(&mut npc.nav);
                let steer = crate::navmesh::steer(grid.as_deref(), &mut budget, &mut nav, pos - Vec3::Y * NPC_CENTER, goal, dt);
                npc.nav = nav;
                let left = (goal - pos).with_y(0.0).length();
                let to = (steer - pos).with_y(0.0);
                let d = to.length();
                if left > 0.35 && d > 0.05 {
                    wish = to / d * speed * left.min(1.0).max(0.4);
                }
            }
        }
        // a weeper's grab: the lunge closes on Corvo, then it stands its ground
        match (npc.grab, ppos) {
            (Some((GrabPhase::Try, el)), Some(p)) if el < 0.55 && npc.stagger <= 0.0 => {
                let to = (p - pos).with_y(0.0);
                let d = to.length();
                // (a wolfhound's leap covers its 8 m)
                let cap = if is_hound(&npc) { 16.0 } else { 8.0 };
                wish = if d > 0.9 { to / d * ((d - 0.9) / (0.55 - el).max(0.15)).min(cap) } else { Vec3::ZERO };
            }
            (Some(_), _) => wish = Vec3::ZERO,
            _ => {}
        }
        // stuck handling: sidestep perpendicular to the desired direction
        if wish.length_squared() > 0.01 {
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
        let new_h = horiz + (wish - horiz) * (if npc.grab.is_some() { 20.0 } else { 8.0 } * dt).min(1.0);
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
            let hit = ctx.as_ref().and_then(|c| c.cast_ray(p + Vec3::Y * 0.6, Vec3::NEG_Y, 3.0, true, ground_filter));
            match hit {
                Some((_, toi)) => {
                    let ground = p.y + 0.6 - toi + NPC_CENTER + 0.03;
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
        let stride = if npc.anim_speed > 3.0 { 2.4 } else { 1.5 };
        let before = npc.anim_phase;
        npc.anim_phase = (npc.anim_phase + npc.anim_speed * dt * TAU / stride).rem_euclid(TAU);
        // a foot down every half cycle (near the player only)
        let crossed = npc.anim_phase < before || (before < PI && npc.anim_phase >= PI);
        let near = ppos.is_some_and(|p| p.distance(t.translation) < 30.0);
        if crossed && near && npc.anim_speed > 0.4 && npc.grounded && !npc.is_down() {
            use crate::footsteps::{Footfall, Gait, Walker};
            let n = npc.name.to_ascii_lowercase();
            let who = if n.contains("wolf") || n.contains("hound") { Walker::Wolfhound } else if n.contains("rat") && !n.contains("pirate") { Walker::Rat } else { Walker::Guard };
            let gait = if npc.anim_speed > 3.0 { Gait::Sprint } else { Gait::Walk };
            steps.write(Footfall { pos: t.translation, gait, who });
        }
        if t.translation.y < -200.0 {
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
    let t_now = time.elapsed_secs() * if tc.world_scale() > 0.0 { 1.0 } else { 0.0 };
    for (npc, rig, children) in &npcs {
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
    flee: Option<ClipId>,
    attack: Vec<ClipId>,
    /// the moves' own: a short blow (small, big), a medium and a long lunge, a kick
    attack_short: Vec<ClipId>,
    attack_short_big: Vec<ClipId>,
    attack_medium: Vec<ClipId>,
    attack_long: Vec<ClipId>,
    attack_long_big: Vec<ClipId>,
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
    stun: Option<ClipId>,
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
    equip: Option<ClipId>,
    /// a weeper's arm grab
    grab_try: Option<ClipId>,
    grab_in: Option<ClipId>,
    grab_loop: Option<ClipId>,
    grab_out: Option<ClipId>,
    grab_miss: Option<ClipId>,
    /// killed from above, by side (front, back, left, right)
    drop_killed: [Option<ClipId>; 4],
}

impl NpcClips {
    /// None unless the character can at least stand and walk.
    pub fn resolve(lib: &CharAnims) -> Option<NpcClips> {
        let c = NpcClips {
            // humans first, then the wolfhound's own sets (Patrol_ / Attack_)
            idle: lib.first(&["Sword_Patrol_Idle", "Empty_Idle", "Sword_Idle", "Patrol_Idle"]),
            idle_fidgets: lib.all(&[
                "Sword_Patrol_Idle_01",
                "Sword_Patrol_Idle_03",
                "Sword_Patrol_Idle_04",
                "Empty_Idle_01",
                "Empty_Idle_02",
                "Empty_Idle_03",
                "Empty_Idle_04",
                "Patrol_Idle_01",
                "Patrol_Idle_02",
                "Patrol_Idle_03",
                "Patrol_Idle_04",
            ]),
            walk: lib.first(&["Sword_Patrol_WalkN", "Empty_Walk_Patrol", "Empty_WalkN", "Sword_WalkN", "Patrol_WalkN"]),
            idle_alert: lib.first(&["Sword_Idle", "Empty_Idle", "Sword_Patrol_Idle", "Attack_idle", "Patrol_Idle"]),
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
            stun: lib.first(&["Sword_HitWindblast", "Empty_HitWindblast", "Patrol_Stun_Loop"]),
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
            equip: lib.first(&["Sword_Patrol_Equip", "Sword_Equip"]),
            grab_try: lib.first(&["Empty_ArmGrab_Try"]),
            grab_in: lib.first(&["Empty_ArmGrab_In"]),
            grab_loop: lib.first(&["Empty_ArmGrab_Loop"]),
            grab_out: lib.first(&["Empty_ArmGrab_Out"]),
            grab_miss: lib.first(&["Empty_ArmGrab_MissDD"]),
            drop_killed: [
                lib.find("Generic_Assassination_DropFront_Slave"),
                lib.find("Generic_Assassination_DropBack_Slave"),
                lib.find("Generic_Assassination_DropLeft_Slave"),
                lib.find("Generic_Assassination_DropRight_Slave"),
            ],
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
    down: bool,
    attacking: bool,
    /// A one-shot (reaction, search, fidget) is playing.
    oneshot: bool,
    hit_seen: f32,
    idle_time: f32,
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
}

impl NpcAnim {
    /// The weapon meshes in the character's hand or holster.
    pub fn swords(&self) -> &[Entity] {
        &self.sword
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
    mut last: Local<std::collections::HashMap<Entity, Option<ClipId>>>,
) {
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for (e, npc, anim, t, scripted) in &npcs {
        if t.translation.distance(ppos) > 8.0 {
            continue;
        }
        let cur = anim.current().map(|p| p.clip);
        if last.get(&e) != Some(&cur) {
            last.insert(e, cur);
            info!(
                "npc clip: {} -> {} (mode {:?} alert {:?} hit {:.2} stagger {:.2} attack {:?} scripted {scripted})",
                npc.name,
                cur.map(|c| anim.lib.clip(c).name.as_str()).unwrap_or("-"),
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
    mut npcs: Query<(Entity, &Npc, &mut NpcAnim, &mut Animator, &Transform, &Children, Option<&DropKilled>, Option<&AssassinClip>, Option<&mut FinisherClip>), Without<ScriptedAnim>>,
    mut visuals: Query<&mut Transform, (With<NpcVisual>, Without<Npc>, Without<Player>)>,
) {
    let dt = time.delta_secs() * tc.world_scale().max(0.0);
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for (e, npc, mut st, mut anim, t, children, dropped, assassin, finisher) in &mut npcs {
        anim.time_scale = tc.world_scale().max(0.0);
        // the clips carry the whole body motion (falls included)
        for c in children.iter() {
            if let Ok(mut vt) = visuals.get_mut(c) {
                let face = Quat::from_rotation_y(FRAC_PI_2);
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

        // sword: drawn when alerted, holstered otherwise
        let drawn = npc.alert != Alert::Unaware
            || matches!(npc.mode, Mode::Combat | Mode::Search | Mode::Investigate)
            || npc.is_down()
            || npc.mode == Mode::Choked;
        if drawn != st.drawn && !st.sword.is_empty() {
            if let Some((j, at)) = if drawn { st.hand } else { st.holster.or(st.hand) } {
                for &s in &st.sword {
                    commands.entity(s).try_insert((at, ChildOf(j)));
                }
            }
            st.drawn = drawn;
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

        if npc.is_down() {
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
                    (pick(&clips.death), None)
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
                let c = match phase {
                    GrabPhase::Try => clips.grab_try,
                    GrabPhase::Hold => clips.grab_in.or(clips.grab_loop),
                    GrabPhase::Release => clips.grab_out,
                    GrabPhase::Miss => clips.grab_miss.or(clips.grab_out),
                };
                if let Some(c) = c {
                    anim.restart(c, false, 1.0, 0.1);
                }
            } else if phase == GrabPhase::Hold && anim.finished() {
                if let Some(c) = clips.grab_loop {
                    anim.play(c, true, 1.0, 0.1);
                }
            }
            st.last_mode = npc.mode;
            st.last_alert = npc.alert;
            continue;
        }
        st.grab_seen = None;
        // melee swing: the move's clip, fitted to its duration
        if npc.attack_t.is_some() {
            if !st.attacking {
                st.attacking = true;
                let (kind, big, len) = npc.swing.as_ref().map(|s| (s.kind, s.big, s.len)).unwrap_or((MoveKind::Short, false, 0.95));
                let own = match kind {
                    MoveKind::Short if big => pick(&clips.attack_short_big).or_else(|| pick(&clips.attack_short)),
                    MoveKind::Short => pick(&clips.attack_short).or_else(|| pick(&clips.attack_short_big)),
                    MoveKind::Medium => pick(&clips.attack_medium),
                    MoveKind::Long if big => pick(&clips.attack_long_big).or_else(|| pick(&clips.attack_long)),
                    MoveKind::Long => pick(&clips.attack_long).or_else(|| pick(&clips.attack_long_big)),
                    MoveKind::Bash => clips.kick,
                    MoveKind::Jump => clips.jump_attack,
                    MoveKind::Left90 => pick(&clips.attack_turn[0]),
                    MoveKind::Right90 => pick(&clips.attack_turn[1]),
                    MoveKind::Left180 => pick(&clips.attack_turn[2]),
                    MoveKind::Right180 => pick(&clips.attack_turn[3]),
                };
                if let Some(c) = own.or_else(|| pick(&clips.attack)) {
                    anim.restart(c, false, lib.duration(c) / len, 0.08);
                }
            }
            st.last_mode = npc.mode;
            st.last_alert = npc.alert;
            continue;
        }
        st.attacking = false;

        // reactions
        if npc.hit_react > st.hit_seen + 1e-3 {
            // the reactions of the hands it has: sword drawn, or empty
            let pool = if (st.drawn && !st.sword.is_empty() || clips.hit_empty.is_empty()) && !clips.hit.is_empty() { &clips.hit } else if clips.hit_empty.is_empty() { &clips.hit } else { &clips.hit_empty };
            let c = if npc.stagger > 1.0 { clips.stun.or(pick(pool)) } else { pick(pool) };
            if let Some(c) = c {
                anim.restart(c, false, 1.3, 0.06);
                st.oneshot = true;
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
                _ => &clips.push,
            };
            if let Some(c) = pick(pool) {
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
        if npc.ambush.is_some() {
            if anim.finished() {
                if let Some(c) = pick(&clips.ambush[1]) {
                    anim.restart(c, true, 1.0, 0.1);
                }
            }
            continue;
        }
        if npc.alert == Alert::Combat && st.last_alert != Alert::Combat {
            if let Some(c) = clips.equip.or(clips.surprised) {
                anim.restart(c, false, 1.4, 0.15);
                st.oneshot = true;
            }
        } else if npc.mode == Mode::Search && st.last_mode != Mode::Search {
            if let Some(c) = pick(&clips.search) {
                anim.restart(c, false, 1.0, 0.25);
                st.oneshot = true;
            }
        }
        st.last_mode = npc.mode;
        st.last_alert = npc.alert;
        let sp = npc.anim_speed;
        if st.oneshot && !anim.finished() && (sp < 0.6 || npc.stagger > 0.0) {
            continue;
        }
        st.oneshot = false;

        // locomotion, played at the rate that matches the ground speed
        let alert = npc.alert != Alert::Unaware || matches!(npc.mode, Mode::Combat | Mode::Search | Mode::Investigate);
        if sp < 0.3 {
            st.idle_time += dt;
            // occasional fidget while standing around unaware
            if !alert && st.idle_time > 8.0 && rand::random::<f32>() < dt * 0.1 {
                if let Some(c) = pick(&clips.idle_fidgets) {
                    anim.restart(c, false, 1.0, 0.3);
                    st.oneshot = true;
                    st.idle_time = 0.0;
                    continue;
                }
            }
            let c = if alert { clips.idle_alert.or(clips.idle) } else { clips.idle };
            if let Some(c) = c {
                anim.play(c, true, 1.0, 0.3);
            }
        } else {
            st.idle_time = 0.0;
            let c = if npc.mode == Mode::Flee {
                clips.flee.or(clips.run)
            } else if sp > (npc.gait[1] * 1.3 + npc.gait[2]) * 0.5 {
                clips.run.or(clips.walk_alert)
            } else if alert {
                clips.walk_alert.or(clips.walk)
            } else {
                clips.walk
            };
            if let Some(c) = c {
                let natural = lib.root_speed(c).max(0.3);
                anim.play(c, true, (sp / natural).clamp(0.5, 1.8), 0.25);
            }
        }
    }
}

fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}
