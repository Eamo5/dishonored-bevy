//! Taps and fountains (`DisWaterSource`): at one, [Use] "Drink" turns the valve and runs the
//! water (the tap's "Use" sequence: its sounds and stream); with the Water of Life or Spirit
//! Water bone charm a drink gives back health (`WaterDrinkingHealthBonus`) or mana
//! (`WaterDrinkingManaBonus`).

use crate::bindings::{hint, Act, Bindings};
use crate::interact::InteractFocus;
use crate::level::LevelInfo;
use crate::particles::SpawnEffect;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(GameState::InGame), spawn_taps.after(crate::level::LevelSpawnSet))
            .add_systems(Update, tap_focus.after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)));
    }
}

/// How near Corvo uses a tap (m).
const REACH: f32 = 1.7;

#[derive(Component)]
struct Tap {
    index: usize,
    /// running for
    busy: f32,
}

fn spawn_taps(mut commands: Commands, level: Option<Res<LevelInfo>>) {
    let Some(level) = level else { return };
    for (i, w) in level.scene.water_sources.iter().enumerate() {
        commands.spawn((Tap { index: i, busy: 0.0 }, Transform::from_translation(Vec3::from(w.position)), DespawnOnExit(GameState::InGame)));
    }
    if !level.scene.water_sources.is_empty() {
        info!("{} taps and fountains", level.scene.water_sources.len());
    }
}

#[allow(clippy::too_many_arguments)]
fn tap_focus(
    time: Res<Time>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<Bindings>),
    (level, data, settings): (Option<Res<LevelInfo>>, Res<crate::gamedata::Data>, Res<crate::settings::Settings>),
    mut focus: ResMut<InteractFocus>,
    mut taps: Query<(&mut Tap, &Transform)>,
    player: Query<&Player>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    (carry, possession, held): (Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::props::Held>),
    mut stats: ResMut<crate::gameplay::PlayerStats>,
    mut timed: ResMut<crate::audio::TimedSounds>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    let Some(level) = level else { return };
    let dt = time.delta_secs();
    for (mut t, _) in &mut taps {
        t.busy = (t.busy - dt).max(0.0);
    }
    // anything else in view comes first
    if focus.0 || carry.carrying() || possession.host.is_some() || held.0.is_some() || player.single().is_ok_and(|p| p.locked) {
        return;
    }
    let Ok(c) = cam.single() else { return };
    let (eye, fwd) = (c.translation(), c.forward().as_vec3());
    let near = taps
        .iter_mut()
        .filter(|(_, t)| {
            let to = t.translation - eye;
            to.length() < REACH && to.normalize_or_zero().dot(fwd) > 0.85
        })
        .min_by(|a, b| a.1.translation.distance(eye).total_cmp(&b.1.translation.distance(eye)));
    let Some((mut tap, tt)) = near else { return };
    let Some(w) = level.scene.water_sources.get(tap.index) else { return };
    focus.0 = true;
    focus.1 = None;
    focus.2 = format!("{} {}", hint(Act::Use), w.text);
    if !keys.just_pressed(bind.key(Act::Use)) || tap.busy > 0.0 {
        return;
    }
    tap.busy = w.duration.max(0.5);
    timed.schedule(&w.sounds, Some(tt.translation));
    if let Some((_, ps, at)) = w.stream {
        fx.write(SpawnEffect { system: Some(ps), secs: (w.duration - 0.6).max(0.6), ..SpawnEffect::at("", Vec3::from(at)) });
    }
    // a drink: the charms' bonuses
    let d = settings.difficulty;
    let (powers, charms) = (stats.powers.clone(), stats.charms.clone());
    let health = data.attribute("WaterDrinkingHealthBonus", d, &powers, &charms);
    let mana = data.attribute("WaterDrinkingManaBonus", d, &powers, &charms);
    if health > 0.0 {
        stats.health = (stats.health + health).min(stats.max_health);
    }
    if mana > 0.0 {
        stats.mana = (stats.mana + mana).min(stats.max_mana);
    }
}
