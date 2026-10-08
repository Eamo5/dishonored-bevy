//! The front end as the original `UI_MainMenu` movie draws it, over the menu map's flying
//! camera: the title screen (`m_StartScreen`: the logo, "PRESS ANY KEY") and the main menu
//! (`m_MainMenu`: the blades, compass, splatters, logo and Corvo's portrait, and its button bar
//! at the bottom right). The bar is `MainMenuButtonBar`'s: horizontal, right-aligned in its
//! slanted band, `m_MainMenu_btn_` buttons side by side, each its upper-case label
//! (`$TitleFont` 25, #e3f2d6) with its lines 10 out (`_offsetLineX`/`Y`) and the band as wide as
//! the buttons less 10; the one under the pointer (or chosen with the keys) plays `over`
//! grown to 110% and 5 lower (`_btnOverProps`, Back.easeOut 0.25 s; out Strong.easeOut
//! 0.15 s). The menu's logic stays `menu.rs`'s; its buttons' hit areas are `MenuItem`s.

use crate::flash::{concat, Clip, FlashClip, Mat, MovieTimelines};
use crate::menu::{Menu, MenuItem};
use crate::ui_fonts::UiFonts;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct FrontendPlugin;

impl Plugin for FrontendPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Frontend>().add_systems(Update, (build, layout).chain().after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "MainMenu";
/// where the movie places its screens (stage units)
const MAIN_AT: Vec2 = Vec2::new(640.0, 353.95);
const START_AT: Vec2 = Vec2::new(640.0, 372.1);
/// "PRESS ANY KEY": the start screen's `_txt_mc` field, its middle
const PRESS_AT: Vec2 = Vec2::new(640.0, 372.1 + 187.9 + 18.0);
/// the buttons' label
const TEXT_SIZE: f32 = 25.0;
const TEXT_COLOR: Color = Color::srgb(0xe3 as f32 / 255.0, 0xf2 as f32 / 255.0, 0xd6 as f32 / 255.0);
/// a button's lines around its label (`_offsetLineX`/`Y`), the top and bottom lines' extra
/// width, the label's height (`textHeight`)
const LINE_OFFSET: f32 = 10.0;
const LINE_EXTRA: f32 = 50.0;
const TEXT_HEIGHT: f32 = 30.0;
/// the button parts' drawn sizes: side line height, top and bottom line widths
const SIDE_LINE_H: f32 = 32.5;
const UP_LINE_W: f32 = 194.5;
const DOWN_LINE_W: f32 = 244.75;
/// the band's backing width (`_bkgd_mc`, anchored at its right)
const BAND_W: f32 = 444.8;
/// the over backing's mask width (`_maskBkgdOver_mc`)
const MASK_W: f32 = 80.0;
/// over, the label turns dark on its light backing (the `over` frames' colour transform:
/// multiplied by 0, plus 23, 25, 28) and rises 2.5
const OVER_COLOR: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);
const OVER_RISE: f32 = 2.5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Screen {
    Start,
    Main,
    NewGame,
    Pause,
    /// the pause screen as the game over menu: its title "GAME OVER", the reason on
    /// `_gameOver_mc` (`GameOverTitle`: `$NormalFont` 35), the mask sliced (`anim_slice`)
    GameOver,
}

/// the game over reason's field in `_gameOver_mc` (`_txt_mc` + `txt`, less its 2 px gutter)
const GO_TEXT_AT: Vec2 = Vec2::new(126.95 + 4.1 + 2.0, -15.3 - 18.7 + 2.0);
const GO_TEXT_SIZE: f32 = 35.0;

/// The pause screen (`UI_PauseMenu`: its backdrop `p_bkgd` and `p_pauseMenu` at the stage's
/// middle: Corvo's mask, the brushes, the compass; `PauseMenu`'s list in `_menu_mc`,
/// left- and bottom-aligned, `p_pauseMenu_item`s 48 high and 5 apart, their brushes taking
/// the 4 frames in turn, the one over slid 30 right with its light brush and dark label; the
/// labels upper-case, white `$TitleFont` 25 stretched 1.2 high; the title `p_pauseMenu_title`
/// ("PAUSE", `$TitleFont` 65) running up the left).
const PAUSE_MOVIE: &str = "PauseMenu";
const PAUSE_AT: Vec2 = Vec2::new(640.0, 360.0);
const PM_ITEM_H: f32 = 48.0;
const PM_ITEM_GAP: f32 = 5.0;
const PM_OVER_X: f32 = 30.0;
const PM_TEXT_AT: Vec2 = Vec2::new(1.55, -19.45);
const PM_TITLE_AT: Vec2 = Vec2::new(227.9, -97.7);
const PM_TITLE_SIZE: f32 = 65.0;
const WHITE: Color = Color::WHITE;
/// the list's mask (`_maskMenu_mc`, set by code: `setMask`), its stage bounds near the list
/// (a slanted rectangle there: its sides at about these x)
const PM_MASK: [f32; 4] = [600.0, 130.0, 1180.0, 700.0];

#[derive(Component)]
struct ClipBox;

/// The new game screen (`m_nGame` at the stage's middle; `NewGameMenu`): its list in
/// `_menu_mc`, left at 10 + 7.5% of the stage's width, the `m_nGame_btn` buttons 48 high and 2
/// apart (their backings cycling through the brush's 3 frames), the one over slid 25 right
/// (Back.easeOut 0.25 s) with its light backing and dark label; the label `$TitleFont` 27
/// stretched 1.2 high; the difficulty's description (`_description_mc`, `$NormalFont` 26, 450
/// wide) and Corvo's portrait for it (`_corvo_mc.portrait_mc`, a frame each).
const NEWGAME_AT: Vec2 = Vec2::new(640.0, 360.0);
const NG_ORIGIN_X: f32 = 10.0 + 0.075 * 1280.0;
const NG_BTN_H: f32 = 48.0;
const NG_BTN_GAP: f32 = 2.0;
const NG_OVER_X: f32 = 25.0;
const NG_TEXT_SIZE: f32 = 27.0;
const NG_TEXT_AT: Vec2 = Vec2::new(2.25, -20.8);
const NG_DESC_AT: Vec2 = Vec2::new(521.95, 18.7);
const NG_DESC_W: f32 = 451.0;
const NG_DESC_SIZE: f32 = 26.0;
/// "BACK", where the help bar's would be
const BACK_AT: Vec2 = Vec2::new(70.0, 680.0);
/// the screen's title (`lib_titleMc` at `_title_mc`: its label `$TitleFont` 38 stretched 1.2
/// high, dark on its light brush)
const NG_TITLE_AT: Vec2 = Vec2::new(-545.75 - 0.15, -282.3 + 3.0);
const NG_TITLE_SIZE: f32 = 38.0;

#[derive(Resource, Default)]
pub struct Frontend {
    shown: Option<(Screen, Vec<String>)>,
    root: Option<Entity>,
    /// time since it opened (s), and each button's over-ness (0..1) and whether it is over
    t: f32,
    over: Vec<(f32, bool)>,
}

#[derive(Component)]
struct FrontRoot;
#[derive(Component)]
struct MainClip;
#[derive(Component)]
struct BtnClip(usize);
#[derive(Component)]
struct BtnText(usize);
#[derive(Component)]
struct PressText;
#[derive(Component)]
struct DescText;
#[derive(Component)]
struct TitleText;

fn stage(w: &Window) -> (f32, Vec2) {
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    (s, (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5)
}

fn translate(x: f32, y: f32) -> Mat {
    [1.0, 0.0, 0.0, 1.0, x, y]
}

fn apply(m: &Mat, p: Vec2) -> Vec2 {
    Vec2::new(m[0] * p.x + m[2] * p.y + m[4], m[1] * p.x + m[3] * p.y + m[5])
}

/// The screen to draw: the main menu's title or front page.
fn wanted(menu: &Menu) -> Option<(Screen, Vec<String>)> {
    match menu.front_page()? {
        (0, _) => Some((Screen::Start, Vec::new())),
        (1, items) => Some((Screen::Main, items.iter().map(|s| s.to_uppercase()).collect())),
        (2, items) => Some((Screen::NewGame, items)),
        (4, items) => Some((Screen::GameOver, items.iter().map(|s| s.to_uppercase()).collect())),
        (3, items) => Some((Screen::Pause, items.iter().map(|s| s.to_uppercase()).collect())),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn build(
    mut commands: Commands,
    menu: Res<Menu>,
    mut fe: ResMut<Frontend>,
    mut timelines: ResMut<MovieTimelines>,
    fonts: Res<UiFonts>,
    data: Res<crate::gamedata::Data>,
    roots: Query<Entity, With<FrontRoot>>,
    stats: Res<crate::gameplay::PlayerStats>,
) {
    let want = wanted(&menu);
    if want == fe.shown {
        return;
    }
    for e in &roots {
        commands.entity(e).try_despawn();
    }
    fe.root = None;
    fe.shown = want.clone();
    fe.t = 0.0;
    let Some((screen, items)) = want else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let title: bevy::text::FontSource = fonts.title.clone().into();
    let root = commands
        .spawn((FrontRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(50), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
        .id();
    fe.root = Some(root);
    let node = || Node { position_type: PositionType::Absolute, ..default() };
    match screen {
        Screen::Start => {
            if let Some(c) = Clip::export(&tl, "m_StartScreen") {
                commands.spawn((MainClip, FlashClip::new(MOVIE, tl.clone(), c).real(), node(), Pickable::IGNORE, ChildOf(root)));
            }
            let press = data.text("DisGFxMoviePlayerMainMenu_Texts", "t_PressAnyKey");
            let press = if press.is_empty() { "PRESS ANY KEY".to_string() } else { press };
            commands.spawn((PressText, Text::new(press), TextFont { font: title.clone(), ..default() }, TextColor(TEXT_COLOR), TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap), node(), Pickable::IGNORE, ChildOf(root)));
        }
        Screen::Main => {
            if let Some(c) = Clip::export(&tl, "m_MainMenu") {
                let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
                // (no DLC on this platform)
                fc.set_visible("_DLCButton_mc", false);
                commands.spawn((MainClip, fc, node(), Pickable::IGNORE, ChildOf(root)));
            }
            for (i, label) in items.iter().enumerate() {
                if let Some(c) = Clip::export(&tl, "m_MainMenu_btn_") {
                    let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
                    // (the label is drawn as text; the flash fields aren't)
                    fc.alpha = 0.0;
                    commands.spawn((BtnClip(i), fc, node(), Pickable::IGNORE, ChildOf(root)));
                }
                commands.spawn((BtnText(i), Text::new(label.clone()), TextFont { font: title.clone(), ..default() }, TextColor(TEXT_COLOR.with_alpha(0.0)), TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap), node(), UiTransform::default(), Pickable::IGNORE, ChildOf(root)));
                // the hit area (`btn`), for the pointer
                commands.spawn((MenuItem(i), Button, node(), ChildOf(root)));
            }
            fe.over = vec![(0.0, false); items.len()];
        }
        Screen::Pause | Screen::GameOver => {
            let over = screen == Screen::GameOver;
            let Some(ptl) = timelines.get(PAUSE_MOVIE) else { return };
            for sym in ["p_bkgd", "p_pauseMenu"] {
                if let Some(c) = Clip::export(&ptl, sym) {
                    let mut fc = FlashClip::new(PAUSE_MOVIE, ptl.clone(), c).real();
                    // (the list's mask is code's, not drawn)
                    fc.set_visible("_maskMenu_mc", false);
                    // the reason's backing shows for a game over; the mask is sliced then
                    // (`OpenCorvo`: `anim_slice`, else its first frame)
                    fc.set_visible("_gameOver_mc", over);
                    if over {
                        fc.goto("_corvo_mc", "anim_slice", true);
                    } else if let Some(c) = fc.clip.child_mut("_corvo_mc") {
                        c.goto(&ptl, 0, false);
                    }
                    // (the backdrop drifts: `AnimatedBackground`)
                    let bg = crate::animbg::AnimatedBackground::new(&fc, "");
                    let mut e = commands.spawn((MainClip, fc, node(), Pickable::IGNORE, ChildOf(root)));
                    if let Some(bg) = bg {
                        e.insert(bg);
                    }
                }
            }
            // the list, clipped to its mask
            let clip_box = commands.spawn((ClipBox, Node { position_type: PositionType::Absolute, overflow: Overflow::clip(), ..default() }, Pickable::IGNORE, ChildOf(root))).id();
            for (i, label) in items.iter().enumerate() {
                if let Some(c) = Clip::export(&ptl, "p_pauseMenu_item") {
                    let mut fc = FlashClip::new(PAUSE_MOVIE, ptl.clone(), c).real();
                    fc.alpha = 0.0;
                    let t2 = fc.tl.clone();
                    if let Some(b) = fc.clip.child_mut("_bkgd_mc") {
                        b.goto(&t2, i % 4, false);
                    }
                    commands.spawn((BtnClip(i), fc, node(), Pickable::IGNORE, ChildOf(clip_box)));
                }
                commands.spawn((BtnText(i), Text::new(label.clone()), TextFont { font: title.clone(), ..default() }, TextColor(WHITE.with_alpha(0.0)), TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap), node(), UiTransform::default(), Pickable::IGNORE, ChildOf(root)));
                commands.spawn((MenuItem(i), Button, node(), ChildOf(root)));
            }
            let t = data.text("DisGFxMoviePlayerMenuBase_Texts", if over { "t_GameOver" } else { "t_Pause" }).to_uppercase();
            commands.spawn((TitleText, Text::new(t), TextFont { font: title.clone(), ..default() }, TextColor(TEXT_COLOR.with_alpha(0.0)), TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap), node(), UiTransform::default(), Pickable::IGNORE, ChildOf(root)));
            if over {
                // why: the scripts' reason, or the death's (`m_PlayerDiedMessage`)
                let why = stats.game_over.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| data.text("Twk_GFxMoviePlayerPauseMenu", "m_PlayerDiedMessage"));
                commands.spawn((DescText, Text::new(why), TextFont::default(), TextColor(TEXT_COLOR.with_alpha(0.0)), TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap), node(), UiTransform::default(), Pickable::IGNORE, ChildOf(root)));
            }
            fe.over = vec![(0.0, false); items.len()];
        }
        Screen::NewGame => {
            if let Some(c) = Clip::export(&tl, "m_nGame") {
                commands.spawn((MainClip, FlashClip::new(MOVIE, tl.clone(), c).real(), node(), Pickable::IGNORE, ChildOf(root)));
            }
            // the difficulties (the last item, Back, is the help bar's)
            let n = items.len().saturating_sub(1);
            for (i, label) in items.iter().enumerate() {
                if i < n {
                    if let Some(c) = Clip::export(&tl, "m_nGame_btn") {
                        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
                        fc.alpha = 0.0;
                        // (`_bMultiBkgdTypes`: the backings take the brush's frames in turn)
                        let t2 = fc.tl.clone();
                        if let Some(b) = fc.clip.child_mut("_bkgd_mc") {
                            b.goto(&t2, i % 3, false);
                        }
                        commands.spawn((BtnClip(i), fc, node(), Pickable::IGNORE, ChildOf(root)));
                    }
                }
                let text = if i < n { label.clone() } else { data.text("DisGFxMoviePlayerBase_Texts", "t_Back") };
                commands.spawn((BtnText(i), Text::new(text), TextFont { font: title.clone(), ..default() }, TextColor(TEXT_COLOR.with_alpha(0.0)), TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap), node(), UiTransform::default(), Pickable::IGNORE, ChildOf(root)));
                commands.spawn((MenuItem(i), Button, node(), ChildOf(root)));
            }
            let t = data.text("DisGFxMoviePlayerMainMenu_Texts", "t_NewGame").to_uppercase();
            commands.spawn((TitleText, Text::new(t), TextFont { font: title.clone(), ..default() }, TextColor(OVER_COLOR.with_alpha(0.0)), TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap), node(), UiTransform::default(), Pickable::IGNORE, ChildOf(root)));
            commands.spawn((DescText, Text::new(""), TextFont::default(), TextColor(TEXT_COLOR.with_alpha(0.0)), TextLayout::new(Justify::Left, bevy::text::LineBreak::WordBoundary), node(), UiTransform::default(), Pickable::IGNORE, ChildOf(root)));
            fe.over = vec![(0.0, false); items.len()];
        }
    }
}

type MainQuery<'w, 's> = Query<'w, 's, (&'static mut FlashClip, &'static mut Node), (With<MainClip>, Without<BtnClip>, Without<BtnText>, Without<MenuItem>, Without<PressText>, Without<DescText>)>;
type BtnQuery<'w, 's> = Query<'w, 's, (&'static BtnClip, &'static mut FlashClip, &'static mut Node), (Without<MainClip>, Without<BtnText>, Without<MenuItem>, Without<PressText>, Without<DescText>)>;
type TextQuery<'w, 's> = Query<'w, 's, (&'static BtnText, &'static mut TextFont, &'static mut TextColor, &'static mut Node, &'static mut UiTransform, &'static ComputedNode), (Without<MainClip>, Without<BtnClip>, Without<MenuItem>, Without<PressText>, Without<DescText>)>;
type HitQuery<'w, 's> = Query<'w, 's, (&'static MenuItem, &'static Interaction, &'static mut Node), (Without<MainClip>, Without<BtnClip>, Without<BtnText>, Without<PressText>, Without<DescText>)>;
type DescQuery<'w, 's> = Query<'w, 's, (&'static mut Text, &'static mut TextFont, &'static mut TextColor, &'static mut Node, &'static mut UiTransform, &'static ComputedNode), (With<DescText>, Without<MainClip>, Without<BtnClip>, Without<BtnText>, Without<MenuItem>, Without<PressText>)>;

type TitleQuery<'w, 's> = Query<'w, 's, (&'static mut TextFont, &'static mut TextColor, &'static mut Node, &'static mut UiTransform, &'static ComputedNode), (With<TitleText>, Without<MainClip>, Without<BtnClip>, Without<BtnText>, Without<MenuItem>, Without<PressText>, Without<DescText>)>;

/// The pause screen laid out: its list at the bottom of `_menu_mc`, the title up the left.
#[allow(clippy::too_many_arguments)]
fn pause(dt: f32, s: f32, off: Vec2, box_at: Vec2, open: f32, menu: &Menu, fe: &mut Frontend, main: &mut MainQuery, btns: &mut BtnQuery, texts: &mut TextQuery, hits: &mut HitQuery, title: &mut TitleQuery, desc: &mut DescQuery) {
    let n = fe.over.len();
    let mut menu_at = Vec2::new(48.15, 229.2);
    let mut title_m: Mat = [-0.16, -0.82, 0.82, -0.16, 29.0, 447.3];
    let mut go_m: Mat = [0.87, -0.23, 0.23, 0.87, -552.9, -80.8];
    for (mut fc, mut node) in main.iter_mut() {
        let p = off + PAUSE_AT * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        fc.scale = s;
        fc.alpha = open;
        if let Some((m, _)) = fc.clip.placed("_menu_mc") {
            menu_at = Vec2::new(m[4], m[5]);
        }
        if let Some((m, _)) = fc.clip.placed("_title_mc") {
            title_m = m;
        }
        if let Some((m, _)) = fc.clip.placed("_gameOver_mc") {
            go_m = m;
        }
    }
    // the game over's reason, along its turned backing
    for (_, mut f, mut col, mut node, mut ut, cn) in desc.iter_mut() {
        let fs = bevy::text::FontSize::Px(GO_TEXT_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let size = cn.size() * cn.inverse_scale_factor();
        let sc = (go_m[0] * go_m[0] + go_m[1] * go_m[1]).sqrt();
        let local = GO_TEXT_AT + Vec2::new(size.x / s * 0.5, size.y / s * 0.5);
        let c = off + (PAUSE_AT + apply(&go_m, local)) * s;
        node.left = Val::Px(c.x - size.x * 0.5);
        node.top = Val::Px(c.y - size.y * 0.5);
        ut.rotation = Rot2::radians(go_m[1].atan2(go_m[0]));
        ut.scale = Vec2::splat(sc);
        col.0 = TEXT_COLOR.with_alpha(open);
    }
    // (the pointer's hover is the menu's choice already: `menu_input`)
    let sel = menu.selected();
    for (i, o) in fe.over.iter_mut().enumerate() {
        o.1 = sel == i;
        let rate = if o.1 { dt / 0.25 } else { -dt / 0.25 };
        o.0 = (o.0 + rate).clamp(0.0, 1.0);
    }
    // bottom-aligned: the list ends at `_menu_mc`
    let total = n as f32 * PM_ITEM_H + (n.saturating_sub(1)) as f32 * PM_ITEM_GAP;
    let at = |i: usize, e: f32| PAUSE_AT + menu_at + Vec2::new(PM_OVER_X * e, i as f32 * (PM_ITEM_H + PM_ITEM_GAP) + PM_ITEM_H * 0.5 - total);
    let over = fe.over.clone();
    for (b, mut fc, mut node) in btns.iter_mut() {
        let (k, on) = over.get(b.0).copied().unwrap_or((0.0, false));
        let e = if on { back_out(k) } else { strong_out(k) };
        // (inside the mask's box)
        let p = off + at(b.0, e) * s - box_at;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        fc.scale = s;
        fc.alpha = open;
        let label = fc.label("").map(|l| l.to_string());
        if on && label.as_deref() != Some("over") {
            fc.goto("", "over", true);
        } else if !on && label.as_deref() == Some("over") {
            fc.goto("", "out", true);
        }
    }
    for (t, mut f, mut col, mut node, mut ut, _) in texts.iter_mut() {
        let (k, on) = over.get(t.0).copied().unwrap_or((0.0, false));
        let e = if on { back_out(k) } else { strong_out(k) };
        let fs = bevy::text::FontSize::Px(TEXT_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let p = off + (at(t.0, e) + PM_TEXT_AT) * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        ut.scale = Vec2::new(1.0, 1.2);
        let c = WHITE.mix(&OVER_COLOR, k);
        col.0 = c.with_alpha(open);
    }
    for (h, _, mut node) in hits.iter_mut() {
        let p = off + at(h.0, 0.0) * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y - PM_ITEM_H * 0.5 * s);
        node.width = Val::Px(480.0 * s);
        node.height = Val::Px(PM_ITEM_H * s);
    }
    // the title, along its turned clip
    for (mut f, mut col, mut node, mut ut, cn) in title.iter_mut() {
        let fs = bevy::text::FontSize::Px(PM_TITLE_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let size = cn.size() * cn.inverse_scale_factor();
        let local = PM_TITLE_AT + Vec2::new(size.x / s * 0.5, size.y / s * 0.6);
        let c = off + (PAUSE_AT + apply(&title_m, local)) * s;
        let sc = (title_m[0] * title_m[0] + title_m[1] * title_m[1]).sqrt();
        node.left = Val::Px(c.x - size.x * 0.5);
        node.top = Val::Px(c.y - size.y * 0.5);
        ut.rotation = Rot2::radians(title_m[1].atan2(title_m[0]));
        ut.scale = Vec2::new(sc, sc * 1.2);
        col.0 = TEXT_COLOR.with_alpha(open);
    }
}

/// The new game screen laid out: its list, the description and portrait for the one chosen.
#[allow(clippy::too_many_arguments)]
fn new_game(dt: f32, s: f32, off: Vec2, open: f32, menu: &Menu, fe: &mut Frontend, main: &mut MainQuery, btns: &mut BtnQuery, texts: &mut TextQuery, hits: &mut HitQuery, desc: &mut DescQuery, data: &crate::gamedata::Data) {
    let n = fe.over.len().saturating_sub(1);
    let menu_local = main.iter().next().and_then(|(fc, _)| fc.clip.placed("_menu_mc")).map(|(m, _)| m).unwrap_or([0.99757385, -0.06976318, 0.06976318, 0.99757385, -426.5, -25.95]);
    let mm = concat(&translate(NEWGAME_AT.x, NEWGAME_AT.y), &menu_local);
    let angle = mm[1].atan2(mm[0]);
    // (the pointer's hover is the menu's choice already: `menu_input`)
    let sel = menu.selected();
    for (i, o) in fe.over.iter_mut().enumerate() {
        o.1 = sel == i && i < n;
        let rate = if o.1 { dt / 0.25 } else { -dt / 0.25 };
        o.0 = (o.0 + rate).clamp(0.0, 1.0);
    }
    let chosen = sel.min(n.saturating_sub(1));
    for (mut fc, mut node) in main.iter_mut() {
        let p = off + NEWGAME_AT * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        fc.scale = s;
        fc.alpha = open;
        // Corvo's portrait for the difficulty (`SetPortrait`)
        let tl = fc.tl.clone();
        if let Some(c) = fc.clip.child_mut("_corvo_mc.portrait_mc") {
            if c.frame != chosen {
                c.goto(&tl, chosen, false);
            }
        }
    }
    let at = |i: usize, e: f32| Vec2::new(NG_ORIGIN_X + NG_OVER_X * e, i as f32 * (NG_BTN_H + NG_BTN_GAP) + NG_BTN_H * 0.5);
    let over = fe.over.clone();
    for (b, mut fc, mut node) in btns.iter_mut() {
        let (k, on) = over.get(b.0).copied().unwrap_or((0.0, false));
        let e = if on { back_out(k) } else { strong_out(k) };
        let m = concat(&mm, &translate(at(b.0, e).x, at(b.0, e).y));
        fc.m = [m[0], m[1], m[2], m[3], 0.0, 0.0];
        let p = off + Vec2::new(m[4], m[5]) * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        fc.scale = s;
        fc.alpha = open;
        let label = fc.label("").map(|l| l.to_string());
        if on && label.as_deref() != Some("over") {
            fc.goto("", "over", true);
        } else if !on && label.as_deref() == Some("over") {
            fc.goto("", "out", true);
        }
    }
    for (t, mut f, mut col, mut node, mut ut, cn) in texts.iter_mut() {
        let size = cn.size() * cn.inverse_scale_factor();
        let (k, on) = over.get(t.0).copied().unwrap_or((0.0, false));
        let e = if on { back_out(k) } else { strong_out(k) };
        let fs = bevy::text::FontSize::Px(NG_TEXT_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let center = if t.0 < n {
            // the label's box, its middle (stretched 1.2 high), on the stage
            let local = at(t.0, e) + NG_TEXT_AT + Vec2::new(size.x / s * 0.5, size.y / s * 0.6);
            off + apply(&mm, local) * s
        } else {
            off + BACK_AT * s
        };
        node.left = Val::Px(center.x - size.x * 0.5);
        node.top = Val::Px(center.y - size.y * 0.5);
        ut.rotation = Rot2::radians(if t.0 < n { angle } else { 0.0 });
        ut.scale = Vec2::new(1.0, if t.0 < n { 1.2 } else { 1.0 });
        let ok = if t.0 < n { k } else if over.get(t.0).is_some_and(|o| o.1) { 1.0 } else { 0.0 };
        let c = if t.0 < n { TEXT_COLOR.mix(&OVER_COLOR, ok) } else { TEXT_COLOR.with_alpha(if sel == t.0 { 1.0 } else { 0.7 }) };
        col.0 = c.with_alpha(c.alpha() * open);
    }
    for (h, _, mut node) in hits.iter_mut() {
        if h.0 < n {
            let c = off + apply(&mm, at(h.0, 0.0) + Vec2::new(66.0, 0.0)) * s;
            node.left = Val::Px(c.x - 190.0 * s);
            node.top = Val::Px(c.y - NG_BTN_H * 0.5 * s);
            node.width = Val::Px(380.0 * s);
            node.height = Val::Px(NG_BTN_H * s);
        } else {
            let c = off + BACK_AT * s;
            node.left = Val::Px(c.x - 60.0 * s);
            node.top = Val::Px(c.y - 20.0 * s);
            node.width = Val::Px(120.0 * s);
            node.height = Val::Px(40.0 * s);
        }
    }
    // the description of the one chosen
    let words = data.text("DisGFxMoviePlayerMainMenu_Texts", &format!("t_difficulty{chosen}Desc"));
    for (mut text, mut f, mut col, mut node, mut ut, cn) in desc.iter_mut() {
        if text.0 != words {
            text.0 = words.clone();
        }
        let fs = bevy::text::FontSize::Px(NG_DESC_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let size = cn.size() * cn.inverse_scale_factor();
        node.width = Val::Px(NG_DESC_W * s);
        let local = NG_DESC_AT + Vec2::new(NG_DESC_W * 0.5, size.y / s * 0.5);
        let c = off + apply(&mm, local) * s;
        node.left = Val::Px(c.x - NG_DESC_W * s * 0.5);
        node.top = Val::Px(c.y - size.y * 0.5);
        ut.rotation = Rot2::radians(angle);
        col.0 = TEXT_COLOR.with_alpha(open);
    }
}

/// Back.easeOut and Strong.easeOut (as Flash's tweens).
fn back_out(k: f32) -> f32 {
    let s = 1.70158;
    let k = k - 1.0;
    k * k * ((s + 1.0) * k + s) + 1.0
}

fn strong_out(k: f32) -> f32 {
    1.0 - (1.0 - k).powi(5)
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn layout(
    time: Res<Time<Real>>,
    menu: Res<Menu>,
    mut fe: ResMut<Frontend>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut main: MainQuery,
    mut btns: BtnQuery,
    mut texts: TextQuery,
    mut hits: HitQuery,
    mut press: Query<(&mut TextFont, &mut Node, &ComputedNode), (With<PressText>, Without<MainClip>, Without<BtnClip>, Without<BtnText>, Without<MenuItem>, Without<DescText>)>,
    (mut desc, data): (DescQuery, Res<crate::gamedata::Data>),
    mut title: TitleQuery,
    mut boxes: Query<&mut Node, (With<ClipBox>, Without<MainClip>, Without<BtnClip>, Without<BtnText>, Without<MenuItem>, Without<PressText>, Without<DescText>, Without<TitleText>)>,
) {
    let Some((screen, items)) = fe.shown.clone() else { return };
    let Ok(w) = window.single() else { return };
    let (s, off) = stage(w);
    let dt = time.delta_secs();
    fe.t += dt;
    // the open: everything fades in (and the bar slides in from the right) over 0.35 s
    let open = strong_out((fe.t / 0.35).min(1.0));
    let at = if screen == Screen::Main { MAIN_AT } else { START_AT };
    for (mut fc, mut n) in &mut main {
        let p = off + at * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
        fc.scale = s;
        fc.alpha = open;
    }
    if screen == Screen::Pause || screen == Screen::GameOver {
        let origin = off + Vec2::new(PM_MASK[0], PM_MASK[1]) * s;
        for mut n in &mut boxes {
            n.left = Val::Px(origin.x);
            n.top = Val::Px(origin.y);
            n.width = Val::Px((PM_MASK[2] - PM_MASK[0]) * s);
            n.height = Val::Px((PM_MASK[3] - PM_MASK[1]) * s);
        }
        pause(dt, s, off, origin, open, &menu, &mut fe, &mut main, &mut btns, &mut texts, &mut hits, &mut title, &mut desc);
        return;
    }
    if screen == Screen::NewGame {
        // the title, on its brush (turned with it: the clip's `_title_mc` is at -2 degrees)
        for (mut f, mut col, mut n, mut ut, cn) in &mut title {
            let fs = bevy::text::FontSize::Px(NG_TITLE_SIZE * s);
            if f.font_size != fs {
                f.font_size = fs;
            }
            let size = cn.size() * cn.inverse_scale_factor();
            let tm = [0.99938965, -0.03489685, 0.03489685, 0.99938965, NG_TITLE_AT.x, NG_TITLE_AT.y];
            let local = Vec2::new(size.x / s * 0.5, size.y / s * 0.6);
            let c = off + (NEWGAME_AT + apply(&tm, local)) * s;
            n.left = Val::Px(c.x - size.x * 0.5);
            n.top = Val::Px(c.y - size.y * 0.5);
            ut.rotation = Rot2::radians(tm[1].atan2(tm[0]));
            ut.scale = Vec2::new(1.0, 1.2);
            col.0 = OVER_COLOR.with_alpha(open);
        }
        new_game(dt, s, off, open, &menu, &mut fe, &mut main, &mut btns, &mut texts, &mut hits, &mut desc, &data);
        return;
    }
    if screen == Screen::Start {
        for (mut f, mut n, cn) in &mut press {
            let fs = bevy::text::FontSize::Px(TEXT_SIZE * s);
            if f.font_size != fs {
                f.font_size = fs;
            }
            let size = cn.size() * cn.inverse_scale_factor();
            let p = off + PRESS_AT * s;
            n.left = Val::Px(p.x - size.x * 0.5);
            n.top = Val::Px(p.y - size.y * 0.5);
        }
        return;
    }
    // the band: the main clip's `_menu_mc` placement, on the stage
    let band_local = main.iter().next().and_then(|(fc, _)| fc.clip.placed("_menu_mc")).map(|(m, _)| m).unwrap_or([0.9494171, 0.03314209, -0.03314209, 0.9494171, 640.35, 237.2]);
    let band = concat(&translate(MAIN_AT.x, MAIN_AT.y), &band_local);
    let band_scale = (band[0] * band[0] + band[1] * band[1]).sqrt();
    let angle = band[1].atan2(band[0]);
    // the labels' widths (stage units), once laid out
    let mut widths = vec![0.0f32; items.len()];
    let mut ready = true;
    for (t, _, _, _, _, cn) in &texts {
        let wpx = cn.size().x * cn.inverse_scale_factor();
        if wpx <= 0.0 {
            ready = false;
        }
        if let Some(wd) = widths.get_mut(t.0) {
            *wd = wpx / (s * band_scale);
        }
    }
    // the buttons side by side, right-aligned (`_btnOriginX` center, `_menuHAlignment` right)
    let bw: Vec<f32> = widths.iter().map(|w| w + LINE_EXTRA).collect();
    let total: f32 = bw.iter().sum();
    let mut centers = Vec::with_capacity(bw.len());
    let mut x = -total;
    for b in &bw {
        centers.push(x + b * 0.5);
        x += b;
    }
    // the slide in from the right
    let slide = (1.0 - open) * (total + 150.0);
    // the backing as wide as the buttons, less 10
    for (mut fc, _) in &mut main {
        if let Some((m, _)) = fc.clip.child_mut("_menu_mc").and_then(|c| c.placed_mut("_bkgd_mc")) {
            m[0] = ((total - 10.0) / BAND_W).max(0.05);
        }
    }
    // over: the pointer's, else the keys' choice
    // (the pointer's hover is the menu's choice already: `menu_input`)
    let sel = Some(menu.selected());
    for (i, o) in fe.over.iter_mut().enumerate() {
        let want = sel == Some(i);
        if want != o.1 {
            o.1 = want;
        }
        let rate = if o.1 { dt / 0.25 } else { -dt / 0.15 };
        o.0 = (o.0 + rate).clamp(0.0, 1.0);
    }
    let over = fe.over.clone();
    for (b, mut fc, mut n) in &mut btns {
        let Some(&c) = centers.get(b.0) else { continue };
        let (k, on) = over.get(b.0).copied().unwrap_or((0.0, false));
        let e = if on { back_out(k) } else { strong_out(k) };
        let grow = 1.0 + 0.1 * e;
        let local = [grow, 0.0, 0.0, grow, c + slide, 5.0 * e];
        let m = concat(&band, &local);
        // the clip's own matrix carries the band's turn and the button's growth
        fc.m = [m[0], m[1], m[2], m[3], 0.0, 0.0];
        let p = off + Vec2::new(m[4], m[5]) * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
        fc.scale = s;
        fc.alpha = if ready { open } else { 0.0 };
        // its lines about its label
        let wd = widths.get(b.0).copied().unwrap_or(0.0);
        if let Some((m, _)) = fc.clip.placed_mut("_leftLine_mc") {
            m[4] = -0.5 * wd - LINE_OFFSET;
            m[3] = TEXT_HEIGHT / SIDE_LINE_H;
        }
        if let Some((m, _)) = fc.clip.placed_mut("_rightLine_mc") {
            m[4] = 0.5 * wd + LINE_OFFSET;
            m[3] = TEXT_HEIGHT / SIDE_LINE_H;
        }
        if let Some((m, _)) = fc.clip.placed_mut("_upLine_mc") {
            m[0] = (wd + LINE_EXTRA) / UP_LINE_W;
            m[5] = -0.5 * TEXT_HEIGHT - LINE_OFFSET;
        }
        if let Some((m, _)) = fc.clip.placed_mut("_downLine_mc") {
            m[0] = (wd + LINE_EXTRA) / DOWN_LINE_W;
            m[5] = 0.5 * TEXT_HEIGHT + LINE_OFFSET;
        }
        // its over backing (`SetBkgd`: the brush, its second frame), masked to its width
        let tl = fc.tl.clone();
        if let Some(c) = fc.clip.child_mut("_bkgdOver_mc") {
            if c.frame == 0 {
                c.goto(&tl, 1, false);
            }
        }
        if let Some((m, _)) = fc.clip.placed_mut("_maskBkgdOver_mc") {
            m[0] = (wd + LINE_EXTRA) / MASK_W;
        }
        // over and out
        let label = fc.label("").map(|l| l.to_string());
        if on && !matches!(label.as_deref(), Some("over") | Some("loop_over")) {
            fc.goto("", "over", true);
        } else if !on && matches!(label.as_deref(), Some("over") | Some("loop_over")) {
            fc.goto("", "out", true);
        }
    }
    for (t, mut f, mut col, mut n, mut ut, cn) in &mut texts {
        let Some(&c) = centers.get(t.0) else { continue };
        let (k, on) = over.get(t.0).copied().unwrap_or((0.0, false));
        let e = if on { back_out(k) } else { strong_out(k) };
        let grow = 1.0 + 0.1 * e;
        let fs = bevy::text::FontSize::Px(TEXT_SIZE * s * band_scale);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let size = cn.size() * cn.inverse_scale_factor();
        // (over: dark and a little higher, as the light backing comes up behind it)
        let ok = k.min(1.0);
        let p = off + apply(&band, Vec2::new(c + slide, 5.0 * e - OVER_RISE * ok)) * s;
        n.left = Val::Px(p.x - size.x * 0.5);
        n.top = Val::Px(p.y - size.y * 0.5);
        ut.rotation = Rot2::radians(angle);
        ut.scale = Vec2::splat(grow);
        let mixed = TEXT_COLOR.mix(&OVER_COLOR, ok);
        col.0 = mixed.with_alpha(if ready { open } else { 0.0 });
    }
    for (h, _, mut n) in &mut hits {
        let Some(&c) = centers.get(h.0) else { continue };
        let bwid = bw.get(h.0).copied().unwrap_or(0.0) * s * band_scale;
        let p = off + apply(&band, Vec2::new(c + slide, 0.0)) * s;
        let hgt = (TEXT_HEIGHT + 2.0 * LINE_OFFSET) * s * band_scale;
        n.left = Val::Px(p.x - bwid * 0.5);
        n.top = Val::Px(p.y - hgt * 0.5);
        n.width = Val::Px(bwid);
        n.height = Val::Px(hgt);
    }
}
