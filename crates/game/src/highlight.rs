//! The golden rim on what Corvo can use (`DisTweaks_InteractableInterface.m_HighLightMaterial`:
//! `vfx_interactivity.interactivity_INST`, a fresnel glow): the thing in reach and looked at
//! (a pickup, a door, a lever...) is drawn over again with it, as the original's highlight
//! mesh component is; the level scripts highlight things of their own
//! (`DisSeqAct_Highlight`).

use crate::interact::{InteractFocus, Pickup, Usable};
use crate::level::{LevelInfo, LevelInstance};
use crate::GameState;
use bevy::asset::embedded_asset;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

pub struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "highlight.wgsl");
        app.add_plugins(MaterialPlugin::<HighlightMaterial>::default())
            .add_systems(OnExit(GameState::InGame), |mut h: ResMut<HighlightLook>| h.0 = None)
            .init_resource::<HighlightLook>()
            .add_systems(Update, highlight.after(crate::interact::FocusSet).run_if(in_state(GameState::InGame)));
    }
}

/// The rim's material (`golden_focus_PMAT` with the instance's `F_Fresnel_Color`,
/// `F_Fresnel_Power`, `F_Fresnel_Visibility`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct HighlightMaterial {
    #[uniform(0)]
    color: Vec4,
    #[uniform(1)]
    params: Vec4,
}

impl Material for HighlightMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/highlight.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
}

/// The level's rim material, made on first use.
#[derive(Resource, Default)]
struct HighlightLook(Option<Handle<HighlightMaterial>>);

/// A mesh drawn over with the rim.
#[derive(Component)]
struct HighlightPart;

#[allow(clippy::too_many_arguments)]
fn highlight(
    mut commands: Commands,
    (focus, settings): (Res<InteractFocus>, Res<crate::settings::Settings>),
    level: Option<Res<LevelInfo>>,
    (mut look, mut mats): (ResMut<HighlightLook>, ResMut<Assets<HighlightMaterial>>),
    vm: Option<Res<crate::kismet::Vm>>,
    (pickups, usables, instances): (Query<&Pickup>, Query<&Usable>, Query<(Entity, &LevelInstance)>),
    meshes: Query<(&Mesh3d, Option<&bevy::mesh::skinning::SkinnedMesh>), Without<HighlightPart>>,
    children: Query<&Children>,
    parts: Query<Entity, With<HighlightPart>>,
    mut lit: Local<Vec<Entity>>,
) {
    let Some(level) = level else { return };
    if look.0.is_none() {
        // the material's parameters as the level cooked them (else the original's values)
        let u = level.scene.highlight_material.and_then(|m| level.scene.materials.get(m as usize)).and_then(|m| m.ue3.as_ref());
        let get = |n: &str| u.and_then(|u| u.param_names.iter().position(|p| p == n).map(|i| Vec4::from_array(u.params[i])));
        let color = get("F_Fresnel_Color").unwrap_or(Vec4::new(1.0, 0.454, 0.0, 1.0));
        let power = get("F_Fresnel_Power").map(|v| v.x).unwrap_or(4.0);
        let visibility = get("F_Fresnel_Visibility").map(|v| v.x).unwrap_or(10.0);
        look.0 = Some(mats.add(HighlightMaterial { color, params: Vec4::new(power, visibility, 0.0, 0.0) }));
    }
    let Some(mat) = look.0.clone() else { return };
    // what to highlight: the thing in focus, and the scripts' (their actors' meshes)
    let mut want: Vec<Entity> = Vec::new();
    let actor_meshes = |a: u32, out: &mut Vec<Entity>| {
        let Some(ka) = vm.as_ref().and_then(|v| v.g.actors.get(a as usize)) else { return };
        for (e, li) in &instances {
            if ka.instances.contains(&li.index) {
                out.push(e);
            }
        }
    };
    // (the thing in focus: as `PSI_HUD_bShowHighlight` has it; the scripts' always)
    if let Some(f) = focus.1.filter(|_| focus.0 && settings.focus_highlight) {
        if let Ok(p) = pickups.get(f) {
            want.extend(p.entities.iter().copied());
        } else if let Ok(u) = usables.get(f) {
            actor_meshes(u.actor, &mut want);
        } else if meshes.contains(f) {
            want.push(f);
        }
    }
    if let Some(vm) = vm.as_ref() {
        for &a in &vm.highlights {
            actor_meshes(a, &mut want);
        }
    }
    want.sort();
    want.dedup();
    if *lit == want {
        return;
    }
    for e in &parts {
        commands.entity(e).try_despawn();
    }
    // each mesh (and its parts' meshes) drawn again, with the rim
    for &e in &want {
        let mut stack = vec![e];
        while let Some(x) = stack.pop() {
            if let Ok((m, skin)) = meshes.get(x) {
                let mut hc = commands.spawn((Mesh3d(m.0.clone()), MeshMaterial3d(mat.clone()), Transform::IDENTITY, HighlightPart, bevy::light::NotShadowCaster));
                // (a skinned part keeps its skin: drawn without it, a mesh with joints breaks the
                // skinned pipeline's bind group)
                if let Some(sk) = skin {
                    hc.insert((sk.clone(), bevy::camera::visibility::DynamicSkinnedMeshBounds));
                }
                let h = hc.id();
                commands.entity(x).add_child(h);
            }
            if let Ok(c) = children.get(x) {
                stack.extend(c.iter());
            }
        }
    }
    if std::env::var("DH_HIGHLIGHT_LOG").is_ok() {
        info!("highlight: {} things ({} parts drawn before)", want.len(), parts.iter().count());
    }
    *lit = want;
}
