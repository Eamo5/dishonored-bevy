//! Impact effects: the contact system's particles where a bullet, bolt, dart or blade strikes
//! (`Dis_ContactSystem`'s intersections, cooked as `scene.contact_fx`), by what strikes and
//! the surface struck: splinters off wood, sparks off metal, a splash in water, blood from a
//! body; and its decals (`scene.contact_decals`): bullet holes, a thrown body's blood.

use crate::decals::SpawnDecal;
use crate::particles::SpawnEffect;
use bevy::prelude::*;
use dhcook::format::{Decal, Scene};

/// What strikes, as the contact system names it (`DisContactType_<this>`).
pub const BULLET: &str = "Bullet";
pub const ARROW: &str = "Arrow";
pub const ARROW_SLEEP: &str = "Arrow_Sleep";
pub const ARROW_FLARE: &str = "Arrow_Flare";
pub const SWORD: &str = "Sword";

/// The contact type of a surface (`footsteps::SURFACES`) as struck.
pub fn struck(surface: u8) -> &'static str {
    match crate::footsteps::SURFACES.get(surface as usize).copied().unwrap_or("Stone") {
        "Wood" => "Env_Wood",
        "Metal" => "Env_Metal",
        "Glass" => "Env_Glass",
        "Carpet" => "Env_Carpet",
        "Dirt" => "Env_Dirt",
        "Grass" => "Env_Grass",
        "Foliage" => "Env_Foliage",
        "Gravel" => "Env_Gravel",
        "Mud" => "Env_Mud",
        "Water" => "Env_Water",
        "RoofTile" => "Env_Tile",
        "Flesh" => "Body",
        _ => "Env_Stone",
    }
}

/// The particle system of `striker` on `struck`: the surface's own, else (a surface, not a
/// body) the plain environment's.
pub fn system(scene: &Scene, striker: &str, struck: &str) -> Option<u32> {
    let find = |s: &str| scene.contact_fx.iter().find(|c| c.0 == striker && c.1 == s).map(|c| c.2);
    find(struck).or_else(|| struck.starts_with("Env_").then(|| find("Environment")).flatten())
}

/// The decal `striker` leaves on `struck`: the intersection's own, else (a surface, not a body)
/// the plain environment's; none where the intersection's decal never draws.
pub fn decal<'a>(scene: &'a Scene, striker: &str, struck: &str) -> Option<&'a Decal> {
    let find = |s: &str| scene.contact_decals.iter().find(|c| c.0 == striker && c.1 == s);
    match find(struck) {
        Some(c) => c.2.as_ref(),
        None => struck.starts_with("Env_").then(|| find("Environment")).flatten().and_then(|c| c.2.as_ref()),
    }
}

/// `striker`'s decal where it struck `struck` at `at` (the surface's `normal`), its texture
/// along the strike, if it came faster than the decal's least speed (uu/s); whether it did.
#[allow(clippy::too_many_arguments)]
pub fn mark(decals: &mut MessageWriter<SpawnDecal>, scene: &Scene, striker: &str, struck: &str, at: Vec3, normal: Vec3, along: Vec3, speed: f32) -> bool {
    let Some(d) = decal(scene, striker, struck) else { return false };
    if speed <= d.min_velocity {
        return false;
    }
    decals.write(SpawnDecal { decal: d.clone(), at, normal, along: Some(along) });
    true
}

/// The impact effect of `striker` on `struck` at `at`, turned to `normal` (the particle
/// system's X axis out of the surface, as UE3 spawns it along the hit normal).
pub fn spawn(fx: &mut MessageWriter<SpawnEffect>, scene: &Scene, striker: &str, struck: &str, at: Vec3, normal: Vec3) {
    let Some(system) = system(scene, striker, struck) else { return };
    let n = normal.normalize_or(Vec3::Y);
    fx.write(SpawnEffect { name: "", system: Some(system), rot: Quat::from_rotation_arc(Vec3::X, n), secs: 3.0, ..SpawnEffect::at("", at) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_fall_back_to_the_environment_but_bodies_do_not() {
        let mut scene = Scene::default();
        scene.contact_fx = vec![("Bullet".into(), "Env_Wood".into(), 4), ("Bullet".into(), "Environment".into(), 7), ("Arrow".into(), "Body".into(), 9)];
        assert_eq!(system(&scene, BULLET, struck(1)), Some(4));
        assert_eq!(system(&scene, BULLET, struck(0)), Some(7));
        assert_eq!(system(&scene, BULLET, "Body"), None);
        assert_eq!(system(&scene, ARROW, "Body"), Some(9));
        assert_eq!(struck(12), "Body");
    }

    #[test]
    fn a_surface_without_its_own_decal_takes_the_environments_and_one_that_never_draws_none() {
        let mut scene = Scene::default();
        let hole = Decal { materials: vec![3], max: [8.0, 8.0], ..default() };
        let splat = Decal { materials: vec![5], max: [150.0, 150.0], min_velocity: 750.0, ..default() };
        scene.contact_decals = vec![
            ("Bullet".into(), "Environment".into(), Some(hole)),
            ("Bullet".into(), "Env_Wood".into(), None),
            ("Limb".into(), "Environment".into(), Some(splat)),
        ];
        assert_eq!(decal(&scene, BULLET, "Env_Stone").map(|d| d.materials[0]), Some(3));
        assert!(decal(&scene, BULLET, "Env_Wood").is_none());
        assert!(decal(&scene, BULLET, "Body").is_none());
        assert_eq!(decal(&scene, "Limb", "Env_Tile").map(|d| d.materials[0]), Some(5));
    }
}
