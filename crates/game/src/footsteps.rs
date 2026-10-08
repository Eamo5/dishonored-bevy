//! Footsteps and landings: the original Wwise footfall events, `FS_<who>_<surface>_<gait>`
//! (e.g. `FS_P_Stone_R`, `FS_Guard_Wood_Sp`, `FS_P_Metal_Fall_High`), chosen by the physical
//! material (`Phm_*`) of the surface underfoot. World colliders carry the surface of each of
//! their triangles ([`ColliderSurfaces`]).

use crate::audio::{has_event, PostEvent};
use crate::level::{GROUP_PROP, GROUP_WORLD};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use bevy_rapier3d::rapier::geometry::FeatureId;
use std::sync::Arc;

pub struct FootstepsPlugin;

impl Plugin for FootstepsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Footfall>().add_systems(Update, play_footfalls.run_if(in_state(GameState::InGame)));
    }
}

/// Footfall surfaces, as the Wwise events name them.
pub const SURFACES: [&str; 13] = ["Stone", "Wood", "Metal", "Glass", "Carpet", "Dirt", "Grass", "Foliage", "Gravel", "Mud", "Water", "RoofTile", "Flesh"];

/// The footfall surface of a physical material (`Phm_` stripped); unknown / none: stone.
pub fn surface_id(phm: &str) -> u8 {
    let s = match phm {
        "Tallboy_Legs" => "Wood",
        "Metal_PASpeaker" | "Pans" | "WhaleOilBattery" | "Wall_of_Light" => "Metal",
        "BreakableGlass" => "Glass",
        "Tiles" => "RoofTile",
        "RiverKrustFlesh" | "ProtectedNPC" => "Flesh",
        "StoneWet" => "Stone",
        x => x,
    };
    SURFACES.iter().position(|n| *n == s).unwrap_or(0) as u8
}

/// Surfaces of a trimesh collider: the mesh section of each triangle, and the surface of
/// each section (from the instance's materials).
#[derive(Component)]
pub struct ColliderSurfaces {
    pub tris: Arc<Vec<u8>>,
    pub sections: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gait {
    Walk,
    Sprint,
    Sneak,
    LandSmall,
    LandHigh,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Walker {
    Player,
    Guard,
    Wolfhound,
    Rat,
}

/// A foot touching the ground at `pos`.
#[derive(Message, Clone, Copy, Debug)]
pub struct Footfall {
    pub pos: Vec3,
    pub gait: Gait,
    pub who: Walker,
}

/// The surface below a point (within `reach` metres).
pub fn surface_below(ctx: &RapierContext, surfaces: &Query<&ColliderSurfaces>, pos: Vec3, reach: f32) -> Option<u8> {
    let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let (e, hit) = ctx.cast_ray_and_get_normal(pos + Vec3::Y * 0.2, Vec3::NEG_Y, reach, true, filter)?;
    let Ok(s) = surfaces.get(e) else { return Some(0) };
    let tri = match hit.feature {
        FeatureId::Face(i) => i as usize,
        _ => return Some(0),
    };
    let n = s.tris.len().max(1);
    let sec = *s.tris.get(tri % n)? as usize;
    Some(s.sections.get(sec).copied().unwrap_or(0))
}

/// The impact event name of a surface (`Imp_<what>_on_<surface>`).
pub fn impact_surface(surface: u8) -> &'static str {
    match SURFACES.get(surface as usize).copied().unwrap_or("Stone") {
        "RoofTile" => "Tile",
        "Foliage" => "Grass",
        "Flesh" => "Body",
        s => s,
    }
}

/// Cast a ray and report what it hit, how far, and the surface there.
pub fn ray_surface(ctx: &RapierContext, surfaces: &Query<&ColliderSurfaces>, from: Vec3, dir: Vec3, max: f32, filter: QueryFilter) -> Option<(Entity, f32, u8)> {
    let (e, hit) = ctx.cast_ray_and_get_normal(from, dir, max, true, filter)?;
    let surface = surfaces
        .get(e)
        .ok()
        .and_then(|s| match hit.feature {
            FeatureId::Face(i) => {
                let n = s.tris.len().max(1);
                s.tris.get(i as usize % n).and_then(|&sec| s.sections.get(sec as usize).copied())
            }
            _ => None,
        })
        .unwrap_or(0);
    Some((e, hit.time_of_impact, surface))
}

fn play_footfalls(
    mut msgs: MessageReader<Footfall>,
    rapier: ReadRapierContext,
    surfaces: Query<&ColliderSurfaces>,
    mut out: MessageWriter<PostEvent>,
) {
    let Ok(ctx) = rapier.single() else {
        msgs.clear();
        return;
    };
    let log = std::env::var("DH_AUDIO_LOG").is_ok();
    for f in msgs.read() {
        let who = match f.who {
            Walker::Player => "P",
            Walker::Guard => "Guard",
            Walker::Wolfhound => "Wolfhound",
            Walker::Rat => {
                out.write(PostEvent::named("FS_Rat_Walk", Some(f.pos)));
                continue;
            }
        };
        let Some(surface) = surface_below(&ctx, &surfaces, f.pos, 2.5) else { continue };
        let gait = match f.gait {
            Gait::Walk => "R",
            Gait::Sprint => "Sp",
            Gait::Sneak => "Sn",
            Gait::LandSmall => "Fall_Small",
            Gait::LandHigh => "Fall_High",
        };
        // not every surface has every gait: fall back to the nearest variant
        let candidates = [
            format!("FS_{who}_{}_{gait}", SURFACES[surface as usize]),
            format!("FS_{who}_{}_R", SURFACES[surface as usize]),
            format!("FS_{who}_Stone_{gait}"),
            format!("FS_{who}_Stone_R"),
        ];
        let Some(name) = candidates.into_iter().find(|n| has_event(n)) else { continue };
        if log {
            info!("footfall {name}");
        }
        out.write(PostEvent::named(&name, Some(f.pos)));
    }
}
