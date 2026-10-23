//! The questions the menus ask as `UI_Global` asks them (`gl_msgBox`, `ShowMessageBox`): the
//! screen veiled (its black and white backings, drifting blades, the vignette), the question in
//! the middle (`$NormalFont` 23, pale, 912 wide, centred), and its buttons on the bar below
//! (`gl_msgBox_btn`: YES, NO; the chosen one lit). A question with a countdown shows its
//! seconds over the words (`gl_msgBox_timer` 80 above the middle: `SetTimer`).

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
/// The countdown's seconds (`timer_mc`).
#[derive(Component)]
struct MsgTimer;

const MOVIE: &str = "Global";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);

fn click(mut menu: ResMut<Menu>, (mut journal, mut store, mut end): (ResMut<crate::journal::Journal>, ResMut<crate::store::Store>, Option<ResMut<crate::mission::MissionEnd>>), buttons: Query<(&Interaction, &MsgButton), Changed<Interaction>>, scripted: Option<Res<crate::script::Scripted>>) {
    if scripted.is_some() {
        return;
    }
    for (i, b) in &buttons {
        if *i == Interaction::Pressed {
            // (the journal's question, the store's, else the menu's)
            if journal.question().is_some() {
                journal.confirm_click = Some(b.0);
            } else if store.question().is_some() {
                store.confirm_click = Some(b.0);
            } else if let Some(e) = end.as_mut().filter(|e| e.asked()) {
                e.confirm_click = Some(b.0);
            } else {
                menu.confirm_click = Some(b.0);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    (menu, journal, store, end): (Res<Menu>, Res<crate::journal::Journal>, Res<crate::store::Store>, Option<Res<crate::mission::MissionEnd>>),
    data: Res<crate::gamedata::Data>,
    mut timelines: ResMut<crate::flash::MovieTimelines>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<MsgRoot>>,
    mut shown: Local<Option<(String, usize, UVec2)>>,
    (mut timers, mut count_shown, mut sfx): (Query<&mut FlashClip, With<MsgTimer>>, Local<Option<u32>>, MessageWriter<crate::audio::PostEvent>),
) {
    let Ok(w) = window.single() else { return };
    // the countdown's seconds as they go (`UpdateCountdown`)
    let count = menu.countdown();
    if count != *count_shown {
        *count_shown = count;
        for mut fc in &mut timers {
            fc.set_text("_txt", count.unwrap_or(0).to_string());
        }
    }
    let size = UVec2::new(w.width() as u32, w.height() as u32);
    let want = menu.question().or_else(|| journal.question()).or_else(|| store.question()).map(|(q, c)| (q.to_string(), c, size)).or_else(|| end.as_ref().and_then(|e| e.question(&data)).map(|(q, c)| (q, c, size)));
    // (the store's buttons: PURCHASE / CANCEL; the others' YES / NO)
    let buttons = if store.question().is_some() && !store.asks_leaving() && menu.question().is_none() && journal.question().is_none() {
        [("DisGFxMoviePlayerStore_Texts", "t_Purchase"), ("DisGFxMoviePlayerBase_Texts", "t_Cancel")]
    } else {
        [("DisGFxMoviePlayerBase_Texts", "t_Yes"), ("DisGFxMoviePlayerBase_Texts", "t_No")]
    };
    if want == *shown {
        return;
    }
    // (its opening sound: the global theme's `G_MsgBoxOpen`)
    if shown.is_none() && want.is_some() {
        sfx.write(crate::audio::PostEvent::named("UI_G_MsgBoxOpen", None));
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
    // its countdown
    if let (Some(n), Some(c)) = (count, Clip::export(&tl, "gl_msgBox_timer")) {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
        fc.scale = s;
        fc.set_text("_txt", n.to_string());
        let p = off + (centre + Vec2::new(0.0, -80.0)) * s;
        commands.spawn((MsgTimer, fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
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
    for (i, ((section, key), dx)) in buttons.into_iter().zip([-165.0, 165.0]).enumerate() {
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
        commands.spawn((Text::new(data.text(section, key).to_uppercase()), TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(24.0 * s), ..default() }, TextColor(if on { DARK } else { PALE }), Pickable::IGNORE, ChildOf(b)));
    }
}
