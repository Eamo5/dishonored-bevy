//! The swords' swing trails: the swings' clips mark when the blade trails (`DisNotify_Trails`:
//! from its time, so long), and the swords' melee extents say with what
//! (`DisMeleeExtentTweak.m_ParticleSystemComponent`: `Sword_Trail`, an AnimTrail emitter): a
//! strip between the blade's ends (`BladeExtent_BL`, `BladeExtent_UR`) where they swept, each
//! sample fading over the emitter's lifetime by its colour and alpha over life. Corvo's, on his
//! arms' clips and his own time, and the characters', on theirs.

use crate::anim::{Animator, ClipId};
use crate::level::{GameAssets, LevelInfo};
use crate::GameState;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

pub struct TrailsPlugin;

impl Plugin for TrailsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, trails.after(bevy::transform::TransformSystems::Propagate).run_if(in_state(GameState::InGame)));
    }
}

/// A sword whose swings trail: its blade's ends in its frame, whose clips it follows (the
/// animator's entity), drawn for the first-person view or the world; the clips' trails (by
/// the animator's clips, found once), the samples left (both ends, their age), the strip.
#[derive(Component)]
pub struct BladeTrail {
    ends: [Vec3; 2],
    animator: Entity,
    view: bool,
    windows: Option<Vec<(ClipId, f32, f32)>>,
    knots: Vec<(Vec3, Vec3, f32)>,
    strip: Option<(Entity, Handle<Mesh>)>,
}

impl BladeTrail {
    pub fn new(ends: [Vec3; 2], animator: Entity, view: bool) -> BladeTrail {
        BladeTrail { ends, animator, view, windows: None, knots: Vec::new(), strip: None }
    }
}

/// A sword prop's blade, its ends in the mesh's frame (cooked from its sockets).
pub fn blade_of(scene: &dhcook::format::Scene, prop: &str) -> Option<[Vec3; 2]> {
    let b = scene.props.iter().find(|p| p.name == prop)?.blade?;
    Some([Vec3::from(b[0]), Vec3::from(b[1])])
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn trails(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    (level, assets): (Option<Res<LevelInfo>>, Option<Res<GameAssets>>),
    mut meshes: ResMut<Assets<Mesh>>,
    mut blades: Query<(&mut BladeTrail, &GlobalTransform)>,
    animators: Query<(&Animator, Option<&crate::npc::Npc>)>,
    mut strips: Query<(&mut Transform, &mut Visibility)>,
) {
    let (Some(level), Some(assets)) = (level, assets) else { return };
    // the emitter: its material, lifetime, colours
    let Some(def) = level.scene.effects.iter().find(|(n, _)| n == "sword_trail").and_then(|(_, i)| level.scene.particle_systems.get(*i as usize)).and_then(|s| s.emitters.first()) else { return };
    let module = |class: &str| def.modules.iter().find(|m| m.class == class);
    let life = module("ParticleModuleLifetime").map_or(0.2, |m| crate::particles::f1(m, "Lifetime", 0.0, [0.5; 3], 0.2)).max(0.02);
    let start = module("ParticleModuleColor");
    let (c0, a0) = start.map_or((Vec3::ONE, 1.0), |m| (crate::particles::f3(m, "StartColor", 0.0, [0.5; 3], Vec3::ONE), crate::particles::f1(m, "StartAlpha", 0.0, [0.5; 3], 1.0)));
    let over = module("ParticleModuleColorOverLife");
    let additive = !assets.particle_mats.get(&def.material).is_some_and(|m| m.original())
        && matches!(level.scene.materials.get(def.material as usize).map(|m| m.blend), Some(dhcook::format::Blend::Additive));
    let colour = |age: f32| {
        let t = (age / life).clamp(0.0, 1.0);
        let (mut c, mut a) = (c0, a0);
        if let Some(m) = over {
            if m.dists.contains_key("ColorOverLife") {
                c = crate::particles::f3(m, "ColorOverLife", t, [0.5; 3], c);
            }
            a = crate::particles::f1(m, "AlphaOverLife", t, [0.5; 3], a).clamp(0.0, 1.0);
        }
        let rgb = if additive { c * a } else { c };
        [rgb.x.max(0.0), rgb.y.max(0.0), rgb.z.max(0.0), a]
    };
    for (mut b, gt) in &mut blades {
        let Ok((anim, npc)) = animators.get(b.animator) else { continue };
        // (its time: Corvo's own for his, the world's or the character's for theirs)
        let scale = if b.view { tc.own_scale() } else { tc.npc_scale(npc.is_some_and(|n| n.out_of_bend)) };
        let dt = time.delta_secs() * scale;
        if b.windows.is_none() {
            b.windows = Some(anim.lib.marks(&level.scene).into_iter().flat_map(|(id, m)| m.trails.into_iter().map(move |(t, d)| (id, t, d))).collect());
        }
        // trailing: its clip within one of its trails
        let on = anim.current().is_some_and(|p| b.windows.as_ref().is_some_and(|w| w.iter().any(|(c, t, d)| *c == p.clip && p.t >= *t && p.t <= t + d)));
        for k in b.knots.iter_mut() {
            k.2 += dt;
        }
        b.knots.retain(|k| k.2 < life);
        if on && dt > 0.0 {
            let (p0, p1) = (gt.transform_point(b.ends[0]), gt.transform_point(b.ends[1]));
            if b.knots.is_empty() && std::env::var("DH_TRAIL_LOG").is_ok() {
                info!("trail: {} swing at {p0:.1}..{p1:.1}", if b.view { "Corvo's" } else { "a character's" });
            }
            b.knots.push((p0, p1, 0.0));
            // (tests: `DH_TRAIL_SHOT`, a screenshot mid-swing, the first three)
            if b.knots.len() == 6 {
                if let Some(dir) = std::env::var_os("DH_TRAIL_SHOT") {
                    static SHOTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    let n = SHOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if n < 3 {
                        let path = std::path::PathBuf::from(dir).join(format!("trail_{n}.png"));
                        commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(bevy::render::view::screenshot::save_to_disk(path));
                    }
                }
            }
        }
        // the strip: oldest to newest, its texture along the trail, across the blade
        if b.knots.len() < 2 {
            if let Some((s, _)) = b.strip {
                if let Ok((_, mut v)) = strips.get_mut(s) {
                    if *v != Visibility::Hidden {
                        *v = Visibility::Hidden;
                    }
                }
            }
            continue;
        }
        if b.strip.is_none() {
            let Some(mat) = assets.particle_mats.get(&def.material) else { continue };
            let mesh = meshes.add(Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default()));
            let mut ec = commands.spawn((Mesh3d(mesh.clone()), Transform::IDENTITY, Visibility::Hidden, NoFrustumCulling, bevy::light::NotShadowCaster, DespawnOnExit(GameState::InGame), Name::new("blade trail")));
            mat.apply(&mut ec);
            if b.view {
                ec.insert(RenderLayers::layer(crate::combat::VIEW_LAYER));
            }
            b.strip = Some((ec.id(), mesh));
        }
        let Some((s, handle)) = b.strip.clone() else { continue };
        let origin = b.knots.last().map(|k| k.0).unwrap_or(Vec3::ZERO);
        let n = b.knots.len();
        let (mut positions, mut normals, mut tangents, mut uvs, mut colors) = (Vec::with_capacity(n * 2), Vec::with_capacity(n * 2), Vec::with_capacity(n * 2), Vec::with_capacity(n * 2), Vec::with_capacity(n * 2));
        let mut indices: Vec<u32> = Vec::with_capacity((n - 1) * 6);
        for (i, (a, z, age)) in b.knots.iter().enumerate() {
            let next = b.knots.get(i + 1).or(b.knots.get(i.wrapping_sub(1))).map_or(*a, |k| k.0);
            let along = (next - *a).normalize_or(Vec3::X);
            let across = (*z - *a).normalize_or(Vec3::Y);
            let normal = along.cross(across).normalize_or(Vec3::Y);
            let u = (age / life).clamp(0.0, 1.0);
            let c = colour(*age);
            for (q, v) in [(*a, 0.0), (*z, 1.0)] {
                positions.push((q - origin).to_array());
                normals.push(normal.to_array());
                tangents.push(along.extend(1.0).to_array());
                uvs.push([u, v]);
                colors.push(c);
            }
            if i + 1 < n {
                let base = (i * 2) as u32;
                indices.extend_from_slice(&[base, base + 2, base + 3, base, base + 3, base + 1]);
            }
        }
        if let Some(mut mesh) = meshes.get_mut(&handle) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uvs.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, tangents);
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
            mesh.insert_indices(Indices::U32(indices));
        }
        if let Ok((mut t, mut v)) = strips.get_mut(s) {
            t.translation = origin;
            if *v != Visibility::Inherited {
                *v = Visibility::Inherited;
            }
        }
    }
}
