//! The options screen as `UI_OptionsMenu` draws it (`OptionsScreen`): its dark paper
//! (`o_OptionsScreen`) over the menu behind (the pause screen's drifting backdrop in a game),
//! "OPTIONS" on its brush (`lib_titleMc`), the categories as main tabs (`lib_mTabs_tab_s` 206
//! apart, centred on (640,150), all turned -4 degrees), the sub-categories over their dark band
//! (`o_OptionsScreen_SubTabs_` at (640,210): `$TitleFont` 21, 50 apart, the chosen one pale and
//! underlined), and the settings' rows from (190,280) (or (190,225) without sub-categories),
//! 48 apart (`o_options_line`, the chosen one `over`: light, its words dark, slid 20 right),
//! each with its widget at x 680: the two choices side by side (`OptionsStepperB_widget`), a
//! value between arrows (`OptionsStepper_widget`), a slider's thumb along its track
//! (`Slider_widget`), or a key binding's box (`o_MappingList_item`). The brightness and the
//! video settings open screens of their own over it (`GammaSetting`, `VideoSettings`).

use crate::flash::{Clip, FlashClip, Mat};
use crate::menu::{Menu, MenuItem, OptView, OPT_CATEGORIES};
use crate::settings::Settings;
use crate::GameState;
use bevy::prelude::*;
use bevy::text::{FontSize, LineBreak};
use bevy::window::PrimaryWindow;

pub struct OptScreenPlugin;

impl Plugin for OptScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (tabs_input, video_clicks, draw).chain().after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Component)]
struct OptRoot;
/// A category's tab clicked.
#[derive(Component)]
struct OptTab(u8);
/// A sub-category's tab clicked.
#[derive(Component)]
struct OptSub(u8);

const MOVIE: &str = "OptionsMenu";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);
const GREY: Color = Color::srgb(130.0 / 255.0, 138.0 / 255.0, 123.0 / 255.0);
/// the screen's turn (-4 degrees)
const TURN: Mat = [0.99756, -0.06976, 0.06976, 0.99756, 0.0, 0.0];
const MENU_BASE: &str = "Twk_GFxMoviePlayerMenuBase";

fn turned(origin: Vec2, p: Vec2) -> Vec2 {
    origin + Vec2::new(TURN[0] * p.x + TURN[2] * p.y, TURN[1] * p.x + TURN[3] * p.y)
}

fn tabs_input(mut menu: ResMut<Menu>, tabs: Query<(&Interaction, &OptTab), Changed<Interaction>>, subs: Query<(&Interaction, &OptSub), Changed<Interaction>>, scripted: Option<Res<crate::script::Scripted>>) {
    if scripted.is_some() || menu.front_page().is_none_or(|p| p.0 != 7) {
        return;
    }
    let (cat, _) = menu.options_tab();
    for (i, t) in &tabs {
        if *i == Interaction::Pressed {
            menu.set_options_tab(t.0, 0);
        }
    }
    for (i, t) in &subs {
        if *i == Interaction::Pressed {
            menu.set_options_tab(cat, t.0);
        }
    }
}

/// A part of the video settings clicked: for the menu's keys to act on (`video_input`).
fn video_clicks(mut menu: ResMut<Menu>, hits: Query<(&Interaction, &crate::menu::VideoHit), Changed<Interaction>>, scripted: Option<Res<crate::script::Scripted>>) {
    if scripted.is_some() || menu.video.is_none() {
        return;
    }
    if let Some((_, h)) = hits.iter().find(|(i, _)| **i == Interaction::Pressed) {
        menu.video_click = Some(*h);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    menu: Res<Menu>,
    settings: Res<Settings>,
    mut timelines: ResMut<crate::flash::MovieTimelines>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    data: Res<crate::gamedata::Data>,
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<OptRoot>>,
    mut shown: Local<Option<String>>,
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
    (mut gamma_shown, mut video_shown): (Local<bool>, Local<Option<(bool, bool)>>),
) {
    let Ok(w) = window.single() else { return };
    let page = menu.front_page().filter(|p| p.0 == 7);
    // (drawn again when anything shown changes)
    let key = page.as_ref().map(|p| {
        let views: Vec<String> = (0..p.1.len())
            .map(|i| match menu.option_view(i, &settings, &data) {
                Some(OptView::Toggle(b)) => format!("t{b}"),
                Some(OptView::Stepper(_, i)) => format!("s{i}"),
                Some(OptView::Slider(_, v)) => format!("v{v}"),
                Some(OptView::Key(k, c)) => format!("k{k}{c}"),
                Some(OptView::Link(_)) => "l".to_string(),
                None => String::new(),
            })
            .collect();
        // (the brightness screen open: its value)
        let views = format!("{}|{:?}", views.join(","), menu.gamma.map(|_| (settings.brightness * 100.0).round() as i32));
        // (the video settings open: their lines, the one chosen, the drop list, the question)
        let views = format!("{views}|{:?}", menu.video.as_ref().map(|v| (v.sel, v.lines, v.drop, v.asked.is_some())));
        format!("{:?}|{}|{:?}|{}|{}x{}|{}", menu.options_tab(), menu.selected(), p.1, views, w.width(), w.height(), menu.in_main())
    });
    if key == *shown {
        return;
    }
    // (its help bar comes in as the screen opens, then stays)
    let opening = shown.is_none();
    *shown = key;
    for e in &roots {
        commands.entity(e).try_despawn();
    }
    let Some((_, labels)) = page else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let lib = timelines.get("lib");
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let root = commands.spawn((OptRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(55), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    let title_font = |size: f32| TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(size * s), ..default() };
    let normal_font = |size: f32| TextFont { font_size: FontSize::Px(size * s), ..default() };
    let place = |commands: &mut Commands, tl: &std::sync::Arc<dhcook::format::Timelines>, movie: &str, sym: &str, at: Vec2, m: Mat, setup: &mut dyn FnMut(&mut FlashClip)| {
        let Some(c) = Clip::export(tl, sym) else { return };
        let mut fc = FlashClip::new(movie, tl.clone(), c).real();
        fc.scale = s;
        fc.m = m;
        setup(&mut fc);
        let p = off + at * s;
        commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
    };
    // words in a box centred at `c` (stage), turned with the screen
    let words = |commands: &mut Commands, c: Vec2, size: Vec2, text: String, font: TextFont, color: Color, justify: Justify, tall: bool| {
        let p = off + (c - size * 0.5) * s;
        let bx = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(p.x),
                    top: Val::Px(p.y),
                    width: Val::Px(size.x * s),
                    height: Val::Px(size.y * s),
                    align_items: AlignItems::Center,
                    justify_content: match justify {
                        Justify::Right => JustifyContent::FlexEnd,
                        Justify::Center => JustifyContent::Center,
                        _ => JustifyContent::FlexStart,
                    },
                    ..default()
                },
                UiTransform { rotation: Rot2::degrees(-4.0), ..default() },
                Pickable::IGNORE,
                ChildOf(root),
            ))
            .id();
        let t = commands.spawn((Text::new(text), font, TextColor(color), TextLayout::new(justify, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(bx))).id();
        if tall {
            commands.entity(t).insert(UiTransform { scale: Vec2::new(1.0, 1.2), ..default() });
        }
    };
    let hit = |commands: &mut Commands, corners: [Vec2; 2], tag: Box<dyn FnOnce(&mut EntityCommands)>| {
        let (lo, hi) = (corners[0].min(corners[1]), corners[0].max(corners[1]));
        let mut e = commands.spawn((Button, Node { position_type: PositionType::Absolute, left: Val::Px(off.x + lo.x * s), top: Val::Px(off.y + lo.y * s), width: Val::Px((hi.x - lo.x) * s), height: Val::Px((hi.y - lo.y) * s), ..default() }, ChildOf(root)));
        tag(&mut e);
    };
    // behind: the pause screen's backdrop (the main menu's scene shows through instead)
    if !menu.in_main() {
        if let Some(ptl) = timelines.get("PauseMenu") {
            if let Some(c) = Clip::export(&ptl, "p_bkgd") {
                let mut fc = FlashClip::new("PauseMenu", ptl.clone(), c).real();
                fc.scale = s;
                let bg = crate::animbg::AnimatedBackground::new(&fc, "");
                let p = off + Vec2::new(640.0, 360.0) * s;
                let mut e = commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
                if let Some(bg) = bg {
                    e.insert(bg);
                }
            }
        }
    }
    // the brightness screen in its place (the options faded away under it: `DisplayGammaSetting`)
    if menu.gamma.is_some() {
        let opening = !*gamma_shown;
        *gamma_shown = true;
        gamma_screen(&mut commands, root, &tl, lib.clone(), s, off, &settings, &data, &fonts, &mut ui, &mut images, opening);
        return;
    }
    *gamma_shown = false;
    place(&mut commands, &tl, MOVIE, "o_OptionsScreen", Vec2::new(640.0, 360.0), crate::flash::IDENTITY, &mut |_| {});
    // the title
    if let Some(lib) = lib.as_ref() {
        place(&mut commands, lib, "lib", "lib_titleMc", Vec2::new(95.0, 65.0), TURN, &mut |fc| {
            for sh in ["_txtShadow0_mc", "_txtShadow1_mc", "_txtShadow2_mc"] {
                fc.set_visible(sh, false);
            }
        });
    }
    let title = data.text("DisGFxMoviePlayerBase_Texts", "t_Options").to_uppercase();
    words(&mut commands, turned(Vec2::new(95.0, 65.0), Vec2::new(2.0 + 345.0, 5.3 + 24.7)), Vec2::new(690.0, 49.4), title, title_font(38.0), DARK, Justify::Left, true);
    // the categories
    let (cat, sub) = menu.options_tab();
    let n = OPT_CATEGORIES.len();
    if let Some(lib) = lib.as_ref() {
        for (i, (name, _)) in OPT_CATEGORIES.iter().enumerate() {
            let on = i as u8 == cat;
            let at = turned(Vec2::new(640.0, 150.0), Vec2::new(206.0 * i as f32 - 103.0 * (n as f32 - 1.0), 0.0));
            place(&mut commands, lib, "lib", "lib_mTabs_tab_s", at, TURN, &mut |fc| {
                fc.set_visible("tab_mc.icUpdate_mc", false);
                if on {
                    fc.goto("tab_mc", "selected", true);
                    fc.goto("tab_mc.glow_mc", "loop", true);
                    if let Some((m, _)) = fc.clip.placed_mut("tab_mc") {
                        *m = [1.25, 0.0, 0.0, 1.25, 0.0, -20.0];
                    }
                } else {
                    let t2 = fc.tl.clone();
                    if let Some(c) = fc.clip.child_mut("tab_mc") {
                        c.goto(&t2, 0, false);
                    }
                }
            });
            let (k, dy) = if on { (1.25, -20.0) } else { (1.0, 0.0) };
            let label = data.text(MENU_BASE, name).to_uppercase();
            words(&mut commands, turned(at, Vec2::new(0.0, dy + k * -4.0)), Vec2::new(230.0, 40.0), label, title_font(20.0 * k), if on { Color::srgb(27.0 / 255.0, 30.0 / 255.0, 31.0 / 255.0) } else { PALE }, Justify::Center, true);
            let t = i as u8;
            hit(&mut commands, [at - Vec2::new(100.0, 38.0), at + Vec2::new(100.0, 38.0)], Box::new(move |e| {
                e.insert(OptTab(t));
            }));
        }
    }
    // the sub-categories
    let subs = OPT_CATEGORIES[cat as usize].1;
    let with_subs = !subs.is_empty();
    if with_subs {
        place(&mut commands, &tl, MOVIE, "o_OptionsScreen_SubTabs_", Vec2::new(640.0, 210.0), TURN, &mut |fc| {
            fc.set_visible("_tabs_mc", false);
        });
        let c = turned(Vec2::new(640.0, 210.0), Vec2::new(0.0, 5.35));
        let p = off + (c - Vec2::new(600.0, 22.0)) * s;
        let row = commands
            .spawn((
                Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(1200.0 * s), height: Val::Px(44.0 * s), justify_content: JustifyContent::Center, align_items: AlignItems::Center, column_gap: Val::Px(50.0 * s), ..default() },
                UiTransform { rotation: Rot2::degrees(-4.0), ..default() },
                Pickable::IGNORE,
                ChildOf(root),
            ))
            .id();
        for (j, key) in subs.iter().enumerate() {
            let on = j as u8 == sub;
            let tab = commands
                .spawn((OptSub(j as u8), Button, Node { top: Val::Px(if on { -5.0 * s } else { 0.0 }), flex_direction: FlexDirection::Column, align_items: AlignItems::Center, ..default() }, ChildOf(row)))
                .id();
            commands.spawn((Text::new(data.text(MENU_BASE, key).to_uppercase()), title_font(21.0), TextColor(if on { PALE } else { GREY }), UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Pickable::IGNORE, ChildOf(tab)));
            if on {
                commands.spawn((Node { width: Val::Percent(100.0), height: Val::Px(3.0 * s), margin: UiRect::top(Val::Px(4.0 * s)), ..default() }, BackgroundColor(PALE), Pickable::IGNORE, ChildOf(tab)));
            }
        }
    }
    // the rows
    let origin = if with_subs { Vec2::new(190.0, 280.0) } else { Vec2::new(190.0, 225.0) };
    let shown_rows = if with_subs { 8 } else { 9 };
    let sel = menu.selected();
    let count = labels.len();
    let first = if count <= shown_rows { 0 } else { sel.saturating_sub(shown_rows - 1).min(count - shown_rows) };
    for v in 0..shown_rows {
        let i = first + v;
        let y = 24.0 + 48.0 * v as f32;
        let label = labels.get(i);
        let on = label.is_some() && i == sel;
        let x = if on { 20.0 } else { 0.0 };
        let at = turned(origin, Vec2::new(x, y));
        let view = label.and_then(|_| menu.option_view(i, &settings, &data));
        let is_key = matches!(view, Some(OptView::Key(..)));
        place(&mut commands, &tl, MOVIE, if is_key { "o_MappingList_item" } else { "o_options_line" }, at, TURN, &mut |fc| {
            let t2 = fc.tl.clone();
            for part in ["_bkgd_mc.mc", "bkgd_mc.mc"] {
                if let Some(b) = fc.clip.child_mut(part) {
                    b.goto(&t2, i % 3, false);
                }
            }
            if label.is_none() {
                fc.goto("", "disabled", true);
            } else if on {
                fc.goto("", "over", true);
            }
            if let Some(OptView::Key(_, true)) = &view {
                fc.goto("widget_mc", "bind_in", true);
            }
        });
        let Some(label) = label else { continue };
        let lx = if is_key { 50.0 + 2.0 } else { 34.5 + 2.0 };
        words(&mut commands, turned(at, Vec2::new(lx + 240.0, 0.0)), Vec2::new(480.0, 32.4), label.clone(), normal_font(25.0), if on { DARK } else { PALE }, Justify::Left, false);
        let wc = turned(at, Vec2::new(680.0, 0.0));
        match view {
            Some(OptView::Stepper(values, idx)) => {
                let last = values.len().saturating_sub(1);
                place(&mut commands, &tl, MOVIE, "OptionsStepper_widget", wc, TURN, &mut |fc| {
                    if on {
                        fc.goto("", "over", true);
                    }
                    if !on || idx == 0 {
                        fc.goto("_leftArrow_mc", "locked", false);
                    }
                    if !on || idx >= last {
                        fc.goto("_rightArrow_mc", "locked", false);
                    }
                });
                let word = values.get(idx).cloned().unwrap_or_default();
                words(&mut commands, wc, Vec2::new(222.0, 30.0), word, normal_font(25.0), if on { Color::WHITE } else { GREY }, Justify::Center, false);
            }
            Some(OptView::Toggle(b)) => {
                let off_on = [data.text("Settings.ProfileSettingValues", "BP_False").to_uppercase(), data.text("Settings.ProfileSettingValues", "BP_True").to_uppercase()];
                place(&mut commands, &tl, MOVIE, "OptionsStepperB_widget", wc, TURN, &mut |fc| {
                    fc.goto(if b { "bkgd1_mc" } else { "bkgd0_mc" }, "selected", true);
                    underline(fc, "", b, &off_on[b as usize]);
                    if !on {
                        fc.goto("_leftArrow_mc", "locked", false);
                        fc.goto("_rightArrow_mc", "locked", false);
                    }
                });
                let chosen = if on { DARK } else { DARK.with_alpha(0.9) };
                words(&mut commands, turned(wc, Vec2::new(-70.0, 0.0)), Vec2::new(130.0, 30.0), off_on[0].clone(), normal_font(23.0), if !b { chosen } else { GREY }, Justify::Right, false);
                words(&mut commands, turned(wc, Vec2::new(71.0, 0.0)), Vec2::new(130.0, 30.0), off_on[1].clone(), normal_font(23.0), if b { chosen } else { GREY }, Justify::Left, false);
            }
            Some(OptView::Slider(t, shown)) => {
                let tx = -128.95 + t * 257.9;
                place(&mut commands, &tl, MOVIE, "Slider_widget", wc, TURN, &mut |fc| {
                    if let Some((m, _)) = fc.clip.placed_mut("_thumb_mc") {
                        m[4] = tx;
                    }
                    if on {
                        fc.goto("", "over", true);
                    } else {
                        fc.goto("_leftArrow_mc", "locked", false);
                        fc.goto("_rightArrow_mc", "locked", false);
                    }
                });
                words(&mut commands, turned(wc, Vec2::new(tx, 0.0)), Vec2::new(52.0, 30.0), shown, normal_font(25.0), DARK, Justify::Center, false);
            }
            Some(OptView::Key(k, waiting)) => {
                let word = if waiting { "Press a key...".to_string() } else { k };
                words(&mut commands, wc, Vec2::new(350.0, 30.0), word, normal_font(25.0), if on { DARK } else { PALE }, Justify::Center, false);
            }
            // a link to its screen (`SetLink`: the brightness's "Modify brightness")
            Some(OptView::Link(word)) => {
                words(&mut commands, wc, Vec2::new(350.0, 30.0), word, normal_font(25.0), if on { DARK } else { PALE }, Justify::Center, false);
            }
            None => {}
        }
        // its hit area (`btn`)
        let corners = [turned(at, Vec2::new(1.35, -22.5)), turned(at, Vec2::new(912.4, 22.5))];
        hit(&mut commands, corners, Box::new(move |e| {
            e.insert(MenuItem(i));
        }));
    }
    // the video settings over it (`DisplayVideoSettings`: the help bar shut, `CloseHelp`)
    if let Some(v) = menu.video.as_ref() {
        let (asked, dropped) = (v.asked.is_some(), v.drop.is_some());
        let anim = match *video_shown {
            None => VideoAnim::Open,
            Some((false, _)) if asked => VideoAnim::Hide,
            Some((true, _)) if !asked => VideoAnim::Show,
            _ => VideoAnim::Still,
        };
        let anim_drop = dropped && !video_shown.is_some_and(|p| p.1);
        *video_shown = Some((asked, dropped));
        video_screen(&mut commands, root, &tl, lib.clone(), s, off, v, &data, anim, anim_drop);
        return;
    }
    *video_shown = None;
    // the help bar (`Options_main`: X Restore Settings, B Back); a key being bound, its prompt
    if menu.capturing_binding() {
        let p = off + Vec2::new(1184.0 - 700.0, 651.0 - 16.0) * s;
        commands.spawn((Text::new("PRESS A KEY    [Esc] CANCEL"), title_font(21.0), TextColor(PALE), TextLayout::new(Justify::Right, LineBreak::NoWrap), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(700.0 * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
    } else if let Some(lib) = lib.clone() {
        use crate::helpbar::{Btn, Entry};
        let entries = [Entry::new(Btn::X, data.text("DisGFxMoviePlayerMenuBase_Texts", "t_RestoreSettings")), Entry::new(Btn::B, data.text("DisGFxMoviePlayerBase_Texts", "t_Back"))];
        crate::helpbar::spawn(&mut commands, root, lib, s, off + crate::helpbar::END * s, &entries, &mut ui, &mut images, !opening);
    }
}

/// A boolean stepper's chosen word underlined (`P_OptionsStepperBoolean.SetWidget` / `SetValue`:
/// its button `selected`, the line as wide as the word, `line_mc.mc` 30 across; under the
/// right-set Off, held to its right), within the widget at `w` (a path to it and a dot, or
/// nothing: the clip itself).
fn underline(fc: &mut FlashClip, w: &str, on: bool, word: &str) {
    let k = on as usize;
    fc.goto(&format!("{w}btn{k}"), "selected", true);
    let width = crate::ui_fonts::measure(false, 23.0, word);
    let x = if on { 0.0 } else { 129.75 - width - 6.0 };
    fc.set(&format!("{w}btn{k}.line_mc.mc"), crate::flash::PropsTo::default().x(x).xscale(width / 30.0));
}

/// The brightness's picture (`DisGFxMoviePlayerGamma.m_BrightnessValues`, `DefaultUI.ini`): a
/// mark each, its grey taken down to 1 - value (`GammaMc.SetGammaImages`).
const BRIGHTNESS_VALUES: [f32; 5] = [0.998, 0.99, 0.95, 0.90, 0.80];

/// The brightness screen as `GammaSetting` draws it (`o_Options_gammaSetting` at the stage's
/// middle): its dark veil, the setting's name (`_title_txt`) over the hint (`_desc_txt`,
/// `t_GammaHint`), the slider on its line (`_widget_mc`, the slider at its origin); the picture
/// under it (`UI_Gamma.GammaImage`'s `GammaMc` at (640,555): the Outsider's mark, 90%, 20 apart,
/// each darkened to 1 - its value and framed by its faint corners, `gamma_logo_indic` at 12.5%);
/// the help bar (`SecondaryScreen`: A Accept, B Back). It rises 150 into place as it opens. The
/// marks show the brightness as the screen's gamma would (the display's value to the power of
/// 1 / brightness, as the scene takes it: our reading of the original's gamma ramp).
#[allow(clippy::too_many_arguments)]
fn gamma_screen(commands: &mut Commands, root: Entity, tl: &std::sync::Arc<dhcook::format::Timelines>, lib: Option<std::sync::Arc<dhcook::format::Timelines>>, s: f32, off: Vec2, settings: &Settings, data: &crate::gamedata::Data, fonts: &crate::ui_fonts::UiFonts, ui: &mut crate::ui_images::UiImages, images: &mut Assets<Image>, opening: bool) {
    let _ = fonts;
    let rise = |fc: &mut FlashClip| {
        if opening {
            fc.set("", crate::flash::PropsTo::default().y(150.0).alpha(0.0));
            fc.tween("", crate::flash::PropsTo::default().y(0.0).alpha(1.0), 0.25, crate::flash::Ease::StrongOut);
        }
    };
    let node = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
    if let Some(c) = Clip::export(tl, "o_Options_gammaSetting") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
        fc.scale = s;
        fc.set_text("_title_txt", data.text("Settings.ProfileSettingIDs", "PSI_Graphics_Gamma").to_uppercase());
        fc.set_text("_desc_txt", data.text("DisGFxMoviePlayerMenuBase_Texts", "t_GammaHint"));
        rise(&mut fc);
        commands.spawn((fc, node(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
    }
    // the slider, focused (`SetWidgetFocus`), its number on its thumb
    let t = ((settings.brightness - 0.5) / 1.1).clamp(0.0, 1.0);
    let tx = -128.95 + t * 257.9;
    let at = Vec2::new(640.0 - 3.0, 360.0 + 90.95);
    if let Some(c) = Clip::export(tl, "Slider_widget") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        if let Some((m, _)) = fc.clip.placed_mut("_thumb_mc") {
            m[4] = tx;
        }
        fc.goto("", "over", true);
        rise(&mut fc);
        commands.spawn((fc, node(at), Pickable::IGNORE, ChildOf(root)));
    }
    let p = off + (at + Vec2::new(tx - 26.0, -15.0)) * s;
    let bx = commands.spawn((Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(52.0 * s), height: Val::Px(30.0 * s), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }, Pickable::IGNORE, ChildOf(root))).id();
    commands.spawn((Text::new(format!("{:.0}", t * 100.0)), TextFont { font_size: FontSize::Px(25.0 * s), ..default() }, TextColor(DARK), Pickable::IGNORE, ChildOf(bx)));
    // the picture: the marks darkened to their values, as the brightness shows them
    let srgb_lin = |x: f32| if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    let base = 128.0 / 255.0;
    let g = settings.brightness.max(0.05);
    let n = BRIGHTNESS_VALUES.len() as f32;
    for (i, v) in BRIGHTNESS_VALUES.iter().enumerate() {
        let c = Vec2::new(640.0 + (i as f32 - (n - 1.0) * 0.5) * 148.0, 555.0);
        if let Some((h, _)) = ui.get(images, "GammaImage", 5) {
            let k = srgb_lin((base * (1.0 - v)).powf(1.0 / g)) / srgb_lin(base);
            let size = 128.0 * 0.9;
            let p = off + (c - Vec2::splat(size * 0.5)) * s;
            commands.spawn((ImageNode::new(h).with_color(Color::linear_rgb(k, k, k)), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(size * s), height: Val::Px(size * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
        }
        if let Some((h, _)) = ui.get(images, "GammaImage", 40003) {
            let p = off + (c - Vec2::new(65.0, 73.0)) * s;
            commands.spawn((ImageNode::new(h).with_color(Color::WHITE.with_alpha(0.125)), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(130.0 * s), height: Val::Px(139.0 * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
        }
    }
    if let Some(lib) = lib {
        use crate::helpbar::{Btn, Entry};
        let base = "DisGFxMoviePlayerBase_Texts";
        crate::helpbar::spawn(commands, root, lib, s, off + crate::helpbar::END * s, &[Entry::new(Btn::A, data.text(base, "t_Validate")), Entry::new(Btn::B, data.text(base, "t_Back"))], ui, images, !opening);
    }
}

/// The video settings' layout as `VideoSettings.InitWidget` sets it once its lines are in: the
/// button bar 20 under the list (at 57.9, its three lines 48 apart: 144.65 high), the widget
/// raised by half its height (276.55, from the title's rules to the bar's foot), its paper as
/// deep as the bar's foot and 20 more, its stroke 50 wider than the paper (682) and 40 deeper
/// than the widget (313.1), turned -2; the widget turned -2.5.
const VIDEO_WIDGET_Y: f32 = -138.3;
const VIDEO_BAR_Y: f32 = 222.55;
const VIDEO_PAPER_H: f32 = 307.1;
const VIDEO_STROKE: Vec2 = Vec2::new(732.0, 353.1);
const VIDEO_TURN: f32 = -2.5;

/// How the video settings come in: opening (from 120%, `Open`), their paper fading out as the
/// question is asked or back as it is answered no (`APressed`, `OnVideoSettingsConfirm`), or
/// drawn again as they stand.
#[derive(Clone, Copy, PartialEq)]
enum VideoAnim {
    Open,
    Hide,
    Show,
    Still,
}

/// The video settings as `VideoSettings` draws them (`o_videoSettings` at the stage's middle)
/// over the options: its dark veil (90%); its paper (`_widget_mc`) and stroke; the title
/// (`title_mc`: the link's name in capitals); its lines (`o_options_line_s`, 48 apart, the
/// chosen one `over` and slid 20 right, `OptionsList`'s), each with its widget 400 along (Off /
/// On side by side, `OptionsStepperB_widget`; the resolution's box, `DropDownMenu_widget`, its
/// list under it while open: `DropDownMenu_list`, seven lines 28 apart on a paper 20 wider, the
/// one set dotted, the one chosen lit); and its buttons (`buttonBar_mc`: APPLY, CANCEL, each
/// with its key's icon before its words, `lib_A_Multi` and `lib_B_Multi` on their PC frames).
/// The parts it acts on can be clicked (the lines' choices, the box and its list, the buttons).
#[allow(clippy::too_many_arguments)]
fn video_screen(commands: &mut Commands, root: Entity, tl: &std::sync::Arc<dhcook::format::Timelines>, lib: Option<std::sync::Arc<dhcook::format::Timelines>>, s: f32, off: Vec2, v: &crate::menu::VideoEdit, data: &crate::gamedata::Data, anim: VideoAnim, anim_drop: bool) {
    use crate::flash::{Ease, PropsTo, IDENTITY};
    use crate::menu::{VideoHit, DROP_LINES};
    // (the options under it not to be pointed at)
    commands.spawn((Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, ChildOf(root)));
    let Some(c) = Clip::export(tl, "o_videoSettings") else { return };
    let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
    fc.scale = s;
    let asked = v.asked.is_some();
    fc.set("_widget_mc", PropsTo::default().y(VIDEO_WIDGET_Y).rotation(VIDEO_TURN));
    fc.set("_widget_mc.bkgd_mc", PropsTo::default().y(VIDEO_PAPER_H * 0.5).yscale(VIDEO_PAPER_H / 256.0));
    fc.set("_widget_mc.stroke_mc", PropsTo::default().y(VIDEO_PAPER_H * 0.5).xscale(VIDEO_STROKE.x / 280.0).yscale(VIDEO_STROKE.y / 36.0).rotation(-2.0));
    fc.set("_widget_mc.buttonBar_mc", PropsTo::default().y(VIDEO_BAR_Y));
    fc.set_visible("_widget_mc.buttonBar_mc", !asked);
    fc.set_text("_widget_mc.title_mc.txt", data.text(MENU_BASE, "m_SettingsSubMenu_VideoSettings").to_uppercase());
    let base = "DisGFxMoviePlayerBase_Texts";
    let bar = "_widget_mc.buttonBar_mc";
    fc.set_text(&format!("{bar}.accept_mc._txt_mc.txt"), data.text(base, "t_Apply").to_uppercase());
    fc.set_text(&format!("{bar}.cancel_mc._txt_mc.txt"), data.text(base, "t_Cancel").to_uppercase());
    // the widget's turn, and where its points stand on the stage
    let (sn, cs) = VIDEO_TURN.to_radians().sin_cos();
    let turn = [cs, sn, -sn, cs, 0.0, 0.0];
    let rot = |p: Vec2| Vec2::new(cs * p.x - sn * p.y, sn * p.x + cs * p.y);
    let rel = |p: Vec2| Vec2::new(0.0, VIDEO_WIDGET_Y) + rot(p);
    let mut hits: Vec<(Vec2, Vec2, VideoHit)> = Vec::new();
    let mut drop_list = None;
    // the lines
    let list = "_widget_mc.list_mc";
    let off_on = [data.text("Settings.ProfileSettingValues", "BP_False").to_uppercase(), data.text("Settings.ProfileSettingValues", "BP_True").to_uppercase()];
    for (i, (label, view)) in v.views(data).into_iter().enumerate() {
        let on = i == v.sel;
        let name = format!("widget{i}");
        let x = if on { 20.0 } else { 0.0 };
        let y = 24.0 + 48.0 * i as f32;
        fc.attach(list, "o_options_line_s", &name, [1.0, 0.0, 0.0, 1.0, x, y]);
        let line = format!("{list}.{name}");
        fc.goto_frame(&format!("{line}._bkgd_mc.mc"), i % 3 + 1, false);
        if on {
            fc.goto(&line, "over", true);
        }
        fc.set_text(&format!("{line}._txt_mc.txt"), label);
        // (its widget's middle, in the widget's space)
        let wc = Vec2::new(-330.55 + x + 400.0, 57.9 + y);
        match view {
            OptView::Toggle(b) => {
                fc.attach(&line, "OptionsStepperB_widget", "_stepperB_mc", [1.0, 0.0, 0.0, 1.0, 400.0, 0.0]);
                let w = format!("{line}._stepperB_mc");
                fc.goto_frame(&format!("{w}.btn0.txt_mc"), 2, false);
                fc.set_text(&format!("{w}.btn0.txt_mc.txt"), off_on[0].clone());
                fc.set_text(&format!("{w}.btn1.txt_mc.txt"), off_on[1].clone());
                let k = b as usize;
                fc.goto(&format!("{w}.bkgd{k}_mc"), "selected", true);
                underline(&mut fc, &format!("{w}."), b, &off_on[k]);
                if on {
                    fc.goto(&w, "over", true);
                } else {
                    fc.goto(&format!("{w}._leftArrow_mc"), "locked", false);
                    fc.goto(&format!("{w}._rightArrow_mc"), "locked", false);
                }
                hits.push((wc + Vec2::new(-70.5, 0.0), Vec2::new(141.0, 32.0), VideoHit::Choice(i, false)));
                hits.push((wc + Vec2::new(70.5, 0.0), Vec2::new(141.0, 32.0), VideoHit::Choice(i, true)));
            }
            OptView::Stepper(words, at) => {
                fc.attach(&line, "DropDownMenu_widget", "_stepper_mc", [1.0, 0.0, 0.0, 1.0, 400.0, 0.0]);
                let w = format!("{line}._stepper_mc");
                fc.set_text(&format!("{w}._txt_mc.txt_mc.txt"), words.get(at).cloned().unwrap_or_default());
                fc.set_visible(&format!("{w}._btn_mc.key_mc"), false);
                if on {
                    fc.goto(&format!("{w}._btn_mc"), if v.drop.is_some() { "list_open" } else { "over" }, true);
                    fc.goto(&format!("{w}._txt_mc"), "over", true);
                }
                if let Some((chosen, top)) = v.drop {
                    // its list, under the box (`CreateDropList`: over all else, drawn on its own)
                    if let Some(c) = Clip::export(tl, "DropDownMenu_list") {
                        let mut lc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
                        lc.scale = s;
                        let shown = words.len().saturating_sub(top).min(DROP_LINES);
                        lc.set_visible("maskContent", false);
                        // (its paper as deep as its lines and 8 more, 20 wider than it)
                        lc.set("_bkgd_mc", PropsTo::default().xscale(303.0 / 128.0).yscale((27.5 * shown as f32 + 8.0) / 128.0));
                        lc.create_empty("", "_list_mc", IDENTITY);
                        for k in 0..shown {
                            let n = top + k;
                            let item = format!("item{n}");
                            let iy = 14.0 + 28.0 * k as f32;
                            lc.attach("_list_mc", "DropDownMenu_list_item", &item, [1.0, 0.0, 0.0, 1.0, 0.0, iy]);
                            let p = format!("_list_mc.{item}");
                            lc.set_text(&format!("{p}._txt_mc.txt"), words[n].clone());
                            lc.set_visible(&format!("{p}._txt_mc._active_mc"), n == at);
                            if n == chosen {
                                lc.goto(&p, "over", true);
                            }
                            hits.push((wc + Vec2::new(-143.45 + 129.6, 16.0 + iy), Vec2::new(258.5, 26.6), VideoHit::DropItem(n)));
                        }
                        let p = rel(wc + Vec2::new(-143.45, 16.0));
                        lc.m = [turn[0], turn[1], turn[2], turn[3], p.x, p.y];
                        // (in from 15 above as it opens: `ShowList`)
                        if anim_drop {
                            let from = p - rot(Vec2::new(0.0, 15.0));
                            lc.set("", PropsTo::default().x(from.x).y(from.y).alpha(0.0));
                            lc.tween("", PropsTo::default().x(p.x).y(p.y).alpha(1.0), 0.2, Ease::StrongOut);
                        }
                        drop_list = Some(lc);
                    }
                } else {
                    hits.push((wc, Vec2::new(328.0, 32.0), VideoHit::Drop));
                }
            }
            _ => {}
        }
    }
    // its coming in, its paper fading
    match anim {
        VideoAnim::Open => {
            fc.set("", PropsTo::default().scale(1.2).alpha(0.0));
            fc.tween("", PropsTo::default().scale(1.0).alpha(1.0), 0.25, Ease::StrongOut);
            fc.set("_black_mc", PropsTo::default().alpha(0.0));
            fc.tween("_black_mc", PropsTo::default().alpha(0.9), 0.25, Ease::StrongOut);
        }
        VideoAnim::Hide => {
            fc.set("_black_mc", PropsTo::default().alpha(0.9));
            fc.tween("_widget_mc", PropsTo::default().alpha(0.0), 0.25, Ease::StrongOut);
        }
        VideoAnim::Show => {
            fc.set("_black_mc", PropsTo::default().alpha(0.9));
            fc.set("_widget_mc", PropsTo::default().alpha(0.0));
            fc.tween("_widget_mc", PropsTo::default().alpha(1.0), 0.25, Ease::StrongOut);
        }
        VideoAnim::Still => {
            fc.set("_black_mc", PropsTo::default().alpha(0.9));
            fc.set("_widget_mc", PropsTo::default().alpha(if asked { 0.0 } else { 1.0 }));
        }
    }
    // the buttons' key icons (`key_mc`: half the words' width and half its own before them)
    let mut icons = Vec::new();
    if let (Some(lib), false) = (lib, asked) {
        for (btn, x, part, hit) in [("A", -154.0, "accept_mc", VideoHit::Apply), ("B", 154.0, "cancel_mc", VideoHit::Cancel)] {
            hits.push((Vec2::new(x - 0.45, VIDEO_BAR_Y + 24.0), Vec2::new(272.0, 48.0), hit));
            let words_w = fc.text_width(&format!("{bar}.{part}._txt_mc.txt")).unwrap_or(0.0);
            let Some(c) = Clip::export(&lib, &format!("lib_{btn}_Multi")) else { continue };
            let mut ic = FlashClip::new("lib", lib.clone(), c).real();
            ic.scale = s;
            ic.goto("", "PC", false);
            let key_w = ic.size("").map(|v| v.x).unwrap_or(36.0);
            let p = rel(Vec2::new(x - 0.5 * words_w - 0.5 * key_w, VIDEO_BAR_Y + 24.0));
            ic.m = [turn[0], turn[1], turn[2], turn[3], p.x, p.y];
            if anim == VideoAnim::Open {
                ic.set("", PropsTo::default().x(p.x * 1.2).y(p.y * 1.2).scale(1.2).alpha(0.0));
                ic.tween("", PropsTo::default().x(p.x).y(p.y).scale(1.0).alpha(1.0), 0.25, Ease::StrongOut);
            } else if anim == VideoAnim::Show {
                ic.set("", PropsTo::default().alpha(0.0));
                ic.tween("", PropsTo::default().alpha(1.0), 0.25, Ease::StrongOut);
            }
            icons.push(ic);
        }
    }
    let at = off + Vec2::new(640.0, 360.0) * s;
    let node = Node { position_type: PositionType::Absolute, left: Val::Px(at.x), top: Val::Px(at.y), ..default() };
    commands.spawn((fc, node.clone(), Pickable::IGNORE, ChildOf(root)));
    for ic in icons.into_iter().chain(drop_list) {
        commands.spawn((ic, node.clone(), Pickable::IGNORE, ChildOf(root)));
    }
    // what can be clicked (turned with it), while nothing is asked
    if asked {
        return;
    }
    for (c, size, h) in hits {
        let p = off + (Vec2::new(640.0, 360.0) + rel(c) - size * 0.5) * s;
        commands.spawn((h, Button, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(size.x * s), height: Val::Px(size.y * s), ..default() }, UiTransform { rotation: Rot2::degrees(VIDEO_TURN), ..default() }, ChildOf(root)));
    }
}
