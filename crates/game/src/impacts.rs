//! Impact effects: the contact system's particles where a bullet, bolt, dart or blade strikes
//! (`Dis_ContactSystem`'s intersections, cooked as `scene.contact_fx`), by what strikes and
//! the surface struck: splinters off wood, sparks off metal, a splash in water, blood from a
//! body.

use crate::particles::SpawnEffect;
use bevy::prelude::*;
use dhcook::format::Scene;

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
}
