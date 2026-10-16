//! The main menu's missions as `UI_MainMenu` draws them (`m_missions`: `MissionsScreen`): its
//! title on its brush, and the missions in the slide (`_common.SlideMenu`), each a card
//! (`m_missions_item`) with its number, its picture (`MissionsScreen_<x>_Large`, loaded whole
//! and centred on its loader: `ImgLoader` without auto-fit) and its name up its banner, a
//! padlock on one without its start (`stop_lock`). The chosen card is whole and raised 35, the
//! next ones 80% (alpha too), then 75%, the rest 70%, each set after the last at its scaled
//! width, the row slid so the chosen one sits half a card in (`SetItemsSize`,
//! `onSelectionUpdated`). Its help bar (`MissionsScreen`): A Accept (locked on a locked
//! mission), X Mission stats (locked without them: `bStatsAvailable`), B Back; X shows the
//! chosen mission's statistics as last shown (`OpenMissionStats`), or fails.

use crate::flash::{Clip, FlashClip, Mat};
use crate::helpbar::{Btn, Entry};
use crate::menu::{Menu, MenuItem};
use crate::save::SaveSlots;
use crate::GameState;
use bevy::prelude::*;
use bevy::text::{FontSize, LineBreak};
use bevy::window::PrimaryWindow;

pub struct MissionScreenPlugin;

impl Plugin for MissionScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (see_stats, draw).chain().after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Component)]
struct MissionRoot;

const MOVIE: &str = "MainMenu";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);
/// The missions' code among the menu's screens (`Menu::front_page`).
pub const CODE: u8 = 16;
/// The slide in `m_missions` (`_slide_mc`: at (-566.1,-69.65), turned).
const SLIDE_AT: Vec2 = Vec2::new(640.0 - 566.1, 360.0 - 69.65);
const SLIDE_M: [f32; 4] = [0.99599, -0.08949, 0.08949, 0.99599];
/// A card's size (`itemW`, its height: its strokes, padlock and picture's loader spanning it):
/// the slide's maker (`MovieClipCreator.CreateElements`, its origin `center`) sets each half a
/// card in and down, the next after it less `_offsetBtnX` (100).
const ITEM_W: f32 = 500.5;
const ITEM_H: f32 = 765.0;
const OFFSET_X: f32 = -100.0;

/// The campaign's missions as the exe's list names them (`SGS_DisMission0..8`): their
/// chapters' tags and pictures.
pub const MISSIONS: [(&str, &str); 9] = [
    ("Chapter2Info_Dishonored", "Prison"),
    ("Chapter4Info_HighOverseer", "Overseer"),
    ("Chapter5Info_HouseOfPleasure", "Brothel"),
    ("Chapter6Info_RoyalPhysician", "Bridge"),
    ("Chapter7Info_LadyBoyle", "Boyle"),
    ("Chapter8Info_LordRegent", "TowerReturn"),
    ("Chapter9Info_FloodedDistrict", "FloodedDistrict"),
    ("Chapter10Info_ReturnToHoundPits", "Hub"),
    ("Chapter12Info_Lighthouse", "Lighthouse"),
];

/// A mission's name: its chapter's title.
pub fn mission_name(data: &crate::gamedata::Data, i: usize) -> String {
    MISSIONS.get(i).and_then(|(tag, _)| data.0.chapters.iter().find(|c| c.tag == *tag)).map(|c| c.title.clone()).unwrap_or_default()
}

/// A card's size and look at its distance from the chosen one (`SetItemsSize`): scale and
/// alpha (100: the chosen), raised (the chosen: 35).
fn card_look(i: usize, sel: usize) -> (f32, f32) {
    match i.abs_diff(sel) {
        0 => (1.0, -35.0),
        1 => (0.8, 0.0),
        2 => (0.75, 0.0),
        _ => (0.7, 0.0),
    }
}

/// A point of the slide (its own coordinates) on the stage.
fn slide_pt(p: Vec2) -> Vec2 {
    SLIDE_AT + Vec2::new(SLIDE_M[0] * p.x + SLIDE_M[2] * p.y, SLIDE_M[1] * p.x + SLIDE_M[3] * p.y)
}

/// A text box on the stage centred at `c`, turned, its words middled up and down.
#[allow(clippy::too_many_arguments)]
fn text_box(commands: &mut Commands, root: Entity, s: f32, off: Vec2, c: Vec2, size: Vec2, turn: f32, words: String, font: TextFont, color: Color, tall: bool) {
    let p = off + (c - size * 0.5) * s;
    let bx = commands
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(size.x * s), height: Val::Px(size.y * s), align_items: AlignItems::Center, ..default() },
            UiTransform { rotation: Rot2::degrees(turn), ..default() },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    let t = commands.spawn((Text::new(words), font, TextColor(color), TextLayout::new(Justify::Left, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(bx))).id();
    if tall {
        commands.entity(t).insert(UiTransform { scale: Vec2::new(1.0, 1.2), ..default() });
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    menu: Res<Menu>,
    slots: Res<SaveSlots>,
    mut timelines: ResMut<crate::flash::MovieTimelines>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    data: Res<crate::gamedata::Data>,
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<MissionRoot>>,
    mut shown: Local<Option<(usize, Vec<String>, UVec2, u64)>>,
    records: Res<crate::mission::MissionRecords>,
) {
    let Ok(w) = window.single() else { return };
    let page = menu.front_page().filter(|p| p.0 == CODE);
    let key = page.as_ref().map(|p| (menu.selected(), p.1.clone(), UVec2::new(w.width() as u32, w.height() as u32), slots.stamp()));
    if key == *shown {
        return;
    }
    *shown = key;
    for e in &roots {
        commands.entity(e).try_despawn();
    }
    let Some((_, labels)) = page else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let root = commands.spawn((MissionRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(55), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    let node = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
    let title_font = |size: f32| TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(size * s), ..default() };
    // the screen: the slide's arrows (no downloadable missions: no tabs; its title over the
    // cards, below)
    let screen = |title: bool| {
        let c = Clip::export(&tl, "m_missions")?;
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        for part in ["_tabs_mc", "_tabsBkgd_mc"] {
            fc.set_visible(part, false);
        }
        for part in ["_slide_mc", "_mouseArea_L", "_mouseArea_R"] {
            fc.set_visible(part, !title);
        }
        fc.set_visible("_title_mc", title);
        Some(fc)
    };
    if let Some(fc) = screen(false) {
        commands.spawn((fc, node(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
    }
    // the cards: each after the last at its scaled width less the overlap, the row slid to put
    // the chosen half a card in
    let sel = menu.selected().min(labels.len().saturating_sub(1));
    let mut xs = Vec::with_capacity(labels.len());
    let mut x = 0.5 * ITEM_W;
    for i in 0..labels.len() {
        xs.push(x);
        x += (ITEM_W + OFFSET_X) * card_look(i, sel).0;
    }
    let shift = -xs.get(sel).copied().unwrap_or(0.0) + 0.5 * ITEM_W;
    let turn = SLIDE_M[1].atan2(SLIDE_M[0]).to_degrees();
    // (drawn from the furthest in, the chosen last: on top)
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(i.abs_diff(sel)));
    for i in order {
        let (k, dy) = card_look(i, sel);
        let at = slide_pt(Vec2::new(shift + xs[i], -10.0 + 0.5 * ITEM_H + dy));
        let card = |p: Vec2| at + Vec2::new(SLIDE_M[0] * p.x + SLIDE_M[2] * p.y, SLIDE_M[1] * p.x + SLIDE_M[3] * p.y) * k;
        let locked = menu.item_slot(i).and_then(|slot| slots.info(slot)).is_none();
        let on = i == sel;
        let alpha = if on { 1.0 } else { k };
        // its picture, centred on its loader (`_img_mc` (0,-252), `loader_mc` (7,22.4))
        if let Some((h, size)) = MISSIONS.get(i).and_then(|m| ui.file(&mut images, "missions", &format!("MissionsScreen_{}_Large", m.1))) {
            let (iw, ih) = (size.x * k, size.y * k);
            let c = card(Vec2::new(7.0, -252.0 + 22.4));
            commands.spawn((
                ImageNode::new(h).with_color(Color::WHITE.with_alpha(alpha)),
                Node { position_type: PositionType::Absolute, left: Val::Px(off.x + (c.x - iw * 0.5) * s), top: Val::Px(off.y + (c.y - ih * 0.5) * s), width: Val::Px(iw * s), height: Val::Px(ih * s), ..default() },
                UiTransform { rotation: Rot2::degrees(turn), ..default() },
                Pickable::IGNORE,
                ChildOf(root),
            ));
        }
        // the card over it: its strokes, padlock, number and banner
        if let Some(c) = Clip::export(&tl, "m_missions_item") {
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
            fc.scale = s;
            fc.m = [SLIDE_M[0] * k, SLIDE_M[1] * k, SLIDE_M[2] * k, SLIDE_M[3] * k, 0.0, 0.0];
            fc.alpha = alpha;
            fc.set_visible("_img_mc", false);
            let label = match (locked, on) {
                (true, true) => "over_lock",
                (true, false) => "stop_lock",
                (false, true) => "over",
                (false, false) => "default",
            };
            fc.goto("", label, on);
            commands.spawn((fc, node(at), Pickable::IGNORE, ChildOf(root)));
        }
        // its number (`_missionNumber_mc`: "01", `$TitleFont` 130, its shadow under it)
        let number = format!("{:02}", i + 1);
        let num_turn: Mat = [0.99, 0.11, -0.11, 0.99, 0.0, 0.0];
        let num_deg = num_turn[1].atan2(num_turn[0]).to_degrees() + turn;
        for (p, color) in [(Vec2::new(-87.65, -14.45), DARK), (Vec2::new(-79.7, -13.55), PALE)] {
            let c = card(p + Vec2::new(-79.0 + 119.0, -88.6 + 77.0));
            text_box(&mut commands, root, s, off, c, Vec2::new(240.0, 156.0) * k, num_deg, number.clone(), title_font(130.0 * k), color.with_alpha(alpha), true);
        }
        // its name up its banner (`_nameBkgd_mc` turned -83 degrees: `txt_mc` (16.5,3.2))
        let name = labels.get(i).cloned().unwrap_or_default().to_uppercase();
        let ban: Mat = [0.12, -0.99, 0.99, 0.12, -226.8, 76.0];
        let mid = Vec2::new(16.5 + 2.3 + 209.4, 3.2 + 1.05 + 18.0);
        let c = card(Vec2::new(ban[4] + ban[0] * mid.x + ban[2] * mid.y, ban[5] + ban[1] * mid.x + ban[3] * mid.y));
        text_box(&mut commands, root, s, off, c, Vec2::new(418.8, 36.0) * k, ban[1].atan2(ban[0]).to_degrees() + turn, name, title_font(30.0 * k), PALE.with_alpha(alpha), false);
        // its hit area (`mouseArea`)
        let (a, b) = (card(Vec2::new(-240.0, -570.0)), card(Vec2::new(250.0, 110.0)));
        let (lo, hi) = (a.min(b), a.max(b));
        commands.spawn((MenuItem(i), Button, Node { position_type: PositionType::Absolute, left: Val::Px(off.x + lo.x * s), top: Val::Px(off.y + lo.y * s), width: Val::Px((hi.x - lo.x) * s), height: Val::Px((hi.y - lo.y) * s), ..default() }, ChildOf(root)));
    }
    // the title on its brush, over the cards
    if let Some(fc) = screen(true) {
        commands.spawn((fc, node(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
    }
    let title = data.text("DisGFxMoviePlayerMainMenu_Texts", "t_MissionsScreenDesc").to_uppercase();
    text_box(&mut commands, root, s, off, Vec2::new(95.0 + 345.0, 77.7 + 24.7), Vec2::new(690.0, 49.4), -1.7, title, title_font(38.0), DARK, true);
    // the help bar: the chosen mission's lock and statistics
    if let Some(lib) = timelines.get("lib") {
        let base = "DisGFxMoviePlayerBase_Texts";
        let locked = menu.item_slot(sel).and_then(|slot| slots.info(slot)).is_none();
        let entries = [
            Entry::new(Btn::A, data.text(base, "t_Validate")).locked(locked),
            Entry::new(Btn::X, data.text("DisGFxMoviePlayerMainMenu_Texts", "t_SeeMissionStats")).locked(!records.has(sel)),
            Entry::new(Btn::B, data.text(base, "t_Back")),
        ];
        crate::helpbar::spawn(&mut commands, root, lib, s, off + crate::helpbar::END * s, &entries, &mut ui, &mut images, false);
    }
}

/// X on the missions (`XPressed`): the chosen mission's statistics as last shown, over the
/// menu (`OpenMissionStats`); none kept, it fails.
#[allow(clippy::too_many_arguments)]
fn see_stats(
    mut commands: Commands,
    menu: Res<Menu>,
    keys: Res<ButtonInput<KeyCode>>,
    (help, scripted, up, taps): (Query<(&Interaction, &crate::helpbar::HelpButton), Changed<Interaction>>, Option<Res<crate::script::Scripted>>, Option<Res<crate::mission::MissionEnd>>, Res<crate::helpbar::Taps>),
    records: Res<crate::mission::MissionRecords>,
    (data, fonts): (Res<crate::gamedata::Data>, Res<crate::ui_fonts::UiFonts>),
    (mut ui, mut images, mut timelines): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>, ResMut<crate::flash::MovieTimelines>),
    window: Query<&Window, With<PrimaryWindow>>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
) {
    if up.is_some() || menu.question().is_some() || menu.front_page().is_none_or(|p| p.0 != CODE) {
        return;
    }
    let click = crate::helpbar::clicked(&help).filter(|_| scripted.is_none());
    if click != Some(Btn::X) && !Btn::X.just_pressed(&keys, &taps) {
        return;
    }
    let Ok(w) = window.single() else { return };
    let shown = crate::mission::show_record(&mut commands, menu.selected(), &records, &data, &fonts, &mut ui, &mut images, &mut timelines, w);
    sfx.write(crate::audio::PostEvent::named(if shown { "UI_Validation" } else { "UI_Failure" }, None));
}
