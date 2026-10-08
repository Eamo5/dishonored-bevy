//! The mask's optics: [Zoom] magnifies the view once Corvo has the first optics upgrade
//! (`Twk_Upgrade_Spyglass1`); with the second (`Twk_Upgrade_Spyglass2`) [Use] while zoomed
//! switches to the stronger lens. The lens frames the view (`AltScreen_Effects.SpyGlass_Lens`)
//! and reads the distance to what's aimed at (`SpyglassDistance`). Sprinting puts it away
//! (`m_bAllowZoomWhileSprint`), with the original lens sounds.

use crate::audio::PostEvent;
use crate::bindings::{Act, Bindings};
use crate::gameplay::PlayerStats;
use crate::level::GROUP_WORLD;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct ZoomPlugin;

impl Plugin for ZoomPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Zoom>()
            .add_systems(OnEnter(GameState::InGame), spawn_lens)
            .add_systems(OnExit(GameState::InGame), |mut z: ResMut<Zoom>| *z = Zoom::default())
            .add_systems(Update, (zoom_input, lens).chain().before(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// Magnification of each lens.
const LEVELS: [f32; 3] = [1.0, 2.0, 4.0];

#[derive(Resource)]
pub struct Zoom {
    pub level: u8,
    /// the magnification now (blending towards the lens's)
    pub factor: f32,
}

impl Default for Zoom {
    fn default() -> Self {
        Zoom { level: 0, factor: 1.0 }
    }
}

#[allow(clippy::too_many_arguments)]
fn zoom_input(
    time: Res<Time>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<Bindings>),
    stats: Res<PlayerStats>,
    mut zoom: ResMut<Zoom>,
    player: Query<&Player>,
    menu: Res<crate::hud::Paused>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let Ok(p) = player.single() else { return };
    let has = |u: &str| stats.upgrades.iter().any(|x| x == u);
    let before = zoom.level;
    if !has("Twk_Upgrade_Spyglass1") || stats.dead || p.sprinting || p.locked || menu.0 {
        zoom.level = 0;
    } else if keys.just_pressed(bind.key(Act::Zoom)) {
        zoom.level = if zoom.level == 0 { 1 } else { 0 };
    } else if zoom.level > 0 && has("Twk_Upgrade_Spyglass2") && keys.just_pressed(bind.key(Act::Use)) {
        zoom.level = if zoom.level == 1 { 2 } else { 1 };
    }
    if zoom.level != before {
        let ev = match (before, zoom.level) {
            (0, _) => "Spyglass_In",
            (_, 0) => "Spyglass_Out",
            (1, 2) => "Spyglass_In_LVL2",
            _ => "Spyglass_Out_LVL2",
        };
        sfx.write(PostEvent::named(ev, None));
    }
    let target = LEVELS[zoom.level as usize % 3];
    let k = (time.delta_secs() * 10.0).min(1.0);
    zoom.factor += (target - zoom.factor) * k;
    if (zoom.factor - target).abs() < 1e-3 {
        zoom.factor = target;
    }
}

#[derive(Component)]
struct Lens;

#[derive(Component)]
struct LensDistance;

fn spawn_lens(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>) {
    let lens = ui.file(&mut images, "effects", "SpyGlass_Lens_01_m").map(|(h, _)| h);
    // the lens mask keeps its frame in red: black, as opaque as the red
    if let Some(mut img) = lens.as_ref().and_then(|h| images.get_mut(h)) {
        if let Some(data) = img.data.as_mut() {
            if data.first().is_some_and(|_| data.len() % 4 == 0) && data.chunks(4).any(|px| px[3] == 255 && px[0] > 0) {
                for px in data.chunks_mut(4) {
                    px[3] = px[0];
                    px[0] = 0;
                    px[1] = 0;
                    px[2] = 0;
                }
            }
        }
    }
    commands
        .spawn((
            Lens,
            Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), justify_content: JustifyContent::Center, align_items: AlignItems::End, ..default() },
            Visibility::Hidden,
            Pickable::IGNORE,
            GlobalZIndex(-1),
            DespawnOnExit(GameState::InGame),
        ))
        .with_children(|c| {
            // (the frame is the post-process graph's `PPG_LensCompose`; this stands in without it)
            if let Some(h) = lens.filter(|_| std::env::var("DH_NO_PPG").is_ok()) {
                c.spawn((ImageNode::new(h).with_mode(bevy::ui::widget::NodeImageMode::Stretch), Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }));
            }
            c.spawn((
                LensDistance,
                Text::new(""),
                TextFont { font_size: FontSize::Px(22.0), ..default() },
                TextColor(Color::srgb(0.9, 0.86, 0.75)),
                Node { margin: UiRect::bottom(percent(12)), ..default() },
            ));
        });
}

/// The lens frame and the distance read-out while zoomed.
fn lens(
    zoom: Res<Zoom>,
    attrs: Res<crate::gamedata::Data>,
    stats: Res<PlayerStats>,
    rapier: ReadRapierContext,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut frame: Query<&mut Visibility, With<Lens>>,
    mut text: Query<&mut Text, With<LensDistance>>,
) {
    let on = zoom.factor > 1.05;
    for mut v in &mut frame {
        let want = if on { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    if !on {
        return;
    }
    // the second lens measures distances (up to `SpyglassDistance`)
    let reach = if stats.upgrades.iter().any(|u| u == "Twk_Upgrade_Spyglass2") { attrs.attribute("SpyglassDistance", 1, &Default::default(), &[]) * 0.01 } else { 0.0 };
    let label = match (cam.single(), rapier.single()) {
        (Ok(c), Ok(ctx)) if reach > 0.0 => ctx
            .cast_ray(c.translation(), c.forward().as_vec3(), reach, true, QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD)))
            .map(|(_, d)| format!("{d:.0} m"))
            .unwrap_or_default(),
        _ => String::new(),
    };
    if let Ok(mut t) = text.single_mut() {
        if t.0 != label {
            t.0 = label;
        }
    }
}
