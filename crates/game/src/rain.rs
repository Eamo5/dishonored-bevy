//! Rain (`DisSeqAct_SetRainEmitter`): the level's rain emitter becomes the camera's rain box,
//! as the original's DishonoredPlayerCamera keeps it (`m_pRainBoxEmitter`, `m_NumRainDrops`).
//! Its drops (`DisParticleModuleRainDrops`) fall about the eye and stop on whatever is above
//! them, splashing there (`DisParticleModuleRainImpacts`); the scripts hear "Rain Start" /
//! "Rain Stop" as Corvo walks out into it or under cover (they put rain on the lens).

use crate::kismet::{Val, Vm};
use crate::level::GROUP_WORLD;
use crate::player::PlayerCamera;
use crate::GameState;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct RainPlugin;

impl Plugin for RainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Rain>()
            .add_systems(OnEnter(GameState::InGame), |mut r: ResMut<Rain>| *r = Rain::default())
            .add_systems(Update, rain_cover.after(crate::kismet::apply_effects).run_if(in_state(GameState::InGame)));
    }
}

/// Columns of this size (metres) share the height rain stops at.
const CELL: f32 = 1.0;
/// Columns known about the eye (metres).
const REACH: i32 = 9;

#[derive(Resource, Default)]
pub struct Rain {
    /// the level's emitters acting as the rain box (indices into `scene.particles`)
    pub emitters: Vec<u32>,
    /// drops at once (`m_NumRainDrops`)
    pub drops: u32,
    op: Option<u32>,
    delay: f32,
    /// Corvo under the open sky (as the scripts last heard), and how long it has been otherwise
    exposed: Option<bool>,
    changing: f32,
    /// since it was set (the level's collision may not be there at first)
    since: f32,
    probe: f32,
    /// the highest surface in each column near the eye (rain stops there; -inf: nothing)
    tops: HashMap<(i32, i32), f32>,
}

fn cell(p: Vec3) -> (i32, i32) {
    ((p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32)
}

impl Rain {
    pub fn active(&self) -> bool {
        !self.emitters.is_empty()
    }

    /// Where rain falling here stops (`None`: not known yet).
    pub fn top(&self, p: Vec3) -> Option<f32> {
        self.tops.get(&cell(p)).copied()
    }
}

fn rain_cover(
    vm: Option<ResMut<Vm>>,
    time: Res<Time>,
    rapier: ReadRapierContext,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut rain: ResMut<Rain>,
) {
    let Some(mut vm) = vm else { return };
    if let Some((op, targets, drops, delay)) = vm.rain.take() {
        let g = vm.g.clone();
        rain.emitters = targets.iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize) } else { None }).flat_map(|a| a.particles.iter().copied()).collect();
        rain.drops = drops.max(1);
        rain.op = Some(op);
        rain.delay = delay;
        rain.exposed = None;
        rain.since = 0.0;
        if std::env::var("DH_RAIN_LOG").is_ok() {
            info!("rain: emitters {:?}, {} drops", rain.emitters, rain.drops);
        }
    }
    if !rain.active() {
        return;
    }
    let (Ok(cam), Ok(ctx)) = (cam.single(), rapier.single()) else { return };
    let eye = cam.translation();
    let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    // the columns about the eye: the first surface down from high above (nearest first, a few
    // a frame), the far ones forgotten
    rain.since += time.delta_secs();
    if rain.since < 1.5 {
        return;
    }
    let c0 = cell(eye);
    rain.tops.retain(|k, _| (k.0 - c0.0).abs() <= REACH + 4 && (k.1 - c0.1).abs() <= REACH + 4);
    let mut budget = 96;
    'cast: for ring in 0..=REACH {
        for dz in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs() != ring && dz.abs() != ring {
                    continue;
                }
                let key = (c0.0 + dx, c0.1 + dz);
                if rain.tops.contains_key(&key) {
                    continue;
                }
                if budget == 0 {
                    break 'cast;
                }
                budget -= 1;
                let from = Vec3::new((key.0 as f32 + 0.5) * CELL, eye.y + 40.0, (key.1 as f32 + 0.5) * CELL);
                let top = ctx.cast_ray(from, Vec3::NEG_Y, 90.0, true, filter).map(|(_, toi)| from.y - toi).unwrap_or(f32::NEG_INFINITY);
                rain.tops.insert(key, top);
            }
        }
    }
    // under the sky or not (a roof overhead), as the scripts hear it after the start delay
    rain.probe -= time.delta_secs();
    if rain.probe > 0.0 {
        return;
    }
    rain.probe = 0.2;
    let open = ctx.cast_ray(eye, Vec3::Y, 60.0, true, filter).is_none();
    if rain.exposed == Some(open) {
        rain.changing = 0.0;
        return;
    }
    rain.changing += 0.2;
    if open && rain.exposed.is_some() && rain.changing < rain.delay {
        return;
    }
    rain.exposed = Some(open);
    rain.changing = 0.0;
    let Some(op) = rain.op else { return };
    let want = if open { "Rain Start" } else { "Rain Stop" };
    let out = vm.g.ops.get(op as usize).and_then(|o| o.outputs.iter().position(|x| x.desc == want));
    if std::env::var("DH_RAIN_LOG").is_ok() {
        info!("rain: {want} (output {out:?})");
    }
    if let Some(out) = out {
        vm.signal(op, out);
    }
}
