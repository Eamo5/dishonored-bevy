//! The pistol's and crossbow's aim (`DishonoredWeapon_Ranged.m_fDispersion`): a shot strays
//! within a cone that closes to the weapon's least dispersion while Corvo keeps steady and
//! opens to its most as he sprints, leaves the ground or blocks
//! (`DisTweaks_WeaponRanged.m_bMaxDispersionOn*`), moving at its interpolation speed; each by
//! difficulty (`DisTweaks_WeaponRanged_Attributes`) less what the accuracy upgrades take off
//! (`Attribute_Dispersion*`). The dispersion is how far the reticle's brackets stand from the
//! middle of the HUD's stage (`Crosshair_Gun.SetDispersion`), so the shot lands within them.
//! A shot kicks the view up (`m_fCamRecoilOnFire`, unreal rotation units, less the upgrades'
//! `Attribute_RecoilReduction`) and it settles back.

use crate::gameplay::PlayerStats;
use crate::gamedata::Data;
use crate::player::{Player, PlayerCamera};
use crate::powers::{Power, Powers};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use std::f32::consts::TAU;

pub struct AimPlugin;

impl Plugin for AimPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Aim>()
            .add_systems(OnEnter(GameState::InGame), |mut a: ResMut<Aim>| *a = Aim::default())
            .add_systems(Update, disperse.run_if(in_state(GameState::InGame)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weapon {
    Pistol,
    Crossbow,
}

impl Weapon {
    pub fn of(p: Power) -> Option<Weapon> {
        match p {
            Power::Pistol | Power::ExplosiveBullet => Some(Weapon::Pistol),
            Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt => Some(Weapon::Crossbow),
            _ => None,
        }
    }
    fn key(self) -> &'static str {
        match self {
            Weapon::Pistol => "pistol",
            Weapon::Crossbow => "crossbow",
        }
    }
    /// its upgrades (`Twk_Upgrade_Pistol_Accuracy1`, `Twk_Upgrade_Crossbow_Accuracy`...)
    fn upgrades(self) -> &'static str {
        match self {
            Weapon::Pistol => "Twk_Upgrade_Pistol",
            Weapon::Crossbow => "Twk_Upgrade_Crossbow",
        }
    }
}

/// The aim of the weapon in hand.
#[derive(Resource, Default)]
pub struct Aim {
    pub weapon: Option<Weapon>,
    /// the dispersion now, its least and its most (the HUD stage's units)
    pub dispersion: f32,
    pub min: f32,
    pub max: f32,
    /// the half-angle (radians) of the cone the dispersion makes on the screen
    pub cone: f32,
    /// the view's kick (radians) and the seconds since the shot
    kick: f32,
    kick_t: f32,
}

impl Aim {
    /// A shot's way: somewhere within the cone about the view's.
    pub fn scatter(&self, dir: Vec3) -> Vec3 {
        if self.cone <= 0.0 {
            return dir;
        }
        let r = self.cone * rand::random::<f32>().sqrt();
        let a = rand::random::<f32>() * TAU;
        let right = dir.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = right.cross(dir).normalize_or(Vec3::Y);
        (dir + (right * a.cos() + up * a.sin()) * r.tan()).normalize_or(dir)
    }
    /// A shot fired: the view kicks.
    pub fn fire(&mut self, data: &Data, stats: &PlayerStats, p: Power) {
        let Some(w) = Weapon::of(p) else { return };
        let units = data.pawn(&format!("{}.m_fCamRecoilOnFire", w.key()), 100.0) - upgrade_sum(data, stats, w, "RecoilReduction");
        self.kick = units.max(0.0) * TAU / 65536.0;
        self.kick_t = 0.0;
    }
    /// The kick's pitch now (radians): up in 50 ms, then settling.
    pub fn view_kick(&self) -> f32 {
        if self.kick <= 0.0 {
            return 0.0;
        }
        const UP: f32 = 0.05;
        if self.kick_t < UP {
            self.kick * self.kick_t / UP
        } else {
            self.kick * (-(self.kick_t - UP) * 9.0).exp()
        }
    }
}

/// What the weapon's upgrades owned add to one of its attributes.
fn upgrade_sum(data: &Data, stats: &PlayerStats, w: Weapon, attr: &str) -> f32 {
    data.0.upgrades.iter().filter(|u| u.id.starts_with(w.upgrades()) && stats.upgrades.contains(&u.id)).flat_map(|u| u.attributes.iter()).filter(|(n, _)| n == attr).map(|(_, v)| v).sum()
}

#[allow(clippy::too_many_arguments)]
fn disperse(
    time: Res<Time>,
    mut aim: ResMut<Aim>,
    (data, settings, stats, powers): (Res<Data>, Res<crate::settings::Settings>, Res<PlayerStats>, Res<Powers>),
    player: Query<&Player>,
    sword: Query<&crate::combat::Sword>,
    cam: Query<&Projection, With<PlayerCamera>>,
    window: Query<&Window, With<PrimaryWindow>>,
) {
    let dt = time.delta_secs();
    aim.kick_t += dt;
    let Some(w) = Weapon::of(powers.selected) else {
        aim.weapon = None;
        aim.cone = 0.0;
        return;
    };
    let d = settings.difficulty.min(3);
    let base = |n: &str| data.pawn(&format!("{}.{n}.{d}", w.key()), 0.0) + upgrade_sum(&data, &stats, w, n);
    let (min, max, speed) = (base("DispersionMin").max(0.0), base("DispersionMax").max(0.0), base("DispersionInterpolationSpeed").max(1.0));
    let flag = |n: &str| data.pawn(&format!("{}.m_bMaxDispersionOn{n}", w.key()), 0.0) > 0.0;
    let wide = player.single().is_ok_and(|p| (p.sprinting && flag("Sprint")) || (!p.grounded && !p.noclip && flag("Jump")))
        || sword.single().is_ok_and(|s| (s.blocking && flag("Block")) || (s.swing > 0.0 && flag("Melee")));
    let target = if wide { max } else { min };
    // (a weapon just taken up: steady)
    if aim.weapon != Some(w) {
        aim.weapon = Some(w);
        aim.dispersion = min;
    }
    let step = speed * dt;
    aim.dispersion = if aim.dispersion < target { (aim.dispersion + step).min(target) } else { (aim.dispersion - step).max(target) };
    aim.min = min;
    aim.max = max.max(min);
    // on the screen: the stage's units scaled to the window, against the view's half-height
    let (Ok(win), Ok(Projection::Perspective(pp))) = (window.single(), cam.single()) else { return };
    let s = (win.width() / 1280.0).min(win.height() / 720.0).max(0.01);
    aim.cone = (aim.dispersion * s / (win.height() * 0.5).max(1.0) * (pp.fov * 0.5).tan()).atan();
}
