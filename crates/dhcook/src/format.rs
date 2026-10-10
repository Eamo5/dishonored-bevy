//! Cooked cache format shared between the cooker and the game.
//! All spatial data is already converted to Bevy space (Y-up, right-handed, meters).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

// 102: collect player cinematic animation sets after creating the arms rig.
pub const SCENE_VERSION: u32 = 102;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Scene {
    /// The level's packages (the persistent level first) and what each holds: the level
    /// scripts stream some in and out (`LevelStreamingKismet`: the Hound Pits' scripts for
    /// each return from a mission).
    #[serde(default)]
    pub levels: Vec<LevelRange>,
    /// The level's music director settings.
    #[serde(default)]
    pub music: Option<MusicEvents>,
    /// How visible Corvo is (`Twk_PlayerVisibility`, the map's `m_pMapPlayerVisSettings`).
    #[serde(default)]
    pub player_vis: Option<PlayerVis>,
    /// Corvo's health effects (his combat tweak's `m_HealthEffects`)
    #[serde(default)]
    pub health_fx: Vec<HealthFx>,
    /// the lens effect as his eyes leave the water (`m_pWaterExitEffectTweaks`: particle
    /// system, life)
    #[serde(default)]
    pub water_exit_lens: Option<(u32, f32)>,
    /// Cube maps: the six face textures (+X, -X, +Y, -Y, +Z, -Z).
    #[serde(default)]
    pub cubes: Vec<[u32; 6]>,
    /// The level's reflection cube (`WorldInfo.mSceneReflection`, the shaders'
    /// `SceneReflectionTexture`; index into `cubes`).
    #[serde(default)]
    pub scene_reflection: Option<u32>,
    /// AI voices (barks).
    #[serde(default)]
    pub barks: Vec<BarkVoice>,
    /// Precomputed light volume samples (Lightmass), relative to the cache root.
    #[serde(default)]
    pub light_volume: Option<String>,
    /// The swarm rat character (index into `npc_types`).
    #[serde(default)]
    pub rat_type: Option<u32>,
    #[serde(default)]
    pub white_rat_material: Option<u32>,
    /// Security systems: walls of light, arc pylons, watchtowers, alarm bells and the whale
    /// oil receptacles and tanks that power them.
    #[serde(default)]
    pub security: Vec<Security>,
    /// Rat swarm spawners (`DisRatSpawner`): where the levels' rats come from
    #[serde(default)]
    pub rat_spawners: Vec<RatSpawner>,
    /// each NPC faction's enemies (`DisTweaks_Faction.m_EnemyFactions`)
    #[serde(default)]
    pub factions: std::collections::BTreeMap<String, Vec<String>>,
    /// loadouts the level's scripts apply (`DisSeqAct_ApplyPlayerLoadout`)
    #[serde(default)]
    pub loadouts: Vec<LoadoutDef>,
    /// Corvo's arms before the Outsider's mark: the arms' materials with the untattooed skin
    /// (parallel to the player_arms type's `body_materials`)
    #[serde(default)]
    pub arms_no_mark: Vec<u32>,
    /// Door sounds by door actor name.
    #[serde(default)]
    pub doors: std::collections::HashMap<String, DoorSounds>,
    pub version: u32,
    pub name: String,
    pub packages: Vec<String>,
    pub meshes: Vec<MeshRef>,
    pub textures: Vec<TextureRef>,
    pub materials: Vec<MaterialDef>,
    pub instances: Vec<Instance>,
    pub lights: Vec<Light>,
    pub player_starts: Vec<PlayerStart>,
    pub spawners: Vec<Spawner>,
    pub routes: Vec<Route>,
    pub pickups: Vec<Pickup>,
    pub volumes: Vec<Volume>,
    pub kill_y: f32,
    pub fog: Option<Fog>,
    /// every fog layer of the map
    #[serde(default)]
    pub fog_layers: Vec<FogLayer>,
    /// the river krusts (`DisRiverKrust`)
    #[serde(default)]
    pub krusts: Vec<Krust>,
    /// the hagfish of the flooded streets (`DisFish`)
    #[serde(default)]
    pub fish: Vec<Fish>,
    /// the doors' keyholes (the `KeyHole` socket of a `DisDoor`'s mesh)
    #[serde(default)]
    pub keyholes: Vec<Keyhole>,
    /// physics props and breakables (`DishonoredMovable`, `DishonoredBreakableNavBlock`)
    #[serde(default)]
    pub movables: Vec<Movable>,
    /// taps and fountains (`DisWaterSource`)
    #[serde(default)]
    pub water_sources: Vec<WaterSource>,
    /// the Overseers' protective tunes played from the level (`DisProtectionTuneSource`)
    #[serde(default)]
    pub tune_sources: Vec<TuneSource>,
    /// the walkable polygons (the levels' `NavigationMeshBase`s)
    #[serde(default)]
    pub navmesh: NavMesh,
    /// lockers, chests, bins, hatches, levers, valves... (`DishonoredUsableObject`)
    #[serde(default)]
    pub usables: Vec<UsableObj>,
    /// tripwires and the launchers they set off (`DisTripwire`, `DisProjectileLauncher`)
    #[serde(default)]
    pub traps: Vec<Trap>,
    /// places rats keep away from (`DisRatRepulsor`)
    #[serde(default)]
    pub rat_repulsors: Vec<RatRepulsor>,
    /// the AI's places: guards' watch points, flee points, hideouts' access points, ambush
    /// points
    #[serde(default)]
    pub ai_markers: AiMarkers,
    /// the rooms and doorways of the sound propagation (`DishonoredAudioVolume`,
    /// `DishonoredAudioPortal`)
    #[serde(default)]
    pub reflections: Vec<Reflection>,
    #[serde(default)]
    pub lens_flares: Vec<LensFlareDef>,
    /// the Dunwall City Trials' scoring rule sets the level scripts use
    #[serde(default)]
    pub challenge_rules: Vec<RulesetDef>,
    #[serde(default)]
    pub audio_cells: Vec<AudioCell>,
    #[serde(default)]
    pub audio_portals: Vec<AudioPortal>,
    /// places characters stop at on their rounds (`DisNPCDistractor`)
    #[serde(default)]
    pub distractors: Vec<Distractor>,
    #[serde(default)]
    pub skeletons: Vec<SkeletonDef>,
    #[serde(default)]
    pub npc_types: Vec<NpcType>,
    /// Named gameplay meshes (weapons etc.) taken from the shared packages.
    #[serde(default)]
    pub props: Vec<PropDef>,
    /// Named gameplay particle effects (explosions, impacts, muzzle flashes): name and
    /// index into `particle_systems`
    #[serde(default)]
    pub effects: Vec<(String, u32)>,
    /// Named textures gameplay effects sample (Dark Vision's noise, the eyelid gradient):
    /// name and texture id
    #[serde(default)]
    pub game_textures: Vec<(String, u32)>,
    /// What interactable things are drawn over with while in reach and looked at (their tweaks'
    /// `m_HighLightMaterial`: `vfx_interactivity.interactivity_INST`, the golden rim)
    #[serde(default)]
    pub highlight_material: Option<u32>,
    /// The post-process graph's materials by name (`scene.materials`): vector fields,
    /// compositions, screen effects
    #[serde(default)]
    pub post_materials: Vec<(String, u32)>,
    /// Corvo's first-person clips' particle notifies (`AnimNotify_PlayParticleEffect`: the
    /// Mark's glow as he casts)
    #[serde(default)]
    pub arm_fx: Vec<ArmFx>,
    /// the stretches of Corvo's clips played in slow motion (the finishers')
    #[serde(default)]
    pub arm_bend: Vec<ArmBend>,
    /// a blade in flesh (`Dis_ContactSystem`'s sword against a body): its blood particle system,
    /// and the blood on the lens with how near (m) and how likely
    #[serde(default)]
    pub blade_blood: Option<BladeBlood>,
    /// the characters' clips that sever a limb (`DishonoredNotify_SeverLimb`: the beheading
    /// finishers)
    #[serde(default)]
    pub npc_severs: Vec<NpcSever>,
    /// a severed limb's blood (`DisSeveredLimbInfo` `SLInfo_Default`)
    #[serde(default)]
    pub severed_limbs: Option<SeveredLimbs>,
    /// The post-process materials' parameter curves over time, by `post_materials` name
    /// (`MaterialInstanceTimeVarying`: the possession's eye opening and closing)
    #[serde(default)]
    pub post_curves: Vec<(String, Vec<PostCurve>)>,
    /// Animation sets (`anims/*.anim`) used by the NPC types.
    #[serde(default)]
    pub anim_sets: Vec<AnimSetRef>,
    /// The level scripts (Kismet sequences of the persistent level and its sublevels).
    #[serde(default)]
    pub kismet: Kismet,
    /// Arkane post-processing of the level (`WorldInfo.m_ArkDefaultPpSettings`).
    #[serde(default)]
    pub post: PostProcess,
    /// Cascade particle systems and the emitters placed in the level.
    #[serde(default)]
    pub particle_systems: Vec<ParticleSystemDef>,
    #[serde(default)]
    pub particles: Vec<ParticleInstance>,
    /// Wwise ambient sound emitters placed in the level
    #[serde(default)]
    pub ambient_sounds: Vec<AmbientSound>,
}

impl Scene {
    /// A door's data by its actor's name and an instance of it (sublevels may share a name:
    /// the later ones are keyed `name@package`).
    pub fn door(&self, actor: &str, instance: u32) -> Option<&DoorSounds> {
        self.levels
            .iter()
            .find(|l| instance >= l.instances.0 && instance < l.instances.1)
            .and_then(|l| self.doors.get(&format!("{actor}@{}", l.name)))
            .or_else(|| self.doors.get(actor))
    }
}

/// An `AkAmbientSound` actor: a Wwise event played at a position.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AmbientSound {
    /// Wwise event name (its id is the FNV hash of the lower-cased name)
    pub event: String,
    pub position: [f32; 3],
    /// plays from level start (else started by the level scripts)
    pub auto_play: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ParticleSystemDef {
    pub name: String,
    pub emitters: Vec<EmitterDef>,
}

/// A cooked UE3 raw distribution: lookup table sampled at `(t - start) * scale`; each entry
/// holds `chunk` floats (values, or minimums then maximums for random ops).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Dist {
    /// 1 none, 2 random (uniform between minimums and maximums), 3 extreme (one of them)
    pub op: u8,
    pub chunk: u8,
    pub scale: f32,
    pub start: f32,
    pub table: Vec<f32>,
}

/// A particle module: its class, distributions, and plain values (bools as 0/1).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ModuleDef {
    pub class: String,
    pub dists: std::collections::BTreeMap<String, Dist>,
    pub values: std::collections::BTreeMap<String, f32>,
    pub names: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EmitterDef {
    pub name: String,
    pub material: u32,
    /// mesh particles (`ParticleModuleTypeDataMesh`)
    pub mesh: Option<u32>,
    /// the mesh's fixed turn (its type data's Pitch / Yaw / Roll), a Bevy-space quaternion
    #[serde(default)]
    pub mesh_rotation: Option<[f32; 4]>,
    pub required: ModuleDef,
    pub spawn: ModuleDef,
    /// (time, count) bursts
    pub bursts: Vec<(f32, u32)>,
    pub modules: Vec<ModuleDef>,
    /// the events it sends the level scripts (`ParticleModuleEventGenerator`: 0 a particle
    /// spawned, 1 one died, 2 one hit something) by name (`SeqEvent_ParticleEvent`'s outputs)
    #[serde(default)]
    pub events: Vec<(u8, String)>,
}

/// A particle system placed in the level (Bevy space).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ParticleInstance {
    pub system: u32,
    pub transform: [f32; 16],
    pub active: bool,
    /// metres, 0 = unlimited
    pub max_draw: f32,
}

/// Arkane post-process parameters (defaults from `Default__WorldInfo`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct PostProcess {
    /// colour balance (cyan-red, magenta-green, yellow-blue) per tonal range
    pub shadows: [f32; 3],
    pub midtones: [f32; 3],
    pub highlights: [f32; 3],
    pub balance_opacity: f32,
    pub pre_desaturation: f32,
    pub post_desaturation: f32,
    pub exposure: f32,
    pub gamma: f32,
    pub film_grain: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub bloom: bool,
    pub bloom_tint: [f32; 3],
    pub bloom_threshold: f32,
    pub bloom_scale: f32,
    /// depth of field (metres)
    pub focus_distance: f32,
    pub in_focus_radius: f32,
    pub far_blur: f32,
}

impl Default for PostProcess {
    fn default() -> Self {
        PostProcess {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            balance_opacity: 1.0,
            pre_desaturation: 0.0,
            post_desaturation: 0.0,
            exposure: 0.0,
            gamma: 1.0,
            film_grain: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            bloom: true,
            bloom_tint: [1.0; 3],
            bloom_threshold: 0.0,
            bloom_scale: 1.0,
            focus_distance: 10.0,
            in_focus_radius: 3.0,
            far_blur: 0.0,
        }
    }
}

/// A package of the level and the ranges of the scene's lists it filled.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LevelRange {
    pub name: String,
    /// loaded by the level scripts (else always)
    pub streamed: bool,
    pub instances: (u32, u32),
    pub spawners: (u32, u32),
    pub pickups: (u32, u32),
    /// its Kismet ops (`kismet.ops`)
    pub ops: (u32, u32),
    /// its volumes (`scene.volumes`: water, blocking, triggers)
    #[serde(default)]
    pub volumes: (u32, u32),
}

/// Kismet sequence graphs: ops (events, actions, conditions, sub-sequences) and variables,
/// with their properties kept generically, plus the level actors they refer to.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Kismet {
    pub ops: Vec<KOp>,
    pub vars: Vec<KVar>,
    pub actors: Vec<KActor>,
    /// Mission objectives and their tasks, by object path.
    #[serde(default)]
    pub objectives: Vec<KObjective>,
    /// Dialogue actors (`DisDialogOneShot`, `DisSeqAct_DialogInputs` targets) and their trees.
    #[serde(default)]
    pub dialogs: Vec<KDialog>,
    /// Every dialogue tree of the levels (the one-shot actors' and the characters', which
    /// `DisSeqAct_DialogInputs` name in `m_pDialogTree`).
    #[serde(default)]
    pub dialog_trees: Vec<KDialogTree>,
    /// Matinee sequences (`InterpData` variables carry a "Matinee" index).
    #[serde(default)]
    pub matinees: Vec<KMatinee>,
    /// each package's ops (first, end), in package order
    #[serde(default)]
    pub level_ops: Vec<(u32, u32)>,
}

/// A matinee (UE3 InterpData): groups of keyframed tracks played on the actors linked to
/// the `SeqAct_Interp` by group name. Values are in UE units (positions) and degrees.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KMatinee {
    pub length: f32,
    pub groups: Vec<KInterpGroup>,
    /// director cuts: (time, camera group name)
    pub cuts: Vec<(f32, String)>,
    /// director fade (amount: 0 clear .. 1 black)
    pub fade: Vec<KCurvePoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KInterpGroup {
    pub name: String,
    pub tracks: Vec<KTrack>,
    /// scripted scenes (Arkane "soirees"): the group marking where the character performs
    #[serde(default)]
    pub stage_mark: String,
    /// the player's group (`InterpGroupPlayer`): Corvo, whatever the bindings
    #[serde(default)]
    pub player: bool,
    /// the animation sets its animations come from (`GroupAnimSets`, object paths)
    #[serde(default)]
    pub anim_sets: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum KTrack {
    /// position / rotation (euler degrees: roll, pitch, yaw) curves; relative to the actor's
    /// initial placement unless `relative` is false (world)
    Move { pos: Vec<KCurvePoint>, rot: Vec<KCurvePoint>, relative: bool },
    /// fire the `SeqAct_Interp` output named after the event
    Event(Vec<(f32, String)>),
    /// Wwise events
    Sound(Vec<(f32, String)>),
    /// show / hide the actor
    Visibility(Vec<(f32, bool)>),
    /// switch the actor (lights, particles) on / off
    Toggle(Vec<(f32, bool)>),
    /// a float property (e.g. a camera's FOVAngle)
    Float { prop: String, points: Vec<KCurvePoint> },
    /// skeletal animations: (start time, sequence, start offset, play rate, looping)
    Anim(Vec<(f32, String, f32, f32, bool)>),
    /// spoken lines: (time, Wwise event, subtitle)
    Dialog(Vec<(f32, String, String)>),
    /// a character turns to face a target (group name, or "Player"): (start, length, target)
    FaceTo(Vec<(f32, f32, String)>),
    /// a character walks to a target: (start, length, gait ("Walk", "Run"...), target)
    Locomotion(Vec<(f32, f32, String, String)>),
    /// a character looks at a target: (start, length, target)
    LookAt(Vec<(f32, f32, String)>),
    /// the actor rides a bone of the target group's character: keys (time, attach (else
    /// detach), bone, offset (UE units), rotation (UE rotator))
    Attach { target: String, keys: Vec<(f32, bool, String, [f32; 3], [i32; 3])> },
    /// a soiree's sections (`InterpTrackSoireeControl`): (start, length, pin name) - a
    /// distraction repeats its "Loop" so many times
    Pins(Vec<(f32, f32, String)>),
}

/// A curve key (UE3 FInterpCurvePoint); floats use `v[0]`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct KCurvePoint {
    pub t: f32,
    pub v: [f32; 3],
    pub arrive: [f32; 3],
    pub leave: [f32; 3],
    /// 0 linear, 1 curve, 2 constant
    pub mode: u8,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KDialog {
    /// index into `actors`
    pub actor: u32,
    /// index into `dialog_trees`
    pub tree: u32,
}

/// A dialogue tree: its conversations, and the graph of hooks (Kismet inputs, the player
/// approaching or loitering...) and conditions (story flags, conversations already had,
/// branches) that picks one.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KDialogTree {
    /// object path (`Dlg_Havelock.DialogTree.Dlg_Havelock_HUB`)
    pub path: String,
    pub conversations: Vec<KConversation>,
    pub nodes: Vec<KDialogNode>,
}

/// A node of a dialogue tree's graph and its outputs (node indices, -1 unlinked).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KDialogNode {
    pub kind: KDialogNodeKind,
    pub outs: Vec<i32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum KDialogNodeKind {
    /// an entry (`DDH_KISMET_ACTIVATED` with its input names, one per output;
    /// `DDH_PLAYER_APPROACHES`, `DDH_PLAYER_LOITERS`...)
    Hook { hook: String, inputs: Vec<String> },
    /// a conversation (index into the tree's)
    Conversation(u32),
    /// a story flag: output 0 when set, 1 when not
    StoryFlag(String),
    SetStoryFlag(String),
    /// a conversation (object path) already played: output 0, else 1
    Fired(String),
    /// at most once per this many seconds: output 0, else 1
    TimeLimit(f32),
    Random,
    Sequential,
    /// activate a Kismet remote event
    Remote(String),
    /// anything else passes on through its first output
    Pass,
}

/// A conversation flattened along its first links.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KConversation {
    pub label: String,
    pub steps: Vec<KDialogStep>,
    /// object path (for "conversation fired" conditions)
    #[serde(default)]
    pub path: String,
    /// plays once only (`m_bFireConvOnlyOnce`)
    #[serde(default)]
    pub once: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum KDialogStep {
    /// subtitle text and speaker slot
    Line(String, i32),
    /// the voice-over event of the following line (Wwise event name)
    Voice(String),
    /// play the matinee (`SeqAct_Interp` with this `m_MatineeGUID`)
    Matinee(String),
    /// activate a Kismet remote event
    Remote(String),
    /// set a story flag
    SetFlag(String),
    /// the player answers (`DisConv_PlayerChoice`): each option's text and the step its
    /// branch starts at (in this list)
    Choice(Vec<(String, u32)>),
    /// the end of a branch
    End,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KObjective {
    pub path: String,
    pub text: String,
    pub tasks: Vec<KTask>,
    /// an optional objective (`m_bOptional`: its markers are `objectiveMarker_secondary`), one
    /// whose tasks get no markers (`m_bShowHUDMarkers` off)
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub no_markers: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KTask {
    pub path: String,
    pub text: String,
    pub hidden: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KOp {
    pub class: String,
    /// Object name, e.g. `SeqAct_Delay_3`.
    pub name: String,
    /// Enclosing `Sequence` op.
    pub parent: Option<u32>,
    pub inputs: Vec<KInput>,
    pub outputs: Vec<KOutput>,
    pub vars: Vec<KLink>,
    pub props: std::collections::BTreeMap<String, KVal>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KInput {
    pub desc: String,
    /// For sub-sequences: the inner `SequenceActivated` event this input fires.
    pub linked: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KOutput {
    pub desc: String,
    /// (op, input index, activation delay in seconds)
    pub links: Vec<(u32, u32, f32)>,
    /// For sub-sequences: the inner `FinishSequence` action that fires this output.
    pub linked: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KLink {
    pub desc: String,
    pub vars: Vec<u32>,
    /// Output variable link (written by the op).
    pub write: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KVar {
    pub class: String,
    /// `VarName`
    pub name: String,
    pub parent: Option<u32>,
    pub props: std::collections::BTreeMap<String, KVal>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum KVal {
    Bool(bool),
    Int(i32),
    Float(f32),
    Str(String),
    Vec3([f32; 3]),
    /// Index into `Kismet::actors`.
    Actor(u32),
    Op(u32),
    Var(u32),
    List(Vec<KVal>),
}

/// A rat swarm spawner and its swarm's tweak (`DisTweaks_RatSwarm`: detection and escape
/// radii in UE units, rats needed to attack, bite damage, roaming radius...).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RatSpawner {
    pub name: String,
    pub position: [f32; 3],
    /// rats it brings and how far around it (metres)
    pub count: u32,
    pub radius: f32,
    pub begin_play: bool,
    /// `Twk_RatsSpawner_Repopulate`, `_LargeRoam`, `_Scripted`
    pub tweak: String,
    pub params: std::collections::BTreeMap<String, f32>,
}

/// A level actor referenced by the scripts and what it became in the scene.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KActor {
    pub name: String,
    pub class: String,
    pub position: [f32; 3],
    pub yaw: f32,
    pub instances: Vec<u32>,
    pub lights: Vec<u32>,
    pub spawner: Option<u32>,
    pub pickup: Option<u32>,
    pub volume: Option<u32>,
    pub start: Option<u32>,
    #[serde(default)]
    pub particles: Vec<u32>,
    #[serde(default)]
    pub sound: Option<u32>,
    /// index into `scene.rat_spawners`
    #[serde(default)]
    pub rat_spawner: Option<u32>,
    /// UE rotator (pitch, yaw, roll; 65536 per turn)
    #[serde(default)]
    pub rotation: [i32; 3],
    /// UE location (for matinee tracks, which work in UE space)
    #[serde(default)]
    pub ue_location: [f32; 3],
    /// camera actors: field of view (degrees)
    #[serde(default)]
    pub fov: f32,
    /// a fog actor: its layer (`scene.fog_layers`)
    #[serde(default)]
    pub fog: Option<u32>,
    /// a river krust (`scene.krusts`)
    #[serde(default)]
    pub krust: Option<u32>,
    /// a tripwire or a launcher (`scene.traps`)
    #[serde(default)]
    pub trap: Option<u32>,
    /// a physics burst (`RB_RadialImpulseActor`'s component): its reach (m), strength (UE
    /// units), whether it sets speeds outright (`bVelChange`) and fades with distance
    /// (`ImpulseFalloff` linear, else constant)
    #[serde(default)]
    pub impulse: Option<[f32; 4]>,
    /// an animated usable object (`scene.usables`)
    #[serde(default)]
    pub usable: Option<u32>,
    /// a prop the matinees animate (a gangway lowering, a machine): its animated type
    /// (`scene.npc_types`, kind "device")
    #[serde(default)]
    pub device: Option<u32>,
    /// its tweak and those it falls back on (`m_FallbackChainCooked`), as object paths: what
    /// `DisSeqCond_CompareTweaks` calls related (the Prison's sword is an
    /// `InventoryPickupSwordBase`)
    #[serde(default)]
    pub tweaks: Vec<String>,
    /// the instances of actors attached to it (`Base`), which move with it: a gate's crusher
    #[serde(default)]
    pub riders: Vec<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AnimSetRef {
    /// AnimSet object path
    pub name: String,
    pub file: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PropDef {
    pub name: String,
    pub mesh: u32,
    pub materials: Vec<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SkeletonDef {
    pub name: String,
    pub bones: Vec<BoneDef>,
    /// The mesh's attachment sockets.
    #[serde(default)]
    pub sockets: Vec<SocketDef>,
}

/// An attachment point relative to a bone (Bevy space).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SocketDef {
    pub name: String,
    pub bone: String,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
}

/// Bind-pose bone, local to its parent, in Bevy space.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BoneDef {
    pub name: String,
    pub parent: i32,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NpcType {
    /// Pawn tweak archetype path, e.g. "Pwn_EliteGuard_MTall_2.Pwn_EliteGuard_MTall_2".
    pub name: String,
    /// guard / overseer / thug / civilian / story / creature
    pub kind: String,
    pub skeleton: Option<u32>,
    pub body: Option<u32>,
    pub body_materials: Vec<u32>,
    pub head: Option<u32>,
    pub head_materials: Vec<u32>,
    /// Indices into `Scene::anim_sets`, in the pawn's AnimSets order.
    #[serde(default)]
    pub anim_sets: Vec<u32>,
    /// The voices this pawn may speak with (indices into `Scene::barks`).
    #[serde(default)]
    pub voices: Vec<u32>,
    /// Its faction (`Faction_Guard_Default`...) and whether that faction counts Corvo's
    /// among its enemies (`m_EnemyFactions`).
    #[serde(default)]
    pub faction: String,
    #[serde(default)]
    pub hostile: bool,
    /// Story group (`Twk_ID_CityGuard`): what the Heart says about them, script conditions.
    #[serde(default)]
    pub story_group: String,
    /// What Corvo becomes possessing one (`m_pPossessableTweaks`).
    #[serde(default)]
    pub possess: Option<Possessable>,
    /// the meshes' material slots (scene materials): what the scripts' new materials replace
    /// (`DisSeqAct_NPCSetMaterials` names slots)
    #[serde(default)]
    pub body_slots: Vec<u32>,
    #[serde(default)]
    pub head_slots: Vec<u32>,
    /// its face (`FaceFXAsset` of the head mesh): index into the map's `FaceFx::actors`
    #[serde(default)]
    pub facefx: Option<u32>,
    /// how it sees and notices (its vision and attention tweaks)
    #[serde(default)]
    pub sight: Option<Sight>,
    /// what it can take and deal (its attributes and arms)
    #[serde(default)]
    pub stats: Option<NpcStats>,
    /// what it carries on its sockets (a tallboy's tanks)
    #[serde(default)]
    pub attachments: Vec<NpcAttachment>,
}

/// A character's numbers and arms. Its attributes are those of its pawn's attribute tweak
/// (`m_pAttributeTweaks[1]`: of the four slots only the Normal one is authored, its
/// `DisAttribute`s holding the four difficulties), by name without `m_` (`HealthMax`,
/// `ParryChanceOfStarting`...): easy, normal, hard, very hard.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NpcStats {
    pub attributes: std::collections::BTreeMap<String, [f32; 4]>,
    /// what it carries (`m_ContentInventoryLoadout`)
    pub weapons: Vec<NpcWeapon>,
    /// its ammunition (`m_Ammo`): type (`Bullet`, `Arrow`, `Grenade`...) and count
    pub ammo: Vec<(String, i32)>,
    /// the crosshair's line over it asleep (`m_AsleepCrosshairText`: "(Unconscious)")
    pub asleep_text: String,
    /// its combat tweak: the damage of a hit it reacts to and of a big one
    /// (`m_HitReact_Min_Damage`, `m_HitReact_High_Damage`), and what a rat swarm eating it does
    /// a second (`m_fRatSwarmDamagePerSecond`)
    pub hit_react: [f32; 2],
    pub rat_dps: f32,
    /// the ways it throws grenades (an overseer's)
    #[serde(default)]
    pub grenades: Vec<GrenadeThrow>,
    /// its blade's moves
    #[serde(default)]
    pub melee: Vec<MeleeMove>,
    /// a music box's tunes (`DisTweaks_WepMusicBox`'s `DisTweaks_NPCTune_Combat` and
    /// `DisTweaks_NPCTune_Protection`)
    #[serde(default)]
    pub tunes: Option<NpcTunes>,
}

/// One of a character's blade moves (its weapon's NPC contexts: `DisTweaks_NPCAttackShort`,
/// `..Medium`, `..Long`, `DisTweaks_NPCBash`): its reach, the angle off its facing it needs,
/// its cooldown, how far ahead it leads its target (`m_fPredictionTime`: a lunge), its chance
/// of a big hit.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MeleeMove {
    /// `Short`, `Medium`, `Long`, `Bash`, `Jump` (a charge), the turning blows at someone to
    /// its side or behind (`Left90`, `Right90`, `Left180`, `Right180`: `angle` about that
    /// side); or a dodge, `SideStep`, `BackStep`; a counter after its parry, `Riposte`; a shove
    /// at someone in its way, `Push`; lying in wait round a corner, `Ambush`
    pub kind: String,
    /// metres: least, most, ideal; and the most height between them (`m_fMaxContextHeightDifference`)
    pub range: [f32; 3],
    pub height: f32,
    /// degrees (`m_fAllowedAngle`)
    pub angle: f32,
    pub cooldown: [f32; 2],
    pub lead: f32,
    pub big: f32,
    /// a dodge's step (m: `m_fSideStepDistance`, `m_fBackStepDistance`)
    #[serde(default)]
    pub step: f32,
}

/// A musical Overseer's tunes.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NpcTunes {
    /// the hurting tune (`DisTweaks_NPCTune_Combat`): reach (m) and allowed angle (degrees),
    /// the damage each `m_fMusicEffectPeriod` (s), the push away (`m_fMusicEffectRepulseForce`,
    /// m/s) and its start / stop sounds; it stops after Corvo is out of reach / unseen (s)
    pub combat_range: f32,
    pub combat_angle: f32,
    pub damage: f32,
    pub period: f32,
    pub repulse: f32,
    pub damage_sounds: [String; 2],
    pub stop_out_of_range: f32,
    pub stop_unseen: f32,
    /// the protective tune (`DisTweaks_NPCTune_Protection`'s `DisTweaks_Tune_Protection`):
    /// disorient and inhibit radii (m), and the sounds as Corvo crosses them (disorient start,
    /// stop, inhibit start, stop)
    pub disorient: f32,
    pub inhibit: f32,
    pub protect_sounds: [String; 4],
}

/// A way a character throws grenades: a context of its grenade weapon
/// (`DisTweaks_NPCLobGrenadeAtUnreachable`, `DisTweaks_NPC_OverseerJumpAway`,
/// `DisTweaks_NPCAttackUnder_Grenade`, `DisTweaks_NPCThrowGrenade`), and its grenade.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GrenadeThrow {
    /// the context's class
    pub kind: String,
    /// its reach (m): least, most, ideal; the least while Corvo is out of reach
    /// (`m_fMinContextRange`..., `m_fMinContextRangeWhenUnreachable`)
    pub range: [f32; 3],
    pub range_unreachable: f32,
    /// seconds between throws (`m_ContextCooldown`), and while Corvo is out of reach
    pub cooldown: [f32; 2],
    pub cooldown_unreachable: [f32; 2],
    /// the throw's speed (m/s), the clearance of a jump away (m)
    pub speed: f32,
    pub jump: f32,
    /// its grenade (`DisTweaks_GrenadeComponent`): the fuse and the most left once it hits
    /// someone (`m_fDetonationDelay`, `m_fMinDetonationDelayAfterHit`)
    pub fuse: [f32; 2],
    pub blast: TrapBlast,
}

/// A part a character carries on a socket of its skeleton (its pawn's `m_pAttachmentsTweaks`: a
/// tallboy's whale oil tanks and shields): its look (`scene.props`, by name); a breakable one's
/// health (`m_Health`), the blows that break it outright (`m_pInstantBreakDamageTypes`) and those
/// it shrugs off (`m_pImmuneToDamageTypes`), and what happens when it breaks (a tank's blast).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NpcAttachment {
    pub socket: String,
    pub prop: String,
    pub health: f32,
    pub instant: Vec<String>,
    pub immune: Vec<String>,
    pub breaks: Option<BreakStep>,
}

/// One of a character's weapons (a `DisTweaks_Wep*`): its item attribute tweak's damage by
/// difficulty (`m_pAttributeTweak[1]`), and what it fires.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NpcWeapon {
    /// the tweak (`Twk_Inv_SwordEliteGuard`) and its class (`DisTweaks_WepSword`)
    pub name: String,
    pub class: String,
    pub melee: [f32; 4],
    pub ranged: [f32; 4],
    /// its projectile (`Twk_Proj_Bullet_Elite`): the damage it carries itself (else the
    /// weapon's, times its multiplier)
    pub projectile: String,
    pub projectile_damage: f32,
    pub projectile_mult: f32,
}

/// A character's eyes (its pawn's `m_pVisionTweak`, `DisTweaks_Vision`) and how its attention
/// to Corvo rises and falls (its brain's attention process: the `DisTweaks_PawnAttention` it
/// uses while he is an enemy it doesn't yet suspect, one it suspects, or no enemy).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Sight {
    /// it takes Corvo striking the neutral for an enemy's act (`DisTweaks_AIBrain.m_bProtectNeutrals`)
    #[serde(default)]
    pub protect_neutrals: bool,
    /// focused vision: full horizontal angle (degrees), range (m), vertical scales up and down
    /// (of the half angle)
    pub inner: [f32; 4],
    /// peripheral vision, the same
    pub outer: [f32; 4],
    pub unsuspecting: Attention,
    pub suspecting: Attention,
    pub neutral: Attention,
    /// the attention level it acts on as aware of Corvo (`m_MinAttentionForAwareness`:
    /// 1 head track, 2 turn to face, 3 investigate, 4 busted)
    pub aware_level: u8,
}

/// `DisTweaks_PawnAttention`: attention (0..`max`) leaks away, seeing Corvo raises it, and its
/// level follows thresholds.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Attention {
    /// per second, out of sight (`m_fAttentionLeakRate`)
    pub leak: f32,
    pub max: f32,
    /// the levels' (head track, turn to face, investigate, busted) start and stop thresholds
    pub levels: [[f32; 2]; 4],
    /// seeing Corvo's gain scale, by difficulty (`m_SeeingPawnAttentionIncreaseScalar_ToPlayer`)
    pub see: [f32; 4],
    /// gain per second while he touches it
    pub touch: f32,
    /// what nearness adds, over 1 - distance / the cone's range (`m_DistanceValueTuning`)
    pub distance: Tuning,
    /// what hearing an anomaly, a threat, danger, a death rattle, a cry for help, an alarm
    /// gives: (amount, the level it rises to at least: 0 none .. 4 busted, 5 max)
    pub heard: [(f32, u8); 6],
    /// finding a body, being attacked
    pub corpse: (f32, u8),
    pub attacked: (f32, u8),
}

/// One of Corvo's health effects (`DisPlayerHealthEffect`): below its share of his health, a
/// looping camera lens effect, with sounds as it comes and goes (his `m_BarkCues`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct HealthFx {
    pub name: String,
    pub threshold: f32,
    pub blend_in: f32,
    pub blend_out: f32,
    /// the lens effect's particle system
    pub lens: Option<u32>,
    /// sound events as it starts and ends
    pub on: Vec<String>,
    pub off: Vec<String>,
}

/// `DisVisCalcTuningParams`: a curve (points, or `scalar * x^exponent + bias`) whose value is
/// added to or multiplies the visibility being worked out.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Tuning {
    pub add: bool,
    pub scalar: f32,
    pub bias: f32,
    pub exponent: f32,
    /// (input, output), when the points stand in for the formula
    pub points: Vec<[f32; 2]>,
}

impl Tuning {
    pub fn value(&self, x: f32) -> f32 {
        if self.points.is_empty() {
            return self.scalar * x.max(0.0).powf(self.exponent) + self.bias;
        }
        let p = &self.points;
        if x <= p[0][0] {
            return p[0][1];
        }
        for w in p.windows(2) {
            if x <= w[1][0] {
                let t = (x - w[0][0]) / (w[1][0] - w[0][0]).max(1e-6);
                return w[0][1] + (w[1][1] - w[0][1]) * t;
            }
        }
        p[p.len() - 1][1]
    }
    /// fold into the value worked out so far
    pub fn apply(&self, v: f32, x: f32) -> f32 {
        if self.add {
            v + self.value(x)
        } else {
            v * self.value(x)
        }
    }
}

/// How visible Corvo is (`DisTweaks_PlayerVisibility`): his light value (normalised over the
/// range the map, a stealth volume or the scripts set) and his speed, through their curves, plus
/// his stance's bonus.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlayerVis {
    pub light: Tuning,
    pub speed: Tuning,
    /// sneaking counts as standing still (`m_bForceZeroSpeedWhileSneaking`)
    pub sneak_still: bool,
    /// the stances' bonuses: standing, walking, sprinting, sneaking, sliding, autocrouching,
    /// swimming, swimming on the surface (`m_fVisLevelBonus_*`)
    pub bonus: [f32; 8],
    /// the light values counted as darkest and brightest: everywhere
    /// (`Twk_GlobalPlayerVisSettings`), and on this map (`m_pMapPlayerVisSettings`)
    pub global: [f32; 2],
    pub map: Option<[f32; 2]>,
}

/// The lines' facial animations and the characters' faces (`facefx/<map>.json`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FaceFx {
    /// by the line's id (its voice event `Play_<id>`)
    pub anims: std::collections::BTreeMap<String, FxAnim>,
    pub actors: Vec<FxActor>,
}

/// A line's face: curves by target (time, value, slope in, slope out; Hermite).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FxAnim {
    pub blend_in: f32,
    pub blend_out: f32,
    pub curves: Vec<(String, Vec<[f32; 4]>)>,
}

/// A character's compiled face graph (nodes in evaluation order) and face bones.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FxActor {
    pub name: String,
    pub nodes: Vec<FxNode>,
    pub bones: Vec<FxBone>,
}

/// A face graph node: its value is its curve's plus its inputs through their link functions
/// (input node, function: 1 linear (slope, offset), 2 quadratic, ...; parameters), clamped.
/// Kind 0 combines, 4 poses bones.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FxNode {
    pub name: String,
    pub kind: u32,
    pub min: f32,
    pub max: f32,
    pub op: u32,
    pub inputs: Vec<(u32, u32, Vec<f32>)>,
}

/// A face bone and how far each bone-pose node moves it (Bevy frame, local to its parent).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FxBone {
    pub name: String,
    pub links: Vec<FxBoneLink>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FxBoneLink {
    pub node: u32,
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub scale: [f32; 3],
}

/// A possessable creature's body and limits (`DisTweaks_Possessable`), in metres.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Possessable {
    /// the collision cylinder's half-height and radius
    pub height: f32,
    pub radius: f32,
    /// m/s on the ground and in the water
    pub ground_speed: f32,
    pub water_speed: f32,
    pub swim: bool,
    /// seconds by Possession level (1, 2)
    pub duration: [f32; 2],
    /// the Possession level needed
    pub level: u8,
}

/// An AI voice: what it says on each dialog hook (barks), from its `DisDialogVoiceData`
/// and the dialog tree it voices.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BarkVoice {
    pub name: String,
    pub hooks: Vec<BarkHook>,
    /// the dialogue tree the voice speaks (object path): the character's own conversations
    #[serde(default)]
    pub tree: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BarkHook {
    /// hook name without the `DDH_` prefix, e.g. "COMBAT_THREAT"
    pub hook: String,
    /// the lines it may say: (Wwise event, subtitle)
    pub lines: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MeshRef {
    pub name: String,
    pub file: String,
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// The simplified collision (`BodySetup.AggGeom` convex hulls, mesh space), which UE3
    /// collides pawns and traces with instead of the triangles (`UseSimpleBoxCollision`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub simple: Vec<Vec<[f32; 3]>>,
    /// a skinned mesh's lesser LODs, for a distance (`LODInfo.DisplayFactor`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lods: Vec<MeshLod>,
    /// its dismemberment LOD (the last, its sections the mesh's `m_MaterialsToBodyParts`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gore: Option<GoreLod>,
    /// the bounds' sphere radius (m)
    #[serde(default)]
    pub radius: f32,
}

/// A lesser LOD: its mesh, its sections' materials, its display factor.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MeshLod {
    pub mesh: u32,
    pub materials: Vec<u32>,
    pub display_factor: f32,
}

/// The dismemberment LOD: its mesh, its sections' materials and each section's body part
/// (`m_OwnerBone`: whose piece it is; `m_CutBone`: the cut it answers to; `m_bShowIfCut`: shown
/// only once cut, a stump's cap, else only while whole).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GoreLod {
    pub mesh: u32,
    pub materials: Vec<u32>,
    pub parts: Vec<(String, String, bool)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TextureRef {
    pub name: String,
    pub file: String,
    /// a render target (`TextureRenderTarget2D`: a scene capture's): its size, or the share of
    /// the screen it is (`m_ResolutionType`: `TRT_HALFSIZE` 0.5)
    #[serde(default)]
    pub render_target: Option<[f32; 3]>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Opaque,
    Masked,
    Translucent,
    Additive,
    Modulate,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MaterialDef {
    pub name: String,
    pub diffuse: Option<u32>,
    pub normal: Option<u32>,
    pub specular: Option<u32>,
    pub emissive: Option<u32>,
    pub blend: Blend,
    pub two_sided: bool,
    pub unlit: bool,
    pub alpha_cutoff: f32,
    pub tint: [f32; 4],
    pub emissive_color: [f32; 3],
    /// True for collision-only / editor helper materials that should never render.
    pub invisible: bool,
    /// Procedural sky parameters (Dishonored "Skies_PMAT" family).
    #[serde(default)]
    pub sky: Option<SkyDef>,
    /// Water surface material.
    #[serde(default)]
    pub water: bool,
    /// Opacity for translucent materials without an opacity texture.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Texture coordinate tiling of the diffuse layer (UV0).
    #[serde(default = "unit_uv")]
    pub uv_scale: [f32; 2],
    /// Tiling of the normal map (generic_PMAT tiles it independently of the diffuse).
    #[serde(default = "unit_uv")]
    pub normal_uv_scale: [f32; 2],
    /// Second diffuse layer blended in through a mask (generic_PMAT "D2" variation).
    #[serde(default)]
    pub layer: Option<LayerDef>,
    /// The original compiled material (shader map of the reference shader cache).
    #[serde(default)]
    pub ue3: Option<Ue3Material>,
    /// Physical surface (`Phm_<surface>`, e.g. "Stone"; empty = none): footsteps, impacts.
    #[serde(default)]
    pub surface: String,
}

/// A door's sounds: the notifies of its open / close animations, and the locked / unlocked
/// events of its tweaks.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DoorSounds {
    /// how much it muffles the sound through its doorway when shut, for Corvo and for the AI
    /// (its tweak's `m_fPlayerSoundOcclusion`, `m_fAISoundOcclusion`)
    #[serde(default)]
    pub occlusion: [f32; 2],
    pub open: Vec<(f32, String)>,
    pub close: Vec<(f32, String)>,
    pub locked: String,
    pub unlocked: String,
    /// locked as the level starts (`m_bLocked`), and the keys that open it (`m_MatchingKeys`:
    /// "Art Dealer's Safe Combination")
    #[serde(default)]
    pub locked_start: bool,
    /// it can be broken down (its tweak's `m_pBreakSteps` authored: a wooden door Windblast
    /// smashes); else a locked one shows `crosshair_unbreakableLockedDoor`
    #[serde(default)]
    pub breakable: bool,
    #[serde(default)]
    pub keys: Vec<String>,
    /// a breakable door's health (`m_Health`), the least blow that wears it (`m_DamageThreshold`)
    /// and what happens when it breaks (its last `m_DoorBreakSteps`: the wood's sound, the
    /// splinters, the pieces)
    #[serde(default)]
    pub health: f32,
    #[serde(default)]
    pub threshold: f32,
    #[serde(default)]
    pub breaks: Option<BreakStep>,
    /// open as the level starts (`m_InitialDoorState`: `DDS_Opened_CW` is `Some(true)`): the
    /// Prison cell's door, open for the interrogation
    #[serde(default)]
    pub open_start: Option<bool>,
}

/// Music director settings of a map (`DisTweaks_MusicEvents`, from the map info): the
/// Wwise state events for each situation and what triggers them.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MusicEvents {
    pub exploration: String,
    pub suspense: String,
    pub rat_attack: String,
    /// by combat intensity level
    pub combat: Vec<String>,
    /// near / far pursuers
    pub chase: Vec<String>,
    /// intensity thresholds of the combat levels
    pub combat_intensity: Vec<f32>,
    /// intensity each fighting enemy adds, by enemy class
    pub contribution: Vec<f32>,
    /// metres
    pub suspense_range: f32,
    pub chase_time: f32,
    /// metres, near / far
    pub chase_range: Vec<f32>,
}

/// Texture slot value of a render target (scene captures): its clear colour, black.
pub const TEX_BLACK: u32 = u32::MAX;

/// A material rendered with its original (translated) shaders.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Ue3Material {
    /// index of the shader map in the cooked shader library (`shaders.bin`)
    pub map: u32,
    /// values of the shader map's parameter nodes, in `shadercache::param_nodes` order
    pub params: Vec<[f32; 4]>,
    /// those parameters' names (what the level scripts set: `SeqAct_SetMatInstScalarParam`)
    #[serde(default)]
    pub param_names: Vec<String>,
    /// texture of each pixel `Texture2D_N` expression
    pub textures: Vec<Option<u32>>,
    /// indices into `Scene::cubes` (or `TEX_BLACK`)
    pub cube_textures: Vec<Option<u32>>,
    /// the material's own blend mode (`MaterialDef.blend` is adjusted for the fallback path)
    pub blend: Blend,
    pub two_sided: bool,
    pub unlit: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LayerDef {
    /// Variation diffuse texture (None = flat `color`).
    pub texture: Option<u32>,
    pub color: [f32; 4],
    pub tiling: f32,
    /// Blend mask texture, sampled at UV0 * `mask_tiling`, channel `mask_channel`.
    pub mask: Option<u32>,
    pub mask_channel: u32,
    pub mask_tiling: f32,
    pub mask_power: f32,
    /// Multiply the base by the variation instead of cross-fading to it.
    pub multiply: bool,
}

fn unit_uv() -> [f32; 2] {
    [1.0, 1.0]
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SkyDef {
    pub top: [f32; 3],
    pub bottom: [f32; 3],
    pub horizon: [f32; 3],
    pub horizon_intensity: f32,
    pub horizon_exponent: f32,
    pub gradient_power: f32,
    pub clouds: Option<u32>,
    pub clouds_color: [f32; 4],
    pub clouds_visibility: f32,
    pub clouds_speed: f32,
    pub clouds2: Option<u32>,
    pub clouds2_color: [f32; 4],
    pub clouds2_visibility: f32,
    pub clouds2_speed: f32,
    pub storm: Option<u32>,
    pub storm_color: [f32; 3],
    pub storm_intensity: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LightMapRef {
    /// Directional coefficient textures (UE3 stores 2-3).
    pub textures: Vec<u32>,
    pub scales: Vec<[f32; 4]>,
    pub coord_scale: [f32; 2],
    pub coord_bias: [f32; 2],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Instance {
    pub mesh: u32,
    /// Per-section material ids.
    pub materials: Vec<u32>,
    /// Column-major 4x4 world matrix.
    pub transform: [f32; 16],
    pub visible: bool,
    pub collide: bool,
    pub cast_shadow: bool,
    pub dynamic: bool,
    pub actor: String,
    pub class: String,
    pub lightmap: Option<LightMapRef>,
    /// Dominant (sun) light relevance: 0 = fully shadowed/irrelevant, 1 = use `sun_shadow`
    /// precomputed visibility, 2 = fully lit.
    #[serde(default)]
    pub sun: u8,
    #[serde(default)]
    pub sun_shadow: Option<LightMapRef>,
    /// the dominant spot / point lights' static (distance field) shadow maps on it:
    /// (light index, shadow map)
    #[serde(default)]
    pub light_shadows: Vec<(u32, LightMapRef)>,
    /// lights it is not lit by at all (`IrrelevantLights`)
    #[serde(default)]
    pub irrelevant_lights: Vec<u32>,
    /// the reflections it shows in (its component's `ReflectionChannels`, `REFLECT_*` bits)
    #[serde(default)]
    pub reflect: u16,
}

/// `RenderingChannelContainer`'s fields, as bits (`Instance::reflect`, `Reflection::channels`).
pub const REFLECT_CHANNELS: [&str; 17] = ["BSP", "Static", "Dynamic", "Skeletal", "Foliage", "Particles", "Sprites", "Decals", "Group_1", "Group_2", "Group_3", "Group_4", "Group_5", "Group_6", "Group_7", "Group_8", "Group_9"];

/// A lens flare (`LensFlareSource`: a candle's glow): its `LensFlare`'s source element, drawn
/// over the scene (`SDPG_Foreground`) facing the view, faded as it is hidden, shaded as
/// `LensFlare_PMAT` does: a round glow `max(0, 1 - 2|uv - 1/2|)^power`, dimmer away from the
/// screen's middle, in the element's colour times the material's, clamped, added.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LensFlareDef {
    pub position: [f32; 3],
    /// the element's size (m), opacity and colour (`Size` x `Scaling`, `Alpha`, `Color`)
    pub size: f32,
    pub alpha: f32,
    pub color: [f32; 3],
    /// its scale and opacity by the view's distance (UE units: `DistMap_Scale`, `DistMap_Alpha`)
    #[serde(default)]
    pub dist_scale: Option<Dist>,
    #[serde(default)]
    pub dist_alpha: Option<Dist>,
    /// the radial distance to the screen's corner (`bNormalizeRadialDistance`)
    pub normalize: bool,
    /// the material's colour (`L_LensFlare_Color` times its alpha), `L_LensFlare_Power`,
    /// `L_Radial_Distance_Factor`, opacity range (`L_Minimum_Opacity`, `L_Maximum_Opacity`)
    pub tint: [f32; 3],
    pub power: f32,
    pub radial: f32,
    pub opacity: [f32; 2],
    /// its pulse (`Lg_Enable_Glowing_LensFlare`: |range sin(2 pi speed t) + base|): speed,
    /// range, base
    #[serde(default)]
    pub glow: Option<[f32; 3]>,
}

/// A planar reflection (`SceneCaptureReflectActor`): the render target it draws into (a texture
/// the water's materials sample at the screen's place), its mirror plane, the channels it
/// shows (`ReflectionChannels`: often only the meshes put in `Group_1`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Reflection {
    pub texture: u32,
    pub point: [f32; 3],
    pub normal: [f32; 3],
    pub channels: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightKind {
    Point,
    Spot,
    Directional,
    Sky,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Light {
    pub kind: LightKind,
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub color: [f32; 3],
    pub brightness: f32,
    pub radius: f32,
    pub falloff: f32,
    pub inner_cone: f32,
    pub outer_cone: f32,
    pub shadows: bool,
    pub enabled: bool,
    /// True if the light only affects dynamic objects in the original (baked otherwise).
    pub dynamic: bool,
    pub actor: String,
    /// a dominant light: lights through its static shadow maps (`Instance::light_shadows`)
    #[serde(default)]
    pub dominant: bool,
    /// its light shafts (`bRenderLightShafts`)
    #[serde(default)]
    pub shafts: Option<LightShafts>,
}

/// A light's shafts (`LightComponent`'s): how far the occlusion reaches (m), the bloom's scale,
/// threshold, screen blend threshold and tint (linear), the radial blur (0-1) and how dark the
/// occluded parts go.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LightShafts {
    pub occlusion_range: f32,
    pub bloom_scale: f32,
    pub bloom_threshold: f32,
    pub screen_blend_threshold: f32,
    pub tint: [f32; 3],
    pub radial_blur: f32,
    pub darkness: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerStart {
    pub position: [f32; 3],
    pub yaw: f32,
    pub tag: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Spawner {
    pub name: String,
    pub position: [f32; 3],
    pub yaw: f32,
    pub pawn: String,
    pub route: Option<u32>,
    pub spawn_on_begin_play: bool,
    #[serde(default)]
    pub npc_type: Option<u32>,
    /// Spawned by a level script action that runs when the level loads.
    #[serde(default)]
    pub script_start: bool,
    /// story group override of this spawner (`Twk_ID_...`)
    #[serde(default)]
    pub story_group: String,
    /// what can be picked from its pocket (`m_pStealablePickup`): a pickup index
    #[serde(default)]
    pub stealable: Option<u32>,
    /// the faction its character takes instead of the pawn's (`m_pFactionTweakOverride`),
    /// and whether that one is Corvo's enemy
    #[serde(default)]
    pub faction: Option<(String, bool)>,
    /// what it makes knows of Corvo from the start (`m_bAwareOfPlayerUponStartup`)
    #[serde(default)]
    pub aware: bool,
    /// its squad (`m_Squad`: whose flee points it uses, who it calls for help)
    #[serde(default)]
    pub squad: String,
    /// what it makes can flee (`m_bCapableOfFleeing`)
    #[serde(default = "yes")]
    pub can_flee: bool,
    /// spawns when the alarm reaches this (`m_SpawnAtSuspicionLevel`: `DAISL_Suspecting`...)
    #[serde(default)]
    pub spawn_at: Option<String>,
    /// spawns when its squad calls for help (`m_bSpawnOnHearHelpRequest`)
    #[serde(default)]
    pub spawn_on_help: bool,
    /// may spawn while Corvo sees the spot (`m_bCanSpawnWhenVisible`) / is near it
    /// (`m_bCanSpawnWhenPlayerNear`)
    #[serde(default)]
    pub spawn_visible: bool,
    #[serde(default)]
    pub spawn_near: bool,
    /// the volumes what it makes keeps within (`m_TetherVolumes`: `scene.volumes`)
    #[serde(default)]
    pub tether: Vec<u32>,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Route {
    pub name: String,
    pub kind: String,
    pub points: Vec<[f32; 3]>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Pickup {
    pub name: String,
    pub class: String,
    pub kind: String,
    pub position: [f32; 3],
    pub instance: Option<u32>,
    /// the abstract item it gives (`m_pAbstractItem`: a note, a clue...) as `Group.Name`
    #[serde(default)]
    pub item: String,
    /// Ammunition (`m_AmmoRanges`: the index is the ammo type — 0 bullets, 1 explosive
    /// bullets, 2 bolts, 3 sleep darts, 4 incendiary bolts, 5 springrazors, 6 grenades,
    /// 7 sticky grenades — and the amount)
    #[serde(default)]
    pub ammo: Vec<(u8, u32)>,
    /// Lower bounds for variable ammunition ranges. Missing entries equal `ammo`'s
    /// upper bound, keeping older cooked maps and fixed pickups unchanged.
    #[serde(default)]
    pub ammo_min: Vec<(u8, u32)>,
    /// Food's original `m_HealthChange`; absent in older cooked maps.
    #[serde(default)]
    pub food_health: Option<u32>,
    /// what the prompt calls it (its tweak's interactable `m_Name`: "Tyvian Ore")
    #[serde(default)]
    pub label: String,
    /// how many (`m_Quantity`), and whether they're coins (`Coins_AbsItm`: pouches, valuables)
    #[serde(default)]
    pub quantity: u32,
    #[serde(default)]
    pub coins: bool,
    /// made by a level script's actor factory (`DisActorFactoryTweakObj`): not there until
    /// the script spawns it, at its spawn point
    #[serde(default)]
    pub factory: bool,
    /// the key it is (`DisKey_Base.m_Name`: what doors' `m_MatchingKeys` name)
    #[serde(default)]
    pub key: String,
    /// the inventory item a weapon pickup gives (`DisTweaks_InventoryPickup.m_pItem`:
    /// `Twk_Inv_PistolEliteGuard_Player`, the Prison's sword)
    #[serde(default)]
    pub inv_item: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Volume {
    pub kind: String,
    pub name: String,
    /// Convex hulls as world-space point clouds.
    pub hulls: Vec<Vec<[f32; 3]>>,
    /// a water volume: how it behaves
    #[serde(default)]
    pub water: Option<Water>,
    /// a stealth volume (`DisStealthVolume`): the light range it sets for Corvo's visibility
    /// (darkest, brightest) and its priority
    #[serde(default)]
    pub stealth: Option<[f32; 3]>,
    /// a forbidden zone (`DisForbiddenZone`): whose it is, who mustn't be in it
    #[serde(default)]
    pub zone: Option<ForbiddenZone>,
    /// a possession volume Corvo can't leave his host in (`DisPossessionVolume`
    /// `m_bDisallowUnpossession`)
    #[serde(default)]
    pub no_unpossess: bool,
}

/// A room of the sound propagation (`DishonoredAudioVolume`): where it is, the reverb it gives
/// (`m_Environment`), the ambience state entering it sets (`m_pSoundEvent`: the drone of the
/// street, of an interior), inside or out (`m_VolumeKind`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AudioCell {
    pub name: String,
    pub hulls: Vec<Vec<[f32; 3]>>,
    pub environment: String,
    pub state_event: String,
    pub interior: bool,
}

/// A doorway between two rooms (`DishonoredAudioPortal`): its rectangle, the rooms, how much it
/// muffles what passes through for Corvo and for the AI (`m_fOcclusion_HeardByPlayer`,
/// `m_fOcclusion_HeardByAI`; the scripts and the doors in it add theirs).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AudioPortal {
    pub name: String,
    pub corners: [[f32; 3]; 4],
    pub cells: [Option<u32>; 2],
    pub occlusion: [f32; 2],
}

/// A forbidden zone: the owning factions treat the forbidden ones found in it as enemies.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ForbiddenZone {
    pub owners: Vec<String>,
    pub forbidden: Vec<String>,
    pub enabled: bool,
}

/// The AI's places in a level.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AiMarkers {
    /// guards' watch points (`DisGuardWatchPoint`): where, how long they watch (min, max s)
    pub watch: Vec<WatchPoint>,
    /// flee points (`DisFleePointActor`, its `DisFleeComponent`)
    pub flee: Vec<FleePoint>,
    /// where guards stand to look into a hideout (`DisHideoutAccessPoint`): position and yaw
    pub hideout_access: Vec<[f32; 4]>,
    /// ambush points (`DisAmbushPoint`)
    pub ambush: Vec<AmbushPoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct WatchPoint {
    pub position: [f32; 3],
    pub time: [f32; 2],
}

/// Where a squad's members flee (to raise the alarm), and the point after it.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FleePoint {
    pub name: String,
    pub position: [f32; 3],
    pub yaw: f32,
    pub squads: Vec<String>,
    pub next: Option<u32>,
}

/// Where a character lies in wait, and the area it springs on (a box along its direction,
/// metres).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AmbushPoint {
    pub name: String,
    pub position: [f32; 3],
    pub yaw: f32,
    /// the area's direction (radians, the game's yaw), length, width, top and bottom
    pub area: [f32; 5],
}

/// A water volume (`DishonoredWaterVolume` and its `DishonoredWaterVolumeInfo`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Water {
    /// the current (`ZoneVelocity`, m/s)
    pub current: [f32; 3],
    /// `FluidFriction`
    pub friction: f32,
    /// splashes going in, softest first: (speed from which it plays (m/s), sound event,
    /// particle system)
    pub entry: Vec<(f32, String, Option<u32>)>,
    /// what's heard climbing out
    pub exit: String,
    /// the loop heard under water, and the event that stops it
    pub underwater: (String, String),
    /// the colour grade under water (`m_PpOverride`)
    pub grade: Option<PostProcess>,
    /// the fog under water (`m_FogComponent`)
    pub fog: Option<Fog>,
    #[serde(default)]
    pub fog_layer: Option<FogLayer>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Fog {
    pub color: [f32; 3],
    pub start: f32,
    pub end: f32,
    pub density: f32,
}

/// A river krust (`DisRiverKrust` + `DisTweaks_RiverKrust`): a shellfish clamped to a
/// wall that opens to spit acid at whoever comes within its aggressive range and shuts tight
/// when they come close; vulnerable only while open.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Krust {
    pub position: [f32; 3],
    /// UE rotator (the shell's facing)
    pub rotation: [i32; 3],
    pub scale: f32,
    pub npc_type: Option<u32>,
    /// ranges (m): defensive enter / exit, aggressive enter / exit
    pub defensive: [f32; 2],
    pub aggressive: [f32; 2],
    /// spit damage by difficulty
    pub damage: [f32; 4],
    pub health: f32,
    /// seconds before the first volley and between volleys, shots per volley
    pub first_volley: [f32; 2],
    pub between_volleys: [f32; 2],
    pub shots: [u32; 2],
    /// spit speed (m/s) and gravity (fraction)
    pub speed: [f32; 2],
    pub gravity: f32,
    /// the spit's trail (particle system)
    pub trail: Option<u32>,
    /// the pearl inside (coins)
    pub loot_coins: u32,
    #[serde(default)]
    pub name: String,
    /// the sequence of each animation state (`ERiverKrustAnimState`)
    #[serde(default)]
    pub anims: Vec<KrustAnim>,
    /// seconds before it shuts when someone comes close, before it opens again once they
    /// have gone, before it calms down once they're out of range; how long a hit keeps it shut
    #[serde(default)]
    pub defensive_delay: [f32; 2],
    #[serde(default)]
    pub defensive_exit_delay: [f32; 2],
    #[serde(default)]
    pub aggressive_exit_delay: [f32; 2],
    #[serde(default)]
    pub reaction: [f32; 2],
    /// the cone it can spit in (degrees)
    #[serde(default)]
    pub fire_angle: f32,
    /// target distances (m) mapped onto the spit's speed range
    #[serde(default)]
    pub speed_distance: [f32; 2],
    /// chance of aiming true, and leading a moving target
    #[serde(default)]
    pub accurate: f32,
    #[serde(default)]
    pub lead: bool,
    /// damage multipliers by damage type: open, and shut in its shell
    #[serde(default)]
    pub filters: Vec<(String, f32)>,
    #[serde(default)]
    pub protected_filters: Vec<(String, f32)>,
    /// the stalk it grows on (`Stalk_Attach`) and its pearl (`Pearl_Big`): mesh, materials
    #[serde(default)]
    pub stalk: Option<(u32, Vec<u32>)>,
    #[serde(default)]
    pub pearl: Option<(u32, Vec<u32>)>,
    #[serde(default)]
    pub pearl_name: String,
}

/// A part of a tripwire trap: a wire across the way (`DisTripwire` + `DisTweaks_Tripwire`) or a
/// launcher on its tripod (`DisProjectileLauncher` + `DisTweaks_ProjectileLauncher`). Tripping
/// the wire raises its `DisSeqEvent_Tripwire`; the level's script fires the launchers
/// (`DisSeqAct_ActivateProjectileLauncher`). A launcher can be disarmed for its ammunition.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Trap {
    pub name: String,
    pub launcher: bool,
    pub position: [f32; 3],
    /// UE rotator
    pub rotation: [i32; 3],
    pub scale: f32,
    /// its mesh and animations, as a character type
    pub npc_type: Option<u32>,
    /// the tripped (firing) sequence and the disarming one
    pub fire_anim: String,
    pub disarm_anim: String,
    /// the prompt: what it's called, the verb, the message once used
    pub label: String,
    pub verb: String,
    pub used_message: String,
    /// a launcher: what it aims at (`m_pTarget`), the socket it shoots from, and when in the
    /// firing sequence it shoots (`DishonoredNotify_FireProjectile`)
    pub target: Option<[f32; 3]>,
    pub socket: String,
    pub fire_at: f32,
    /// the shot: speed (m/s), damage on a direct hit, gravity multiplier, trail, its in-air
    /// sound, and its blast when it explodes
    pub speed: f32,
    pub damage: f32,
    pub gravity: f32,
    pub trail: Option<u32>,
    pub fly_sound: String,
    pub blast: Option<TrapBlast>,
    /// the sequences' particle notifies: (socket, system) firing and disarming
    pub fire_fx: Option<(String, u32)>,
    pub disarm_fx: Option<(String, u32)>,
    /// what disarming it gives (`m_HarvestedAmmo`: ammo type, amount)
    pub harvest: Vec<(u8, u32)>,
}

/// A place characters stop at on their rounds (`DisNPCDistractor` and its
/// `DisNPCDistractionComponent`): passing within its range (and sight of it), a calm one may
/// stop and play its scene (a "soiree": scavenging the ground, leaning on a railing), then
/// go on; the level scripts hear of it (`DisSeqEvent_Distracted`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Distractor {
    pub name: String,
    pub position: [f32; 3],
    pub yaw: f32,
    /// how near (m), whether it must be seen, the wait before it draws anyone again (s)
    pub range: f32,
    pub los: bool,
    pub cooldown: f32,
    /// the chance a calm / a wary passer-by stops (`m_SuspicionLvl1_Chances`,
    /// `m_SuspicionLvl234_Chances` `m_fChanceToAnimate`)
    pub chance: f32,
    pub chance_wary: f32,
    /// the patrol routes it draws from (route actor names; none: any)
    pub routes: Vec<String>,
    /// its scene (`kismet.matinees`), the places its groups stand for, and how many times
    /// each section repeats (pin, min, max)
    pub matinee: Option<u32>,
    pub marks: Vec<(String, [f32; 3])>,
    pub loops: Vec<(String, u32, u32)>,
    /// without a scene: the animation (prefix + `In`, `Loop`, `Out`) and how long it loops
    pub anim: String,
    pub time: [f32; 2],
}

/// A place rats keep away from (`DisRatRepulsor`): within its radius no swarm goes, and (with
/// `m_bPreventDevouringRatSpawn`) Corvo's Devouring Swarm can't be summoned.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RatRepulsor {
    pub position: [f32; 3],
    pub radius: f32,
    pub no_summon: bool,
}

/// A trap shot's explosion (`DisTweaks_Explosion`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TrapBlast {
    /// metres: to characters (all, full), to Corvo (all, full)
    pub radius: f32,
    pub full: f32,
    pub player_radius: f32,
    pub player_full: f32,
    /// by difficulty
    pub damage: [f32; 4],
    pub effect: Option<u32>,
    pub sound: String,
}

/// A usable object (`DishonoredUsableObject` + `DisTweaks_UsableObject`): its moving part's
/// mesh and animations, and the stages each use moves it through (`m_Stages`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UsableObj {
    pub name: String,
    pub npc_type: Option<u32>,
    /// its moving part as cooked in place (the bind pose), which the animated one replaces
    pub instance: Option<u32>,
    pub stages: Vec<UsableStage>,
    /// what it's called and its prompt (`m_pInteractableTweaks`)
    pub label: String,
    pub text: String,
    /// locked as the level starts (`m_bLocked`), and the keys that open it (`m_MatchingKeys`)
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub keys: Vec<String>,
    /// an audiograph player's card (`DisAudioLogPlayer.m_pAudioLog`, as
    /// `AbstractInv_Graphs.AG_..`), the sounds as it starts and stops (`m_pFlavorSounds`) and
    /// the card's sequence while it plays (`m_PlayingAnimName`)
    #[serde(default)]
    pub audiograph: String,
    #[serde(default)]
    pub flavor: Vec<String>,
    #[serde(default)]
    pub playing_anim: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UsableStage {
    pub name: String,
    /// the sequence that moves it into this stage
    pub anim: String,
    /// the prompt for this stage (`m_InteractTextOverride`)
    pub text: String,
    /// it can't be used again after this (`m_bDeactivateUsableWhenDone`)
    pub once: bool,
}

/// The levels' navigation meshes merged: vertices (Bevy space) and convex polygons.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NavMesh {
    pub verts: Vec<[f32; 3]>,
    pub polys: Vec<NavPoly>,
    /// the meshes riding movers (`ArkDynamicPylon`), the scripts join and part from the rest
    /// (`ArkSeqAct_ChangePylonConnection`): the pylon's actor, its polygons (from, to)
    #[serde(default)]
    pub dynamic: Vec<(String, u32, u32)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NavPoly {
    pub verts: Vec<u32>,
    /// the polygons it opens onto: (polygon, the shared edge's two vertices)
    pub links: Vec<(u32, u32, u32)>,
}

/// A protective tune (`DisTweaks_Tune_Protection`): within one radius Corvo is disoriented,
/// within the other his powers fail; its start and stop sounds for each.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TuneSource {
    pub position: [f32; 3],
    pub disorient: f32,
    pub inhibit: f32,
    pub sounds: [String; 4],
}

/// A tap or a fountain (`DisWaterSource` + `DisTweaks_WaterSource`): its prompt, and what its
/// use plays (the "Use" stage's sequence: the valve, the running water).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct WaterSource {
    pub name: String,
    /// the prompt's verb ("Drink")
    pub text: String,
    pub position: [f32; 3],
    /// (seconds, Wwise event)
    pub sounds: Vec<(f32, String)>,
    /// the water's stream: when, which system, where
    pub stream: Option<(f32, u32, [f32; 3])>,
    /// how long using it takes (the sequence)
    pub duration: f32,
}

/// A physics prop (`DishonoredMovable` + `DisTweaks_Movable`: a bottle, a chair, a pan) or a
/// breakable in the way (`DishonoredBreakableNavBlock`: a crate, boards), on its instance.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Movable {
    pub instance: u32,
    /// a whale oil tank (`DisWhaleOilBattery`): its actor, for its receptacle
    #[serde(default)]
    pub tank: Option<String>,
    /// what the prompt calls it
    pub name: String,
    /// Corvo can pick it up (`m_bInteractable`)
    pub interactable: bool,
    /// it stays put until broken (`m_bFixed`)
    pub fixed: bool,
    /// `m_WeightClass` (0 tiny, 1 small, 2 medium, 3 large) and the damage it does thrown
    pub weight: u8,
    pub damage: f32,
    /// `m_Health`, `m_DamageThreshold`; the impact speeds that hurt it, thrown and dropped (m/s)
    pub health: f32,
    pub threshold: f32,
    pub break_speed: f32,
    pub break_speed_drop: f32,
    pub grab_sound: String,
    /// the surface it's made of (`m_pContactTypeOverride`: Glass, Wood, Metal...)
    pub surface: String,
    /// what happens when it breaks (its last `m_Steps`)
    pub breaks: Option<BreakStep>,
    /// what its knocks sound like, against the world and against a body (the contact system,
    /// `Dis_ContactSystem`, for its contact type)
    #[serde(default)]
    pub impacts: Vec<Impact>,
    /// the physics joints it hangs by (`RB_ConstraintActor`s: a PA speaker's): held where it
    /// is until the scripts destroy them
    #[serde(default)]
    pub joints: Vec<String>,
    /// a whale oil tank's charge (`DisTweaks_WhaleOilBattery`): what it holds full
    /// (`m_InitialNumberOfCharges`), and what a device it feeds spends killing someone
    /// (`m_PawnChargeCost`), an animal (`m_AmbientAnimalChargeCost`), on a watchtower's shot
    /// (`m_WatchtowerChargeCost`); and the delay before it sets off a tank by it
    /// (`m_fExplosionChainTimer`)
    #[serde(default)]
    pub charges: Option<[f32; 5]>,
}

/// One of the contact system's intersections (striker against struck): its sound (volume
/// scaled with the speed between two bounds), particle and AI noise.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Impact {
    /// what was struck (`Environment`, `Body`, `Env_Wood`...)
    pub against: String,
    pub sound: String,
    /// m/s: below `min_speed` at `min_volume`, full from `max_speed` (when it scales)
    pub min_speed: f32,
    pub max_speed: f32,
    pub min_volume: f32,
    pub scale_with_speed: bool,
    pub particle: Option<u32>,
    /// how far the AI hear it (m), and whether it alarms them
    pub noise: f32,
    pub threatening: bool,
}

/// A movable's break (`DisUsableObjectBreakSteps` step).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BreakStep {
    pub sound: String,
    /// how far the AI hear it (m) and whether it alarms them (`EAINoiseContext_Threatening`)
    pub noise: f32,
    pub threatening: bool,
    pub particle: Option<u32>,
    pub particle_offset: [f32; 3],
    /// the pieces it falls into: mesh, materials
    pub chunks: Vec<(u32, Vec<u32>)>,
    /// it bursts (`m_pExplosion`: a whale oil tank)
    #[serde(default)]
    pub blast: Option<TrapBlast>,
}

/// A door's keyhole (its mesh's `KeyHole` socket) as the closed door stands, and the view
/// through it.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Keyhole {
    /// the door (its instance, and its actor for the level scripts)
    pub instance: u32,
    pub actor: String,
    pub center: [f32; 3],
    /// through the door (either way)
    pub normal: [f32; 3],
    /// field of view (degrees, `m_fKeyHoleFOV`)
    pub fov: f32,
}

/// A hagfish (`DisFish` + `DisTweaks_Fish`): it roams the water around where it was put and
/// goes for whoever swims near, biting (`m_DamagePerHit` by difficulty, `m_fMinTimeBetweenBites`
/// apart); it eats the bodies in the water.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Fish {
    pub name: String,
    pub position: [f32; 3],
    pub yaw: f32,
    pub npc_type: Option<u32>,
    /// metres around its place, metres per second roaming and attacking
    pub roam: f32,
    pub speed: f32,
    pub attack_speed: f32,
    pub damage: [f32; 4],
    pub bite_interval: f32,
    /// how close to the eye it bites (m)
    pub bite_distance: f32,
    /// seconds to eat a body
    pub consume: f32,
    pub swim: String,
    pub bites: Vec<String>,
    pub eat: String,
    pub death: String,
}

/// A river krust animation state's sequence (`RiverKrustAnimSequenceSpec`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct KrustAnim {
    pub name: String,
    pub rate: [f32; 2],
    /// latest random start (s)
    pub start: f32,
    pub looping: bool,
    pub restart: bool,
    pub blend: f32,
}

/// One layer of Dishonored's fog (`DisFog` / `DisFogComponent`), as the original full-screen
/// pass (`FDisFogPixelShader`) applies it: the part of the view ray inside the layer's height
/// band, beyond its near plane, ramps to its opacity at its far plane (through its LUT when it
/// has one); a sun layer only shows towards the sun.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FogLayer {
    /// linear colour
    pub color: [f32; 3],
    pub opacity: f32,
    /// the height band (m)
    pub min_height: f32,
    pub max_height: f32,
    /// fogged distance where it starts and peaks (m)
    pub near: f32,
    pub far: f32,
    /// no fog on what's farther than this (view depth, m): the sky
    pub no_fog: f32,
    pub height_density: f32,
    /// a sun layer: its power (`SunPower`)
    pub sun: Option<f32>,
    /// the fog curve over [near, far] (`m_RawLut` / `FogLUT`), empty: linear
    pub lut: Vec<f32>,
    pub interior: bool,
    pub enabled: bool,
    /// the level package it's in (`scene.levels`)
    pub level: u32,
}

// ---------------------------------------------------------------- meshes

#[derive(Debug, Clone, Default)]
pub struct MeshFile {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub tangents: Vec<[f32; 4]>,
    pub uv0: Vec<[f32; 2]>,
    pub uv1: Vec<[f32; 2]>,
    pub colors: Vec<[u8; 4]>,
    pub indices: Vec<u32>,
    /// (first_index, index_count) per section
    pub sections: Vec<(u32, u32)>,
    /// Optional skinning data (indices into the owning skeleton).
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
}

const MESH_MAGIC: &[u8; 4] = b"DHM2";
const TEX_MAGIC: &[u8; 4] = b"DHT1";

fn put<T: bytemuck::Pod>(out: &mut Vec<u8>, v: &[T]) {
    out.extend_from_slice(&(v.len() as u32).to_le_bytes());
    out.extend_from_slice(bytemuck::cast_slice(v));
}

fn get<T: bytemuck::Pod + Copy>(d: &[u8], pos: &mut usize) -> Result<Vec<T>> {
    if *pos + 4 > d.len() {
        bail!("truncated");
    }
    let n = u32::from_le_bytes(d[*pos..*pos + 4].try_into().unwrap()) as usize;
    *pos += 4;
    let bytes = n * std::mem::size_of::<T>();
    if *pos + bytes > d.len() {
        bail!("truncated array");
    }
    let v: Vec<T> = bytemuck::pod_collect_to_vec(&d[*pos..*pos + bytes]);
    *pos += bytes;
    Ok(v)
}

/// A Lightmass volume lighting sample (UE3 `FVolumeLightingSample`) in Bevy space: the
/// incident lighting at a point as two directional lobes plus an ambient term (linear).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VolumeSample {
    pub position: [f32; 3],
    pub radius: f32,
    pub indirect_dir: [f32; 3],
    pub shadowed: f32,
    pub indirect: [f32; 3],
    pub _pad0: f32,
    pub environment_dir: [f32; 3],
    pub _pad1: f32,
    pub environment: [f32; 3],
    pub _pad2: f32,
    pub ambient: [f32; 3],
    pub _pad3: f32,
}

const LVOL_MAGIC: &[u8; 4] = b"DHLV";

pub fn write_light_volume(path: &Path, samples: &[VolumeSample]) -> Result<()> {
    let mut out = Vec::new();
    out.extend_from_slice(LVOL_MAGIC);
    put(&mut out, samples);
    write_atomic(path, &out)
}

pub fn read_light_volume(path: &Path) -> Result<Vec<VolumeSample>> {
    let d = std::fs::read(path)?;
    if d.len() < 4 || &d[..4] != LVOL_MAGIC {
        bail!("bad light volume {}", path.display());
    }
    let mut p = 4;
    get(&d, &mut p)
}

impl MeshFile {
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(MESH_MAGIC);
        put(&mut out, &self.positions);
        put(&mut out, &self.normals);
        put(&mut out, &self.tangents);
        put(&mut out, &self.uv0);
        put(&mut out, &self.uv1);
        put(&mut out, &self.colors);
        put(&mut out, &self.indices);
        let secs: Vec<[u32; 2]> = self.sections.iter().map(|s| [s.0, s.1]).collect();
        put(&mut out, &secs);
        if !self.joints.is_empty() {
            put(&mut out, &self.joints);
            put(&mut out, &self.weights);
        }
        write_atomic(path, &out)
    }

    pub fn read(path: &Path) -> Result<MeshFile> {
        let d = std::fs::read(path)?;
        if d.len() < 4 || &d[..4] != MESH_MAGIC {
            bail!("bad mesh file {}", path.display());
        }
        let mut p = 4;
        let positions = get(&d, &mut p)?;
        let normals = get(&d, &mut p)?;
        let tangents = get(&d, &mut p)?;
        let uv0 = get(&d, &mut p)?;
        let uv1 = get(&d, &mut p)?;
        let colors = get(&d, &mut p)?;
        let indices = get(&d, &mut p)?;
        let secs: Vec<[u32; 2]> = get(&d, &mut p)?;
        let (joints, weights) = if p < d.len() { (get(&d, &mut p)?, get(&d, &mut p)?) } else { (Vec::new(), Vec::new()) };
        Ok(MeshFile {
            joints,
            weights,
            positions,
            normals,
            tangents,
            uv0,
            uv1,
            colors,
            indices,
            sections: secs.into_iter().map(|s| (s[0], s[1])).collect(),
        })
    }
}

// ---------------------------------------------------------------- animations

/// Keyframes of one bone channel; frames without a key interpolate linearly.
#[derive(Debug, Clone, Default)]
pub struct KeyTrack<T> {
    /// Index into `AnimFile::bones`.
    pub bone: u16,
    pub frames: Vec<u16>,
    pub values: Vec<T>,
}

#[derive(Debug, Clone, Default)]
pub struct AnimClip {
    pub name: String,
    pub duration: f32,
    /// Frames per second.
    pub rate: f32,
    pub frames: u16,
    /// Displacement of the root bone over the clip (Bevy space, metres, mesh frame).
    /// The root's own keys are dropped: the game moves characters itself.
    pub root_motion: [f32; 3],
    /// Local bone rotations / translations in Bevy space.
    pub rotations: Vec<KeyTrack<[f32; 4]>>,
    pub translations: Vec<KeyTrack<[f32; 3]>>,
    /// sound notifies: (time in seconds, Wwise event name)
    pub sounds: Vec<(f32, String)>,
}

/// The sequences of one AnimSet, bound to bone names.
#[derive(Debug, Clone, Default)]
pub struct AnimFile {
    pub bones: Vec<String>,
    pub clips: Vec<AnimClip>,
}

const ANIM_MAGIC: &[u8; 4] = b"DHA2";

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    out.extend_from_slice(&(b.len().min(u16::MAX as usize) as u16).to_le_bytes());
    out.extend_from_slice(&b[..b.len().min(u16::MAX as usize)]);
}

fn get_str(d: &[u8], p: &mut usize) -> Result<String> {
    let n = get_pod::<u16>(d, p)? as usize;
    let Some(b) = d.get(*p..*p + n) else { bail!("truncated string") };
    *p += n;
    Ok(String::from_utf8_lossy(b).into_owned())
}

fn get_pod<T: bytemuck::Pod>(d: &[u8], p: &mut usize) -> Result<T> {
    let n = std::mem::size_of::<T>();
    let Some(b) = d.get(*p..*p + n) else { bail!("truncated") };
    *p += n;
    Ok(bytemuck::pod_read_unaligned(b))
}

impl AnimFile {
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(ANIM_MAGIC);
        out.extend_from_slice(&(self.bones.len() as u16).to_le_bytes());
        for b in &self.bones {
            put_str(&mut out, b);
        }
        out.extend_from_slice(&(self.clips.len() as u32).to_le_bytes());
        for c in &self.clips {
            put_str(&mut out, &c.name);
            for v in [c.duration, c.rate, c.root_motion[0], c.root_motion[1], c.root_motion[2]] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&c.frames.to_le_bytes());
            out.extend_from_slice(&(c.rotations.len() as u16).to_le_bytes());
            for t in &c.rotations {
                out.extend_from_slice(&t.bone.to_le_bytes());
                out.extend_from_slice(&(t.frames.len() as u16).to_le_bytes());
                out.extend_from_slice(bytemuck::cast_slice(&t.frames));
                // unit quaternions as 16-bit signed normalized
                for q in &t.values {
                    for x in q {
                        out.extend_from_slice(&((x.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
                    }
                }
            }
            out.extend_from_slice(&(c.translations.len() as u16).to_le_bytes());
            for t in &c.translations {
                out.extend_from_slice(&t.bone.to_le_bytes());
                out.extend_from_slice(&(t.frames.len() as u16).to_le_bytes());
                out.extend_from_slice(bytemuck::cast_slice(&t.frames));
                out.extend_from_slice(bytemuck::cast_slice(&t.values));
            }
            out.extend_from_slice(&(c.sounds.len() as u16).to_le_bytes());
            for (t, e) in &c.sounds {
                out.extend_from_slice(&t.to_le_bytes());
                put_str(&mut out, e);
            }
        }
        write_atomic(path, &out)
    }

    pub fn read(path: &Path) -> Result<AnimFile> {
        let d = std::fs::read(path)?;
        if d.len() < 4 || &d[..4] != ANIM_MAGIC {
            bail!("bad anim file {}", path.display());
        }
        let mut p = 4;
        let nb = get_pod::<u16>(&d, &mut p)? as usize;
        let mut bones = Vec::with_capacity(nb);
        for _ in 0..nb {
            bones.push(get_str(&d, &mut p)?);
        }
        let nc = get_pod::<u32>(&d, &mut p)? as usize;
        let mut clips = Vec::with_capacity(nc);
        for _ in 0..nc {
            let name = get_str(&d, &mut p)?;
            let mut f = [0f32; 5];
            for v in f.iter_mut() {
                *v = get_pod(&d, &mut p)?;
            }
            let frames = get_pod::<u16>(&d, &mut p)?;
            let mut c = AnimClip { name, duration: f[0], rate: f[1], frames, root_motion: [f[2], f[3], f[4]], ..Default::default() };
            let nr = get_pod::<u16>(&d, &mut p)?;
            for _ in 0..nr {
                let bone = get_pod::<u16>(&d, &mut p)?;
                let n = get_pod::<u16>(&d, &mut p)? as usize;
                let mut t = KeyTrack { bone, frames: Vec::with_capacity(n), values: Vec::with_capacity(n) };
                for _ in 0..n {
                    t.frames.push(get_pod(&d, &mut p)?);
                }
                for _ in 0..n {
                    let q: [i16; 4] = get_pod(&d, &mut p)?;
                    t.values.push(q.map(|x| x as f32 / 32767.0));
                }
                c.rotations.push(t);
            }
            let nt = get_pod::<u16>(&d, &mut p)?;
            for _ in 0..nt {
                let bone = get_pod::<u16>(&d, &mut p)?;
                let n = get_pod::<u16>(&d, &mut p)? as usize;
                let mut t = KeyTrack { bone, frames: Vec::with_capacity(n), values: Vec::with_capacity(n) };
                for _ in 0..n {
                    t.frames.push(get_pod(&d, &mut p)?);
                }
                for _ in 0..n {
                    t.values.push(get_pod(&d, &mut p)?);
                }
                c.translations.push(t);
            }
            let ns = get_pod::<u16>(&d, &mut p)?;
            for _ in 0..ns {
                let t: f32 = get_pod(&d, &mut p)?;
                c.sounds.push((t, get_str(&d, &mut p)?));
            }
            clips.push(c);
        }
        Ok(AnimFile { bones, clips })
    }
}

// ---------------------------------------------------------------- textures

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TexFormat {
    Bc1 = 0,
    Bc2 = 1,
    Bc3 = 2,
    Bc5 = 3,
    Rgba8 = 4,
    R8 = 5,
    Rg8 = 6,
    /// Single channel block compression (shadow factor pages).
    Bc4 = 7,
}

impl TexFormat {
    pub fn from_u32(v: u32) -> Option<TexFormat> {
        Some(match v {
            0 => TexFormat::Bc1,
            1 => TexFormat::Bc2,
            2 => TexFormat::Bc3,
            3 => TexFormat::Bc5,
            4 => TexFormat::Rgba8,
            5 => TexFormat::R8,
            6 => TexFormat::Rg8,
            7 => TexFormat::Bc4,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TexFile {
    pub format: TexFormat,
    pub width: u32,
    pub height: u32,
    pub srgb: bool,
    pub clamp: bool,
    /// All mip levels concatenated, largest first.
    pub mips: Vec<Vec<u8>>,
}

impl TexFile {
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(TEX_MAGIC);
        for v in [
            self.format as u32,
            self.width,
            self.height,
            self.mips.len() as u32,
            self.srgb as u32,
            self.clamp as u32,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for m in &self.mips {
            out.extend_from_slice(&(m.len() as u32).to_le_bytes());
            out.extend_from_slice(m);
        }
        write_atomic(path, &out)
    }

    pub fn read(path: &Path) -> Result<TexFile> {
        let d = std::fs::read(path)?;
        if d.len() < 28 || &d[..4] != TEX_MAGIC {
            bail!("bad texture file {}", path.display());
        }
        let u = |i: usize| u32::from_le_bytes(d[4 + i * 4..8 + i * 4].try_into().unwrap());
        let format = TexFormat::from_u32(u(0)).ok_or_else(|| anyhow::anyhow!("bad tex format"))?;
        let (width, height, nmips, srgb, clamp) = (u(1), u(2), u(3), u(4) != 0, u(5) != 0);
        let mut p = 28;
        let mut mips = Vec::new();
        for _ in 0..nmips {
            if p + 4 > d.len() {
                bail!("truncated texture");
            }
            let n = u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
            p += 4;
            if p + n > d.len() {
                bail!("truncated texture mip");
            }
            mips.push(d[p..p + n].to_vec());
            p += n;
        }
        Ok(TexFile { format, width, height, srgb, clamp, mips })
    }
}

pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Turn an object path into a safe relative file name.
pub fn sanitize(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' })
        .collect::<String>()
        .to_ascii_lowercase()
}

/// An audiograph (`DisAbstractItemAudioLog`): its card's name and words, and the recording it
/// plays (its speaker's `Dlg_AudioGraphs` tree: the `DisConv_Hook_PlayAudioLog` link of its
/// `m_AudioLogName`, its blurbs' Wwise events and words).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Audiograph {
    pub log: String,
    pub title: String,
    pub description: String,
    pub lines: Vec<(String, String)>,
}

/// Campaign-wide game data from the original tweaks: powers, the player's attributes,
/// bone charms, upgrades and the craftsman's store (`cache/game/gamedata.json`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GameData {
    /// the Dunwall City Trials' challenges (`DisDLC05GameInfo.m_Challenges`)
    #[serde(default)]
    pub challenges: Vec<ChallengeDef>,
    /// the player statistics the achievements read (`DisTweaks_PlayerStats.m_StatInfos`), and the
    /// achievements (`m_Achievements`, by `EAchievement`)
    #[serde(default)]
    pub stat_infos: Vec<StatInfoDef>,
    #[serde(default)]
    pub achievements: Vec<AchievementDef>,
    pub actives: Vec<ActivePowerDef>,
    pub passives: Vec<PassivePowerDef>,
    /// player attributes (`m_HealthMax` -> `HealthMax`): easy, normal, hard, very hard
    pub attributes: std::collections::BTreeMap<String, [f32; 4]>,
    /// the player pawn's numeric tweaks (elixir values, jump impulse...)
    pub pawn: std::collections::BTreeMap<String, f32>,
    /// the grading the menus that pause the game put over it (`DisGlobalUIManager`'s
    /// `m_pBlurTweaks`, `UI_Blur`: its `m_Parameters` flattened, `m_fFadeInTime`...)
    #[serde(default)]
    pub ui_blur: std::collections::BTreeMap<String, f32>,
    pub charms: Vec<CharmDef>,
    pub upgrades: Vec<UpgradeDef>,
    pub stores: Vec<StoreDef>,
    /// how long a possession lasts at power level 1 / 2: animals, humans (0: not possible)
    pub possess_animal: [f32; 2],
    pub possess_human: [f32; 2],
    /// the user interface's strings (`Localization/INT/DishonoredGame.int`):
    /// "DisGFxMoviePlayerJournal_Texts.t_Blink" -> "Blink"
    #[serde(default)]
    pub texts: std::collections::BTreeMap<String, String>,
    /// loading screen hints by set ("Prison", "Overseer"...)
    #[serde(default)]
    pub hints: std::collections::BTreeMap<String, Vec<String>>,
    /// location names of the maps ("L_Streets1_P" -> "Distillery District")
    #[serde(default)]
    pub map_names: std::collections::BTreeMap<String, String>,
    /// The Heart's whispers (`Dlg_HeartGadget`)
    #[serde(default)]
    pub heart: ConvGraph,
    /// The end-of-mission screens (`Twk_MissionStats.*`), by tweak name
    #[serde(default)]
    pub mission_stats: Vec<MissionStatsDef>,
    /// What Corvo carries when a mission starts on its own (`Twk_PlayerLoadouts.*`)
    #[serde(default)]
    pub loadouts: Vec<LoadoutDef>,
    /// The campaign's missions and their maps (`DefaultGame.ini` `m_MissionsGame`), and the
    /// chaos level at which each mission counts as high chaos (`m_MissionChaosThreshold`)
    #[serde(default)]
    pub missions: Vec<(i32, Vec<String>)>,
    #[serde(default)]
    pub chaos_thresholds: Vec<i32>,
    /// The chapters (`Twk_ChapterInfoList`) by tag: the journal's mission page
    #[serde(default)]
    pub chapters: Vec<ChapterDef>,
    /// Abstract items (notes, mission clues, valuables...) by `Group.Name`: name and text
    /// (`Localization/INT/*.int`)
    #[serde(default)]
    pub abstract_items: std::collections::BTreeMap<String, (String, String)>,
    /// where the journal shows each abstract item (`m_JournalDisplaySection`: `DJIS_Mission`,
    /// `DJIS_Note`, `DJIS_None`...) and its picture (`m_JournalIconName`), by the same keys
    #[serde(default)]
    pub journal_items: std::collections::BTreeMap<String, (String, String)>,
    /// the audiographs, by their item (`AbstractInv_Graphs.AG_Pub01a_Havelock`)
    #[serde(default)]
    pub audiographs: std::collections::BTreeMap<String, Audiograph>,
    /// characters' names (a pawn tweak's `DisTweaks_InteractableInterface.m_Name`), by pawn
    /// object name: "Twk_Pawn_AntonSokolov" -> "Anton Sokolov"
    #[serde(default)]
    pub pawn_names: std::collections::BTreeMap<String, String>,
    /// The post-process graph's material nodes that override the grading while they run
    /// (`ArkPpNodeMaterial::m_bOverrideUberPp`), by effect name ("BendTimeVectors",
    /// "BlinkVectors2"...): their `m_UberParameters`, flattened
    #[serde(default)]
    pub post_nodes: std::collections::BTreeMap<String, std::collections::BTreeMap<String, f32>>,
}

/// A time-varying material parameter (`ScalarParameterValues[].ParameterValueCurve`): its
/// value before it plays and its keys (seconds, value; linear).
/// What a player statistic counts (`DisTweaks_PlayerStats.m_StatInfos`): its statistic
/// (`ePlayerStat_NumKills`...), its label, the blows' damage types it counts (none: any), the
/// characters it counts only, and those it leaves out (pawn tweaks: the Prison's assassins).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct StatInfoDef {
    pub stat: String,
    pub text: String,
    pub damage_types: Vec<String>,
    pub tweaks: Vec<String>,
    pub excluded: Vec<String>,
}

/// An achievement (`m_Achievements[EAchievement]`): judged when the scripts say
/// (`m_bEvaluatedAtKismet`: `DisSeqAct_EvalAchievement`) or as the statistics change; its
/// conditions, all to hold: a statistic (`stat_infos` index), the comparison
/// (`eValueInequality_GreaterThan`...), the threshold.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AchievementDef {
    pub name: String,
    pub kismet: bool,
    pub evals: Vec<(u32, String, f32)>,
    /// each condition's streak (`m_fStreakValue`, `m_fStreakTime`): the statistic must rise by
    /// so much within so many seconds (six kills in one, thirty metres in one); 0 none
    #[serde(default)]
    pub streaks: Vec<[f32; 2]>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PostCurve {
    pub param: String,
    pub default: f32,
    pub keys: Vec<[f32; 2]>,
}

impl PostCurve {
    /// The value at `t` seconds (held before the first key and after the last).
    pub fn at(&self, t: f32) -> f32 {
        let Some(first) = self.keys.first() else { return self.default };
        if t <= first[0] {
            return first[1];
        }
        for w in self.keys.windows(2) {
            let (a, b) = (w[0], w[1]);
            if t <= b[0] {
                let k = if b[0] > a[0] { (t - a[0]) / (b[0] - a[0]) } else { 1.0 };
                return a[1] + (b[1] - a[1]) * k;
            }
        }
        self.keys.last().map(|k| k[1]).unwrap_or(self.default)
    }

    /// When its last key is.
    pub fn end(&self) -> f32 {
        self.keys.last().map(|k| k[0]).unwrap_or(0.0)
    }
}

/// An effect a first-person clip starts: clip, seconds in, the arms' socket, particle system
/// (`scene.particle_systems`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ArmFx {
    pub clip: String,
    pub time: f32,
    pub socket: String,
    pub system: u32,
}

/// A stretch of one of Corvo's clips in slow motion (`DisNotify_BendTime_Ranged`: a finisher's
/// slow motion; `DisNotify_AdrenalineBendTime_Ranged`: a Blood Thirst kill's): from `time` for
/// `duration` seconds of the clip, his and the world's time scales, eased in and out.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ArmBend {
    pub clip: String,
    pub time: f32,
    pub duration: f32,
    pub player: f32,
    pub world: f32,
    pub fade_in: f32,
    pub fade_out: f32,
    pub adrenaline: bool,
}

/// A clip's limb severed: when, the bone it breaks at, the push it gives the limb (unreal
/// units, along the bone: `eDisSeverLimbImpulse_SeverBoneX`) and whether the body falls limp.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NpcSever {
    pub clip: String,
    pub time: f32,
    pub bone: String,
    pub impulse: f32,
    pub ragdoll: bool,
}

/// Severed limbs: each break bone's blood when severed (socket, particle system, attached),
/// and the blood on the lens near it (particle system, reach m, chance).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SeveredLimbs {
    pub limbs: Vec<SeveredLimb>,
    pub lens: Option<(u32, f32, f32)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SeveredLimb {
    pub bone: String,
    pub effects: Vec<(String, u32, bool)>,
}

/// The contact system's sword against a body.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BladeBlood {
    pub system: u32,
    pub lens: Option<(u32, f32, f32)>,
}

/// A chapter of the story (`DisTweaks_ChapterInfo`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ChapterDef {
    pub tag: String,
    pub title: String,
    pub description: String,
    /// the targets: name and portrait (`cache/ui/portraits/<name>.png`)
    pub targets: Vec<(String, String)>,
}

/// A mission statistics screen (`DisTweaks_MissionStats`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MissionStatsDef {
    pub name: String,
    pub mission: i32,
    pub stats: Vec<MissionStatRow>,
    /// story outcomes shown when their flag has the value: (description, flag, value)
    pub summary: Vec<(String, String, bool)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MissionStatRow {
    pub description: String,
    /// `ePlayerStat_HostileKill`...
    pub stat: String,
    /// shown as the sum with this one
    pub add: Option<String>,
    /// a checkbox: ticked when the value is zero (or non-zero with `nonzero`)
    pub checkbox: bool,
    pub nonzero: bool,
    /// how many there are to find in the mission (`m_MissionStatsMaxValues`; 0: no limit shown)
    #[serde(default)]
    pub max: u32,
}

/// A mission start loadout (`DisTweaks_PlayerLoadout`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LoadoutDef {
    pub name: String,
    /// inventory items (tweak names)
    pub items: Vec<String>,
    /// abstract items (runes, coins, blueprints) and how many
    pub abstract_items: Vec<(String, u32)>,
    pub powers: Vec<(String, u8)>,
    pub upgrades: Vec<String>,
}

/// A dialogue graph (`DisDialogTree`): hooks lead through branches and conditions to the
/// lines spoken.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ConvGraph {
    pub nodes: Vec<ConvNode>,
    /// hook (`HEART_TARGET_WHISPER`) -> node
    pub hooks: std::collections::BTreeMap<String, u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ConvNode {
    /// `Blurb`, `SequentialBranch`, `RandomBranch`, `SpeakerInStoryGroup`, `CheckStoryFlag`
    pub class: String,
    /// the nodes it leads to (-1: nothing)
    pub outputs: Vec<i32>,
    /// random branches' weights
    #[serde(default)]
    pub weights: Vec<f32>,
    /// sequential branches start again after the last
    #[serde(default)]
    pub looping: bool,
    /// story groups of a speaker condition, one per output (exact match?)
    #[serde(default)]
    pub groups: Vec<(String, bool)>,
    /// a story flag condition (GUID as the level scripts write it)
    #[serde(default)]
    pub flag: String,
    #[serde(default)]
    pub label: String,
    /// a line: subtitle and Wwise event
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub event: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ActivePowerDef {
    /// `Blink`, `DarkVision`, `BendTime`, `Windblast`, `Possess`, `DevouringSwarm`
    pub name: String,
    /// mana per use (percent of the base bar)
    pub mana: f32,
    /// runes to acquire level 1 / level 2
    pub runes: [u32; 2],
    /// each level's settings (`m_HorizDistance`, `m_TimeDuration`...; nested as `a.b`)
    pub levels: Vec<std::collections::BTreeMap<String, f32>>,
    /// Wwise events of each level (`m_pBlinkSoundEvent` -> event name)
    pub level_sounds: Vec<std::collections::BTreeMap<String, String>>,
    /// level-independent settings
    pub params: std::collections::BTreeMap<String, f32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PassivePowerDef {
    /// `Vitality`, `BloodThirsty`, `ShadowKill`, `Celerity`
    pub name: String,
    /// level 0 is "not owned"; 1 and 2 the acquired levels
    pub levels: Vec<PassiveLevel>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PassiveLevel {
    pub runes: u32,
    pub mods: Vec<AttrMod>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AttrMod {
    /// `HealthMax` (the `Attribute_` prefix dropped)
    pub attribute: String,
    /// `AddVal`, `SetVal` or `AddBasePercent`
    pub op: String,
    pub value: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CharmDef {
    pub attribute: String,
    /// (name, description, value) of each strength
    pub levels: Vec<(String, String, f32)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UpgradeDef {
    /// `Twk_Upgrade_Armor`
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub attributes: Vec<(String, f32)>,
    /// upgrades this one replaces
    pub replaces: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct StoreDef {
    /// `Twk_Store_Craftsman`
    pub id: String,
    pub sections: Vec<String>,
    pub items: Vec<StoreItem>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct StoreItem {
    pub name: String,
    pub description: String,
    pub icon: String,
    pub section: i32,
    pub coins: u32,
    /// what it gives: an upgrade id or a pickup tweak (`SleepDart_Ammo_twk`)
    pub item: String,
    /// what must be owned first (upgrade ids, blueprints, items)
    pub requires: Vec<String>,
    /// how many it gives (ammunition, elixirs)
    #[serde(default)]
    pub quantity: u32,
}

/// A security device of a map (and what powers it).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Security {
    /// `WallOfLight`, `ArcPylon`, `WatchTower`, `AlarmBell`, `Receptacle`, `Battery`
    pub kind: String,
    pub actor: String,
    pub position: [f32; 3],
    pub yaw: f32,
    /// devices: the receptacle powering them (actor name)
    #[serde(default)]
    pub receptacle: String,
    /// receptacles: the tank in them (actor name)
    #[serde(default)]
    pub battery: String,
    /// detection cylinder (metres): base, radius, height
    #[serde(default)]
    pub detect_at: [f32; 3],
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub height: f32,
    #[serde(default)]
    pub damage: f32,
    /// factions it spares (`Faction_Guard_Default`...)
    #[serde(default)]
    pub friendly: Vec<String>,
    /// Wwise events by tweak property (`m_pStartFireSound`...)
    #[serde(default)]
    pub sounds: std::collections::BTreeMap<String, String>,
    /// numbers of its tweak (`m_fChargeDuration`...)
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, f32>,
    /// alarm bells: the spawners its ringing calls in (scene indices)
    #[serde(default)]
    pub spawners: Vec<String>,
    /// walls of light: the eye above (`DisDetectionEye`, actor name) and its looks
    /// (`DisTweaks_DetectionEye`): neutral, a threat seen, a friend seen, unpowered
    #[serde(default)]
    pub eye: String,
    #[serde(default)]
    pub eye_materials: [u32; 4],
    /// watch towers: the turning head's skeleton (`scene.skeletons`: `Tower_Jnt`, `Light_Jnt`,
    /// `Arrow_Socket`) and where it stands (Bevy-space matrix of its mesh component)
    #[serde(default)]
    pub skeleton: Option<u32>,
    #[serde(default)]
    pub rig: Option<[f32; 16]>,
    /// the searchlight's beam (`Regent_Light_Cone`): mesh and materials
    #[serde(default)]
    pub cone: Option<(u32, Vec<u32>)>,
    /// the shot (`m_pProjectileTweak`): its blast and trail (`scene.particle_systems`)
    #[serde(default)]
    pub blast: Option<TrapBlast>,
    #[serde(default)]
    pub trail: Option<u32>,
}

/// A Scaleform movie's timelines (`ui/<movie>/timeline.json`): its sprites' frames as the movie
/// stores them (display list changes, frame labels, the frame scripts' simple controls) and
/// the bitmaps its shapes are filled with, for playing its animated clips as authored.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Timelines {
    /// frames per second
    pub rate: f32,
    pub sprites: std::collections::HashMap<u16, SpriteTimeline>,
    pub shapes: std::collections::HashMap<u16, Vec<ShapeBitmap>>,
    /// every shape's bounds (x min, y min, x max, y max): masks are drawn by them
    #[serde(default)]
    pub bounds: std::collections::HashMap<u16, [f32; 4]>,
    /// exported symbols: name -> sprite
    pub exports: std::collections::HashMap<String, u16>,
    /// the text fields (`DefineEditText`), by character id
    #[serde(default)]
    pub texts: std::collections::HashMap<u16, EditTextDef>,
}

/// A movie's text field (`DefineEditText`): its box (stage units: x min, y min, x max, y max),
/// font (its name: `$NormalFont`, `$TitleFont`...), size, colour (RGBA 0..1), alignment (0 left,
/// 1 right, 2 centre, 3 justify), margins and leading, how it lays out, its initial text and the
/// variable it shows.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EditTextDef {
    pub bounds: [f32; 4],
    pub font: String,
    pub size: f32,
    pub color: [f32; 4],
    pub align: u8,
    pub margins: [f32; 2],
    pub leading: f32,
    pub multiline: bool,
    pub wrap: bool,
    pub html: bool,
    pub text: String,
    pub var: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SpriteTimeline {
    pub frames: Vec<TlFrame>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TlFrame {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ops: Vec<TlOp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TlAction>,
}

/// `PlaceObject2/3` and `RemoveObject2`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TlOp {
    Place {
        depth: u16,
        /// moves (or replaces the character of) what is at the depth
        moved: bool,
        id: Option<u16>,
        name: Option<String>,
        /// [a, b, c, d, tx, ty]: x' = a x + c y + tx, y' = b x + d y + ty (stage units)
        m: Option<[f32; 6]>,
        /// multiply (0..1) and add (0..255) terms, rgba
        cx: Option<[f32; 8]>,
        /// a mask over the depths up to this one
        clip: Option<u16>,
    },
    Remove(u16),
}

/// The frame scripts' controls of their own clip (anything else is left out).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum TlAction {
    Stop,
    Play,
    /// 0-based
    GotoFrame(u16),
    GotoLabel(String),
    /// `this._visible = ...`
    Visible(bool),
}

/// A shape's bitmap fill: the image (`ui/<movie>/<bitmap>.png`), its size as the movie declares
/// it and the fill matrix from its pixels to the shape (stage units), the shape's bounds (the
/// bitmap shows within them) and whether it tiles (a repeating fill).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ShapeBitmap {
    pub bitmap: u16,
    pub size: [f32; 2],
    pub m: [f32; 6],
    /// x min, y min, x max, y max
    #[serde(default)]
    pub bounds: Option<[f32; 4]>,
    #[serde(default)]
    pub repeat: bool,
}

/// A Dunwall City Trials challenge (`DisDLC05GameInfo.m_Challenges`): its names and words, its
/// kind (`DDCT_Stealth`, `DDCT_Action`, `DDCT_Mobility`, `DDCT_Puzzle`), leaderboard, the scores
/// of its three medals (normal and expert mode), its map (`m_LaunchCommand` "start <map>").
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ChallengeDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub expert_description: String,
    pub kind: String,
    pub leaderboard: String,
    pub medals: [i32; 3],
    pub expert_medals: [i32; 3],
    pub map: String,
    pub can_end_early: bool,
}

/// A Dunwall City Trials scoring rule set (`DisDLC05Tweaks_ChallengeScoringRuleset`): its
/// rules and bonuses (`m_Rules`), its combo multipliers (`m_Multipliers`), and the statistics
/// its results screen lists (`m_ResultsMenuStats`: lookup, name).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RulesetDef {
    pub name: String,
    pub rules: Vec<ScoreRuleDef>,
    pub multipliers: Vec<ScoreRuleDef>,
    pub stats: Vec<(String, String)>,
}

/// One of a rule set's rules (`DisDLC05ScoringRule_*`, `..Bonus_*`, `..Modifier_*`,
/// `DisDLC05ChallengeRule_Combo_*`): its name in the set, its entry (what the results call
/// it), its class, its base gain (`m_iBaseGain`), its gains by victim (`m_NpcBaseGains`: the
/// story group, gain, entry), its other numbers (`m_fTimeOut`, `m_fTimeBetweenTwoKill`...).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ScoreRuleDef {
    pub name: String,
    pub entry: String,
    pub class: String,
    pub base_gain: i32,
    pub gains: Vec<(String, i32, String)>,
    pub params: std::collections::BTreeMap<String, f32>,
}
