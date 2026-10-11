//! Spawns the loaded level into the Bevy world.

use crate::loading::{PendingLevel, SubMesh};
use crate::lightmap::{entry, make_page_array, LayerParams, LightmapExt, LmParams, WorldMaterial};
use crate::sky::{SkyMaterial, SkyParams};
use crate::ue3mat::{Ue3Key, Ue3Material, Ue3Params, Ue3Programs};
use dhcook::ue3prog::{Policy, INSTANCE_STRIDE};
use crate::world_light::{EnvLight, WorldLighting};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::VertexAttributeValues;
use dhcook::format::SkeletonDef;
use bevy::mesh::MeshTag;
use bevy::render::storage::ShaderBuffer;
use crate::GameState;
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy_rapier3d::prelude::*;
use dhcook::format::{Blend, LightKind, MaterialDef, Scene, TexFile, TexFormat};
use std::collections::HashMap;

pub struct LevelPlugin;

impl Plugin for LevelPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(LightTuning::default())
            .add_systems(OnEnter(GameState::InGame), spawn_level.in_set(LevelSpawnSet))
            .add_systems(Update, (run_sun_probes, stream_volumes).run_if(in_state(GameState::InGame)));
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct LevelSpawnSet;

/// Gameplay-relevant data from the map, kept after spawning.
#[derive(Resource)]
pub struct LevelInfo {
    pub scene: Scene,
}

/// Pre-placed corpses (plague victims, previous fights) rather than live characters.
pub fn is_corpse_type(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("dead_npc") || n.contains("pwndead") || n.contains("pwn_dead")
}

/// Pick the player start: the requested index, or else the one with the fewest live
/// hostile NPCs placed nearby (sub-maps have one start per entrance and some open
/// straight into a guarded room).
pub fn choose_start(scene: &Scene, requested: Option<usize>) -> Option<usize> {
    if scene.player_starts.is_empty() {
        return None;
    }
    if let Some(i) = requested {
        return Some(i.min(scene.player_starts.len() - 1));
    }
    // a mission's own opening start wins (its nearby NPCs belong to the opening scene)
    let tag = scene.player_starts[0].tag.to_ascii_lowercase();
    let tag = tag.strip_prefix("playerstart").unwrap_or(&tag);
    if ["dragged", "newgame", "start", "dropoff", "default", "arrival", "begin"].iter().any(|k| tag.contains(k)) {
        return Some(0);
    }
    let threat = |p: Vec3| -> usize {
        scene
            .spawners
            .iter()
            .filter(|sp| sp.spawn_on_begin_play || sp.script_start)
            .filter_map(|sp| sp.npc_type.and_then(|t| scene.npc_types.get(t as usize)).map(|t| (sp, t)))
            .filter(|(_, t)| matches!(t.kind.as_str(), "guard" | "overseer" | "thug" | "creature") && !is_corpse_type(&t.name))
            .filter(|(sp, _)| Vec3::from(sp.position).distance(p) < 15.0)
            .count()
    };
    // designers' debug starts only when there's nothing else
    let debug = |i: usize| scene.player_starts[i].tag.to_ascii_lowercase().contains("debug");
    (0..scene.player_starts.len()).min_by_key(|&i| (debug(i), threat(Vec3::from(scene.player_starts[i].position)), i))
}

/// A rendered/collidable instance from the original level.
#[derive(Component)]
pub struct LevelInstance {
    pub class: String,
    pub actor: String,
    /// Index into `scene.instances`.
    pub index: u32,
}

/// A blocking volume's collider in a streamed sublevel (its name): solid while it is in.
#[derive(Component)]
pub struct StreamedVolume(pub String);

/// Blocking volumes come and go with their sublevels.
pub fn stream_volumes(mut commands: Commands, vm: Option<Res<crate::kismet::Vm>>, q: Query<(Entity, &StreamedVolume, Has<ColliderDisabled>)>) {
    let Some(vm) = vm else { return };
    for (e, s, off) in &q {
        let out = vm.level_out(&s.0);
        if out && !off {
            commands.entity(e).try_insert(ColliderDisabled);
        } else if !out && off {
            commands.entity(e).try_remove::<ColliderDisabled>();
        }
    }
}

/// Materials the level scripts can put on a level instance's surfaces (`SeqAct_SetMaterial`):
/// (material, the surface's entity, the material made for it).
#[derive(Component)]
pub struct MatSwaps(pub Vec<(u32, Entity, Handle<Ue3Material>)>);

/// Collider entity belonging to a level instance (kept separate so render transforms
/// can carry non-uniform scale).
#[derive(Component)]
pub struct InstanceCollider(pub Entity);

#[derive(Component)]
pub struct LevelLight(pub u32);

struct SunProbe {
    entry: u32,
    points: [Vec3; 2],
    /// tests sun visibility (else only the light environment is estimated)
    sun: bool,
}

/// Instances without baked shadowing get their sun visibility from physics raycasts
/// once the level colliders exist.
#[derive(Resource)]
struct PendingSunProbes {
    probes: Vec<SunProbe>,
    frames: u32,
}

fn run_sun_probes(
    mut commands: Commands,
    pending: Option<ResMut<PendingSunProbes>>,
    rapier: bevy_rapier3d::plugin::ReadRapierContext,
    wl: Option<ResMut<WorldLighting>>,
) {
    let (Some(mut p), Some(mut wl)) = (pending, wl) else { return };
    p.frames += 1;
    if p.frames < 3 {
        return; // wait for colliders to be registered with rapier
    }
    let Ok(ctx) = rapier.single() else { return };
    let to_sun = -wl.sun_dir.normalize_or(Vec3::NEG_Y);
    let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let mut shadowed = 0;
    for probe in &p.probes {
        // light environment of the original shaders (ambient from nearby lights)
        let (irr, _) = wl.estimate(probe.points[0], &ctx, None);
        let u = probe.entry as usize * dhcook::ue3prog::INSTANCE_STRIDE as usize + 5;
        if u < wl.ue_entries.len() {
            let a = irr * crate::world_light::UE_AMBIENT_SCALE;
            wl.ue_entries[u] = [a.x, a.y, a.z, 1.0];
        }
        if !probe.sun || !wl.sun_enabled {
            continue;
        }
        let lit = probe
            .points
            .iter()
            .filter(|pt| ctx.cast_ray(**pt + to_sun * 0.6, to_sun, 800.0, true, filter).is_none())
            .count();
        if lit == 0 {
            wl.set_sun_shadowed(probe.entry);
            shadowed += 1;
        }
    }
    wl.dirty = true;
    info!("sun probes: {} of {} props shadowed", shadowed, p.probes.len());
    commands.remove_resource::<PendingSunProbes>();
}

/// Collision group bits.
pub const GROUP_WORLD: Group = Group::GROUP_1;
pub const GROUP_PLAYER: Group = Group::GROUP_2;
pub const GROUP_NPC: Group = Group::GROUP_3;
pub const GROUP_PROP: Group = Group::GROUP_4;

#[derive(Resource, Clone)]
pub struct LightTuning {
    pub point_scale: f32,
    pub sun_scale: f32,
    pub ambient: f32,
    pub lightmap_exposure: f32,
    pub lightmap_floor: f32,
    pub actor_ambient: f32,
}

impl Default for LightTuning {
    fn default() -> Self {
        let env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        Self {
            point_scale: env("DH_POINT_SCALE", 600.0),
            sun_scale: env("DH_SUN_SCALE", 4000.0),
            ambient: env("DH_AMBIENT", 120.0),
            lightmap_exposure: env("DH_LM_EXPOSURE", 1500.0),
            lightmap_floor: env("DH_LM_FLOOR", 0.03),
            actor_ambient: env("DH_ACTOR_AMBIENT", 0.04),
        }
    }
}

fn tex_format(f: TexFormat, srgb: bool) -> TextureFormat {
    match (f, srgb) {
        (TexFormat::Bc1, true) => TextureFormat::Bc1RgbaUnormSrgb,
        (TexFormat::Bc1, false) => TextureFormat::Bc1RgbaUnorm,
        (TexFormat::Bc2, true) => TextureFormat::Bc2RgbaUnormSrgb,
        (TexFormat::Bc2, false) => TextureFormat::Bc2RgbaUnorm,
        (TexFormat::Bc3, true) => TextureFormat::Bc3RgbaUnormSrgb,
        (TexFormat::Bc3, false) => TextureFormat::Bc3RgbaUnorm,
        (TexFormat::Bc5, _) => TextureFormat::Bc5RgUnorm,
        (TexFormat::Rgba8, true) => TextureFormat::Rgba8UnormSrgb,
        (TexFormat::Rgba8, false) => TextureFormat::Rgba8Unorm,
        (TexFormat::R8, _) => TextureFormat::R8Unorm,
        (TexFormat::Rg8, _) => TextureFormat::Rg8Unorm,
        (TexFormat::Bc4, _) => TextureFormat::Bc4RUnorm,
    }
}

pub fn make_image(tf: &TexFile, srgb: bool) -> Image {
    let mut data = Vec::with_capacity(tf.mips.iter().map(|m| m.len()).sum());
    for m in &tf.mips {
        data.extend_from_slice(m);
    }
    let mut img = Image::new_uninit(
        Extent3d { width: tf.width, height: tf.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        tex_format(tf.format, srgb),
        RenderAssetUsages::RENDER_WORLD,
    );
    img.texture_descriptor.mip_level_count = tf.mips.len() as u32;
    img.data = Some(data);
    let addr = if tf.clamp { ImageAddressMode::ClampToEdge } else { ImageAddressMode::Repeat };
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: addr,
        address_mode_v: addr,
        address_mode_w: addr,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });
    img
}

/// A mesh with every attribute the original shaders read (UE3 local vertex factory).
pub fn make_mesh_full(s: &SubMesh, mirrored: bool) -> Mesh {
    let n = s.positions.len();
    let mut m = make_mesh(s, mirrored);
    if s.uv1.len() != n {
        m.insert_attribute(Mesh::ATTRIBUTE_UV_1, s.uv0.clone());
    }
    if s.tangents.len() != n {
        m.insert_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0f32, 0.0, 0.0, 1.0]; n]);
    }
    let colors: Vec<[f32; 4]> = if s.colors.len() == n {
        s.colors.iter().map(|c| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]).collect()
    } else {
        vec![[1.0; 4]; n]
    };
    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    m
}

pub fn make_mesh(s: &SubMesh, mirrored: bool) -> Mesh {
    let mut indices = s.indices.clone();
    if mirrored {
        for t in indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, s.positions.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, s.normals.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, s.uv0.clone());
    if s.uv1.len() == s.positions.len() {
        m.insert_attribute(Mesh::ATTRIBUTE_UV_1, s.uv1.clone());
    }
    if s.tangents.len() == s.positions.len() {
        m.insert_attribute(Mesh::ATTRIBUTE_TANGENT, s.tangents.clone());
    }
    let skinned = !s.joints.is_empty() && s.joints.len() == s.positions.len();
    if skinned {
        m.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(s.joints.clone()));
        m.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, s.weights.clone());
    }
    m.insert_indices(Indices::U32(indices));
    if skinned {
        if let Ok(b) = m.clone().with_generated_skinned_mesh_bounds() {
            m = b;
        }
    }
    m
}

#[derive(Clone)]
enum MatHandle {
    Std(Handle<WorldMaterial>),
    Sky(Handle<SkyMaterial>),
    Ue3(Handle<Ue3Material>),
}

/// Material of a character or prop part: the original shaders, or the approximation.
#[derive(Clone)]
pub enum PartMat {
    Std(Handle<WorldMaterial>),
    Ue3(Handle<Ue3Material>),
}

impl PartMat {
    pub fn apply(&self, e: &mut EntityCommands) {
        match self {
            PartMat::Std(h) => {
                e.insert(MeshMaterial3d(h.clone()));
            }
            PartMat::Ue3(h) => {
                e.insert(MeshMaterial3d(h.clone()));
            }
        }
    }

    /// As `apply`, for parts whose owner may go away meanwhile.
    pub fn try_apply(&self, e: &mut EntityCommands) {
        match self {
            PartMat::Std(h) => {
                e.try_insert(MeshMaterial3d(h.clone()));
            }
            PartMat::Ue3(h) => {
                e.try_insert(MeshMaterial3d(h.clone()));
            }
        }
    }
}

/// Material of particle sprites and meshes.
#[derive(Clone)]
pub enum ParticleMat {
    Std(Handle<StandardMaterial>),
    Ue3(Handle<Ue3Material>),
}

impl ParticleMat {
    pub fn apply(&self, e: &mut EntityCommands) {
        match self {
            ParticleMat::Std(h) => {
                e.insert(MeshMaterial3d(h.clone()));
            }
            ParticleMat::Ue3(h) => {
                e.insert(MeshMaterial3d(h.clone()));
            }
        }
    }

    /// The original shaders take the particle colour and alpha as the vertex colour.
    pub fn original(&self) -> bool {
        matches!(self, ParticleMat::Ue3(_))
    }
}

/// Renderable parts (mesh + material) of a character or prop.
#[derive(Clone, Default)]
pub struct VisualParts {
    /// in the world (original shaders where compiled)
    pub parts: Vec<(Handle<Mesh>, PartMat)>,
    /// each part's original material (the head's?, scene material)
    pub mats: Vec<(bool, u32)>,
    /// for the first-person view model (foreground: own field of view, never clipped)
    pub view_parts: Vec<(Handle<Mesh>, PartMat)>,
    /// each part's section of its mesh
    pub sections: Vec<usize>,
}

pub struct NpcVisual {
    pub kind: String,
    pub name: String,
    /// Corvo's arms before the Outsider's mark (player arms only)
    pub no_mark: Option<VisualParts>,
    pub skeleton: SkeletonDef,
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    pub parts: VisualParts,
    /// The pawn's original animations bound to this skeleton.
    pub anims: Option<std::sync::Arc<crate::anim::CharAnims>>,
    /// the meshes' material slots (scene materials)
    pub body_slots: Vec<u32>,
    pub head_slots: Vec<u32>,
    /// the lesser LODs (body and head of each rank) and their display factors
    /// (`LODInfo.DisplayFactor`), and the body's bounds radius (m)
    pub lods: Vec<(f32, VisualParts)>,
    pub radius: f32,
    /// the dismemberment LOD: its parts and each one's body part (owner bone, cut bone, shown
    /// once cut)
    pub gore: Option<(VisualParts, Vec<(String, String, bool)>)>,
}

/// Character and prop visuals built from the cooked data, for gameplay systems.
#[derive(Resource, Default)]
pub struct GameAssets {
    pub white_rat_material: Option<PartMat>,
    pub npc_types: Vec<Option<NpcVisual>>,
    /// materials the scripts put on characters, by scene material
    pub npc_mat_swaps: HashMap<u32, PartMat>,
    pub props: std::collections::HashMap<String, VisualParts>,
    /// sprite materials of the particle emitters (original shaders, or unlit
    /// approximations), by scene material id
    pub particle_mats: HashMap<u32, ParticleMat>,
    /// meshes of mesh-particle emitters, by scene mesh id
    pub particle_meshes: HashMap<u32, (Handle<Mesh>, ParticleMat)>,
    /// each river krust's stalk and pearl
    pub krust_parts: Vec<(Option<VisualParts>, Option<VisualParts>)>,
    /// the pieces broken props fall into, by mesh
    pub prop_chunks: HashMap<u32, VisualParts>,
    /// gameplay effects' textures by name (`scene.game_textures`)
    pub textures: HashMap<String, Handle<Image>>,
    /// the post-process graph's materials by name (`scene.post_materials`)
    pub post: HashMap<String, PostMat>,
}

/// A post-process material as the graph runs it: its shader map, parameter values (and
/// names) and textures (`Texture2D_N`).
#[derive(Clone)]
pub struct PostMat {
    pub map: u32,
    pub params: Vec<[f32; 4]>,
    pub param_names: Vec<String>,
    pub textures: Vec<Option<Handle<Image>>>,
    /// its parameters' curves over time (a `MaterialInstanceTimeVarying`)
    pub curves: Vec<dhcook::format::PostCurve>,
}

struct MatBuilder<'a> {
    defs: &'a [MaterialDef],
    textures: &'a [Option<TexFile>],
    images: HashMap<(u32, bool), Handle<Image>>,
    mats: HashMap<u32, Option<MatHandle>>,
    black: Handle<Image>,
    white: Handle<Image>,
    ext: LightmapExt,
    /// bindings shared by every original-shader material (textures filled per material)
    ue_base: Option<Ue3Material>,
    ue_mats: HashMap<(u32, Policy), Option<Handle<Ue3Material>>>,
    /// the light passes' materials, one per surface material and light (its parameters' and
    /// shadow's bits): the passes of a light over surfaces of one material share it, and draw
    /// together
    light_mats: HashMap<(u32, Policy, [u32; 16], Option<[u32; 5]>), Option<Handle<Ue3Material>>>,
    cube_faces: &'a [[u32; 6]],
    cubes: HashMap<u32, Option<Handle<Image>>>,
    /// render-target textures: the images the reflections draw into
    render_targets: HashMap<u32, Handle<Image>>,
}

impl MatBuilder<'_> {
    fn image(&mut self, id: Option<u32>, srgb: bool, assets: &mut Assets<Image>) -> Option<Handle<Image>> {
        let id = id?;
        if let Some(h) = self.render_targets.get(&id) {
            return Some(h.clone());
        }
        if let Some(h) = self.images.get(&(id, srgb)) {
            return Some(h.clone());
        }
        let tf = self.textures.get(id as usize)?.as_ref()?;
        let h = assets.add(make_image(tf, srgb));
        self.images.insert((id, srgb), h.clone());
        Some(h)
    }

    /// The material rendered with its original shaders for a light-map policy, if it has a
    /// compiled program.
    fn ue3_material(
        &mut self,
        id: u32,
        policy: Policy,
        images: &mut Assets<Image>,
        materials: &mut Assets<Ue3Material>,
        programs: &mut Ue3Programs,
        shaders: &mut Assets<Shader>,
    ) -> Option<Handle<Ue3Material>> {
        if let Some(m) = self.ue_mats.get(&(id, policy)) {
            return m.clone();
        }
        let r = self.build_ue3_material(id, policy, images, materials, programs, shaders);
        self.ue_mats.insert((id, policy), r.clone());
        r
    }

    /// A dynamic light's pass over a lit surface: the material's light shaders with the
    /// light's parameters (UE space) in its last uniform slots, added over the surface.
    #[allow(clippy::too_many_arguments)]
    fn ue3_light_material(
        &mut self,
        id: u32,
        policy: Policy,
        light: [Vec4; 4],
        shadow: Option<(Vec4, u32)>,
        images: &mut Assets<Image>,
        materials: &mut Assets<Ue3Material>,
        programs: &mut Ue3Programs,
        shaders: &mut Assets<Shader>,
    ) -> Option<Handle<Ue3Material>> {
        let bits = |v: &[Vec4]| -> Vec<u32> { v.iter().flat_map(|q| q.to_array()).map(f32::to_bits).collect() };
        let key = (
            id,
            policy,
            <[u32; 16]>::try_from(bits(&light)).unwrap_or_default(),
            shadow.map(|(sb, layer)| {
                let b = bits(&[sb]);
                [b[0], b[1], b[2], b[3], layer]
            }),
        );
        if let Some(m) = self.light_mats.get(&key) {
            return m.clone();
        }
        let group = self.light_mats.len() as u32;
        let m = self.build_ue3_light_material(id, policy, light, shadow, group, images, materials, programs, shaders);
        self.light_mats.insert(key, m.clone());
        m
    }

    #[allow(clippy::too_many_arguments)]
    fn build_ue3_light_material(
        &mut self,
        id: u32,
        policy: Policy,
        light: [Vec4; 4],
        shadow: Option<(Vec4, u32)>,
        group: u32,
        images: &mut Assets<Image>,
        materials: &mut Assets<Ue3Material>,
        programs: &mut Ue3Programs,
        shaders: &mut Assets<Shader>,
    ) -> Option<Handle<Ue3Material>> {
        let ue = self.defs.get(id as usize)?.ue3.as_ref()?;
        if ue.unlit || !matches!(ue.blend, Blend::Opaque | Blend::Masked) || ue.params.len() > dhcook::ue3prog::DYN_SHADOW_SLOT {
            return None;
        }
        let base = self.ue3_material(id, policy, images, materials, programs, shaders)?;
        let mut m = materials.get(&base)?.clone();
        for (k, v) in light.iter().enumerate() {
            m.params.p[dhcook::ue3prog::DYN_LIGHT_SLOT + k] = *v;
        }
        if let Some((scale_bias, layer)) = shadow {
            m.params.p[dhcook::ue3prog::DYN_SHADOW_SLOT] = scale_bias;
            m.params.p[dhcook::ue3prog::DYN_SHADOW_SLOT + 1] = Vec4::new(layer as f32, 0.0, 0.0, 0.0);
        }
        m.key.blend = 3;
        m.key.light_pass = true;
        m.light_group = group;
        Some(materials.add(m))
    }

    fn build_ue3_material(
        &mut self,
        id: u32,
        policy: Policy,
        images: &mut Assets<Image>,
        materials: &mut Assets<Ue3Material>,
        programs: &mut Ue3Programs,
        shaders: &mut Assets<Shader>,
    ) -> Option<Handle<Ue3Material>> {
        let def = self.defs.get(id as usize)?.clone();
        if def.invisible {
            return None;
        }
        let ue = def.ue3.as_ref()?;
        let mut m = self.ue_base.clone()?;
        let lit = !ue.unlit && matches!(ue.blend, Blend::Opaque | Blend::Masked) && !matches!(policy, Policy::PointLight | Policy::SpotLight | Policy::PointLightShadowed | Policy::SpotLightShadowed);
        let program = programs.get(ue.map, policy, lit, shaders)?;
        m.key = Ue3Key::new(program, ue.blend, ue.two_sided);
        m.map = ue.map;
        m.mat_id = id;
        m.light_ok = !ue.unlit && matches!(ue.blend, Blend::Opaque | Blend::Masked) && ue.params.len() <= dhcook::ue3prog::DYN_SHADOW_SLOT;
        let mut params = Ue3Params::default();
        for (i, v) in ue.params.iter().take(params.p.len()).enumerate() {
            params.p[i] = Vec4::from(*v);
        }
        m.params = params;
        for (i, t) in ue.textures.iter().enumerate() {
            if *t == Some(dhcook::format::TEX_BLACK) {
                m.set_texture(i, m.black.clone());
                continue;
            }
            let srgb = t.and_then(|t| self.textures.get(t as usize)).and_then(|t| t.as_ref()).map(|t| t.srgb).unwrap_or(true);
            if let Some(h) = self.image(*t, srgb, images) {
                m.set_texture(i, h);
            }
        }
        if let Some(Some(c)) = ue.cube_textures.first() {
            if let Some(h) = self.cube(*c, images) {
                m.cube0 = h;
            }
        }
        Some(materials.add(m))
    }

    /// A cube map from its six cooked faces.
    fn cube(&mut self, id: u32, images: &mut Assets<Image>) -> Option<Handle<Image>> {
        use bevy::render::render_resource::{TextureViewDescriptor, TextureViewDimension};
        if let Some(h) = self.cubes.get(&id) {
            return h.clone();
        }
        let faces = self.cube_faces.get(id as usize)?;
        let files: Vec<&TexFile> = faces.iter().filter_map(|&f| self.textures.get(f as usize).and_then(|t| t.as_ref())).collect();
        let ok = files.len() == 6 && files.iter().all(|f| f.width == files[0].width && f.height == files[0].height && f.format == files[0].format && f.mips.len() == files[0].mips.len());
        let h = ok.then(|| {
            let mut img = make_image(files[0], files[0].srgb);
            let mut data = Vec::new();
            for f in &files {
                for m in &f.mips {
                    data.extend_from_slice(m);
                }
            }
            img.data = Some(data);
            img.texture_descriptor.size.depth_or_array_layers = 6;
            img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
            images.add(img)
        });
        if h.is_none() {
            warn!("cube map {id}: faces don't match");
        }
        self.cubes.insert(id, h.clone());
        h
    }

    fn material(
        &mut self,
        id: u32,
        images: &mut Assets<Image>,
        materials: &mut Assets<WorldMaterial>,
        skies: &mut Assets<SkyMaterial>,
    ) -> Option<MatHandle> {
        if let Some(m) = self.mats.get(&id) {
            return m.clone();
        }
        let def = self.defs.get(id as usize).cloned().unwrap_or_default();
        let hide_blend = std::env::var("DH_HIDE_TRANSLUCENT").is_ok()
            && matches!(def.blend, Blend::Translucent | Blend::Additive | Blend::Modulate);
        let h = if def.invisible || hide_blend {
            None
        } else if let Some(sky) = &def.sky {
            let v3 = |c: [f32; 3], w: f32| Vec4::new(c[0], c[1], c[2], w);
            let clouds = self.image(sky.clouds, false, images).unwrap_or(self.black.clone());
            let clouds2 = self.image(sky.clouds2, false, images).unwrap_or(self.black.clone());
            let storm = self.image(sky.storm, true, images);
            Some(MatHandle::Sky(skies.add(SkyMaterial {
                params: SkyParams {
                    top: v3(sky.top, 1.0),
                    bottom: v3(sky.bottom, 1.0),
                    horizon: v3(sky.horizon, sky.horizon_intensity),
                    clouds_color: Vec4::from(sky.clouds_color),
                    clouds2_color: Vec4::from(sky.clouds2_color),
                    storm_color: v3(sky.storm_color, sky.storm_intensity),
                    p0: Vec4::new(sky.horizon_exponent, sky.gradient_power, sky.clouds_visibility, sky.clouds2_visibility),
                    p1: Vec4::new(sky.clouds_speed, sky.clouds2_speed, if storm.is_some() { 1.0 } else { 0.0 }, 0.9),
                },
                clouds,
                clouds2,
                storm: storm.unwrap_or(self.black.clone()),
            })))
        } else if def.water {
            // "*_under" variants are the same surface seen from below the waterline.
            let under = def.name.to_ascii_lowercase().contains("under");
            let opaque = def.blend == Blend::Opaque;
            Some(MatHandle::Std(materials.add(WorldMaterial {
                base: StandardMaterial {
                    base_color: Color::srgba(0.025, 0.04, 0.045, if opaque { 1.0 } else { 0.9 }),
                    perceptual_roughness: 0.18,
                    reflectance: 0.25,
                    alpha_mode: if opaque { AlphaMode::Opaque } else { AlphaMode::Blend },
                    double_sided: def.two_sided || under,
                    cull_mode: if under {
                        Some(bevy::render::render_resource::Face::Front)
                    } else if def.two_sided {
                        None
                    } else {
                        Some(bevy::render::render_resource::Face::Back)
                    },
                    ..default()
                },
                extension: self.ext.clone(),
            })))
        } else {
            let base = self.image(def.diffuse, true, images);
            let normal = self.image(def.normal, false, images);
            let emissive_tex = self.image(def.emissive, true, images);
            let alpha_mode = match def.blend {
                Blend::Opaque => AlphaMode::Opaque,
                Blend::Masked => AlphaMode::Mask(def.alpha_cutoff.clamp(0.05, 0.95)),
                Blend::Translucent => AlphaMode::Blend,
                Blend::Additive => AlphaMode::Add,
                Blend::Modulate => AlphaMode::Multiply,
            };
            let t = def.tint;
            let alpha = if def.blend == Blend::Translucent { def.opacity.unwrap_or(1.0) } else { 1.0 };
            let emissive = if emissive_tex.is_some() {
                LinearRgba::rgb(def.emissive_color[0], def.emissive_color[1], def.emissive_color[2]) * 4.0
            } else {
                LinearRgba::BLACK
            };
            let unlit = def.unlit || matches!(def.blend, Blend::Additive);
            let valid = |v: [f32; 2]| Some(Vec2::from(v)).filter(|v| v.min_element() > 1e-3).unwrap_or(Vec2::ONE);
            let (uv, nuv) = (valid(def.uv_scale), valid(def.normal_uv_scale));
            // Opaque generic_PMAT materials whose diffuse tiles differently from the normal
            // map, or that carry a variation layer, take the layered shader path.
            let layered = def.blend == Blend::Opaque && !unlit && base.is_some() && (def.layer.is_some() || uv != nuv);
            let mut extension = self.ext.clone();
            if layered {
                let layer = def.layer.clone().unwrap_or_default();
                let var_tex = self.image(layer.texture, true, images);
                let mask_tex = self.image(layer.mask, false, images);
                let mut channel = Vec4::ZERO;
                channel[layer.mask_channel.min(3) as usize] = 1.0;
                extension.layer = LayerParams {
                    diffuse: Vec4::new(uv.x, uv.y, 1.0, if layer.multiply { 1.0 } else { 0.0 }),
                    color: if def.layer.is_some() { Vec4::from(layer.color) } else { Vec4::ONE },
                    tiling: Vec4::new(layer.tiling.max(1e-3), layer.mask_tiling.max(1e-3), layer.mask_power.max(1e-3), if var_tex.is_some() { 1.0 } else { 0.0 }),
                    channel: if def.layer.is_some() && mask_tex.is_some() { channel } else { Vec4::ZERO },
                    tint: Vec4::new(t[0], t[1], t[2], 1.0),
                };
                extension.diffuse = base.clone().unwrap_or(self.white.clone());
                extension.variation = var_tex.unwrap_or(self.white.clone());
                extension.layer_mask = mask_tex.unwrap_or(self.black.clone());
            }
            Some(MatHandle::Std(materials.add(WorldMaterial { extension, base: StandardMaterial {
                base_color: Color::linear_rgba(t[0], t[1], t[2], alpha),
                base_color_texture: base,
                normal_map_texture: normal,
                emissive,
                emissive_texture: emissive_tex,
                perceptual_roughness: 0.75,
                metallic: 0.0,
                reflectance: 0.35,
                alpha_mode,
                uv_transform: bevy::math::Affine2::from_scale(if layered { nuv } else { uv }),
                double_sided: def.two_sided,
                cull_mode: if def.two_sided || unlit { None } else { Some(bevy::render::render_resource::Face::Back) },
                unlit,
                fog_enabled: !unlit,
                ..default()
            } })))
        };
        self.mats.insert(id, h.clone());
        h
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_level(
    mut commands: Commands,
    mut pending: ResMut<PendingLevel>,
    mut mesh_assets: ResMut<Assets<Mesh>>,
    mut image_assets: ResMut<Assets<Image>>,
    mut mat_assets: ResMut<Assets<WorldMaterial>>,
    mut sky_assets: ResMut<Assets<SkyMaterial>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut ibp_assets: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut std_assets: ResMut<Assets<StandardMaterial>>,
    tuning: Res<LightTuning>,
    (mut ue_assets, mut ue_programs, mut shader_assets): (ResMut<Assets<Ue3Material>>, ResMut<Ue3Programs>, ResMut<Assets<Shader>>),
    (campaign, campaign_script): (Res<crate::gameplay::Campaign>, Res<crate::campaign::CampaignScript>),
) {
    let Some(level) = pending.0.take() else {
        error!("no pending level");
        return;
    };
    let t0 = std::time::Instant::now();
    let level = *level;
    let scene = level.scene;
    // DH_SPAWN_PROFILE: how long each part of the spawn took
    let prof = std::env::var("DH_SPAWN_PROFILE").is_ok();
    let lap = |what: &str| {
        if prof {
            info!("spawn: {what} at {:.2}s", t0.elapsed().as_secs_f32());
        }
    };
    // Lightmap / shadow pages -> texture array layers; per-instance data -> storage buffer.
    let use_baked = std::env::var("DH_NO_LIGHTMAPS").is_err();
    let mut page_layers: Vec<u32> = Vec::new();
    let mut shadow_layers: Vec<u32> = Vec::new();
    let layer_of = |list: &mut Vec<u32>, id: u32| -> u32 {
        match list.iter().position(|&p| p == id) {
            Some(l) => l as u32,
            None => {
                list.push(id);
                list.len() as u32 - 1
            }
        }
    };
    let tex_ok = |id: u32| level.textures.get(id as usize).and_then(|t| t.as_ref()).is_some();
    // entry 0: no lightmap, fully sunlit (used by anything without baked data)
    let mut entries: Vec<[f32; 4]> = entry([0.0; 4], [0.0; 4], 0, 0, false, 2).to_vec();
    let mut tags: Vec<u32> = vec![0; scene.instances.len()];
    let mut sun_probes: Vec<SunProbe> = Vec::new();
    let mut light_shadow_maps: HashMap<(u32, u32), (u32, Vec4)> = HashMap::new();
    // original light-map pages (colour, directional) of each irradiance page
    let mut raw_pages: HashMap<u32, (u32, u32)> = HashMap::new();
    if use_baked {
        for (i, inst) in scene.instances.iter().enumerate() {
            let mut lm_rect = [0.0; 4];
            let mut lm_layer = 0;
            let mut has_lm = false;
            if let Some(lm) = &inst.lightmap {
                if let Some(&page) = lm.textures.first().filter(|&&p| tex_ok(p)) {
                    lm_layer = layer_of(&mut page_layers, page);
                    lm_rect = [lm.coord_bias[0], lm.coord_bias[1], lm.coord_scale[0], lm.coord_scale[1]];
                    has_lm = true;
                    if let (Some(&nac), Some(&dmc)) = (lm.textures.get(1), lm.textures.get(2)) {
                        raw_pages.entry(page).or_insert((nac, dmc));
                    }
                }
            }
            // the dominant lights' shadow maps: page layer and UE `LightmapCoordinateScaleBias`
            for (li, sh) in &inst.light_shadows {
                if let Some(page) = sh.textures.first().copied().filter(|&p| tex_ok(p)) {
                    let layer = layer_of(&mut shadow_layers, page);
                    light_shadow_maps.insert((i as u32, *li), (layer, Vec4::new(sh.coord_scale[0], sh.coord_scale[1], sh.coord_bias[1], sh.coord_bias[0])));
                }
            }
            let mut sh_rect = [0.0; 4];
            let mut sh_layer = 0;
            let mut sun = inst.sun;
            if sun == 1 {
                match inst.sun_shadow.as_ref().and_then(|sh| sh.textures.first().copied().filter(|&p| tex_ok(p)).map(|p| (p, sh))) {
                    Some((page, sh)) => {
                        sh_layer = layer_of(&mut shadow_layers, page);
                        sh_rect = [sh.coord_bias[0], sh.coord_bias[1], sh.coord_scale[0], sh.coord_scale[1]];
                    }
                    None => sun = 2,
                }
            }
            // instances without baked lighting: light environment (and sun visibility)
            let probe = !has_lm;
            tags[i] = (entries.len() / 3) as u32;
            if probe {
                let m = Mat4::from_cols_array(&inst.transform);
                let me = &scene.meshes[inst.mesh as usize];
                let center = m.transform_point3((Vec3::from(me.min) + Vec3::from(me.max)) * 0.5);
                let top = m.transform_point3(Vec3::new((me.min[0] + me.max[0]) * 0.5, me.max[1], (me.min[2] + me.max[2]) * 0.5));
                sun_probes.push(SunProbe { entry: tags[i], points: [center, top], sun: sun == 2 });
            }
            entries.extend_from_slice(&entry(lm_rect, sh_rect, lm_layer, sh_layer, has_lm, sun));
        }
    }
    let page_files: Vec<&TexFile> = page_layers.iter().filter_map(|&p| level.textures[p as usize].as_ref()).collect();
    let shadow_files: Vec<&TexFile> = shadow_layers.iter().filter_map(|&p| level.textures[p as usize].as_ref()).collect();
    lap("pages");
    let baked = !page_files.is_empty();
    // The dominant directional light is evaluated in the world shader with its static shadow maps.
    let sun_light = scene.lights.iter().find(|l| l.kind == LightKind::Directional && l.enabled && l.dynamic);
    let params = match (baked, sun_light) {
        (true, Some(l)) => LmParams {
            misc: Vec4::new(tuning.lightmap_exposure, tuning.lightmap_floor, 1.0, 0.0),
            sun_dir: Vec3::from(l.direction).normalize_or(Vec3::NEG_Y).extend(0.0),
            sun_color: (Vec3::from(l.color) * l.brightness * tuning.sun_scale).extend(0.0),
        },
        _ => LmParams { misc: Vec4::new(tuning.lightmap_exposure, tuning.lightmap_floor, 0.0, 0.0), ..default() },
    };
    // dynamic actor slots (characters, held items) live at the end of the buffer
    const DYN_SLOTS: u32 = 128;
    let dyn_base = (entries.len() / 3) as u32;
    for _ in 0..DYN_SLOTS {
        entries.extend_from_slice(&entry([0.0; 4], [0.0; 4], 0, 0, false, 0));
    }
    let entries_handle = buffers.add(ShaderBuffer::from(entries.clone()));
    // the original shaders' per-instance data, at the same tags
    let ue_ambient: f32 = std::env::var("DH_UE3_AMBIENT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.05);
    let ue_entries = {
        let n = entries.len() / 3;
        let mut v = vec![[0.0f32; 4]; n * INSTANCE_STRIDE as usize];
        for (i, inst) in scene.instances.iter().enumerate() {
            let tag = tags[i] as usize;
            if tag == 0 {
                continue;
            }
            let e = &entries[tag * 3..tag * 3 + 3];
            let o = tag * INSTANCE_STRIDE as usize;
            // light-map coordinate scale.xy, bias.xy
            v[o] = [e[0][2], e[0][3], e[0][0], e[0][1]];
            if let Some(lm) = &inst.lightmap {
                v[o + 1] = lm.scales.first().copied().unwrap_or([1.0; 4]);
                v[o + 2] = lm.scales.get(1).copied().unwrap_or([1.0; 4]);
            }
            v[o + 3] = e[2];
            v[o + 4] = [e[1][2], e[1][3], e[1][0], e[1][1]];
            if inst.lightmap.is_none() {
                v[o + 5] = [ue_ambient, ue_ambient, ue_ambient, 1.0];
            }
        }
        // entry 0 as an instance (particles): a soft ambient for lit sprites
        let pa: f32 = std::env::var("DH_UE3_PARTICLE_AMBIENT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.3);
        v[5] = [pa, pa, pa, 1.0];
        // entry 0: the dominant directional light (UE space, towards the light)
        if let Some(l) = scene.lights.iter().find(|l| l.kind == LightKind::Directional && l.enabled && l.dynamic) {
            let d = Vec3::from(l.direction).normalize_or(Vec3::NEG_Y);
            v[0] = [-d.x, -d.z, -d.y, 0.0];
            let c = Vec3::from(l.color) * l.brightness.max(0.0);
            v[1] = [c.x, c.y, c.z, 0.0];
        }
        v
    };
    lap("entries");
    let ue_entries_handle = buffers.add(ShaderBuffer::from(ue_entries.clone()));
    let env_lights: Vec<EnvLight> = scene
        .lights
        .iter()
        .filter(|l| l.enabled && matches!(l.kind, LightKind::Point | LightKind::Spot))
        .map(|l| EnvLight {
            pos: Vec3::from(l.position),
            radius: l.radius.max(0.5),
            color: Vec3::from(l.color) * l.brightness.max(0.0),
            spot: (l.kind == LightKind::Spot).then(|| (Vec3::from(l.direction).normalize_or(Vec3::NEG_Y), l.outer_cone.clamp(1.0, 89.0).to_radians().cos())),
        })
        .collect();
    commands.insert_resource(WorldLighting {
        buffer: entries_handle.clone(),
        entries: entries.clone(),
        dyn_base,
        dyn_cap: DYN_SLOTS,
        dyn_next: 0,
        lights: env_lights,
        sun_dir: params.sun_dir.truncate(),
        sun_enabled: params.misc.z > 0.0,
        sun_color: scene
            .lights
            .iter()
            .find(|l| l.kind == LightKind::Directional && l.enabled && l.dynamic)
            .map(|l| Vec3::from(l.color) * l.brightness.max(0.0))
            .unwrap_or(Vec3::ZERO),
        dirty: false,
        timer: 0.0,
        ambient: tuning.actor_ambient,
        ue_buffer: Some(ue_entries_handle.clone()),
        ue_entries,
        volume: crate::world_light::LightVolume::new(level.light_volume.clone()),
    });
    lap("world lighting");
    if !sun_probes.is_empty() {
        commands.insert_resource(PendingSunProbes { probes: std::mem::take(&mut sun_probes), frames: 0 });
    }
    let white = image_assets.add(Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    ));
    let black = image_assets.add(Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    ));
    let ext = LightmapExt {
        entries: entries_handle,
        pages: image_assets.add(make_page_array(&page_files)),
        params,
        shadow_pages: image_assets.add(make_page_array(&shadow_files)),
        layer: LayerParams::default(),
        diffuse: white.clone(),
        variation: white.clone(),
        layer_mask: black.clone(),
    };
    lap("images");
    let ue_base = (crate::ue3mat::enabled() && crate::ue3mat::library().is_some()).then(|| {
        let raw = |k: usize| -> Vec<&TexFile> {
            page_layers
                .iter()
                .filter_map(|p| raw_pages.get(p))
                .filter_map(|r| level.textures[if k == 0 { r.0 } else { r.1 } as usize].as_ref())
                .collect()
        };
        let (nac, dmc) = (raw(0), raw(1));
        let complete = nac.len() == page_layers.len() && dmc.len() == page_layers.len();
        if !complete {
            warn!("original light-map pages missing ({} / {} / {}): recook the map", nac.len(), dmc.len(), page_layers.len());
        }
        let black_cube = image_assets.add(black_cube_image());
        Ue3Material {
            params: Ue3Params::default(),
            t0: white.clone(),
            t1: white.clone(),
            t2: white.clone(),
            t3: white.clone(),
            t4: white.clone(),
            t5: white.clone(),
            t6: white.clone(),
            t7: white.clone(),
            t8: white.clone(),
            t9: white.clone(),
            t10: white.clone(),
            t11: white.clone(),
            cube0: black_cube.clone(),
            instances: ue_entries_handle.clone(),
            lm0: image_assets.add(make_page_array(if complete { &nac } else { &[] })),
            lm1: image_assets.add(make_page_array(if complete { &dmc } else { &[] })),
            shadow: ext.shadow_pages.clone(),
            white: white.clone(),
            black: black.clone(),
            refl: black_cube.clone(),
            black_cube,
            key: Ue3Key::new(0, Blend::Opaque, false),
            map: 0,
            mat_id: u32::MAX,
            light_ok: false,
            light_group: 0,
        }
    });
    let mut mb = MatBuilder {
        defs: &scene.materials,
        textures: &level.textures,
        images: HashMap::new(),
        mats: HashMap::new(),
        black,
        white,
        ext,
        ue_base,
        ue_mats: HashMap::new(),
        light_mats: HashMap::new(),
        cube_faces: &scene.cubes,
        cubes: HashMap::new(),
        render_targets: {
            let rt = crate::reflections::make_targets(&scene, &mut image_assets);
            commands.insert_resource(crate::reflections::ReflectionTargets(rt.clone()));
            rt
        },
    };
    // the meshes the reflections show (their groups: `ReflectionChannels`)
    let reflect_groups = crate::reflections::shown_groups(&scene);
    lap("lighting buffers");
    // the level's reflection cube, for every surface
    if let Some(h) = scene.scene_reflection.and_then(|c| mb.cube(c, &mut image_assets)) {
        if let Some(b) = mb.ue_base.as_mut() {
            b.refl = h;
        }
    }
    let mut mesh_cache: HashMap<(u32, usize, bool, bool), Handle<Mesh>> = HashMap::new();
    let mut n_ue3 = 0usize;
    let mut n_entities = 0usize;
    let mut n_lightmapped = 0usize;
    let mut n_colliders = 0usize;

    lap("reflection");
    // dynamic lights over the original shaders: each light's own pass over the lit surfaces
    // it reaches (`DH_NO_DYN_LIGHTS` turns them off)
    let baked_world = !page_layers.is_empty();
    let dyn_lights: Vec<(usize, Policy, Vec3, f32, [Vec4; 4])> = if std::env::var("DH_NO_DYN_LIGHTS").is_ok() {
        Vec::new()
    } else {
        scene
            .lights
            .iter()
            .enumerate()
            .filter(|(_, l)| l.enabled && (l.dynamic || !baked_world) && matches!(l.kind, LightKind::Point | LightKind::Spot) && l.brightness > 0.0 && l.radius > 0.1)

            .map(|(li, l)| {
                let p = Vec3::from(l.position);
                let ue = |v: Vec3| Vec3::new(v.x, v.z, v.y);
                let c = Vec3::from(l.color) * l.brightness;
                let d = ue(Vec3::from(l.direction).normalize_or(Vec3::NEG_Y));
                let outer = l.outer_cone.clamp(1.0, 89.0).to_radians().cos();
                let inner = l.inner_cone.clamp(0.0, l.outer_cone.clamp(1.0, 89.0)).to_radians().cos();
                let params = [
                    (ue(p) * 100.0).extend(1.0 / (l.radius * 100.0)),
                    c.extend(if l.falloff > 0.0 { l.falloff } else { 2.0 }),
                    Vec4::new(outer, 1.0 / (inner - outer).max(1e-3), 0.0, 0.0),
                    d.extend(0.0),
                ];
                (li, if l.kind == LightKind::Spot { Policy::SpotLight } else { Policy::PointLight }, p, l.radius, params)
            })
            .collect()
    };
    let mut n_light_passes = 0usize;
    let mut lit_surfaces = Vec::new();
    let loaded_levels = crate::campaign::initial_levels(&scene, &campaign, &campaign_script);
    // instances the level scripts refer to exist even when hidden (scripts may show them)
    let scripted: std::collections::HashSet<u32> = scene.kismet.actors.iter().flat_map(|a| a.instances.iter().copied()).collect();
    // what the scripts' `SeqAct_SetMaterial` put on which instances: instance -> (material, section)
    let swap_targets: HashMap<u32, Vec<(u32, usize)>> = {
        let k = &scene.kismet;
        let mut out: HashMap<u32, Vec<(u32, usize)>> = HashMap::new();
        for op in k.ops.iter().filter(|o| o.class == "SeqAct_SetMaterial") {
            let Some(dhcook::format::KVal::Int(id)) = op.props.get("material_id") else { continue };
            let slot = match op.props.get("MaterialIndex") {
                Some(dhcook::format::KVal::Int(n)) => (*n).max(0) as usize,
                _ => 0,
            };
            for l in op.vars.iter().filter(|l| l.desc == "Target") {
                for &v in &l.vars {
                    if let Some(dhcook::format::KVal::Actor(a)) = k.vars.get(v as usize).and_then(|v| v.props.get("ObjValue")) {
                        for &inst in k.actors.get(*a as usize).map(|a| a.instances.as_slice()).unwrap_or(&[]) {
                            out.entry(inst).or_default().push((*id as u32, slot));
                        }
                    }
                }
            }
        }
        out
    };
    for (i, inst) in scene.instances.iter().enumerate() {
        let m = Mat4::from_cols_array(&inst.transform);
        if !m.is_finite() {
            continue;
        }
        let mirrored = Mat3::from_mat4(m).determinant() < 0.0;
        let transform = Transform::from_matrix(m);
        let Some(Some(prepared)) = level.meshes.get(inst.mesh as usize) else { continue };

        let mut parts: Vec<(Handle<Mesh>, MatHandle, bool)> = Vec::new();
        // materials the scripts may put on its surfaces (`SeqAct_SetMaterial`): material, part
        let mut swaps: Vec<(u32, usize, Handle<Ue3Material>)> = Vec::new();
        // the surfaces drawn with the original shaders (for the dynamic lights' passes)
        let mut lit_parts: Vec<(Handle<Mesh>, u32, Handle<Ue3Material>)> = Vec::new();
        // streamed sublevels not loaded wait, hidden (their colliders off)
        let streamed_out = crate::campaign::instance_unloaded(&scene, &loaded_levels, i as u32);
        // (and the scripts' factory items, waiting for them)
        let hidden_scripted = !inst.visible && (scripted.contains(&(i as u32)) || inst.class == "DisFactoryItem") || streamed_out && inst.visible;
        if inst.visible || hidden_scripted {
            for (si, sec) in prepared.sections.iter().enumerate() {
                let Some(sec) = sec else { continue };
                let mat_id = *inst.materials.get(si).unwrap_or(&0);
                // original shaders: light-mapped surfaces, and unlit / translucent ones
                // (sections without their own light-map coordinates use UV0, as UE3 binds the
                // last UV channel when the light-map index is out of range)
                let lightmapped = tags[i] != 0 && inst.lightmap.as_ref().is_some_and(|l| l.textures.len() >= 3);
                let ue = scene.materials.get(mat_id as usize).and_then(|d| d.ue3.as_ref());
                let ue_policy = match ue {
                    Some(_) if lightmapped => Some(Policy::DirectionalLightMap),
                    // unlit and translucent surfaces, and lit ones without baked lighting
                    // (dominant light pass plus ambient)
                    Some(_) => Some(Policy::NoLightMap),
                    _ => None,
                };
                let ue_mat = ue_policy.and_then(|p| mb.ue3_material(mat_id, p, &mut image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets));
                let full = ue_mat.is_some();
                let base = ue_mat.clone();
                let mat = match ue_mat {
                    Some(h) => {
                        n_ue3 += 1;
                        MatHandle::Ue3(h)
                    }
                    None => {
                        let Some(mat) = mb.material(mat_id, &mut image_assets, &mut mat_assets, &mut sky_assets) else { continue };
                        mat
                    }
                };
                let mh = mesh_cache
                    .entry((inst.mesh, si, mirrored, full))
                    .or_insert_with(|| mesh_assets.add(if full { make_mesh_full(sec, mirrored) } else { make_mesh(sec, mirrored) }))
                    .clone();
                if let Some(base) = base {
                    lit_parts.push((mh.clone(), mat_id, base));
                }
                // the scripts' replacements, made as this surface's own
                if let (Some(policy), Some(news)) = (ue_policy, swap_targets.get(&(i as u32))) {
                    for &(new_id, slot) in news {
                        if slot == si {
                            if let Some(h) = mb.ue3_material(new_id, policy, &mut image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets) {
                                swaps.push((new_id, parts.len(), h));
                            }
                        }
                    }
                }
                parts.push((mh, mat, full || (sec.uv1.len() == sec.positions.len() && !sec.uv1.is_empty())));
            }
        }
        let collider = level.colliders.get(i).cloned().flatten();
        if parts.is_empty() && collider.is_none() {
            continue;
        }
        // a wall of light's eye: its looks (neutral, threat, friend, unpowered)
        // (sublevels may reuse an eye's name: its wall is the nearest of that name)
        let eye_looks = (inst.class == "DisDetectionEye" && parts.len() == 1)
            .then(|| {
                scene
                    .security
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| !d.eye.is_empty() && d.eye == inst.actor)
                    .min_by(|a, b| Vec3::from(a.1.position).distance(transform.translation).total_cmp(&Vec3::from(b.1.position).distance(transform.translation)))
            })
            .flatten()
            .map(|(di, d)| (di, d.eye_materials.map(|m| (m != 0).then(|| mb.ue3_material(m, Policy::NoLightMap, &mut image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets)).flatten())));
        let mut e = commands.spawn((
            LevelInstance { class: inst.class.clone(), actor: inst.actor.clone(), index: i as u32 },
            transform,
            if hidden_scripted { Visibility::Hidden } else { Visibility::default() },
            DespawnOnExit(GameState::InGame),
        ));
        let cast = inst.cast_shadow;
        let tag = tags[i];
        if inst.lightmap.is_some() && tag != 0 {
            n_lightmapped += 1;
        }
        let mut part_entities: Vec<Entity> = Vec::new();
        if parts.len() == 1 {
            let (mh, mat, uv1) = parts.pop().unwrap();
            insert_mesh(&mut e, mh, mat, cast, if uv1 { tag } else { 0 });
            part_entities.push(e.id());
        } else {
            e.with_children(|p| {
                for (mh, mat, uv1) in parts {
                    let mut c = p.spawn_empty();
                    insert_mesh(&mut c, mh, mat, cast, if uv1 { tag } else { 0 });
                    part_entities.push(c.id());
                }
            });
        }
        let inst_entity = e.id();
        if inst.reflect & reflect_groups != 0 {
            for pe in &part_entities {
                commands.entity(*pe).insert(bevy::camera::visibility::RenderLayers::from_layers(&[0, crate::reflections::REFLECT_LAYER]));
            }
        }
        if !swaps.is_empty() {
            let list = swaps.into_iter().filter_map(|(id, part, h)| part_entities.get(part).map(|pe| (id, *pe, h))).collect();
            commands.entity(inst_entity).insert(MatSwaps(list));
        }
        n_entities += 1;
        if let Some((device, looks)) = eye_looks {
            commands.entity(inst_entity).insert(crate::security::EyeLooks { device, looks, shown: usize::MAX });
        }
        // its surfaces, for the gameplay lights
        if !lit_parts.is_empty() {
            if let Some(mr) = scene.meshes.get(inst.mesh as usize) {
                let (lo, hi) = (Vec3::from(mr.min), Vec3::from(mr.max));
                lit_surfaces.push(crate::fxlight::LitSurface {
                    entity: inst_entity,
                    center: m.transform_point3((lo + hi) * 0.5),
                    reach: (m.transform_vector3((hi - lo) * 0.5)).length(),
                    tag: tags[i],
                    parts: lit_parts.clone(),
                });
            }
        }
        // the dynamic lights reaching it
        if !lit_parts.is_empty() && !dyn_lights.is_empty() {
            if let Some(mr) = scene.meshes.get(inst.mesh as usize) {
                let (lo, hi) = (Vec3::from(mr.min), Vec3::from(mr.max));
                let center = m.transform_point3((lo + hi) * 0.5);
                let reach = (m.transform_vector3((hi - lo) * 0.5)).length();
                for (li, policy, lp, radius, params) in &dyn_lights {
                    if center.distance(*lp) > radius + reach || inst.irrelevant_lights.contains(&(*li as u32)) {
                        continue;
                    }
                    // a spot only reaches what is inside its cone
                    if *policy == Policy::SpotLight {
                        let l = &scene.lights[*li];
                        let to = center - *lp;
                        let d = to.length();
                        if d > reach {
                            let axis = Vec3::from(l.direction).normalize_or(Vec3::NEG_Y);
                            let off = to.dot(axis) / d;
                            let half = l.outer_cone.clamp(1.0, 89.0).to_radians() + (reach / d).clamp(-1.0, 1.0).asin();
                            if off.clamp(-1.0, 1.0).acos() > half {
                                continue;
                            }
                        }
                    }
                    // a dominant light through its shadow map here (none: unshadowed)
                    let (policy, extra) = match light_shadow_maps.get(&(i as u32, *li as u32)) {
                        Some((layer, sb)) => (if *policy == Policy::SpotLight { Policy::SpotLightShadowed } else { Policy::PointLightShadowed }, Some((*sb, *layer))),
                        None => (*policy, None),
                    };
                    for (mh, mat_id, _) in &lit_parts {
                        let Some(lm) = mb.ue3_light_material(*mat_id, policy, *params, extra, &mut image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets) else { continue };
                        let pass = commands
                            .spawn((Mesh3d(mh.clone()), MeshMaterial3d(lm), MeshTag(tag), NotShadowCaster, bevy::light::NotShadowReceiver, LevelLight(*li as u32), Transform::IDENTITY, Visibility::Inherited))
                            .id();
                        commands.entity(inst_entity).add_child(pass);
                        n_light_passes += 1;
                    }
                }
            }
        }
        if let Some((col, t, r, tris)) = collider {
            let sections = inst.materials.iter().map(|&m| scene.materials.get(m as usize).map(|d| crate::footsteps::surface_id(&d.surface)).unwrap_or(0)).collect();
            let ce = commands
                .spawn((
                    col,
                    crate::footsteps::ColliderSurfaces { tris, sections },
                    RigidBody::Fixed,
                    CollisionGroups::new(GROUP_WORLD, Group::ALL),
                    Transform::from_translation(t).with_rotation(r),
                    DespawnOnExit(GameState::InGame),
                ))
                .id();
            commands.entity(inst_entity).insert(InstanceCollider(ce));
            if streamed_out {
                commands.entity(ce).insert(ColliderDisabled);
            }
            n_colliders += 1;
        }
    }

    lap("instances");
    // Blocking volumes become invisible convex colliders (with their sublevel, if streamed).
    for (vi, v) in scene.volumes.iter().enumerate() {
        if v.kind != "BlockingVolume" {
            continue;
        }
        let streamed = crate::campaign::volume_level(&scene, vi as u32).map(str::to_string);
        for hull in &v.hulls {
            let pts: Vec<Vec3> = hull.iter().map(|p| Vec3::from(*p)).collect();
            if let Some(c) = Collider::convex_hull(&pts) {
                let mut e = commands.spawn((c, RigidBody::Fixed, CollisionGroups::new(GROUP_WORLD, Group::ALL), Transform::IDENTITY, DespawnOnExit(GameState::InGame)));
                if let Some(l) = &streamed {
                    e.insert(StreamedVolume(l.clone()));
                    if !loaded_levels.contains(&l.to_ascii_lowercase()) {
                        e.insert(ColliderDisabled);
                    }
                }
                n_colliders += 1;
            }
        }
    }

    // Lights
    let mut n_lights = 0;
    let mut ambient_set = false;
    let baked = !page_layers.is_empty();
    for (li, l) in scene.lights.iter().enumerate() {
        if !l.enabled {
            continue;
        }
        if baked && !l.dynamic && l.kind != LightKind::Sky {
            continue; // already in the lightmaps
        }
        if baked && l.kind == LightKind::Directional {
            // evaluated in the world shader with precomputed shadows; the dominant one also
            // casts the dynamic objects' shadows (a shadow map only: no light of its own)
            if l.dynamic && sun_light.is_some_and(|s| std::ptr::eq(s, l)) {
                let dir = Vec3::from(l.direction).normalize_or(Vec3::NEG_Y);
                commands.spawn((
                    LevelLight(li as u32),
                    DespawnOnExit(GameState::InGame),
                    DirectionalLight { illuminance: 0.0, shadow_maps_enabled: true, affects_lightmapped_mesh_diffuse: false, ..default() },
                    bevy::light::CascadeShadowConfigBuilder { num_cascades: 2, first_cascade_far_bound: 12.0, maximum_distance: 60.0, ..default() }.build(),
                    Transform::from_translation(Vec3::from(l.position)).looking_to(dir, if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y }),
                ));
            }
            continue;
        }
        let color = Color::linear_rgb(l.color[0], l.color[1], l.color[2]);
        let pos = Vec3::from(l.position);
        let dir = Vec3::from(l.direction).normalize_or(Vec3::NEG_Y);
        let lumens = l.brightness.max(0.0) * tuning.point_scale * (l.radius.clamp(0.5, 60.0)).powi(2);
        match l.kind {
            LightKind::Point => {
                commands.spawn((
                    LevelLight(li as u32),
                    DespawnOnExit(GameState::InGame),
                    PointLight {
                        color,
                        intensity: lumens,
                        range: l.radius.max(0.5),
                        radius: 0.05,
                        shadow_maps_enabled: false,
                        affects_lightmapped_mesh_diffuse: l.dynamic,
                        ..default()
                    },
                    Transform::from_translation(pos),
                ));
            }
            LightKind::Spot => {
                let outer = l.outer_cone.clamp(1.0, 89.0).to_radians();
                let inner = l.inner_cone.clamp(0.0, l.outer_cone.clamp(1.0, 89.0)).to_radians();
                commands.spawn((
                    LevelLight(li as u32),
                    DespawnOnExit(GameState::InGame),
                    SpotLight {
                        color,
                        intensity: lumens,
                        range: l.radius.max(0.5),
                        radius: 0.05,
                        outer_angle: outer,
                        inner_angle: inner.min(outer * 0.99),
                        shadow_maps_enabled: false,
                        affects_lightmapped_mesh_diffuse: l.dynamic,
                        ..default()
                    },
                    Transform::from_translation(pos).looking_to(dir, if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y }),
                ));
            }
            LightKind::Directional => {
                commands.spawn((
                    LevelLight(li as u32),
                    DespawnOnExit(GameState::InGame),
                    DirectionalLight {
                        color,
                        illuminance: l.brightness * tuning.sun_scale,
                        shadow_maps_enabled: true,
                        affects_lightmapped_mesh_diffuse: l.dynamic,
                        ..default()
                    },
                    // the dynamic objects' shadows near the view (the world's are baked)
                    bevy::light::CascadeShadowConfigBuilder { num_cascades: 2, first_cascade_far_bound: 12.0, maximum_distance: 60.0, ..default() }.build(),
                    Transform::from_translation(pos).looking_to(dir, if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y }),
                ));
            }
            LightKind::Sky => {
                commands.insert_resource(GlobalAmbientLight {
                    color,
                    brightness: tuning.ambient * l.brightness.max(0.2),
                    affects_lightmapped_meshes: false,
                });
                ambient_set = true;
            }
        }
        n_lights += 1;
    }
    if !ambient_set {
        commands.insert_resource(GlobalAmbientLight {
            color: Color::srgb(0.55, 0.6, 0.7),
            brightness: tuning.ambient,
            affects_lightmapped_meshes: false,
        });
    }

    lap("lights");
    // ---- characters and props
    let mut build_parts = |mesh_id: Option<u32>, mats: &[u32], mb: &mut MatBuilder, mesh_assets: &mut Assets<Mesh>, image_assets: &mut Assets<Image>, mat_assets: &mut Assets<WorldMaterial>, sky_assets: &mut Assets<SkyMaterial>| -> VisualParts {
        let mut out = VisualParts::default();
        let Some(Some(prepared)) = mesh_id.and_then(|m| level.meshes.get(m as usize)) else { return out };
        for (si, sec) in prepared.sections.iter().enumerate() {
            let Some(sec) = sec else { continue };
            let mat_id = *mats.get(si).unwrap_or(&0);
            let Some(MatHandle::Std(mat)) = mb.material(mat_id, image_assets, mat_assets, sky_assets) else { continue };
            let mesh = mesh_assets.add(make_mesh(sec, false));
            // lit by the dominant light pass and the actor's light environment
            match mb.ue3_material(mat_id, Policy::NoLightMap, image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets) {
                Some(h) => {
                    let full = mesh_assets.add(make_mesh_full(sec, false));
                    let fg = ue_assets.get(&h).cloned().map(|mut m| {
                        m.key.foreground = true;
                        ue_assets.add(m)
                    });
                    out.view_parts.push((full.clone(), PartMat::Ue3(fg.unwrap_or_else(|| h.clone()))));
                    out.parts.push((full, PartMat::Ue3(h)));
                    out.mats.push((false, mat_id));
                    out.sections.push(si);
                }
                None => {
                    out.view_parts.push((mesh.clone(), PartMat::Std(mat.clone())));
                    out.parts.push((mesh, PartMat::Std(mat)));
                    out.mats.push((false, mat_id));
                    out.sections.push(si);
                }
            }
        }
        out
    };
    let mut game_assets = GameAssets::default();
    let mut anim_libs: HashMap<(Option<u32>, Vec<u32>), std::sync::Arc<crate::anim::CharAnims>> = HashMap::new();
    for t in &scene.npc_types {
        let Some(skel) = t.skeleton.and_then(|i| scene.skeletons.get(i as usize)) else {
            game_assets.npc_types.push(None);
            continue;
        };
        let mut parts = build_parts(t.body, &t.body_materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
        let no_mark = (t.kind == "player" && scene.arms_no_mark.len() == t.body_materials.len())
            .then(|| build_parts(t.body, &scene.arms_no_mark, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets));
        let head = build_parts(t.head, &t.head_materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
        parts.mats.extend(head.mats.iter().map(|(_, m)| (true, *m)));
        parts.parts.extend(head.parts);
        parts.view_parts.extend(head.view_parts);
        parts.sections.extend(head.sections);
        // the lesser LODs (the body's, each with the head's of the same rank, else its own),
        // and the dismemberment one (the head's, else its whole head with the neck's piece)
        let mref = |m: Option<u32>| m.and_then(|m| scene.meshes.get(m as usize));
        let (bref, href) = (mref(t.body), mref(t.head));
        let mut lods = Vec::new();
        for (i, bl) in bref.map(|b| b.lods.as_slice()).unwrap_or_default().iter().enumerate() {
            if bl.display_factor <= 0.0 {
                continue;
            }
            let mut p = build_parts(Some(bl.mesh), &bl.materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
            let hp = match href.and_then(|h| h.lods.get(i)) {
                Some(hl) => build_parts(Some(hl.mesh), &hl.materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets),
                None => build_parts(t.head, &t.head_materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets),
            };
            p.mats.extend(hp.mats.iter().map(|(_, m)| (true, *m)));
            p.parts.extend(hp.parts);
            p.sections.extend(hp.sections);
            lods.push((bl.display_factor, p));
        }
        let gore = bref.and_then(|b| b.gore.as_ref()).map(|bg| {
            let mut p = build_parts(Some(bg.mesh), &bg.materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
            let whole = |owner: &str| (owner.to_string(), "None".to_string(), false);
            let mut info: Vec<(String, String, bool)> = p.sections.iter().map(|&s| bg.parts.get(s).cloned().unwrap_or_else(|| whole("Root_jnt"))).collect();
            let (hp, hinfo) = match href.and_then(|h| h.gore.as_ref()) {
                Some(hg) => {
                    let hp = build_parts(Some(hg.mesh), &hg.materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
                    let hi: Vec<(String, String, bool)> = hp.sections.iter().map(|&s| hg.parts.get(s).cloned().unwrap_or_else(|| whole("head_jnt"))).collect();
                    (hp, hi)
                }
                None => {
                    let hp = build_parts(t.head, &t.head_materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
                    let hi = hp.sections.iter().map(|_| whole("head_jnt")).collect();
                    (hp, hi)
                }
            };
            p.mats.extend(hp.mats.iter().map(|(_, m)| (true, *m)));
            p.parts.extend(hp.parts);
            p.sections.extend(hp.sections);
            info.extend(hinfo);
            (p, info)
        });
        let radius = bref.map(|b| b.radius).unwrap_or(1.0).max(0.1);
        // inverse bind poses from the bind hierarchy
        let mut world: Vec<Mat4> = Vec::with_capacity(skel.bones.len());
        for b in &skel.bones {
            let local = Mat4::from_rotation_translation(Quat::from_array(b.rotation).normalize(), Vec3::from(b.translation));
            let w = if b.parent >= 0 && (b.parent as usize) < world.len() { world[b.parent as usize] * local } else { local };
            world.push(w);
        }
        let ibp: Vec<Mat4> = world.iter().map(|m| m.inverse()).collect();
        // one clip library per distinct (skeleton, anim sets)
        let sets: Vec<std::sync::Arc<dhcook::format::AnimFile>> =
            t.anim_sets.iter().filter_map(|&i| level.anims.get(i as usize).cloned().flatten()).collect();
        // (their names, in the same order: what the clips' marks are kept by)
        let set_names: Vec<String> = t
            .anim_sets
            .iter()
            .filter(|&&i| level.anims.get(i as usize).is_some_and(|a| a.is_some()))
            .map(|&i| scene.anim_sets.get(i as usize).map(|a| a.name.clone()).unwrap_or_default())
            .collect();
        // (`DH_ANIM_CHECK`: how much of each set binds to the skeleton)
        if std::env::var("DH_ANIM_CHECK").is_ok() {
            let names: std::collections::HashSet<String> = skel.bones.iter().map(|b| b.name.to_ascii_lowercase()).collect();
            for &i in &t.anim_sets {
                let Some(Some(f)) = level.anims.get(i as usize) else { continue };
                let mapped = f.bones.iter().filter(|b| names.contains(&b.to_ascii_lowercase())).count();
                info!("anim check: {} ({} bones) set {} binds {mapped}/{} bones, {} clips", t.name, skel.bones.len(), scene.anim_sets.get(i as usize).map(|a| a.name.as_str()).unwrap_or("?"), f.bones.len(), f.clips.len());
            }
        }
        let anims = if sets.is_empty() {
            None
        } else {
            let key = (t.skeleton, t.anim_sets.clone());
            Some(
                anim_libs
                    .entry(key)
                    .or_insert_with(|| std::sync::Arc::new(crate::anim::CharAnims::new(skel, sets).with_names(set_names)))
                    .clone(),
            )
        };
        game_assets.npc_types.push(Some(NpcVisual {
            kind: t.kind.clone(),
            name: t.name.clone(),
            skeleton: skel.clone(),
            inverse_bindposes: ibp_assets.add(SkinnedMeshInverseBindposes::from(ibp)),
            parts,
            no_mark,
            anims,
            body_slots: t.body_slots.clone(),
            head_slots: t.head_slots.clone(),
            lods,
            radius,
            gore,
        }));
    }
    for p in &scene.props {
        let parts = build_parts(Some(p.mesh), &p.materials, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
        game_assets.props.insert(p.name.clone(), parts);
    }
    // the watch towers' beams (`watchtower`)
    for (i, d) in scene.security.iter().enumerate() {
        if let Some((mesh, mats)) = &d.cone {
            let parts = build_parts(Some(*mesh), mats, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
            game_assets.props.insert(format!("wt_cone_{i}"), parts);
        }
    }
    for (name, id) in &scene.post_materials {
        let Some(u) = scene.materials.get(*id as usize).and_then(|m| m.ue3.as_ref()) else { continue };
        let textures = u
            .textures
            .iter()
            .map(|t| {
                let srgb = t.and_then(|t| mb.textures.get(t as usize)).and_then(|t| t.as_ref()).is_some_and(|t| t.srgb);
                mb.image(*t, srgb, &mut image_assets)
            })
            .collect();
        let curves = scene.post_curves.iter().find(|(n, _)| n == name).map(|(_, c)| c.clone()).unwrap_or_default();
        game_assets.post.insert(name.clone(), PostMat { map: u.map, params: u.params.clone(), param_names: u.param_names.clone(), textures, curves });
    }
    for (name, id) in &scene.game_textures {
        let srgb = mb.textures.get(*id as usize).and_then(|t| t.as_ref()).is_some_and(|t| t.srgb);
        if let Some(h) = mb.image(Some(*id), srgb, &mut image_assets) {
            game_assets.textures.insert(name.clone(), h);
        }
    }
    for (mesh, mats) in scene.movables.iter().filter_map(|m| m.breaks.as_ref()).chain(scene.doors.values().filter_map(|d| d.breaks.as_ref())).flat_map(|b| b.chunks.iter()) {
        if !game_assets.prop_chunks.contains_key(mesh) {
            let parts = build_parts(Some(*mesh), mats, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets);
            game_assets.prop_chunks.insert(*mesh, parts);
        }
    }
    for k in &scene.krusts {
        let mut part = |m: &Option<(u32, Vec<u32>)>| {
            m.as_ref().map(|(mesh, mats)| build_parts(Some(*mesh), mats, &mut mb, &mut mesh_assets, &mut image_assets, &mut mat_assets, &mut sky_assets))
        };
        let stalk = part(&k.stalk);
        let pearl = part(&k.pearl);
        game_assets.krust_parts.push((stalk, pearl));
    }
    if let Some(id) = scene.white_rat_material {
        game_assets.white_rat_material = match mb.ue3_material(id, Policy::NoLightMap, &mut image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets) {
            Some(h) => Some(PartMat::Ue3(h)),
            None => match mb.material(id, &mut image_assets, &mut mat_assets, &mut sky_assets) {
                Some(MatHandle::Std(h)) => Some(PartMat::Std(h)),
                _ => None,
            },
        };
    }
    // the materials the scripts put on characters (`DisSeqAct_NPCSetMaterials`)
    for op in scene.kismet.ops.iter().filter(|o| o.class == "DisSeqAct_NPCSetMaterials") {
        for key in ["body_mats", "head_mats"] {
            let Some(dhcook::format::KVal::List(l)) = op.props.get(key) else { continue };
            for e in l {
                let dhcook::format::KVal::List(pair) = e else { continue };
                let Some(dhcook::format::KVal::Int(id)) = pair.get(1) else { continue };
                let id = *id as u32;
                if game_assets.npc_mat_swaps.contains_key(&id) {
                    continue;
                }
                let m = match mb.ue3_material(id, Policy::NoLightMap, &mut image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets) {
                    Some(h) => Some(PartMat::Ue3(h)),
                    None => match mb.material(id, &mut image_assets, &mut mat_assets, &mut sky_assets) {
                        Some(MatHandle::Std(h)) => Some(PartMat::Std(h)),
                        _ => None,
                    },
                };
                if let Some(m) = m {
                    game_assets.npc_mat_swaps.insert(id, m);
                }
            }
        }
    }
    lap("characters and props");
    // particle materials: unlit, texture x tint x particle colour
    for e in scene.particle_systems.iter().flat_map(|s| s.emitters.iter()) {
        if game_assets.particle_mats.contains_key(&e.material) {
            continue;
        }
        let Some(def) = scene.materials.get(e.material as usize) else { continue };
        if def.invisible {
            continue;
        }
        if let Some(h) = mb.ue3_material(e.material, Policy::NoLightMap, &mut image_assets, &mut ue_assets, &mut ue_programs, &mut shader_assets) {
            // sprites face the camera whichever way they are wound: never culled (a copy, the
            // material may also dress level geometry)
            let h = match ue_assets.get(&h).cloned() {
                Some(mut m) if !m.key.two_sided && e.mesh.is_none() => {
                    m.key.two_sided = true;
                    ue_assets.add(m)
                }
                _ => h,
            };
            let pm = ParticleMat::Ue3(h);
            game_assets.particle_mats.insert(e.material, pm.clone());
            if let Some(mesh_id) = e.mesh {
                if let Some(Some(prepared)) = level.meshes.get(mesh_id as usize) {
                    if let Some(Some(sec)) = prepared.sections.first() {
                        game_assets.particle_meshes.insert(mesh_id, (mesh_assets.add(make_mesh_full(sec, false)), pm));
                    }
                }
            }
            continue;
        }
        let tex = mb.image(def.diffuse.or(def.emissive), true, &mut image_assets);
        let t = def.tint;
        let glow = if def.diffuse.is_none() && def.emissive.is_some() { def.emissive_color } else { [1.0; 3] };
        let mat = std_assets.add(StandardMaterial {
            base_color: Color::linear_rgba(t[0] * glow[0], t[1] * glow[1], t[2] * glow[2], 1.0),
            base_color_texture: tex,
            unlit: true,
            alpha_mode: match def.blend {
                Blend::Additive => AlphaMode::Add,
                Blend::Modulate => AlphaMode::Multiply,
                Blend::Masked => AlphaMode::Mask(def.alpha_cutoff.clamp(0.05, 0.95)),
                _ => AlphaMode::Blend,
            },
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        game_assets.particle_mats.insert(e.material, ParticleMat::Std(mat.clone()));
        if let Some(mesh_id) = e.mesh {
            if let Some(Some(prepared)) = level.meshes.get(mesh_id as usize) {
                if let Some(Some(sec)) = prepared.sections.first() {
                    let h = mesh_assets.add(make_mesh(sec, false));
                    game_assets.particle_meshes.insert(mesh_id, (h, ParticleMat::Std(mat)));
                }
            }
        }
    }
    commands.insert_resource(game_assets);

    let (n_mats, n_images) = (mb.mats.len(), mb.images.len());
    drop(mb);
    info!(
        "spawned level {} in {:.2}s: {n_entities} instances ({n_lightmapped} lightmapped, {n_ue3} original-shader parts, {} pages, {} shadow pages), {n_colliders} colliders, {n_lights} lights ({n_light_passes} dynamic light passes), {} meshes, {n_mats} materials, {n_images} images",
        scene.name,
        t0.elapsed().as_secs_f32(),
        page_layers.len(),
        shadow_layers.len(),
        mesh_cache.len(),
    );
    commands.insert_resource(crate::fxlight::LitSurfaces(lit_surfaces));
    commands.insert_resource(crate::fxlight::RuntimeLights(dyn_lights.iter().map(|(li, p, pos, r, params)| (*li as u32, *p, *pos, *r, *params)).collect()));
    commands.insert_resource(LevelInfo { scene });
}

fn insert_mesh(e: &mut EntityCommands, mesh: Handle<Mesh>, mat: MatHandle, cast_shadow: bool, tag: u32) {
    e.insert(Mesh3d(mesh));
    match mat {
        MatHandle::Std(m) => {
            e.insert(MeshMaterial3d(m));
            if !cast_shadow {
                e.insert(NotShadowCaster);
            }
            e.insert(MeshTag(tag));
        }
        MatHandle::Sky(m) => {
            e.insert((MeshMaterial3d(m), NotShadowCaster, bevy::light::NotShadowReceiver));
        }
        MatHandle::Ue3(m) => {
            e.insert((MeshMaterial3d(m), MeshTag(tag), NotShadowCaster, bevy::light::NotShadowReceiver));
        }
    }
}

/// A 1x1 black cube map.
fn black_cube_image() -> Image {
    use bevy::render::render_resource::{TextureViewDescriptor, TextureViewDimension};
    let mut img = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 6 },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
    img
}
