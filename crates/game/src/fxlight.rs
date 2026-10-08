//! Gameplay lights (explosion flashes, burning bolts) on the surfaces drawn with the original
//! shaders: like the levels' runtime lights, each one is the materials' own point-light pass
//! added over the surfaces it reaches, following the light as it moves and fades.

use crate::level::LevelInfo;
use crate::ue3mat::{Ue3Material, Ue3Programs};
use crate::GameState;
use bevy::mesh::MeshTag;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use dhcook::format::Blend;
use dhcook::ue3prog::Policy;

pub struct FxLightPlugin;

impl Plugin for FxLightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LitSurfaces>()
            .init_resource::<FxPasses>()
            .init_resource::<RuntimeLights>()
            .init_resource::<PartLightMats>()
            .init_resource::<LightLevels>()
            .add_systems(Update, (fx_light_passes, part_light_passes, apply_light_levels).chain().run_if(in_state(GameState::InGame)))
            .add_systems(OnExit(GameState::InGame), |mut p: ResMut<FxPasses>, mut m: ResMut<PartLightMats>, mut l: ResMut<LightLevels>| {
                p.0.clear();
                m.0.clear();
                *l = LightLevels::default();
            });
    }
}

/// A light from gameplay: linear colour, UE3 brightness and radius (metres).
#[derive(Component, Clone, Copy)]
pub struct FxLight {
    pub color: Vec3,
    pub brightness: f32,
    pub radius: f32,
}

/// The level's surfaces drawn with the original shaders, for the gameplay lights' passes.
#[derive(Resource, Default)]
pub struct LitSurfaces(pub Vec<LitSurface>);

pub struct LitSurface {
    /// the instance (its passes are its children)
    pub entity: Entity,
    pub center: Vec3,
    pub reach: f32,
    pub tag: u32,
    /// mesh, material id and that material's base-pass material
    pub parts: Vec<(Handle<Mesh>, u32, Handle<Ue3Material>)>,
}

/// The level's runtime point and spot lights: light index, pass policy, position, radius
/// and the shaders' parameters.
#[derive(Resource, Default)]
pub struct RuntimeLights(pub Vec<(u32, Policy, Vec3, f32, [Vec4; 4])>);

/// A character part drawn with the original shaders: the runtime and gameplay lights near it
/// add their passes, as UE3's light environments light characters with the dynamic lights.
#[derive(Component)]
pub struct LitPart;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum LightKey {
    Level(u32),
    Fx(Entity),
}

/// The light passes on a part.
#[derive(Component, Default)]
struct PartPasses {
    passes: Vec<(LightKey, Entity)>,
    next: f32,
    /// the part's material the passes were made for (the arms swap skins)
    base: Option<AssetId<Ue3Material>>,
}

/// The light-pass material per (light, part material).
#[derive(Resource, Default)]
struct PartLightMats(HashMap<(LightKey, AssetId<Ue3Material>), Option<Handle<Ue3Material>>>);

/// Level lights' brightness as the matinees animate it (`DominantPointLightComponent0.Brightness`:
/// the Flooded District's sparking cable, the sewer furnace, the Brothel's electrocution, the
/// Lighthouse's lightning): light -> UE3 brightness, put on its passes (the surfaces' and the
/// characters'), its Bevy light, and for the dominant sun the world's sun colour.
#[derive(Resource, Default)]
pub struct LightLevels {
    pub want: HashMap<u32, f32>,
    applied: HashMap<u32, f32>,
    /// Bevy lights' own intensity at the level's brightness
    base: HashMap<Entity, f32>,
}

#[allow(clippy::type_complexity)]
fn apply_light_levels(
    mut levels: ResMut<LightLevels>,
    level: Option<Res<LevelInfo>>,
    passes: Query<(&crate::level::LevelLight, &MeshMaterial3d<Ue3Material>)>,
    mut bevy_lights: Query<(Entity, &crate::level::LevelLight, Option<&mut PointLight>, Option<&mut SpotLight>)>,
    cache: Res<PartLightMats>,
    mut runtime: ResMut<RuntimeLights>,
    mut materials: ResMut<Assets<Ue3Material>>,
    wl: Option<ResMut<crate::world_light::WorldLighting>>,
) {
    let Some(level) = level else { return };
    let changed: Vec<(u32, f32)> = levels.want.iter().filter(|(li, b)| levels.applied.get(*li).is_none_or(|a| (a - **b).abs() > 1e-3)).map(|(li, b)| (*li, *b)).collect();
    if changed.is_empty() {
        return;
    }
    let mut wl = wl;
    for (li, b) in changed {
        levels.applied.insert(li, b);
        let Some(l) = level.scene.lights.get(li as usize) else { continue };
        let color = Vec3::from(l.color) * b.max(0.0);
        if l.kind == dhcook::format::LightKind::Directional {
            // the dominant sun: the original shaders' entry 0 and the characters' sun
            if let Some(wl) = wl.as_mut() {
                if wl.ue_entries.len() > 1 {
                    wl.ue_entries[1] = [color.x, color.y, color.z, 0.0];
                }
                wl.sun_color = color;
                wl.dirty = true;
            }
            continue;
        }
        let set = |m: &mut Ue3Material| {
            let p = &mut m.params.p[dhcook::ue3prog::DYN_LIGHT_SLOT + 1];
            *p = color.extend(p.w);
        };
        for (ll, mat) in &passes {
            if ll.0 == li {
                if let Some(mut m) = materials.get_mut(&mat.0) {
                    set(&mut m);
                }
            }
        }
        for ((key, _), h) in &cache.0 {
            if *key == LightKey::Level(li) {
                if let Some(mut m) = h.as_ref().and_then(|h| materials.get_mut(h)) {
                    set(&mut m);
                }
            }
        }
        if let Some(r) = runtime.0.iter_mut().find(|r| r.0 == li) {
            r.4[1] = color.extend(r.4[1].w);
        }
        let ratio = if l.brightness > 0.0 { b.max(0.0) / l.brightness } else { 0.0 };
        for (e, ll, point, spot) in &mut bevy_lights {
            if ll.0 != li {
                continue;
            }
            if let Some(mut p) = point {
                let base = *levels.base.entry(e).or_insert(p.intensity);
                p.intensity = base * ratio;
            } else if let Some(mut s) = spot {
                let base = *levels.base.entry(e).or_insert(s.intensity);
                s.intensity = base * ratio;
            }
        }
    }
}

/// The most lights a character part takes at once.
const PART_LIGHTS: usize = 3;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn part_light_passes(
    mut commands: Commands,
    time: Res<Time>,
    mut parts: Query<
        (Entity, &GlobalTransform, &Mesh3d, &MeshTag, &bevy::mesh::skinning::SkinnedMesh, &MeshMaterial3d<Ue3Material>, Option<&mut PartPasses>, Option<&bevy::camera::visibility::RenderLayers>),
        With<LitPart>,
    >,
    fx: Query<(Entity, &FxLight, &GlobalTransform)>,
    runtime: Res<RuntimeLights>,
    toggles: Query<(&crate::level::LevelLight, &Visibility), Without<LitPart>>,
    mut cache: ResMut<PartLightMats>,
    mut materials: ResMut<Assets<Ue3Material>>,
    mut programs: ResMut<Ue3Programs>,
    mut shaders: ResMut<Assets<Shader>>,
    mut disabled: Local<Option<bool>>,
) {
    // `DH_NO_PART_LIGHTS`: characters without the dynamic lights' passes (for comparison)
    if *disabled.get_or_insert_with(|| std::env::var("DH_NO_PART_LIGHTS").is_ok()) {
        return;
    }
    // gameplay lights follow their light every frame
    let fx_params: HashMap<Entity, [Vec4; 4]> = fx.iter().map(|(e, l, gt)| (e, light_params(gt.translation(), l))).collect();
    cache.0.retain(|(k, _), m| match k {
        LightKey::Fx(e) => match fx_params.get(e) {
            Some(p) => {
                if let Some(mut mat) = m.as_ref().and_then(|h| materials.get_mut(h)) {
                    for (i, v) in p.iter().enumerate() {
                        mat.params.p[dhcook::ue3prog::DYN_LIGHT_SLOT + i] = *v;
                    }
                }
                true
            }
            None => false,
        },
        LightKey::Level(_) => true,
    });
    let dt = time.delta_secs();
    let mut hidden: Option<std::collections::HashSet<u32>> = None;
    for (e, gt, mesh, tag, skin, mat, passes, layers) in &mut parts {
        let Some(mut passes) = passes else {
            commands.entity(e).try_insert(PartPasses::default());
            continue;
        };
        if passes.base != Some(mat.0.id()) {
            passes.base = Some(mat.0.id());
            for (_, pe) in passes.passes.drain(..) {
                commands.entity(pe).try_despawn();
            }
            passes.next = 0.0;
        }
        passes.next -= dt;
        if passes.next > 0.0 {
            continue;
        }
        passes.next = 0.2;
        // characters from their chest; the view model from the eye it hangs on
        let center = gt.translation() + if layers.is_some() { Vec3::ZERO } else { Vec3::Y };
        // the lights reaching it, strongest first
        let off = hidden.get_or_insert_with(|| toggles.iter().filter(|(_, v)| **v == Visibility::Hidden).map(|(l, _)| l.0).collect());
        let mut near: Vec<(f32, LightKey, Policy)> = Vec::new();
        for (li, policy, pos, radius, params) in &runtime.0 {
            let d = pos.distance(center);
            if d < radius + 0.5 && !off.contains(li) {
                let policy = if matches!(policy, Policy::SpotLight | Policy::SpotLightShadowed) { Policy::SpotLight } else { Policy::PointLight };
                near.push((params[1].truncate().length() * (1.0 - d / (radius + 0.5)).powi(2), LightKey::Level(*li), policy));
            }
        }
        for (le, l, lgt) in &fx {
            let d = lgt.translation().distance(center);
            if d < l.radius + 0.5 && l.brightness > 0.0 {
                near.push((l.brightness * (1.0 - d / (l.radius + 0.5)).powi(2), LightKey::Fx(le), Policy::PointLight));
            }
        }
        near.sort_by(|a, b| b.0.total_cmp(&a.0));
        near.truncate(PART_LIGHTS);
        // drop the passes of lights out of reach, add the new ones
        passes.passes.retain(|(k, pe)| {
            let keep = near.iter().any(|n| n.1 == *k);
            if !keep {
                commands.entity(*pe).try_despawn();
            }
            keep
        });
        for (_, key, policy) in near {
            if passes.passes.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let lm = cache
                .0
                .entry((key, mat.0.id()))
                .or_insert_with(|| {
                    let base = materials.get(&mat.0)?;
                    if !base.light_ok {
                        return None;
                    }
                    let params = match key {
                        LightKey::Level(li) => runtime.0.iter().find(|r| r.0 == li)?.4,
                        LightKey::Fx(le) => *fx_params.get(&le)?,
                    };
                    let program = programs.get(base.map, policy, false, &mut shaders)?;
                    let mut m = base.clone();
                    m.key.program = program;
                    m.key.blend = 3;
                    m.key.light_pass = true;
                    for (i, v) in params.iter().enumerate() {
                        m.params.p[dhcook::ue3prog::DYN_LIGHT_SLOT + i] = *v;
                    }
                    Some(materials.add(m))
                })
                .clone();
            let Some(lm) = lm else { continue };
            let mut pass = commands.spawn((
                Mesh3d(mesh.0.clone()),
                MeshTag(tag.0),
                bevy::mesh::skinning::SkinnedMesh { inverse_bindposes: skin.inverse_bindposes.clone(), joints: skin.joints.clone() },
                bevy::camera::visibility::DynamicSkinnedMeshBounds,
                MeshMaterial3d(lm),
                bevy::light::NotShadowCaster,
                bevy::light::NotShadowReceiver,
                Transform::IDENTITY,
                Visibility::Inherited,
            ));
            if let LightKey::Level(li) = key {
                pass.insert(crate::level::LevelLight(li));
            }
            if let Some(l) = layers {
                pass.insert((l.clone(), bevy::camera::visibility::NoFrustumCulling));
            }
            let pe = pass.id();
            commands.queue(attach_or_drop(e, pe));
            passes.passes.push((key, pe));
        }
    }
}

#[derive(Default)]
struct FxState {
    placed: bool,
    /// where the passes were placed
    at: Vec3,
    radius: f32,
    passes: Vec<Entity>,
    /// the light-pass material per surface material
    mats: HashMap<u32, Option<Handle<Ue3Material>>>,
    params: [Vec4; 4],
}

#[derive(Resource, Default)]
struct FxPasses(HashMap<Entity, FxState>);

/// Light parameters in the shaders' layout (UE space; see `ue3prog::DYN_LIGHT_SLOT`).
fn light_params(pos: Vec3, l: &FxLight) -> [Vec4; 4] {
    let ue = |v: Vec3| Vec3::new(v.x, v.z, v.y);
    [
        (ue(pos) * 100.0).extend(1.0 / (l.radius.max(0.1) * 100.0)),
        (l.color * l.brightness).extend(2.0),
        Vec4::new(-1.0, 1.0, 0.0, 0.0),
        Vec4::new(0.0, 0.0, -1.0, 0.0),
    ]
}

#[allow(clippy::too_many_arguments)]
fn fx_light_passes(
    mut commands: Commands,
    lights: Query<(Entity, &FxLight, &GlobalTransform)>,
    surfaces: Res<LitSurfaces>,
    level: Option<Res<LevelInfo>>,
    mut state: ResMut<FxPasses>,
    mut materials: ResMut<Assets<Ue3Material>>,
    mut programs: ResMut<Ue3Programs>,
    mut shaders: ResMut<Assets<Shader>>,
) {
    // lights that went out
    let live: Vec<Entity> = lights.iter().map(|(e, ..)| e).collect();
    state.0.retain(|e, s| {
        let keep = live.contains(e);
        if !keep {
            for p in &s.passes {
                commands.entity(*p).try_despawn();
            }
        }
        keep
    });
    let Some(level) = level else { return };
    for (e, l, gt) in &lights {
        let pos = gt.translation();
        let params = light_params(pos, l);
        let st = state.0.entry(e).or_default();
        // (re)place the passes when the light has moved or grown
        let moved = !st.placed || st.at.distance(pos) > 0.75 || l.radius > st.radius + 0.5;
        if moved && l.brightness > 0.0 {
            for p in st.passes.drain(..) {
                commands.entity(p).try_despawn();
            }
            st.placed = true;
            st.at = pos;
            st.radius = l.radius;
            for s in surfaces.0.iter().filter(|s| s.center.distance(pos) < l.radius + s.reach) {
                for (mesh, mat_id, base) in &s.parts {
                    let mat = st
                        .mats
                        .entry(*mat_id)
                        .or_insert_with(|| {
                            let ue = level.scene.materials.get(*mat_id as usize)?.ue3.as_ref()?;
                            if ue.unlit || !matches!(ue.blend, Blend::Opaque | Blend::Masked) || ue.params.len() > dhcook::ue3prog::DYN_SHADOW_SLOT {
                                return None;
                            }
                            let program = programs.get(ue.map, Policy::PointLight, false, &mut shaders)?;
                            let mut m = materials.get(base)?.clone();
                            m.key.program = program;
                            m.key.blend = 3;
                            m.key.light_pass = true;
                            for (k, v) in params.iter().enumerate() {
                                m.params.p[dhcook::ue3prog::DYN_LIGHT_SLOT + k] = *v;
                            }
                            Some(materials.add(m))
                        })
                        .clone();
                    let Some(mat) = mat else { continue };
                    let pass = commands
                        .spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), MeshTag(s.tag), bevy::light::NotShadowCaster, bevy::light::NotShadowReceiver, Transform::IDENTITY, Visibility::Inherited))
                        .id();
                    commands.queue(attach_or_drop(s.entity, pass));
                    st.passes.push(pass);
                }
            }
            st.params = [Vec4::NAN; 4];
        }
        // follow the light's position and brightness
        if st.params != params {
            st.params = params;
            for h in st.mats.values().flatten() {
                if let Some(mut m) = materials.get_mut(h) {
                    for (k, v) in params.iter().enumerate() {
                        m.params.p[dhcook::ue3prog::DYN_LIGHT_SLOT + k] = *v;
                    }
                }
            }
        }
    }
}

/// Parent a pass to its mesh, or drop it when the mesh went away meanwhile (a level
/// streamed out, a pickup taken).
fn attach_or_drop(parent: Entity, child: Entity) -> impl FnOnce(&mut World) {
    move |world: &mut World| {
        if world.get_entity(parent).is_ok() {
            world.entity_mut(parent).add_child(child);
        } else if let Ok(c) = world.get_entity_mut(child) {
            c.despawn();
        }
    }
}
