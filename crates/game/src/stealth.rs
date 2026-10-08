//! How visible Corvo is (`DishonoredVisibilityComponent` with `Twk_PlayerVisibility`): his
//! light value, normalised over the darkest and brightest values the scripts, the stealth volume
//! he stands in (`DisStealthVolume`, by priority), the map or the game set, and his speed, each
//! through its curve, plus his stance's bonus. Each onlooker adds what nearness gives
//! (`npc::npc_perception`).

use crate::level::LevelInfo;
use crate::player::Player;
use crate::world_light::LitActor;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::Collider;

pub struct StealthPlugin;

impl Plugin for StealthPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerVisibility>()
            .add_systems(OnEnter(GameState::InGame), (build_volumes.after(crate::level::LevelSpawnSet), spawn_shroud))
            .add_systems(Update, (player_visibility.after(crate::player::PlayerMoveSet), stealth_shroud).run_if(in_state(GameState::InGame)));
    }
}

/// What Corvo's light, speed and stance add up to, for every onlooker.
#[derive(Resource, Default, Debug)]
pub struct PlayerVisibility {
    /// the light value normalised over the range in force (0 darkest .. 1 brightest)
    pub light: f32,
    /// speed (0 still .. 1 sprinting; sneaking counts as still)
    pub speed: f32,
    /// light and speed through their curves, plus the stance's bonus
    pub value: f32,
    /// the light range in force (darkest, brightest)
    pub range: [f32; 2],
}

/// The level's stealth volumes: their pieces, bounds, light range and priority.
#[derive(Resource, Default)]
struct StealthVolumes(Vec<(Vec<Collider>, Vec3, Vec3, [f32; 3])>);

fn build_volumes(mut commands: Commands, level: Option<Res<LevelInfo>>) {
    let mut out = Vec::new();
    for v in level.iter().flat_map(|l| l.scene.volumes.iter()) {
        let Some(st) = v.stealth else { continue };
        let pts: Vec<Vec3> = v.hulls.iter().flatten().map(|p| Vec3::from(*p)).collect();
        let hulls: Vec<Collider> = v.hulls.iter().filter_map(|h| Collider::convex_hull(&h.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>())).collect();
        if hulls.is_empty() {
            continue;
        }
        let min = pts.iter().fold(Vec3::INFINITY, |a, b| a.min(*b));
        let max = pts.iter().fold(Vec3::NEG_INFINITY, |a, b| a.max(*b));
        out.push((hulls, min, max, st));
    }
    if !out.is_empty() {
        info!("{} stealth volumes", out.len());
    }
    commands.insert_resource(StealthVolumes(out));
}

#[allow(clippy::too_many_arguments)]
fn player_visibility(
    level: Option<Res<LevelInfo>>,
    volumes: Option<Res<StealthVolumes>>,
    vm: Option<Res<crate::kismet::Vm>>,
    swim: Res<crate::swim::Swim>,
    player: Query<(&Transform, &Player, Option<&LitActor>)>,
    mut vis: ResMut<PlayerVisibility>,
    mut last: Local<Option<[f32; 2]>>,
) {
    let Some(pv) = level.as_ref().and_then(|l| l.scene.player_vis.as_ref()) else { return };
    let Ok((t, p, lit)) = player.single() else { return };
    // the range: the scripts', else the stealth volume's (highest priority), the map's, the game's
    let volume = volumes.as_ref().and_then(|v| {
        v.0.iter()
            .filter(|(h, min, max, _)| t.translation.cmpge(*min).all() && t.translation.cmple(*max).all() && h.iter().any(|c| c.contains_point(Vec3::ZERO, Quat::IDENTITY, t.translation)))
            .max_by(|a, b| a.3[2].total_cmp(&b.3[2]))
            .map(|v| [v.3[0], v.3[1]])
    });
    let range = vm.as_ref().and_then(|v| v.player_vis).or(volume).or(pv.map).unwrap_or(pv.global);
    if std::env::var("DH_VIS_LOG").is_ok() && *last != Some(range) {
        info!("player vis range {range:?} (scripted {:?}, volume {volume:?})", vm.as_ref().and_then(|v| v.player_vis));
    }
    *last = Some(range);
    let raw = lit.map(|l| l.brightness).unwrap_or(0.4);
    let light = ((raw - range[0]) / (range[1] - range[0]).max(1e-3)).clamp(0.0, 1.0);
    let flat = p.velocity.with_y(0.0).length();
    let speed = if p.crouched && pv.sneak_still { 0.0 } else { (flat / crate::player::SPRINT_SPEED).clamp(0.0, 1.0) };
    // the stance: standing, walking, sprinting, sneaking .. swimming, swimming on the surface
    let stance = if swim.under.is_some() {
        6
    } else if swim.swimming() {
        7
    } else if p.crouched {
        3
    } else if p.sprinting && flat > 0.5 {
        2
    } else if flat > 0.3 {
        1
    } else {
        0
    };
    let mut value = 0.0;
    value = pv.light.apply(value, light);
    value = pv.speed.apply(value, speed);
    value += pv.bonus[stance];
    *vis = PlayerVisibility { light, speed, value, range };
}

/// The stealth shroud (the HUD effects movie's `hud_blackShroud_`: its bitmap stretched over the
/// screen): sneaking, the screen's edges darken, `BlackShroud.Open` tweening it in over 0.35 s
/// (`Strong.easeOut`) and `Close` out again; it hides once anyone has busted Corvo
/// (`m_SneakEffectHiddenMaxAttention`). Going into sneaking plays `m_pEnterStealthSoundEvent`.
#[derive(Component, Default)]
struct Shroud {
    from: f32,
    to: f32,
    t: f32,
}

/// The shroud's bitmap in the HUD effects movie.
const SHROUD_IMAGE: u32 = 41;
const SHROUD_TWEEN: f32 = 0.35;

fn spawn_shroud(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>) {
    let Some((h, _)) = ui.get(&mut images, "HUDFX", SHROUD_IMAGE) else { return };
    commands.spawn((
        Shroud::default(),
        ImageNode::new(h).with_color(Color::srgba(1.0, 1.0, 1.0, 0.0)).with_mode(bevy::ui::widget::NodeImageMode::Stretch),
        Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() },
        GlobalZIndex(-1),
        Pickable::IGNORE,
        DespawnOnExit(GameState::InGame),
        crate::hud::HudPart("DHE_StealthShroud"),
    ));
}

fn stealth_shroud(
    time: Res<Time>,
    player: Query<&Player>,
    npcs: Query<&crate::npc::Npc>,
    possession: Res<crate::possession::Possession>,
    stats: Res<crate::gameplay::PlayerStats>,
    mut shroud: Query<(&mut Shroud, &mut ImageNode)>,
    mut sounds: MessageWriter<crate::audio::PostEvent>,
    mut was: Local<bool>,
) {
    let Ok(p) = player.single() else { return };
    let busted = npcs.iter().any(|n| !n.is_down() && (n.attn_level >= 4 || n.alert == crate::npc::Alert::Combat && n.mode == crate::npc::Mode::Combat));
    let sneaking = p.crouched && !busted && possession.host.is_none() && !stats.dead;
    for (mut s, mut img) in &mut shroud {
        if sneaking != *was {
            s.from = img.color.alpha();
            s.to = if sneaking { 1.0 } else { 0.0 };
            s.t = 0.0;
        }
        s.t = (s.t + time.delta_secs() / SHROUD_TWEEN).min(1.0);
        let k = 1.0 - (1.0 - s.t).powi(5);
        let a = s.from + (s.to - s.from) * k;
        if (img.color.alpha() - a).abs() > 1e-4 {
            img.color.set_alpha(a);
        }
    }
    if std::env::var("DH_VIS_LOG").is_ok() && sneaking != *was {
        let by: Vec<&str> = npcs.iter().filter(|n| !n.is_down() && (n.attn_level >= 4 || n.alert == crate::npc::Alert::Combat && n.mode == crate::npc::Mode::Combat)).map(|n| n.name.as_str()).collect();
        info!("sneaking {sneaking} (crouched {}, busted by {by:?}, shroud {})", p.crouched, shroud.iter().count());
    }
    if sneaking && !*was {
        sounds.write(crate::audio::PostEvent::named("Snd_Power_P_Entering_Sneak", None));
    }
    *was = sneaking;
}
