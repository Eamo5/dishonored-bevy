//! The story packs' own front ends (The Knife of Dunwall's `UI_MainMenu_DLC06.MainMenu`, The
//! Brigmore Witches' `UI_MainMenu_DLC07.MainMenu`, cooked as `MainMenu_DLC06` and `_DLC07`), over
//! their own menu maps (`DLC06_Dishonored_MainMenu`, `L_DLC07_MainMenu`), as their classes show
//! the home screen (`MM_Home_Screen`): the blades, circles and texture, the pack's logo on its
//! brush (`_logo_mc`), Daud's portrait (`_daud_mc`) and the menu (`_menu_mc`, turned with the
//! screen): `MM_Home_Menu_item`s down from its top 5 apart, the chosen one along 30
//! (`_btnOverProps`, as the trials' home has it) playing its `over`.
//! The menu's items and their choosing are `menu.rs`'s; this draws them. Its 3D scene follows
//! the menu (`menu::dlc_scene`).

use crate::flash::{Clip, Ease, FlashClip, MovieTimelines, PropsTo, IDENTITY};
use crate::menu::{Menu, MenuItem, DLC_PACKS};
use crate::GameState;
use bevy::prelude::*;

pub struct DlcMenuPlugin;

impl Plugin for DlcMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (draw, arrows, infos).chain().after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

/// The home's code among the front pages (`Menu::front_page`), and the new game's.
pub const CODE: u8 = 40;
pub const NEW_GAME: u8 = 41;
/// the new game's dots apart (`MM_NewGame_Menu`: our reading), the render target's movie and
/// its size (`DifficultyInfos`, `DLC06_MainMenu_MS.DifficultyUI_RenderText`)
const DOT_GAP: f32 = 62.0;
const INFOS_MOVIE: [&str; 2] = ["DifficultyInfos_DLC06", "DifficultyInfos_DLC07"];
/// the chosen item's move along (`_btnOverProps`), the items' gap
const OVER: f32 = 30.0;
const GAP: f32 = 5.0;

#[derive(Component)]
struct DlcRoot;

#[derive(Component)]
struct Home;

#[derive(Default)]
struct Shown {
    page: Option<(u8, Vec<String>)>,
    pack: Option<usize>,
    sel: Option<usize>,
    size: UVec2,
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    menu: Res<Menu>,
    roots: Query<Entity, With<DlcRoot>>,
    mut homes: Query<&mut FlashClip, With<Home>>,
    window: Query<&Window>,
    mut timelines: ResMut<MovieTimelines>,
    mut shown: Local<Shown>,
) {
    let Ok(w) = window.single() else { return };
    let size = UVec2::new(w.width() as u32, w.height() as u32);
    let page = menu.front_page().filter(|p| p.0 == CODE || p.0 == NEW_GAME);
    let pack = menu.dlc();
    if page != shown.page || size != shown.size || pack != shown.pack {
        shown.page = page.clone();
        shown.size = size;
        shown.pack = pack;
        shown.sel = None;
        for e in &roots {
            commands.entity(e).despawn();
        }
        let (Some((code, labels)), Some(pack)) = (page, pack) else { return };
        let movie = DLC_PACKS[pack].movie;
        let Some(tl) = timelines.get(movie) else { return };
        let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
        let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
        let root = commands.spawn((DlcRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(20), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
        let at = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
        if let Some(c) = Clip::export(&tl, "m_Vignette") {
            let mut fc = FlashClip::new(movie, tl.clone(), c).real();
            fc.scale = s;
            commands.spawn((fc, at(Vec2::new(640.0, 360.0)), ZIndex(3), Pickable::IGNORE, ChildOf(root)));
        }
        if code == NEW_GAME {
            new_game(&mut commands, root, &tl, movie, &labels, s, off);
            return;
        }
        let Some(c) = Clip::export(&tl, "MM_Home_Screen") else { return };
        let mut fc = FlashClip::new(movie, tl.clone(), c).real().with_texts();
        fc.scale = s;
        let mut h = 40.0;
        let mut bw = 330.0;
        for (i, l) in labels.iter().enumerate() {
            fc.attach("_menu_mc", "MM_Home_Menu_item", &format!("btn{i}"), IDENTITY);
            let p = format!("_menu_mc.btn{i}");
            if i == 0 {
                h = fc.size(&p).map_or(40.0, |v| v.y).clamp(20.0, 80.0);
                bw = fc.size(&format!("{p}.btn")).map_or(330.0, |v| v.x).clamp(100.0, 600.0);
            }
            fc.set_visible(&format!("{p}.btn"), false);
            fc.set_text(&format!("{p}._txt_mc.txt"), l.to_uppercase());
            fc.wrap.insert(format!("{p}._txt_mc.txt"), false);
        }
        // the items from the menu's top down, each its height and the gap apart, the menu turned
        // with the screen (`_menu_mc`'s matrix)
        let m = fc.props("_menu_mc");
        let menu_at = m.as_ref().map_or(Vec2::new(-327.5, -53.25), |p| Vec2::new(p.x, p.y));
        let k = m.as_ref().map_or(1.0, |p| p.xscale);
        let turn = m.as_ref().map_or(-0.1225, |p| p.rotation.to_radians());
        let rot = Vec2::from_angle(turn);
        for i in 0..labels.len() {
            let y = i as f32 * (h + GAP) + 0.5 * h;
            fc.set(&format!("_menu_mc.btn{i}"), PropsTo::default().y(y).x(0.0));
            let local = Vec2::new(bw * 0.5 - 130.0 * 0.62, y) * k;
            let c = Vec2::new(640.0, 360.0) + menu_at + rot.rotate(local);
            let p = off + (c - Vec2::new(bw, h) * k * 0.5) * s;
            commands.spawn((Button, MenuItem(i), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(bw * k * s), height: Val::Px(h * k * s), ..default() }, ZIndex(5), ChildOf(root)));
        }
        commands.spawn((Home, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
        return;
    }
    if shown.page.is_none() {
        return;
    }
    let sel = menu.selected();
    if shown.sel == Some(sel) {
        return;
    }
    let before = shown.sel;
    shown.sel = Some(sel);
    let n = shown.page.as_ref().map_or(0, |p| p.1.len());
    if shown.page.as_ref().is_some_and(|p| p.0 == NEW_GAME) {
        // (the chosen difficulty's dot turning, the others still)
        for mut fc in &mut homes {
            for i in 0..n {
                let p = format!("_menu_mc.dot{i}");
                let tl = fc.tl.clone();
                if let Some(c) = fc.clip.child_mut(&p) {
                    c.goto(&tl, 0, i == sel);
                }
                if let Some(c) = fc.clip.child_mut(&format!("{p}._bkgd_mc")) {
                    c.goto(&tl, (i == sel) as usize, false);
                }
                fc.tween(&p, PropsTo::default().xscale(if i == sel { 1.25 } else { 1.0 }).yscale(if i == sel { 1.25 } else { 1.0 }), 0.2, Ease::StrongOut);
            }
        }
        return;
    }
    for mut fc in &mut homes {
        for i in 0..n {
            let p = format!("_menu_mc.btn{i}");
            let on = i == sel;
            fc.tween(&p, PropsTo::default().x(if on { OVER } else { 0.0 }), 0.25, if on { Ease::BackOut } else { Ease::StrongOut });
            if on || before == Some(i) {
                fc.goto(&p, if on { "over" } else { "out" }, true);
            }
        }
    }
}

/// A story pack's new game's arrows (`_arrL_mc`, `_arrR_mc`): the difficulty before, after.
#[derive(Component)]
struct DlcArrow(i32);

/// The new game's screen (`MM_NewGame_Screen`): its brush across the bottom (`_menu_mc`), the
/// difficulties' dots on it (`MM_NewGame_Menu_item`, side by side), its arrows either side; their
/// names and words are on the card the menu map shows (`infos`).
fn new_game(commands: &mut Commands, root: Entity, tl: &std::sync::Arc<dhcook::format::Timelines>, movie: &'static str, labels: &[String], s: f32, off: Vec2) {
    let Some(c) = Clip::export(tl, "MM_NewGame_Screen") else { return };
    let mut fc = FlashClip::new(movie, tl.clone(), c).real().with_texts();
    fc.scale = s;
    let menu_at = fc.props("_menu_mc").map_or(Vec2::new(-270.0, 285.1), |p| Vec2::new(p.x, p.y));
    let n = labels.len();
    for i in 0..n {
        let x = (i as f32 - (n as f32 - 1.0) * 0.5) * DOT_GAP;
        fc.attach("_menu_mc", "MM_NewGame_Menu_item", &format!("dot{i}"), IDENTITY);
        let p = format!("_menu_mc.dot{i}");
        fc.set(&p, PropsTo::default().x(x).y(0.0));
        fc.set_visible(&format!("{p}.btn"), false);
        let c = off + (Vec2::new(640.0, 360.0) + menu_at + Vec2::new(x, 0.0)) * s;
        let size = Vec2::splat(DOT_GAP * 0.9) * s;
        commands.spawn((Button, MenuItem(i), Node { position_type: PositionType::Absolute, left: Val::Px(c.x - size.x * 0.5), top: Val::Px(c.y - size.y * 0.5), width: Val::Px(size.x), height: Val::Px(size.y), ..default() }, ZIndex(5), ChildOf(root)));
    }
    for (dir, p) in [(-1, "_arrL_mc"), (1, "_arrR_mc")] {
        fc.set_visible(&format!("{p}.btn"), false);
        let at = fc.props(p).map_or(Vec2::new(dir as f32 * 208.0, 0.0), |q| Vec2::new(q.x, q.y));
        let c = off + (Vec2::new(640.0, 360.0) + menu_at + at) * s;
        let size = Vec2::new(70.0, 90.0) * s;
        commands.spawn((Button, DlcArrow(dir), Node { position_type: PositionType::Absolute, left: Val::Px(c.x - size.x * 0.5), top: Val::Px(c.y - size.y * 0.5), width: Val::Px(size.x), height: Val::Px(size.y), ..default() }, ZIndex(5), ChildOf(root)));
    }
    let at = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
    commands.spawn((Home, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
}

/// The new game's arrows clicked: the difficulty before or after.
fn arrows(mut menu: ResMut<Menu>, buttons: Query<(&Interaction, &DlcArrow), Changed<Interaction>>) {
    for (i, a) in &buttons {
        if *i == Interaction::Pressed {
            menu.step_sel(a.0);
        }
    }
}

/// The card the menu map shows by the chosen difficulty's figure (`DiificultyUI_RenderMat`): the
/// pack's `DifficultyInfos` movie drawn into its render target (`DifficultyUI_RenderText`, as the
/// menu's `DisGFxActionOpenMovie` with its `RenderTexture` has it), the difficulty's name and
/// words (`t_difficulty<n>_DLC06`, its `Desc`; the last, locked: what opens it).
#[derive(Component)]
struct InfosCard;

#[allow(clippy::too_many_arguments)]
fn infos(
    mut commands: Commands,
    menu: Res<Menu>,
    level: Option<Res<crate::level::LevelInfo>>,
    targets: Option<Res<crate::reflections::ReflectionTargets>>,
    mut cards: Query<(Entity, &mut FlashClip), With<InfosCard>>,
    mut timelines: ResMut<MovieTimelines>,
    data: Res<crate::gamedata::Data>,
    mut shown: Local<Option<(usize, usize)>>,
    mut parts: Local<Vec<Entity>>,
) {
    let want = menu.front_page().filter(|p| p.0 == NEW_GAME).and(menu.dlc()).map(|pack| (pack, menu.selected()));
    if want == *shown {
        return;
    }
    let had = shown.map(|s| s.0);
    *shown = want;
    let Some((pack, sel)) = want else {
        for e in parts.drain(..) {
            commands.entity(e).try_despawn();
        }
        return;
    };
    let base = "DisGFxMoviePlayerMainMenu_Texts";
    let title = data.text(base, &format!("t_difficulty{sel}_DLC06")).to_uppercase();
    let words = if menu.item_locked(sel) { data.text("DisDLC06MoviePlayerMainMenu_Texts", "t_LastDifficultyUnlockCondition") } else { data.text(base, &format!("t_difficulty{sel}_DLC06Desc")) };
    let fill = |fc: &mut FlashClip| {
        for p in ["_infos_mc.title_mc.txt", "_infos_mc.title_mc.txtShad"] {
            fc.set_text(p, title.clone());
        }
        for p in ["_infos_mc.desc_mc.txt", "_infos_mc.desc_mc.txtShad"] {
            fc.set_text(p, words.clone());
        }
    };
    if had == Some(pack) && !parts.is_empty() {
        for (_, mut fc) in &mut cards {
            fill(&mut fc);
        }
        return;
    }
    for e in parts.drain(..) {
        commands.entity(e).try_despawn();
    }
    let (Some(level), Some(targets)) = (level, targets) else { return };
    let Some((image, size)) = level.scene.textures.iter().enumerate().find(|(_, t)| t.name.contains("difficultyui_rendertext")).and_then(|(i, t)| targets.0.get(&(i as u32)).map(|h| (h.clone(), t.render_target.map_or([1024.0, 512.0], |r| [r[0], r[1]])))) else { return };
    let Some(tl) = timelines.get(INFOS_MOVIE[pack]) else { return };
    let Some(c) = Clip::export(&tl, "DI_Infos") else { return };
    let cam = commands
        .spawn((
            Camera2d,
            Camera { order: -85, clear_color: ClearColorConfig::Custom(Color::NONE), ..default() },
            bevy::camera::RenderTarget::Image(image.into()),
            Msaa::Off,
            DespawnOnExit(GameState::InGame),
        ))
        .id();
    let root = commands.spawn((Node { width: Val::Px(size[0]), height: Val::Px(size[1]), ..default() }, bevy::ui::UiTargetCamera(cam), DespawnOnExit(GameState::InGame))).id();
    let mut fc = FlashClip::new(INFOS_MOVIE[pack], tl.clone(), c).real().with_texts();
    fc.scale = 1.0;
    fill(&mut fc);
    commands.spawn((InfosCard, fc, Node { position_type: PositionType::Absolute, left: Val::Px(0.0), top: Val::Px(0.0), ..default() }, ChildOf(root)));
    parts.extend([cam, root]);
}
