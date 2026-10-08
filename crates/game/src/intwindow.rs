//! The interaction window (the HUD movie's `hud_intWindow`, class `InteractionsWindow`) right of
//! the crosshair: the name of what Corvo looks at (`_title_txt`: the body font at 23, white)
//! over a faint rule and what he can do with it (`_interactions_txt`: 22, pale green), on the
//! window's backing. Its bitmaps are the movie's (`flash`); its text is set as
//! `SetInteractions` sets it, and it shuts with nothing to do.

use crate::flash::{Clip, FlashClip, MovieTimelines};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct IntWindowPlugin;

impl Plugin for IntWindowPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Window_>().add_systems(OnEnter(GameState::InGame), |mut w: ResMut<Window_>| *w = Window_::default()).add_systems(Update, update_window.run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";
const SYMBOL: &str = "hud_intWindow";
/// `interactions_window_mc` on the stage
const AT: Vec2 = Vec2::new(691.1, 328.0);
/// the text fields: where (their text inside the 2-unit gutter), size, colour
const TITLE_AT: Vec2 = Vec2::new(4.0, 4.0);
const TITLE_SIZE: f32 = 23.0;
const LINES_AT: Vec2 = Vec2::new(12.0, 36.0);
const LINES_SIZE: f32 = 22.0;
const LINES_COLOR: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
/// `_interactions_txt`'s width (it wraps within it)
const LINES_W: f32 = 306.0;

#[derive(Resource, Default)]
struct Window_ {
    root: Option<Entity>,
}

#[derive(Component)]
struct WindowRoot;
#[derive(Component)]
struct WindowClip;
#[derive(Component)]
struct Title;
#[derive(Component)]
struct Lines;

#[allow(clippy::type_complexity)]
fn update_window(
    mut commands: Commands,
    mut state: ResMut<Window_>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    focus: Res<crate::interact::InteractFocus>,
    settings: Res<crate::settings::Settings>,
    mut roots: Query<(&mut Node, &mut Visibility), (With<WindowRoot>, Without<Title>, Without<Lines>)>,
    mut clip: Query<&mut FlashClip, With<WindowClip>>,
    mut title: Query<(&mut Text, &mut TextFont, &mut Node), (With<Title>, Without<Lines>, Without<WindowRoot>)>,
    mut lines: Query<(&mut Text, &mut TextFont, &mut Node), (With<Lines>, Without<Title>, Without<WindowRoot>)>,
) {
    let Ok(w) = window.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    // built under the HUD (hidden with it)
    let root = match state.root.filter(|r| roots.contains(*r)) {
        Some(r) => r,
        None => {
            let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
            let Some(c) = Clip::export(&tl, SYMBOL) else { return };
            let r = commands
                .spawn((WindowRoot, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h)))
                .with_children(|r| {
                    r.spawn((WindowClip, FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                    r.spawn((Title, Text::new(""), TextFont::default(), TextColor(Color::WHITE), TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                    r.spawn((Lines, Text::new(""), TextFont::default(), TextColor(LINES_COLOR), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                })
                .id();
            state.root = Some(r);
            return;
        }
    };
    let open = focus.0 && !focus.2.is_empty() && settings.interactions;
    if let Ok((mut n, mut v)) = roots.get_mut(root) {
        let p = off + AT * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
        let want = if open { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    if !open {
        return;
    }
    if let Ok(mut fc) = clip.single_mut() {
        fc.scale = s;
    }
    let set = |t: &mut Text, f: &mut TextFont, n: &mut Node, text: &str, size: f32, at: Vec2| {
        if t.0 != text {
            t.0 = text.to_string();
        }
        let fs = bevy::text::FontSize::Px(size * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        n.left = Val::Px(at.x * s);
        n.top = Val::Px(at.y * s);
    };
    if let Ok((mut t, mut f, mut n)) = title.single_mut() {
        set(&mut t, &mut f, &mut n, &focus.3, TITLE_SIZE, TITLE_AT);
    }
    if let Ok((mut t, mut f, mut n)) = lines.single_mut() {
        set(&mut t, &mut f, &mut n, &focus.2, LINES_SIZE, LINES_AT);
        n.width = Val::Px(LINES_W * s);
    }
}
