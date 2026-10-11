//! Dunwall City Trials' menu (`UI_ChallengesMenu_DLC05.ChallengesMenu`, cooked as
//! `ChallengesMenu_DLC05`), over its own menu map (`L_DLC05_MainMenu_P`), as its classes show
//! it: the home screen (`CM_Home_Screen`: the Trials' name on its brush, the menu rising from
//! the bottom left, `CM_Home_Menu_item`s 5 apart, the chosen one along 30), and the challenges
//! (`CM_ChallengesList_Screen`): the modes' tabs (`lib_mTabs_tab`: normal, expert), the
//! challenges' grid (`CM_ChallengesList_item`, 5 across, overlapping 20, each a little turned,
//! its picture `ChallengeImg_<id>_Small` loaded in, the chosen one grown 15%; the locked
//! padlocked), and the chosen one's details: its picture (`_Large`), kind, name and words (an
//! expert mode's first, in red), the best score's medal (`DLC05_CM_Medal`), the scores the
//! stars take (`_hintScore_mc`), or, locked, what opens it (`t_ChallengeUnlockCondition`).
//! And the gallery (`CM_Gallery_Screen`): its pieces' pictures in a grid (`CM_Gallery_List`: 7
//! across, 3 rows in view, each `UI_<id>_S`, the new ones marked, the locked padlocked), a
//! locked one's way to it (`t_ArtworkUnlockCondition_*`), and the chosen one large
//! (`CM_Gallery_ViewPort_Screen`: `UI_<id>_L` in its frame, the arrows to the next unlocked).
//! And the welcome the first time (`CM_WelcomeDisclaimer_Screen`): its words, the challenges'
//! kinds, a medal, scrolled within their mask. And the leaderboards (`Leaderboards_Screen`, its
//! own movie `Leaderboards_DLC05`): a challenge's details, the arrows to the others, a page of
//! nine rows (`Leaderboards_List_item`: rank, name, score; the player's marked), the modes'
//! tabs. The original's were online; these are the profile's own best runs, the last marked.
//! The menu's items and their choosing are `menu.rs`'s; this draws them.

use crate::challenge::{best_key, ChallengeProfile};
use crate::flash::{Clip, Ease, FlashClip, LoadedImage, MovieTimelines, PropsTo, IDENTITY};
use crate::menu::{Menu, MenuItem, GALLERY_COLS};
use crate::GameState;
use bevy::prelude::*;

pub struct Dlc05MenuPlugin;

impl Plugin for Dlc05MenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (draw, tabs, arrows, board_arrows, welcome).chain().after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "ChallengesMenu_DLC05";
const LIB: &str = "lib";
const MEDAL_MOVIE: &str = "Brief_DLC05";
const BOARD_MOVIE: &str = "Leaderboards_DLC05";
const CM_TEXTS: &str = "DisDLC05MoviePlayerChallengeMenu_Texts";
const B_TEXTS: &str = "DisGFxMoviePlayerBase_Texts";
/// the grid's columns (`InitList(challList, 5, 2)`) and the items' overlap
/// (`_mcCreator._offsetX`, `_offsetY`, `_offsetOriginX`)
const COLS: usize = 5;
const OVERLAP: f32 = -20.0;
const ORIGIN_X: f32 = 5.0;
/// the list's place (`SetChallengesList`: the 85% safe area's left and 25 in, 60 under the
/// tabs) and the tabs'
const LIST_AT: Vec2 = Vec2::new(-640.0 + 96.0 + 25.0, -250.0 + 60.0);
const TABS_Y: f32 = -250.0;
/// the details' medal (`_details_mc` 445.8, 177.8 + `_medal_mc` -116.7, -370.6)
const MEDAL_AT: Vec2 = Vec2::new(445.8 - 116.7, 177.8 - 370.6);
/// `_btnOverProps` of the grid (115%) and of the home menu (30 along)
const ITEM_OVER: f32 = 1.15;
const HOME_OVER: f32 = 30.0;
/// the gallery's rows in view (`InitList(list, 7, 3)`), its items' overlap (`_offsetX`, -15)
const GALLERY_ROWS: usize = 3;
const GALLERY_OVERLAP: f32 = -15.0;

#[derive(Component)]
struct TrialsRoot;

/// What a part of the screen is.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum Part {
    Home,
    List,
    Tab(bool),
    Medal,
    Gallery,
    Welcome,
    Board,
}

/// A leaderboard's arrows (`_arrL_mc`, `_arrR_mc`): the challenge before, after.
#[derive(Component)]
struct BoardArrow(i32);

/// The welcome's words scrolled (`AnalogScrollView`: from their top, as far as they go
/// past their mask).
#[derive(Component)]
struct Welcome {
    scroll: f32,
    max: f32,
}

/// The welcome's medal (the briefing movie's) and the cut it is drawn in (the words' mask),
/// its button.
#[derive(Component)]
struct WelcomeMedal;
#[derive(Component)]
struct WelcomeCut;
#[derive(Component)]
struct WelcomeButton;

/// The large piece's arrows (`_arrL_mc`, `_arrR_mc`): the previous, the next unlocked.
#[derive(Component)]
struct GalleryArrow(i32);

/// A mode's tab's button (the pointer on it changes the list).
#[derive(Component)]
struct TabButton(bool);

/// What is drawn: the screen (`front_page`'s 8, 9, 10), its items, the item shown chosen.
#[derive(Default)]
struct Shown {
    page: Option<(u8, Vec<String>)>,
    sel: Option<usize>,
    size: UVec2,
    /// the gallery: the first row in view, the piece seen large, the screen last built
    scroll: usize,
    view: Option<usize>,
    code: u8,
    /// a leaderboard's row chosen
    row: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    menu: Res<Menu>,
    roots: Query<Entity, With<TrialsRoot>>,
    mut parts: Query<(&Part, &mut FlashClip)>,
    window: Query<&Window>,
    mut timelines: ResMut<MovieTimelines>,
    data: Res<crate::gamedata::Data>,
    mut profile: ResMut<ChallengeProfile>,
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
    mut shown: Local<Shown>,
) {
    let Ok(w) = window.single() else { return };
    let size = UVec2::new(w.width() as u32, w.height() as u32);
    let page = menu.front_page().filter(|p| (8..=14).contains(&p.0));
    if page != shown.page || size != shown.size {
        shown.page = page.clone();
        shown.size = size;
        shown.sel = None;
        shown.view = None;
        // (the gallery opened afresh: its first rows)
        let code_now = page.as_ref().map_or(0, |p| p.0);
        if code_now != shown.code {
            shown.scroll = 0;
        }
        shown.code = code_now;
        for e in &roots {
            commands.entity(e).despawn();
        }
        let Some((code, labels)) = page else { return };
        let (Some(tl), Some(lib), Some(mtl)) = (timelines.get(MOVIE), timelines.get(LIB), timelines.get(MEDAL_MOVIE)) else { return };
        let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
        let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
        // (over a run's results, `dlc05results`' 70, when a leaderboard opens from them)
        let z = if menu.over_results() { 75 } else { 20 };
        let root = commands.spawn((TrialsRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(z), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
        let at = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
        // a button over a stage rectangle (its middle, its size)
        let button = |commands: &mut Commands, c: Vec2, size: Vec2| {
            let p = off + (c - size * 0.5) * s;
            commands.spawn((Button, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(size.x * s), height: Val::Px(size.y * s), ..default() }, ZIndex(5), ChildOf(root))).id()
        };
        // the vignette over it all
        if let Some(c) = Clip::export(&tl, "m_Vignette") {
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
            fc.scale = s;
            commands.spawn((fc, at(Vec2::new(640.0, 360.0)), ZIndex(3), Pickable::IGNORE, ChildOf(root)));
        }
        if code == 8 {
            // the home screen
            let Some(c) = Clip::export(&tl, "CM_Home_Screen") else { return };
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
            fc.scale = s;
            let title = data.text(CM_TEXTS, "t_DLC05_Name").to_uppercase();
            for p in ["_title_mc._txt_mc.txt", "_title_mc._txtShadow0_mc.txt", "_title_mc._txtShadow1_mc.txt"] {
                fc.set_text(p, title.clone());
                fc.wrap.insert(p.into(), false);
            }
            // the menu: its items from the bottom up (`_menuVAlignment` bottom), centred on
            // their origins, 5 apart
            let mut h = 40.0;
            let mut bw = 330.0;
            for (i, l) in labels.iter().enumerate() {
                fc.attach("_menu_mc", "CM_Home_Menu_item", &format!("btn{i}"), IDENTITY);
                let p = format!("_menu_mc.btn{i}");
                if i == 0 {
                    h = fc.size(&p).map_or(40.0, |v| v.y).clamp(20.0, 80.0);
                    bw = fc.size(&format!("{p}.btn")).map_or(330.0, |v| v.x).clamp(100.0, 600.0);
                }
                fc.set_visible(&format!("{p}.btn"), false);
                fc.set_text(&format!("{p}._txt_mc.txt"), l.to_uppercase());
                fc.wrap.insert(format!("{p}._txt_mc.txt"), false);
            }
            let n = labels.len() as f32;
            let total = n * h + (n - 1.0) * 5.0;
            let menu_at = fc.props("_menu_mc").map_or(Vec2::new(-520.0, 298.3), |p| Vec2::new(p.x, p.y));
            let k = fc.props("_menu_mc").map_or(0.97, |p| p.xscale);
            for i in 0..labels.len() {
                let y = -total + i as f32 * (h + 5.0) + 0.5 * h;
                fc.set(&format!("_menu_mc.btn{i}"), PropsTo::default().y(y).x(0.0));
                let c = Vec2::new(640.0, 360.0) + menu_at + Vec2::new(bw * 0.5, y) * k;
                let b = button(&mut commands, c, Vec2::new(bw, h) * k);
                commands.entity(b).insert(MenuItem(i));
            }
            commands.spawn((Part::Home, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
            return;
        }
        if code == 13 || code == 14 {
            // a leaderboard (`Leaderboards_Screen.Open`, `PlayOpenAnimation`, `InitTabs`): its
            // rows filled once built (`FillLeaderboard`)
            let expert = code == 14;
            let Some(btl) = timelines.get(BOARD_MOVIE) else { return };
            let Some(c) = Clip::export(&btl, "Leaderboards_Screen") else { return };
            let mut fc = FlashClip::new(BOARD_MOVIE, btl.clone(), c).real().with_texts();
            fc.scale = s;
            fc.set_visible("", true);
            // (the online filters, the pad's shoulders, the loading, the tooltip: not here)
            for p in ["_subTabs_mc", "_tabs_mc", "_loading_mc", "_ttip_mc", "_arrL_mc.btn", "_arrR_mc.btn"] {
                fc.set_visible(p, false);
            }
            for p in ["_arrL_mc.arrow_mc", "_arrR_mc.arrow_mc"] {
                fc.goto(p, "loop", true);
            }
            // (the filters' bar, empty without them)
            if let Some((_, cx)) = fc.clip.placed_at_mut(33) {
                cx[3] = 0.0;
            }
            // the rows' list, masked (`ItemsList`: its `maskContent`)
            fc.create_empty("_list_mc", "_rows_mc", IDENTITY);
            fc.set_mask("_list_mc._rows_mc", "_list_mc.maskContent");
            if let Some(sp) = fc.props("_bkgd_mc") {
                fc.set("_bkgd_mc", PropsTo::default().alpha(0.0).x(sp.x - 250.0));
                fc.tween("_bkgd_mc", sp.into(), 0.25, Ease::StrongInOut);
            }
            // the arrows' buttons
            for (dir, p) in [(-1, "_arrL_mc"), (1, "_arrR_mc")] {
                if let Some((c, size)) = stage_rect(&fc, &format!("{p}.btn")) {
                    let b = button(&mut commands, Vec2::new(640.0, 360.0) + c, size);
                    commands.entity(b).insert(BoardArrow(dir));
                }
            }
            // the modes' tabs (`_tabLinkageName = 'lib_mTabs_tab'`, from the safe area's left)
            for (k, (key, ex)) in [("t_NormalMode", false), ("t_ExpertMode", true)].into_iter().enumerate() {
                let Some(c) = Clip::export(&lib, "lib_mTabs_tab") else { continue };
                let mut tab = FlashClip::new(LIB, lib.clone(), c).real().with_texts();
                tab.scale = s;
                tab.set_text("tab_mc.txt_mc.txt", data.text(B_TEXTS, key).to_uppercase());
                tab.set_visible("btn", false);
                tab.set_visible("tab_mc.icUpdate_mc", false);
                tab.goto("tab_mc", if ex == expert { "selected" } else { "unselected" }, true);
                let x = -640.0 + 96.0 - 50.0 + 135.0 + k as f32 * 211.0;
                let c = Vec2::new(640.0 + x, 360.0 + BOARD_TABS_Y);
                commands.spawn((Part::Tab(ex), tab, at(c), Pickable::IGNORE, ChildOf(root)));
                let b = button(&mut commands, c + Vec2::new(0.0, -20.0), Vec2::new(250.0, 50.0));
                commands.entity(b).insert(TabButton(ex));
            }
            commands.spawn((Part::Board, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
            return;
        }
        if code == 12 {
            // the welcome (`CM_WelcomeDisclaimer_Screen.InitDisplay`, `Open`)
            let Some(c) = Clip::export(&tl, "CM_WelcomeDisclaimer_Screen") else { return };
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
            fc.scale = s;
            fc.set_visible("", true);
            let raw = |k: &str| data.0.texts.get(&format!("{CM_TEXTS}.{k}")).cloned().unwrap_or_default().replace("§C_WHITE§", "<font color=\"#FFFFFF\">").replace("§C§", "</font>");
            let title = data.text(CM_TEXTS, "t_DLC05_Name").to_uppercase();
            for p in ["_title_mc._txt_mc.txt", "_title_mc._txtShadow0_mc.txt", "_title_mc._txtShadow1_mc.txt"] {
                fc.set_text(p, title.clone());
                fc.wrap.insert(p.into(), false);
            }
            let t = "_content_mc.txt_mc";
            let last = format!("{}<br>{}<br> ", raw("t_Welcome_txt2"), raw("t_Welcome_txt3_PC"));
            for (f, words) in [("txt_0", raw("t_Welcome_txt0")), ("txt_1", raw("t_Welcome_txt1")), ("txt_2", last)] {
                fc.set_text(&format!("{t}.{f}"), words);
                fc.wrap.insert(format!("{t}.{f}"), true);
            }
            // the challenges' kinds, their icons and names
            for (i, k) in ["t_MobilityType", "t_StealthType", "t_ActionType", "t_PuzzleType"].into_iter().enumerate() {
                fc.goto_frame(&format!("{t}.types_mc.ic{i}.ic"), i + 1, false);
                fc.set_text(&format!("{t}.types_mc.ic{i}.txt"), data.text(B_TEXTS, k).to_uppercase());
            }
            // (each part under the last, 15 apart: `_y + _height + 15`; the kinds 10 more)
            let at_y = |fc: &mut FlashClip, p: &str, y: f32| fc.set(&format!("{t}.{p}"), PropsTo::default().y(y));
            let y0 = fc.props(&format!("{t}.txt_0")).map_or(2.3, |p| p.y);
            let ty = y0 + fc.text_height(&format!("{t}.txt_0")).unwrap_or(83.0) + 25.0;
            at_y(&mut fc, "types_mc", ty);
            let y1 = ty + fc.size(&format!("{t}.types_mc")).map_or(180.0, |v| v.y) + 15.0;
            at_y(&mut fc, "txt_1", y1);
            let my = y1 + fc.text_height(&format!("{t}.txt_1")).unwrap_or(55.0) + 15.0;
            at_y(&mut fc, "medal_mc", my);
            let y2 = my + fc.size(&format!("{t}.medal_mc")).map_or(320.0, |v| v.y) + 15.0;
            at_y(&mut fc, "txt_2", y2);
            let bottom = y2 + fc.text_height(&format!("{t}.txt_2")).unwrap_or(110.0);
            let view = fc.size("_content_mc.mask_mc").map_or(408.0, |v| v.y);
            fc.set_mask(t, "_content_mc.mask_mc");
            // the button (`SetClickableButton('A', t_ContinueGame)`)
            let label = "_btn_mc._content_mc.txt";
            fc.set_text(label, data.text(B_TEXTS, "t_ContinueGame").to_uppercase());
            fc.wrap.insert(label.into(), false);
            fc.autosize.insert(label.into(), 0);
            if let (Some(p), Some((a, b))) = (fc.props("_btn_mc"), fc.clip.child("_btn_mc").and_then(|c| c.bounds(&tl))) {
                let w = fc.text_width(label).unwrap_or(100.0).max(b.x - a.x);
                let c = Vec2::new(640.0 + p.x + a.x + w * 0.5, 360.0 + p.y + (a.y + b.y) * 0.5);
                let btn = button(&mut commands, c, Vec2::new(w, b.y - a.y).max(Vec2::splat(30.0)));
                commands.entity(btn).insert((MenuItem(0), WelcomeButton));
            }
            // (`Open`: the backdrop in from the left, the button and the words from large)
            for (p, from, secs, ease) in [
                ("_bkgd_mc", None, 0.35, Ease::BackInOut),
                ("_btn_mc", Some(PropsTo::default().alpha(0.0).xscale(1.5).yscale(2.0).rotation(-35.0)), 0.25, Ease::BackInOut),
                ("_content_mc", Some(PropsTo::default().alpha(0.0).xscale(1.5).yscale(2.0)), 0.25, Ease::StrongOut),
            ] {
                let Some(sp) = fc.props(p) else { continue };
                fc.set(p, from.unwrap_or(PropsTo::default().alpha(0.0).x(sp.x - 250.0)));
                fc.tween(p, sp.into(), secs, ease);
            }
            // the medal (`medal_mc.mc.SetChallengeMedal(2, 26830)`: the briefing's), drawn
            // within the words' mask
            if let Some(c) = Clip::export(&mtl, "DLC05_CM_Medal") {
                let mut mc = FlashClip::new(MEDAL_MOVIE, mtl.clone(), c).real().with_texts();
                mc.scale = s;
                show_medal(&mut mc, 26830, 2, &data.text(B_TEXTS, "t_Pts"));
                let cut = commands.spawn((WelcomeCut, Node { position_type: PositionType::Absolute, overflow: Overflow::clip(), ..default() }, ZIndex(1), Pickable::IGNORE, ChildOf(root))).id();
                commands.spawn((WelcomeMedal, mc, Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE, ChildOf(cut)));
            }
            commands.spawn((Part::Welcome, Welcome { scroll: 0.0, max: (bottom - view).max(0.0) }, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
            return;
        }
        if code == 11 {
            // the gallery (`CM_Gallery_Screen.Open`, `FillGalleryList`, `CM_Gallery_List.SetList`)
            let Some(c) = Clip::export(&tl, "CM_Gallery_Screen") else { return };
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
            fc.scale = s;
            fc.set_visible("", true);
            fc.set_visible("_viewport_mc", false);
            fc.set("_lockInfos_mc._lock_mc", PropsTo::default().alpha(0.0));
            let list = fc.props("_list_mc").map_or(Vec2::new(-479.4, -304.6), |p| Vec2::new(p.x, p.y));
            fc.create_empty("_list_mc", "_grid_mc", IDENTITY);
            let (mut iw, mut ih) = (114.0, 114.0);
            let scroll = shown.scroll;
            for i in 0..labels.len() {
                let Some((g, locked)) = menu.item_gallery(i) else { continue };
                let row = i / GALLERY_COLS;
                if row < scroll || row >= scroll + GALLERY_ROWS {
                    continue;
                }
                let p = format!("_list_mc._grid_mc.item{i}");
                fc.attach("_list_mc._grid_mc", "CM_ChallengesList_item", &format!("item{i}"), IDENTITY);
                if let Some(v) = fc.size(&format!("{p}.btn")) {
                    (iw, ih) = (v.x.clamp(40.0, 300.0), v.y.clamp(40.0, 300.0));
                }
                fc.set_visible(&format!("{p}.btn"), false);
                fc.set_visible(&format!("{p}.ic_mc"), false);
                fc.goto(&p, if locked { "stop_lock" } else { "default" }, false);
                fc.set_visible(&format!("{p}.lock_mc"), locked);
                let item = data.0.gallery.get(g);
                let new = !locked && item.is_some_and(|d| !profile.gallery_seen.contains(&d.id));
                fc.set_visible(&format!("{p}.new_mc"), new);
                if new {
                    fc.goto(&format!("{p}.new_mc"), "loop", true);
                }
                let (col, r) = ((i % GALLERY_COLS) as f32, (row - scroll) as f32);
                let x = ORIGIN_X + iw * 0.5 + col * (GALLERY_OVERLAP + iw);
                let y = ih * 0.5 + r * (GALLERY_OVERLAP + ih);
                // (each a little turned: `getRandomNum(-2.5, 1.75)`, the same each time)
                let turn = -2.5 + 4.25 * (((i * 7919) % 100) as f32 / 100.0);
                fc.set(&p, PropsTo::default().x(x).y(y).rotation(turn));
                if let Some(d) = item {
                    let thumb = ui.file(&mut images, "dlc05gallery", &format!("UI_{}_S", d.id));
                    fc.load_image(&format!("{p}.imgLoader"), thumb);
                }
                let b = button(&mut commands, Vec2::new(640.0, 360.0) + list + Vec2::new(x, y), Vec2::new(iw + GALLERY_OVERLAP, ih + GALLERY_OVERLAP));
                commands.entity(b).insert(MenuItem(i));
            }
            // (the large piece's arrows, on its frame's sides)
            for (dir, x) in [(-1, -650.0), (1, 650.0)] {
                let b = button(&mut commands, Vec2::new(640.0 + x - 38.5 * dir as f32, 360.0), Vec2::new(70.0, 200.0));
                commands.entity(b).insert(GalleryArrow(dir));
            }
            commands.spawn((Part::Gallery, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
            return;
        }
        // the challenges: the screen, its list
        let expert = code == 10;
        let Some(c) = Clip::export(&tl, "CM_ChallengesList_Screen") else { return };
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
        fc.scale = s;
        // (the tabs are the shared library's; the shoulders' buttons a pad's)
        fc.set_visible("_tabs_mc", false);
        fc.create_empty("", "_list_mc", [1.0, 0.0, 0.0, 1.0, LIST_AT.x, LIST_AT.y]);
        let (mut iw, mut ih) = (114.0, 114.0);
        for i in 0..labels.len() {
            let Some((ci, _, locked)) = menu.item_challenge(i) else { continue };
            let Some(def) = data.0.challenges.get(ci) else { continue };
            let p = format!("_list_mc.item{i}");
            fc.attach("_list_mc", "CM_ChallengesList_item", &format!("item{i}"), IDENTITY);
            if i == 0 {
                if let Some(v) = fc.size(&format!("{p}.btn")) {
                    (iw, ih) = (v.x.clamp(40.0, 300.0), v.y.clamp(40.0, 300.0));
                }
            }
            fc.set_visible(&format!("{p}.btn"), false);
            fc.set_visible(&format!("{p}.new_mc"), false);
            fc.goto(&p, if locked { "stop_lock" } else { "default" }, false);
            fc.goto_frame(&format!("{p}.ic_mc"), kind_of(&def.kind), false);
            let (col, row) = ((i % COLS) as f32, (i / COLS) as f32);
            let x = ORIGIN_X + iw * 0.5 + col * (OVERLAP + iw);
            let y = ih * 0.5 + row * (OVERLAP + ih);
            // (each a little turned: `getRandomNum(-2.5, 1.75)`, the same each time)
            let turn = -2.5 + 4.25 * (((i * 7919) % 100) as f32 / 100.0);
            fc.set(&p, PropsTo::default().x(x).y(y).rotation(turn));
            let thumb = ui.file(&mut images, "dlc05", &format!("ChallengeImg_{}_Small", def.id));
            fc.load_image(&format!("{p}.imgLoader"), thumb);
            let b = button(&mut commands, Vec2::new(640.0, 360.0) + LIST_AT + Vec2::new(x, y), Vec2::new(iw + OVERLAP, ih + OVERLAP));
            commands.entity(b).insert(MenuItem(i));
        }
        commands.spawn((Part::List, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
        // the modes' tabs (`TabsHandler`: 271 wide, overlapping 60, from the safe area's left)
        for (k, (key, ex)) in [("t_NormalMode", false), ("t_ExpertMode", true)].into_iter().enumerate() {
            let Some(c) = Clip::export(&lib, "lib_mTabs_tab") else { continue };
            let mut fc = FlashClip::new(LIB, lib.clone(), c).real().with_texts();
            fc.scale = s;
            fc.set_text("tab_mc.txt_mc.txt", data.text(B_TEXTS, key).to_uppercase());
            fc.set_visible("btn", false);
            fc.set_visible("tab_mc.icUpdate_mc", false);
            fc.goto("tab_mc", if ex == expert { "selected" } else { "unselected" }, true);
            let x = -640.0 + 96.0 - 50.0 + 135.0 + k as f32 * 211.0;
            let c = Vec2::new(640.0 + x, 360.0 + TABS_Y);
            commands.spawn((Part::Tab(ex), fc, at(c), Pickable::IGNORE, ChildOf(root)));
            let b = button(&mut commands, c + Vec2::new(0.0, -20.0), Vec2::new(250.0, 50.0));
            commands.entity(b).insert(TabButton(ex));
        }
        // the best score's medal
        if let Some(c) = Clip::export(&mtl, "DLC05_CM_Medal") {
            let mut fc = FlashClip::new(MEDAL_MOVIE, mtl.clone(), c).real().with_texts();
            fc.scale = s;
            fc.set_visible("", false);
            commands.spawn((Part::Medal, fc, at(Vec2::new(640.0, 360.0) + MEDAL_AT), Pickable::IGNORE, ChildOf(root)));
        }
        return;
    }
    let Some((code, labels)) = shown.page.clone() else { return };
    let sel = menu.selected();
    if code == 13 || code == 14 {
        // a leaderboard: its challenge changed (`OnChallengeChange`: the details, the rows
        // anew) or its row chosen (`playSelectionOn` / `Out`)
        if shown.sel == Some(sel) && shown.row == menu.board_row {
            return;
        }
        let fresh = shown.sel != Some(sel);
        shown.sel = Some(sel);
        shown.row = menu.board_row;
        let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| **p == Part::Board) else { return };
        let Some((ci, expert)) = menu.item_board(sel) else { return };
        let Some(def) = data.0.challenges.get(ci) else { return };
        let (runs, last) = profile.board(&best_key(&def.id, expert));
        let row = menu.board_row.or(last).unwrap_or(0);
        if fresh {
            fill_board(&mut fc, def, (sel, labels.len()), &runs, last, &data, &mut ui, &mut images);
        }
        for k in 0..runs.len().min(crate::challenge::BOARD_RUNS) {
            let p = format!("_list_mc._rows_mc.row{k}");
            let on = k == row;
            let was = fc.props(&p).is_some_and(|q| q.x > 1.0);
            if on != was || fresh {
                fc.tween(&p, PropsTo::default().x(if on { 15.0 } else { 0.0 }), if on { 0.2 } else { 0.15 }, if on { Ease::BackInOut } else { Ease::StrongOut });
                if on || was {
                    fc.goto(&p, if on { "over" } else { "out" }, true);
                }
            }
        }
        return;
    }
    if code == 11 {
        // (the chosen row out of view: the rows scrolled, the screen built again)
        let row = sel / GALLERY_COLS;
        let want = if row < shown.scroll { row } else if row >= shown.scroll + GALLERY_ROWS { row + 1 - GALLERY_ROWS } else { shown.scroll };
        if want != shown.scroll {
            shown.scroll = want;
            shown.page = None;
            return;
        }
        // a piece seen large, or not (`CM_Gallery_ViewPort_Screen.Open`, `Close`)
        if shown.view != menu.gallery_view {
            let was = shown.view;
            shown.view = menu.gallery_view;
            let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| **p == Part::Gallery) else { return };
            match menu.gallery_view.and_then(|i| menu.item_gallery(i)) {
                Some((g, _)) => {
                    let id = data.0.gallery.get(g).map(|d| d.id.clone()).unwrap_or_default();
                    for p in ["_list_mc", "_lockInfos_mc"] {
                        fc.set_visible(p, false);
                    }
                    for p in ["_RB_mc", "_LB_mc", "_scrollView_mc._loadingAnim_mc"] {
                        fc.set_visible(&format!("_viewport_mc.{p}"), false);
                    }
                    fc.set_visible("_viewport_mc", true);
                    // (its picture fitted to the view's frame, about its middle)
                    if let Some((image, natural)) = ui.file(&mut images, "dlc05gallery", &format!("UI_{id}_L")) {
                        let zone = fc.size("_viewport_mc._scrollView_mc._zone_mc").unwrap_or(Vec2::new(1090.0, 568.0));
                        let k = (zone.x / natural.x.max(1.0)).min(zone.y / natural.y.max(1.0));
                        let size = natural * k;
                        fc.images.insert("_viewport_mc._scrollView_mc._imgLoader_mc".into(), LoadedImage { image, size, at: -size * 0.5 });
                    }
                    if was.is_none() {
                        fc.set("_viewport_mc", PropsTo::default().alpha(0.0).scale(1.1));
                        fc.tween("_viewport_mc", PropsTo::default().alpha(1.0).scale(1.0), 0.25, Ease::StrongOut);
                    }
                    // (seen: no longer new)
                    if !id.is_empty() && profile.gallery_seen.insert(id) {
                        profile.save();
                    }
                }
                None => {
                    // (back to the list: its marks again)
                    shown.page = None;
                }
            }
            return;
        }
    }
    // the chosen item: drawn so, and (in the lists) its details
    if shown.sel == Some(sel) {
        return;
    }
    let before = shown.sel;
    shown.sel = Some(sel);
    if code == 11 {
        let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| **p == Part::Gallery) else { return };
        // (`playSelectionOn` / `Out`)
        for i in 0..labels.len() {
            let p = format!("_list_mc._grid_mc.item{i}");
            if fc.props(&p).is_none() {
                continue;
            }
            let item_locked = menu.item_gallery(i).is_some_and(|g| g.1);
            if i == sel {
                fc.tween(&p, PropsTo::default().scale(ITEM_OVER), 0.2, Ease::BackInOut);
                fc.goto(&p, if item_locked { "over_lock" } else { "over" }, true);
            } else if before == Some(i) || before.is_none() {
                fc.tween(&p, PropsTo::default().scale(1.0), 0.15, Ease::StrongOut);
                if before == Some(i) {
                    fc.goto(&p, if item_locked { "out_lock" } else { "out" }, true);
                }
            }
        }
        // a locked one's way to it (`CM_Gallery_Details.SetDetails`)
        let lock = "_lockInfos_mc._lock_mc";
        fc.tween_end(lock, true);
        let bx = fc.props(lock).map_or(110.5, |p| p.x);
        match menu.item_gallery(sel) {
            Some((g, true)) => {
                let Some(item) = data.0.gallery.get(g) else { return };
                let white = |v: String| format!("<font color=\"#FFFFFF\">{v}</font>");
                let raw = |k: &str| data.0.texts.get(&format!("{CM_TEXTS}.{k}")).cloned().unwrap_or_default();
                let expert = item.all_expert || (item.expert && !item.normal);
                let mode = white(data.text(B_TEXTS, if expert { "t_ExpertMode" } else { "t_NormalMode" }));
                let t = if item.all_normal || item.all_expert {
                    raw("t_ArtworkUnlockCondition_1").replace("§MODE§", &mode)
                } else {
                    let def = data.0.challenges.iter().find(|c| c.leaderboard == item.challenge);
                    let medals = def.map_or([0; 3], |c| if expert { c.expert_medals } else { c.medals });
                    let pts = medals.get((item.stars.max(1) - 1) as usize).copied().unwrap_or(0);
                    raw(if item.stars > 1 { "t_ArtworkUnlockCondition_0p" } else { "t_ArtworkUnlockCondition_0" })
                        .replace("§STARS§", &white(item.stars.to_string()))
                        .replace("§PTS_COUNT§", &white(format!("{pts} {}", data.text(B_TEXTS, "t_Pts"))))
                        .replace("§CHALLENGE§", &white(def.map_or(String::new(), |c| c.name.clone())))
                        .replace("§MODE§", &mode)
                };
                fc.set_text(&format!("{lock}.txt"), t);
                fc.set(lock, PropsTo::default().x(bx - 250.0).alpha(0.0));
                fc.tween(lock, PropsTo::default().x(bx).alpha(1.0), 0.22, Ease::StrongOut);
            }
            _ => fc.tween(lock, PropsTo::default().alpha(0.0), 0.25, Ease::StrongOut),
        }
        return;
    }
    if code == 8 {
        if let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| **p == Part::Home) {
            for i in 0..labels.len() {
                let p = format!("_menu_mc.btn{i}");
                let on = i == sel;
                fc.tween(&p, PropsTo::default().x(if on { HOME_OVER } else { 0.0 }), 0.25, if on { Ease::BackOut } else { Ease::StrongOut });
                if on || before == Some(i) {
                    fc.goto(&p, if on { "over" } else { "out" }, true);
                }
            }
        }
        return;
    }
    let Some((ci, expert, locked)) = menu.item_challenge(sel) else { return };
    let Some(def) = data.0.challenges.get(ci).cloned() else { return };
    let best = profile.best.get(&best_key(&def.id, expert)).copied();
    let medals = if expert && def.expert_medals.iter().any(|m| *m > 0) { def.expert_medals } else { def.medals };
    let pts = data.text(B_TEXTS, "t_Pts");
    for (part, mut fc) in &mut parts {
        match part {
            Part::List => {
                // (`playSelectionOn` / `Out`)
                for i in 0..labels.len() {
                    let p = format!("_list_mc.item{i}");
                    let item_locked = menu.item_challenge(i).is_some_and(|c| c.2);
                    if i == sel {
                        fc.tween(&p, PropsTo::default().scale(ITEM_OVER), 0.2, Ease::BackInOut);
                        fc.goto(&p, if item_locked { "over_lock" } else { "over" }, true);
                    } else if before == Some(i) || before.is_none() {
                        fc.tween(&p, PropsTo::default().scale(1.0), 0.15, Ease::StrongOut);
                        if before == Some(i) {
                            fc.goto(&p, if item_locked { "out_lock" } else { "out" }, true);
                        }
                    }
                }
                // the details (`SetDetails`, `SetTextInfos`, `Open`)
                let d = "_details_mc";
                fc.set_text(&format!("{d}._name_mc.txt"), def.name.to_uppercase());
                fc.wrap.insert(format!("{d}._name_mc.txt"), false);
                let mut words = String::new();
                if expert && !def.expert_description.is_empty() {
                    words.push_str(&format!("<font color=\"#C35128\">{}{}</font><br>", data.text(CM_TEXTS, "t_ExpertModeTitle"), def.expert_description));
                }
                words.push_str(&def.description);
                fc.set_text(&format!("{d}._desc_mc.txt"), words);
                fc.goto_frame(&format!("{d}._icon_mc"), kind_of(&def.kind), false);
                let large = ui.file(&mut images, "dlc05", &format!("ChallengeImg_{}_Large", def.id));
                fc.load_image(&format!("{d}._thumb_mc"), large);
                // locked: dimmed, the padlock and what opens it; open: the stars' scores
                let dim = if locked { 0.45 } else { 1.0 };
                for p in ["_frame_mc", "_name_mc", "_thumb_mc", "_icon_mc"] {
                    fc.set(&format!("{d}.{p}"), PropsTo::default().alpha(dim));
                }
                fc.set_visible(&format!("{d}._lock_mc"), locked);
                fc.set_visible(&format!("{d}._hintScore_mc"), !locked);
                if locked {
                    let raw = data.0.texts.get(&format!("{CM_TEXTS}.t_ChallengeUnlockCondition")).cloned().unwrap_or_default();
                    let grey = "<font color=\"#A8B49E\">";
                    let t = raw.replace("§PTS_COUNT§", &format!("{grey}{} {pts}</font>", def.medals[1])).replace("§C_WHITE§", grey).replace("§C§", "</font>");
                    fc.set_text(&format!("{d}._lock_mc._txt_mc.txt"), t);
                    for p in ["_padlock_mc", "_padlockStroke_mc"] {
                        let path = format!("{d}._lock_mc.{p}");
                        if let Some(sp) = fc.props(&path) {
                            let back: PropsTo = sp.into();
                            fc.set(&path, PropsTo::default().alpha(0.0).xscale(2.0).yscale(2.5).rotation(20.0));
                            fc.tween(&path, back.alpha(1.0).xscale(1.0).yscale(1.0).rotation(0.0), 0.25, Ease::BackInOut);
                        }
                    }
                } else {
                    // (`SetHintScore`: line i its threshold and i + 1 stars)
                    for (l, m) in medals.iter().enumerate() {
                        let line = format!("{d}._hintScore_mc._line{l}_mc");
                        fc.set_text(&format!("{line}.txt"), format!("{m} {pts}"));
                        for k in 0..3 {
                            fc.goto(&format!("{line}.mc{k}"), if k <= l { "on" } else { "off" }, false);
                        }
                    }
                }
                // (the parts sliding in)
                for (p, from) in [("_name_mc", PropsTo::default().alpha(0.0)), ("_desc_mc", PropsTo::default().alpha(0.0)), ("_icon_mc", PropsTo::default().alpha(0.0).xscale(2.0).yscale(3.5))] {
                    let path = format!("{d}.{p}");
                    fc.tween_end(&path, true);
                    if let Some(sp) = fc.props(&path) {
                        let back: PropsTo = sp.into();
                        fc.set(&path, from);
                        fc.tween(&path, back.xscale(1.0).yscale(1.0), 0.25, Ease::StrongOut);
                    }
                }
            }
            Part::Medal => {
                // (`SetChallengeMedal`: the best and its stars; none, none)
                match best.filter(|_| !locked) {
                    Some(b) => {
                        let won = medals.iter().filter(|m| **m > 0 && b >= **m as i64).count();
                        show_medal(&mut fc, b, won, &pts);
                    }
                    None => fc.set_visible("", false),
                }
            }
            _ => {}
        }
    }
}

/// The tabs' line on a leaderboard (`_tabs_mc._y`).
const BOARD_TABS_Y: f32 = -265.0;

/// A clip within's rectangle on the stage (from the clip's origin): its middle and size.
fn stage_rect(fc: &FlashClip, path: &str) -> Option<(Vec2, Vec2)> {
    let m = fc.clip.path_matrix(path)?;
    let (a, b) = fc.clip.child(path).and_then(|c| c.bounds(&fc.tl)).or_else(|| {
        // (a shape, not a clip: its own bounds)
        let (parent, last) = path.rsplit_once('.').unwrap_or(("", path));
        let c = if parent.is_empty() { Some(&fc.clip) } else { fc.clip.child(parent) }?;
        let r = fc.tl.bounds.get(&c.placed_id(last)?)?;
        Some((Vec2::new(r[0], r[1]), Vec2::new(r[2], r[3])))
    })?;
    let pts = [Vec2::new(a.x, a.y), Vec2::new(b.x, a.y), Vec2::new(a.x, b.y), Vec2::new(b.x, b.y)].map(|p| Vec2::new(m[0] * p.x + m[2] * p.y + m[4], m[1] * p.x + m[3] * p.y + m[5]));
    let (lo, hi) = pts.iter().fold((pts[0], pts[0]), |(l, h), p| (l.min(*p), h.max(*p)));
    Some(((lo + hi) * 0.5, hi - lo))
}

/// `Leaderboards_Screen.SetDetails`, `FillLeaderboard`, `Leaderboards_List.SetList`: the
/// challenge's picture, kind and place among them, its rows (ranked, the player's name and
/// when, the score; the last run marked, `indic_mc`), the page's other rows empty
/// (`ghostItem`s, `disabled`), or nothing to show (`Leaderboards_List_empty`); in as they
/// open (`ShowDetails`, `PlayShowAnimation`).
#[allow(clippy::too_many_arguments)]
fn fill_board(fc: &mut FlashClip, def: &dhcook::format::ChallengeDef, (at, of): (usize, usize), runs: &[crate::challenge::Run], last: Option<usize>, data: &crate::gamedata::Data, ui: &mut crate::ui_images::UiImages, images: &mut Assets<Image>) {
    let d = "_details_mc";
    let name = format!("{d}._name_mc.txt");
    fc.set_text(&name, format!("{}/{} - {}", at + 1, of, def.name).to_uppercase());
    fc.wrap.insert(name.clone(), false);
    fc.autosize.insert(name, 0);
    fc.goto_frame(&format!("{d}._icon_mc"), kind_of(&def.kind), false);
    let large = ui.file(images, "dlc05", &format!("ChallengeImg_{}_Large", def.id));
    fc.load_image(&format!("{d}._thumb_mc"), large);
    // (`Leaderboards_Details.Open`)
    for (p, from, secs, ease) in [
        ("_icon_mc", PropsTo::default().alpha(0.0).xscale(2.0).yscale(3.5), 0.25, Ease::BackInOut),
        ("_frame_mc", PropsTo::default().alpha(0.0).xscale(2.5).yscale(2.0), 0.25, Ease::BackInOut),
        ("_name_mc", PropsTo::default().alpha(0.0), 0.2, Ease::StrongOut),
        ("_thumb_mc", PropsTo::default().alpha(0.0), 0.25, Ease::StrongInOut),
    ] {
        let path = format!("{d}.{p}");
        fc.tween_end(&path, true);
        let Some(sp) = fc.props(&path) else { continue };
        let from = match p {
            "_name_mc" => from.x(sp.x - 25.0).y(sp.y + 250.0),
            "_thumb_mc" => from.x(sp.x + 5.0).y(sp.y - 150.0),
            _ => from,
        };
        fc.set(&path, from);
        fc.tween(&path, sp.into(), secs, ease);
    }
    // the rows
    for k in 0..crate::challenge::BOARD_RUNS {
        fc.remove(&format!("_list_mc._rows_mc.row{k}"));
    }
    fc.remove("_list_mc.empty");
    let view = fc.size("_list_mc.maskContent").unwrap_or(Vec2::new(560.0, 440.0));
    let view_at = fc.props("_list_mc.maskContent").map_or(Vec2::ZERO, |p| Vec2::new(p.x, p.y));
    if runs.is_empty() {
        fc.attach("_list_mc", "Leaderboards_List_empty", "empty", IDENTITY);
        fc.set("_list_mc.empty", PropsTo::default().x(view_at.x - 20.0 + 0.5 * view.x).y(view_at.y + 0.5 * view.y).rotation(-2.5));
        fc.set_text("_list_mc.empty.txt", data.text(B_TEXTS, "t_EmptyList"));
    } else {
        let who = std::env::var("USERNAME").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "Corvo".into());
        let pts = data.text(B_TEXTS, "t_Pts");
        let mut h = 48.0;
        let (mut f1, mut f2) = (1, 1);
        for k in 0..crate::challenge::BOARD_RUNS {
            let p = format!("_list_mc._rows_mc.row{k}");
            fc.attach("_list_mc._rows_mc", "Leaderboards_List_item", &format!("row{k}"), IDENTITY);
            if k == 0 {
                h = fc.size(&format!("{p}.btn")).map_or(48.0, |v| v.y).clamp(20.0, 80.0);
            }
            fc.set_visible(&format!("{p}.btn"), false);
            fc.set(&p, PropsTo::default().x(0.0).y(k as f32 * h + 0.5 * h));
            // (the backdrops' frames one after another, row by row)
            fc.goto_frame(&format!("{p}._bkgd_mc.mc"), f1, false);
            fc.goto_frame(&format!("{p}._bkgd_mc"), f2, false);
            f1 = if f1 < fc.total_frames(&format!("{p}._bkgd_mc.mc")) { f1 + 1 } else { 1 };
            f2 = if f2 < fc.total_frames(&format!("{p}._bkgd_mc")) { f2 + 1 } else { 1 };
            let mine = last == Some(k);
            fc.set_visible(&format!("{p}.indic_mc"), mine);
            if mine {
                fc.goto(&format!("{p}.indic_mc"), "loop", true);
            }
            match runs.get(k) {
                Some(r) => {
                    let when = if r.at > 0 { format!("  -  {}", crate::save::ago(r.at)) } else { String::new() };
                    let bright = |t: String| if mine { format!("<font color=\"#E3F2D6\">{t}</font>") } else { t };
                    for t in ["txt_mc", "txtShad_mc"] {
                        fc.set_text(&format!("{p}.{t}.txt_rank"), (k + 1).to_string());
                        fc.set_text(&format!("{p}.{t}.txt_name"), bright(format!("{who}{when}")));
                        fc.set_text(&format!("{p}.{t}.txt_score"), bright(format!("{} {pts}", r.score)));
                        for f in ["txt_rank", "txt_name", "txt_score"] {
                            fc.wrap.insert(format!("{p}.{t}.{f}"), false);
                        }
                    }
                }
                None => {
                    for t in ["txt_mc", "txtShad_mc"] {
                        for f in ["txt_rank", "txt_name", "txt_score"] {
                            fc.set_text(&format!("{p}.{t}.{f}"), "");
                        }
                    }
                    fc.goto(&p, "disabled", false);
                }
            }
        }
    }
    // (`PlayShowAnimation`: the list in from the left)
    if let Some(sp) = fc.props("_list_mc") {
        fc.tween_end("_list_mc", true);
        let back = fc.props("_list_mc").map(|p| p.x).unwrap_or(sp.x);
        fc.set("_list_mc", PropsTo::default().alpha(0.0).x(back - 150.0));
        fc.tween("_list_mc", PropsTo::default().alpha(1.0).x(back), 0.25, Ease::StrongOut);
    }
}

/// A leaderboard's arrows clicked: the challenge before or after.
fn board_arrows(mut menu: ResMut<Menu>, buttons: Query<(&Interaction, &BoardArrow), Changed<Interaction>>) {
    for (i, a) in &buttons {
        if *i == Interaction::Pressed {
            menu.board_step(a.0);
        }
    }
}

/// `DLC05_CM_Medal.SetChallengeMedal`: a score and its stars won, the score and its unit
/// centred under them.
fn show_medal(fc: &mut FlashClip, score: i64, won: usize, pts: &str) {
    fc.set_visible("", true);
    fc.set_text("_score_mc.txt_mc.txt_score", score.to_string());
    fc.set_text("_score_mc.txt_mc.txt_pts", pts.to_string());
    fc.autosize.insert("_score_mc.txt_mc.txt_score".into(), 0);
    fc.autosize.insert("_score_mc.txt_mc.txt_pts".into(), 0);
    let sw = fc.text_width("_score_mc.txt_mc.txt_score").unwrap_or(80.0);
    let pw = fc.text_width("_score_mc.txt_mc.txt_pts").unwrap_or(40.0);
    let sx = fc.props("_score_mc.txt_mc.txt_score").map_or(2.0, |p| p.x);
    fc.set("_score_mc.txt_mc.txt_pts", PropsTo::default().x(sx + sw + 2.5));
    fc.set("_score_mc.txt_mc", PropsTo::default().x(-0.5 * (sw + 2.5 + pw)));
    for k in 0..3 {
        fc.goto(&format!("_stars_mc.mc{k}"), if k < won { "on" } else { "off" }, false);
    }
}

/// The welcome's words scrolled (the wheel, the arrows: `AnalogScrollView`), its medal kept
/// with them and cut to their mask, its button lit under the pointer.
#[allow(clippy::type_complexity)]
fn welcome(
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    time: Res<Time<Real>>,
    mut screens: Query<(&mut Welcome, &mut FlashClip, &Node), (Without<WelcomeMedal>, Without<WelcomeCut>)>,
    mut medals: Query<(&mut FlashClip, &mut Node), (With<WelcomeMedal>, Without<Welcome>, Without<WelcomeCut>)>,
    mut cuts: Query<&mut Node, (With<WelcomeCut>, Without<Welcome>, Without<WelcomeMedal>)>,
    button: Query<&Interaction, (With<WelcomeButton>, Changed<Interaction>)>,
) {
    let lines: f32 = wheel.read().map(|w| if w.unit == bevy::input::mouse::MouseScrollUnit::Line { w.y } else { w.y / 40.0 }).sum();
    let Ok((mut w, mut fc, node)) = screens.single_mut() else { return };
    let dt = time.delta_secs();
    let mut step = -lines * 60.0;
    if keys.pressed(KeyCode::ArrowDown) || keys.pressed(KeyCode::KeyS) {
        step += 500.0 * dt;
    }
    if keys.pressed(KeyCode::ArrowUp) || keys.pressed(KeyCode::KeyW) {
        step -= 500.0 * dt;
    }
    if keys.just_pressed(KeyCode::PageDown) {
        step += 300.0;
    }
    if keys.just_pressed(KeyCode::PageUp) {
        step -= 300.0;
    }
    let to = (w.scroll + step).clamp(0.0, w.max);
    if to != w.scroll {
        w.scroll = to;
        fc.set("_content_mc.txt_mc", PropsTo::default().y(-to));
    }
    for i in &button {
        match i {
            Interaction::Hovered => {
                fc.goto("_btn_mc", "over", true);
            }
            Interaction::None => {
                fc.goto("_btn_mc", "out", true);
            }
            _ => {}
        }
    }
    // the medal where its place in the words is, cut to their mask
    let (Ok((mut mc, mut mn)), Ok(mut cn)) = (medals.single_mut(), cuts.single_mut()) else { return };
    let (Val::Px(ox), Val::Px(oy)) = (node.left, node.top) else { return };
    let s = fc.scale;
    let (Some(m), Some(mm), Some((a, b))) = (fc.clip.path_matrix("_content_mc.txt_mc.medal_mc.mc"), fc.clip.path_matrix("_content_mc.mask_mc"), fc.clip.child("_content_mc.mask_mc").and_then(|c| c.bounds(&fc.tl))) else { return };
    // (the mask's own rectangle about its middle, square to the screen, as the words are cut)
    let mid = (a + b) * 0.5;
    let (cx, cy) = ((mm[0] * mid.x + mm[2] * mid.y + mm[4]) * s, (mm[1] * mid.x + mm[3] * mid.y + mm[5]) * s);
    let (wd, ht) = ((mm[0] * mm[0] + mm[1] * mm[1]).sqrt() * (b.x - a.x).abs() * s, (mm[2] * mm[2] + mm[3] * mm[3]).sqrt() * (b.y - a.y).abs() * s);
    let (l, t) = (ox + cx - wd * 0.5, oy + cy - ht * 0.5);
    if cn.left != Val::Px(l) || cn.top != Val::Px(t) || cn.width != Val::Px(wd) || cn.height != Val::Px(ht) {
        (cn.left, cn.top, cn.width, cn.height) = (Val::Px(l), Val::Px(t), Val::Px(wd), Val::Px(ht));
    }
    let (ml, mt) = (Val::Px(ox - l), Val::Px(oy - t));
    if mn.left != ml || mn.top != mt {
        (mn.left, mn.top) = (ml, mt);
    }
    mc.scale = s;
    mc.m = m;
    mc.alpha = fc.alpha * fc.props("_content_mc").map_or(1.0, |p| p.alpha);
}

/// A challenge kind's icon frame (`_DLC05.eChallengeType`).
fn kind_of(kind: &str) -> usize {
    match kind {
        "DDCT_Mobility" => 1,
        "DDCT_Stealth" => 2,
        "DDCT_Action" => 3,
        "DDCT_Puzzle" => 4,
        _ => 1,
    }
}

/// The large piece's arrows clicked: the previous or next unlocked.
fn arrows(mut menu: ResMut<Menu>, buttons: Query<(&Interaction, &GalleryArrow), Changed<Interaction>>) {
    for (i, a) in &buttons {
        if *i == Interaction::Pressed {
            menu.gallery_step(a.0);
        }
    }
}

/// A mode's tab clicked: the other list.
fn tabs(mut menu: ResMut<Menu>, buttons: Query<(&Interaction, &TabButton), Changed<Interaction>>) {
    for (i, t) in &buttons {
        if *i == Interaction::Pressed {
            menu.switch_trials_tab(t.0);
        }
    }
}
