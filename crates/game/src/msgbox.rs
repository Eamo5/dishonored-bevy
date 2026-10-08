//! The questions the menus ask as `UI_Global` asks them (`gl_msgBox`, `ShowMessageBox`): the
//! screen veiled (its black and white backings, drifting blades, the vignette), the question in
//! the middle (`$NormalFont` 23, pale, 912 wide, centred), and its buttons on the bar below
//! (`gl_msgBox_btn`: YES, NO; the chosen one lit).

use crate::flash::{Clip, FlashClip};
use crate::menu::Menu;
use crate::GameState;
use bevy::prelude::*;
use bevy::text::{FontSize, LineBreak};
use bevy::window::PrimaryWindow;

pub struct MsgBoxPlugin;

impl Plugin for MsgBoxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (click, draw).chain().after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Component)]
struct MsgRoot;
#[derive(Component)]
struct MsgButton(usize);

const MOVIE: &str = "Global";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);

fn click(mut menu: ResMut<Menu>, buttons: Query<(&Interaction, &MsgButton), Changed<Interaction>>, scripted: Option<Res<crate::script::Scripted>>) {
    if scripted.is_some() {
        return;
    }
    for (i, b) in &buttons {
        if *i == Interaction::Pressed {
            menu.confirm_click = Some(b.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    menu: Res<Menu>,
    data: Res<crate::gamedata::Data>,
    mut timelines: ResMut<crate::flash::MovieTimelines>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<MsgRoot>>,
    mut shown: Local<Option<(String, usize, UVec2)>>,
) {
    let Ok(w) = window.single() else { return };
    let want = menu.question().map(|(q, c)| (q.to_string(), c, UVec2::new(w.width() as u32, w.height() as u32)));
    if want == *shown {
        return;
    }
    *shown = want.clone();
    for e in &roots {
        commands.entity(e).try_despawn();
    }
    let Some((question, choice, _)) = want else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let root = commands.spawn((MsgRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(120), DespawnOnExit(GameState::InGame))).id();
    let centre = Vec2::new(640.0, 360.0);
    if let Some(c) = Clip::export(&tl, "gl_msgBox") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        let p = off + centre * s;
        commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
    }
    // the question, middled on the box
    let p = off + (centre - Vec2::new(456.4, 60.0)) * s;
    let bx = commands
        .spawn((Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(912.8 * s), height: Val::Px(120.0 * s), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }, Pickable::IGNORE, ChildOf(root)))
        .id();
    // (its colour tags: §C_BLUE§ ... §C§)
    let clean = question.replace("§C_BLUE§", "").replace("§C§", "").replace("<br>", "\n");
    commands.spawn((Text::new(clean), TextFont { font_size: FontSize::Px(23.0 * s), ..default() }, TextColor(PALE), TextLayout::new(Justify::Center, LineBreak::WordBoundary), Node { max_width: Val::Px(912.8 * s), ..default() }, Pickable::IGNORE, ChildOf(bx)));
    // its buttons on the bar (`btnBar_mc` at (-16.15,136.2))
    let base = "DisGFxMoviePlayerBase_Texts";
    for (i, (key, dx)) in [("t_Yes", -165.0), ("t_No", 165.0)].into_iter().enumerate() {
        let at = centre + Vec2::new(-16.15 + dx, 136.2);
        let on = i == choice;
        if let Some(c) = Clip::export(&tl, "gl_msgBox_btn") {
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
            fc.scale = s;
            if on {
                fc.goto("", "over", true);
                // (its light brush: `_btn_mc`'s own `over`)
                fc.goto("_btn_mc", "over", true);
            }
            let p = off + at * s;
            commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
        }
        let p = off + (at - Vec2::new(121.05, 14.85)) * s;
        let b = commands
            .spawn((MsgButton(i), Button, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y - 8.0 * s), width: Val::Px(242.15 * s), height: Val::Px(46.0 * s), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }, ChildOf(root)))
            .id();
        commands.spawn((Text::new(data.text(base, key).to_uppercase()), TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(24.0 * s), ..default() }, TextColor(if on { DARK } else { PALE }), Pickable::IGNORE, ChildOf(b)));
    }
}
