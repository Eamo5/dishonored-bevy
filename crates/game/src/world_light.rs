//! Lighting for dynamic actors (NPCs, the player's held items).
//!
//! The original game lit characters with a DynamicLightEnvironment that sampled the
//! static lights. We do the same on the CPU: each actor owns a slot in the world
//! lighting storage buffer, and a few times per second we estimate its irradiance from
//! nearby static lights (with line-of-sight checks) plus sun visibility.

use crate::level::GROUP_WORLD;
use crate::GameState;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bevy_rapier3d::prelude::*;

pub struct WorldLightPlugin;

impl Plugin for WorldLightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (update_actor_lighting, upload_world_lighting).chain().run_if(in_state(GameState::InGame)),
        );
    }
}

/// The level's Lightmass volume samples, bucketed on grids for lookups: each sample on the
/// finest grid whose cells it doesn't dwarf (some levels have samples hundreds of metres wide;
/// bucketing those on the fine grid took a level 20 s to spawn).
#[derive(Default)]
pub struct LightVolume {
    samples: Vec<dhcook::format::VolumeSample>,
    grids: Vec<(f32, std::collections::HashMap<(i32, i32, i32), Vec<u32>>)>,
    /// wider still: looked at everywhere
    wide: Vec<u32>,
}

/// Lighting at a point, from the light volume (UE units).
#[derive(Clone, Copy, Debug)]
pub struct VolumeLight {
    pub ambient: Vec3,
    /// the dominant incident direction (towards the light) and its radiance
    pub dir: Vec3,
    pub directional: Vec3,
    /// fraction not shadowed from the dominant (sun) light
    pub unshadowed: f32,
}

/// The grids' cell sizes (metres); a sample goes on the first whose cell is at least an
/// eighth of its radius.
const VOLUME_CELLS: [f32; 3] = [4.0, 32.0, 256.0];

impl LightVolume {
    pub fn new(samples: Vec<dhcook::format::VolumeSample>) -> Self {
        let mut grids: Vec<(f32, std::collections::HashMap<(i32, i32, i32), Vec<u32>>)> = VOLUME_CELLS.iter().map(|c| (*c, Default::default())).collect();
        let mut wide = Vec::new();
        for (i, s) in samples.iter().enumerate() {
            let Some((cell, grid)) = grids.iter_mut().find(|(c, _)| s.radius <= c * 8.0) else {
                wide.push(i as u32);
                continue;
            };
            let p = Vec3::from(s.position);
            let lo = ((p - s.radius) / *cell).floor().as_ivec3();
            let hi = ((p + s.radius) / *cell).floor().as_ivec3();
            for x in lo.x..=hi.x {
                for y in lo.y..=hi.y {
                    for z in lo.z..=hi.z {
                        grid.entry((x, y, z)).or_default().push(i as u32);
                    }
                }
            }
        }
        LightVolume { samples, grids, wide }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Interpolated lighting, weighted as UE3 does: (1 - d^2/r^2) / r^2 for every sample
    /// whose radius contains the point.
    pub fn sample(&self, p: Vec3) -> Option<VolumeLight> {
        let near = self.grids.iter().filter_map(|(cell, g)| {
            let c = (p / *cell).floor().as_ivec3();
            g.get(&(c.x, c.y, c.z))
        });
        let (mut wsum, mut amb, mut col, mut dir, mut lit) = (0.0, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, 0.0);
        let lum = |v: Vec3| v.dot(Vec3::new(0.3, 0.59, 0.11));
        for &i in near.flatten().chain(self.wide.iter()) {
            let s = &self.samples[i as usize];
            let r2 = s.radius * s.radius;
            let d2 = Vec3::from(s.position).distance_squared(p);
            if d2 >= r2 || r2 <= 0.0 {
                continue;
            }
            let w = (1.0 - d2 / r2) / r2;
            let (ind, env) = (Vec3::from(s.indirect), Vec3::from(s.environment));
            wsum += w;
            amb += Vec3::from(s.ambient) * w;
            col += (ind + env) * w;
            dir += (Vec3::from(s.indirect_dir) * lum(ind) + Vec3::from(s.environment_dir) * lum(env)) * w;
            lit += (1.0 - s.shadowed) * w;
        }
        if wsum <= 0.0 {
            return None;
        }
        Some(VolumeLight { ambient: amb / wsum, dir: dir.normalize_or(Vec3::Y), directional: col / wsum, unshadowed: lit / wsum })
    }
}

#[derive(Clone, Copy)]
pub struct EnvLight {
    pub pos: Vec3,
    pub radius: f32,
    pub color: Vec3,
    /// spot direction and cos(outer cone), or None for point lights
    pub spot: Option<(Vec3, f32)>,
}

#[derive(Resource)]
pub struct WorldLighting {
    pub buffer: Handle<ShaderBuffer>,
    pub entries: Vec<[f32; 4]>,
    pub dyn_base: u32,
    pub dyn_cap: u32,
    pub dyn_next: u32,
    pub lights: Vec<EnvLight>,
    pub sun_dir: Vec3,
    pub sun_enabled: bool,
    /// the dominant directional light's colour (UE light colour x brightness)
    pub sun_color: Vec3,
    pub dirty: bool,
    pub timer: f32,
    /// Ambient fill used for actors (irradiance units, like the lightmaps).
    pub ambient: f32,
    /// The original shaders' instance buffer (`ue3prog::INSTANCE_STRIDE` vec4 per tag).
    pub ue_buffer: Option<Handle<ShaderBuffer>>,
    pub ue_entries: Vec<[f32; 4]>,
    pub volume: LightVolume,
}

/// An entity whose world-material meshes read their lighting from a dynamic slot.
#[derive(Component)]
pub struct LitActor {
    pub slot: u32,
    /// Height above the entity origin used as the sampling point.
    pub probe_height: f32,
    /// Smoothed irradiance (luminance-ish) for gameplay (stealth uses it).
    pub brightness: f32,
    pub color: Vec3,
    pub sun: f32,
    /// the light environment's dominant local light: direction towards it and colour
    pub dominant: Option<(Vec3, Vec3)>,
}

impl WorldLighting {
    pub fn alloc_slot(&mut self) -> u32 {
        if self.dyn_next >= self.dyn_cap {
            return 0;
        }
        let s = self.dyn_base + self.dyn_next;
        self.dyn_next += 1;
        s
    }

    pub fn set_slot(&mut self, slot: u32, color: Vec3, sun: bool) {
        self.set_slot_env(slot, color, sun, None);
    }

    /// A light environment for a slot: ambient, sun visibility and the dominant local light
    /// (direction towards it, colour), which the original shaders' light pass renders.
    pub fn set_slot_env(&mut self, slot: u32, color: Vec3, sun: bool, dominant: Option<(Vec3, Vec3)>) {
        if slot == 0 {
            return;
        }
        let i = slot as usize * 3;
        if i + 2 >= self.entries.len() {
            return;
        }
        self.entries[i + 1] = [color.x, color.y, color.z, 0.0];
        let b = |v: u32| f32::from_bits(v);
        self.entries[i + 2] = [b(0), b(0), b(2), b(if sun { 2 } else { 0 })];
        let u = slot as usize * dhcook::ue3prog::INSTANCE_STRIDE as usize;
        if u + 7 < self.ue_entries.len() {
            self.ue_entries[u + 3] = [b(0), b(0), b(2), b(if sun || dominant.is_some() { 2 } else { 0 })];
            let a = color * UE_AMBIENT_SCALE;
            self.ue_entries[u + 5] = [a.x, a.y, a.z, 1.0];
            // a local dominant light replaces the sun in the light pass (UE space, towards it)
            match dominant {
                Some((d, c)) => {
                    self.ue_entries[u + 6] = [d.x, d.z, d.y, 1.0];
                    self.ue_entries[u + 7] = [c.x, c.y, c.z, 0.0];
                }
                None => self.ue_entries[u + 6] = [0.0; 4],
            }
        }
        self.dirty = true;
    }

    /// Mark an instance as out of the dominant light (both lighting paths).
    pub fn set_sun_shadowed(&mut self, tag: u32) {
        let i = tag as usize * 3 + 2;
        if i < self.entries.len() {
            self.entries[i][3] = f32::from_bits(0);
        }
        let u = tag as usize * dhcook::ue3prog::INSTANCE_STRIDE as usize + 3;
        if u < self.ue_entries.len() {
            self.ue_entries[u][3] = f32::from_bits(0);
        }
        self.dirty = true;
    }

    /// Estimate irradiance (lightmap units) and sun visibility at `p`.
    pub fn estimate(&self, p: Vec3, ctx: &RapierContext, exclude: Option<Entity>) -> (Vec3, bool) {
        let (irr, sun, _) = self.environment(p, ctx, exclude);
        (irr, sun)
    }

    /// A light environment at `p`, as UE3's DynamicLightEnvironment composes it for
    /// characters: the brightest visible local light becomes the dominant light (rendered
    /// with the light pass), the others go to the ambient. Returns (ambient irradiance,
    /// sun visible, dominant local light (direction towards it, colour)).
    pub fn environment(&self, p: Vec3, ctx: &RapierContext, exclude: Option<Entity>) -> (Vec3, bool, Option<(Vec3, Vec3)>) {
        // the baked light volume (indirect lighting, as the original light environments use
        // it): ambient term, its strongest incident direction, baked sun shadowing
        let vol = self.volume.sample(p);
        let mut filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
        if let Some(e) = exclude {
            filter = filter.exclude_collider(e);
        }
        let mut contrib: Vec<(f32, Vec3, Vec3)> = Vec::new();
        for l in &self.lights {
            let d = l.pos - p;
            let dist = d.length();
            if dist >= l.radius || dist < 1e-3 {
                continue;
            }
            let mut a = (1.0 - (dist / l.radius).powi(2)).max(0.0);
            a *= a;
            if let Some((dir, cos_outer)) = l.spot {
                let c = (-d / dist).dot(dir);
                if std::env::var("DH_LIGHT_LOG2").is_ok() && dist < 8.0 {
                    info!("env light at {:.2} dist {dist:.2} r {:.1} col {:.2} spot cos {c:.2} outer {cos_outer:.2}", l.pos, l.radius, l.color);
                }
                if c < cos_outer {
                    continue;
                }
                a *= ((c - cos_outer) / (1.0 - cos_outer).max(1e-3)).clamp(0.0, 1.0).sqrt();
            }
            let c = l.color * a;
            contrib.push((c.max_element(), c, l.pos));
        }
        contrib.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut irr = match &vol {
            Some(v) => (v.ambient + v.directional * VOLUME_WRAP) * VOLUME_AMBIENT / UE_AMBIENT_SCALE,
            None => Vec3::splat(self.ambient),
        };
        let mut dominant = None;
        // as UE3's light environments: the light's visibility is the share of traces from
        // points over the actor (head, middle, feet) that reach it
        let probes = [p + Vec3::Y * 0.45, p, p - Vec3::Y * 0.6];
        for (i, (_, c, lp)) in contrib.iter().enumerate() {
            if i < 6 {
                // stop short of the light: fixtures enclose their light's position
                let seen = probes
                    .iter()
                    .filter(|q| {
                        let d = *lp - **q;
                        let dist = d.length();
                        ctx.cast_ray(**q, d / dist.max(1e-3), (dist - 0.6).max(0.0), true, filter).is_none()
                    })
                    .count() as f32
                    / probes.len() as f32;
                if seen <= 0.0 {
                    if std::env::var("DH_LIGHT_LOG2").is_ok() {
                        info!("env light at {:.2} blocked (col {:.2})", *lp, c);
                    }
                    continue;
                }
                let c = *c * seen;
                let d = *lp - p;
                let dist = d.length();
                if dominant.is_none() {
                    dominant = Some((d / dist, c));
                    continue;
                }
                irr += c * 0.22;
                continue;
            } else if i > 16 {
                break;
            }
            irr += *c * 0.22;
        }
        let sun = match &vol {
            Some(v) => self.sun_enabled && v.unshadowed > 0.5,
            None => self.sun_enabled && ctx.cast_ray(p, -self.sun_dir, 600.0, true, filter).is_none(),
        };
        // without a direct local light, the volume's dominant direction lights the actor
        if dominant.is_none() {
            dominant = vol.filter(|v| v.directional.max_element() > 1e-4).map(|v| (v.dir, v.directional * VOLUME_DIRECTIONAL));
        }
        (irr, sun, dominant)
    }
}

fn update_actor_lighting(
    time: Res<Time>,
    wl: Option<ResMut<WorldLighting>>,
    rapier: ReadRapierContext,
    mut actors: Query<(Entity, &GlobalTransform, &mut LitActor)>,
) {
    let Some(mut wl) = wl else { return };
    wl.timer -= time.delta_secs();
    if wl.timer > 0.0 {
        return;
    }
    wl.timer = 0.15;
    let Ok(ctx) = rapier.single() else { return };
    let mut updates = Vec::new();
    for (e, gt, mut la) in &mut actors {
        let p = gt.translation() + Vec3::Y * la.probe_height;
        let (irr, sun, dominant) = wl.environment(p, &ctx, Some(e));
        // smooth to avoid popping
        la.color = la.color.lerp(irr, 0.5);
        la.sun = la.sun + ((sun as u8 as f32) - la.sun) * 0.5;
        // the sun stays the dominant light where it shines and outweighs the local light
        let sun_strength = if la.sun > 0.5 { wl.sun_color.max_element() } else { 0.0 };
        let local = dominant.filter(|(_, c)| c.max_element() > sun_strength);
        la.dominant = match (la.dominant, local) {
            (Some((d0, c0)), Some((d1, c1))) => Some((d0.lerp(d1, 0.5).normalize_or(d1), c0.lerp(c1, 0.5))),
            (None, Some((d1, c1))) => Some((d1, c1 * 0.5)),
            (Some((d0, c0)), None) if c0.max_element() > 0.02 => Some((d0, c0 * 0.5)),
            _ => None,
        };
        let sun_lum: f32 = if sun { 0.6 } else { 0.0 };
        let dom_lum = la.dominant.map(|(_, c)| c.dot(Vec3::new(0.3, 0.59, 0.11)) * 0.3).unwrap_or(0.0);
        la.brightness = la.color.dot(Vec3::new(0.3, 0.59, 0.11)) + sun_lum.max(dom_lum);
        if la.probe_height == 0.5 && std::env::var("DH_LIGHT_LOG").is_ok() {
            info!("player light env at {p}: ambient {:.3} sun {} dominant {:?} raw {:?}", la.color, la.sun, la.dominant, dominant);
        }
        updates.push((la.slot, la.color, la.sun > 0.5, la.dominant));
    }
    for (slot, c, s, d) in updates {
        wl.set_slot_env(slot, c, s, d);
    }
}

fn upload_world_lighting(wl: Option<ResMut<WorldLighting>>, mut buffers: ResMut<Assets<ShaderBuffer>>) {
    let Some(mut wl) = wl else { return };
    if !wl.dirty || std::env::var("DH_SKIP").map(|s| s.contains("upload")).unwrap_or(false) {
        return;
    }
    wl.dirty = false;
    if let Some(mut buf) = buffers.get_mut(&wl.buffer) {
        *buf = ShaderBuffer::from(wl.entries.clone());
    }
    if let Some(h) = wl.ue_buffer.clone() {
        if let Some(mut buf) = buffers.get_mut(&h) {
            *buf = ShaderBuffer::from(wl.ue_entries.clone());
        }
    }
}

/// Light volume radiance to the original shaders' ambient colour, the share of the
/// directional lobes that wraps into the ambient, and the lobes' scale as a dominant light.
const VOLUME_AMBIENT: f32 = 1.0;
const VOLUME_WRAP: f32 = 0.25;
const VOLUME_DIRECTIONAL: f32 = 1.0;

/// Estimated irradiance (lightmap units of the approximated path) to the original shaders'
/// ambient colour.
pub const UE_AMBIENT_SCALE: f32 = 4.0;
