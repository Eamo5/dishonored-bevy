//! Material parameters the level scripts set (`SeqAct_SetMatInstScalarParam`: the Outsider's
//! fade and glow, a lamp coming on): every original-shader material made from that material
//! instance takes the new value in the parameter's uniform slot (the slot's name is cooked with
//! the material, `Ue3Material::param_names`).

use crate::level::LevelInfo;
use crate::ue3mat::Ue3Material;
use crate::GameState;
use bevy::prelude::*;
use std::collections::HashMap;

pub struct MatParamsPlugin;

impl Plugin for MatParamsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MatIndex>()
            .add_systems(OnEnter(GameState::InGame), |mut i: ResMut<MatIndex>| *i = MatIndex::default())
            .add_systems(Update, (set_params, set_materials).after(crate::kismet::apply_effects).run_if(in_state(GameState::InGame)));
    }
}

/// `SeqAct_SetMaterial`: the targets' surfaces take the material made for them at load.
fn set_materials(mut commands: Commands, vm: Option<ResMut<crate::kismet::Vm>>, insts: Query<(&crate::level::LevelInstance, &crate::level::MatSwaps)>) {
    let Some(mut vm) = vm else { return };
    if vm.set_materials.is_empty() {
        return;
    }
    let g = vm.g.clone();
    for (targets, id) in std::mem::take(&mut vm.set_materials) {
        let wanted: Vec<u32> = targets
            .iter()
            .filter_map(|v| if let crate::kismet::Val::Actor(a) = v { g.actors.get(*a as usize) } else { None })
            .flat_map(|a| a.instances.iter().copied())
            .collect();
        let mut swapped = 0;
        for (li, swaps) in &insts {
            if !wanted.contains(&li.index) {
                continue;
            }
            for (m, e, h) in &swaps.0 {
                if *m == id {
                    commands.entity(*e).try_insert(MeshMaterial3d(h.clone()));
                    swapped += 1;
                }
            }
        }
        if std::env::var("DH_SCRIPT_WORLD_LOG").is_ok() {
            info!("level scripts: material {id} on {} instances ({swapped} surfaces)", wanted.len());
        }
    }
}

/// The level's materials by name, and the material assets made from each (rebuilt when the
/// asset count changes: light passes are added as lights come near).
#[derive(Resource, Default)]
struct MatIndex {
    by_name: HashMap<String, Vec<u32>>,
    assets: HashMap<u32, Vec<AssetId<Ue3Material>>>,
    counted: usize,
}

fn set_params(vm: Option<ResMut<crate::kismet::Vm>>, level: Option<Res<LevelInfo>>, mut materials: ResMut<Assets<Ue3Material>>, mut index: ResMut<MatIndex>) {
    let (Some(mut vm), Some(level)) = (vm, level) else { return };
    if vm.mat_params.is_empty() {
        return;
    }
    let requests = std::mem::take(&mut vm.mat_params);
    let scene = &level.scene;
    if index.by_name.is_empty() {
        for (i, m) in scene.materials.iter().enumerate() {
            index.by_name.entry(m.name.to_ascii_lowercase()).or_default().push(i as u32);
        }
    }
    if index.counted != materials.len() {
        index.counted = materials.len();
        index.assets.clear();
        for (id, m) in materials.iter() {
            if m.mat_id != u32::MAX {
                index.assets.entry(m.mat_id).or_default().push(id);
            }
        }
    }
    let log = std::env::var("DH_SCRIPT_WORLD_LOG").is_ok();
    for (path, param, value) in requests {
        let ids = index.by_name.get(&path.to_ascii_lowercase()).cloned().unwrap_or_default();
        let mut set = 0;
        for id in ids {
            let Some(ue) = scene.materials.get(id as usize).and_then(|m| m.ue3.as_ref()) else { continue };
            let slots: Vec<usize> = ue.param_names.iter().enumerate().filter(|(_, n)| n.eq_ignore_ascii_case(&param)).map(|(i, _)| i).collect();
            if slots.is_empty() {
                continue;
            }
            for a in index.assets.get(&id).cloned().unwrap_or_default() {
                if let Some(mut m) = materials.get_mut(a) {
                    for &s in &slots {
                        if s < m.params.p.len() {
                            m.params.p[s] = Vec4::splat(value);
                        }
                    }
                    set += 1;
                }
            }
        }
        if log {
            info!("level scripts: {path}.{param} = {value} ({set} materials)");
        }
    }
}
