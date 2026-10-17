//! The pistol's and crossbow's aim (`DishonoredWeapon_Ranged.m_fDispersion`): a shot strays
//! within a cone that closes to the weapon's least dispersion while Corvo keeps steady and
//! opens to its most as he sprints, leaves the ground or blocks
//! (`DisTweaks_WeaponRanged.m_bMaxDispersionOn*`), moving at its interpolation speed; each by
//! difficulty (`DisTweaks_WeaponRanged_Attributes`) less what the accuracy upgrades take off
//! (`Attribute_Dispersion*`). The dispersion is how far the reticle's brackets stand from the
//! middle of the HUD's stage (`Crosshair_Gun.SetDispersion`), so the shot lands within them.
//! A shot kicks the view up (`m_fCamRecoilOnFire`, unreal rotation units, less the upgrades'
//! `Attribute_RecoilReduction`) and it settles back.
//!
//! The aim assist, as the executable's `DisItemContext_AimAssistAttack` does it (0x00C14290 and
//! the functions it calls; decoded for this rewrite): with the auto aim option on for the device
//! in use (`PSI_Mouse_bAutoAim`, off by default; `PSI_Gamepad_bAutoAim`, on) and the weapon's
//! `m_bUseAimAssist_Mouse` or `_Gamepad`, each hit region of each character not down (its
//! friends passed over, `m_bDisableAssistForFriendlyPawns`) and each object of interest (a
//! tallboy's tank, whale oil tanks, river krusts, thrown whiskey bottles, rats, small props) is
//! weighed: its box is put on the screen (centre and half extent, normalised), and the crosshair
//! must fall within that half extent plus its window's `m_fAimAssist_MaxSize_Horiz` / `_Vert`
//! (pixels of 1280 x 720, scaled by the strength option and, past `m_fDistanceToReduce`, by
//! `m_fAimAssistWindowScale`); it must be within the reach across the ground
//! (`m_fMaxAimAssistDistance`, the group's own if set, times `Attribute_Range` at least 1) and
//! seen from the eye. The one scoring least, (`m_fAimAssistAngleWeight` x its angle off the view
//! over `m_fMaxAimAssistAngle` + `m_fAimAssistDistWeight` x its distance over the reach, each
//! clamped to 1) x its `m_fPriorityWeightScale`, is the target: a shot goes for its point
//! (`AimPoint`, 0x00C14640, before the dispersion), and the HUD's crosshair slides onto it at
//! `m_fCrosshairInterpolationSpeed` (0x00BB22F3), back to the middle without one. Zoomed, the
//! groups for the zoomed lens are used (empty for Corvo's weapons).

use crate::gameplay::PlayerStats;
use crate::gamedata::Data;
use dhcook::format::{AimAssist, AssistGroup, AssistInfo};
use crate::player::{Player, PlayerCamera};
use crate::powers::{Power, Powers};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use std::f32::consts::TAU;

pub struct AimPlugin;

impl Plugin for AimPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Aim>()
            .add_systems(OnEnter(GameState::InGame), |mut a: ResMut<Aim>| *a = Aim::default())
            .add_systems(Update, disperse.run_if(in_state(GameState::InGame)))
            .add_systems(Update, assist.before(crate::gadgets::grenade_focus).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weapon {
    Pistol,
    Crossbow,
}

impl Weapon {
    pub fn of(p: Power) -> Option<Weapon> {
        match p {
            Power::Pistol | Power::ExplosiveBullet => Some(Weapon::Pistol),
            Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt | Power::ExplosiveBolt => Some(Weapon::Crossbow),
            _ => None,
        }
    }
    fn key(self) -> &'static str {
        match self {
            Weapon::Pistol => "pistol",
            Weapon::Crossbow => "crossbow",
        }
    }
    /// its upgrades (`Twk_Upgrade_Pistol_Accuracy1`, `Twk_Upgrade_Crossbow_Accuracy`...)
    fn upgrades(self) -> &'static str {
        match self {
            Weapon::Pistol => "Twk_Upgrade_Pistol",
            Weapon::Crossbow => "Twk_Upgrade_Crossbow",
        }
    }
}

/// The aim of the weapon in hand.
#[derive(Resource, Default)]
pub struct Aim {
    pub weapon: Option<Weapon>,
    /// the dispersion now, its least and its most (the HUD stage's units)
    pub dispersion: f32,
    pub min: f32,
    pub max: f32,
    /// the half-angle (radians) of the cone the dispersion makes on the screen
    pub cone: f32,
    /// the view's kick (radians) and the seconds since the shot
    kick: f32,
    kick_t: f32,
    /// what the aim assist holds the weapon to
    pub assist: Option<Assisted>,
    /// where the HUD's crosshair stands (normalised from the middle, y up), sliding to the
    /// target's place and back
    pub crosshair: Vec2,
}

/// What the aim assist holds the weapon to (`DisCachedAimAssistPosResult`): the thing, the
/// point a shot goes for, its place on the screen (normalised, y up), and whether the
/// crosshair is outside the thing's safe part (`m_bWillTrack`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Assisted {
    pub entity: Entity,
    pub point: Vec3,
    pub ndc: Vec2,
    pub track: bool,
}

impl Aim {
    /// The way a shot goes before it strays: at what the aim assist holds (`AimPoint`), else the
    /// view's.
    pub fn aimed(&self, eye: Vec3, dir: Vec3) -> Vec3 {
        let way = self.assist.map_or(dir, |a| (a.point - eye).normalize_or(dir));
        if std::env::var("DH_AIM_LOG").is_ok() {
            info!("aim assist: shot {:.2} deg off the view, at {:?}", way.angle_between(dir).to_degrees(), self.assist.map(|a| a.entity));
        }
        way
    }
    /// A shot's way: somewhere within the cone about the view's.
    pub fn scatter(&self, dir: Vec3) -> Vec3 {
        if self.cone <= 0.0 {
            return dir;
        }
        let r = self.cone * rand::random::<f32>().sqrt();
        let a = rand::random::<f32>() * TAU;
        let right = dir.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = right.cross(dir).normalize_or(Vec3::Y);
        (dir + (right * a.cos() + up * a.sin()) * r.tan()).normalize_or(dir)
    }
    /// A shot fired: the view kicks.
    pub fn fire(&mut self, data: &Data, stats: &PlayerStats, p: Power) {
        let Some(w) = Weapon::of(p) else { return };
        let units = data.pawn(&format!("{}.m_fCamRecoilOnFire", w.key()), 100.0) - upgrade_sum(data, stats, w, "RecoilReduction");
        self.kick = units.max(0.0) * TAU / 65536.0;
        self.kick_t = 0.0;
    }
    /// The kick's pitch now (radians): up in 50 ms, then settling.
    pub fn view_kick(&self) -> f32 {
        if self.kick <= 0.0 {
            return 0.0;
        }
        const UP: f32 = 0.05;
        if self.kick_t < UP {
            self.kick * self.kick_t / UP
        } else {
            self.kick * (-(self.kick_t - UP) * 9.0).exp()
        }
    }
}

/// A character's hit regions as boxes about its middle (its capsule's, 1.88 m tall), split as the
/// shots tell them apart (`powers`: the head above 0.58 m, the legs below -0.35 m): centre (its
/// right +x, up +y) and half extents, by `eDisHitRegion` (none, head, torso, left leg, right
/// leg, left arm, right arm). The original's are its physics bodies' boxes.
const REGIONS: [(Vec3, Vec3); 7] = [
    (Vec3::ZERO, Vec3::ZERO),
    (Vec3::new(0.0, 0.76, 0.0), Vec3::new(0.12, 0.18, 0.12)),
    (Vec3::new(0.0, 0.115, 0.0), Vec3::new(0.2, 0.465, 0.14)),
    (Vec3::new(-0.1, -0.645, 0.0), Vec3::new(0.09, 0.295, 0.09)),
    (Vec3::new(0.1, -0.645, 0.0), Vec3::new(0.09, 0.295, 0.09)),
    (Vec3::new(-0.27, 0.15, 0.0), Vec3::new(0.06, 0.32, 0.06)),
    (Vec3::new(0.27, 0.15, 0.0), Vec3::new(0.06, 0.32, 0.06)),
];

/// A box about a thing (local centre, half extents) as the world's axis-aligned one.
fn world_box(g: &GlobalTransform, c: Vec3, h: Vec3) -> (Vec3, Vec3) {
    let m = g.affine().matrix3;
    (g.transform_point(c), Vec3::from(m.x_axis.abs()) * h.x + Vec3::from(m.y_axis.abs()) * h.y + Vec3::from(m.z_axis.abs()) * h.z)
}

/// One thing weighed by the aim assist (0x00C10650): its box (world, axis aligned) put on the
/// screen (0x00C02FE0: its centre, and its corners' farthest from it clamped to the screen,
/// normalised), the crosshair within its window, within the reach across the ground: its score,
/// its place on the screen, and whether the crosshair is within its safe part.
#[allow(clippy::too_many_arguments)]
pub fn assess(clip_from_world: Mat4, eye: Vec3, fwd: Vec3, center: Vec3, half: Vec3, info: &AssistInfo, aa: &AimAssist, reach: f32, strength: f32) -> Option<(f32, Vec2, bool)> {
    if !info.on {
        return None;
    }
    let project = |p: Vec3| -> Option<Vec2> {
        let c = clip_from_world * p.extend(1.0);
        (c.w > 1e-4).then(|| c.truncate().truncate() / c.w)
    };
    let at = project(center)?;
    let mut ext = Vec2::ZERO;
    for k in 0..8 {
        let s = Vec3::new(if k & 1 == 0 { -1.0 } else { 1.0 }, if k & 2 == 0 { -1.0 } else { 1.0 }, if k & 4 == 0 { -1.0 } else { 1.0 });
        let c = project(center + half * s)?.clamp(Vec2::NEG_ONE, Vec2::ONE);
        ext = ext.max((c - at).abs());
    }
    let d = (center - eye).xz().length();
    if d > reach {
        return None;
    }
    // (the window narrowed far off: the first distance passed)
    let scale = aa.reduce.iter().find(|r| d > r[0]).map_or(1.0, |r| r[1]);
    let window = ext + Vec2::new(info.max[0] / 1280.0, info.max[1] / 720.0) * strength * scale;
    if at.x.abs() >= window.x || at.y.abs() >= window.y {
        return None;
    }
    let angle = fwd.dot((center - eye).normalize_or_zero()).clamp(-1.0, 1.0).acos().to_degrees();
    let score = (aa.angle_weight * (angle / aa.max_angle.max(1e-3)).clamp(0.0, 1.0) + aa.dist_weight * (d / reach.max(1e-3)).clamp(0.0, 1.0)) * info.priority;
    let safe = ext.x * info.safe[0] > at.x.abs() && ext.y * info.safe[1] > at.y.abs();
    Some((score, at, safe))
}

/// The object of interest a thing is (`m_ObjectsOfInterest`, 0x00C077A0): the first whose tweak
/// its own falls back on, else (without a tweak to match) whose class it is.
fn interest<'a>(group: &'a AssistGroup, tweaks: &[String], class: &str) -> Option<&'a AssistInfo> {
    group
        .objects
        .iter()
        .find(|o| if !o.tweak.is_empty() && !tweaks.is_empty() { tweaks.contains(&o.tweak) } else { !o.class.is_empty() && o.class == class })
        .map(|o| &o.info)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn assist(
    time: Res<Time>,
    mut aim: ResMut<Aim>,
    (data, settings, stats, powers, zoom, level): (Res<Data>, Res<crate::settings::Settings>, Res<PlayerStats>, Res<Powers>, Res<crate::zoom::Zoom>, Option<Res<crate::level::LevelInfo>>),
    cam: Query<(&Projection, &GlobalTransform), With<PlayerCamera>>,
    player: Query<Entity, With<Player>>,
    npcs: Query<(Entity, &GlobalTransform, &crate::npc::Npc), Without<crate::possession::Possessed>>,
    (parts, props, krusts, bottles, rats): (
        Query<(Entity, &GlobalTransform, &crate::npcparts::NpcAttached)>,
        Query<(Entity, &GlobalTransform, &crate::props::Prop)>,
        Query<(Entity, &GlobalTransform, &crate::krust::Krust)>,
        Query<(Entity, &GlobalTransform), With<crate::whiskey::Bottle>>,
        Query<(Entity, &GlobalTransform), (With<crate::swarm::Rat>, Without<crate::swarm::DeadRat>)>,
    ),
    bodies: Query<(&GlobalTransform, &bevy_rapier3d::prelude::Collider)>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
) {
    use bevy_rapier3d::prelude::{CollisionGroups, Group, QueryFilter};
    let found = (|| -> Option<Assisted> {
        let w = Weapon::of(powers.selected)?;
        let aa = data.0.aim_assist.iter().find(|a| a.weapon == w.key())?;
        // (the device in use: the pad's look, `m_bUsingGamepadForLook`)
        let pad = crate::pad::using();
        let (on, strength) = if pad { (aa.pad && settings.pad_auto_aim, settings.pad_auto_aim_strength) } else { (aa.mouse && settings.mouse_auto_aim, settings.mouse_auto_aim_strength) };
        if !on || stats.dead {
            return None;
        }
        let group = &aa.groups[usize::from(pad) * 2 + usize::from(zoom.level > 0)];
        let level = level.as_ref()?;
        let (projection, cg) = cam.single().ok()?;
        let clip_from_world = projection.get_clip_from_view() * Mat4::from(cg.affine().inverse());
        let (eye, fwd) = (cg.translation(), cg.forward().as_vec3());
        let reach = group.max_dist.unwrap_or(aa.max_dist) * upgrade_sum(&data, &stats, w, "Range").max(1.0);
        let strength = (strength / 100.0).clamp(0.0, 1.0);
        // what is weighed: the thing, those its line of sight passes through, its box, its window
        let mut weigh: Vec<(Entity, [Option<Entity>; 2], Vec3, Vec3, &AssistInfo)> = Vec::new();
        let friendly = |n: &crate::npc::Npc| level.scene.allies.get(&n.faction).is_some_and(|a| a.iter().any(|f| dhcook::format::is_player_faction(f)));
        for (e, g, n) in &npcs {
            if n.is_down() {
                continue;
            }
            // (neutral, his friend, his enemy)
            let stance = if n.hostile() { 2 } else if friendly(n) { 1 } else { 0 };
            if (group.skip_neutral && stance == 0) || (group.skip_friendly && stance == 1) {
                continue;
            }
            for (r, info) in group.regions.iter().enumerate().skip(1) {
                let (c, h) = world_box(g, REGIONS[r].0, REGIONS[r].1);
                weigh.push((e, [Some(e), None], c, h, info));
            }
        }
        for (e, g, a) in &parts {
            let Some(def) = level.scene.npc_types.get(a.npc_type as usize).and_then(|t| t.attachments.get(a.index)) else { continue };
            if a.health <= 0.0 {
                continue;
            }
            if let Some(info) = interest(group, &def.tweaks, "") {
                weigh.push((e, [Some(e), Some(a.npc)], g.translation(), Vec3::splat(0.2), info));
            }
        }
        for (e, g, p) in &props {
            let Some(m) = level.scene.movables.get(p.index) else { continue };
            if m.pool.is_some() && !p.is_live() {
                continue;
            }
            let class = if m.tank.is_some() { "DisWhaleOilBattery" } else { "DishonoredMovable" };
            let Some(info) = interest(group, &m.tweaks, class) else { continue };
            // (its body's box)
            let (body, _) = p.body();
            let (c, h) = body
                .and_then(|b| bodies.get(b).ok())
                .map(|(bg, col)| {
                    let a = col.raw.compute_local_aabb();
                    let (lo, hi) = (Vec3::new(a.mins.x, a.mins.y, a.mins.z), Vec3::new(a.maxs.x, a.maxs.y, a.maxs.z));
                    world_box(bg, (lo + hi) * 0.5, (hi - lo) * 0.5)
                })
                .unwrap_or((g.translation(), Vec3::splat(0.15)));
            weigh.push((e, [Some(e), body], c, h, info));
        }
        for (e, g, k) in &krusts {
            if !k.alive() {
                continue;
            }
            if let Some(info) = interest(group, &[], "DisRiverKrust") {
                weigh.push((e, [Some(e), Some(k.collider())], g.translation(), Vec3::splat(0.3), info));
            }
        }
        for (e, g) in &bottles {
            if let Some(info) = interest(group, &[], "DisProjectile_Whiskey") {
                weigh.push((e, [Some(e), None], g.translation(), Vec3::splat(0.08), info));
            }
        }
        for (e, g) in &rats {
            if let Some(info) = interest(group, &[], "DisGameCrowdAgentSkeletalRat") {
                weigh.push((e, [Some(e), None], g.translation() + Vec3::Y * 0.05, Vec3::new(0.08, 0.06, 0.08), info));
            }
        }
        if std::env::var("DH_AIM_LOG").is_ok_and(|v| v == "2") {
            let held = weigh.iter().filter(|(_, _, c, h, info)| assess(clip_from_world, eye, fwd, *c, *h, info, aa, reach, strength).is_some()).count();
            info!("aim assist: {} weighed, {held} in their windows", weigh.len());
        }
        let ctx = rapier.single().ok()?;
        let me = player.single().ok();
        let mut best: Option<(f32, Assisted)> = None;
        for (e, pass, c, h, info) in weigh {
            let Some((score, ndc, safe)) = assess(clip_from_world, eye, fwd, c, h, info, aa, reach, strength) else { continue };
            if best.is_some_and(|b| b.0 <= score) {
                continue;
            }
            // seen from the eye: nothing but the thing itself between (0x00BE1560)
            let to = c - eye;
            let dist = to.length();
            let counts = |x: Entity| Some(x) != me && !pass.contains(&Some(x));
            let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, crate::level::GROUP_WORLD | crate::level::GROUP_NPC | crate::level::GROUP_PROP)).predicate(&counts);
            if let Some((hit, toi)) = ctx.cast_ray(eye, to / dist.max(1e-4), (dist - 0.05).max(0.0), true, filter) {
                if std::env::var("DH_AIM_LOG").is_ok_and(|v| v == "2") {
                    info!("aim assist: {e:?} behind {hit:?} at {toi:.2} of {dist:.2}");
                }
                continue;
            }
            best = Some((score, Assisted { entity: e, point: c, ndc, track: !safe }));
        }
        best.map(|b| b.1)
    })();
    if std::env::var("DH_AIM_LOG").is_ok() && found.map(|f| f.entity) != aim.assist.map(|f| f.entity) {
        info!("aim assist: {:?}", found);
    }
    aim.assist = found;
    // the crosshair slides to it (`m_fCrosshairInterpolationSpeed` per second, of the way left)
    let target = found.map_or(Vec2::ZERO, |a| a.ndc);
    let k = (data.pawn("hud.m_fCrosshairInterpolationSpeed", 4.5) * time.delta_secs()).clamp(0.0, 1.0);
    let at = aim.crosshair;
    aim.crosshair = at + (target - at) * k;
}

/// What the weapon's upgrades owned add to one of its attributes.
pub(crate) fn upgrade_sum(data: &Data, stats: &PlayerStats, w: Weapon, attr: &str) -> f32 {
    data.0.upgrades.iter().filter(|u| u.id.starts_with(w.upgrades()) && stats.upgrades.contains(&u.id)).flat_map(|u| u.attributes.iter()).filter(|(n, _)| n == attr).map(|(_, v)| v).sum()
}

#[allow(clippy::too_many_arguments)]
fn disperse(
    time: Res<Time>,
    mut aim: ResMut<Aim>,
    (data, settings, stats, powers): (Res<Data>, Res<crate::settings::Settings>, Res<PlayerStats>, Res<Powers>),
    player: Query<&Player>,
    sword: Query<&crate::combat::Sword>,
    cam: Query<&Projection, With<PlayerCamera>>,
    window: Query<&Window, With<PrimaryWindow>>,
) {
    let dt = time.delta_secs();
    aim.kick_t += dt;
    let Some(w) = Weapon::of(powers.selected) else {
        aim.weapon = None;
        aim.cone = 0.0;
        return;
    };
    let d = settings.difficulty.min(3);
    let base = |n: &str| data.pawn(&format!("{}.{n}.{d}", w.key()), 0.0) + upgrade_sum(&data, &stats, w, n);
    let (min, max, speed) = (base("DispersionMin").max(0.0), base("DispersionMax").max(0.0), base("DispersionInterpolationSpeed").max(1.0));
    let flag = |n: &str| data.pawn(&format!("{}.m_bMaxDispersionOn{n}", w.key()), 0.0) > 0.0;
    let wide = player.single().is_ok_and(|p| (p.sprinting && flag("Sprint")) || (!p.grounded && !p.noclip && flag("Jump")))
        || sword.single().is_ok_and(|s| (s.blocking && flag("Block")) || (s.swing > 0.0 && flag("Melee")));
    let target = if wide { max } else { min };
    // (a weapon just taken up: steady)
    if aim.weapon != Some(w) {
        aim.weapon = Some(w);
        aim.dispersion = min;
    }
    let step = speed * dt;
    aim.dispersion = if aim.dispersion < target { (aim.dispersion + step).min(target) } else { (aim.dispersion - step).max(target) };
    aim.min = min;
    aim.max = max.max(min);
    // on the screen: the stage's units scaled to the window, against the view's half-height
    let (Ok(win), Ok(Projection::Perspective(pp))) = (window.single(), cam.single()) else { return };
    let s = (win.width() / 1280.0).min(win.height() / 720.0).max(0.01);
    aim.cone = (aim.dispersion * s / (win.height() * 0.5).max(1.0) * (pp.fov * 0.5).tan()).atan();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pistol() -> AimAssist {
        AimAssist { weapon: "pistol".into(), mouse: true, pad: true, max_angle: 45.0, max_dist: 40.0, angle_weight: 10000.0, dist_weight: 1.0, reduce: vec![[20.0, 0.5]], blend: 0.1, groups: Default::default() }
    }
    const HEAD: AssistInfo = AssistInfo { on: true, priority: 0.13, safe: [0.5, 0.7], max: [160.0, 40.0] };
    const TORSO: AssistInfo = AssistInfo { on: true, priority: 0.15, safe: [0.7, 0.7], max: [150.0, 0.0] };

    /// A square view down -z from the origin, 1 rad tall.
    fn view() -> Mat4 {
        Mat4::perspective_rh(1.0, 1.0, 0.1, 1000.0) * Mat4::look_to_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y)
    }

    #[test]
    fn the_window_reaches_past_the_region_by_its_margin() {
        let (m, aa, half) = (view(), pistol(), Vec3::new(0.12, 0.18, 0.12));
        // a head 10 m off, the crosshair just past it but within its 160 px: held
        let near = assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.44, 0.0, -10.0), half, &HEAD, &aa, 40.0, 1.0);
        let (_, at, safe) = near.expect("within the window");
        assert!(at.x > 0.05 && !safe, "off its own box, outside the safe part");
        // the strength at none leaves only its own box
        assert!(assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.44, 0.0, -10.0), half, &HEAD, &aa, 40.0, 0.0).is_none());
        // farther across: missed
        assert!(assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(1.0, 0.0, -10.0), half, &HEAD, &aa, 40.0, 1.0).is_none());
        // under the crosshair: within the safe part
        assert!(assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.0, 0.0, -10.0), half, &HEAD, &aa, 40.0, 1.0).unwrap().2);
        // behind, or beyond the reach across the ground: nothing
        assert!(assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.0, 0.0, 10.0), half, &HEAD, &aa, 40.0, 1.0).is_none());
        assert!(assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.0, 0.0, -41.0), half, &HEAD, &aa, 40.0, 1.0).is_none());
    }

    #[test]
    fn far_windows_narrow_and_the_nearer_region_to_the_view_scores_less() {
        let (m, aa, half) = (view(), pistol(), Vec3::new(0.12, 0.18, 0.12));
        // past 20 m the window is halved: a head this far across is missed, held without
        let far = Vec3::new(1.5, 0.0, -30.0);
        assert!(assess(m, Vec3::ZERO, Vec3::NEG_Z, far, half, &HEAD, &aa, 40.0, 1.0).is_none());
        assert!(assess(m, Vec3::ZERO, Vec3::NEG_Z, far, half, &HEAD, &AimAssist { reduce: Vec::new(), ..pistol() }, 40.0, 1.0).is_some());
        // a head and a torso alike placed: the head's weight is less
        let h = assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.1, 0.0, -10.0), half, &HEAD, &aa, 40.0, 1.0).unwrap().0;
        let t = assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.1, 0.0, -10.0), Vec3::new(0.2, 0.46, 0.14), &TORSO, &aa, 40.0, 1.0).unwrap().0;
        assert!(h < t);
        // nearer the view's line scores less than farther off it
        let off = assess(m, Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.4, 0.0, -10.0), half, &HEAD, &aa, 40.0, 1.0).unwrap().0;
        assert!(h < off);
    }
}
