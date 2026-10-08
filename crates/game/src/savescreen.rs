//! The save and load screens as `UI_LoadGame` draws them (`m_saveGame_` / `m_loadGame_` at the
//! stage's middle): the shard texture behind, the title on its brush (`lib_titleMc`, "SAVE
//! GAME" / "LOAD", dark), the list turned -2.5 degrees at (125,155) (`LoadGameList`: one
//! column, 7 rows 71.5 apart; each `m_loadGameList_item` with its mission strip, its date
//! (`$TitleFont` 23) over its chapter (`$NormalFont` 25, after an `AUTOSAVE - ` /
//! `QUICKSAVE - ` prefix), the chosen one `over`: light, its words dark, slid 15 right; ghost
//! rows `disabled` up to 7), and in the main menu the save's mission picture with its date and
//! name up its banners (`LoadGameDetails`, turned -83 degrees). In save mode the first row
//! makes a new save (`t_CreateNewSave`).

use crate::flash::{Clip, FlashClip, Mat};
use crate::menu::{Menu, MenuItem};
use crate::save::SaveSlots;
use crate::GameState;
use bevy::prelude::*;
use bevy::text::{FontSize, LineBreak};
use bevy::window::PrimaryWindow;

pub struct SaveScreenPlugin;

impl Plugin for SaveScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, draw.after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Component)]
struct SaveRoot;

const MOVIE: &str = "LoadGame";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);
/// the list's place and turn (`_list_mc`)
const LIST_AT: Vec2 = Vec2::new(125.0, 155.0);
const LIST_M: [f32; 4] = [0.99915, -0.04362, 0.04362, 0.99915];
const ROWS: usize = 7;
const ROW_Y0: f32 = 36.0;
const ROW_PITCH: f32 = 71.5;
const MENU_BASE: &str = "DisGFxMoviePlayerMenuBase_Texts";

/// A point of the list on the stage.
fn list_pt(x: f32, y: f32) -> Vec2 {
    LIST_AT + Vec2::new(LIST_M[0] * x + LIST_M[2] * y, LIST_M[1] * x + LIST_M[3] * y)
}

/// A save's date, in local time (`saveDate`).
pub fn local_date(unix: u64) -> String {
    #[cfg(windows)]
    // SAFETY: plain Win32 time conversions on stack values
    unsafe {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        let t = (unix + 11_644_473_600) * 10_000_000;
        let ft = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut loc: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) != 0 && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut loc) != 0 {
            // (`m_DateFormat`: "month/day/year - hour:minute:second")
            return format!("{:02}/{:02}/{:04} - {:02}:{:02}:{:02}", loc.wMonth, loc.wDay, loc.wYear, loc.wHour, loc.wMinute, loc.wSecond);
        }
    }
    // (the civil date in UTC)
    let days = (unix / 86400) as i64;
    let secs = unix % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{m:02}/{d:02}/{y:04} - {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
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
    roots: Query<Entity, With<SaveRoot>>,
    mut shown: Local<Option<(u8, usize, Vec<String>, UVec2, u64)>>,
) {
    let Ok(w) = window.single() else { return };
    let page = menu.front_page().filter(|p| p.0 == 5 || p.0 == 6);
    let key = page.as_ref().map(|p| (p.0, menu.selected(), p.1.clone(), UVec2::new(w.width() as u32, w.height() as u32), slots.stamp()));
    if key == *shown {
        return;
    }
    *shown = key;
    for e in &roots {
        commands.entity(e).try_despawn();
    }
    let Some((code, labels)) = page else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let save_mode = code == 6;
    let in_game = !menu.in_main();
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let root = commands.spawn((SaveRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(55), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    let node = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
    let title_font = |size: f32| TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(size * s), ..default() };
    let normal_font = |size: f32| TextFont { font_size: FontSize::Px(size * s), ..default() };
    let sel = menu.selected();
    let n = labels.len();
    let first = if n <= ROWS { 0 } else { sel.saturating_sub(ROWS - 1).min(n - ROWS) };
    // the screen: the texture, the list's rules, the details (main menu only), the title brush
    if let Some(c) = Clip::export(&tl, if save_mode { "m_saveGame_" } else { "m_loadGame_" }) {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        fc.set_visible("_list_mc.maskContent", false);
        let chosen = menu.item_slot(sel).and_then(|slot| slots.info(slot));
        if in_game || chosen.is_none() {
            fc.set_visible("_details_mc", false);
        } else if crate::loading::mission_art(chosen.map(|c| c.0).unwrap_or("")).is_some() {
            fc.set_visible("_details_mc._empty_mc", false);
        }
        commands.spawn((fc, node(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
    }
    let title = data.text(MENU_BASE, if save_mode { "t_SaveGame" } else { "t_LoadGame" }).to_uppercase();
    text_box(&mut commands, root, s, off, Vec2::new(97.0 + 345.0, 80.3 + 24.7), Vec2::new(690.0, 49.4), -2.0, title, title_font(38.0), DARK, true);
    // the rows
    for v in 0..ROWS {
        let i = first + v;
        let y = ROW_Y0 + ROW_PITCH * v as f32;
        let slot = labels.get(i).and(menu.item_slot(i));
        let info = slot.and_then(|sl| slots.info(sl));
        let on = slot.is_some() && i == sel;
        let x = if on { 15.0 } else { 0.0 };
        if let Some(c) = Clip::export(&tl, "m_loadGameList_item") {
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
            fc.scale = s;
            fc.m = [LIST_M[0], LIST_M[1], LIST_M[2], LIST_M[3], 0.0, 0.0];
            let t2 = fc.tl.clone();
            if let Some(b) = fc.clip.child_mut("_bkgd_mc.mc") {
                b.goto(&t2, i % 4, false);
            }
            if slot.is_none() {
                fc.goto("", "disabled", true);
            } else if on {
                fc.goto("", "over", true);
            }
            commands.spawn((fc, node(list_pt(x, y)), Pickable::IGNORE, ChildOf(root)));
        }
        let Some(slot) = slot else { continue };
        let color = if on { DARK } else { PALE };
        match info {
            Some((map, mission, at)) => {
                // the mission's strip (`img_mc`: 256 x 64, fitted to its height)
                if let Some((h, size)) = crate::loading::mission_art(map).and_then(|a| ui.file(&mut images, "missions", &format!("MissionsScreen_{a}_Small"))) {
                    let k = (64.0 / size.y.max(1.0)).min(256.0 / size.x.max(1.0));
                    let (iw, ih) = (size.x * k, size.y * k);
                    let c = list_pt(x + 158.15, y);
                    let p = off + (c - Vec2::new(iw, ih) * 0.5) * s;
                    commands.spawn((
                        ImageNode::new(h),
                        Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(iw * s), height: Val::Px(ih * s), ..default() },
                        UiTransform { rotation: Rot2::degrees(-2.5), ..default() },
                        Pickable::IGNORE,
                        ChildOf(root),
                    ));
                }
                let prefix = match slot {
                    0 => data.text("Twk_GFxMoviePlayerMenuBase", "m_QuicksavePrefix"),
                    crate::save::AUTOSAVE_SLOT => data.text("Twk_GFxMoviePlayerMenuBase", "m_AutosavePrefix"),
                    _ => String::new(),
                };
                text_box(&mut commands, root, s, off, list_pt(x + 496.1, y - 14.75), Vec2::new(411.45, 36.1), -2.5, local_date(at), title_font(23.0), color, true);
                text_box(&mut commands, root, s, off, list_pt(x + 495.7, y + 12.6), Vec2::new(410.55, 30.75), -2.5, format!("{prefix}{mission}"), normal_font(25.0), color, false);
            }
            None => {
                let words = data.text(MENU_BASE, "t_CreateNewSave").to_uppercase();
                text_box(&mut commands, root, s, off, list_pt(x + 446.35, y), Vec2::new(304.0, 36.2), -2.5, words, normal_font(23.0), if on { DARK } else { PALE.with_alpha(0.7) }, true);
            }
        }
        // the row's hit area (`btn`)
        let (a, b) = (list_pt(6.35, y - 31.65), list_pt(759.3, y + 30.4));
        let (lo, hi) = (a.min(b), a.max(b));
        commands.spawn((MenuItem(i), Button, Node { position_type: PositionType::Absolute, left: Val::Px(off.x + lo.x * s), top: Val::Px(off.y + lo.y * s), width: Val::Px((hi.x - lo.x) * s), height: Val::Px((hi.y - lo.y) * s), ..default() }, ChildOf(root)));
    }
    // the chosen save's picture, date and name (main menu)
    if in_game {
        return;
    }
    let Some((map, mission, at)) = menu.item_slot(sel).and_then(|slot| slots.info(slot)) else { return };
    let details = Vec2::new(1085.0, 360.0);
    if let Some((h, size)) = crate::loading::mission_art(map).and_then(|a| ui.file(&mut images, "missions", &format!("MissionsScreen_{a}_Large"))) {
        // (its box, 480 x 680, drawn further back: `_z` 270)
        let bx = Vec2::new(480.0, 680.0) * 0.82;
        let k = (bx.x / size.x.max(1.0)).min(bx.y / size.y.max(1.0));
        let (iw, ih) = (size.x * k, size.y * k);
        let c = details + Vec2::new(10.0, 0.0);
        commands.spawn((ImageNode::new(h), node(c - Vec2::new(iw, ih) * 0.5), Pickable::IGNORE, ChildOf(root))).insert(Node {
            position_type: PositionType::Absolute,
            left: Val::Px(off.x + (c.x - iw * 0.5) * s),
            top: Val::Px(off.y + (c.y - ih * 0.5) * s),
            width: Val::Px(iw * s),
            height: Val::Px(ih * s),
            ..default()
        });
    }
    // the banners' words, up their turned strips (`_date_mc`, `_name_mc`)
    let turn: Mat = [0.12187, -0.99255, 0.99255, 0.12187, 0.0, 0.0];
    let up = |o: Vec2, size: Vec2| details + o + Vec2::new(turn[0] * size.x * 0.5 + turn[2] * size.y * 0.5, turn[1] * size.x * 0.5 + turn[3] * size.y * 0.5);
    text_box(&mut commands, root, s, off, up(Vec2::new(-228.7, 221.0), Vec2::new(388.6, 43.75)), Vec2::new(388.6, 43.75), -83.0, local_date(at), title_font(32.0), PALE, false);
    text_box(&mut commands, root, s, off, up(Vec2::new(-175.6, 195.4), Vec2::new(486.0, 35.85)), Vec2::new(486.0, 35.85), -83.0, mission.to_uppercase(), title_font(28.0), PALE, false);
}
