//! Coordinate conversion: UE3 (left-handed, Z-up, centimeters) -> Bevy (right-handed, Y-up, meters).

use glam::{Mat4, Vec3, Vec4};

/// Unreal units to meters.
pub const UNIT: f32 = 0.01;

pub fn ue_point(p: [f32; 3]) -> [f32; 3] {
    [p[0] * UNIT, p[2] * UNIT, p[1] * UNIT]
}

pub fn ue_dir(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[2], p[1]]
}

/// UE3 FRotationMatrix as a column-vector glam matrix (UE space).
pub fn rot_matrix(r: [i32; 3]) -> Mat4 {
    let k = std::f32::consts::TAU / 65536.0;
    let (p, y, ro) = (r[0] as f32 * k, r[1] as f32 * k, r[2] as f32 * k);
    let (sp, cp) = p.sin_cos();
    let (sy, cy) = y.sin_cos();
    let (sr, cr) = ro.sin_cos();
    let x = Vec4::new(cp * cy, cp * sy, sp, 0.0);
    let yv = Vec4::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp, 0.0);
    let z = Vec4::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp, 0.0);
    Mat4::from_cols(x, yv, z, Vec4::W)
}

/// Scale, then rotate, then translate (UE3 FScaleRotationTranslationMatrix).
pub fn srt(scale: Vec3, rot: [i32; 3], trans: Vec3) -> Mat4 {
    Mat4::from_translation(trans) * rot_matrix(rot) * Mat4::from_scale(scale)
}

fn swap_yz() -> Mat4 {
    Mat4::from_cols(Vec4::X, Vec4::Z, Vec4::Y, Vec4::W)
}

/// Convert a UE-space world matrix to Bevy space.
pub fn ue_to_bevy(m: Mat4) -> Mat4 {
    let c = Mat4::from_scale(Vec3::splat(UNIT)) * swap_yz();
    let ci = swap_yz() * Mat4::from_scale(Vec3::splat(1.0 / UNIT));
    c * m * ci
}

/// UE yaw (65536 units/turn) -> Bevy rotation about +Y such that
/// `Quat::from_rotation_y(yaw) * -Z` equals the UE facing direction.
pub fn ue_yaw_to_bevy(yaw: i32) -> f32 {
    let a = yaw as f32 * std::f32::consts::TAU / 65536.0;
    let d = Vec3::new(a.cos(), 0.0, a.sin());
    (-d.x).atan2(-d.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_consistent() {
        // A point transformed in UE space then converted must equal converting first.
        let m = srt(Vec3::new(2.0, 1.0, 3.0), [1000, 9000, -3000], Vec3::new(100.0, 200.0, 300.0));
        let p = [10.0, -20.0, 30.0];
        let w_ue = m.transform_point3(Vec3::from(p));
        let a = Vec3::from(ue_point(w_ue.to_array()));
        let b = ue_to_bevy(m).transform_point3(Vec3::from(ue_point(p)));
        assert!((a - b).length() < 1e-4, "{a} vs {b}");
    }

    #[test]
    fn yaw() {
        use glam::Quat;
        for y in [0, 16384, 32768, -16384, 5000] {
            let a = y as f32 * std::f32::consts::TAU / 65536.0;
            let ue_fwd = Vec3::from(ue_dir([a.cos(), a.sin(), 0.0]));
            let f = Quat::from_rotation_y(ue_yaw_to_bevy(y)) * Vec3::NEG_Z;
            assert!((f - ue_fwd).length() < 1e-4);
        }
    }
}
