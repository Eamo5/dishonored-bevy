//! Gadgets bought from Piero (and found): grenades and sticky grenades, springrazors,
//! incendiary bolts and explosive bullets, with the original tweaks' numbers
//! (`DisTweaks_Explosion`: 6 m reach, full damage within 3 m; `DisTweaks_SpringRazorPlaced`:
//! a 2.5 m trigger cylinder, 0.5 s arming, shrapnel to 9 m). Daud's (The Knife of Dunwall):
//! chokedust, thrown as a grenade, a cloud that leaves whoever breathes it choking
//! (`DisDLC06Tweaks_Explosion`); arc mines and stun mines, placed on any surface, lightning
//! for whoever comes near (`DisDLC06Tweaks_ArcMinePlaced`), picked back up while unspent.

use crate::audio::PostEvent;
use crate::gameplay::{HitKind, HudMessages, Noise, NoiseKind, NpcHit, PlayerStats, TimeControl};
use crate::level::{GameAssets, GROUP_NPC, GROUP_PROP, GROUP_WORLD};
use crate::npc::Npc;
use crate::particles::SpawnEffect;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct GadgetsPlugin;

impl Plugin for GadgetsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<UseGadget>()
            .init_resource::<GadgetRestore>()
            .add_message::<Explosion>()
            .add_message::<NpcGrenade>()
            .add_message::<ChokeDust>()
            .add_message::<Gunshot>()
            .add_systems(Update, grenade_focus.after(crate::interact::FocusSet).before(crate::props::prop_focus).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, mine_focus.after(grenade_focus).before(crate::props::prop_focus).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, restore_gadgets.after(crate::save::restore_npcs).before(grenade_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (use_gadget, npc_grenades, grenades_struck, fly_grenades, razors, mines, choke_clouds, acid_clouds, explode).chain().after(grenade_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, scripted_mine_clearing.run_if(in_state(GameState::InGame)));
    }
}

/// The scripts clear Daud's mines from a volume (`DisDLC06SeqAct_DestroyArcMines`: the ground a
/// scene plays on, Timsh's streets' ambush, the ship's deck): those placed inside are gone.
fn scripted_mine_clearing(mut commands: Commands, vm: Option<ResMut<crate::kismet::Vm>>, level: Option<Res<crate::level::LevelInfo>>, mines: Query<(Entity, &Razor, &Transform)>) {
    let (Some(mut vm), Some(level)) = (vm, level) else { return };
    if vm.destroy_mines.is_empty() {
        return;
    }
    for v in std::mem::take(&mut vm.destroy_mines) {
        let Some(vol) = level.scene.volumes.get(v as usize) else { continue };
        for (e, r, t) in &mines {
            if r.mine.is_some() && vol.hulls.iter().any(|h| hull_contains(h, t.translation)) {
                commands.entity(e).despawn();
            }
        }
    }
}

/// Whether a convex hull (its points) holds a point: within its bounds, and inside the hull.
fn hull_contains(points: &[[f32; 3]], p: Vec3) -> bool {
    let pts: Vec<Vec3> = points.iter().map(|q| Vec3::from(*q)).collect();
    if pts.len() < 4 {
        return false;
    }
    let (lo, hi) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), q| (l.min(*q), h.max(*q)));
    if p.cmplt(lo - 0.05).any() || p.cmpgt(hi + 0.05).any() {
        return false;
    }
    match bevy_rapier3d::prelude::Collider::convex_hull(&pts) {
        Some(c) => c.contains_point(Vec3::ZERO, Quat::IDENTITY, p),
        None => true,
    }
}

#[cfg(test)]
mod hull_tests {
    use super::*;

    #[test]
    fn a_point_is_held_by_a_box_hull_and_not_outside_it() {
        let cube: Vec<[f32; 3]> = (0..8).map(|i| [(i & 1) as f32 * 2.0, (i >> 1 & 1) as f32 * 2.0, (i >> 2 & 1) as f32 * 2.0]).collect();
        assert!(hull_contains(&cube, Vec3::splat(1.0)));
        assert!(!hull_contains(&cube, Vec3::new(3.0, 1.0, 1.0)));
        assert!(!hull_contains(&cube[..3], Vec3::splat(1.0)));
    }
}

/// The gadgets' ammunition in the inventory (store items).
pub const GRENADES: &str = "Grenade_Ammo_WithItem_twk";
pub const STICKY: &str = "StickyGrenade_Ammo_WithItem_twk";
pub const RAZORS: &str = "SpringRazor_Ammo_WithItem_twk";
/// Rewire tools, by the key they are carried under: Corvo's store item (`RewireTool_twk`).
pub const REWIRE: &str = "RewireTool_twk";

/// The key gear is carried under: a rewire tool by any of its names (Daud's store's
/// `DLC06_Pckp_RewireTool`, the pickups' `RewireTool_AbsItm`, `DLC06_AbsItm_RewireTool`)
/// as Corvo's, so either player's tools rewire; anything else as itself.
pub fn gear_key(item: &str) -> &str {
    if item.to_ascii_lowercase().contains("rewiretool") {
        REWIRE
    } else {
        item
    }
}

#[cfg(test)]
mod gear_tests {
    #[test]
    fn every_rewire_tool_is_carried_as_one_kind() {
        for n in ["RewireTool_twk", "AbstractItems.RewireTool_AbsItm", "DLC06_AbsItm_RewireTool", "DLC06_Pckp_RewireTool"] {
            assert_eq!(super::gear_key(n), super::REWIRE);
        }
        assert_eq!(super::gear_key("HereticBrand_AbsItm"), "HereticBrand_AbsItm");
    }
}

pub const FLARES: &str = "Flare_Ammo_twk";
pub const EXPLOSIVE: &str = "ExplosiveBullets_Ammo_twk";
/// Daud's (The Knife of Dunwall).
pub const EXPLOSIVE_BOLTS: &str = "DLC06_Ammo_ExplosiveBolt";
pub const ARC_MINES: &str = "DLC06_Ammo_ArcMine";
pub const CHOKEDUST: &str = "DLC06_Ammo_ChokeDust";
pub const STUN_MINES: &str = "DLC06_Ammo_StunMine";

/// The original's `eDisAmmoType`, in order: Corvo's eight, then The Knife of Dunwall's.
pub const AMMO_NAMES: [&str; AMMO_TYPES] = ["Bullet", "Bullet_Explosive", "Arrow", "Arrow_Sleep", "Arrow_Flare", "SpringRazor", "Grenade", "StickyGrenade", "Arrow_Explosive", "ArcMine", "FlashSmoke", "ArcMineStun"];
pub const AMMO_TYPES: usize = 12;

/// An ammunition type by its original name (`eDisAmmoType_` dropped).
pub fn ammo_index(name: &str) -> Option<u8> {
    AMMO_NAMES.iter().position(|n| n.eq_ignore_ascii_case(name.trim_start_matches("eDisAmmoType_"))).map(|i| i as u8)
}

/// Original pickup ranges are inclusive; fixed quantities do not consume a random roll.
pub fn roll_ammo_amount(min: u32, max: u32) -> u32 {
    roll_ammo_amount_with(min, max, &mut rand::rng())
}

fn roll_ammo_amount_with(min: u32, max: u32, rng: &mut impl rand::Rng) -> u32 {
    let min = min.min(max);
    if min == max { max } else { rng.random_range(min..=max) }
}

/// Ammunition by the original ammo type (the index into `m_AmmoRanges`): adds it and names it.
pub fn give_ammo(stats: &mut PlayerStats, attrs: &crate::gamedata::Attrs, ty: u8, n: u32) -> (&'static str, u32) {
    let Some(&capacity) = attrs.ammo_capacity.get(ty as usize) else { return ("Ammo", 0) };
    let count = ammo_mut(stats, ty, true).unwrap();
    let added = n.min(capacity.saturating_sub(*count));
    *count += added;
    (ammo_name(ty), added)
}

pub fn ammo_count(stats: &PlayerStats, ty: u8) -> u32 {
    match ty {
        0 => stats.bullets,
        2 => stats.bolts,
        3 => stats.sleep_darts,
        _ => ammo_item(ty).and_then(|id| stats.items.get(id)).copied().unwrap_or(0),
    }
}

fn ammo_item(ty: u8) -> Option<&'static str> {
    Some(match ty { 1 => EXPLOSIVE, 4 => FLARES, 5 => RAZORS, 6 => GRENADES, 7 => STICKY, 8 => EXPLOSIVE_BOLTS, 9 => ARC_MINES, 10 => CHOKEDUST, 11 => STUN_MINES, _ => return None })
}

pub fn ammo_type(item: &str) -> Option<u8> {
    // (Daud's stores' ammunition by its type: `ammo:<type>`)
    if let Some(ty) = item.strip_prefix("ammo:").and_then(|t| t.parse::<u8>().ok()) {
        return (ty < AMMO_TYPES as u8).then_some(ty);
    }
    Some(match item {
        "Bullets_Store_Ammo_twk" | "Bullets_Ammo_twk" => 0,
        "Bolt_Ammo_twk" | "Bolts_Ammo_twk" => 2,
        "SleepDart_Ammo_twk" => 3,
        _ => return (0..AMMO_TYPES as u8).find(|ty| ammo_item(*ty) == Some(item)),
    })
}

pub fn ammo_name(ty: u8) -> &'static str {
    match ty {
        0 => "Bullets",
        1 => "Explosive Bullets",
        2 => "Crossbow Bolts",
        3 => "Sleep Darts",
        4 => "Incendiary Bolts",
        5 => "Springrazors",
        6 => "Grenades",
        8 => "Explosive Bolts",
        9 => "Arc Mines",
        10 => "Chokedust",
        11 => "Stun Mines",
        _ => "Sticky Grenades",
    }
}

/// The count of an ammunition type (the original's `eDisAmmoType` order, as `give_ammo`);
/// a gadget's only when had, or `add` to have it.
pub fn ammo_mut(stats: &mut PlayerStats, ty: u8, add: bool) -> Option<&mut u32> {
    let key = match ty {
        0 => return Some(&mut stats.bullets),
        2 => return Some(&mut stats.bolts),
        3 => return Some(&mut stats.sleep_darts),
        _ => ammo_item(ty)?,
    };
    if add {
        Some(stats.items.entry(key.to_string()).or_default())
    } else {
        stats.items.get_mut(key)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Gadget {
    Grenade,
    StickyGrenade,
    SpringRazor,
    /// Daud's
    Chokedust,
    ArcMine,
    StunMine,
}

/// A mine of Daud's: lethal (`DLC06_ArcMine_Placed`) or stunning (`DLC06_StunMine_Placed`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum MineKind {
    Arc,
    Stun,
}

impl MineKind {
    /// Its numbers' prefix in the cooked pawn data, its model, its name.
    fn key(self) -> &'static str {
        match self {
            MineKind::Arc => "arcmine",
            MineKind::Stun => "stunmine",
        }
    }
    fn name(self) -> &'static str {
        match self {
            MineKind::Arc => "Arc Mine",
            MineKind::Stun => "Stun Mine",
        }
    }
    fn ammo(self) -> u8 {
        match self {
            MineKind::Arc => 9,
            MineKind::Stun => 11,
        }
    }
}

/// Chokedust going off: its cloud (`m_fDamageRadius`), lasting `m_fExplosionDuration`.
#[derive(Message, Clone, Copy)]
pub struct ChokeDust {
    pub at: Vec3,
    pub radius: f32,
    pub secs: f32,
}

/// How long chokedust leaves someone choking (`m_fSmokeGrenadeStunTime`).
pub const CHOKEDUST_STUN: f32 = 4.0;

#[derive(Component)]
struct ChokeCloud {
    radius: f32,
    left: f32,
    /// those it has had (each chokes once)
    had: Vec<Entity>,
}

/// Chokedust's cloud (`DLC06_Proj_ChokeDust`'s explosion: 300 uu across, 3 s) and the most
/// left of its fuse once it hits someone, from Daud's data.
fn chokedust_blast(data: &crate::gamedata::Data) -> ([f32; 5], f32) {
    let x = |k: &str, d: f32| data.pawn(&format!("chokedust.explosion.{k}"), d);
    let r = x("m_fDamageRadius", 300.0) * 0.01;
    ([r, x("m_fFullDamageRadius", 300.0) * 0.01, 0.0, 0.0, x("m_fExplosionDuration", 3.0)], data.pawn("chokedust.m_fMinDetonationDelayAfterHit", FUSE_AFTER_HIT))
}

/// Throw or place a gadget (the left hand, aimed along `dir`).
#[derive(Message, Clone, Copy)]
pub struct UseGadget {
    pub gadget: Gadget,
    pub eye: Vec3,
    pub dir: Vec3,
    /// a grenade's fuse already burnt in the hand (cooking): all of it, it goes off there
    pub cooked: f32,
}

/// A blast: everyone within `radius` is hurt, fully within `full`.
#[derive(Message, Clone, Copy)]
pub struct Explosion {
    pub at: Vec3,
    pub radius: f32,
    pub full: f32,
    pub damage: f32,
    /// the original effect (`scene.effects`)
    pub effect: &'static str,
    /// its reach to Corvo, all and full (`m_fPlayerDamageRadius`, `m_fPlayerFullDamageRadius`;
    /// else `radius` and 1.5 m)
    pub player: Option<[f32; 2]>,
    /// Damage attribution survives the projectile, including a returned enemy grenade.
    pub kind: HitKind,
    /// its maker plays its own sound (a whiskey bottle's `m_pSoundEvent`), not its kind's
    pub silent: bool,
}

/// A grenade a character throws (an overseer's): from where, how fast, its fuse (and the most
/// left once it hits someone), its blast.
#[derive(Message, Clone)]
pub struct NpcGrenade {
    pub owner: Entity,
    pub from: Vec3,
    pub vel: Vec3,
    pub fuse: [f32; 2],
    pub blast: dhcook::format::TrapBlast,
    /// what it is thrown at, and the danger zone the throw lays to it (`danger`)
    pub target: Vec3,
    pub danger: f32,
    /// its look in flight, and a blast that lingers (the Dead Eels' acid bottles)
    pub look: dhcook::format::GrenadeLook,
}

/// A grenade whose blast lingers (`DisDLC06Tweaks_Explosion`: the Dead Eels' acid bottles): its look
/// and blast's own effect and sound.
#[derive(Component, Clone, serde::Serialize, serde::Deserialize)]
pub struct Lingering {
    look: dhcook::format::GrenadeLook,
    effect: Option<u32>,
    sound: String,
}

/// A lingering blast where it went off (`DisDLC06Explosion`): for `left` s, every `interval` it
/// catches the characters within its reach and Corvo within his, each once (`m_DamagedActors`),
/// that seen from it.
#[derive(Component)]
pub struct AcidCloud {
    /// reach to characters, to Corvo, damage
    reach: [f32; 3],
    left: f32,
    interval: f32,
    tick: f32,
    had: Vec<Entity>,
    corvo_had: bool,
    lens: Option<u32>,
}

/// What pulls a thrown grenade down (m/s², the original's 1500 uu/s²).
const GRAVITY: f32 = 15.0;

/// The velocity that throws from `from` to land at `to` at `speed` (m/s): the low arc (at 45
/// degrees, as far as it goes, when it can't reach).
pub fn aim_throw(from: Vec3, to: Vec3, speed: f32) -> Vec3 {
    let d = to - from;
    let flat = d.with_y(0.0);
    let x = flat.length().max(0.01);
    let dir = flat / x;
    let v2 = speed * speed;
    let disc = v2 * v2 - GRAVITY * (GRAVITY * x * x + 2.0 * d.y * v2);
    let ang = if disc < 0.0 { std::f32::consts::FRAC_PI_4 } else { ((v2 - disc.sqrt()) / (GRAVITY * x)).atan() };
    dir * ang.cos() * speed + Vec3::Y * ang.sin() * speed
}

/// The grenade's throw and fuse (`Twk_Inv_GrenadeCorvo`'s `DisTweaks_ThrowGrenade`,
/// `Twk_Proj_Grenade`'s `DisTweaks_GrenadeComponent`): its speed (m/s), the angle above the
/// view, the fuse, and the fuse left at most once it hits someone.
const THROW_SPEED: f32 = 25.0;
const THROW_ANGLE: f32 = 5.0;
pub const FUSE: f32 = 3.0;
const FUSE_AFTER_HIT: f32 = 0.5;

/// The view's direction pitched up by `deg`.
fn throw_dir(dir: Vec3, deg: f32) -> Vec3 {
    let flat = dir.with_y(0.0);
    let right = flat.cross(Vec3::Y).normalize_or(Vec3::X);
    (Quat::from_axis_angle(right, deg.to_radians()) * dir).normalize_or(dir)
}

#[derive(Component)]
pub struct Grenade {
    vel: Vec3,
    pub fuse: f32,
    /// the whole fuse, and the most left once it hits someone
    pub fuse_total: f32,
    after_hit: f32,
    sticky: bool,
    stuck: Option<(Entity, Vec3)>,
    /// its blast: reach to characters (all, full), to Corvo (all, full), damage
    pub blast: [f32; 5],
    /// who threw it (a character: its own body doesn't stop it, Corvo's does)
    owner: Option<Entity>,
    throwback: bool,
    /// Kept separately from adhesion, which changes after a sticky grenade lands.
    kind: HitKind,
    /// the danger zone about it (m: its reach x `m_fAIDangerRatio`), and whether it has laid it
    /// since it left the hand (`danger`: again where it strikes)
    danger: f32,
    armed: bool,
}

impl Grenade {
    /// The character who threw it (none: Corvo's, or taken from its thrower).
    pub fn thrower(&self) -> Option<Entity> {
        self.owner
    }

    /// The character it's stuck to.
    pub fn stuck_on(&self) -> Option<Entity> {
        self.stuck.map(|s| s.0)
    }

    /// Caught in a Wind Blast (the original redirects the projectiles in its cone, as roshan's
    /// r614/blink research found): sent on along the blast at least as fast, the thrower's no
    /// more; one stuck to someone stays.
    pub fn blown(&mut self, dir: Vec3) {
        if self.stuck.is_some() {
            return;
        }
        self.take();
        self.vel = dir * (self.vel.length() * 1.2).max(12.0) + Vec3::Y * 2.0;
    }
    fn take(&mut self) {
        self.throwback |= self.owner.take().is_some();
        if self.kind == HitKind::EnemyExplosion {
            self.kind = HitKind::Explosion;
        }
        self.stuck = None;
        self.vel = Vec3::ZERO;
        // Picking one up never restarts its fuse.
    }

    fn release(&mut self, direction: Vec3, velocity: Vec3, strength: f32, thrown: bool) {
        self.vel = velocity.with_y(0.0) + if thrown { throw_dir(direction, THROW_ANGLE) * strength.max(5.0) } else { direction * 1.0 };
        if thrown && self.throwback {
            self.kind = HitKind::GrenadeThrowback;
        }
    }
}

/// The armed projectile's original interaction is "Tap `k to carry". It stays
/// live in the hand, uses the ordinary movable drop/throw prompts, and consumes
/// no inventory ammunition when thrown back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn grenade_focus(
    paused: Res<crate::hud::Paused>,
    bind: Res<crate::bindings::Bindings>,
    mut focus: ResMut<crate::interact::InteractFocus>,
    mut held: ResMut<crate::props::Held>,
    mut grenades: Query<(Entity, &mut Grenade, &Transform)>,
    player: Query<(Entity, &Player)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    rapier: ReadRapierContext,
    (stats, attrs, carry, possession, peek): (Res<PlayerStats>, Res<crate::gamedata::Attrs>, Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::keyhole::Peek>),
) {
    if paused.0 { return; }
    let (Ok((pe, p)), Ok(camera)) = (player.single(), cam.single()) else { return };
    if let Some(e) = held.0 {
        let Ok((_, mut grenade, _)) = grenades.get_mut(e) else { return };
        focus.0 = true;
        focus.1 = None;
        focus.2.clear();
        focus.3.clear();
        let thrown = bind.just_pressed(crate::bindings::Act::Primary) && !stats.dead && possession.host.is_none();
        if thrown || bind.just_pressed(crate::bindings::Act::Use) || stats.dead || possession.host.is_some() {
            grenade.release(camera.forward().as_vec3(), p.velocity, attrs.throw, thrown);
            held.0 = None;
            held.1 = 0.3;
        }
        return;
    }
    if held.busy() || stats.dead || p.locked || carry.carrying() || possession.host.is_some() || peek.at.is_some() {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let eye = camera.translation();
    let dir = camera.forward().as_vec3();
    let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
    let nearest = grenades.iter().filter_map(|(e, grenade, t)| {
        let delta = t.translation - eye;
        let along = delta.dot(dir);
        if grenade.fuse <= 0.0 || !(0.0..=2.2).contains(&along) || (delta - dir * along).length_squared() > 0.18 * 0.18 {
            return None;
        }
        let distance = delta.length();
        if distance > 0.12 && ctx.cast_ray(eye, delta / distance, distance - 0.12, true, filter).is_some() {
            return None;
        }
        Some((e, along))
    }).min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((e, _)) = nearest else { return };
    focus.0 = true;
    focus.1 = Some(e);
    focus.2 = format!("Tap {} Carry", crate::bindings::hint(crate::bindings::Act::Use));
    focus.3 = if grenades.get(e).is_ok_and(|g| g.1.kind == HitKind::Chokedust) { "Primed Chokedust" } else { "Armed Grenade" }.into();
    if bind.just_pressed(crate::bindings::Act::Use) {
        if let Ok((_, mut grenade, _)) = grenades.get_mut(e) {
            grenade.take();
            held.0 = Some(e);
        }
    }
}

/// Corvo's grenade's blast (`Twk_Proj_Grenade`'s `DisTweaks_Explosion`: 600 uu reach, 300 uu
/// full damage, 600 / 150 uu to him, 50 damage).
const BLAST: [f32; 5] = [6.0, 3.0, 6.0, 1.5, 50.0];

#[derive(Component)]
pub(crate) struct Razor {
    armed: f32,
    triggered: Option<f32>,
    /// a mine of Daud's instead: which, the shots it has fired, the wait before the next
    mine: Option<MineKind>,
    shots: u32,
    wait: f32,
}

impl Razor {
    fn placed(mine: Option<MineKind>) -> Self {
        Razor { armed: 0.0, triggered: None, mine, shots: 0, wait: 0.0 }
    }

    /// what Dark Vision shows of it (`darkvision`): an item, a mine once it has fired a
    /// mechanism (`DisDLC06Gadget_ArcMinePlaced`: unused, its shots all left)
    pub(crate) fn soul(&self) -> u8 {
        if self.mine.is_some() && self.shots > 0 { 2 } else { 1 }
    }
}

/// Entity references use saved NPC spawners, never runtime entity IDs.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct GadgetsSave {
    grenades: Vec<GrenadeSave>,
    razors: Vec<RazorSave>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GrenadeSave {
    position: [f32; 3],
    rotation: [f32; 4],
    velocity: [f32; 3],
    fuse: f32,
    fuse_total: f32,
    after_hit: f32,
    sticky: bool,
    /// None: free flight; Some(None, ..): stuck to the world.
    stuck: Option<(Option<u32>, [f32; 3])>,
    blast: [f32; 5],
    owner: Option<u32>,
    throwback: bool,
    kind: HitKind,
    held: bool,
    #[serde(default)]
    danger: f32,
    /// a blast that lingers, and the grenade's own look (a Dead Eel's acid bottle)
    #[serde(default)]
    lingering: Option<Lingering>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RazorSave {
    position: [f32; 3],
    rotation: [f32; 4],
    armed: f32,
    triggered: Option<f32>,
    #[serde(default)]
    mine: Option<MineKind>,
    #[serde(default)]
    shots: u32,
    #[serde(default)]
    wait: f32,
}

#[derive(Resource, Default)]
pub struct GadgetRestore(pub Option<GadgetsSave>);

impl GadgetsSave {
    pub(crate) fn capture<'a>(
        grenades: impl IntoIterator<Item = (Entity, &'a Grenade, &'a Transform, Option<&'a Lingering>)>,
        razors: impl IntoIterator<Item = (&'a Razor, &'a Transform)>,
        npcs: impl IntoIterator<Item = (Entity, u32)>,
        held: Option<Entity>,
    ) -> Self {
        let npcs: std::collections::HashMap<_, _> = npcs.into_iter().collect();
        Self {
            grenades: grenades.into_iter().map(|(e, g, t, l)| GrenadeSave {
                position: t.translation.to_array(), rotation: t.rotation.to_array(), velocity: g.vel.to_array(),
                fuse: g.fuse, fuse_total: g.fuse_total, after_hit: g.after_hit, sticky: g.sticky,
                stuck: g.stuck.map(|(e, offset)| (npcs.get(&e).copied(), offset.to_array())),
                blast: g.blast, owner: g.owner.and_then(|e| npcs.get(&e).copied()),
                throwback: g.throwback, kind: g.kind, held: held == Some(e), danger: g.danger, lingering: l.cloned(),
            }).collect(),
            razors: razors.into_iter().map(|(r, t)| RazorSave {
                position: t.translation.to_array(), rotation: t.rotation.to_array(), armed: r.armed, triggered: r.triggered,
                mine: r.mine, shots: r.shots, wait: r.wait,
            }).collect(),
        }
    }
}

/// Run after saved characters have spawned so adhesive grenades and their owners
/// point at the new entities. Restoring never replays a throw or restarts a fuse.
fn restore_gadgets(
    mut commands: Commands,
    mut pending: ResMut<GadgetRestore>,
    npcs: Query<(Entity, &crate::npc::FromSpawner)>,
    old: Query<Entity, Or<(With<Grenade>, With<Razor>)>>,
    mut held: ResMut<crate::props::Held>,
    assets: Option<Res<GameAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(saved) = pending.0.take() else { return };
    for e in &old {
        if held.0 == Some(e) {
            held.0 = None;
        }
        commands.entity(e).despawn();
    }
    let npcs: std::collections::HashMap<_, _> = npcs.iter().map(|(e, s)| (s.0, e)).collect();
    let mut model = |entity: &mut EntityCommands, name: &str, fallback: Mesh| {
        if let Some(parts) = assets.as_ref().and_then(|a| a.props.get(name)) {
            entity.with_children(|c| {
                for (mesh, mat) in &parts.parts {
                    let mut m = c.spawn((Mesh3d(mesh.clone()), bevy::mesh::MeshTag(0), Transform::IDENTITY));
                    mat.apply(&mut m);
                }
            });
        } else {
            entity.insert((Mesh3d(meshes.add(fallback)), MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.12, 0.11, 0.1), perceptual_roughness: 0.6, metallic: 0.6, ..default()
            }))));
        }
    };
    for g in saved.grenades {
        let mut e = commands.spawn((
            Grenade {
                vel: Vec3::from(g.velocity), fuse: g.fuse, fuse_total: g.fuse_total, after_hit: g.after_hit,
                sticky: g.sticky, stuck: g.stuck.map(|(s, off)| (s.and_then(|s| npcs.get(&s).copied()).unwrap_or(Entity::PLACEHOLDER), Vec3::from(off))),
                blast: g.blast, owner: g.owner.and_then(|s| npcs.get(&s).copied()), throwback: g.throwback, kind: g.kind,
                danger: if g.danger > 0.0 { g.danger } else { g.blast[0] }, armed: false,
            },
            Transform::from_translation(Vec3::from(g.position)).with_rotation(Quat::from_array(g.rotation)),
            Visibility::default(), DespawnOnExit(GameState::InGame),
        ));
        if g.held {
            held.0 = Some(e.id());
            held.1 = 0.0;
        }
        let look = g.lingering.as_ref().map(|l| l.look.mesh.clone()).filter(|m| assets.as_ref().is_some_and(|a| a.props.contains_key(m.as_str())));
        model(&mut e, look.as_deref().unwrap_or(if g.kind == HitKind::Chokedust { "chokedust" } else { "grenade" }), Sphere::new(0.06).into());
        if let Some(l) = g.lingering {
            e.insert(l);
        }
    }
    for r in saved.razors {
        let mut e = commands.spawn((
            Razor { armed: r.armed, triggered: r.triggered, mine: r.mine, shots: r.shots, wait: r.wait },
            Transform::from_translation(Vec3::from(r.position)).with_rotation(Quat::from_array(r.rotation)),
            Visibility::default(), DespawnOnExit(GameState::InGame),
        ));
        model(&mut e, r.mine.map_or("springrazor", MineKind::key), Cylinder::new(0.09, 0.06).into());
    }
}

fn use_gadget(
    mut commands: Commands,
    mut uses: MessageReader<UseGadget>,
    mut stats: ResMut<PlayerStats>,
    mut sfx: MessageWriter<PostEvent>,
    rapier: ReadRapierContext,
    player: Query<Entity, With<Player>>,
    assets: Option<Res<GameAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    data: Res<crate::gamedata::Data>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    for u in uses.read() {
        let key = match u.gadget {
            Gadget::Grenade => GRENADES,
            Gadget::StickyGrenade => STICKY,
            Gadget::SpringRazor => RAZORS,
            Gadget::Chokedust => CHOKEDUST,
            Gadget::ArcMine => ARC_MINES,
            Gadget::StunMine => STUN_MINES,
        };
        let n = stats.items.get(key).copied().unwrap_or(0);
        if n == 0 {
            sfx.write(PostEvent::named("Snd_Power_Empty", None));
            continue;
        }
        // the original model, else a stand-in
        let mut model = |ec: &mut EntityCommands, prop: &str, fallback: Mesh| match assets.as_ref().and_then(|a| a.props.get(prop)) {
            Some(parts) => {
                ec.with_children(|c| {
                    for (mesh, mat) in &parts.parts {
                        let mut m = c.spawn((Mesh3d(mesh.clone()), bevy::mesh::MeshTag(0), Transform::IDENTITY));
                        mat.apply(&mut m);
                    }
                });
            }
            None => {
                ec.insert((
                    Mesh3d(meshes.add(fallback)),
                    MeshMaterial3d(mats.add(StandardMaterial { base_color: Color::srgb(0.12, 0.11, 0.1), perceptual_roughness: 0.6, metallic: 0.6, ..default() })),
                ));
            }
        };
        match u.gadget {
            Gadget::Grenade | Gadget::StickyGrenade | Gadget::Chokedust => {
                stats.items.insert(key.into(), n - 1);
                let choke = u.gadget == Gadget::Chokedust;
                let (blast, after_hit) = if choke { chokedust_blast(&data) } else { (BLAST, FUSE_AFTER_HIT) };
                // (the pin was pulled as it began to cook)
                let fuse = (FUSE - u.cooked).max(0.0);
                let mut ec = commands.spawn((
                    // (`DisTweaks_ThrowGrenade`: 2500 uu/s, 5 degrees above the view;
                    // `DisTweaks_GrenadeComponent.m_fDetonationDelay` 3 s)
                    Grenade {
                        vel: if fuse > 0.0 { throw_dir(u.dir, THROW_ANGLE) * THROW_SPEED } else { Vec3::ZERO },
                        fuse,
                        fuse_total: FUSE,
                        after_hit,
                        sticky: u.gadget == Gadget::StickyGrenade,
                        stuck: None,
                        blast,
                        owner: None,
                        throwback: false,
                        kind: match u.gadget {
                            Gadget::StickyGrenade => HitKind::StickyGrenade,
                            Gadget::Chokedust => HitKind::Chokedust,
                            _ => HitKind::Explosion,
                        },
                        // (its explosion's `m_fAIDangerRatio`: Corvo's grenade's the default, 1)
                        danger: blast[0] * if choke { data.pawn("chokedust.explosion.m_fAIDangerRatio", 1.0) } else { 1.0 },
                        armed: false,
                    },
                    Transform::from_translation(u.eye + u.dir * 0.5),
                    Visibility::default(),
                    DespawnOnExit(GameState::InGame),
                ));
                model(&mut ec, if choke { "chokedust" } else { "grenade" }, Sphere::new(0.06).into());
                // (thrown, it trails: `Ps_Grenade_Trail`, chokedust's own)
                if fuse > 0.0 {
                    fx.write(SpawnEffect { follow: Some(ec.id()), secs: fuse, ..SpawnEffect::at(if choke { "chokedust_trail" } else { "grenade_trail" }, Vec3::ZERO) });
                }
            }
            Gadget::SpringRazor | Gadget::ArcMine | Gadget::StunMine => {
                let mine = match u.gadget {
                    Gadget::ArcMine => Some(MineKind::Arc),
                    Gadget::StunMine => Some(MineKind::Stun),
                    _ => None,
                };
                // placed on the surface aimed at, within reach
                let Ok(ctx) = rapier.single() else { continue };
                let mut filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
                if let Ok(pe) = player.single() {
                    filter = filter.exclude_collider(pe);
                }
                // on the surface aimed at, else the floor in front
                let ahead = u.eye + u.dir.with_y(0.0).normalize_or_zero() * 1.2;
                let Some((_, hit)) = ctx.cast_ray_and_get_normal(u.eye, u.dir, 3.0, true, filter).or_else(|| ctx.cast_ray_and_get_normal(ahead, Vec3::NEG_Y, 3.0, true, filter)) else {
                    continue;
                };
                stats.items.insert(key.into(), n - 1);
                // (a mine hums as it arms: `m_pArmingStartSoundEvent`)
                sfx.write(PostEvent::named(if mine.is_some() { "Snd_ArcMine_Activated" } else { "Snd_Spring_Razor_Install" }, Some(hit.point)));
                let mut ec = commands.spawn((
                    Razor::placed(mine),
                    Transform::from_translation(hit.point + hit.normal * 0.03).with_rotation(Quat::from_rotation_arc(Vec3::Y, hit.normal)),
                    Visibility::default(),
                    DespawnOnExit(GameState::InGame),
                ));
                model(&mut ec, mine.map_or("springrazor", MineKind::key), Cylinder::new(0.09, 0.06).into());
            }
        }
    }
}

/// The characters' grenades: out of the hand, the grenade's own model.
#[allow(clippy::too_many_arguments)]
fn npc_grenades(mut commands: Commands, mut throws: MessageReader<NpcGrenade>, assets: Option<Res<GameAssets>>, data: Res<crate::gamedata::Data>, stats: Res<PlayerStats>, settings: Res<crate::settings::Settings>, mut fx: MessageWriter<SpawnEffect>) {
    for g in throws.read() {
        let b = &g.blast;
        let fuse = g.fuse[0] + data.attribute("GrenadeExplosionDelay", settings.difficulty, &stats.powers, &stats.charms).max(0.0);
        let mut ec = commands.spawn((
            Grenade {
                vel: g.vel,
                fuse,
                fuse_total: fuse,
                after_hit: g.fuse[1],
                sticky: false,
                stuck: None,
                blast: [b.radius, b.full, b.player_radius, b.player_full, b.damage[settings.difficulty.min(3) as usize]],
                owner: Some(g.owner),
                throwback: false,
                kind: HitKind::EnemyExplosion,
                danger: b.danger,
                armed: false,
            },
            Transform::from_translation(g.from),
            Visibility::default(),
            DespawnOnExit(GameState::InGame),
        ));
        // (its own mesh, `m_pGrenadeTweaks.m_pSkeletalMesh`: an overseer's grenade, a Dead Eel's acid
        // bottle)
        let props = assets.as_ref().map(|a| &a.props);
        if let Some(parts) = props.and_then(|p| p.get(g.look.mesh.as_str()).or_else(|| p.get("grenade"))) {
            ec.with_children(|c| {
                for (mesh, mat) in &parts.parts {
                    let mut m = c.spawn((Mesh3d(mesh.clone()), bevy::mesh::MeshTag(0), Transform::IDENTITY));
                    mat.apply(&mut m);
                }
            });
        }
        if g.look.linger > 0.0 {
            ec.insert(Lingering { look: g.look.clone(), effect: b.effect, sound: b.sound.clone() });
        }
        let id = ec.id();
        // (its trail, `m_pTrailEffect`)
        if let Some(sys) = g.look.trail {
            fx.write(SpawnEffect { follow: Some(id), system: Some(sys), secs: fuse, ..SpawnEffect::at("", Vec3::ZERO) });
        }
    }
}

/// A shot's path (Corvo's pistol; a character's comes as `NpcShot`), for what it passes.
#[derive(Message, Clone, Copy)]
pub struct Gunshot {
    pub from: Vec3,
    pub to: Vec3,
}

/// How long a grenade struck lasts before it goes off (`m_fExplosionDelayOnTakeDamage`, 0.2 s
/// on every grenade), and how near a shot passes to strike one (m).
const STRUCK_FUSE: f32 = 0.2;
const GRENADE_RADIUS: f32 = 0.12;

/// A grenade that takes damage of a kind that sets grenades off (`m_bExplodesGrenades`: a
/// bullet, a blast, a spring razor; not a bolt, a dart, the sword or a whiskey bottle) goes off
/// 0.2 s later, its fuse set to that, longer or shorter (0x00C584F0 / 0x00C582B0, decoded for
/// this rewrite).
fn grenades_struck(
    mut q: Query<(&mut Grenade, &Transform)>,
    (mut blasts, mut shots, mut gun): (MessageReader<Explosion>, MessageReader<crate::npc::NpcShot>, MessageReader<Gunshot>),
) {
    let mut paths: Vec<(Vec3, Vec3)> = shots.read().filter(|s| !s.bow).map(|s| (s.from, s.to)).collect();
    paths.extend(gun.read().map(|g| (g.from, g.to)));
    let blasts: Vec<(Vec3, f32)> = blasts.read().filter(|b| b.effect != "whiskey" && b.radius > 0.0).map(|b| (b.at, b.radius)).collect();
    if paths.is_empty() && blasts.is_empty() {
        return;
    }
    for (mut g, t) in &mut q {
        let p = t.translation;
        let shot = paths.iter().any(|(a, b)| {
            let ab = *b - *a;
            let k = ((p - *a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
            (*a + ab * k).distance(p) < GRENADE_RADIUS
        });
        let blasted = blasts.iter().any(|(at, r)| at.distance(p) < *r && at.distance(p) > 0.05);
        if shot || blasted {
            g.fuse = STRUCK_FUSE;
        }
    }
}

fn fly_grenades(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut q: Query<(Entity, &mut Grenade, &mut Transform, Option<&Lingering>)>,
    npcs: Query<(Entity, &Transform), (With<Npc>, Without<Grenade>)>,
    player: Query<Entity, With<Player>>,
    mut blasts: MessageWriter<Explosion>,
    mut sfx: MessageWriter<PostEvent>,
    mut held: ResMut<crate::props::Held>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    (mut clouds, mut hits, mut zones): (MessageWriter<ChokeDust>, MessageWriter<NpcHit>, ResMut<crate::danger::DangerZones>),
    (mut fx, mut rats, mut noise, data): (MessageWriter<SpawnEffect>, MessageWriter<crate::swarm::KillRats>, MessageWriter<Noise>, Res<crate::gamedata::Data>),
) {
    let dt = time.delta_secs() * tc.projectile_scale();
    let Ok(ctx) = rapier.single() else { return };
    let pe = player.single().ok();
    let all = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC | crate::level::GROUP_PLAYER));
    for (e, mut g, mut t, lingering) in &mut q {
        let in_hand = held.0 == Some(e);
        if in_hand {
            if let Ok(camera) = cam.single() {
                let eye = camera.translation();
                let dir = camera.forward().as_vec3();
                let filter = pe.map_or(all, |p| all.exclude_collider(p));
                let reach = ctx.cast_ray(eye, dir, 0.8, true, filter).map_or(0.8, |(_, distance)| (distance - 0.1).max(0.1));
                t.translation = eye + dir * reach - Vec3::Y * 0.15;
            }
        }
        g.fuse -= dt;
        if g.fuse <= 0.0 {
            let b = g.blast;
            // (one sunk into a wall goes off just out of it)
            let at = match g.stuck {
                Some((w, off)) if npcs.get(w).is_err() => t.translation + off,
                _ => t.translation,
            };
            if g.kind == HitKind::Chokedust {
                clouds.write(ChokeDust { at, radius: b[0], secs: b[4] });
            } else if let Some(l) = lingering {
                // (a blast that lingers: its cloud, its own effect and sound, the rats about it
                // dead, the danger left a while longer, `m_fAdditionalDangerTime`)
                let look = &l.look;
                commands.spawn((
                    AcidCloud { reach: [b[0], b[2], b[4]], left: look.linger, interval: look.interval.max(0.05), tick: 0.0, had: Vec::new(), corvo_had: false, lens: look.lens },
                    Transform::from_translation(at),
                    DespawnOnExit(GameState::InGame),
                ));
                if !l.sound.is_empty() {
                    sfx.write(PostEvent::named(&l.sound, Some(at)));
                }
                if let Some(sys) = l.effect {
                    fx.write(SpawnEffect { system: Some(sys), secs: look.linger + 2.0, ..SpawnEffect::at("", at) });
                }
                if look.rat_kill > 0.0 {
                    rats.write(crate::swarm::KillRats { at, radius: look.rat_kill, by_player: !matches!(g.kind, HitKind::EnemyExplosion | HitKind::ByOthers), source: at });
                }
                let loud = data.0.noise_loudness.get(&look.loudness).copied().unwrap_or(15.0);
                noise.write(Noise { corvo: !matches!(g.kind, HitKind::EnemyExplosion | HitKind::ByOthers), pos: at, radius: loud, kind: NoiseKind::Threatening });
                zones.sphere(e, look.linger + look.extra_danger, at, g.danger);
            } else {
                blasts.write(Explosion { at, radius: b[0], full: b[1], damage: b[4], effect: "grenade", player: Some([b[2], b[3]]), kind: g.kind, silent: false });
                // (stuck to someone: `m_fAttachedDamage` 75 more to them)
                if let Some((who, _)) = g.stuck.filter(|(w, _)| npcs.get(*w).is_ok()) {
                    hits.write(NpcHit { npc: who, damage: 75.0, kind: g.kind, from: t.translation });
                }
            }
            if in_hand {
                held.0 = None;
                held.1 = 0.3;
            }
            commands.entity(e).despawn();
            continue;
        }
        if in_hand {
            continue;
        }
        // its danger zone, as it leaves the hand, for the rest of its fuse (`danger`)
        if !g.armed {
            g.armed = true;
            zones.sphere(e, g.fuse, t.translation, g.danger);
        }
        // (Corvo's own pass through him; a character's through its thrower)
        let filter = match (g.owner, pe) {
            (Some(o), _) => all.exclude_collider(o),
            (None, Some(p)) => all.exclude_collider(p),
            (None, None) => all,
        };
        if let Some((who, off)) = g.stuck {
            if let Ok((_, nt)) = npcs.get(who) {
                t.translation = nt.translation + off;
            }
            continue;
        }
        g.vel.y -= 15.0 * dt;
        let step = g.vel * dt;
        let len = step.length();
        if len < 1e-5 {
            continue;
        }
        if let Some((hit, h)) = ctx.cast_ray_and_get_normal(t.translation, step / len, len + 0.06, true, filter) {
            if std::env::var_os("DH_GADGET_LOG").is_some() {
                info!("grenade strikes {hit:?} at {:.2} (speed {:.1})", h.point, g.vel.length());
            }
            t.translation = h.point + h.normal * 0.06;
            // (laid again where it strikes)
            zones.sphere(e, g.fuse, t.translation, g.danger);
            if g.sticky {
                // sticks to what it hits, sunk 5 uu into it (as roshan's r614/blink research found):
                // a person carries it; on the world it keeps how far out its blast starts
                t.translation = h.point - h.normal * 0.05;
                g.stuck = Some(npcs.get(hit).map(|(_, nt)| (hit, t.translation - nt.translation)).unwrap_or((hit, h.normal * 0.11)));
                if npcs.get(hit).is_err() {
                    g.vel = Vec3::ZERO;
                } else {
                    // (they flail at it, `npc.rs`)
                    commands.entity(hit).try_insert(crate::npc::StuckGrenade);
                }
            } else {
                // the bounce (as roshan's r614/blink research decoded it): v' = (v - (1 + e)(v.n)n) d,
                // e and d a floor's (0.65, 0.4) and a wall's (0.3, 0.1) blended by the normal's
                // height, a body's 0.1 and 0.05; it rests once it falls slower than 1.5 m/s x n.z.
                // The fuse runs on whatever it hits.
                let body = npcs.get(hit).is_ok() || Some(hit) == pe;
                let up = h.normal.y.clamp(0.0, 1.0);
                let (e, d) = if body { (0.1, 0.05) } else { (0.3 + 0.35 * up, 0.1 + 0.3 * up) };
                let speed = g.vel.length();
                g.vel = (g.vel - (1.0 + e) * g.vel.dot(h.normal) * h.normal) * d;
                if -g.vel.y < 1.5 * up && up > 0.5 && g.vel.length() < 1.5 {
                    g.vel = Vec3::ZERO;
                }
                if speed > 1.0 {
                    sfx.write(PostEvent::named("Snd_Phys_Metal_Grenade", Some(t.translation)));
                }
            }
        } else {
            t.translation += step;
        }
    }
}

fn razors(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut q: Query<(Entity, &mut Razor, &Transform)>,
    npcs: Query<(Entity, &Npc, &Transform), Without<Razor>>,
    mut hits: MessageWriter<NpcHit>,
    mut sfx: MessageWriter<PostEvent>,
    mut noise: MessageWriter<Noise>,
    mut fx: MessageWriter<SpawnEffect>,
    (data, mut stats, player): (Res<crate::gamedata::Data>, ResMut<PlayerStats>, Query<&Transform, (With<Player>, Without<Razor>)>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    // its cylinders (the original's, as roshan's r614/blink research decoded them): 250 x 250 uu
    // along the surface's normal, the blast's widened by the Spring Razor upgrade
    // (`SpringRazorRadiusBonus`)
    let bonus = data.0.upgrades.iter().filter(|u| stats.upgrades.contains(&u.id)).flat_map(|u| &u.attributes).filter(|(n, _)| n == "SpringRazorRadiusBonus").map(|(_, v)| *v).fold(1.0f32, f32::max);
    for (e, mut r, t) in &mut q {
        if r.mine.is_some() {
            continue;
        }
        let p = t.translation;
        let axis = (t.rotation * Vec3::Y).normalize_or(Vec3::Y);
        let inside = |at: Vec3, radius: f32| {
            let to = at - p;
            let along = to.dot(axis);
            (-0.3..=2.5).contains(&along) && (to - axis * along).length() < radius
        };
        let seen = |at: Vec3| {
            let to = at - p;
            let d = to.length();
            ctx.cast_ray(p + axis * 0.1, to / d.max(1e-3), d, true, walls).is_none()
        };
        if r.armed < 0.5 {
            r.armed += dt;
            if r.armed >= 0.5 {
                sfx.write(PostEvent::named("Snd_Spring_Razor_Arming", Some(p)));
            }
            continue;
        }
        match r.triggered {
            None => {
                // a character steps into its cylinder, in its sight
                if npcs.iter().any(|(_, n, nt)| !n.is_down() && inside(nt.translation, 2.5) && seen(nt.translation)) {
                    r.triggered = Some(0.0);
                }
            }
            Some(tt) if tt + dt >= 0.35 => {
                // the shrapnel (`m_fDamageActorDelayTime` 0.35 s on): every character in its
                // cylinder and sight dies; Corvo there takes `Twk_Inv_SpringRazorPlaced`'s
                // `m_ImmuneNPCDamage` (50: the pawns the cut can't sever take it, the characters
                // `m_NonNPCDamage`'s 5 and the cut, as roshan's r614/blink research decoded it)
                sfx.write(PostEvent::named("Snd_Spring_Razor_Shot", Some(p)));
                fx.write(SpawnEffect { rot: t.rotation, ..SpawnEffect::at("springrazor", p) });
                noise.write(Noise { corvo: false, pos: p, radius: 15.0, kind: NoiseKind::Threatening });
                if let Ok(pt) = player.single() {
                    if inside(pt.translation, 2.5 * bonus) && seen(pt.translation) && !stats.dead {
                        stats.take_damage(50.0);
                        stats.damage_flash = 1.0;
                        stats.hit_from = Some(p);
                        if stats.health <= 0.0 {
                            stats.dead = true;
                        }
                    }
                }
                for (ne, n, nt) in &npcs {
                    if n.is_down() || !inside(nt.translation, 2.5 * bonus) || !seen(nt.translation) {
                        continue;
                    }
                    let damage = 999.0;
                    hits.write(NpcHit { npc: ne, damage, kind: HitKind::SpringRazor, from: p });
                    sfx.write(PostEvent::named("Imp_SpringRazor_on_Body", Some(nt.translation)));
                    fx.write(SpawnEffect { follow: Some(ne), secs: 2.0, ..SpawnEffect::at("springrazor_blood", Vec3::Y * 0.3) });
                }
                commands.entity(e).despawn();
            }
            Some(tt) => r.triggered = Some(tt + dt),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn explode(
    mut commands: Commands,
    mut blasts: MessageReader<Explosion>,
    mut hits: MessageWriter<NpcHit>,
    mut stats: ResMut<PlayerStats>,
    mut sfx: MessageWriter<PostEvent>,
    mut noise: MessageWriter<Noise>,
    npcs: Query<(Entity, &Npc, &Transform)>,
    player: Query<&Transform, With<Player>>,
    mut flashes: Query<(Entity, &mut PointLight, &mut crate::fxlight::FxLight, &mut ExplosionFlash)>,
    time: Res<Time>,
    mut fx: MessageWriter<SpawnEffect>,
    mut rats: MessageWriter<crate::swarm::KillRats>,
    (rapier, attrs): (bevy_rapier3d::prelude::ReadRapierContext, Res<crate::gamedata::Attrs>),
    mut razors: Query<(&mut Razor, &Transform), Without<Npc>>,) {
    // a blast reaches what it can see (the original's `VisibleCollidingActors`)
    let ctx = rapier.single().ok();
    let walls = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
    let seen = |from: Vec3, to: Vec3| {
        let d = to - from;
        let l = d.length();
        l < 0.3 || ctx.as_ref().is_none_or(|c| c.cast_ray(from, d / l, l - 0.2, true, walls).is_none())
    };
    for b in blasts.read() {
        // (an explosive bolt's: `DLC06_DaudArrowExplosion.m_pSoundEvent`)
        if !b.silent {
            sfx.write(PostEvent::named(if b.kind == HitKind::ExplosiveBolt { "Snd_AI_TBoy_Arrow_Fire_Expl" } else { "Snd_Grenade_Explosion" }, Some(b.at)));
        }
        rats.write(crate::swarm::KillRats { at: b.at, radius: b.full.max(2.0), by_player: !matches!(b.kind, HitKind::EnemyExplosion | HitKind::ByOthers), source: b.at });
        if !b.silent {
            crate::rumble::play_near("contact.Explosion.DisContactType.radius", b.at);
        }
        fx.write(SpawnEffect::at(b.effect, b.at));
        noise.write(Noise { corvo: !matches!(b.kind, HitKind::EnemyExplosion | HitKind::ByOthers), pos: b.at, radius: 25.0, kind: NoiseKind::Threatening });
        let scale = |d: f32| if d <= b.full { 1.0 } else { (1.0 - (d - b.full) / (b.radius - b.full).max(0.1)).max(0.0) };
        // a blast sets off the Spring Razors it reaches (`m_fDamageReactionTime` 0.75 s)
        for (mut r, rt) in &mut razors {
            if r.mine.is_none() && r.triggered.is_none() && r.armed >= 0.5 && rt.translation.distance(b.at) < b.radius {
                r.triggered = Some(-0.75);
            }
        }
        for (e, n, t) in &npcs {
            let d = t.translation.distance(b.at);
            if (!n.is_down() || n.mode == crate::npc::Mode::Unconscious) && d < b.radius && seen(b.at + Vec3::Y * 0.3, t.translation) {
                hits.write(NpcHit { npc: e, damage: b.damage * scale(d), kind: b.kind, from: b.at });
            }
        }
        if let Ok(pt) = player.single() {
            // `m_fPlayerFullDamageRadius` 150 uu
            let d = pt.translation.distance(b.at);
            let [reach, full] = b.player.unwrap_or([b.radius, 1.5]);
            if d < reach && !stats.dead && seen(b.at + Vec3::Y * 0.3, pt.translation) {
                let k = if d <= full { 1.0 } else { (1.0 - (d - full) / (reach - full).max(0.01)).max(0.0) };
                stats.take_damage(attrs.armored(b.damage * k));
                stats.damage_flash = 1.0;
                stats.hit_from = Some(b.at);
                if stats.health <= 0.0 {
                    stats.dead = true;
                }
            }
        }
        // the flash
        commands.spawn((
            ExplosionFlash(0.0),
            PointLight { color: Color::srgb(1.0, 0.6, 0.25), intensity: 4.0e6, range: 12.0, ..default() },
            crate::fxlight::FxLight { color: Vec3::new(1.0, 0.6, 0.25), brightness: FLASH_BRIGHTNESS, radius: 12.0 },
            Transform::from_translation(b.at + Vec3::Y * 0.3),
            DespawnOnExit(GameState::InGame),
        ));
    }
    for (e, mut l, mut fx, mut f) in &mut flashes {
        f.0 += time.delta_secs();
        let k = (1.0 - f.0 / 0.4).max(0.0);
        l.intensity = 4.0e6 * k;
        fx.brightness = FLASH_BRIGHTNESS * k;
        if f.0 > 0.4 {
            commands.entity(e).despawn();
        }
    }
}

#[derive(Component)]
struct ExplosionFlash(f32);

/// Daud's mines: armed after `m_fArmingTime`, they strike whoever not of his comes into their
/// cylinder (512 uu across, 256 uu above and below) and in their sight, `m_NumShots` times
/// `m_fDelayBetweenShots` apart (`m_DamageToPawns` each; a stun mine's puts them to sleep),
/// the lightning from their `Shoot` socket to the one struck; spent, they break.
#[allow(clippy::too_many_arguments)]
fn mines(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    data: Res<crate::gamedata::Data>,
    mut q: Query<(Entity, &mut Razor, &Transform)>,
    npcs: Query<(Entity, &Npc, &Transform), (Without<Razor>, Without<crate::summon::Summoned>)>,
    mut hits: MessageWriter<NpcHit>,
    mut sfx: MessageWriter<PostEvent>,
    mut noise: MessageWriter<Noise>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    for (e, mut m, t) in &mut q {
        let Some(kind) = m.mine else { continue };
        let v = |k: &str, d: f32| data.pawn(&format!("{}.{k}", kind.key()), d);
        let at = t.translation;
        let muzzle = at + t.rotation * Vec3::Y * 0.12;
        let arming = v("m_fArmingTime", 2.0);
        if m.armed < arming {
            m.armed += dt;
            if m.armed >= arming {
                sfx.write(PostEvent::named("Snd_ArcMine_Activated_Stop", Some(at)));
                sfx.write(PostEvent::named("Snd_ArcMine_Idle", Some(at)));
            }
            continue;
        }
        m.wait = (m.wait - dt).max(0.0);
        if m.wait > 0.0 {
            continue;
        }
        // (`m_FriendlyFactions`: Daud's and his assassins')
        let friendly = |f: &str| {
            let f = f.to_ascii_lowercase();
            f.contains("fctn_daud") || f.contains("fctn_assassin") || f.contains("faction_corvo")
        };
        let target = npcs
            .iter()
            .filter(|(_, n, _)| !n.is_down() && !friendly(&n.faction))
            .filter_map(|(ne, _, nt)| {
                let chest = nt.translation + Vec3::Y * 0.35;
                let d = chest - muzzle;
                let len = d.length();
                let inside = d.with_y(0.0).length() < 5.12 && d.y.abs() < 2.56;
                (inside && len > 0.05 && ctx.cast_ray(muzzle, d / len, (len - 0.3).max(0.0), true, walls).is_none()).then_some((ne, chest, len))
            })
            .min_by(|a, b| a.2.total_cmp(&b.2));
        let Some((ne, chest, _)) = target else { continue };
        sfx.write(PostEvent::named("Snd_ArcMine_Fire", Some(muzzle)));
        let strike = if kind == MineKind::Stun { "stunmine_strike" } else { "arcmine_strike" };
        fx.write(SpawnEffect { secs: 0.8, beam_to: Some(chest), ..SpawnEffect::at(strike, muzzle) });
        fx.write(SpawnEffect { follow: Some(ne), secs: 1.0, ..SpawnEffect::at("electrified", Vec3::ZERO) });
        hits.write(NpcHit { npc: ne, damage: v("m_DamageToPawns", 50.0), kind: if kind == MineKind::Stun { HitKind::StunMine } else { HitKind::ArcMine }, from: muzzle });
        // (`m_ShootingNoiseLoudness`: 2 m)
        noise.write(Noise { corvo: false, pos: muzzle, radius: 2.0, kind: NoiseKind::Anomaly });
        m.shots += 1;
        m.wait = v("m_fDelayBetweenShots", 1.0);
        if m.shots >= v("m_NumShots", 2.0).max(1.0) as u32 {
            // spent: it breaks (`m_pDestructionEffect`; `m_DestructionNoiseLoudness` 20 m)
            sfx.write(PostEvent::named("Snd_ArcMine_Idle_Stop", Some(at)));
            sfx.write(PostEvent::named("Snd_ArcMine_Destroy_Start", Some(at)));
            fx.write(SpawnEffect { rot: t.rotation, ..SpawnEffect::at(if kind == MineKind::Stun { "stunmine_break" } else { "arcmine_break" }, at) });
            noise.write(Noise { corvo: false, pos: at, radius: 20.0, kind: NoiseKind::Threatening });
            commands.entity(e).despawn();
        }
    }
}

/// An armed mine yet to fire can be taken back (its ammunition again: `m_pGrabSound`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mine_focus(
    mut commands: Commands,
    paused: Res<crate::hud::Paused>,
    bind: Res<crate::bindings::Bindings>,
    mut focus: ResMut<crate::interact::InteractFocus>,
    held: Res<crate::props::Held>,
    mines: Query<(Entity, &Razor, &Transform)>,
    player: Query<(Entity, &Player)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    rapier: ReadRapierContext,
    (mut stats, attrs, data): (ResMut<PlayerStats>, Res<crate::gamedata::Attrs>, Res<crate::gamedata::Data>),
    mut sfx: MessageWriter<PostEvent>,
    mut msgs: ResMut<HudMessages>,
    mut log: ResMut<crate::pickuplog::PickupLog>,
) {
    if paused.0 || focus.0 || held.busy() || stats.dead {
        return;
    }
    let (Ok((pe, p)), Ok(camera), Ok(ctx)) = (player.single(), cam.single(), rapier.single()) else { return };
    if p.locked {
        return;
    }
    let eye = camera.translation();
    let dir = camera.forward().as_vec3();
    let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
    let nearest = mines
        .iter()
        .filter_map(|(e, m, t)| {
            let kind = m.mine?;
            let armed = m.armed >= data.pawn(&format!("{}.m_fArmingTime", kind.key()), 2.0);
            if !armed || m.shots > 0 {
                return None;
            }
            let to = t.translation - eye;
            let d = to.length();
            if d > 2.4 || d < 0.05 || to.dot(dir) / d < 0.94 || ctx.cast_ray(eye, to / d, (d - 0.12).max(0.0), true, filter).is_some() {
                return None;
            }
            Some((e, kind, d))
        })
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let Some((e, kind, _)) = nearest else { return };
    focus.0 = true;
    focus.1 = Some(e);
    focus.2 = format!("{} Pick up", crate::bindings::hint(crate::bindings::Act::Use));
    focus.3 = kind.name().into();
    if bind.just_pressed(crate::bindings::Act::Use) {
        let (_, added) = give_ammo(&mut stats, &attrs, kind.ammo(), 1);
        if added > 0 {
            commands.entity(e).despawn();
            focus.1 = None;
            focus.2.clear();
            focus.3.clear();
            log.add(format!("{} +1", ammo_name(kind.ammo())), crate::pickuplog::ammo_icon(kind.ammo()));
            sfx.write(PostEvent::named("Snd_ArcMine_Idle_Stop", None));
            sfx.write(PostEvent::named("Snd_UI_Ingame_Ammo_Pickup", None));
        } else {
            msgs.push("You can't carry more ammo of this type");
            sfx.write(PostEvent::named("Snd_UI_Ingame_Max_Ammo", None));
        }
    }
}

/// Chokedust's clouds: whoever is caught in one while it lasts chokes (`HitKind::Chokedust`),
/// the rats near it die (`m_fRatKillRadius`).
#[allow(clippy::too_many_arguments)]
fn choke_clouds(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    data: Res<crate::gamedata::Data>,
    mut puffs: MessageReader<ChokeDust>,
    mut clouds: Query<(Entity, &mut ChokeCloud, &Transform)>,
    npcs: Query<(Entity, &Npc, &Transform), Without<ChokeCloud>>,
    mut hits: MessageWriter<NpcHit>,
    mut sfx: MessageWriter<PostEvent>,
    mut noise: MessageWriter<Noise>,
    mut fx: MessageWriter<SpawnEffect>,
    mut rats: MessageWriter<crate::swarm::KillRats>,
) {
    for p in puffs.read() {
        if std::env::var_os("DH_GADGET_LOG").is_some() {
            let near: Vec<String> = npcs.iter().filter(|(_, n, _)| !n.is_down()).map(|(_, n, t)| (n, t.translation.distance(p.at))).filter(|(_, d)| *d < 12.0).map(|(n, d)| format!("{} {d:.1} m", n.name)).collect();
            info!("chokedust: a {:.1} m cloud at {:.1} for {:.1} s; near: {near:?}", p.radius, p.at, p.secs);
        }
        sfx.write(PostEvent::named("Snd_DLC06_ChokeDust", Some(p.at)));
        fx.write(SpawnEffect { secs: p.secs + 2.0, ..SpawnEffect::at("chokedust", p.at) });
        rats.write(crate::swarm::KillRats { at: p.at, radius: data.pawn("chokedust.explosion.m_fRatKillRadius", 200.0) * 0.01, by_player: true, source: p.at });
        noise.write(Noise { corvo: true, pos: p.at, radius: 10.0, kind: NoiseKind::Threatening });
        commands.spawn((ChokeCloud { radius: p.radius, left: p.secs, had: Vec::new() }, Transform::from_translation(p.at), DespawnOnExit(GameState::InGame)));
    }
    let dt = time.delta_secs() * tc.world_scale();
    for (e, mut c, t) in &mut clouds {
        for (ne, n, nt) in &npcs {
            if n.is_down() || c.had.contains(&ne) || nt.translation.distance(t.translation) > c.radius {
                continue;
            }
            c.had.push(ne);
            hits.write(NpcHit { npc: ne, damage: 0.0, kind: HitKind::Chokedust, from: t.translation });
        }
        c.left -= dt;
        if c.left <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// Lingering blasts (the Dead Eels' acid): every `interval` while they last, each character within
/// reach and in sight of it, and Corvo within his, is caught once (`m_DamagedActors`), its damage
/// falling off past the full reach as a blast's does; Corvo caught sees its lens effect
/// (`m_pCameraEffectTweaks`: `DLC07_CamEffect_AcidBottle_AOE`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn acid_clouds(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut clouds: Query<(Entity, &mut AcidCloud, &Transform)>,
    npcs: Query<(Entity, &Npc, &Transform), Without<AcidCloud>>,
    player: Query<&Transform, (With<Player>, Without<AcidCloud>)>,
    (mut hits, mut stats, attrs, vm): (MessageWriter<NpcHit>, ResMut<PlayerStats>, Res<crate::gamedata::Attrs>, Option<ResMut<crate::kismet::Vm>>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let seen = |from: Vec3, to: Vec3| {
        let d = to - from;
        let l = d.length();
        l < 0.3 || ctx.as_ref().is_none_or(|c| c.cast_ray(from, d / l, l - 0.2, true, walls).is_none())
    };
    let mut lens = None;
    for (e, mut c, t) in &mut clouds {
        let at = t.translation + Vec3::Y * 0.3;
        c.tick -= dt;
        if c.tick <= 0.0 {
            c.tick = c.interval;
            let [reach, corvo_reach, damage] = c.reach;
            let full = reach * 0.99;
            let fall = |d: f32, r: f32, f: f32| if d <= f { 1.0 } else { (1.0 - (d - f) / (r - f).max(0.01)).max(0.0) };
            for (ne, n, nt) in &npcs {
                let d = nt.translation.distance(at);
                if n.is_down() || c.had.contains(&ne) || d > reach || !seen(at, nt.translation) {
                    continue;
                }
                c.had.push(ne);
                hits.write(NpcHit { npc: ne, damage: damage * fall(d, reach, full), kind: HitKind::Acid, from: at });
            }
            if let Ok(pt) = player.single() {
                let d = pt.translation.distance(at);
                if !c.corvo_had && !stats.dead && d < corvo_reach && seen(at, pt.translation) {
                    c.corvo_had = true;
                    stats.take_damage(attrs.armored(damage * fall(d, corvo_reach, corvo_reach * 0.99)));
                    stats.damage_flash = 1.0;
                    stats.hit_from = Some(at);
                    if stats.health <= 0.0 {
                        stats.dead = true;
                    }
                    lens = lens.or(c.lens);
                }
            }
        }
        c.left -= dt;
        if c.left <= 0.0 {
            commands.entity(e).despawn();
        }
    }
    if let (Some(sys), Some(mut vm)) = (lens, vm) {
        vm.lens.push((crate::hudfx::GAMEPLAY_LENS + 0x500, Some((sys, false, 0.0))));
    }
}

/// An explosion's flash on the original-shader surfaces (UE3 brightness).
const FLASH_BRIGHTNESS: f32 = 6.0;

#[cfg(test)]
mod tests {
    #[test]
    fn original_daud_bolt_purse_rolls_both_inclusive_bounds() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let amounts: Vec<_> = (0..64).map(|_| super::roll_ammo_amount_with(2, 3, &mut rng)).collect();
        assert!(amounts.iter().all(|n| (2..=3).contains(n)));
        assert!(amounts.contains(&2) && amounts.contains(&3));
        assert_eq!(super::roll_ammo_amount_with(2, 2, &mut rng), 2);
        assert_eq!(super::roll_ammo_amount_with(3, 2, &mut rng), 2);
        assert_eq!(super::roll_ammo_amount_with(0, 0, &mut rng), 0);
    }
    #[test]
    fn ammunition_grants_clamp_all_types_and_preserve_existing_excess() {
        let attrs = crate::gamedata::Attrs::default();
        let mut stats = crate::gameplay::PlayerStats::default();
        for ty in 0..super::AMMO_TYPES as u8 {
            *super::ammo_mut(&mut stats, ty, true).unwrap() = 0;
            let capacity = attrs.ammo_capacity[ty as usize];
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, capacity - 1).1, capacity - 1);
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, u32::MAX).1, 1);
            assert_eq!(super::ammo_count(&stats, ty), capacity);
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, 1).1, 0);
            *super::ammo_mut(&mut stats, ty, true).unwrap() = capacity + 4;
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, 1).1, 0);
            assert_eq!(super::ammo_count(&stats, ty), capacity + 4);
        }
        assert_eq!(super::give_ammo(&mut stats, &attrs, 255, 10).1, 0);
        for id in [super::GRENADES, super::STICKY, super::RAZORS, super::FLARES, super::EXPLOSIVE, super::EXPLOSIVE_BOLTS, super::ARC_MINES, super::CHOKEDUST, super::STUN_MINES, "Bolt_Ammo_twk", "Bolts_Ammo_twk", "SleepDart_Ammo_twk", "Bullets_Store_Ammo_twk"] {
            assert!(super::ammo_type(id).is_some(), "{id}");
        }
        assert!(super::ammo_type("Twk_Upgrade_AmmoPouch2").is_none());
        assert_eq!(super::ammo_index("eDisAmmoType_FlashSmoke"), Some(10));
        assert_eq!(super::ammo_index("ArcMineStun"), Some(11));
        assert_eq!(super::ammo_index("Arrow_Sleep"), Some(3));
    }
    use super::*;

    fn grenade(owner: Option<Entity>) -> Grenade {
        Grenade { vel: Vec3::X, fuse: 0.7, fuse_total: 3.0, after_hit: 0.5, sticky: false, stuck: None, blast: BLAST, owner, throwback: false, kind: if owner.is_some() { HitKind::EnemyExplosion } else { HitKind::Explosion }, danger: BLAST[0], armed: false }
    }

    #[test]
    fn taking_and_returning_a_grenade_keeps_its_remaining_fuse() {
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let mut g = grenade(Some(owner));
        g.take();
        assert_eq!(g.fuse, 0.7);
        assert_eq!(g.owner, None);
        assert_eq!(g.kind, HitKind::Explosion);
        g.release(Vec3::NEG_Z, Vec3::new(2.0, -20.0, 0.0), 25.0, true);
        assert_eq!(g.kind, HitKind::GrenadeThrowback);
        assert_eq!(g.fuse, 0.7);
        assert!(g.vel.z < -20.0 && g.vel.y > 0.0);
        assert_eq!(g.vel.x, 2.0);
    }

    #[test]
    fn own_grenades_and_dropped_enemy_grenades_are_not_throwbacks() {
        let mut own = grenade(None);
        own.take();
        own.release(Vec3::NEG_Z, Vec3::ZERO, 25.0, true);
        assert_eq!(own.kind, HitKind::Explosion);
        let mut world = World::new();
        let mut enemy = grenade(Some(world.spawn_empty().id()));
        enemy.take();
        enemy.release(Vec3::NEG_Z, Vec3::ZERO, 25.0, false);
        assert_eq!(enemy.kind, HitKind::Explosion);
        assert_eq!(enemy.fuse, 0.7);
    }

    #[test]
    fn enemy_grenades_use_difficulty_and_clockwork_malfunction() {
        let mut app = App::new();
        app.add_message::<NpcGrenade>().add_message::<PostEvent>().add_message::<SpawnEffect>()
            .init_resource::<crate::gamedata::Data>()
            .init_resource::<PlayerStats>()
            .insert_resource(crate::settings::Settings { difficulty: 3, ..default() })
            .add_systems(Update, npc_grenades);
        app.world_mut().resource_mut::<crate::gamedata::Data>().0.charms.push(dhcook::format::CharmDef { attribute: "GrenadeExplosionDelay".into(), levels: vec![("Clockwork Malfunction".into(), String::new(), 1.0)], ..Default::default() });
        app.world_mut().resource_mut::<PlayerStats>().charms.push("Clockwork Malfunction".into());
        let owner = app.world_mut().spawn_empty().id();
        app.world_mut().write_message(NpcGrenade { owner, from: Vec3::ZERO, vel: Vec3::X, fuse: [3.0, 0.5], blast: dhcook::format::TrapBlast { damage: [10.0, 20.0, 30.0, 40.0], ..default() }, target: Vec3::X, danger: 0.0, look: default() });
        app.update();
        let mut q = app.world_mut().query::<&Grenade>();
        let g = q.single(app.world()).unwrap();
        assert_eq!(g.blast[4], 40.0);
        assert_eq!(g.fuse, 4.0);
        assert_eq!(g.fuse_total, 4.0);
        assert_eq!(g.kind, HitKind::EnemyExplosion);
    }

    #[test]
    fn saved_explosives_restore_fuses_adhesion_holding_and_npc_references() {
        let mut original = World::new();
        let owner = original.spawn_empty().id();
        let wall = original.spawn_empty().id();
        let attached = original.spawn_empty().id();
        let carried = original.spawn_empty().id();
        let fixed = original.spawn_empty().id();
        let mut enemy = grenade(Some(owner));
        enemy.sticky = true;
        enemy.stuck = Some((owner, Vec3::Y));
        let mut returned = grenade(Some(owner));
        returned.take();
        let mut sticky = grenade(None);
        sticky.kind = HitKind::StickyGrenade;
        sticky.sticky = true;
        sticky.stuck = Some((wall, Vec3::Y * 0.11));
        let pose = Transform::from_translation(Vec3::new(2.0, 3.0, 4.0)).with_rotation(Quat::from_rotation_y(0.7));
        let razor = Razor { armed: 0.3, triggered: Some(0.2), mine: Some(MineKind::Stun), shots: 1, wait: 0.4 };
        let saved = GadgetsSave::capture([(attached, &enemy, &pose, None), (carried, &returned, &pose, None), (fixed, &sticky, &pose, None)], [(&razor, &pose)], [(owner, 17)], Some(carried));
        let bytes = serde_json::to_vec(&saved).unwrap();
        let saved: GadgetsSave = serde_json::from_slice(&bytes).unwrap();

        let mut app = App::new();
        app.init_resource::<crate::props::Held>().init_resource::<Assets<Mesh>>().init_resource::<Assets<StandardMaterial>>()
            .insert_resource(GadgetRestore(Some(saved))).add_systems(Update, restore_gadgets);
        for _ in 0..10 { app.world_mut().spawn_empty(); }
        let new_owner = app.world_mut().spawn(crate::npc::FromSpawner(17)).id();
        assert_ne!(owner, new_owner);
        let stale = app.world_mut().spawn((grenade(None), Transform::IDENTITY)).id();
        app.world_mut().resource_mut::<crate::props::Held>().0 = Some(stale);
        app.update();
        assert!(app.world().get_entity(stale).is_err());
        let held = app.world().resource::<crate::props::Held>().0.unwrap();
        let mut q = app.world_mut().query::<(Entity, &Grenade, &Transform)>();
        assert_eq!(q.iter(app.world()).count(), 3);
        for (e, g, t) in q.iter(app.world()) {
            assert_eq!(g.fuse, 0.7);
            assert_eq!(g.fuse_total, 3.0);
            assert_eq!(t.translation, pose.translation);
            assert_eq!(t.rotation, pose.rotation);
            match g.kind {
                HitKind::EnemyExplosion => {
                    assert_eq!(g.owner, Some(new_owner));
                    assert_eq!(g.stuck, Some((new_owner, Vec3::Y)));
                }
                HitKind::StickyGrenade => assert_eq!(g.stuck, Some((Entity::PLACEHOLDER, Vec3::Y * 0.11))),
                HitKind::Explosion => {
                    assert_eq!(e, held);
                    assert!(g.throwback);
                    assert!(g.owner.is_none());
                }
                _ => panic!("unexpected saved damage type"),
            }
        }
        let mut razors = app.world_mut().query::<(&Razor, &Transform)>();
        let (razor, t) = razors.single(app.world()).unwrap();
        assert_eq!(razor.armed, 0.3);
        assert_eq!(razor.triggered, Some(0.2));
        assert_eq!((razor.mine, razor.shots, razor.wait), (Some(MineKind::Stun), 1, 0.4));
        assert_eq!(t.translation, pose.translation);

        app.world_mut().resource_mut::<GadgetRestore>().0 = Some(GadgetsSave::default());
        app.update();
        assert_eq!(q.iter(app.world()).count(), 0);
        assert_eq!(razors.iter(app.world()).count(), 0);
        assert!(app.world().resource::<crate::props::Held>().0.is_none());
    }
}
