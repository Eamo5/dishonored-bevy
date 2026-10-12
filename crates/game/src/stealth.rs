//! How visible Corvo is (`DishonoredVisibilityComponent` with `Twk_PlayerVisibility`): his
//! light value, normalised over the darkest and brightest values the scripts, the stealth volume
//! he stands in (`DisStealthVolume`, by priority), the map or the game set, and his speed, each
//! through its curve, plus his stance's bonus. Each onlooker adds what nearness gives
//! (`npc::npc_perception`).

use crate::level::{LevelInfo, GROUP_WORLD};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use dhcook::format::{Light, LightKind};

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
    time: Res<Time>,
    rapier: ReadRapierContext,
    wl: Option<Res<crate::world_light::WorldLighting>>,
    (slide, auto): (Res<crate::player::Slide>, Res<crate::player::AutoCrouch>),
    player: Query<(&Transform, &Player)>,
    mut vis: ResMut<PlayerVisibility>,
    (mut last, mut raw_light): (Local<Option<[f32; 2]>>, Local<(f32, f32)>),
) {
    let Some(level) = level else { return };
    let Some(pv) = level.scene.player_vis.as_ref() else { return };
    let Ok((t, p)) = player.single() else { return };
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
    // (his light, read every tenth of a second)
    raw_light.0 -= time.delta_secs();
    if raw_light.0 <= 0.0 {
        raw_light.0 = 0.1;
        let ctx = rapier.single().ok();
        let baked = wl.as_ref().and_then(|w| w.volume.sample(t.translation)).map(|v| [lum(v.ambient), lum(v.directional)]).unwrap_or([0.0; 2]);
        raw_light.1 = light_value(&level.scene.lights, baked[0] + baked[1], t.translation, ctx.as_ref());
        if std::env::var("DH_VIS_LOG").is_ok() {
            info!("player raw light {:.3} at {:.1} (baked: ambient {:.4}, directional {:.4})", raw_light.1, t.translation, baked[0], baked[1]);
        }
    }
    let raw = raw_light.1;
    let light = ((raw - range[0]) / (range[1] - range[0]).max(1e-3)).clamp(0.0, 1.0);
    let flat = p.velocity.with_y(0.0).length();
    // (sneaking, or crouched by the world, counts as still; a slide doesn't)
    let sliding = p.crouched && slide.0.is_some();
    let speed = if p.crouched && !sliding && pv.sneak_still { 0.0 } else { (flat / crate::player::SPRINT_SPEED).clamp(0.0, 1.0) };
    // the stance: standing, walking, sprinting, sneaking .. swimming, swimming on the surface
    // (swimming, sliding and being crouched by the world come first, as roshan's r614/blink
    // research found: `bonus_autocrouching` -1)
    let stance = if swim.under.is_some() {
        6
    } else if swim.swimming() {
        7
    } else if sliding {
        4
    } else if p.crouched && auto.0 {
        5
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
    if std::env::var("DH_VIS_LOG").is_ok() && raw_light.0 == 0.1 {
        info!("player visibility {value:.2}: light {light:.2}, speed {speed:.2}, stance {stance}");
    }
    *vis = PlayerVisibility { light, speed, value, range };
}

/// Corvo's raw light value, as the original's visibility component reads his light environment
/// (measured on the original by roshan's r614/blink research, exact at ten points):
/// `(L / 4 pi)^(1/2.2)`, 0 below 0.0001 before the power. L is the light volume's baked light
/// there (`baked`: its ambient and directional radiance's luminance, which carries the static
/// lights) plus each live light's brightness times its colour's luminance (0.3 / 0.59 / 0.11 of
/// `(byte / 255)^2.2`) times its falloff. A point or spot light falls off as
/// `(1 - d^2/R^2)^FalloffExponent`, a spot also over its cone, and a light casting shadows needs
/// a clear line to him. A directional light counts where it reaches him; the sky light counts
/// twice its brightness everywhere.
pub fn light_value(lights: &[Light], baked: f32, p: Vec3, ctx: Option<&RapierContext>) -> f32 {
    let filter = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    // (a clear line from him: towards a light, stopping short of its fixture)
    let clear = |dir: Vec3, len: f32| ctx.is_none_or(|c| c.cast_ray(p, dir, len, true, filter).is_none());
    let mut sum = baked;
    // (the lights baked into the light volume are dark live: the volume carries them)
    for l in lights.iter().filter(|l| l.enabled && l.brightness > 0.0 && l.live) {
        let lum = ue_luminance(l.color) * l.brightness;
        match l.kind {
            LightKind::Sky => sum += 2.0 * lum,
            LightKind::Directional => {
                let to = -Vec3::from(l.direction).normalize_or(Vec3::NEG_Y);
                if !l.shadows || clear(to, 300.0) {
                    sum += lum;
                }
            }
            LightKind::Point | LightKind::Spot => {
                let d = Vec3::from(l.position) - p;
                let dist = d.length();
                if dist >= l.radius || l.radius <= 0.0 {
                    continue;
                }
                let mut f = (1.0 - (dist / l.radius).powi(2)).max(0.0).powf(l.falloff.max(0.0));
                if l.kind == LightKind::Spot && dist > 1e-3 {
                    // (UE3's cone: the square of the way from the outer cone to the inner)
                    let c = (-d / dist).dot(Vec3::from(l.direction).normalize_or(Vec3::NEG_Y));
                    let (inner, outer) = (l.inner_cone.min(l.outer_cone).to_radians().cos(), l.outer_cone.to_radians().cos());
                    f *= ((c - outer) / (inner - outer).max(1e-4)).clamp(0.0, 1.0).powi(2);
                }
                if f <= 0.0 || l.shadows && dist > 0.6 && !clear(d / dist, dist - 0.5) {
                    continue;
                }
                sum += lum * f;
            }
        }
    }
    let x = sum / (4.0 * std::f32::consts::PI);
    if x > 1e-4 { x.powf(1.0 / 2.2) } else { 0.0 }
}

fn lum(c: Vec3) -> f32 {
    c.dot(Vec3::new(0.3, 0.59, 0.11))
}

/// The luminance of a light's colour as the original's light environment takes it: the bytes'
/// `(c / 255)^2.2` (the cooked colour is the sRGB curve's).
fn ue_luminance(c: [f32; 3]) -> f32 {
    let byte = |v: f32| {
        let s = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round().clamp(0.0, 255.0) / 255.0
    };
    0.3 * byte(c[0]).powf(2.2) + 0.59 * byte(c[1]).powf(2.2) + 0.11 * byte(c[2]).powf(2.2)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn light(kind: LightKind, brightness: f32, rgb: [u8; 3]) -> Light {
        let lin = |b: u8| {
            let c = b as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        Light {
            kind,
            position: [0.0; 3],
            direction: [0.0, -1.0, 0.0],
            color: [lin(rgb[0]), lin(rgb[1]), lin(rgb[2])],
            brightness,
            radius: 0.0,
            falloff: 2.0,
            inner_cone: 0.0,
            outer_cone: 44.0,
            shadows: false,
            enabled: true,
            dynamic: false,
            actor: String::new(),
            dominant: false,
            shafts: None,
            live: true,
        }
    }

    #[test]
    fn the_light_value_matches_the_originals() {
        // roshan's r614/blink measurement on the Playground at (-5200, 6000): the sky light
        // (0.1, (203, 229, 255)) and the directional light (0.25, (164, 209, 255)): raw 0.1838
        let lights = [light(LightKind::Sky, 0.1, [203, 229, 255]), light(LightKind::Directional, 0.25, [164, 209, 255])];
        let v = light_value(&lights, 0.0, Vec3::ZERO, None);
        assert!((v - 0.1838).abs() < 0.002, "{v}");
        // and its lamp (Brightness 4, (255, 230, 194), radius 14 m, falloff 2) adding 0.628 at
        // 10 m: (1 - (10/14)^2)^2 x 4 x lum
        let mut lamp = light(LightKind::Point, 4.0, [255, 230, 194]);
        lamp.dynamic = true;
        lamp.radius = 14.0;
        lamp.position = [10.0, 0.0, 0.0];
        let f = (1.0f32 - (10.0f32 / 14.0).powi(2)).powi(2);
        let with = light_value(&[lights[0].clone(), lights[1].clone(), lamp], 0.0, Vec3::ZERO, None);
        let want = ((0.3026 + 4.0 * f * ue_luminance(light(LightKind::Point, 1.0, [255, 230, 194]).color)) / (4.0 * std::f32::consts::PI)).powf(1.0 / 2.2);
        assert!((with - want).abs() < 1e-4, "{with} {want}");
    }
}
