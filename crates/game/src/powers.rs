//! Supernatural powers and the left-hand gadgets: Blink, Dark Vision, Bend Time, Windblast,
//! Possession, Devouring Swarm, the pistol and crossbow (bolts, sleep darts), elixirs, mana
//! and health regeneration. Levels, ranges, durations and costs come from the original
//! tweaks (`gamedata`); powers are owned (acquired with runes in the journal).

use crate::audio::PostEvent;
use crate::footsteps::{impact_surface, ray_surface, ColliderSurfaces};
use crate::gamedata::{Attrs, Data};
use crate::gameplay::{HitKind, HudMessages, Noise, NoiseKind, NpcHit, NpcStagger, PlayerStats, TimeControl};
use crate::level::{GameAssets, GROUP_NPC, GROUP_PROP, GROUP_WORLD};
use crate::npc::Npc;
use crate::player::{Player, PlayerCamera, RADIUS, STAND_HALF};
use crate::worlddamage::{Reach, WorldDamage};
use crate::GameState;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_rapier3d::prelude::*;

pub struct PowersPlugin;

impl Plugin for PowersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Powers>()
            .init_resource::<Shortcuts>()
            .init_resource::<ProjectileRestore>()
            .add_message::<PowerUsed>()
            .add_message::<PowerEquipped>()
            .add_systems(OnEnter(GameState::InGame), (reset_powers, spawn_blink_marker))
            .add_systems(Update, restore_projectiles.after(crate::save::restore_npcs).before(bolt_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, bolt_focus.after(crate::interact::FocusSet).before(crate::gadgets::grenade_focus).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(
                Update,
                (select_power, use_power, windblast_gusts, ignite, transversal_freeze, cook_grenade, blink_travel, update_projectiles, regenerate, dark_vision_post, crate::darkvision::update_dark_vision, crate::darkvision::dark_vision_sounds)
                    .chain()
                    .after(crate::gadgets::grenade_focus)
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

/// What the left hand holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Power {
    Empty,
    Blink,
    DarkVision,
    BendTime,
    Windblast,
    Possess,
    DevouringSwarm,
    Pistol,
    Crossbow,
    SleepDart,
    Heart,
    Grenade,
    StickyGrenade,
    SpringRazor,
    IncendiaryBolt,
    ExplosiveBullet,
    /// Daud's (The Knife of Dunwall)
    SummonAssassin,
    ExplosiveBolt,
    ArcMine,
    StunMine,
    Chokedust,
}

/// The original keyboard shortcut order (`DefaultUI.ini`'s `m_AutoAssign_KeyboardItemOrder`,
/// The Knife of Dunwall's after Corvo's; its arc mine set at `[29]`, so last): the first ten
/// items owned go on keys 1-0.
pub const SHORTCUT_ORDER: [Power; 20] = [
    Power::Pistol,
    Power::Crossbow,
    Power::Grenade,
    Power::SpringRazor,
    Power::Heart,
    Power::Blink,
    Power::DarkVision,
    Power::Windblast,
    Power::DevouringSwarm,
    Power::BendTime,
    Power::Possess,
    Power::SummonAssassin,
    Power::SleepDart,
    Power::IncendiaryBolt,
    Power::StickyGrenade,
    Power::ExplosiveBullet,
    Power::ExplosiveBolt,
    Power::Chokedust,
    Power::StunMine,
    Power::ArcMine,
];

/// The shortcut order for whoever plays: Daud's Void Gaze and Transversal have places of their
/// own (`DPWI_DLC06_DaudDarkVision`, `DPWI_DLC06_Transversal`: after Possession).
pub fn shortcut_order(daud: bool) -> Vec<Power> {
    let mut o = SHORTCUT_ORDER.to_vec();
    if daud {
        o.retain(|p| !matches!(p, Power::Blink | Power::DarkVision));
        let at = o.iter().position(|p| *p == Power::SummonAssassin).unwrap_or(o.len());
        o.splice(at..at, [Power::DarkVision, Power::Blink]);
    }
    o
}

impl Power {
    pub fn name(self) -> &'static str {
        match self {
            Power::Empty => "",
            Power::Blink => "Blink",
            Power::DarkVision => "Dark Vision",
            Power::BendTime => "Bend Time",
            Power::Windblast => "Wind Blast",
            Power::Possess => "Possession",
            Power::DevouringSwarm => "Devouring Swarm",
            Power::Pistol => "Pistol",
            Power::Crossbow => "Crossbow",
            Power::SleepDart => "Sleep Darts",
            Power::Heart => "The Heart",
            Power::Grenade => "Grenade",
            Power::StickyGrenade => "Sticky Grenade",
            Power::SpringRazor => "Springrazor",
            Power::IncendiaryBolt => "Incendiary Bolts",
            Power::ExplosiveBullet => "Explosive Bullets",
            Power::SummonAssassin => "Summon Assassin",
            Power::ExplosiveBolt => "Explosive Bolts",
            Power::ArcMine => "Arc Mine",
            Power::StunMine => "Stun Mine",
            Power::Chokedust => "Chokedust",
        }
    }
    /// Its name as the one playing has it (Daud's Transversal and Void Gaze).
    pub fn label(self, daud: bool) -> &'static str {
        match self {
            Power::Blink if daud => "Transversal",
            Power::DarkVision if daud => "Void Gaze",
            p => p.name(),
        }
    }
    /// Its icon as the one playing has it (`UI_EquipmentIcons_DLC06`'s for Daud's powers).
    pub fn icon_for(self, daud: bool) -> &'static str {
        match self {
            Power::Blink if daud => "ic_pow_DLC06_transversal",
            Power::DarkVision if daud => "ic_pow_DLC06_daudDarkVision",
            p => p.icon(),
        }
    }
    /// The power's name in the cooked game data (`None` for weapons).
    pub fn key(self) -> Option<&'static str> {
        Some(match self {
            Power::Blink => "Blink",
            Power::DarkVision => "DarkVision",
            Power::BendTime => "BendTime",
            Power::Windblast => "Windblast",
            Power::Possess => "Possess",
            Power::DevouringSwarm => "DevouringSwarm",
            Power::SummonAssassin => "SummonAssassin",
            _ => return None,
        })
    }
    pub fn from_key(k: &str) -> Option<Power> {
        SHORTCUT_ORDER.iter().copied().find(|p| p.key().is_some_and(|x| x.eq_ignore_ascii_case(k)))
    }
    /// The original equipment icon (`cache/ui/icons/<name>.png`).
    pub fn icon(self) -> &'static str {
        match self {
            Power::Empty => "",
            Power::Blink => "ic_pow_Blink",
            Power::DarkVision => "ic_pow_darkVision",
            Power::BendTime => "ic_pow_bendTime",
            Power::Windblast => "ic_pow_windBlast",
            Power::Possess => "ic_pow_possession",
            Power::DevouringSwarm => "ic_pow_ratSwarm",
            Power::Pistol => "ic_item_regularBullet",
            Power::Crossbow => "ic_item_regularBolt",
            Power::SleepDart => "ic_item_sleepDart",
            Power::Heart => "ic_item_heart",
            Power::Grenade => "ic_item_regularGrenade",
            Power::StickyGrenade => "ic_item_stickyGrenade",
            Power::SpringRazor => "ic_item_springRazor",
            Power::IncendiaryBolt => "ic_item_flareBolt",
            Power::ExplosiveBullet => "ic_item_explosiveBullet",
            Power::SummonAssassin => "ic_pow_DLC06_summonAssassin",
            Power::ExplosiveBolt => "ic_item_DLC06_explosiveBolt",
            Power::ArcMine => "ic_item_DLC06_arcMine",
            Power::StunMine => "ic_item_DLC06_arcMineStun",
            Power::Chokedust => "ic_item_DLC06_flashSmoke",
        }
    }
    /// Fired from the pistol or crossbow.
    pub fn is_ranged(self) -> bool {
        matches!(self, Power::Pistol | Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt | Power::ExplosiveBullet | Power::ExplosiveBolt)
    }
    /// Held in the left hand rather than cast (the gadget idle).
    pub fn is_gadget(self) -> bool {
        self.is_ranged() || matches!(self, Power::Heart | Power::Grenade | Power::StickyGrenade | Power::SpringRazor | Power::ArcMine | Power::StunMine | Power::Chokedust)
    }
    /// The inventory entry of a gadget's ammunition (`stats.items`).
    pub fn ammo_item(self) -> Option<&'static str> {
        use crate::gadgets::*;
        Some(match self {
            Power::Grenade => GRENADES,
            Power::StickyGrenade => STICKY,
            Power::SpringRazor => RAZORS,
            Power::IncendiaryBolt => FLARES,
            Power::ExplosiveBullet => EXPLOSIVE,
            Power::ExplosiveBolt => EXPLOSIVE_BOLTS,
            Power::ArcMine => ARC_MINES,
            Power::StunMine => STUN_MINES,
            Power::Chokedust => CHOKEDUST,
            _ => return None,
        })
    }
    /// Its ammunition's type (`eDisAmmoType` order), for its name.
    pub fn ammo_type(self) -> Option<u8> {
        Some(match self {
            Power::Pistol => 0,
            Power::ExplosiveBullet => 1,
            Power::Crossbow => 2,
            Power::SleepDart => 3,
            Power::IncendiaryBolt => 4,
            Power::SpringRazor => 5,
            Power::Grenade => 6,
            Power::StickyGrenade => 7,
            Power::ExplosiveBolt => 8,
            Power::ArcMine => 9,
            Power::Chokedust => 10,
            Power::StunMine => 11,
            _ => return None,
        })
    }
    /// Ammunition left (`None` for powers and the Heart).
    pub fn ammo(self, stats: &PlayerStats) -> Option<u32> {
        match self {
            Power::Pistol => Some(stats.bullets),
            Power::Crossbow => Some(stats.bolts),
            Power::SleepDart => Some(stats.sleep_darts),
            p => p.ammo_item().map(|k| stats.items.get(k).copied().unwrap_or(0)),
        }
    }
    pub fn owned(self, stats: &PlayerStats) -> bool {
        match (self.key(), self.ammo_item()) {
            (Some(k), _) => stats.power(k) > 0,
            // gadgets once found or bought
            (_, Some(item)) => stats.items.contains_key(item),
            _ => {
                self != Power::Empty
                    && stats.weapons
                    && !(stats.no_crossbow && matches!(self, Power::Crossbow | Power::SleepDart))
                    && !(stats.no_pistol && self == Power::Pistol)
                    && !(stats.no_heart && self == Power::Heart)
            }
        }
    }
    pub fn level(self, stats: &PlayerStats) -> u8 {
        self.key().map(|k| stats.power(k)).unwrap_or(1)
    }
    /// Mana per use (Dark Vision and the rest from the tweaks, percent of a 100 bar).
    pub fn mana(self, data: &Data) -> f32 {
        self.key().and_then(|k| data.active(k)).map(|a| a.mana).unwrap_or(0.0)
    }
}

/// The shortcut keys' items.
pub fn shortcuts(stats: &PlayerStats, daud: bool) -> Vec<Power> {
    shortcut_order(daud).into_iter().filter(|p| p.owned(stats)).take(10).collect()
}

/// A power was used (level scripts listen: `DisSeqEvent_PowerUsed`).
#[derive(Message, Clone, Copy)]
pub struct PowerUsed(pub Power);

/// A power was put in the left hand (`DisSeqEvent_PowerEquipped`).
#[derive(Message, Clone, Copy)]
pub struct PowerEquipped(pub Power);

#[derive(Resource)]
pub struct Powers {
    pub selected: Power,
    pub dark_vision: bool,
    /// seconds of Dark Vision left
    pub dark_vision_left: f32,
    pub aiming: bool,
    pub blink_target: Option<Vec3>,
    pub blink: Option<(Vec3, Vec3, f32)>,
    pub cooldown: f32,
    pub fov_kick: f32,
    /// Bumped on every use of a power or weapon (`cast_power` is what was used).
    pub cast_seq: u32,
    pub cast_power: Power,
    /// the left hand's press that ended a possession, held: it does nothing more
    swallow: bool,
    /// a grenade cooking in the hand: which, and for how long (`GrenadeCooking`)
    pub cooking: Option<(Power, f32)>,
    /// Blink held back after each move or cancel (`m_fCooldownTime`), and Dark Vision's time
    /// in a fight (`m_fDesactivateInCombatTimer`)
    pub blink_lock: f32,
    pub dv_combat: f32,
    /// each gun's magazine (pistol, crossbow): the ammunition in it and the rounds spent from
    /// it; its reload under way (s, counted while it is in hand); the arms' reload clip to
    /// play (bumped, which)
    pub magazine: [Option<(Power, u32)>; 2],
    pub reload: [f32; 2],
    pub reload_anim: (u32, &'static str),
    /// Wind Blasts under way
    pub gusts: Vec<Gust>,
    /// Corvo's velocity as a Blink begins, his again as it ends; whether it ends crouched
    pub blink_vel: Vec3,
    pub blink_crouch: bool,
    /// the landing's kind (its marker), and the marker shown
    pub blink_spot: Option<BlinkSpot>,
    pub blink_shown: Option<BlinkSpot>,
}

/// A Wind Blast under way: since the cast (s), from where and which way, its level; who it
/// reaches (gathered once as it starts: each with its push and damage factor) and has not yet.
#[derive(Clone)]
pub struct Gust {
    pub t: f32,
    eye: Vec3,
    dir: Vec3,
    from: Vec3,
    level: u8,
    targets: Option<Vec<(Entity, f32)>>,
    /// where it meets the world, how far from the eye: each its puff as the front reaches it
    impacts: Vec<(Vec3, f32)>,
}

/// The directions Wind Blast's gust is traced along for the world it meets: its middle and two
/// rings within its cone, as fractions of the half angle (our reading of its sweep).
const GUST_RINGS: [(f32, usize); 3] = [(0.0, 1), (0.45, 6), (0.85, 8)];
/// The least distance between two of its puffs where it meets the world (m: our reading, the
/// puff's dust metres across, so one wall takes a few, not one a ray).
const GUST_IMPACT_SPACING: f32 = 2.0;

/// The cast clip's `DisNotify_WindBlast` (s): when the blast starts (roshan's r614/blink
/// research).
const WINDBLAST_NOTIFY: f32 = 0.27;
/// How hard Wind Blast must push a character to knock it off its feet, m/s (our reading: the
/// original's momentum against the pawn's `m_fMomentumImpulseMax`, 400 uu/s, is native).
const KNOCKDOWN_SPEED: f32 = 6.0;

impl Default for Powers {
    fn default() -> Self {
        Self {
            selected: Power::Empty,
            dark_vision: false,
            dark_vision_left: 0.0,
            aiming: false,
            blink_target: None,
            blink: None,
            cooldown: 0.0,
            fov_kick: 0.0,
            cast_seq: 0,
            cast_power: Power::Blink,
            swallow: false,
            cooking: None,
            blink_lock: 0.0,
            dv_combat: 0.0,
            magazine: [None; 2],
            reload: [0.0; 2],
            reload_anim: (0, ""),
            gusts: Vec::new(),
            blink_vel: Vec3::ZERO,
            blink_crouch: false,
            blink_spot: None,
            blink_shown: None,
        }
    }
}

/// The time a gun's fire action takes before the next shot or its reload (`Pistol_Fire`:
/// 0.3984 s; the crossbow's 24 frames).
const FIRE_ACTION: f32 = 0.4;

/// A gun's reload, as the original's stages time it (`DisTweaks_ReloadGunPlayer`: in,
/// `m_fReloadTime` over `ReloadSpeed`, out; with `FastReload` its quick action, that long, as
/// roshan's r614/blink research timed them): seconds, and the quick one or not.
fn reload_time(data: &Data, stats: &PlayerStats, w: crate::aim::Weapon) -> (f32, bool) {
    let fast = crate::aim::upgrade_sum(data, stats, w, "FastReload");
    if fast > 0.0 {
        return (fast, true);
    }
    let speed = crate::aim::upgrade_sum(data, stats, w, "ReloadSpeed");
    let [i, mid, o] = reload_stages(w);
    (i + if speed > 0.0 { mid / speed } else { mid } + o, false)
}

/// A gun's reload stages (in, the timed middle, out: s).
fn reload_stages(w: crate::aim::Weapon) -> [f32; 3] {
    match w {
        crate::aim::Weapon::Pistol => [0.426, 0.75, 0.302],
        crate::aim::Weapon::Crossbow => [0.225 / 1.3, 1.0, 0.169 / 1.3],
    }
}

/// Rounds a gun's magazine holds (`1 + MagazineBonus` for the pistol, one bolt).
fn magazine_size(data: &Data, stats: &PlayerStats, w: crate::aim::Weapon) -> u32 {
    1 + crate::aim::upgrade_sum(data, stats, w, "MagazineBonus").max(0.0) as u32
}

/// A gun's reload begins (after `delay`), for the ammunition `sel`.
fn start_reload(powers: &mut Powers, data: &Data, stats: &PlayerStats, sel: Power, delay: f32) {
    let Some(w) = crate::aim::Weapon::of(sel) else { return };
    powers.reload[w as usize] = reload_time(data, stats, w).0 + delay + 1e-4;
    powers.magazine[w as usize] = Some((sel, 0));
}

/// Whether the gun has a round of `sel` in it; empty, or loaded with another ammunition, it
/// reloads first.
fn chamber(powers: &mut Powers, data: &Data, stats: &PlayerStats, sel: Power) -> bool {
    let Some(w) = crate::aim::Weapon::of(sel) else { return true };
    let i = w as usize;
    if powers.reload[i] > 0.0 {
        return false;
    }
    match powers.magazine[i] {
        Some((p, spent)) if p == sel && spent < magazine_size(data, stats, w) => true,
        None => {
            powers.magazine[i] = Some((sel, 0));
            true
        }
        _ => {
            start_reload(powers, data, stats, sel, 0.0);
            false
        }
    }
}

/// A round fired: the magazine emptied, the gun reloads after its fire action while
/// ammunition is held.
fn spend_round(powers: &mut Powers, data: &Data, stats: &PlayerStats, sel: Power, held: u32) {
    let Some(w) = crate::aim::Weapon::of(sel) else { return };
    let i = w as usize;
    let spent = match &mut powers.magazine[i] {
        Some((p, n)) if *p == sel => {
            *n += 1;
            *n
        }
        _ => {
            powers.magazine[i] = Some((sel, 1));
            1
        }
    };
    if spent >= magazine_size(data, stats, w) && held > 0 {
        start_reload(powers, data, stats, sel, FIRE_ACTION);
    }
}

/// The gun in hand's reload goes on; its stages' clips start as they come (and the sound its
/// first stage's notify plays: `Snd_Weapon_Gun_Corvo_Reload`, `Snd_Crossbow_P_Reload`).
fn tick_reload(powers: &mut Powers, data: &Data, stats: &PlayerStats, dt: f32) -> Option<&'static str> {
    let w = crate::aim::Weapon::of(powers.selected)?;
    let i = w as usize;
    let before = powers.reload[i];
    if before <= 0.0 {
        return None;
    }
    let left = (before - dt).max(0.0);
    powers.reload[i] = left;
    let (t, fast) = reload_time(data, stats, w);
    let pistol = w == crate::aim::Weapon::Pistol;
    let out = reload_stages(w)[2];
    let clip = if before > t && left <= t {
        Some(match (pistol, fast) {
            (true, true) => "Pistol_FastReload",
            (true, false) => "Pistol_Reload_In",
            (false, true) => "Crossbow_RapidReload",
            (false, false) => "Crossbow_Reload_In",
        })
    } else if !fast && before > out && left <= out {
        Some(if pistol { "Pistol_Reload_Out" } else { "Crossbow_Reload_Out" })
    } else {
        None
    };
    if let Some(c) = clip {
        powers.reload_anim = (powers.reload_anim.0.wrapping_add(1), c);
    }
    (before > t && left <= t).then_some(if pistol { "Snd_Weapon_Gun_Corvo_Reload" } else { "Snd_Crossbow_P_Reload" })
}

#[derive(Component)]
struct BlinkMarker;

/// A character an incendiary bolt struck: seconds before it burns.
#[derive(Component)]
pub struct Ignite(pub f32);

/// The flames take those incendiary bolts struck.
fn ignite(mut commands: Commands, time: Res<Time>, tc: Res<TimeControl>, mut q: Query<(Entity, &mut Ignite, &Transform)>, mut hits: MessageWriter<NpcHit>) {
    let dt = time.delta_secs() * tc.world_scale();
    for (e, mut i, t) in &mut q {
        i.0 -= dt;
        if i.0 <= 0.0 {
            hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::Fire, from: t.translation });
            commands.entity(e).try_remove::<Ignite>();
        }
    }
}

/// A hit this high above a character's middle is to its head.
const HEAD: f32 = 0.58;
/// Below the hips, from a character's middle: its legs (a bolt's limb there).
const LEGS: f32 = -0.35;

#[derive(Component)]
pub struct Projectile {
    pub vel: Vec3,
    pub kind: HitKind,
    pub life: f32,
    pub stuck: bool,
    pub recoverable: bool,
    attachment: Option<BoltAttachment>,
}

/// Player-owned power state must travel with a save: a projectile saved in
/// stopped time must not resume moving merely because its map was reloaded.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PowersSave {
    selected: Power,
    dark_vision_left: f32,
    cooldown: f32,
    cooking: Option<(Power, f32)>,
    blink: Option<([f32; 3], [f32; 3], f32)>,
    bend_remaining: f32,
    world_dilation: f32,
    scripted: Option<(f32, f32)>,
    #[serde(default = "one")]
    player_dilation: f32,
}

fn one() -> f32 {
    1.0
}

impl PowersSave {
    pub fn capture(powers: &Powers, time: &TimeControl) -> Self {
        Self {
            selected: powers.selected,
            dark_vision_left: if powers.dark_vision { powers.dark_vision_left } else { 0.0 },
            cooldown: powers.cooldown,
            cooking: powers.cooking,
            blink: powers.blink.map(|(a, b, t)| (a.to_array(), b.to_array(), t)),
            bend_remaining: time.bend_remaining,
            world_dilation: time.world_dilation,
            scripted: time.scripted,
            player_dilation: time.player_dilation,
        }
    }

    pub fn restore(self, powers: &mut Powers, time: &mut TimeControl) {
        *powers = Powers {
            selected: self.selected,
            dark_vision: self.dark_vision_left > 0.0,
            dark_vision_left: self.dark_vision_left,
            cooldown: self.cooldown,
            cooking: self.cooking,
            blink: self.blink.map(|(a, b, t)| (Vec3::from(a), Vec3::from(b), t)),
            ..default()
        };
        *time = TimeControl {
            bend_remaining: self.bend_remaining,
            world_dilation: self.world_dilation,
            scripted: self.scripted,
            player_dilation: self.player_dilation,
            bend_on: self.bend_remaining > 0.0,
            ..default()
        };
    }
}

#[derive(Clone, Copy)]
struct BoltAttachment {
    npc: Entity,
    bone: Option<usize>,
    offset: Vec3,
    rotation: Quat,
}

impl Projectile {
    fn physics(data: &Data, kind: HitKind) -> (f32, f32) {
        let (prefix, speed, gravity) = match kind {
            HitKind::SleepDart => ("arrow_sleep", 0.7, 0.4),
            HitKind::Fire => ("arrow_flare", 0.2, 0.5),
            HitKind::ExplosiveBolt => ("arrow_explosive", data.pawn("arrow.m_fSpeedMultiplier", 1.0), 0.5),
            _ => ("arrow", 1.0, 1.3),
        };
        let speed = data.pawn("crossbow.m_fArrowFireSpeed", 20000.0) * UU * data.pawn(&format!("{prefix}.m_fSpeedMultiplier"), speed).max(0.0);
        let gravity = data.pawn("world.DefaultGravityZ", -1500.0) * UU * data.pawn(&format!("{prefix}.m_fGravityMultiplier"), gravity).max(0.0);
        (speed, gravity)
    }
    fn survives_body_hit(base_chance: f32, modifier: f32, roll: f32) -> bool {
        roll >= (base_chance + modifier).clamp(0.0, 1.0)
    }
}

fn recover_bolt(stats: &mut PlayerStats, attrs: &Attrs) -> bool {
    if stats.bolts >= attrs.bolt_capacity { return false; }
    stats.bolts += 1;
    true
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectileSave {
    position: [f32; 3],
    rotation: [f32; 4],
    velocity: [f32; 3],
    kind: HitKind,
    life: f32,
    stuck: bool,
    recoverable: bool,
    /// Stable NPC spawner and bone index; the offset is local to that bone.
    attachment: Option<(u32, Option<usize>, [f32; 3], [f32; 4])>,
}

#[derive(Resource, Default)]
pub struct ProjectileRestore(pub Option<Vec<ProjectileSave>>);

pub(crate) fn save_projectiles<'a>(projectiles: impl IntoIterator<Item = (&'a Projectile, &'a Transform)>, npcs: impl IntoIterator<Item = (Entity, u32)>) -> Vec<ProjectileSave> {
    let npcs: std::collections::HashMap<_, _> = npcs.into_iter().collect();
    projectiles.into_iter().map(|(p, t)| ProjectileSave {
        position: t.translation.to_array(), rotation: t.rotation.to_array(), velocity: p.vel.to_array(),
        kind: p.kind, life: p.life, stuck: p.stuck, recoverable: p.recoverable,
        attachment: p.attachment.and_then(|a| Some((*npcs.get(&a.npc)?, a.bone, a.offset.to_array(), a.rotation.to_array()))),
    }).collect()
}

pub(crate) fn projectile_model(entity: &mut EntityCommands, assets: Option<&GameAssets>, kind: HitKind) {
    if kind == HitKind::Fire {
        entity.insert((
            PointLight { color: Color::srgb(1.0, 0.55, 0.2), intensity: 60_000.0, range: 5.0, ..default() },
            crate::fxlight::FxLight { color: Vec3::new(1.0, 0.55, 0.2), brightness: 3.0, radius: 5.0 },
        ));
    }
    let prop = match kind { HitKind::SleepDart => "bolt_sleep", HitKind::Fire => "bolt_flare", HitKind::ExplosiveBolt => "bolt_explosive", _ => "bolt" };
    if let Some(parts) = assets.and_then(|a| a.props.get(prop).or(a.props.get("bolt"))) {
        entity.with_children(|c| {
            for (mesh, mat) in &parts.parts {
                let mut m = c.spawn((Mesh3d(mesh.clone()), MeshTag(0), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))));
                mat.apply(&mut m);
            }
        });
    }
}

fn restore_projectiles(mut commands: Commands, mut pending: ResMut<ProjectileRestore>, old: Query<Entity, With<Projectile>>, npcs: Query<(Entity, &crate::npc::FromSpawner)>, assets: Option<Res<GameAssets>>) {
    let Some(saved) = pending.0.take() else { return };
    for e in &old { commands.entity(e).despawn(); }
    let npcs: std::collections::HashMap<_, _> = npcs.iter().map(|(e, s)| (s.0, e)).collect();
    for p in saved {
        let attachment = p.attachment.and_then(|(s, bone, offset, rotation)| Some(BoltAttachment { npc: *npcs.get(&s)?, bone, offset: Vec3::from(offset), rotation: Quat::from_array(rotation) }));
        let mut e = commands.spawn((
            Projectile { vel: Vec3::from(p.velocity), kind: p.kind, life: p.life, stuck: p.stuck, recoverable: p.recoverable, attachment },
            Transform::from_translation(Vec3::from(p.position)).with_rotation(Quat::from_array(p.rotation)),
            Visibility::default(), DespawnOnExit(GameState::InGame),
        ));
        projectile_model(&mut e, assets.as_deref(), p.kind);
    }
}

/// Ordinary bolts use their own interaction, rather than masquerading as a
/// level-authored pickup (which would fire the wrong Kismet pickup event).
#[allow(clippy::too_many_arguments)]
pub(crate) fn bolt_focus(
    mut commands: Commands,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    mut focus: ResMut<crate::interact::InteractFocus>,
    bolts: Query<(Entity, &Projectile, &Transform)>,
    positions: Query<&Transform>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    player: Query<(Entity, &Player)>,
    (mut stats, attrs, mut held, carry, possession, paused): (ResMut<PlayerStats>, Res<Attrs>, ResMut<crate::props::Held>, Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::hud::Paused>),
    rapier: ReadRapierContext,
    mut sound: MessageWriter<PostEvent>,
    mut log: ResMut<crate::pickuplog::PickupLog>,
    mut messages: ResMut<HudMessages>,
) {
    let (Ok((pe, p)), Ok(camera), Ok(ctx)) = (player.single(), cam.single(), rapier.single()) else { return };
    if paused.0 || stats.dead || p.locked || held.busy() || carry.carrying() || possession.host.is_some() { return; }
    let eye = camera.translation();
    let direction = camera.forward().as_vec3();
    let nearest = bolts.iter().filter(|(_, b, _)| b.recoverable).filter_map(|(e, bolt, t)| {
        let to = t.translation - eye;
        let distance = to.length();
        if distance > 2.4 || distance < 0.05 || to.dot(direction) / distance < 0.94 { return None; }
        let not_host = |collider| bolt.attachment.is_none_or(|a| collider != a.npc);
        let filter = QueryFilter::default().exclude_collider(pe).predicate(&not_host).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
        if ctx.cast_ray(eye, to / distance, (distance - 0.08).max(0.0), true, filter).is_some() { return None; }
        Some((e, distance, bolt.attachment.map(|a| a.npc)))
    }).min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((e, distance, host)) = nearest else { return };
    if focus.1.filter(|focused| Some(*focused) != host).and_then(|e| positions.get(e).ok()).is_some_and(|t| t.translation.distance(eye) + 0.1 < distance) { return; }
    focus.0 = true;
    focus.1 = Some(e);
    focus.2 = format!("{} Pick up", crate::bindings::hint(crate::bindings::Act::Use));
    focus.3 = "Bolt".into();
    if keys.just_pressed(bind.key(crate::bindings::Act::Use)) {
        if recover_bolt(&mut stats, &attrs) {
            // Consume the short pickup action, so the same Use press cannot
            // also grab a loose prop behind the disappearing bolt.
            held.1 = held.1.max(0.1);
            commands.entity(e).despawn();
            focus.1 = None;
            focus.2.clear();
            focus.3.clear();
            log.add("Crossbow Bolts +1", crate::pickuplog::ammo_icon(2));
            sound.write(PostEvent::named("Snd_UI_Ingame_Ammo_Pickup", None));
        } else {
            messages.push("You can't carry more ammo of this type");
            sound.write(PostEvent::named("Snd_UI_Ingame_Max_Ammo", None));
        }
    }
}

fn reset_powers(mut p: ResMut<Powers>, stats: Res<PlayerStats>, data: Res<Data>) {
    *p = Powers::default();
    p.selected = shortcuts(&stats, data.is_daud()).into_iter().find(|p| p.key().is_some()).unwrap_or(Power::Empty);
}

/// Blink's target: where its marker's effect stands (`BlinkSpot::effect`).
fn spawn_blink_marker(mut commands: Commands) {
    commands.spawn((BlinkMarker, Transform::default(), Visibility::Hidden, DespawnOnExit(GameState::InGame)));
}

/// The number keys' ten slots, as the original's power wheel keeps them
/// (`DisGFxMoviePlayerPowerWheel`, decoded by roshan's r614/blink research): each holds an item
/// or nothing. Those the player hasn't set (or emptied) himself take, in turn from the first,
/// the first item of the auto-assignment order (`m_AutoAssign_KeyboardItemOrder`: the
/// shortcut order) that he has, that no slot holds and that is of the slot's kind
/// (`m_AutoAssign_KeyboardShortcut`: weapons 1-5, the Heart 6, powers 7-0).
#[derive(Resource, Default)]
pub struct Shortcuts {
    pub slots: [Option<Power>; 10],
    /// the slots he set (or emptied) himself (`m_ShortcutPlayerAssignFlags`)
    pub mine: [bool; 10],
    /// the slot he last took (`m_LastUsedShortcutIndex`): the mouse wheel steps on from it
    pub last: usize,
    /// a number key held: its slot, shown on the dock, taken as it's let go
    pub held: Option<usize>,
    /// the mouse wheel stepping: the slot shown, and how long before it's taken
    pub stepping: Option<(usize, f32)>,
}

/// The mouse wheel's steps settle this long after the last (`m_fShortcutsInputTimer`).
const SHORTCUT_SETTLE: f32 = 0.33;

impl Shortcuts {
    /// The kind of item a slot takes: 0 weapons, 1 the Heart, 2 powers.
    fn kind(slot: usize) -> u8 {
        match slot {
            0..=4 => 0,
            5 => 1,
            _ => 2,
        }
    }
    fn kind_of(p: Power) -> u8 {
        if p == Power::Heart {
            1
        } else if p.key().is_some() {
            2
        } else {
            0
        }
    }
    /// Empty the slots of what he no longer has, then fill the free ones (`SetShortcuts`).
    pub fn refresh(&mut self, stats: &PlayerStats, daud: bool) {
        for s in self.slots.iter_mut() {
            if s.is_some_and(|p| !p.owned(stats)) {
                *s = None;
            }
        }
        let order = shortcut_order(daud);
        for i in 0..10 {
            if self.slots[i].is_some() || self.mine[i] {
                continue;
            }
            let pick = order.iter().copied().find(|p| Shortcuts::kind_of(*p) == Shortcuts::kind(i) && p.owned(stats) && !self.slots.contains(&Some(*p)));
            self.slots[i] = pick;
        }
    }
    /// Put an item on a slot (or take it off, when it's there already): his own choice.
    pub fn assign(&mut self, slot: usize, p: Power) -> bool {
        let on = self.slots[slot] != Some(p);
        for s in self.slots.iter_mut() {
            if *s == Some(p) {
                *s = None;
            }
        }
        self.slots[slot] = on.then_some(p);
        self.mine[slot] = true;
        on
    }
    /// The next slot holding something, on from `from` the given way (round the end).
    fn step(&self, from: usize, way: i32) -> Option<usize> {
        (1..=10).map(|k| (from as i32 + way * k).rem_euclid(10) as usize).find(|i| self.slots[*i].is_some())
    }
}

#[allow(clippy::too_many_arguments)]
fn select_power(
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    stats: Res<PlayerStats>,
    mut powers: ResMut<Powers>,
    mut equipped: MessageWriter<PowerEquipped>,
    mut sfx: MessageWriter<PostEvent>,
    (possession, choice, paused): (Res<crate::possession::Possession>, Res<crate::choice::Choice>, Res<crate::hud::Paused>),
    data: Res<Data>,
    (mut slots, wheel, time): (ResMut<Shortcuts>, Res<crate::wheel::Wheel>, Res<Time>),
) {
    // the number keys answer a choice on screen
    if paused.0 || possession.host.is_some() || choice.pending.is_some() {
        return;
    }
    let list = shortcuts(&stats, data.is_daud());
    // (filled again as the wheel or the dock comes up, not while the wheel is open:
    // `SetShortcuts` on each start)
    if !wheel.open {
        slots.refresh(&stats, data.is_daud());
    }
    let mut sel = None;
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::Digit0,
    ];
    for (i, k) in digits.iter().enumerate() {
        if keys.just_pressed(*k) {
            if let Some(p) = wheel.hovered() {
                // the wheel open: what it points at goes to the slot (or leaves it)
                let on = slots.assign(i, p);
                sfx.write(PostEvent::named(if on { "UI_P_AssignShortcut" } else { "UI_P_UnassignShortcut" }, None));
            } else {
                // shown on the dock while held, taken as it's let go
                slots.held = Some(i);
                slots.stepping = None;
            }
        }
        if keys.just_released(*k) && slots.held == Some(i) {
            slots.held = None;
            if let Some(p) = slots.slots[i] {
                sel = Some(p);
                slots.last = i;
            }
        }
    }
    // the mouse wheel steps through the filled slots from the last taken (not with a key held
    // or the wheel open), each step putting off the choice; it settles on the one shown
    if scroll.delta.y.abs() > 0.0 && !wheel.open && slots.held.is_none() {
        let way = if scroll.delta.y > 0.0 { -1 } else { 1 };
        let from = slots.stepping.map(|(s, _)| s).unwrap_or(slots.last);
        if let Some(next) = slots.step(from, way) {
            slots.stepping = Some((next, SHORTCUT_SETTLE));
        }
    }
    if let Some((slot, left)) = slots.stepping {
        let left = left - time.delta_secs();
        if left <= 0.0 {
            slots.stepping = None;
            slots.last = slot;
            sel = slots.slots[slot];
        } else {
            slots.stepping = Some((slot, left));
        }
    }
    // lost what was held (a fresh map, a script took it), or an empty hand and something to hold
    if sel.is_none() && ((powers.selected != Power::Empty && !powers.selected.owned(&stats)) || (powers.selected == Power::Empty && !list.is_empty())) {
        // a power first (the left hand's natural use), else any item
        sel = Some(list.iter().copied().find(|p| p.key().is_some()).or(list.iter().copied().find(|p| !p.is_ranged())).or(list.first().copied()).unwrap_or(Power::Empty));
    }
    if let Some(s) = sel {
        if s != powers.selected {
            powers.selected = s;
            powers.aiming = false;
            equipped.write(PowerEquipped(s));
            sfx.write(PostEvent::named(if s.key().is_some() { "Snd_Power_Switch" } else { "Snd_Power_Equip" }, None));
        }
    }
}

/// Compute where Blink would put the player (capsule centre), including ledge grabs.
/// Where a Blink lands, as its marker shows it: on the ground, on a ledge it climbs, or in
/// the air it falls from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlinkSpot {
    Ground,
    Ledge,
    Fall,
}

impl BlinkSpot {
    /// The marker's effect (`Twk_Blink`'s `m_pGroundPS`, `m_pMantlePS`, `m_pLowFallPS`).
    fn effect(self) -> &'static str {
        match self {
            BlinkSpot::Ground => "blink_ground",
            BlinkSpot::Ledge => "blink_mantle",
            BlinkSpot::Fall => "blink_fall",
        }
    }
}

fn blink_destination(ctx: &RapierContext, player: Entity, eye: Vec3, dir: Vec3, range: f32, vert: f32) -> Option<(Vec3, bool, BlinkSpot)> {
    let filter = QueryFilter::default().exclude_collider(player).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
    // upward blinks reach less far (the original's vertical limit)
    let range = if dir.y > 0.0 { range.min(vert / dir.y.max(1e-3)) } else { range };
    let mut point = eye + dir * range;
    let mut ledge = None;
    if let Some((_, hit)) = ctx.cast_ray_and_get_normal(eye, dir, range, true, filter) {
        let hp = hit.point;
        point = hp - dir * (RADIUS + 0.15);
        if hit.normal.y.abs() < 0.5 {
            // wall: is there a ledge just above?
            let flat = dir.with_y(0.0).normalize_or_zero();
            let probe = hp + flat * 0.45 + Vec3::Y * 2.0;
            if let Some((_, g)) = ctx.cast_ray_and_get_normal(probe, Vec3::NEG_Y, 2.4, true, filter) {
                if g.normal.y > 0.7 && g.point.y > hp.y - 0.3 {
                    let center = g.point + Vec3::Y * (STAND_HALF + RADIUS + 0.06);
                    if ctx.cast_ray(g.point + Vec3::Y * 0.05, Vec3::Y, 1.75, true, filter).is_none() {
                        ledge = Some(center);
                    }
                }
            }
        } else if hit.normal.y > 0.5 {
            // floor: stand on it
            point = hp + Vec3::Y * (STAND_HALF + RADIUS + 0.06);
        }
    }
    let mut dest = ledge.unwrap_or(point);
    if ledge.is_none() {
        // settle onto the ground below if close
        if let Some((_, g)) = ctx.cast_ray_and_get_normal(dest, Vec3::NEG_Y, STAND_HALF + RADIUS + 1.2, true, filter) {
            if g.normal.y > 0.6 {
                dest = g.point + Vec3::Y * (STAND_HALF + RADIUS + 0.06);
            }
        }
    }
    // make sure the capsule fits
    let shape = Collider::capsule_y(STAND_HALF, RADIUS * 0.95);
    let mut blocked = false;
    ctx.intersect_shape(dest, Quat::IDENTITY, &*shape.raw, filter, |_| {
        blocked = true;
        false
    });
    if blocked {
        // try a little higher (stairs/edges)
        let up = dest + Vec3::Y * 0.4;
        let mut b2 = false;
        ctx.intersect_shape(up, Quat::IDENTITY, &*shape.raw, filter, |_| {
            b2 = true;
            false
        });
        if b2 {
            // (where only a crouch fits, he arrives crouched: the original asks only room for
            // its crawl height, as roshan's r614/blink research found)
            let low = dest - Vec3::Y * (STAND_HALF - crate::player::CROUCH_HALF);
            let crouch = Collider::capsule_y(crate::player::CROUCH_HALF, RADIUS * 0.95);
            let mut b3 = false;
            ctx.intersect_shape(low, Quat::IDENTITY, &*crouch.raw, filter, |_| {
                b3 = true;
                false
            });
            return (!b3).then_some((low, true, spot_of(ctx, low, ledge.is_some(), filter)));
        }
        dest = up;
    }
    Some((dest, false, spot_of(ctx, dest, ledge.is_some(), filter)))
}

/// The kind of a Blink's landing: a ledge, else ground under its feet, else the air.
fn spot_of(ctx: &RapierContext, dest: Vec3, ledge: bool, filter: QueryFilter) -> BlinkSpot {
    if ledge {
        BlinkSpot::Ledge
    } else if ctx.cast_ray(dest, Vec3::NEG_Y, STAND_HALF + RADIUS + 0.35, true, filter).is_some() {
        BlinkSpot::Ground
    } else {
        BlinkSpot::Fall
    }
}

/// Unreal units to metres.
const UU: f32 = 0.01;

#[allow(clippy::too_many_arguments)]
fn use_power(
    mut commands: Commands,
    (time, mouse, cursor, scripted, settings, hard): (Res<Time>, Res<ButtonInput<MouseButton>>, Single<&CursorOptions>, Option<Res<crate::script::Scripted>>, Res<crate::settings::Settings>, Res<crate::player::HardLanding>),
    rapier: ReadRapierContext,
    (assets, data, attrs, tune, level_info): (Option<Res<GameAssets>>, Res<Data>, Res<Attrs>, Res<crate::musicbox::Tune>, Option<Res<crate::level::LevelInfo>>),
    mut powers: ResMut<Powers>,
    mut stats: ResMut<PlayerStats>,
    mut tc: ResMut<TimeControl>,
    mut msgs: ResMut<HudMessages>,
    (mut hits, mut noise, mut sfx, mut used, mut world_hits): (MessageWriter<NpcHit>, MessageWriter<Noise>, MessageWriter<PostEvent>, MessageWriter<PowerUsed>, MessageWriter<WorldDamage>),
    mut player: Query<(Entity, &Transform, &mut Player)>,
    (cam, surfaces, strikeables, mut struck): (Query<&GlobalTransform, With<PlayerCamera>>, Query<&ColliderSurfaces>, Query<&crate::gameplay::Strikeable>, MessageWriter<crate::gameplay::Struck>),
    mut marker: Query<(Entity, &mut Transform, &mut Visibility), (With<BlinkMarker>, Without<Player>)>,
    npcs: Query<(Entity, &Transform, &Npc), (Without<Player>, Without<BlinkMarker>)>,
    (mut possession, mut start_possess, hosts, possess_overrides): (
        ResMut<crate::possession::Possession>,
        MessageWriter<crate::possession::PossessRequest>,
        Query<(Entity, &GlobalTransform), (With<crate::possession::Host>, Without<crate::possession::Possessed>)>,
        Res<crate::possession::PossessOverrides>,
    ),
    (mut swarm, mut gadget, mut blast, mut kill_cams, mut decals): (
        MessageWriter<crate::swarm::SummonSwarm>,
        MessageWriter<crate::gadgets::UseGadget>,
        MessageWriter<crate::gadgets::Explosion>,
        MessageWriter<crate::killcam::StartKillCam>,
        MessageWriter<crate::decals::SpawnDecal>,
    ),
    (mut aim, held, paused, mut summon, nav, mut fx, muzzles, mut tagged): (
        ResMut<crate::aim::Aim>,
        Res<crate::props::Held>,
        Res<crate::hud::Paused>,
        MessageWriter<crate::summon::Summon>,
        Option<Res<crate::navmesh::NavGrid>>,
        MessageWriter<crate::particles::SpawnEffect>,
        Query<(Entity, &GlobalTransform, &crate::arms::Muzzle)>,
        Query<(&crate::particles::EffectTag, &mut crate::particles::ParticleEmitter)>,
    ),
) {
    if paused.0 {
        // Menu mouse releases must not spend mana or queue a Blink on resume.
        powers.aiming = false;
        powers.blink_target = None;
        if let Ok((_, _, mut visibility)) = marker.single_mut() { *visibility = Visibility::Hidden; }
        return;
    }
    let dt = time.delta_secs();
    powers.cooldown = (powers.cooldown - dt).max(0.0);
    powers.fov_kick = (powers.fov_kick - dt * 3.0).max(0.0);
    if let Some(s) = tick_reload(&mut powers, &data, &stats, dt * tc.player_rate()) {
        sfx.write(PostEvent::named(s, None));
    }
    let Ok((pe, pt, mut p)) = player.single_mut() else { return };
    let Ok(cg) = cam.single() else { return };
    let eye = cg.translation();
    let dir = cg.forward().as_vec3();
    let Ok(ctx) = rapier.single() else { return };
    powers.blink_lock = (powers.blink_lock - dt).max(0.0);
    // Dark Vision lasts its duration, and ends a while into a fight (as roshan's r614/blink
    // research decoded the original: `m_fDesactivateInCombatTimer`)
    if powers.dark_vision {
        let fighting = npcs.iter().any(|(_, _, n)| !n.is_down() && n.hostile() && n.mode == crate::npc::Mode::Combat);
        powers.dv_combat = if fighting { powers.dv_combat + dt } else { 0.0 };
        if powers.dv_combat > data.active("DarkVision").and_then(|a| a.params.get("m_fDesactivateInCombatTimer")).copied().unwrap_or(1.0) {
            powers.dark_vision_left = 0.0;
        }
        powers.dark_vision_left -= dt;
        if powers.dark_vision_left <= 0.0 {
            powers.dark_vision = false;
            sfx.write(PostEvent::named(data.power_sound("DarkVision", 1, "m_pDeactivationSoundEvent").unwrap_or("Darkvision_Stop"), None));
        }
    }
    let grabbed = cursor.grab_mode != CursorGrabMode::None || scripted.is_some();
    // (a hard landing takes his left hand a moment: `m_bActionUsesLeftHand`)
    let can_act = grabbed && !stats.dead && !p.locked && !held.busy() && powers.blink.is_none() && tc.wheel >= 1.0 && hard.0 <= 0.0;
    let sel = powers.selected;
    let level = sel.level(&stats);
    // possessing: the left hand only ends it
    if possession.host.is_some() {
        if can_act && mouse.just_pressed(MouseButton::Right) {
            possession.exit_requested = true;
            powers.swallow = true;
        }
        return;
    }
    if powers.swallow {
        if mouse.pressed(MouseButton::Right) {
            return;
        }
        powers.swallow = false;
    }
    // Blink aiming
    let blink_range = (data.power_f("Blink", level, "m_HorizDistance", 1100.0)).min(data.power_f("Blink", level, "m_Distance", 2000.0)) * UU;
    let blink_vert = data.power_f("Blink", level, "m_VertDistance", 500.0) * UU;
    // (aiming Blink needs its mana, and waits out its cooldown after the last move or cancel)
    let blink_ready = stats.mana >= Power::Blink.mana(&data) && powers.blink_lock <= 0.0;
    if sel == Power::Blink && can_act && mouse.pressed(MouseButton::Right) && (blink_ready || powers.aiming) {
        powers.aiming = true;
        let dest = blink_destination(&ctx, pe, eye, dir, blink_range, blink_vert);
        powers.blink_target = dest.map(|d| d.0);
        powers.blink_crouch = dest.is_some_and(|d| d.1);
        powers.blink_spot = dest.map(|d| d.2);
    } else if sel == Power::Blink && can_act && mouse.just_pressed(MouseButton::Right) && stats.mana < Power::Blink.mana(&data) {
        out_of_mana(&mut powers, &mut sfx, &mut msgs);
    }
    if let Ok((me, mut mt, mut mv)) = marker.single_mut() {
        // the marker's effect for the landing (the ground, a ledge, the air), carried by it
        let want = match (powers.aiming && sel == Power::Blink, powers.blink_target) {
            (true, Some(t)) => {
                *mv = Visibility::Inherited;
                // (at its feet)
                mt.translation = t - Vec3::Y * (STAND_HALF + RADIUS);
                powers.blink_spot
            }
            _ => {
                *mv = Visibility::Hidden;
                None
            }
        };
        if want != powers.blink_shown {
            if std::env::var("DH_BLINK_LOG").is_ok() {
                info!("blink marker: {:?} at {:.2}", want, mt.translation);
            }
            for (tag, mut em) in &mut tagged {
                if tag.0 == me {
                    em.stop();
                }
            }
            if let Some(spot) = want {
                // (a map without the ledge's marker shows the ground's)
                let has = |n: &str| level_info.as_ref().is_some_and(|l| l.scene.effects.iter().any(|e| e.0 == n));
                let name = if has(spot.effect()) { spot.effect() } else { BlinkSpot::Ground.effect() };
                fx.write(crate::particles::SpawnEffect { follow: Some(me), tag: Some(me), secs: 600.0, ..crate::particles::SpawnEffect::at(name, Vec3::ZERO) });
            }
            powers.blink_shown = want;
        }
    }
    let cost = sel.mana(&data);
    if powers.aiming && sel == Power::Blink && mouse.just_released(MouseButton::Right) {
        powers.aiming = false;
        // (moved or not, Blink waits its cooldown after the move: `m_fCooldownTime`)
        powers.blink_lock = data.active("Blink").and_then(|a| a.params.get("m_fCooldownTime")).copied().unwrap_or(1.0) + 0.27;
        if let Some(target) = powers.blink_target.take() {
            if tune.inhibited {
                // an Overseer's music box: the Mark fails
                msgs.push("Your powers are blocked");
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
            } else if stats.mana >= cost {
                stats.spend_mana(cost, &attrs);
                powers.blink = Some((pt.translation, target, 0.0));
                sfx.write(PostEvent::named(data.power_sound("Blink", level, "m_pBlinkWarmupSoundEvent").unwrap_or("Snd_Power_P_Blink_Start"), None));
                powers.cast_seq += 1;
                powers.cast_power = Power::Blink;
                used.write(PowerUsed(Power::Blink));
                p.locked = true;
                powers.blink_vel = p.velocity;
                p.velocity = Vec3::ZERO;
                powers.fov_kick = 1.0;
            } else {
                out_of_mana(&mut powers, &mut sfx, &mut msgs);
            }
        }
        return;
    }
    // the Heart's whispers are its own (heart.rs)
    if !can_act || !mouse.just_pressed(MouseButton::Right) || powers.cooldown > 0.0 || matches!(sel, Power::Empty | Power::Blink | Power::Heart) {
        return;
    }
    // toggles end for free
    if sel == Power::DarkVision && powers.dark_vision {
        powers.dark_vision = false;
        sfx.write(PostEvent::named(data.power_sound("DarkVision", level, "m_pDeactivationSoundEvent").unwrap_or("Darkvision_Stop"), None));
        powers.cooldown = 0.3;
        return;
    }
    if sel == Power::BendTime && tc.bend_remaining > 0.0 {
        tc.bend_remaining = 0.01;
        return;
    }
    if tune.inhibited && sel.key().is_some() {
        msgs.push("Your powers are blocked");
        sfx.write(PostEvent::named("Snd_Power_Empty", None));
        powers.cooldown = 0.5;
        return;
    }
    // (short of mana: a remedy drunk first, if the option says so)
    if stats.mana < cost && settings.auto_mana_elixir && stats.mana_elixirs > 0 {
        stats.mana_elixirs -= 1;
        stats.mana = (stats.mana + attrs.mana_elixir_amount(stats.max_mana, rand::random::<f32>())).min(stats.max_mana);
        stats.mana_cap = stats.mana_cap.max(stats.mana);
        sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Mana", None));
    }
    if stats.mana < cost {
        out_of_mana(&mut powers, &mut sfx, &mut msgs);
        return;
    }
    let mut cast = true;
    match sel {
        Power::Empty | Power::Blink | Power::Heart => {}
        Power::SummonAssassin => {
            // where (`m_fTargetingRange`): an enemy aimed at (within `m_fMaxAngleToTargetNPC_Deg`,
            // in sight) is gone for from `m_fDistFromTarget` before it; else the ground aimed at
            // (`m_fTargetingPullback` short of it), or the floor below where a wall stops the aim
            let param = |k: &str, d: f32| data.active("SummonAssassin").and_then(|a| a.params.get(k)).copied().unwrap_or(d);
            let range = param("m_fTargetingRange", 2000.0) * UU;
            let cone = param("m_fMaxAngleToTargetNPC_Deg", 10.0).to_radians();
            let walls = QueryFilter::default().exclude_collider(pe).exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
            let ground = |q: Vec3| ctx.cast_ray(q + Vec3::Y * 1.0, Vec3::NEG_Y, 30.0, true, walls).map(|(_, toi)| q + Vec3::Y * (1.0 - toi));
            let target = npcs
                .iter()
                .filter(|(_, _, n)| !n.is_down() && n.hostile())
                .filter_map(|(e, nt, _)| {
                    // (aimed at anywhere from its feet to its head)
                    let base = nt.translation - Vec3::Y * crate::npc::NPC_CENTER;
                    let along = (base - eye).dot(dir).max(0.0);
                    let near = (eye + dir * along).y.clamp(base.y, base.y + 1.8);
                    let to = base.with_y(near) - eye;
                    let d = to.length();
                    (d < range && to.normalize_or_zero().angle_between(dir) < cone && ctx.cast_ray(eye, to / d.max(1e-3), (d - 0.4).max(0.0), true, walls).is_none()).then_some((e, nt.translation, d))
                })
                .min_by(|a, b| a.2.total_cmp(&b.2));
            // (only where one can walk: `m_TargetOffNavMeshMessage`)
            let walkable = |p: &Vec3| nav.as_ref().is_none_or(|g| g.on_mesh(*p));
            let at = match target {
                // (before the enemy, Daud's side of it; nearer it where that is no floor)
                Some((_, tpos, _)) => {
                    let back = (pt.translation - tpos).with_y(0.0).normalize_or(Vec3::Z);
                    let far = param("m_fDistFromTarget", 350.0) * UU;
                    [1.0, 0.75, 0.5, 0.3].iter().find_map(|k| ground(tpos.with_y(tpos.y - crate::npc::NPC_CENTER + 0.5) + back * far * k).filter(walkable))
                }
                // (the ground aimed at; a wall's foot; aimed at nothing in reach, the ground
                // below the end of it: `m_fTargetingGroundCheckDistance`)
                None => match ctx.cast_ray_and_get_normal(eye, dir, range, true, walls) {
                    Some((_, h)) => {
                        let q = h.point - dir * param("m_fTargetingPullback", 80.0) * UU;
                        if h.normal.y > 0.5 { Some(q) } else { ground(q) }
                    }
                    None => {
                        let q = eye + dir * range;
                        let down = (param("m_fTargetingGroundCheckDistance", 50000.0) * UU).min(80.0);
                        ctx.cast_ray(q, Vec3::NEG_Y, down, true, walls).map(|(_, toi)| q - Vec3::Y * toi)
                    }
                }
                .filter(walkable),
            };
            if std::env::var_os("DH_SUMMON_LOG").is_some() {
                let feet = pt.translation - Vec3::Y * crate::npc::NPC_CENTER;
                let hit = ctx.cast_ray_and_get_normal(eye, dir, range, true, walls).map(|(_, h)| (h.point, h.normal));
                info!("summon: aimed {:?}, at {:?}; Daud's feet {feet:.1} on the mesh {}, the aim hits {hit:?} (ground {:?}); {} enemies about", target.map(|t| t.1), at, walkable(&feet), hit.and_then(|h| ground(h.0 - dir * 0.8)).map(|g| (g, walkable(&g))), npcs.iter().filter(|(_, _, n)| !n.is_down() && n.hostile()).count());
            }
            match at {
                Some(at) => {
                    stats.spend_mana(cost, &attrs);
                    summon.write(crate::summon::Summon { at, target: target.map(|t| t.0), level });
                    sfx.write(PostEvent::named(data.power_sound("SummonAssassin", level, "m_pTargetingSuccessSoundEvent").unwrap_or("Snd_Pwr_Summon_Out"), None));
                    powers.cooldown = 1.0;
                }
                None => {
                    msgs.push("You cannot summon there.");
                    sfx.write(PostEvent::named(data.power_sound("SummonAssassin", level, "m_pTargetingFailSoundEvent").unwrap_or("Snd_Pwr_Summon_Fail"), None));
                    cast = false;
                }
            }
        }
        Power::DarkVision => {
            stats.spend_mana(cost, &attrs);
            powers.dark_vision = true;
            powers.dark_vision_left = data.power_f("DarkVision", level, "m_fDuration", 30.0);
            sfx.write(PostEvent::named(data.power_sound("DarkVision", level, "m_pActivationSoundEvent").unwrap_or("Darkvision_Start"), None));
            powers.cooldown = 0.3;
        }
        Power::BendTime => {
            stats.spend_mana(cost, &attrs);
            // level 2 stops time; its duration is counted in the stopped world's time in the
            // original, so both levels last the level-1 span here
            // the level's: how long, the world's and Corvo's paces, coming in and going out
            // (`Twk_BendTime`: 12 s at 0.15 / 0.85, then 8 s stopped / 0.9; 0.15 s in, 0.25 out)
            tc.bend_remaining = data.power_f("BendTime", level, "m_TimeDuration", 12.0);
            tc.world_dilation = data.power_f("BendTime", level, "m_WorldDilation", 0.15);
            tc.player_dilation = data.power_f("BendTime", level, "m_PlayerDilation", 1.0);
            tc.ramp_in = data.power_f("BendTime", level, "m_fTimeDilationWarmupTime", 0.15);
            tc.ramp_out = data.power_f("BendTime", level, "m_fTimeDilationCooldownTime", 0.25);
            tc.bend_turn();
            sfx.write(PostEvent::named(data.power_sound("BendTime", level, "m_pBendTimeSoundStartEvent").unwrap_or("Snd_Power_P_Bend_Time"), None));
            sfx.write(PostEvent::named("Snd_UI_Ingame_Slomo_Start", None));
            powers.cooldown = 0.5;
        }
        Power::Windblast => {
            stats.spend_mana(cost, &attrs);
            powers.cooldown = 1.0;
            sfx.write(PostEvent::named(data.power_sound("Windblast", level, "m_pWindBlastSoundEvent").unwrap_or("Snd_Pwr_P_Windblast_Lvl_1"), None));
            // (the blast itself starts at the cast clip's `DisNotify_WindBlast`: `windblast_gusts`)
            powers.gusts.push(Gust { t: 0.0, eye, dir, from: pt.translation, level, targets: None, impacts: Vec::new() });
        }
        Power::Possess => {
            let range = data.power_f("Possess", level, "m_fPossessRange", 1000.0) * UU;
            match crate::possession::find_target(&ctx, pe, eye, dir, range, level, npcs.iter().map(|(e, t, n)| (e, t, &*n)), hosts.iter().map(|(e, g)| (e, g.translation() + Vec3::Y * 0.1)), &possess_overrides) {
                Some(target) => {
                    stats.spend_mana(cost, &attrs);
                    start_possess.write(crate::possession::PossessRequest { host: target, level });
                    powers.cooldown = 1.0;
                }
                None => {
                    msgs.push(if level >= 2 { "No one to possess" } else { "No animal to possess" });
                    sfx.write(PostEvent::named("Snd_Power_Empty", None));
                    cast = false;
                }
            }
        }
        Power::DevouringSwarm => {
            let reach = data.power_f("DevouringSwarm", level, "m_fMaxDistance", 1500.0) * UU;
            let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
            // where it lands: the floor in front, or below the aimed point
            let aim = ctx.cast_ray_and_get_normal(eye, dir, reach, true, filter).map(|(_, h)| (h.point, h.normal));
            let spot = match aim {
                Some((p, n)) if n.y > 0.5 => Some(p),
                Some((p, n)) => ctx.cast_ray(p + n * 0.3, Vec3::NEG_Y, 6.0, true, filter).map(|(_, toi)| p + n * 0.3 - Vec3::Y * toi),
                None => ctx.cast_ray(eye + dir * reach.min(6.0), Vec3::NEG_Y, 8.0, true, filter).map(|(_, toi)| eye + dir * reach.min(6.0) - Vec3::Y * toi),
            };
            // (not where rats are kept away)
            let spot = spot.filter(|at| level_info.as_ref().is_none_or(|l| crate::swarm::summon_allowed(l, *at)));
            match spot {
                Some(at) => {
                    stats.spend_mana(cost, &attrs);
                    swarm.write(crate::swarm::SummonSwarm { at, level });
                    sfx.write(PostEvent::named(data.power_sound("DevouringSwarm", level, "m_pUseSoundEvent").unwrap_or("Snd_Power_P_Devouring_Swarm_Spell"), None));
                    powers.cooldown = 1.0;
                }
                None => {
                    msgs.push("Cannot summon the Devouring Swarm here");
                    cast = false;
                }
            }
        }
        Power::Grenade | Power::StickyGrenade | Power::SpringRazor | Power::Chokedust | Power::ArcMine | Power::StunMine => {
            if sel.ammo(&stats) == Some(0) {
                msgs.push(format!("Out of {}", sel.ammo_type().map(crate::gadgets::ammo_name).unwrap_or("ammunition").to_lowercase()));
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
                return;
            }
            // a grenade (chokedust too): the pin pulled, it cooks while held (`cook_grenade`)
            if matches!(sel, Power::Grenade | Power::StickyGrenade | Power::Chokedust) {
                powers.cooking = Some((sel, 0.0));
                sfx.write(PostEvent::named("Snd_Grenade_Pin", None));
                return;
            }
            let g = match sel {
                Power::ArcMine => crate::gadgets::Gadget::ArcMine,
                Power::StunMine => crate::gadgets::Gadget::StunMine,
                _ => crate::gadgets::Gadget::SpringRazor,
            };
            gadget.write(crate::gadgets::UseGadget { gadget: g, eye, dir, cooked: 0.0 });
            powers.cooldown = 1.0;
        }
        Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt | Power::ExplosiveBolt => {
            let (kind, empty) = match sel {
                Power::SleepDart => (HitKind::SleepDart, "Out of sleep darts"),
                Power::IncendiaryBolt => (HitKind::Fire, "Out of incendiary bolts"),
                Power::ExplosiveBolt => (HitKind::ExplosiveBolt, "Out of explosive bolts"),
                _ => (HitKind::Bolt, "Out of bolts"),
            };
            let ammo = match sel {
                Power::SleepDart => &mut stats.sleep_darts,
                Power::Crossbow => &mut stats.bolts,
                p => stats.items.entry(p.ammo_item().unwrap_or(crate::gadgets::FLARES).to_string()).or_default(),
            };
            if *ammo == 0 {
                msgs.push(empty);
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
                return;
            }
            if !chamber(&mut powers, &data, &stats, sel) {
                return;
            }
            let ammo = match sel {
                Power::SleepDart => &mut stats.sleep_darts,
                Power::Crossbow => &mut stats.bolts,
                p => stats.items.entry(p.ammo_item().unwrap_or(crate::gadgets::FLARES).to_string()).or_default(),
            };
            *ammo -= 1;
            let held = *ammo;
            stats.shots_fired += 1;
            powers.cooldown = FIRE_ACTION;
            spend_round(&mut powers, &data, &stats, sel, held);
            sfx.write(PostEvent::named("Snd_Crossbow_P_Fire", None));
            // (its flash at the crossbow's muzzle, drawn with the hands)
            if let Some((m, _, _)) = muzzles.iter().find(|(_, _, m)| m.0 == Power::Crossbow) {
                fx.write(crate::particles::SpawnEffect { follow: Some(m), turn: true, view: true, secs: 0.5, ..crate::particles::SpawnEffect::at("crossbow_muzzle", Vec3::ZERO) });
            }
            // (the bolt strays within the aim's cone; the view kicks)
            let dir = aim.scatter(dir);
            aim.fire(&data, &stats, sel);
            // (a regular bolt that will kill: the kill cam follows it, as the option has it)
            let lethal = (kind == HitKind::Bolt && data.pawn("arrow.m_bKillCamEnabled", 0.0) > 0.0)
                .then(|| ray_surface(&ctx, &surfaces, eye, dir, 80.0, QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_NPC | GROUP_PROP))))
                .flatten()
                .and_then(|(hit, toi, _)| {
                    let (_, nt, n) = npcs.get(hit).ok()?;
                    let at = eye + dir * toi;
                    let damage = attrs.bolt.damage(0.0, at.y - nt.translation.y > HEAD, n.alert != crate::npc::Alert::Combat);
                    (!n.is_down() && n.min_health <= 0.0 && damage >= n.health).then_some(hit)
                })
                .filter(|&hit| {
                    let last = !npcs.iter().any(|(e, t, n)| e != hit && !n.is_down() && n.mode == crate::npc::Mode::Combat && t.translation.distance(pt.translation) < 25.0);
                    crate::combat::kill_cam(settings.kill_cam, last)
                });
            let mut e = commands.spawn((
                Projectile { vel: dir * Projectile::physics(&data, kind).0 * crate::aim::upgrade_sum(&data, &stats, crate::aim::Weapon::Crossbow, "Range").max(1.0), kind, life: 6.0, stuck: false, recoverable: kind == HitKind::Bolt && data.pawn("arrow.m_bCanRecoverAmmoInAir", 1.0) > 0.0, attachment: None },
                Transform::from_translation(eye + dir * 0.4).looking_to(dir, Vec3::Y),
                Visibility::default(),
                DespawnOnExit(GameState::InGame),
            ));
            projectile_model(&mut e, assets.as_deref(), kind);
            // (an incendiary bolt trails its fire, `Flare_Bolt_Trail`)
            if kind == HitKind::Fire {
                fx.write(crate::particles::SpawnEffect { follow: Some(e.id()), secs: 6.0, ..crate::particles::SpawnEffect::at("flare_trail", Vec3::ZERO) });
            }
            if let Some(npc) = lethal {
                kill_cams.write(crate::killcam::StartKillCam { bolt: e.id(), npc });
            }
        }
        Power::Pistol | Power::ExplosiveBullet => {
            let explosive = sel == Power::ExplosiveBullet;
            let ammo = if explosive { stats.items.entry(crate::gadgets::EXPLOSIVE.to_string()).or_default() } else { &mut stats.bullets };
            if *ammo == 0 {
                msgs.push(if explosive { "Out of explosive bullets" } else { "Out of bullets" });
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
                return;
            }
            if !chamber(&mut powers, &data, &stats, sel) {
                return;
            }
            let ammo = if explosive { stats.items.entry(crate::gadgets::EXPLOSIVE.to_string()).or_default() } else { &mut stats.bullets };
            *ammo -= 1;
            let held = *ammo;
            stats.shots_fired += 1;
            powers.cooldown = FIRE_ACTION;
            spend_round(&mut powers, &data, &stats, sel, held);
            let dir = aim.scatter(dir);
            aim.fire(&data, &stats, sel);
            // its flash at the pistol's muzzle (drawn with the hands), its tracer off from there
            // (`Ps_PlayerPistol_StdMuzzle_01`, `Ps_Pistol_StdTracer_01`)
            let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_NPC | GROUP_PROP));
            let shot = crate::footsteps::ray_surface_normal(&ctx, &surfaces, eye, dir, 80.0, filter);
            if let Some((m, mt, _)) = muzzles.iter().find(|(_, _, m)| m.0 == Power::Pistol) {
                fx.write(crate::particles::SpawnEffect { follow: Some(m), turn: true, view: true, secs: 2.5, ..crate::particles::SpawnEffect::at("pistol_muzzle", Vec3::ZERO) });
                // (the tracer's beam: from the muzzle to where the shot ends)
                let end = eye + dir * shot.map_or(80.0, |s| s.1);
                fx.write(crate::particles::SpawnEffect { beam_to: Some(end), rot: Quat::from_rotation_arc(Vec3::X, dir), secs: 0.6, ..crate::particles::SpawnEffect::at("pistol_tracer", mt.translation()) });
            }
            if let Some((hit, toi, surface, normal)) = shot {
                let at = eye + dir * toi;
                // what it strikes shows it (`Dis_ContactSystem`: splinters, sparks, blood)
                if let Some(level) = level_info.as_ref() {
                    let what = if npcs.get(hit).is_ok() { "Body" } else { crate::impacts::struck(surface) };
                    crate::impacts::spawn(&mut fx, &level.scene, crate::impacts::BULLET, what, at, if what == "Body" { -dir } else { normal });
                    // (and its hole)
                    crate::impacts::mark(&mut decals, &level.scene, crate::impacts::BULLET, what, at, normal, dir, f32::INFINITY);
                }
                if explosive {
                    // `Twk_Proj_Bullet_Explosive`: 500 uu reach, 200 uu full, 50 damage
                    blast.write(crate::gadgets::Explosion { at: at - dir * 0.2, radius: 5.0, full: 2.0, damage: 50.0, effect: "explosive_bullet", player: None, kind: HitKind::ExplosiveBullet, silent: false });
                }
                if let Ok((_, nt, n)) = npcs.get(hit) {
                    // the shot's damage by range, to the head (a kill), on the unaware
                    let head = at.y - nt.translation.y > HEAD;
                    let damage = attrs.bullet.damage(toi, head, n.alert != crate::npc::Alert::Combat);
                    if head {
                        commands.entity(hit).try_insert(crate::npc::HeadHit);
                    } else if at.y - nt.translation.y < LEGS {
                        let f = n.forward();
                        commands.entity(hit).try_insert(crate::npc::LegHit((at - nt.translation).dot(Vec3::new(-f.z, 0.0, f.x)) > 0.0));
                    }
                    hits.write(NpcHit { npc: hit, damage, kind: HitKind::Bullet, from: pt.translation });
                    sfx.write(PostEvent::named("Imp_Bullet_on_Body", Some(at)));
                    // it goes through (`m_fPiercingChance` 1): the next one in its line takes
                    // `m_fPostPiercingDamageRatio` of it and stops it (roshan's r614/blink research)
                    if !explosive && rand::random::<f32>() <= data.pawn("bullet.m_fPiercingChance", 1.0) {
                        let on = filter.exclude_collider(hit);
                        if let Some((next, t2)) = ctx.cast_ray(at + dir * 0.05, dir, (80.0 - toi).max(0.0), true, on) {
                            if let Ok((_, nt2, n2)) = npcs.get(next) {
                                let at2 = at + dir * (0.05 + t2);
                                let head2 = at2.y - nt2.translation.y > HEAD;
                                let d2 = attrs.bullet.damage(toi + 0.05 + t2, head2, n2.alert != crate::npc::Alert::Combat) * data.pawn("bullet.m_fPostPiercingDamageRatio", 0.5);
                                hits.write(NpcHit { npc: next, damage: d2, kind: HitKind::Bullet, from: pt.translation });
                                sfx.write(PostEvent::named("Imp_Bullet_on_Body", Some(at2)));
                            }
                        }
                    }
                } else if let Ok(s) = strikeables.get(hit) {
                    struck.write(crate::gameplay::Struck { target: s.0, damage: attrs.bullet.damage(toi, false, false), kind: HitKind::Bullet, at });
                    sfx.write(PostEvent::named("Imp_Bullet_on_Body", Some(at)));
                } else {
                    sfx.write(PostEvent::named(&format!("Imp_Bullet_on_{}", impact_surface(surface)), Some(at)));
                    noise.write(Noise { pos: at, radius: 30.0, kind: NoiseKind::Danger });
                }
                if npcs.get(hit).is_err() {
                    world_hits.write(WorldDamage::player(Reach::Hit { collider: hit, at }, attrs.bullet.damage(toi, false, false), crate::worlddamage::bullet_type(toi, &attrs)));
                }
            }
            noise.write(Noise { pos: pt.translation, radius: 60.0, kind: NoiseKind::Danger });
        }
    }
    if cast {
        powers.cast_seq += 1;
        powers.cast_power = sel;
        if sel.key().is_some() {
            used.write(PowerUsed(sel));
        }
    }
}

/// Wind Blasts under way, as the original's (decoded by roshan's r614/blink research): at the
/// cast clip's notify it gathers, once, everyone within its cone (`m_Angle`) and `m_Distance`,
/// by k = (1 - d/R)^2 x ((cos to aim - cos half) / (1 - cos half))^2 (past 1 m); its front
/// (`m_Speed`) reaches each in turn: the push `m_Force` along the aim, its height raised to
/// `m_ExtraUpForce` x (1 - d/R), times k (a body flies at half that momentum, at least 800 uu/s
/// across); the damage `m_Damage` x k, `m_HitWallAdditionalDamage` more for one slammed into a
/// wall. The doors, the scripts' things and the loose props before it go as it starts.
#[allow(clippy::too_many_arguments)]
fn windblast_gusts(
    time: Res<Time>,
    tc: Res<TimeControl>,
    data: Res<Data>,
    mut powers: ResMut<Powers>,
    mut npcs: Query<(Entity, &Transform, &mut Npc)>,
    rapier: ReadRapierContext,
    (mut hits, mut stag, mut noise): (MessageWriter<NpcHit>, MessageWriter<NpcStagger>, MessageWriter<Noise>),
    (mut world_hits, mut door_blast, mut props): (MessageWriter<WorldDamage>, MessageWriter<crate::interact::DoorBlast>, MessageWriter<crate::props::PropGust>),
    (mut grenades, held): (Query<(Entity, &mut crate::gadgets::Grenade, &Transform)>, Res<crate::props::Held>),
    (mut arrows, mut spits, mut bolts): (Query<(&mut crate::tallboy::Arrow, &Transform)>, Query<(&mut crate::krust::Spit, &Transform)>, Query<(&mut Projectile, &Transform)>),
    (mut fx, mut sfx, waters): (MessageWriter<crate::particles::SpawnEffect>, MessageWriter<PostEvent>, Option<Res<crate::swim::Waters>>),
) {
    if powers.gusts.is_empty() {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let dt = time.delta_secs() * tc.own_scale();
    let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    for g in powers.gusts.iter_mut() {
        g.t += dt;
        if g.t < WINDBLAST_NOTIFY {
            continue;
        }
        let level = g.level;
        let force = data.power_f("Windblast", level, "m_Force", 1800.0);
        let up = data.power_f("Windblast", level, "m_ExtraUpForce", 1000.0);
        let damage = data.power_f("Windblast", level, "m_Damage", 20.0);
        let reach = data.power_f("Windblast", level, "m_Distance", 3000.0) * UU;
        let speed = data.power_f("Windblast", level, "m_Speed", 5000.0) * UU;
        let half_angle = (data.power_f("Windblast", level, "m_Angle", 135.0) * 0.5).to_radians();
        let wall_bonus = data.active("Windblast").and_then(|a| a.params.get("m_HitWallAdditionalDamage")).copied().unwrap_or(100.0);
        let cos_half = half_angle.cos();
        let flat = g.dir.with_y(0.0).normalize_or_zero();
        let (eye, dir) = (g.eye, g.dir);
        let factor = |at: Vec3| -> Option<(f32, f32, f32)> {
            let to = at - eye;
            let d = to.length();
            let along = flat.dot(to.with_y(0.0).normalize_or_zero());
            if d >= reach || along <= cos_half {
                return None;
            }
            let near = 1.0 - (d / reach).min(1.0);
            let mut k = near * near;
            if d > 1.0 {
                let a = (along - cos_half) / (1.0 - cos_half);
                k *= a * a;
            }
            (k > 0.0).then_some((d, k, near))
        };
        if g.targets.is_none() {
            use crate::particles::SpawnEffect;
            // its gust from his left hand (`m_HandEffectSocketName` LeftHandWpn), by its level, or
            // under water its own (`m_EffectTemplate`, `m_UnderwaterEffectTemplate`)
            let wet = |p: Vec3| waters.as_ref().is_some_and(|w| w.at(p).is_some());
            let side = dir.cross(Vec3::Y).normalize_or(Vec3::X);
            let hand = eye + dir * 0.45 - side * 0.2 - Vec3::Y * 0.2;
            let gust = if wet(eye) { "windblast_underwater" } else if level >= 2 { "windblast_2" } else { "windblast_1" };
            fx.write(SpawnEffect { rot: Quat::from_rotation_arc(Vec3::X, dir), secs: 3.0, ..SpawnEffect::at(gust, hand) });
            // the ground under him kicked up (`Windblast_Ground`: dirt, or water with its burst),
            // its Y along the blast (the contact's `DCO_VELOCITY_DIRECTION`)
            if let Some((_, toi)) = ctx.cast_ray(g.from, Vec3::NEG_Y, 2.5, true, filter) {
                let at = g.from - Vec3::Y * toi;
                let water = wet(at + Vec3::Y * 0.05);
                if water {
                    sfx.write(PostEvent::named("Snd_Phys_Water_Expl", Some(at)));
                }
                fx.write(SpawnEffect { rot: Quat::from_rotation_arc(Vec3::Z, dir), secs: 3.0, ..SpawnEffect::at(if water { "windblast_water" } else { "windblast_dirt" }, at) });
            }
            // where it meets the world in its cone, met by its front in turn
            let across = side.cross(dir).normalize_or(Vec3::Y);
            for (frac, n) in GUST_RINGS {
                for k in 0..n {
                    let a = k as f32 / n as f32 * std::f32::consts::TAU;
                    let off = (side * a.cos() + across * a.sin()) * (half_angle * frac).tan();
                    let ray = (dir + off).normalize_or(dir);
                    if let Some((_, toi)) = ctx.cast_ray(eye, ray, reach, true, filter) {
                        let at = eye + ray * toi;
                        if g.impacts.iter().all(|(p, _)| p.distance(at) >= GUST_IMPACT_SPACING) {
                            g.impacts.push((at, toi));
                        }
                    }
                }
            }
            // it starts: gathered once; the scripts' candles and fires it blows out, the
            // things it knocks over, the doors (the wooden ones break), the loose props
            g.targets = Some(npcs.iter().filter(|(_, _, n)| !n.is_down()).filter_map(|(e, t, _)| factor(t.translation).map(|(d, _, _)| (e, d))).collect());
            world_hits.write(WorldDamage::player(Reach::Cone { at: eye, dir, len: reach, half: half_angle }, damage, "DisDamageType_WindBlast"));
            door_blast.write(crate::interact::DoorBlast { from: g.from, dir, reach, half_angle, damage });
            props.write(crate::props::PropGust { eye, dir, reach, half_angle, force, up });
            // (the grenades in flight it sends back)
            for (ge, mut gr, gt) in &mut grenades {
                if held.0 != Some(ge) && factor(gt.translation).is_some() {
                    gr.blown(dir);
                }
            }
            // (and every other projectile in flight before it, turned along the blast: the
            // original gathers every actor, projectiles too, as roshan's r614/blink research
            // found: a tallboy's arrow, a krust's spit, his own bolts)
            let mut turned = 0;
            for (mut a, at) in &mut arrows {
                if factor(at.translation).is_some() {
                    a.blown(dir);
                    turned += 1;
                }
            }
            for (mut s, st) in &mut spits {
                if factor(st.translation).is_some() {
                    s.blown(dir);
                    turned += 1;
                }
            }
            for (mut p, pt) in &mut bolts {
                if !p.stuck && factor(pt.translation).is_some() {
                    p.vel = dir * p.vel.length();
                    turned += 1;
                }
            }
            if turned > 0 {
                info!("windblast turns {turned} projectiles back");
            }
            noise.write(Noise { pos: g.from, radius: 15.0, kind: NoiseKind::Danger });
        }
        let front = (g.t - WINDBLAST_NOTIFY) * speed;
        // (its puffs where the front has reached the world: `Windblast_GroundImpact_01`, its Y
        // along the blast across the ground, `DCO_VELOCITY_DIRECTION_2D`)
        let flat_dir = dir.with_y(0.0).normalize_or(dir);
        g.impacts.retain(|&(at, d)| {
            if d > front {
                return true;
            }
            fx.write(crate::particles::SpawnEffect { rot: Quat::from_rotation_arc(Vec3::Z, flat_dir), secs: 3.0, ..crate::particles::SpawnEffect::at("windblast_impact", at) });
            false
        });
        let Some(targets) = g.targets.as_mut() else { continue };
        targets.retain(|&(e, d0)| {
            if d0 > front {
                return true;
            }
            let Ok((_, nt, mut npc)) = npcs.get_mut(e) else { return false };
            // (one already lying there: its bodies are blown on, `ragdoll`)
            if npc.is_down() || npc.floored {
                return false;
            }
            let Some((_, k, near)) = factor(nt.translation) else { return false };
            let z = dir.y * force;
            let z = if z < 0.0 { z + near * up } else { z.max(near * up) };
            let momentum = Vec3::new(dir.x * force, z, dir.z * force) * k;
            let mut vel = momentum * 0.5 * UU;
            let across = vel.with_y(0.0);
            if across.length() < 4.0 {
                let lift = vel.y;
                vel = across.normalize_or(flat) * 4.0;
                vel.y = lift;
            }
            npc.velocity += vel;
            stag.write(NpcStagger { npc: e, secs: 2.0, parried: false });
            // blown hard, off its feet (limp, then up again: `npc.rs`); less, it reels back
            // (`Sword_HitWindblast`)
            if vel.length() >= KNOCKDOWN_SPEED {
                npc.knock = Some(vel);
            }
            // slammed into a wall within the flight's first moments?
            let wall = ctx.cast_ray(nt.translation, vel.with_y(0.0).normalize_or(flat), (vel.with_y(0.0).length() * 0.3).max(0.5), true, filter).is_some();
            // (the wall's bonus at level 2 only, flung at 500 uu/s or more)
            let dmg = (damage * k).round() + if wall && level >= 2 && vel.length() >= 5.0 { wall_bonus } else { 0.0 };
            if dmg > 0.0 {
                hits.write(NpcHit { npc: e, damage: dmg, kind: HitKind::Windblast, from: g.from });
            }
            false
        });
    }
    powers.gusts.retain(|g| (g.targets.as_ref().is_none_or(|t| !t.is_empty()) || !g.impacts.is_empty()) && g.t < 3.0);
}

fn out_of_mana(powers: &mut Powers, sfx: &mut MessageWriter<PostEvent>, msgs: &mut HudMessages) {
    msgs.push("Not enough mana");
    sfx.write(PostEvent::named("Snd_Power_Empty", None));
    powers.cooldown = 0.4;
}

/// Daud's Blink (Transversal): aimed while he stands still (no step, no jump), even
/// mid-jump, time stops for the world and for him (`m_fBendTimeDownTime`: at once), and
/// comes back as he moves or lets go (`m_fBendTimeUpTime`).
fn transversal_freeze(
    time: Res<Time>,
    data: Res<Data>,
    stats: Res<PlayerStats>,
    powers: Res<Powers>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    mut tc: ResMut<TimeControl>,
) {
    let transversal = data.active("Blink").is_some_and(|a| a.params.get("Transversal").is_some_and(|v| *v > 0.5));
    if !transversal && tc.aim_freeze >= 1.0 {
        return;
    }
    let dt = time.delta_secs().min(0.1);
    let lvl = stats.power("Blink").max(1);
    let moving = [crate::bindings::Act::Forward, crate::bindings::Act::Back, crate::bindings::Act::Left, crate::bindings::Act::Right, crate::bindings::Act::Jump].iter().any(|a| keys.pressed(bind.key(*a)));
    let held = transversal && powers.aiming && powers.selected == Power::Blink && powers.blink.is_none() && !moving;
    if held {
        let down = data.power_f("Blink", lvl, "m_fBendTimeDownTime", 0.0);
        tc.aim_freeze = if down <= 0.01 { 0.0 } else { (tc.aim_freeze - dt / down).max(0.0) };
    } else {
        let up = data.power_f("Blink", lvl, "m_fBendTimeUpTime", 0.5).max(0.01);
        tc.aim_freeze = (tc.aim_freeze + dt / up).min(1.0);
    }
}

fn blink_travel(
    time: Res<Time>,
    data: Res<Data>,
    stats: Res<PlayerStats>,
    mut powers: ResMut<Powers>,
    mut player: Query<(&mut Transform, &mut Player, &mut Collider)>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let Some((from, to, t)) = powers.blink else { return };
    let Ok((mut pt, mut p, mut col)) = player.single_mut() else { return };
    // 100 uu a tick (60 m/s), his velocity his again at the end: a falling Blink keeps its fall
    // (as roshan's r614/blink research measured)
    let dur = (from.distance(to) / 60.0).max(0.05);
    let nt = (t + time.delta_secs() / dur).min(1.0);
    pt.translation = from.lerp(to, nt);
    if nt >= 1.0 {
        let level = stats.power("Blink");
        sfx.write(PostEvent::named(data.power_sound("Blink", level, "m_pBlinkSoundEvent").unwrap_or("Snd_Power_P_Blink_Stop"), None));
        powers.blink = None;
        p.locked = false;
        p.velocity = powers.blink_vel;
        p.grounded = false;
        if powers.blink_crouch && !p.crouched {
            p.crouched = true;
            *col = Collider::capsule_y(crate::player::CROUCH_HALF, RADIUS);
        }
    } else {
        powers.blink = Some((from, to, nt));
    }
}

#[allow(clippy::too_many_arguments)]
fn update_projectiles(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    (mut hits, attrs, data): (MessageWriter<NpcHit>, Res<Attrs>, Res<Data>),
    player: Query<Entity, With<Player>>,
    npcs: Query<(&Npc, &Transform, Option<&crate::npc::NpcRig>), Without<Projectile>>,
    mut q: Query<(Entity, &mut Projectile, &mut Transform)>,
    (surfaces, mut sfx): (Query<&ColliderSurfaces>, MessageWriter<PostEvent>),
    (strikeables, mut struck, mut world_hits): (Query<&crate::gameplay::Strikeable>, MessageWriter<crate::gameplay::Struck>, MessageWriter<WorldDamage>),
    globals: Query<&GlobalTransform>,
    mut blasts: MessageWriter<crate::gadgets::Explosion>,
    (mut fx, level): (MessageWriter<crate::particles::SpawnEffect>, Option<Res<crate::level::LevelInfo>>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let pe = player.single().ok();
    for (e, mut pr, mut t) in &mut q {
        let mut embedded_in_living = false;
        if let Some(a) = pr.attachment {
            let Ok((npc, nt, rig)) = npcs.get(a.npc) else {
                commands.entity(e).despawn();
                continue;
            };
            let frame = a.bone.and_then(|i| rig?.joint_at(i)).and_then(|e| globals.get(e).ok()).copied().unwrap_or(GlobalTransform::from(*nt));
            t.translation = frame.affine().transform_point3(a.offset);
            t.rotation = frame.compute_transform().rotation * a.rotation;
            embedded_in_living = !npc.is_down();
        }
        // Recoverable bolts remain on surfaces and corpses. Only free flight and
        // bolts embedded in living enemies consume their lifespan.
        if !pr.stuck || !pr.recoverable || embedded_in_living {
            pr.life -= dt;
        }
        if pr.life <= 0.0 {
            // an explosive bolt goes off where it struck (`m_fDetonationTimer` after), the one
            // it is in taking `m_fAttachedDamage` too (`DLC06_DaudArrowExplosion`: 500 uu
            // reach, 200 uu full, 50 damage)
            if pr.kind == HitKind::ExplosiveBolt && pr.stuck {
                let x = |k: &str, d: f32| data.pawn(&format!("arrow_explosive.explosion.{k}"), d);
                if let Some(a) = pr.attachment {
                    hits.write(NpcHit { npc: a.npc, damage: x("m_fAttachedDamage", 50.0), kind: HitKind::ExplosiveBolt, from: t.translation });
                }
                let player_reach = data.0.pawn.get("arrow_explosive.explosion.m_fPlayerDamageRadius").map(|r| [r * UU, x("m_fPlayerFullDamageRadius", 150.0) * UU]);
                blasts.write(crate::gadgets::Explosion {
                    at: t.translation - t.forward().as_vec3() * 0.15,
                    radius: x("m_fDamageRadius", 500.0) * UU,
                    full: x("m_fFullDamageRadius", 200.0) * UU,
                    damage: x("m_RadialDamage", 50.0),
                    effect: "bolt_explosion",
                    player: player_reach,
                    kind: HitKind::ExplosiveBolt,
                    silent: false,
                });
            }
            commands.entity(e).despawn();
            continue;
        }
        if pr.stuck {
            continue;
        }
        let acceleration = Vec3::Y * Projectile::physics(&data, pr.kind).1;
        let step = pr.vel * dt + acceleration * (0.5 * dt * dt);
        pr.vel += acceleration * dt;
        let len = step.length();
        if len <= 0.0 {
            continue;
        }
        let mut filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_NPC | GROUP_PROP));
        if let Some(pe) = pe {
            filter = filter.exclude_collider(pe);
        }
        if let Some((hit, toi, surface, normal)) = crate::footsteps::ray_surface_normal(&ctx, &surfaces, t.translation, step / len, len, filter) {
            t.translation += step / len * toi;
            pr.stuck = true;
            pr.life = pr.life.min(5.0);
            let what = if npcs.get(hit).is_ok() { "Body" } else { impact_surface(surface) };
            // what it strikes shows it (`Dis_ContactSystem`: a bolt's, a dart's, a burning one's)
            if let Some(level) = level.as_ref() {
                let striker = match pr.kind {
                    HitKind::SleepDart => crate::impacts::ARROW_SLEEP,
                    HitKind::Fire => crate::impacts::ARROW_FLARE,
                    _ => crate::impacts::ARROW,
                };
                let body = npcs.get(hit).is_ok();
                let struck = if body { "Body" } else { crate::impacts::struck(surface) };
                crate::impacts::spawn(&mut fx, &level.scene, striker, struck, t.translation, if body { -step / len } else { normal });
            }
            if pr.kind == HitKind::Bolt {
                pr.recoverable = data.pawn("arrow.m_bCanRecoverAmmoInEnvironment", 1.0) > 0.0;
            }
            // (an explosive bolt: its fuse)
            if pr.kind == HitKind::ExplosiveBolt {
                pr.recoverable = false;
                pr.life = data.pawn("arrow_explosive.m_fDetonationTimer", 0.4).max(0.01);
            }
            sfx.write(PostEvent::named(&format!("Imp_Arrow_on_{what}"), Some(t.translation)));
            if pr.kind == HitKind::Fire && npcs.get(hit).is_err() {
                sfx.write(PostEvent::named(&format!("Imp_Arrow_on_{what}_Flare"), Some(t.translation)));
            }
            if let Ok(s) = strikeables.get(hit) {
                let damage = match pr.kind {
                    HitKind::Bolt => attrs.bolt.damage,
                    HitKind::Fire => 999.0,
                    _ => 0.0,
                };
                struck.write(crate::gameplay::Struck { target: s.0, damage, kind: pr.kind, at: t.translation });
            }
            if npcs.get(hit).is_err() {
                let kind = crate::worlddamage::hit_type(pr.kind, 0.0, &attrs);
                let damage = if pr.kind == HitKind::SleepDart { 0.0 } else { attrs.bolt.damage };
                world_hits.write(WorldDamage::player(Reach::Hit { collider: hit, at: t.translation }, damage, kind));
            }
            if let Ok((npc, nt, rig)) = npcs.get(hit) {
                // (a bolt or fire kills one lying knocked out too)
                if !npc.is_down() || (npc.mode == crate::npc::Mode::Unconscious && matches!(pr.kind, HitKind::Bolt | HitKind::Fire)) {
                    // the region struck: the head, the torso, else a limb (below the hips, or out
                    // to the side of its chest)
                    let local = t.translation - nt.translation;
                    let f = npc.forward();
                    let side = local.with_y(0.0).dot(Vec3::new(-f.z, 0.0, f.x)).abs();
                    let head = local.y > HEAD;
                    let limb = !head && (local.y < LEGS || side > 0.22);
                    // a bolt: more to the head, doubled (`m_fDamageMultiplier_Stealth`) on the head
                    // or torso of one not fighting him, a limb's at most 15; an incendiary one 50,
                    // never killing outright, and the flames 0.5 s on (as roshan's r614/blink
                    // research decoded them)
                    let stealthy = npc.alert != crate::npc::Alert::Combat || npc.mode == crate::npc::Mode::Flee;
                    // (where it came from: back along its flight; the head marked for what it
                    // wears there, an overseer's mask)
                    let came = t.translation - step / len * 2.0;
                    if head {
                        commands.entity(hit).try_insert(crate::npc::HeadHit);
                    } else if local.y < LEGS {
                        commands.entity(hit).try_insert(crate::npc::LegHit(local.with_y(0.0).dot(Vec3::new(-f.z, 0.0, f.x)) > 0.0));
                    }
                    match pr.kind {
                        HitKind::Bolt => {
                            let damage = attrs.bolt.damage(0.0, head, stealthy && !limb);
                            hits.write(NpcHit { npc: hit, damage: if limb { damage.min(15.0) } else { damage }, kind: pr.kind, from: came });
                        }
                        HitKind::Fire => {
                            if !npc.is_down() {
                                hits.write(NpcHit { npc: hit, damage: 50.0f32.min(npc.health - 1.0).max(0.0), kind: HitKind::Bolt, from: came });
                            }
                            commands.entity(hit).try_insert(Ignite(0.5));
                        }
                        _ => {
                            hits.write(NpcHit { npc: hit, damage: 0.0, kind: pr.kind, from: came });
                        }
                    }
                }
                let explosive = pr.kind == HitKind::ExplosiveBolt;
                if explosive || pr.kind == HitKind::Bolt && data.pawn("arrow.m_bCanRecoverAmmoInBodies", 1.0) > 0.0
                    && Projectile::survives_body_hit(data.pawn("arrow.m_fChanceToBreak", 0.7), attrs.bolt_break_modifier, rand::random::<f32>()) {
                    let nearest = rig.and_then(|r| (0..r.len()).filter_map(|i| Some((i, *globals.get(r.joint_at(i)?).ok()?)))
                        .min_by(|a, b| a.1.translation().distance_squared(t.translation).total_cmp(&b.1.translation().distance_squared(t.translation))));
                    let frame = nearest.map(|(_, g)| g).unwrap_or(GlobalTransform::from(*nt));
                    pr.attachment = Some(BoltAttachment { npc: hit, bone: nearest.map(|(i, _)| i),
                        offset: frame.affine().inverse().transform_point3(t.translation), rotation: frame.compute_transform().rotation.inverse() * t.rotation });
                    pr.recoverable = !explosive;
                    if !explosive {
                        pr.life = data.pawn("arrow.m_fStuckInAlivePawnVanishTime", 10.0).max(0.01);
                    }
                } else {
                    commands.entity(e).despawn();
                }
            }
        } else {
            t.translation += step;
            let v = pr.vel;
            t.look_to(v, Vec3::Y);
        }
    }
}

#[cfg(test)]
mod projectile_tests {
    use super::*;

    #[test]
    fn arrow_variants_use_original_gravity_and_speed_modifiers() {
        let mut data = Data::default();
        for (kind, speed, gravity) in [(HitKind::Bolt, 200.0, -19.5), (HitKind::SleepDart, 140.0, -6.0), (HitKind::Fire, 40.0, -7.5)] {
            let actual = Projectile::physics(&data, kind);
            assert!((actual.0 - speed).abs() < 1e-5);
            assert!((actual.1 - gravity).abs() < 1e-5);
        }
        data.0.pawn.insert("world.DefaultGravityZ".into(), -1000.0);
        data.0.pawn.insert("arrow_sleep.m_fSpeedMultiplier".into(), 0.5);
        data.0.pawn.insert("arrow_sleep.m_fGravityMultiplier".into(), 0.2);
        assert_eq!(Projectile::physics(&data, HitKind::SleepDart), (100.0, -2.0));
    }

    #[test]
    fn loading_preserves_active_powers_and_resets_transient_menu_slowdown() {
        let powers = Powers { selected: Power::Crossbow, dark_vision: true, dark_vision_left: 6.5,
            cooldown: 0.4, cooking: Some((Power::Grenade, 1.2)), blink: Some((Vec3::X, Vec3::Z, 0.05)), ..default() };
        let tc = TimeControl { bend_remaining: 4.5, world_dilation: 0.0, wheel: 0.1, finisher: 0.2, scripted: Some((0.3, 7.0)), aim_freeze: 1.0, ..default() };
        let bytes = serde_json::to_vec(&PowersSave::capture(&powers, &tc)).unwrap();
        let saved: PowersSave = serde_json::from_slice(&bytes).unwrap();
        let mut loaded = Powers::default();
        let mut time = TimeControl::default();
        saved.restore(&mut loaded, &mut time);
        assert_eq!(loaded.selected, Power::Crossbow);
        assert!(loaded.dark_vision);
        assert_eq!(loaded.dark_vision_left, 6.5);
        assert_eq!(loaded.cooldown, 0.4);
        assert_eq!(loaded.cooking, powers.cooking);
        assert_eq!(loaded.blink, powers.blink);
        assert_eq!(time.bend_remaining, 4.5);
        assert_eq!(time.world_scale(), 0.0);
        assert_eq!(time.scripted, Some((0.3, 7.0)));
        assert_eq!(time.wheel, 1.0);
        assert_eq!(time.finisher, 1.0);
        PowersSave::capture(&Powers::default(), &TimeControl::default()).restore(&mut loaded, &mut time);
        assert!(!loaded.dark_vision);
        assert!(loaded.cooking.is_none());
        assert!(loaded.blink.is_none());
        assert_eq!(time.world_scale(), 1.0);
    }

    #[test]
    fn reinforced_bolts_reduce_breakage_and_recovery_respects_capacity() {
        let ordinary = (0..100).filter(|i| Projectile::survives_body_hit(0.7, 0.0, (*i as f32 + 0.5) / 100.0)).count();
        let reinforced = (0..100).filter(|i| Projectile::survives_body_hit(0.7, -0.5, (*i as f32 + 0.5) / 100.0)).count();
        assert_eq!(ordinary, 30);
        assert_eq!(reinforced, 80);
        assert!(Projectile::survives_body_hit(0.7, -2.0, 0.0));
        assert!(!Projectile::survives_body_hit(0.7, 2.0, 0.99));
        let mut stats = PlayerStats::default();
        stats.bolts = 9;
        let mut attrs = Attrs::default();
        assert!(recover_bolt(&mut stats, &attrs));
        assert_eq!(stats.bolts, 10);
        assert!(!recover_bolt(&mut stats, &attrs));
        assert_eq!(stats.bolts, 10);
        attrs.bolt_capacity = 20;
        assert!(recover_bolt(&mut stats, &attrs));
        assert_eq!(stats.bolts, 11);
    }

    #[test]
    fn projectile_save_remaps_embedded_bolts_and_preserves_flight() {
        let mut old = World::new();
        let npc = old.spawn_empty().id();
        let stuck = Projectile { vel: Vec3::ZERO, kind: HitKind::Bolt, life: 7.5, stuck: true, recoverable: true,
            attachment: Some(BoltAttachment { npc, bone: Some(7), offset: Vec3::X * 0.1, rotation: Quat::from_rotation_z(0.3) }) };
        let flying = Projectile { vel: Vec3::X * 45.0, kind: HitKind::Bolt, life: 4.0, stuck: false, recoverable: true, attachment: None };
        let pose = Transform::from_xyz(1.0, 2.0, 3.0);
        let saved = save_projectiles([(&stuck, &pose), (&flying, &pose)], [(npc, 11)]);
        let saved = serde_json::from_slice::<Vec<ProjectileSave>>(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut app = App::new();
        app.insert_resource(ProjectileRestore(Some(saved))).add_systems(Update, restore_projectiles);
        for _ in 0..10 { app.world_mut().spawn_empty(); }
        let restored_npc = app.world_mut().spawn(crate::npc::FromSpawner(11)).id();
        assert_ne!(npc, restored_npc);
        let stale = app.world_mut().spawn((flying, pose)).id();
        app.update();
        assert!(app.world().get_entity(stale).is_err());
        let mut q = app.world_mut().query::<(&Projectile, &Transform)>();
        assert_eq!(q.iter(app.world()).count(), 2);
        for (p, t) in q.iter(app.world()) {
            assert_eq!(t.translation, pose.translation);
            assert!(p.recoverable);
            if p.stuck {
                let a = p.attachment.unwrap();
                assert_eq!(a.npc, restored_npc);
                assert_eq!(a.bone, Some(7));
                assert_eq!(a.offset, Vec3::X * 0.1);
                assert_eq!(a.rotation, Quat::from_rotation_z(0.3));
                assert_eq!(p.life, 7.5);
            } else {
                assert_eq!(p.vel, Vec3::X * 45.0);
                assert_eq!(p.life, 4.0);
            }
        }
        app.world_mut().resource_mut::<ProjectileRestore>().0 = Some(Vec::new());
        app.update();
        assert_eq!(q.iter(app.world()).count(), 0);
    }

    #[test]
    fn bend_time_freezes_bolt_motion_gravity_and_lifetime() {
        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<Attrs>().init_resource::<Data>()
            .insert_resource(TimeControl { bend_remaining: 10.0, world_dilation: 0.0, ..default() })
            .add_message::<NpcHit>().add_message::<PostEvent>().add_message::<crate::gameplay::Struck>().add_message::<WorldDamage>().add_message::<crate::gadgets::Explosion>().add_message::<crate::particles::SpawnEffect>()
            .add_systems(Update, update_projectiles);
        app.world_mut().spawn((DefaultRapierContext, RapierContextSimulation::default()));
        let e = app.world_mut().spawn((Projectile { vel: Vec3::X * 10.0, kind: HitKind::Bolt, life: 10.0, stuck: false, recoverable: true, attachment: None }, Transform::IDENTITY)).id();
        app.world_mut().resource_mut::<Time>().advance_by(std::time::Duration::from_secs(1));
        app.update();
        assert_eq!(app.world().get::<Projectile>(e).unwrap().life, 10.0);
        assert_eq!(app.world().get::<Projectile>(e).unwrap().vel, Vec3::X * 10.0);
        assert_eq!(app.world().get::<Transform>(e).unwrap().translation, Vec3::ZERO);
        app.world_mut().resource_mut::<TimeControl>().world_dilation = 0.1;
        app.update();
        assert!((app.world().get::<Projectile>(e).unwrap().life - 9.9).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.x - 1.0).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.y + 0.0975).abs() < 1e-5);
        assert!((app.world().get::<Projectile>(e).unwrap().vel.y + 1.95).abs() < 1e-5);
        app.world_mut().resource_mut::<TimeControl>().bend_remaining = 0.0;
        app.update();
        assert!((app.world().get::<Projectile>(e).unwrap().life - 8.9).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.x - 11.0).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.y + 11.7975).abs() < 1e-4);
        // An intact bolt stuck in the world must not vanish before it is looted.
        {
            let mut projectile = app.world_mut().get_mut::<Projectile>(e).unwrap();
            projectile.stuck = true;
            projectile.life = 0.1;
        }
        app.update();
        assert_eq!(app.world().get::<Projectile>(e).unwrap().life, 0.1);
        assert!((app.world().get::<Transform>(e).unwrap().translation.x - 11.0).abs() < 1e-5);
    }
}

/// A grenade cooking in the hand (the HUD's `GrenadeCooking`): its fuse burns while the button
/// is held; let go, it's thrown with what's left of it; [Use] puts it back
/// (`DUI_Context_CancelGrenadeCooking`); held to the end, it goes off in the hand.
#[allow(clippy::too_many_arguments)]
fn cook_grenade(
    time: Res<Time>,
    paused: Res<crate::hud::Paused>,
    tc: Res<TimeControl>,
    mouse: Res<ButtonInput<MouseButton>>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    mut powers: ResMut<Powers>,
    stats: Res<PlayerStats>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut gadget: MessageWriter<crate::gadgets::UseGadget>,
) {
    if paused.0 { return; }
    let Some((p, t)) = powers.cooking else { return };
    let Ok(cg) = cam.single() else { return };
    if stats.dead || powers.selected != p {
        powers.cooking = None;
        return;
    }
    if keys.just_pressed(bind.key(crate::bindings::Act::Use)) {
        powers.cooking = None;
        powers.swallow = true;
        return;
    }
    let t = t + time.delta_secs() * tc.world_scale();
    let g = match p {
        Power::StickyGrenade => crate::gadgets::Gadget::StickyGrenade,
        Power::Chokedust => crate::gadgets::Gadget::Chokedust,
        _ => crate::gadgets::Gadget::Grenade,
    };
    let (eye, dir) = (cg.translation(), cg.forward().as_vec3());
    if t >= crate::gadgets::FUSE || !mouse.pressed(MouseButton::Right) {
        powers.cooking = None;
        powers.cooldown = 1.0;
        gadget.write(crate::gadgets::UseGadget { gadget: g, eye, dir, cooked: t.min(crate::gadgets::FUSE) });
        // (the throw: the left hand's deploy)
        powers.cast_seq += 1;
        powers.cast_power = p;
    } else {
        powers.cooking = Some((p, t));
    }
}

#[cfg(test)]
mod elixir_input_tests {
    use super::*;

    #[test]
    fn menu_number_keys_do_not_change_equipped_power() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<KeyCode>>().init_resource::<AccumulatedMouseScroll>()
            .init_resource::<PlayerStats>().init_resource::<Powers>().init_resource::<Data>()
            .init_resource::<crate::possession::Possession>().init_resource::<crate::choice::Choice>()
            .insert_resource(crate::hud::Paused(true))
            .init_resource::<Shortcuts>().init_resource::<crate::wheel::Wheel>().init_resource::<Time>()
            .add_message::<PowerEquipped>().add_message::<PostEvent>().add_systems(Update, select_power);
        app.world_mut().resource_mut::<PlayerStats>().weapons = true;
        app.world_mut().resource_mut::<Powers>().selected = Power::Empty;
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Digit1);
        app.update();
        assert_eq!(app.world().resource::<Powers>().selected, Power::Empty);
        // (back in the game: the key pressed and let go takes its slot, the pistol's)
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release(KeyCode::Digit1);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Digit1);
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release(KeyCode::Digit1);
        app.update();
        assert_eq!(app.world().resource::<Powers>().selected, Power::Pistol);
        assert_eq!(app.world().resource::<Shortcuts>().slots[5], Some(Power::Heart));
    }

    #[test]
    fn shortcuts_fill_by_kind_and_keep_his_own() {
        let mut stats = PlayerStats::default();
        stats.weapons = true;
        stats.powers.insert("Blink".into(), 1);
        let mut sc = Shortcuts::default();
        sc.refresh(&stats, false);
        // weapons on 1-5, the Heart on 6, powers on 7-0
        assert_eq!(sc.slots[0], Some(Power::Pistol));
        assert_eq!(sc.slots[1], Some(Power::Crossbow));
        assert_eq!(sc.slots[5], Some(Power::Heart));
        assert_eq!(sc.slots[6], Some(Power::Blink));
        assert_eq!(sc.slots[7], None);
        // put on another slot by him: off its old one, and kept there
        assert!(sc.assign(9, Power::Blink));
        sc.refresh(&stats, false);
        assert_eq!(sc.slots[9], Some(Power::Blink));
        assert_eq!(sc.slots[6], None);
        // taken off his slot: it stays empty
        assert!(!sc.assign(9, Power::Blink));
        sc.refresh(&stats, false);
        assert_eq!(sc.slots[9], None);
    }

    #[test]
    fn paused_mouse_release_keeps_a_cooking_grenade_in_hand() {
        let mut app = App::new();
        let mut powers = Powers::default();
        powers.selected = Power::Grenade;
        powers.cooking = Some((Power::Grenade, 0.5));
        app.init_resource::<Time>().init_resource::<TimeControl>()
            .init_resource::<ButtonInput<KeyCode>>().init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<crate::bindings::Bindings>().init_resource::<PlayerStats>()
            .insert_resource(crate::hud::Paused(true)).insert_resource(powers)
            .add_message::<crate::gadgets::UseGadget>().add_systems(Update, cook_grenade);
        app.world_mut().spawn((PlayerCamera, GlobalTransform::IDENTITY));
        app.update();
        assert_eq!(app.world().resource::<Powers>().cooking, Some((Power::Grenade, 0.5)));
        assert_eq!(app.world().resource::<Messages<crate::gadgets::UseGadget>>().len(), 0);
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.update();
        assert!(app.world().resource::<Powers>().cooking.is_none());
        assert_eq!(app.world().resource::<Messages<crate::gadgets::UseGadget>>().len(), 1);
    }

    #[test]
    fn paused_menu_shortcuts_do_not_drink_elixirs() {
        let mut app = App::new();
        let mut stats = PlayerStats::default();
        stats.health = 10.0;
        stats.max_health = 100.0;
        stats.mana = 10.0;
        stats.max_mana = 100.0;
        stats.mana_cap = 10.0;
        stats.health_elixirs = 2;
        stats.mana_elixirs = 2;
        app.init_resource::<Time>().init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<crate::bindings::Bindings>().init_resource::<Attrs>()
            .init_resource::<HudMessages>().insert_resource(crate::hud::Paused(true))
            .insert_resource(stats)
            .add_message::<PostEvent>().add_systems(Update, regenerate);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyR);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyT);
        app.update();
        let stats = app.world().resource::<PlayerStats>();
        assert_eq!((stats.health_elixirs, stats.mana_elixirs), (2, 2));
        assert_eq!((stats.health, stats.mana), (10.0, 10.0));
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.update();
        assert_eq!(app.world().resource::<PlayerStats>().health_elixirs, 2);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release_all();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyR);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyT);
        app.update();
        let stats = app.world().resource::<PlayerStats>();
        assert_eq!((stats.health_elixirs, stats.mana_elixirs), (1, 1));
        assert!(stats.health > 10.0 && stats.mana > 10.0);
    }
}

/// Mana and health regeneration (the original's step regeneration up to a cap) and elixirs
/// (`GBA_HealthElixir` R, `GBA_ManaElixir` T).
#[allow(clippy::too_many_arguments)]
fn regenerate(
    time: Res<Time>,
    paused: Res<crate::hud::Paused>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    attrs: Res<Attrs>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut sfx: MessageWriter<PostEvent>,
    mut last_health: Local<f32>,
) {
    if paused.0 { return; }
    let dt = time.delta_secs();
    stats.damage_flash = (stats.damage_flash - dt * 1.5).max(0.0);
    if stats.dead {
        *last_health = 0.0;
        return;
    }
    // mana: after a delay, back up to the cap left by the last expense
    stats.mana_delay = (stats.mana_delay - dt).max(0.0);
    if stats.mana_delay <= 0.0 && stats.mana < stats.mana_cap {
        stats.mana = (stats.mana + attrs.mana_regen_rate * dt).min(stats.mana_cap);
    }
    // health: hurt restarts the delay; then it regenerates up to its limit
    if stats.health < *last_health - 0.01 {
        stats.health_delay = attrs.health_regen_delay;
    }
    stats.health_delay = (stats.health_delay - dt).max(0.0);
    if stats.health_delay <= 0.0 && stats.health < attrs.health_regen_limit.min(stats.max_health) {
        stats.health = (stats.health + attrs.health_regen_rate * dt).min(attrs.health_regen_limit.min(stats.max_health));
    }
    *last_health = stats.health;
    if keys.just_pressed(bind.key(crate::bindings::Act::HealthElixir)) {
        if stats.health_elixirs > 0 && stats.health < stats.max_health {
            stats.health_elixirs -= 1;
            stats.health = (stats.health + attrs.health_elixir).min(stats.max_health);
            *last_health = stats.health;
            sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Life", None));
        } else if stats.health_elixirs == 0 {
            msgs.push("No health elixirs");
        }
    }
    if keys.just_pressed(bind.key(crate::bindings::Act::ManaElixir)) {
        if stats.mana_elixirs > 0 && stats.mana < stats.max_mana {
            stats.mana_elixirs -= 1;
            stats.mana = (stats.mana + attrs.mana_elixir_amount(stats.max_mana, rand::random::<f32>())).min(stats.max_mana);
            stats.mana_cap = stats.mana_cap.max(stats.mana);
            sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Mana", None));
        } else if stats.mana_elixirs == 0 {
            msgs.push("No mana elixirs");
        }
    }
}

/// Dark Vision's grading over the world's (`Twk_DarkVision`'s `m_StaticUberAdjustement`), fading
/// in and out over its level's durations.
fn dark_vision_post(powers: Res<Powers>, data: Res<Data>, stats: Res<PlayerStats>, mut post: ResMut<crate::postfx::PowerPost>) {
    use crate::postfx::PowerPost;
    if powers.dark_vision == post.layers.contains_key(&PowerPost::DARK_VISION) {
        return;
    }
    if !powers.dark_vision {
        post.set(PowerPost::DARK_VISION, None);
        return;
    }
    let level = stats.power("DarkVision").max(1);
    let layer = data.active("DarkVision").map(|a| crate::postfx::PostLayer {
        fields: crate::postfx::uber_fields(&a.params, "m_PpGlobalParameters.m_StaticUberAdjustement."),
        fade_in: data.power_f("DarkVision", level, "m_PpPerLevelParameters.m_PowerFadeInDuration", 0.1),
        fade_out: data.power_f("DarkVision", level, "m_PpPerLevelParameters.m_PowerFadeOutDuration", 0.2),
    });
    post.set(PowerPost::DARK_VISION, layer);
}

pub fn configure_xray_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (cfg, _) = store.config_mut::<DefaultGizmoConfigGroup>();
    cfg.depth_bias = -1.0;
    cfg.line.width = 2.0;
}
