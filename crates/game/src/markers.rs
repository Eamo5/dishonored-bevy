//! Objective markers (Options > Objective Markers): each visible active task of a current
//! objective whose target the level scripts set (`DisSeqAct_UpdateTaskTarget`) gets the HUD
//! movie's `objectiveMarker_primary` over its target (following a character), as the HUD
//! tweak's `m_TaskMarkerSettings` place it: 10 units above it, at 90% from 100 m growing to
//! 150% at 5 m, fading out within 10 m, its description (`_description_mc`: the distance, the
//! body font at 21 over its brushed backing) shown while it is near the middle of the screen
//! (`m_fMarkerFocusDistance`); off the screen it is not kept at its edge
//! (`m_bKeepBBInMarkerArea`).
//!
//! The Heart's marks (`m_bShowHeartTargetMarkers`): while it is held, the level's runes and
//! bone charms show the movie's `runeMarker` / `boneCharmMarker`, as `m_RuneMarkerSettings` /
//! `m_BoneCharmMarkerSettings` place them: 3 units above, 30 stage units up the screen, 90% at
//! 10 m growing to 150% at 5 m, fading out as Corvo nears; these are kept at the screen's edge
//! when it is off it, the locator turned toward them.

use crate::flash::{turn_scale, Clip, FlashClip, MovieTimelines};
use crate::kismet::{TaskState, Vm};
use crate::npc::{FromSpawner, Npc};
use crate::player::PlayerCamera;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct MarkersPlugin;

impl Plugin for MarkersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (update_markers, heart_markers).run_if(in_state(GameState::InGame)));
    }
}

const MAX_MARKERS: usize = 6;
const MOVIE: &str = "HUD";
/// `m_TaskMarkerSettings`'s symbol, `m_OptionalTaskMarkerSettings`' (the same placing)
const SYMBOL: &str = "objectiveMarker_primary";
const SYMBOL_OPTIONAL: &str = "objectiveMarker_secondary";
/// `m_TaskMarkerSettings`: world offset (10 units), scale by distance (10000 units 90% to 500
/// units 150%), alpha by distance (1000 units full to 0 at none)
const OFFSET_Z: f32 = 0.1;
const SCALE: (f32, f32, f32, f32) = (100.0, 5.0, 0.9, 1.5);
const ALPHA_FROM: f32 = 10.0;
/// `m_fMarkerFocusDistance` (stage units from the middle)
const FOCUS: f32 = 225.0;
/// the description's text (`_description_mc.txt`, centred, its top above the marker)
const TEXT_TOP: f32 = -33.85;
const TEXT_SIZE: f32 = 21.0;
const TEXT_COLOR: Color = Color::srgb(225.0 / 255.0, 240.0 / 255.0, 212.0 / 255.0);

/// `m_RuneMarkerSettings` / `m_BoneCharmMarkerSettings`: symbols, scale by distance (1000
/// units 90% to 500 units 150%), alpha (full from 1000 units, none at none), the screen offset
const RUNE_SYMBOL: &str = "runeMarker";
const CHARM_SYMBOL: &str = "boneCharmMarker";
const HEART_SCALE: (f32, f32, f32, f32) = (10.0, 5.0, 0.9, 1.5);
const HEART_ALPHA_FROM: f32 = 10.0;
const HEART_OFFSET_Z: f32 = 0.03;
const HEART_SCREEN_UP: f32 = 30.0;
const HEART_MAX: usize = 16;
/// the area kept markers stay within: their 30-unit boxes inside the stage's edge
const AREA_MARGIN: f32 = 40.0;

/// A Heart mark of the pool, and whether it shows a bone charm's symbol.
#[derive(Component)]
struct HeartMarker(usize, bool);

#[derive(Component)]
struct HeartText(usize);

/// A marker of the pool, and whether it shows the optional objectives' symbol.
#[derive(Component)]
struct Marker(usize, bool);

#[derive(Component)]
struct MarkerText(usize);

/// Where the visible active tasks of the current objectives point (a character where it
/// stands now, by its spawner): marker positions.
pub fn task_points(vm: &Vm, npc_at: impl Fn(u32) -> Option<Vec3>, pickup_at: impl Fn(u32) -> Option<Vec3>) -> Vec<(Vec3, bool)> {
    task_targets(vm, npc_at, pickup_at).into_iter().map(|t| (t.0, t.3)).collect()
}

/// The task markers, and the actors and pickups they are (a pickup where it is now: a key on a
/// guard's belt), and whether they are an optional objective's.
pub fn task_targets(vm: &Vm, npc_at: impl Fn(u32) -> Option<Vec3>, pickup_at: impl Fn(u32) -> Option<Vec3>) -> Vec<(Vec3, Option<u32>, u32, bool)> {
    let mut points: Vec<(Vec3, Option<u32>, u32, bool)> = Vec::new();
    for path in &vm.objectives {
        let Some(o) = vm.g.objectives.iter().find(|o| o.path == *path) else { continue };
        if o.no_markers {
            continue;
        }
        if vm.tasks.get(path).is_some_and(|t| t.0 != TaskState::Active) {
            continue;
        }
        for task in &o.tasks {
            if !vm.tasks.get(&task.path).is_some_and(|t| t.0 == TaskState::Active && !t.1) {
                continue;
            }
            // (its targets: `path`, then `path#1`...)
            let mut targets: Vec<(&String, u32)> = vm.task_targets.iter().filter(|(k, _)| **k == task.path || k.strip_prefix(task.path.as_str()).is_some_and(|r| r.starts_with('#'))).map(|(k, v)| (k, v.0)).collect();
            targets.sort();
            for (_, a) in targets {
                let Some(ka) = vm.g.actors.get(a as usize) else { continue };
                // (a pickup no longer there: taken)
                let pickup = ka.pickup.map(|p| pickup_at(p));
                if pickup.is_some_and(|p| p.is_none()) {
                    continue;
                }
                let at = ka
                    .spawner
                    .and_then(&npc_at)
                    .map(|p| p + Vec3::Y * 1.1)
                    .or_else(|| pickup.flatten().map(|p| p + Vec3::Y * 0.3))
                    .unwrap_or(Vec3::from(ka.position) + Vec3::Y * 0.5);
                if !points.iter().any(|p| p.0.distance(at) < 1.0) {
                    points.push((at, ka.pickup, a, o.optional));
                }
            }
        }
    }
    points
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_markers(
    mut commands: Commands,
    vm: Option<Res<Vm>>,
    settings: Res<crate::settings::Settings>,
    paused: Res<crate::hud::Paused>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    cam: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    npcs: Query<(&FromSpawner, &Transform, &Npc)>,
    pickups: Query<(&crate::interact::Pickup, &Transform), Without<Npc>>,
    mut markers: Query<(&mut Marker, &mut FlashClip, &mut Node, &mut Visibility), Without<MarkerText>>,
    mut texts: Query<(&MarkerText, &mut Text, &mut TextFont, &mut TextColor, &mut Node, &mut Visibility, &ComputedNode), Without<Marker>>,
) {
    let (Some(vm), Ok((camera, cg)), Ok(w)) = (vm, cam.single(), window.single()) else { return };
    // the markers (built once, under the HUD)
    if markers.is_empty() {
        let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
        for i in 0..MAX_MARKERS {
            let Some(c) = Clip::export(&tl, SYMBOL) else { return };
            commands.spawn((Marker(i, false), FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h)));
            commands.spawn((
                MarkerText(i),
                Text::new(""),
                TextFont::default(),
                TextColor(TEXT_COLOR),
                TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap),
                Node { position_type: PositionType::Absolute, ..default() },
                Visibility::Hidden,
                Pickable::IGNORE,
                ChildOf(h),
            ));
        }
        return;
    }
    let mut points = if settings.markers && !paused.0 {
        task_points(&vm, |s| npcs.iter().find(|(f, _, n)| f.0 == s && !n.is_down()).map(|(_, t, _)| t.translation), |i| pickups.iter().find(|(p, _)| p.index == i).map(|(_, t)| t.translation))
    } else {
        Vec::new()
    };
    points.truncate(MAX_MARKERS);
    if std::env::var("DH_MARKER_LOG").is_ok() && !points.is_empty() {
        info!("markers: {:?}", points);
    }
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let mid = Vec2::new(w.width(), w.height()) * 0.5;
    let eye = cg.translation();
    let fwd = cg.forward().as_vec3();
    for (mut m, mut fc, mut node, mut vis) in &mut markers {
        // an optional objective's marker is the other symbol
        if let Some(&(_, optional)) = points.get(m.0) {
            if optional != m.1 {
                m.1 = optional;
                let tl = fc.tl.clone();
                if let Some(c) = Clip::export(&tl, if optional { SYMBOL_OPTIONAL } else { SYMBOL }) {
                    *fc = FlashClip::new(MOVIE, tl, c);
                }
            }
        }
        // where it shows: on the screen, ahead (else not at all)
        let shown = points.get(m.0).map(|p| p.0).and_then(|p| {
            let at = p + Vec3::Y * OFFSET_Z;
            ((at - eye).dot(fwd) > 0.1).then(|| camera.world_to_viewport(cg, at).ok()).flatten().filter(|v| v.x >= 0.0 && v.y >= 0.0 && v.x <= w.width() && v.y <= w.height()).map(|v| (v, (at - eye).length()))
        });
        let Some((pos, d)) = shown else {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            for (t, _, _, _, _, mut tv, _) in &mut texts {
                if t.0 == m.0 && *tv != Visibility::Hidden {
                    *tv = Visibility::Hidden;
                }
            }
            continue;
        };
        // the tweak's variations by distance
        let k = ((SCALE.0 - d) / (SCALE.0 - SCALE.1)).clamp(0.0, 1.0);
        let scale = SCALE.2 + (SCALE.3 - SCALE.2) * k;
        let alpha = (d / ALPHA_FROM).clamp(0.0, 1.0);
        let focused = (pos - mid).length() / s < FOCUS;
        node.left = Val::Px(pos.x);
        node.top = Val::Px(pos.y);
        fc.scale = s * scale;
        fc.alpha = alpha;
        fc.set_visible("_description_mc", focused);
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
        // the distance, in the description
        let label = format!("{:.0} m", d);
        for (t, mut text, mut font, mut col, mut tn, mut tv, cn) in &mut texts {
            if t.0 != m.0 {
                continue;
            }
            if text.0 != label {
                text.0 = label.clone();
            }
            let fs = bevy::text::FontSize::Px(TEXT_SIZE * s * scale);
            if font.font_size != fs {
                font.font_size = fs;
            }
            let size = cn.size() * cn.inverse_scale_factor();
            tn.left = Val::Px(pos.x - size.x * 0.5);
            tn.top = Val::Px(pos.y + TEXT_TOP * s * scale);
            col.0 = TEXT_COLOR.with_alpha(alpha);
            let want = if focused { Visibility::Inherited } else { Visibility::Hidden };
            if *tv != want {
                *tv = want;
            }
        }
    }
}

/// The Heart's marks over the runes and bone charms still to be found, while it is held.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn heart_markers(
    mut commands: Commands,
    (powers, settings): (Res<crate::powers::Powers>, Res<crate::settings::Settings>),
    (paused, stats): (Res<crate::hud::Paused>, Res<crate::gameplay::PlayerStats>),
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    cam: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    pickups: Query<(&crate::interact::Pickup, &Transform), Without<Npc>>,
    mut markers: Query<(&mut HeartMarker, &mut FlashClip, &mut Node, &mut Visibility), (Without<HeartText>, Without<Marker>, Without<MarkerText>)>,
    mut texts: Query<(&HeartText, &mut Text, &mut TextFont, &mut TextColor, &mut Node, &mut Visibility, &ComputedNode), (Without<HeartMarker>, Without<Marker>, Without<MarkerText>)>,
) {
    let (Ok((camera, cg)), Ok(w)) = (cam.single(), window.single()) else { return };
    if markers.is_empty() {
        let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
        for i in 0..HEART_MAX {
            let Some(c) = Clip::export(&tl, RUNE_SYMBOL) else { return };
            commands.spawn((HeartMarker(i, false), FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h)));
            commands.spawn((
                HeartText(i),
                Text::new(""),
                TextFont::default(),
                TextColor(TEXT_COLOR),
                TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap),
                Node { position_type: PositionType::Absolute, ..default() },
                Visibility::Hidden,
                Pickable::IGNORE,
                ChildOf(h),
            ));
        }
        return;
    }
    let eye = cg.translation();
    let held = powers.selected == crate::powers::Power::Heart && !paused.0 && !stats.dead && settings.heart_markers;
    let mut points: Vec<(Vec3, bool)> = if held {
        pickups
            .iter()
            .filter_map(|(p, t)| match p.kind {
                crate::interact::PickupKind::Rune => Some((t.translation, false)),
                crate::interact::PickupKind::BoneCharm => Some((t.translation, true)),
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };
    points.sort_by(|a, b| a.0.distance(eye).total_cmp(&b.0.distance(eye)));
    points.truncate(HEART_MAX);
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let size = Vec2::new(w.width(), w.height());
    let mid = size * 0.5;
    let half = mid - Vec2::splat(AREA_MARGIN * s);
    let to_cam = cg.affine().inverse();
    for (mut m, mut fc, mut node, mut vis) in &mut markers {
        let Some(&(p, charm)) = points.get(m.0) else {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            for (t, _, _, _, _, mut tv, _) in &mut texts {
                if t.0 == m.0 && *tv != Visibility::Hidden {
                    *tv = Visibility::Hidden;
                }
            }
            continue;
        };
        if charm != m.1 {
            m.1 = charm;
            let tl = fc.tl.clone();
            if let Some(c) = Clip::export(&tl, if charm { CHARM_SYMBOL } else { RUNE_SYMBOL }) {
                *fc = FlashClip::new(MOVIE, tl, c);
            }
        }
        let at = p + Vec3::Y * HEART_OFFSET_Z;
        let d = (at - eye).length();
        let k = ((HEART_SCALE.0 - d) / (HEART_SCALE.0 - HEART_SCALE.1)).clamp(0.0, 1.0);
        let scale = HEART_SCALE.2 + (HEART_SCALE.3 - HEART_SCALE.2) * k;
        let alpha = (d / HEART_ALPHA_FROM).clamp(0.0, 1.0);
        // on the screen (raised by its offset), else kept at the edge of the marker area
        let local = to_cam.transform_point3(at);
        let seen = (local.z < -0.1).then(|| camera.world_to_viewport(cg, at).ok()).flatten().map(|v| v - Vec2::Y * HEART_SCREEN_UP * s * scale);
        let inside = |v: Vec2| (v - mid).abs().cmple(half).all();
        let (pos, kept) = match seen.filter(|v| inside(*v)) {
            Some(v) => (v, None),
            None => {
                let dir = match seen {
                    Some(v) => (v - mid).normalize_or(Vec2::Y),
                    None => Vec2::new(local.x, -local.y).normalize_or(Vec2::Y),
                };
                let t = (half.x / dir.x.abs().max(1e-4)).min(half.y / dir.y.abs().max(1e-4));
                (mid + dir * t, Some(dir))
            }
        };
        node.left = Val::Px(pos.x);
        node.top = Val::Px(pos.y);
        fc.scale = s * scale;
        fc.alpha = alpha;
        // the locator points the way when it is kept at the edge
        fc.set_visible("_locator_mc", kept.is_some());
        if let Some(dir) = kept {
            if let Some((mm, _)) = fc.clip.placed_mut("_locator_mc") {
                *mm = turn_scale(dir.x.atan2(-dir.y).to_degrees() + 180.0, 1.0, 1.0);
            }
        }
        let focused = kept.is_none() && (pos - mid).length() / s < FOCUS;
        fc.set_visible("_description_mc", focused);
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
        let label = format!("{:.0} m", d);
        for (t, mut text, mut font, mut col, mut tn, mut tv, cn) in &mut texts {
            if t.0 != m.0 {
                continue;
            }
            if text.0 != label {
                text.0 = label.clone();
            }
            let fs = bevy::text::FontSize::Px(TEXT_SIZE * s * scale);
            if font.font_size != fs {
                font.font_size = fs;
            }
            let sz = cn.size() * cn.inverse_scale_factor();
            tn.left = Val::Px(pos.x - sz.x * 0.5);
            tn.top = Val::Px(pos.y + TEXT_TOP * s * scale);
            col.0 = TEXT_COLOR.with_alpha(alpha);
            let want = if focused { Visibility::Inherited } else { Visibility::Hidden };
            if *tv != want {
                *tv = want;
            }
        }
    }
}
