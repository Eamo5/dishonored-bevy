//! Swimming in the levels' water volumes (`DishonoredWaterVolume`): Corvo swims at and under
//! the surface (`StatePlayerMasterSwim`: slow without strokes, faster sprinting), holds his
//! breath (`m_fApneaMaxDuration`, regained over `m_fApneaRecoveryDuration`) and drowns after;
//! going in splashes by speed (the volume's `m_WaterEntrySettings`), the current carries him,
//! and under water the volume's colour grade, fog and sound loop take over.

use crate::audio::{event_id, PostEvent};
use crate::bindings::{Act, Bindings};
use crate::gameplay::{HudMessages, PlayerStats};
use crate::level::LevelInfo;
use crate::player::{Player, PlayerCamera, CROUCH_HALF, RADIUS, STAND_HALF};
use crate::GameState;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use dhcook::format::Water;

pub struct SwimPlugin;

impl Plugin for SwimPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Swim>()
            .add_systems(OnEnter(GameState::InGame), build_waters.after(crate::level::LevelSpawnSet))
            .add_systems(Update, stream_waters.before(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (splashes, breathe, underwater_look).chain().after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// How far under the surface a swimmer's body floats (its centre), and the eye above it.
pub const FLOAT_DEPTH: f32 = 0.12;
pub const SWIM_EYE: f32 = 0.3;

/// A water volume: its convex pieces and the water it holds.
struct WaterVol {
    hulls: Vec<Collider>,
    min: Vec3,
    max: Vec3,
    water: Water,
    /// the streamed sublevel it comes with, and whether that is out (no water then: the
    /// Tower's waterlock fills once the boat has risen)
    level: Option<String>,
    out: bool,
}

impl WaterVol {
    fn contains(&self, p: Vec3) -> bool {
        !self.out && p.cmpge(self.min).all() && p.cmple(self.max).all() && self.hulls.iter().any(|h| h.contains_point(Vec3::ZERO, Quat::IDENTITY, p))
    }

    /// The water's surface above (or at) `p`.
    fn surface(&self, p: Vec3) -> f32 {
        let from = Vec3::new(p.x, self.max.y + 1.0, p.z);
        let d = self.hulls.iter().filter_map(|h| h.cast_ray(Vec3::ZERO, Quat::IDENTITY, from, Vec3::NEG_Y, self.max.y - self.min.y + 2.0, true)).fold(f32::INFINITY, f32::min);
        if d.is_finite() {
            from.y - d
        } else {
            self.max.y
        }
    }
}

#[derive(Resource, Default)]
pub struct Waters(Vec<WaterVol>);

impl Waters {
    /// The water at a point (its index) and the surface there.
    pub fn at(&self, p: Vec3) -> Option<(usize, f32)> {
        self.0.iter().position(|w| w.contains(p)).map(|i| (i, self.0[i].surface(p)))
    }

    /// A water's fog layer (seen from under its surface).
    pub fn layer(&self, i: usize) -> Option<&dhcook::format::FogLayer> {
        self.0.get(i).and_then(|w| w.water.fog_layer.as_ref())
    }
}

/// The player in the water.
#[derive(Resource)]
pub struct Swim {
    /// the water the body is in, and its surface
    pub water: Option<usize>,
    pub surface: f32,
    /// the eyes are under water (in which water)
    pub under: Option<usize>,
    /// breath held (seconds left of `max`)
    pub breath: f32,
    pub max: f32,
    drown_t: f32,
    /// the level's fog, kept while the water's replaces it
    level_fog: Option<DistanceFog>,
    was_in: Option<usize>,
}

impl Default for Swim {
    fn default() -> Self {
        Swim { water: None, surface: 0.0, under: None, breath: 30.0, max: 30.0, drown_t: 0.0, level_fog: None, was_in: None }
    }
}

impl Swim {
    pub fn swimming(&self) -> bool {
        self.water.is_some()
    }
}

fn build_waters(mut commands: Commands, level: Option<Res<LevelInfo>>, data: Res<crate::gamedata::Data>) {
    let mut out = Vec::new();
    if let Some(level) = level {
        for (vi, v) in level.scene.volumes.iter().enumerate() {
            let Some(w) = &v.water else { continue };
            let pts: Vec<Vec3> = v.hulls.iter().flatten().map(|p| Vec3::from(*p)).collect();
            let hulls: Vec<Collider> = v.hulls.iter().filter_map(|h| Collider::convex_hull(&h.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>())).collect();
            if hulls.is_empty() {
                continue;
            }
            let min = pts.iter().fold(Vec3::INFINITY, |a, b| a.min(*b));
            let max = pts.iter().fold(Vec3::NEG_INFINITY, |a, b| a.max(*b));
            let level = crate::campaign::volume_level(&level.scene, vi as u32).map(str::to_string);
            out.push(WaterVol { hulls, min, max, water: w.clone(), level, out: false });
        }
    }
    if !out.is_empty() {
        info!("{} water volumes", out.len());
    }
    commands.insert_resource(Waters(out));
    let max = data.pawn("m_fApneaMaxDuration", 30.0);
    commands.insert_resource(Swim { breath: max, max, ..default() });
}

/// Waters of streamed sublevels are there while their level is.
fn stream_waters(vm: Option<Res<crate::kismet::Vm>>, waters: Option<ResMut<Waters>>) {
    let (Some(vm), Some(mut waters)) = (vm, waters) else { return };
    for w in waters.bypass_change_detection().0.iter_mut() {
        if let Some(l) = &w.level {
            w.out = vm.level_out(l);
        }
    }
}

/// Where the player is in the water (before moving).
pub fn water_state(waters: Option<Res<Waters>>, mut swim: ResMut<Swim>, player: Query<(&Transform, &Player)>, cam: Query<&GlobalTransform, With<PlayerCamera>>) {
    let (Some(waters), Ok((t, p))) = (waters, player.single()) else { return };
    if p.noclip {
        swim.water = None;
        swim.under = None;
        return;
    }
    // the body swims once its middle is in: wading is walking
    match waters.at(t.translation) {
        Some((i, s)) => {
            swim.water = Some(i);
            swim.surface = s;
        }
        None => swim.water = None,
    }
    let eye = cam.single().map(|g| g.translation()).unwrap_or(t.translation);
    swim.under = waters.0.iter().position(|w| w.contains(eye));
}

/// Swimming: along the look direction under water (jump rises, crouch dives), level at the
/// surface; idle swimmers float up. Called by the player controller while in the water.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn swim_move(
    p: &mut Player,
    t: &Transform,
    kcc: &mut KinematicCharacterController,
    col: &mut Collider,
    keys: &ButtonInput<KeyCode>,
    bind: &Bindings,
    forward: f32,
    attrs: &crate::gamedata::Attrs,
    swim: &Swim,
    waters: &Waters,
    dt: f32,
    host: Option<crate::possession::HostBody>,
) {
    if p.crouched && host.is_none() {
        p.crouched = false;
        *col = Collider::capsule_y(STAND_HALF, RADIUS);
    }
    let look = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch) * Vec3::NEG_Z;
    let right = Quat::from_rotation_y(p.yaw) * Vec3::X;
    let mut wish = Vec3::ZERO;
    if keys.pressed(bind.key(Act::Forward)) {
        wish += look;
    }
    if keys.pressed(bind.key(Act::Back)) {
        wish -= look;
    }
    if keys.pressed(bind.key(Act::Right)) {
        wish += right;
    }
    if keys.pressed(bind.key(Act::Left)) {
        wish -= right;
    }
    wish += look * forward;
    // a creature floats with its back at the surface
    let float_y = swim.surface - host.map(|b| (b.half + b.radius) * 0.5).unwrap_or(FLOAT_DEPTH);
    let at_surface = t.translation.y > float_y - 0.3;
    // at the surface, swimming on looks ahead (`m_bConstantSpeedAtSurface`): only a steep
    // look or crouch dives
    if at_surface && wish.y < 0.0 && wish.y > -0.6 {
        wish.y = 0.0;
    }
    if keys.pressed(bind.key(Act::Jump)) {
        wish.y += 1.0;
    }
    if keys.pressed(bind.key(Act::Crouch)) {
        wish.y -= 1.0;
    }
    let mut wish = wish.normalize_or_zero();
    // a creature that can't swim (`m_bCanSwimWhilePossessed`) keeps to the surface
    if host.is_some_and(|b| !b.swim) {
        wish.y = wish.y.max(0.0);
    }
    p.sprinting = keys.pressed(bind.key(Act::Sprint)) && wish != Vec3::ZERO && host.is_none();
    let speed = match host {
        Some(b) => b.water,
        None if p.sprinting => attrs.swim_fast,
        None => attrs.swim,
    };
    let current = swim.water.and_then(|i| waters.0.get(i)).map(|w| Vec3::from(w.water.current)).unwrap_or(Vec3::ZERO);
    let mut target = wish * speed + current;
    // the water holds a still swimmer up
    if wish.y.abs() < 0.01 && t.translation.y < float_y {
        target.y += 0.7;
    }
    p.velocity += (target - p.velocity) * (2.5 * dt).min(1.0);
    // never out of the water but by climbing
    let room = float_y - t.translation.y;
    if room < 0.0 {
        p.velocity.y = p.velocity.y.min(room * 4.0);
    } else if p.velocity.y * dt > room {
        p.velocity.y = room / dt.max(1e-4);
    }
    kcc.snap_to_ground = None;
    kcc.translation = Some(p.velocity * dt);
    p.grounded = false;
    p.air_time = 0.0;
    p.fall_speed = 0.0;
    let _ = CROUCH_HALF;
}

/// Splashes going in and climbing out.
fn splashes(mut swim: ResMut<Swim>, waters: Option<Res<Waters>>, player: Query<(&Transform, &Player)>, mut audio: MessageWriter<PostEvent>, mut fx: MessageWriter<crate::particles::SpawnEffect>) {
    let (Some(waters), Ok((t, p))) = (waters, player.single()) else { return };
    let now = swim.water;
    if now != swim.was_in {
        let at = Vec3::new(t.translation.x, swim.surface, t.translation.z);
        match (swim.was_in, now) {
            (None, Some(i)) => {
                let speed = p.velocity.length().max(p.fall_speed);
                if let Some(w) = waters.0.get(i) {
                    if let Some((_, sound, system)) = w.water.entry.iter().rev().find(|e| speed >= e.0).or(w.water.entry.first()) {
                        if !sound.is_empty() {
                            audio.write(PostEvent::named(sound, Some(at)));
                        }
                        if let Some(s) = system {
                            fx.write(crate::particles::SpawnEffect { system: Some(*s), ..crate::particles::SpawnEffect::at("", at) });
                        }
                    }
                }
            }
            (Some(i), None) => {
                if let Some(w) = waters.0.get(i).filter(|w| !w.water.exit.is_empty()) {
                    audio.write(PostEvent::named(&w.water.exit, Some(t.translation)));
                }
            }
            _ => {}
        }
        swim.was_in = now;
    }
}

/// Holding the breath under water; drowning once it's gone.
#[allow(clippy::too_many_arguments)]
fn breathe(
    time: Res<Time>,
    mut swim: ResMut<Swim>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    attrs: Res<crate::gamedata::Attrs>,
    data: Res<crate::gamedata::Data>,
    possession: Res<crate::possession::Possession>,
) {
    let dt = time.delta_secs();
    if stats.dead {
        return;
    }
    // a fish breathes the water
    let gills = possession.body.is_some_and(|b| b.fish);
    if swim.under.is_some() && !gills {
        swim.breath = (swim.breath - dt).max(0.0);
        if swim.breath <= 0.0 {
            // the original's drowning attributes (`DrowningDamageStep` / `-Amount`), a tenth
            // of Corvo's health per unit
            swim.drown_t += dt;
            let step = attrs.drown_step.max(0.1);
            while swim.drown_t >= step {
                swim.drown_t -= step;
                stats.health = (stats.health - attrs.drown_damage * 0.1 * stats.max_health).max(0.0);
                stats.damage_flash = 1.0;
                if stats.health <= 0.0 {
                    stats.dead = true;
                    msgs.push("You drowned");
                    break;
                }
            }
        }
    } else {
        let recover = data.pawn("m_fApneaRecoveryDuration", 5.0).max(0.1);
        swim.breath = (swim.breath + swim.max / recover * dt).min(swim.max);
        swim.drown_t = 0.0;
    }
}

/// Under water: the water's fog, colour grade and sound loop.
fn underwater_look(
    mut swim: ResMut<Swim>,
    waters: Option<Res<Waters>>,
    mut cams: Query<&mut DistanceFog, With<PlayerCamera>>,
    mut post: ResMut<crate::postfx::PostOverride>,
    mut audio: MessageWriter<PostEvent>,
    mut stop: MessageWriter<crate::audio::StopEvent>,
    mut last: Local<Option<usize>>,
) {
    let Some(waters) = waters else { return };
    let now = swim.under;
    if now == *last {
        return;
    }
    let old = last.and_then(|i| waters.0.get(i));
    let new = now.and_then(|i| waters.0.get(i));
    if let Some(w) = old {
        if !w.water.underwater.0.is_empty() {
            stop.write(crate::audio::StopEvent(event_id(&w.water.underwater.0)));
        }
        if !w.water.underwater.1.is_empty() {
            audio.write(PostEvent::named(&w.water.underwater.1, None));
        }
    }
    if let Ok(mut fog) = cams.single_mut() {
        match new.and_then(|w| w.water.fog.as_ref()) {
            Some(f) => {
                if swim.level_fog.is_none() {
                    swim.level_fog = Some(fog.clone());
                }
                fog.color = Color::linear_rgba(f.color[0], f.color[1], f.color[2], f.density.max(0.05));
                fog.falloff = FogFalloff::Linear { start: f.start, end: f.end };
            }
            None => {
                if let Some(f) = swim.level_fog.take() {
                    *fog = f;
                }
            }
        }
    }
    post.0 = new.and_then(|w| w.water.grade.map(|g| (now.unwrap_or(0) as u32, g)));
    if let Some(w) = new {
        if !w.water.underwater.0.is_empty() {
            audio.write(PostEvent::named(&w.water.underwater.0, None));
        }
    }
    *last = now;
}

// (the breath gauge is the HUD movie's: `oxygen`)
