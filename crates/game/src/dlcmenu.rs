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
        app.add_systems(Update, draw.after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

/// The home's code among the front pages (`Menu::front_page`).
pub const CODE: u8 = 40;
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
    let page = menu.front_page().filter(|p| p.0 == CODE);
    let pack = menu.dlc();
    if page != shown.page || size != shown.size || pack != shown.pack {
        shown.page = page.clone();
        shown.size = size;
        shown.pack = pack;
        shown.sel = None;
        for e in &roots {
            commands.entity(e).despawn();
        }
        let (Some((_, labels)), Some(pack)) = (page, pack) else { return };
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
