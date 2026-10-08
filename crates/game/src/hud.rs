//! HUD and in-game menus.

use crate::gameplay::{HudMessages, PlayerStats, TimeControl};
use crate::powers::Powers;
use crate::GameState;
use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_rapier3d::prelude::RapierConfiguration;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Paused>()
            .add_systems(OnEnter(GameState::InGame), spawn_hud)
            .add_systems(
                Update,
                (update_adrenaline, update_texts, update_overlays, pause_and_restart, death_texts, hud_parts, fade_inventory).run_if(in_state(GameState::InGame)),
            );
    }
}

#[derive(Resource, Default)]
pub struct Paused(pub bool);

/// The running tally (coins, runes, elixirs, chaos) isn't part of the original HUD: it shows
/// for a few seconds when something in it changes, then fades.
fn fade_inventory(time: Res<Time<Real>>, mut q: Query<(&Text, &mut TextColor), With<InventoryText>>, mut last: Local<(String, f32)>) {
    let Ok((t, mut c)) = q.single_mut() else { return };
    if t.0 != last.0 {
        last.0 = t.0.clone();
        last.1 = 0.0;
    }
    last.1 += time.delta_secs();
    // (the original's pickup log, `pickuplog`, says what was found; the tally only shows with
    // `DH_TALLY=1`)
    let a = if std::env::var("DH_TALLY").is_ok() { (1.0 - (last.1 - 5.0)).clamp(0.0, 1.0) } else { 0.0 };
    if (c.0.alpha() - a).abs() > 1e-3 {
        c.0.set_alpha(a);
    }
}

/// A part of the HUD the level scripts can hide (`EDisHudElement` name).
#[derive(Component)]
pub(crate) struct HudPart(pub(crate) &'static str);

/// The scripts hide and show the HUD's parts (the Tower's opening, the Outsider's dream), and
/// so do the options: the gauges (`PSI_HUD_Visibility`: off, always, or contextual: a while
/// after they change and while either runs low) and the stance's shroud.
fn hud_parts(
    ui: Option<Res<crate::kismet::ScriptUi>>,
    settings: Res<crate::settings::Settings>,
    (time, stats): (Res<Time<Real>>, Res<crate::gameplay::PlayerStats>),
    mut seen: Local<(f32, f32, f32)>,
    mut parts: Query<(&HudPart, &mut Visibility)>,
) {
    // (contextual: 4 s after health or mana last changed, and while under the HUD tweak's
    // low thresholds)
    let (h, m) = (stats.health / stats.max_health.max(1.0), stats.mana / stats.max_mana.max(1.0));
    if (h - seen.0).abs() > 1e-4 || (m - seen.1).abs() > 1e-4 {
        *seen = (h, m, 0.0);
    }
    seen.2 += time.delta_secs();
    let gauges = match settings.hud_gauges {
        0 => false,
        1 => seen.2 < 4.0 || h < crate::gauges::LOW_HEALTH || m < crate::gauges::LOW_MANA,
        _ => true,
    };
    for (p, mut v) in &mut parts {
        let script = ui.as_ref().is_some_and(|ui| ui.hud_hidden.contains(p.0) || ui.hud_hidden.contains(crate::kismet::HUD_ALL));
        let option = match p.0 {
            "DHE_Health" | "DHE_Mana" => !gauges,
            "DHE_StealthShroud" => !settings.player_stance,
            _ => false,
        };
        let want = if script || option { Visibility::Hidden } else { Visibility::Inherited };
        if *v != want {
            *v = want;
        }
    }
}

#[derive(Component)]
struct InventoryText;
#[derive(Component)]
pub(crate) struct MessagesText;
#[derive(Component)]
struct DamageOverlay;
#[derive(Component)]
struct PowerOverlay;
#[derive(Component)]
struct DeathScreen;

/// The in-game HUD (hidden in the main menu).
#[derive(Component)]
pub struct HudRoot;


fn text(s: &str, size: f32, color: Color) -> (Text, TextFont, TextColor) {
    (Text::new(s), TextFont { font_size: FontSize::Px(size), ..default() }, TextColor(color))
}

fn spawn_hud(mut commands: Commands) {
    let gold = Color::srgb(0.86, 0.78, 0.6);
    let root = commands
        .spawn((
            HudRoot,
            Node { width: percent(100), height: percent(100), position_type: PositionType::Absolute, ..default() },
            DespawnOnExit(GameState::InGame),
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(root).with_children(|r| {
        // tints (under everything)
        r.spawn((PowerOverlay, Node { width: percent(100), height: percent(100), position_type: PositionType::Absolute, ..default() }, BackgroundColor(Color::NONE)));
        r.spawn((DamageOverlay, Node { width: percent(100), height: percent(100), position_type: PositionType::Absolute, ..default() }, BackgroundColor(Color::NONE)));
        // (health, mana and the left hand's icon: the original gauges, `gauges`)
        // Blood Thirst's adrenaline, under them
        r.spawn(Node { position_type: PositionType::Absolute, left: px(40), top: percent(40), ..default() }).with_children(|c| {
            c.spawn((AdrenalineBar, Node { width: px(200), height: px(5), ..default() }, BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.5)), Visibility::Hidden))
                .with_child((AdrenalineFill, Node { width: percent(0), height: percent(100), ..default() }, BackgroundColor(Color::srgb(0.85, 0.22, 0.1))));
        });
        // inventory (bottom-right)
        r.spawn((InventoryText, HudPart("DHE_Equipment"), text("", 16.0, gold), TextLayout::justify(Justify::Right), Node { position_type: PositionType::Absolute, right: px(28), bottom: px(28), width: px(460), ..default() }));
        // messages: the HUD movie's `gameMsg_mc` (placed by `hudtext`)
        r.spawn((MessagesText, text("", 24.0, Color::WHITE), TextLayout::justify(Justify::Center), Node { position_type: PositionType::Absolute, ..default() }));
        // crosshair + prompt (centre)
        r.spawn(Node {
            position_type: PositionType::Absolute,
            width: percent(100),
            height: percent(100),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            row_gap: px(40),
            ..default()
        })
        .with_children(|c| {
            // (the crosshair is the HUD movie's, `crosshair`; this keeps the prompt below it)
            c.spawn(Node { width: px(4), height: px(4), ..default() });
        });
        // death screen
        r.spawn((
            DeathScreen,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: px(14),
                ..default()
            },
            BackgroundColor(Color::srgba(0.25, 0.0, 0.0, 0.55)),
            Visibility::Hidden,
        ))
        .with_children(|c| {
            c.spawn((DeathTitle, text("YOU DIED", 64.0, Color::srgb(0.9, 0.82, 0.7))));
            c.spawn((DeathReason, text("", 26.0, Color::srgb(0.9, 0.85, 0.78))));
            c.spawn(text("Press R to try again", 22.0, Color::srgb(0.85, 0.85, 0.85)));
        });
    });
}

#[derive(Component)]
struct DeathTitle;

#[derive(Component)]
struct DeathReason;

/// A death, or the scripts' game over (the original's "Game Over" and their reason).
fn death_texts(
    stats: Res<PlayerStats>,
    data: Res<crate::gamedata::Data>,
    mut title: Query<&mut Text, (With<DeathTitle>, Without<DeathReason>)>,
    mut reason: Query<&mut Text, (With<DeathReason>, Without<DeathTitle>)>,
) {
    if !stats.is_changed() {
        return;
    }
    let (t, r) = match &stats.game_over {
        Some(why) => {
            let go = data.text("DisGFxMoviePlayerMenuBase_Texts", "t_GameOver");
            ((if go.is_empty() { "Game Over".to_string() } else { go }).to_uppercase(), why.clone())
        }
        None => ("YOU DIED".to_string(), String::new()),
    };
    if let Ok(mut x) = title.single_mut() {
        if x.0 != t {
            x.0 = t;
        }
    }
    if let Ok(mut x) = reason.single_mut() {
        if x.0 != r {
            x.0 = r;
        }
    }
}

#[derive(Component)]
struct AdrenalineBar;

#[derive(Component)]
struct AdrenalineFill;

#[allow(clippy::type_complexity)]
fn update_adrenaline(
    stats: Res<PlayerStats>,
    attrs: Res<crate::gamedata::Attrs>,
    mut bar: Query<&mut Visibility, With<AdrenalineBar>>,
    mut fill: Query<(&mut Node, &mut BackgroundColor), With<AdrenalineFill>>,
) {
    let on = stats.power("BloodThirsty") > 0;
    for mut v in &mut bar {
        let want = if on { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    if let Ok((mut n, mut c)) = fill.single_mut() {
        let k = (stats.adrenaline / attrs.adrenaline_max.max(1.0)).clamp(0.0, 1.0);
        n.width = percent(100.0 * k);
        // full: ready for a fatality
        c.0 = if k >= 1.0 { Color::srgb(1.0, 0.45, 0.2) } else { Color::srgb(0.75, 0.18, 0.08) };
    }
}

fn update_texts(
    stats: Res<PlayerStats>,
    msgs: Res<HudMessages>,
    mut texts: ParamSet<(
        Query<&mut Text, With<InventoryText>>,
        Query<&mut Text, With<MessagesText>>,
    )>,
) {
    if let Ok(mut t) = texts.p0().single_mut() {
        t.0 = format!(
            "Coins {}   Runes {}\nElixirs {} {}   Remedies {} {}\nKills {}   Knockouts {}   Chaos {}",
            stats.coins,
            stats.runes,
            stats.health_elixirs,
            crate::bindings::hint(crate::bindings::Act::HealthElixir).replace('[', "(").replace(']', ")"),
            stats.mana_elixirs,
            crate::bindings::hint(crate::bindings::Act::ManaElixir).replace('[', "(").replace(']', ")"),
            stats.kills,
            stats.knockouts,
            stats.chaos()
        );
    }
    if let Ok(mut t) = texts.p1().single_mut() {
        t.0 = msgs.items.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join("\n");
    }
}

#[allow(clippy::type_complexity)]
fn update_overlays(
    stats: Res<PlayerStats>,
    powers: Res<Powers>,
    tc: Res<TimeControl>,
    mut q: ParamSet<(
        Query<&mut BackgroundColor, With<DamageOverlay>>,
        Query<&mut BackgroundColor, With<PowerOverlay>>,
        Query<&mut Visibility, With<DeathScreen>>,
    )>,
) {
    if let Ok(mut bg) = q.p0().single_mut() {
        // (the original's damage feedback is the HUD effects movie's, `hudfx`; this tint only
        // stands in without it)
        bg.0 = if std::env::var("DH_NO_HUDFX").is_ok() {
            let low = if stats.health < 30.0 && !stats.dead { 0.12 } else { 0.0 };
            Color::srgba(0.6, 0.0, 0.0, (stats.damage_flash * 0.35 + low).min(0.6))
        } else {
            Color::NONE
        };
    }
    if let Ok(mut bg) = q.p1().single_mut() {
        // (the powers' looks are the post-process graph's: `ppgraph`, `darkvision`; this tint
        // only stands in when it is off)
        bg.0 = if std::env::var("DH_NO_PPG").is_err() {
            Color::NONE
        } else if tc.bend_remaining > 0.0 {
            Color::srgba(0.55, 0.45, 0.25, 0.22)
        } else if powers.fov_kick > 0.0 {
            Color::srgba(0.4, 0.7, 1.0, 0.15 * powers.fov_kick)
        } else {
            Color::NONE
        };
    }
    if let Ok(mut v) = q.p2().single_mut() {
        // (the game over menu says it: `frontend`)
        let _ = stats.dead;
        *v = Visibility::Hidden;
    }
}

#[allow(clippy::too_many_arguments)]
fn pause_and_restart(
    keys: Res<ButtonInput<KeyCode>>,
    stats: Res<PlayerStats>,
    mut paused: ResMut<Paused>,
    mut time: ResMut<Time<Virtual>>,
    mut cursor: Single<&mut CursorOptions>,
    mut rapier_cfg: Query<&mut RapierConfiguration>,
    mut next: ResMut<NextState<GameState>>,
    mut exit: MessageWriter<AppExit>,
) {
    let want_pause = paused.0;
    if want_pause && !time.is_paused() {
        time.pause();
    } else if !want_pause && time.is_paused() {
        time.unpause();
    }
    for mut c in &mut rapier_cfg {
        c.physics_pipeline_active = !want_pause;
    }
    if want_pause || stats.dead {
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
    }
    if stats.dead && keys.just_pressed(KeyCode::KeyR) {
        paused.0 = false;
        time.unpause();
        next.set(GameState::Loading);
    }
    let _ = &mut exit;
}
