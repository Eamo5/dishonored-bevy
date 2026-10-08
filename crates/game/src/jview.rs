//! The journal drawn from the original `UI_Journal` movie (`journal` keeps its state and input):
//! the pause screens' drifting backdrop (`p_bkgd`), the tab bar's band (`Journal`), the main tabs
//! (lib `lib_mTabs_tab_s`, 196 apart along y 100, the chosen one light and lifted), the pages'
//! own panels (`j_obj_mc`, `j_logs_mc`...), their sub tabs (`j_SubTabsBkgd`, labels left-aligned
//! from page x -472.75, 25 apart), lists (`j_obj_objectivesList_list`, `j_logs_logsList`, the
//! inventory's 4 x 3 cards...), and details panels (`j_obj_chapterDetails`, `j_itemDetails`,
//! `j_pow_powerDetails`), with the texts the movie's code writes into them drawn as text over
//! the clips, laid out as `JournalScreen`, `TabsHandler`, `SubTabsHandler`, `j_ItemsListScreen`
//! and the page classes do (page origin: the stage's middle).

use crate::flash::{concat, Clip, FlashClip, Mat};
use crate::gamedata::Data;
use crate::gameplay::PlayerStats;
use crate::ui_images::UiImages;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource, LineBreak};
use dhcook::format::Timelines;
use std::sync::Arc;

pub struct JViewPlugin;

impl Plugin for JViewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, (sel_corners, keep_selected_in_view, wheel_scroll, animate).before(crate::flash::PlayClips).run_if(in_state(crate::GameState::InGame)));
    }
}

/// The page coming in (`j_ScreenBase.SetContent`): from `from` (stage units) and transparent
/// to its place over `dur` (Strong.easeOut); `t` runs on through rebuilds.
#[derive(Component, Clone, Copy)]
pub struct JEnter {
    pub t: f32,
    pub dur: f32,
    pub from: Vec2,
}
/// The journal put away (`CloseJournal`): fading and growing to 105% over 0.3 s, then gone.
#[derive(Component)]
pub struct JLeave {
    pub t: f32,
}
/// A drawn thing's own opacity, kept while its group fades.
#[derive(Component)]
struct BaseAlpha(f32);

/// How far a page slides in (`_offsetX`) and how long it takes (`SetContent`'s 0.25 s); the
/// journal's opening fade (`Open`: 0.3 s) and its closing (`_animCloseDuration`).
pub const SLIDE: f32 = 250.0;
pub const SLIDE_TIME: f32 = 0.25;
pub const OPEN_TIME: f32 = 0.3;
pub const CLOSE_TIME: f32 = 0.3;

fn strong_out(k: f32) -> f32 {
    1.0 - (1.0 - k.clamp(0.0, 1.0)).powi(5)
}

/// The journal's entrances and exit: the page's slide and fade, the closing fade and growth.
#[allow(clippy::type_complexity)]
fn animate(
    mut commands: Commands,
    time: Res<Time<bevy::time::Real>>,
    mut entering: Query<(Entity, &mut JEnter, &mut UiTransform), Without<JLeave>>,
    mut leaving: Query<(Entity, &mut JLeave, &mut UiTransform), Without<JEnter>>,
    children: Query<&Children>,
    (mut fcs, mut texts, mut imgs, bases): (Query<&mut FlashClip>, Query<&mut TextColor>, Query<&mut ImageNode>, Query<&BaseAlpha>),
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
) {
    let dt = time.delta_secs().min(0.05);
    let s = window.single().map(|w| (w.width() / 1280.0).min(w.height() / 720.0)).unwrap_or(1.0);
    let mut fade = |commands: &mut Commands, e: Entity, a: f32| {
        for d in std::iter::once(e).chain(children.iter_descendants(e)) {
            let base = |v: f32| bases.get(d).map(|b| b.0).unwrap_or(v);
            let mut set = None;
            if let Ok(mut fc) = fcs.get_mut(d) {
                let b = base(fc.alpha);
                fc.alpha = b * a;
                set = Some(b);
            } else if let Ok(mut t) = texts.get_mut(d) {
                let b = base(t.0.alpha());
                t.0.set_alpha(b * a);
                set = Some(b);
            } else if let Ok(mut im) = imgs.get_mut(d) {
                let b = base(im.color.alpha());
                im.color.set_alpha(b * a);
                set = Some(b);
            }
            if let (Some(b), false) = (set, bases.contains(d)) {
                commands.entity(d).try_insert(BaseAlpha(b));
            }
        }
    };
    for (e, mut en, mut ut) in &mut entering {
        en.t += dt;
        let k = strong_out(en.t / en.dur);
        let off = en.from * (1.0 - k) * s;
        ut.translation = Val2::px(off.x, off.y);
        fade(&mut commands, e, k);
        if en.t >= en.dur {
            commands.entity(e).remove::<JEnter>();
        }
    }
    for (e, mut lv, mut ut) in &mut leaving {
        lv.t += dt;
        let k = strong_out(lv.t / CLOSE_TIME);
        ut.scale = Vec2::splat(1.0 + 0.05 * k);
        fade(&mut commands, e, 1.0 - k);
        if lv.t >= CLOSE_TIME {
            commands.entity(e).despawn();
        }
    }
}

pub const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
pub const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);
const TAB_GREY: Color = Color::srgb(130.0 / 255.0, 138.0 / 255.0, 123.0 / 255.0);
const ROW: Color = Color::srgb(168.0 / 255.0, 180.0 / 255.0, 158.0 / 255.0);
const LOCKED: Color = Color::srgba(150.0 / 255.0, 161.0 / 255.0, 141.0 / 255.0, 0.8);
const LABEL: Color = Color::srgb(151.0 / 255.0, 163.0 / 255.0, 144.0 / 255.0);
const POWER_NAME: Color = Color::srgb(183.0 / 255.0, 197.0 / 255.0, 172.0 / 255.0);
const CARD_NAME: Color = Color::srgb(133.0 / 255.0, 142.0 / 255.0, 125.0 / 255.0);
const CARD_NAME_ON: Color = Color::srgb(200.0 / 255.0, 204.0 / 255.0, 196.0 / 255.0);
const RED: Color = Color::srgb(140.0 / 255.0, 25.0 / 255.0, 24.0 / 255.0);
const MOVIE: &str = "Journal";
pub const J: &str = "DisGFxMoviePlayerJournal_Texts";
pub const BASE: &str = "DisGFxMoviePlayerBase_Texts";

/// A main tab clicked (the page).
#[derive(Component)]
pub struct JTab(pub u8);
/// A sub tab clicked.
#[derive(Component)]
pub struct JSub(pub u8);
/// A list's row, card or slot (pointed at: chosen).
#[derive(Component)]
pub struct JRow(pub usize);
/// The page's action (buy the power, play the audiograph, wear the charm).
#[derive(Component)]
pub struct JAct;
/// A list scrolled to keep the chosen row in view.
#[derive(Component)]
struct JScroll;
/// The chosen row of a scrolled list.
#[derive(Component)]
struct JSelRow;
/// The objectives list's selection brackets (`j_objList_itemSelection`), fitted to their row
/// (its width on the stage).
#[derive(Component)]
struct SelCorners(f32);

/// Where the 1280 x 720 stage is on the screen.
#[derive(Clone, Copy)]
pub struct Stage {
    pub s: f32,
    pub off: Vec2,
}

impl Stage {
    pub fn of(w: &Window) -> Stage {
        let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
        Stage { s, off: (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5 }
    }
}

/// A page point (origin at the stage's middle) on the stage.
pub fn pg(x: f32, y: f32) -> Vec2 {
    Vec2::new(640.0 + x, 360.0 + y)
}

fn apply(m: &Mat, p: Vec2) -> Vec2 {
    Vec2::new(m[0] * p.x + m[2] * p.y + m[4], m[1] * p.x + m[3] * p.y + m[5])
}

/// The journal's state the view shows.
pub struct JState<'a> {
    pub page: u8,
    pub obj_tab: u8,
    pub obj_sel: usize,
    pub log_tab: u8,
    pub log_sel: usize,
    pub power_sel: usize,
    pub charm_sel: usize,
    pub inv_tab: u8,
    pub inv_sel: usize,
    pub playing: Option<&'a str>,
}

/// A text's look.
#[derive(Clone)]
struct Look {
    title: bool,
    size: f32,
    color: Color,
    justify: Justify,
    wrap: bool,
    /// the field stretched (`_yscale` 120)
    tall: bool,
}

impl Look {
    fn title(size: f32, color: Color) -> Look {
        Look { title: true, size, color, justify: Justify::Left, wrap: false, tall: true }
    }
    fn normal(size: f32, color: Color) -> Look {
        Look { title: false, size, color, justify: Justify::Left, wrap: true, tall: false }
    }
    fn center(mut self) -> Look {
        self.justify = Justify::Center;
        self
    }
    fn right(mut self) -> Look {
        self.justify = Justify::Right;
        self
    }
    fn justified(mut self) -> Look {
        self.justify = Justify::Justified;
        self
    }
    fn flat(mut self) -> Look {
        self.tall = false;
        self
    }
    fn nowrap(mut self) -> Look {
        self.wrap = false;
        self
    }
}

pub struct Ctx<'a, 'w, 's> {
    pub c: &'a mut Commands<'w, 's>,
    pub root: Entity,
    /// the journal's frame (the root while the page is drawn into its own, sliding, layer)
    pub frame: Entity,
    pub st: Stage,
    pub tl: Arc<Timelines>,
    pub lib: Option<Arc<Timelines>>,
    pub title: FontSource,
    pub ui: &'a mut UiImages,
    pub images: &'a mut Assets<Image>,
}

impl Ctx<'_, '_, '_> {
    fn px(&self, v: f32) -> Val {
        Val::Px(v * self.st.s)
    }

    /// A node's place under the root (screen pixels) or under a container whose top-left is
    /// at `origin` on the stage.
    fn at(&self, parent: Entity, origin: Vec2, p: Vec2) -> Vec2 {
        if parent == self.root || parent == self.frame {
            self.st.off + p * self.st.s
        } else {
            (p - origin) * self.st.s
        }
    }

    /// A symbol of the journal movie (or the lib's) with its origin at a stage point.
    fn sym(&mut self, parent: Entity, origin: Vec2, lib: bool, name: &str, at: Vec2, setup: impl FnOnce(&mut FlashClip)) -> Option<Entity> {
        let tl = if lib { self.lib.clone()? } else { self.tl.clone() };
        let clip = match name.parse::<u16>() {
            Ok(id) => Clip::new(&tl, id),
            Err(_) => Clip::export(&tl, name)?,
        };
        let mut fc = FlashClip::new(if lib { "lib" } else { MOVIE }, tl, clip).real();
        fc.scale = self.st.s;
        setup(&mut fc);
        let p = self.at(parent, origin, at);
        Some(self.c.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(parent))).id())
    }

    fn font(&self, l: &Look) -> TextFont {
        let mut f = TextFont { font_size: FontSize::Px((l.size * self.st.s).max(1.0)), ..default() };
        if l.title {
            f.font = self.title.clone();
        }
        f
    }

    /// A text with its top-left at a stage point (`width`: its field's, wrapping).
    fn text(&mut self, parent: Entity, origin: Vec2, s: impl Into<String>, at: Vec2, width: Option<f32>, l: &Look) -> Entity {
        let p = self.at(parent, origin, at);
        let node = Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: width.map(|w| self.px(w)).unwrap_or(Val::Auto), ..default() };
        let e = self.c.spawn((Text::new(s), self.font(l), TextColor(l.color), TextLayout::new(l.justify, if l.wrap { LineBreak::WordBoundary } else { LineBreak::NoWrap }), node, Pickable::IGNORE, ChildOf(parent))).id();
        if l.tall {
            self.c.entity(e).insert(UiTransform { scale: Vec2::new(1.0, 1.2), ..default() });
        }
        e
    }

    /// A text placed in a box on the stage (aligned in it as `l` says, middled up and down).
    fn text_in(&mut self, parent: Entity, origin: Vec2, s: impl Into<String>, rect: Rect, l: &Look) -> Entity {
        let p = self.at(parent, origin, rect.min);
        let justify = match l.justify {
            Justify::Center => JustifyContent::Center,
            Justify::Right => JustifyContent::FlexEnd,
            _ => JustifyContent::FlexStart,
        };
        let bx = self
            .c
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(p.x),
                    top: Val::Px(p.y),
                    width: self.px(rect.width()),
                    height: self.px(rect.height()),
                    justify_content: justify,
                    align_items: AlignItems::Center,
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(parent),
            ))
            .id();
        let e = self.c.spawn((Text::new(s), self.font(l), TextColor(l.color), TextLayout::new(l.justify, if l.wrap { LineBreak::WordBoundary } else { LineBreak::NoWrap }), Pickable::IGNORE, ChildOf(bx))).id();
        if l.tall {
            self.c.entity(e).insert(UiTransform { scale: Vec2::new(1.0, 1.2), ..default() });
        }
        bx
    }

    /// A box on the stage clipping what is in it (a list's mask), scrolled or not.
    fn clip_box(&mut self, rect: Rect, scroll: bool) -> Entity {
        let root = self.root;
        let p = self.at(root, Vec2::ZERO, rect.min);
        let node = Node {
            position_type: PositionType::Absolute,
            left: Val::Px(p.x),
            top: Val::Px(p.y),
            width: self.px(rect.width()),
            height: self.px(rect.height()),
            flex_direction: FlexDirection::Column,
            overflow: if scroll { Overflow::scroll_y() } else { Overflow::clip() },
            ..default()
        };
        let e = self.c.spawn((node, Pickable::IGNORE, ChildOf(root))).id();
        if scroll {
            self.c.entity(e).insert((ScrollPosition::default(), JScroll));
        }
        e
    }

    /// A plain box (a container, a hit area) on the stage.
    fn boxed(&mut self, parent: Entity, origin: Vec2, rect: Rect) -> Entity {
        let p = self.at(parent, origin, rect.min);
        self.c
            .spawn((Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: self.px(rect.width()), height: self.px(rect.height()), ..default() }, Pickable::IGNORE, ChildOf(parent)))
            .id()
    }

    /// A clickable area.
    fn hit(&mut self, parent: Entity, origin: Vec2, rect: Rect, tag: impl Bundle) -> Entity {
        let e = self.boxed(parent, origin, rect);
        self.c.entity(e).remove::<Pickable>().insert((Button, tag));
        e
    }

    /// A picture fitted into a box centred on a stage point (shrunk only, as `ImgLoader`).
    fn picture(&mut self, parent: Entity, origin: Vec2, img: (Handle<Image>, Vec2), centre: Vec2, bx: Vec2, turn: f32, alpha: f32) -> Entity {
        let (h, size) = img;
        let k = (bx.x / size.x.max(1.0)).min(bx.y / size.y.max(1.0)).min(1.0);
        let (w, hh) = (size.x * k, size.y * k);
        let p = self.at(parent, origin, centre - Vec2::new(w, hh) * 0.5);
        self.c
            .spawn((
                ImageNode::new(h).with_color(Color::WHITE.with_alpha(alpha)),
                Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: self.px(w), height: self.px(hh), ..default() },
                UiTransform { rotation: Rot2::degrees(turn), ..default() },
                Pickable::IGNORE,
                ChildOf(parent),
            ))
            .id()
    }

}

/// The deterministic little turn of an inventory card (`{-2.5 .. 2.5}` degrees).
fn card_turn(i: usize) -> f32 {
    let h = (i as u32).wrapping_mul(2654435761) >> 29;
    [-2.5, -1.5, -0.5, 0.5, 1.5, 2.5][(h % 6) as usize]
}

/// The whole journal: backdrop, tab bar, the page.
#[allow(clippy::too_many_arguments)]
pub fn build(cx: &mut Ctx, js: &JState, data: &Data, stats: &PlayerStats, vm: Option<&crate::kismet::Vm>, map: &str, enter: Option<JEnter>) {
    let root = cx.root;
    // the backdrop (`bkgd_mc`), drifting
    if let Some(e) = cx.sym(root, Vec2::ZERO, false, "p_bkgd", Vec2::new(640.0, 360.0), |_| {}) {
        cx.c.queue(move |w: &mut World| {
            let bg = w.get::<FlashClip>(e).and_then(|fc| crate::animbg::AnimatedBackground::new(fc, ""));
            if let (Some(bg), Ok(mut em)) = (bg, w.get_entity_mut(e)) {
                em.insert(bg);
            }
        });
    }
    // the tab bar's band (`journal_mc`'s `_bkgdTabs_mc`)
    cx.sym(root, Vec2::ZERO, false, "Journal", Vec2::new(640.0, 360.0), |_| {});
    // the page on its own layer, sliding in when it or its sub tab changes
    let page = cx.c.spawn((Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, UiTransform::default(), Pickable::IGNORE, ChildOf(root))).id();
    if let Some(en) = enter {
        cx.c.entity(page).insert(en);
    }
    cx.frame = root;
    cx.root = page;
    match js.page {
        crate::journal::PAGE_MISSION => objectives(cx, js, data, stats, vm, map),
        crate::journal::PAGE_NOTES => logs(cx, js, data, stats),
        crate::journal::PAGE_POWERS => powers(cx, js, data, stats),
        crate::journal::PAGE_CHARMS => charms(cx, js, data, stats),
        _ => inventory(cx, js, data, stats),
    }
    cx.root = root;
    main_tabs(cx, js.page, data);
    // the vignette over all (`vignette_mc`)
    cx.sym(root, Vec2::ZERO, false, "m_Vignette", Vec2::new(640.0, 360.0), |_| {});
}

/// The main tabs (`TabsHandler`: `lib_mTabs_tab_s` 256 wide, 60 overlapping, centred on the
/// stage at y 100; the chosen one `selected`, lifted 20 and drawn 1.25 times nearer).
fn main_tabs(cx: &mut Ctx, page: u8, data: &Data) {
    let root = cx.root;
    let pages = crate::journal::PAGES;
    let n = pages.len();
    let total = 196.0 * (n as f32 - 1.0) + 256.0;
    for (i, p) in pages.iter().enumerate() {
        let x = 640.0 - total * 0.5 + 128.0 + 196.0 * i as f32;
        let on = *p == page;
        let label = match *p {
            crate::journal::PAGE_MISSION => data.text(BASE, "t_Objectives"),
            crate::journal::PAGE_NOTES => data.text(BASE, "t_Logs"),
            crate::journal::PAGE_POWERS => data.text(BASE, "t_Powers"),
            crate::journal::PAGE_CHARMS => data.text(BASE, "t_BoneCharms"),
            _ => data.text(BASE, "t_Inventory"),
        }
        .to_uppercase();
        cx.sym(root, Vec2::ZERO, true, "lib_mTabs_tab_s", Vec2::new(x, 100.0), |fc| {
            let tl = fc.tl.clone();
            // (the update badge: no tab is flagged)
            fc.set_visible("tab_mc.icUpdate_mc", false);
            if on {
                fc.goto("tab_mc", "selected", true);
                fc.goto("tab_mc.glow_mc", "loop", true);
                if let Some((m, _)) = fc.clip.placed_mut("tab_mc") {
                    *m = [1.25, 0.0, 0.0, 1.25, 0.0, -20.0];
                }
            } else if let Some(c) = fc.clip.child_mut("tab_mc") {
                c.goto(&tl, 0, false);
            }
        });
        // the label (`txt_mc` at y -2.7, `$TitleFont` 20 stretched; dark on the chosen tab)
        let (k, dy) = if on { (1.25, -20.0) } else { (1.0, 0.0) };
        let mid = Vec2::new(x, 100.0 + dy + k * -4.0);
        let l = Look::title(20.0 * k, if on { Color::srgb(27.0 / 255.0, 30.0 / 255.0, 31.0 / 255.0) } else { PALE }).center();
        cx.text_in(root, Vec2::ZERO, label, Rect::from_center_size(mid, Vec2::new(230.0, 40.0)), &l);
        cx.hit(root, Vec2::ZERO, Rect::from_corners(Vec2::new(x - 102.65, 100.0 - 37.95), Vec2::new(x + 102.95, 100.0 + 37.95)), JTab(*p));
    }
}

/// A page's sub tabs (`j_SubTabsBkgd` at page (-250,-185); the labels `$NormalFont` 25 in upper
/// case, 25 apart from page x -472.75 at y -185: grey, the chosen one pale, raised 5 and
/// underlined).
fn sub_tabs(cx: &mut Ctx, labels: &[String], sel: u8) {
    // (on the frame: the sub tabs stay while their content slides)
    let root = cx.frame;
    cx.sym(root, Vec2::ZERO, false, "j_SubTabsBkgd", pg(-250.0, -185.0), |_| {});
    let left = pg(-472.75, -185.0 - 22.0);
    let p = cx.at(root, Vec2::ZERO, left);
    let s = cx.st.s;
    let row = cx
        .c
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), height: Val::Px(44.0 * s), column_gap: Val::Px(25.0 * s), align_items: AlignItems::Center, ..default() },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    for (i, label) in labels.iter().enumerate() {
        let on = i as u8 == sel;
        let l = Look::normal(25.0, if on { PALE } else { TAB_GREY }).nowrap();
        let font = cx.font(&l);
        let tab = cx
            .c
            .spawn((
                JSub(i as u8),
                Button,
                Node { padding: UiRect::horizontal(Val::Px(2.0 * s)), top: Val::Px(if on { -5.0 * s } else { 0.0 }), flex_direction: FlexDirection::Column, align_items: AlignItems::Center, ..default() },
                ChildOf(row),
            ))
            .id();
        cx.c.spawn((Text::new(label.to_uppercase()), font, TextColor(l.color), TextLayout::new(Justify::Center, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(tab)));
        if on {
            cx.c.spawn((Node { width: Val::Percent(100.0), height: Val::Px(3.0 * s), margin: UiRect::top(Val::Px(1.0 * s)), ..default() }, BackgroundColor(PALE), Pickable::IGNORE, ChildOf(tab)));
        }
    }
}

/// A list's scrolling, clipped box on the stage.
fn list_box(cx: &mut Ctx, rect: Rect) -> Entity {
    cx.clip_box(rect, true)
}

/// The empty list's card (`j_itemsList_empty`, turned -2.5 degrees) at a stage point.
fn empty_list(cx: &mut Ctx, centre: Vec2, words: String) {
    let root = cx.root;
    cx.sym(root, Vec2::ZERO, false, "j_itemsList_empty", centre, |_| {});
    let t = cx.text_in(root, Vec2::ZERO, words, Rect::from_center_size(centre + Vec2::new(0.0, 0.5), Vec2::new(360.0, 34.0)), &Look::normal(26.0, ROW).center().nowrap());
    cx.c.entity(t).insert(UiTransform { rotation: Rot2::degrees(-2.5), ..default() });
}

/// An objectives list row: the objective's or task's words, its state, whether optional.
pub struct ObjRow {
    pub text: String,
    pub task: bool,
    pub state: crate::kismet::TaskState,
    pub optional: bool,
    pub marker: bool,
}

/// The objectives in the list's order: primary ones first, each with its visible tasks.
pub fn objective_rows(vm: Option<&crate::kismet::Vm>) -> Vec<ObjRow> {
    use crate::kismet::TaskState;
    let mut out = Vec::new();
    let Some(vm) = vm else { return out };
    for optional in [false, true] {
        for path in &vm.objectives {
            let Some(o) = vm.g.objectives.iter().find(|o| &o.path == path) else { continue };
            if o.optional != optional {
                continue;
            }
            let tasks: Vec<ObjRow> = o
                .tasks
                .iter()
                .filter_map(|t| {
                    let (state, hidden) = vm.tasks.get(&t.path).copied().unwrap_or((TaskState::Inactive, t.hidden));
                    (!hidden && state != TaskState::Inactive && !t.text.is_empty()).then(|| ObjRow { text: crate::kismet::clean_text(&t.text), task: true, state, optional, marker: false })
                })
                .collect();
            let state = if !tasks.is_empty() && tasks.iter().all(|t| t.state == TaskState::Completed) { TaskState::Completed } else { TaskState::Active };
            out.push(ObjRow { text: crate::kismet::clean_text(&o.text), task: false, state, optional, marker: !o.no_markers && state == TaskState::Active });
            out.extend(tasks);
        }
    }
    out
}

/// The Objectives page (`j_ObjectivesScreen`): Tasks, Mission clues, Mission Items.
fn objectives(cx: &mut Ctx, js: &JState, data: &Data, stats: &PlayerStats, vm: Option<&crate::kismet::Vm>, map: &str) {
    let root = cx.root;
    cx.sym(root, Vec2::ZERO, false, "j_obj_mc", pg(0.0, 0.0), |_| {});
    sub_tabs(cx, &[data.text(J, "t_Tasks"), data.text(J, "t_ChapterNotes"), data.text(J, "t_MissionItems")], js.obj_tab);
    // the coins (`j_obj_moneyCart_` at (-590,288))
    cx.sym(root, Vec2::ZERO, false, "j_obj_moneyCart_", pg(-590.0, 288.0), |_| {});
    let coins = data.text(BASE, if stats.coins == 1 { "t_CoinCount" } else { "t_CoinsCount" }).replace("§NB_COINS§", &stats.coins.to_string()).to_uppercase();
    cx.text_in(root, Vec2::ZERO, coins, Rect::from_corners(pg(-590.0 + 98.55 - 2.0, 288.0 - 15.25 - 2.0), pg(-590.0 + 98.55 + 195.0, 288.0 - 15.25 + 30.0)), &Look::title(21.0, PALE).nowrap());
    let chapter = data.0.chapters.iter().find(|c| c.tag.eq_ignore_ascii_case(&stats.chapter));
    match js.obj_tab {
        0 => {
            // the list (`j_obj_objectivesList_list` at (-512,-155); its mask 628 x 414)
            cx.sym(root, Vec2::ZERO, false, "j_obj_objectivesList_list", pg(-512.0, -155.0), |fc| fc.set_visible("maskContent", false));
            let mask = Rect::from_corners(pg(-511.2, -155.5), pg(-511.2 + 628.12, -155.5 + 414.34));
            let rows = objective_rows(vm);
            if rows.is_empty() {
                empty_list(cx, pg(-512.0 + 314.0, -155.0 + 207.0), data.text(J, "t_EmptyObjectivesList"));
            } else {
                let list = list_box(cx, mask);
                objective_list(cx, list, &rows, js.obj_sel, data);
            }
            chapter_details(cx, data, chapter);
        }
        1 => {
            // the mission's clues (`j_obj_chapterNotes`), the mission's picture beside them
            cx.sym(root, Vec2::ZERO, false, "j_obj_chapterNotes", pg(0.0, 0.0), |fc| fc.set_visible("_scrollView_mc.mask_mc", false));
            let notes: Vec<String> = stats.notes.iter().filter(|n| crate::journal::is_clue(n)).map(|k| data.0.abstract_items.get(k).map(|x| x.1.clone()).unwrap_or_default()).filter(|t| !t.is_empty()).collect();
            let rect = Rect::from_corners(pg(-507.0, -146.65), pg(-507.0 + 625.0, -146.65 + 414.95));
            if notes.is_empty() {
                empty_list(cx, rect.center(), data.text(BASE, "t_EmptyList"));
            } else {
                let list = list_box(cx, rect);
                let s = cx.st.s;
                for (i, n) in notes.iter().enumerate() {
                    if i > 0 {
                        // the separator (`j_obj_chapterNotes_sep`)
                        let sep = cx.c.spawn((Node { width: Val::Percent(100.0), height: Val::Px(48.0 * s), margin: UiRect::vertical(Val::Px(10.0 * s)), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(list))).id();
                        cx.sym(sep, Vec2::ZERO, false, "j_obj_chapterNotes_sep", Vec2::new(311.0, 24.0), |_| {});
                    }
                    let l = Look::normal(24.0, PALE).justified();
                    let font = cx.font(&l);
                    cx.c.spawn((Text::new(n.clone()), font, TextColor(PALE), TextLayout::new(Justify::Justified, LineBreak::WordBoundary), Node { width: Val::Px(620.0 * s), margin: UiRect::left(Val::Px(7.0 * s)), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(list)));
                }
            }
            let at = pg(405.0, 25.0);
            let det = cx.sym(root, Vec2::ZERO, false, "j_obj_chapterNotes_details", at, |_| {});
            let art = crate::loading::mission_art(map).and_then(|a| cx.ui.file(cx.images, "missions", &format!("MissionsScreen_{a}_Large")));
            if let Some(img) = art {
                cx.picture(root, Vec2::ZERO, img, pg(415.0, 25.0), Vec2::new(480.0, 680.0), 0.0, 1.0);
                if let Some(d) = det {
                    cx.c.queue(move |w: &mut World| {
                        if let Some(mut fc) = w.get_mut::<FlashClip>(d) {
                            fc.set_visible("_empty_mc", false);
                        }
                    });
                }
            }
            // the mission's name up its banner (`_name_mc`, turned -83 degrees)
            if let Some(ch) = chapter {
                let m: Mat = [0.12, -0.99, 0.99, 0.12, -226.1, 238.75];
                let mid = at + apply(&m, Vec2::new(18.5 + 205.45, 5.2 + 20.9));
                let t = cx.text_in(root, Vec2::ZERO, ch.title.to_uppercase(), Rect::from_center_size(mid, Vec2::new(410.9, 41.75)), &Look::title(31.0, PALE).flat().nowrap());
                cx.c.entity(t).insert(UiTransform { rotation: Rot2::radians(m[1].atan2(m[0])), ..default() });
            }
        }
        _ => {
            // the mission's items (`j_obj_missionItems_list`): cards as the inventory's
            cx.sym(root, Vec2::ZERO, false, "j_obj_missionItems_list", pg(-512.0, -155.0), |fc| fc.set_visible("maskContent", false));
            let items = crate::journal::mission_items(data, stats);
            item_cards(cx, data, &items, js.obj_sel);
        }
    }
}

/// The objectives: section headers (`j_obj_objectivesList_section`), objectives and their
/// tasks (` j_obj_objectivesList_taskItem`: `$NormalFont` 24, tinted by state), the chosen one
/// bracketed (`j_objList_itemSelection`).
fn objective_list(cx: &mut Ctx, list: Entity, rows: &[ObjRow], sel: usize, data: &Data) {
    use crate::kismet::TaskState;
    let s = cx.st.s;
    let mask_w = 628.12;
    let mut section: Option<bool> = None;
    for (i, r) in rows.iter().enumerate() {
        let mut gap = if i == 0 { 0.0 } else if r.task { 0.0 } else { 20.0 };
        if !r.task && section != Some(r.optional) {
            section = Some(r.optional);
            let head = cx.c.spawn((Node { width: Val::Percent(100.0), height: Val::Px(49.0 * s), margin: UiRect::top(Val::Px(if i == 0 { 0.0 } else { 20.0 } * s)), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(list))).id();
            cx.sym(head, Vec2::ZERO, false, "j_obj_objectivesList_section", Vec2::new(mask_w * 0.5, 0.0), |_| {});
            let words = data.text(J, if r.optional { "t_SecondaryTasks" } else { "t_PrimaryTasks" }).to_uppercase();
            cx.text_in(head, Vec2::ZERO, words, Rect::from_corners(Vec2::new(0.0, 10.35), Vec2::new(mask_w, 10.35 + 31.0)), &Look::title(22.0, DARK).center());
            gap = 10.0;
        }
        let on = i == sel;
        let locked = matches!(r.state, TaskState::Completed | TaskState::Failed);
        let x = if r.task { 55.0 } else { 0.0 } + if on && !locked { 10.0 } else { 0.0 };
        let width = mask_w - 33.65 - 60.0 - if r.task { 55.0 } else { 0.0 };
        let row = cx
            .c
            .spawn((
                JRow(i),
                Button,
                Node {
                    width: Val::Px((mask_w - x) * s),
                    margin: UiRect { left: Val::Px(x * s), top: Val::Px(gap * s), ..default() },
                    padding: UiRect { left: Val::Px(33.65 * s), top: Val::Px(5.0 * s), bottom: Val::Px(5.0 * s), ..default() },
                    flex_shrink: 0.0,
                    ..default()
                },
                ChildOf(list),
            ))
            .id();
        if on {
            cx.c.entity(row).insert(JSelRow);
        }
        let color = if locked { LOCKED } else if on { PALE } else { ROW };
        let l = Look::normal(24.0, color);
        let font = cx.font(&l);
        cx.c.spawn((Text::new(r.text.clone()), font, TextColor(color), TextLayout::new(Justify::Left, LineBreak::WordBoundary), Node { width: Val::Px(width * s), ..default() }, Pickable::IGNORE, ChildOf(row)));
        // its state (`ic`: a diamond and a tick or a cross) and its marker (`marker_mc`)
        let label = match r.state {
            TaskState::Completed => Some("done"),
            TaskState::Failed => Some("failed"),
            _ => None,
        };
        if let Some(label) = label {
            cx.sym(row, Vec2::ZERO, false, "259", Vec2::new(15.0, 5.0 - 2.0 + 14.3), |fc| {
                fc.goto("", label, true);
            });
        }
        if r.marker {
            cx.sym(row, Vec2::ZERO, false, "265", Vec2::new(14.75, 5.0 - 2.0 + 13.85), |fc| {
                let tl = fc.tl.clone();
                if let Some(c) = fc.clip.child_mut("ic_mc") {
                    c.goto(&tl, if r.optional { 1 } else { 0 }, false);
                }
            });
        }
        if on {
            let sel_w = mask_w - x;
            let p = Vec2::ZERO;
            let e = cx.sym(row, p, false, "j_objList_itemSelection", Vec2::ZERO, |fc| {
                if locked {
                    fc.alpha = 0.3;
                }
            });
            if let Some(e) = e {
                cx.c.entity(e).insert((SelCorners(sel_w), Node { position_type: PositionType::Absolute, left: Val::Px(0.0), top: Val::Percent(50.0), ..default() }));
            }
        }
    }
}

/// The chapter's details (`j_obj_chapterDetails` at (355,-280)): its targets' portraits, its
/// name and briefing.
fn chapter_details(cx: &mut Ctx, data: &Data, chapter: Option<&dhcook::format::ChapterDef>) {
    let root = cx.root;
    let at = pg(355.0, -280.0);
    let n = chapter.map(|c| c.targets.len().min(2)).unwrap_or(0);
    let mut targets: Vec<Mat> = Vec::new();
    cx.sym(root, Vec2::ZERO, false, "j_obj_chapterDetails", at, |fc| {
        let tl = fc.tl.clone();
        let tm = fc.clip.placed("_targets_mc").map(|p| p.0);
        if n == 0 {
            fc.set_visible("_targets_mc", false);
            return;
        }
        if let Some(c) = fc.clip.child_mut("_targets_mc") {
            c.goto(&tl, n, false);
            for k in 0..n {
                let name = format!("target{k}_mc");
                // the target's state: none (`targetState` 5: `animPortrait_mc` pale)
                if let Some(t) = c.child_mut(&name) {
                    t.goto(&tl, 4, false);
                    if let Some(a) = t.child_mut("animPortrait_mc") {
                        a.goto(&tl, 4, false);
                    }
                }
                if let (Some(tm), Some((m, _))) = (tm, c.placed(&name)) {
                    targets.push(concat(&tm, &m));
                }
            }
        }
    });
    if let Some(ch) = chapter {
        for (k, m) in targets.iter().enumerate() {
            let Some((_, portrait)) = ch.targets.get(k) else { continue };
            if let Some(img) = cx.ui.file(cx.images, "portraits", portrait) {
                let sc = (m[0] * m[0] + m[1] * m[1]).sqrt();
                cx.picture(root, Vec2::ZERO, img, at + apply(m, Vec2::new(-15.0, -5.0)), Vec2::splat(192.0 * sc), m[1].atan2(m[0]).to_degrees(), 1.0);
            }
        }
        // the scroll view: x -202, y 285 under targets (60 without), down to y 530
        let top = if n > 0 { 285.0 } else { 60.0 };
        let rect = Rect::from_corners(at + Vec2::new(-202.0 + 1.65, top + 0.7), at + Vec2::new(-202.0 + 340.0, 530.0));
        let col = list_box(cx, rect);
        cx.c.entity(col).remove::<JScroll>();
        let s = cx.st.s;
        let name = Look::title(26.0, PALE).center();
        let font = cx.font(&name);
        cx.c.spawn((Text::new(ch.title.to_uppercase()), font, TextColor(PALE), TextLayout::new(Justify::Center, LineBreak::WordBoundary), UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Node { width: Val::Px(336.0 * s), margin: UiRect::top(Val::Px(4.0 * s)), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
        cx.c.spawn((Node { width: Val::Px(256.0 * s), height: Val::Px(2.0 * s), margin: UiRect::top(Val::Px(8.0 * s)), flex_shrink: 0.0, ..default() }, BackgroundColor(PALE.with_alpha(0.25)), Pickable::IGNORE, ChildOf(col)));
        let desc = Look::normal(23.0, PALE).justified();
        let font = cx.font(&desc);
        cx.c.spawn((Text::new(crate::gamedata::readable(ch.description.trim())), font, TextColor(PALE), TextLayout::new(Justify::Justified, LineBreak::WordBoundary), Node { width: Val::Px(334.0 * s), margin: UiRect::new(Val::Px(2.0 * s), Val::ZERO, Val::Px(7.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
        let _ = data;
    }
}

/// An item's details (`j_itemDetails` at (355,-280)): its picture, count, name and words.
fn item_details(cx: &mut Ctx, item: Option<(&str, &str, Option<(Handle<Image>, Vec2)>, Option<u32>)>) {
    let root = cx.root;
    let at = pg(355.0, -280.0);
    let qty = item.as_ref().and_then(|i| i.3).filter(|q| *q > 1);
    cx.sym(root, Vec2::ZERO, false, "j_itemDetails", at, |fc| {
        if qty.is_none() {
            fc.set_visible("_quantity_mc", false);
        }
    });
    let Some((name, desc, img, _)) = item else { return };
    if let Some(img) = img {
        cx.picture(root, Vec2::ZERO, img, pg(342.0, -73.0), Vec2::splat(269.5 * 1.05), 0.0, 1.0);
    }
    if let Some(q) = qty {
        cx.text(root, Vec2::ZERO, q.to_string(), pg(355.0 - 121.15 - 59.0 + 2.0, -280.0 + 343.3 - 20.65 + 2.0), None, &Look::title(30.0, PALE));
    }
    details_text(cx, Rect::from_corners(pg(175.35, 95.2), pg(175.35 + 327.0, 95.2 + 160.0)), name, desc);
}

/// A details panel's scrolling name (`$TitleFont` 26, upper case) and words (`$NormalFont` 25).
fn details_text(cx: &mut Ctx, rect: Rect, name: &str, desc: &str) {
    let s = cx.st.s;
    let col = list_box(cx, rect);
    cx.c.entity(col).remove::<JScroll>();
    let l = Look::title(26.0, PALE);
    let font = cx.font(&l);
    cx.c.spawn((Text::new(name.to_uppercase()), font, TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::WordBoundary), UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Node { width: Val::Px(321.0 * s), margin: UiRect::new(Val::Px(4.2 * s), Val::ZERO, Val::Px(4.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
    if !desc.is_empty() {
        let l = Look::normal(25.0, PALE);
        let font = cx.font(&l);
        cx.c.spawn((Text::new(desc.to_string()), font, TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::WordBoundary), Node { width: Val::Px(321.0 * s), margin: UiRect::new(Val::Px(3.65 * s), Val::ZERO, Val::Px(13.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
    }
}

/// A note, book, audiograph or map of the Notes page.
pub struct LogEntry {
    pub key: String,
    pub name: String,
    pub desc: String,
    pub icon: &'static str,
}

/// The Notes page's entries under a sub tab: written notes (and maps), books, audiographs.
pub fn log_entries(data: &Data, stats: &PlayerStats, tab: u8) -> Vec<LogEntry> {
    let mut out = Vec::new();
    for k in &stats.notes {
        if crate::journal::is_clue(k) {
            continue;
        }
        // (the mission's items have their own tab; some are never shown: `DJIS_None`)
        if data.0.journal_items.get(k).is_some_and(|(s, _)| s == "DJIS_Mission" || s == "DJIS_None") {
            continue;
        }
        let (want, icon) = if k.starts_with("ag:") {
            (2, "Audiograph")
        } else if k.to_ascii_lowercase().contains("book") {
            (1, "ReadableNote_Book")
        } else {
            (0, "ReadableNote_Sheet")
        };
        if want != tab {
            continue;
        }
        let (name, desc) = if let Some((t, _)) = crate::journal::location_map(k) {
            (t, String::new())
        } else if let Some(a) = k.strip_prefix("ag:").and_then(|g| data.0.audiographs.get(g)) {
            (a.title.clone(), a.description.clone())
        } else {
            let (n, d) = data.0.abstract_items.get(k).cloned().unwrap_or_default();
            (if n.is_empty() { k.rsplit('.').next().unwrap_or(k).replace('_', " ") } else { n }, d)
        };
        out.push(LogEntry { key: k.clone(), name, desc: crate::gamedata::readable(&desc), icon });
    }
    // the maps after the notes
    out.sort_by_key(|e| e.key.starts_with("map:"));
    out
}

/// The rows of a one-column list (`j_logs_logsList_item`, `j_bc_charmsList_item`: 56 apart, 8
/// shown): the first shown, keeping the chosen one in.
fn window(n: usize, sel: usize, shown: usize) -> usize {
    if n <= shown {
        0
    } else {
        sel.saturating_sub(shown - 1).min(n - shown)
    }
}

/// The Notes page (`j_LogsScreen`): written notes, books, audiographs.
fn logs(cx: &mut Ctx, js: &JState, data: &Data, stats: &PlayerStats) {
    let root = cx.root;
    cx.sym(root, Vec2::ZERO, false, "j_logs_mc", pg(0.0, 0.0), |_| {});
    sub_tabs(cx, &[data.text(J, "t_Notes"), data.text(J, "t_Books"), data.text(J, "t_AudioLogs")], js.log_tab);
    cx.sym(root, Vec2::ZERO, false, "j_logs_logsList", pg(-512.0, -155.0), |fc| fc.set_visible("maskContent", false));
    let entries = log_entries(data, stats, js.log_tab);
    let sel = js.log_sel.min(entries.len().saturating_sub(1));
    let first = window(entries.len(), sel, 8);
    let clipped = cx.clip_box(Rect::from_corners(pg(-511.0, -155.5), pg(-511.0 + 691.0, -155.5 + 448.0)), false);
    let origin = pg(-511.0, -155.5);
    for v in 0..8 {
        let i = first + v;
        let y = -127.0 + 56.0 * v as f32;
        let e = entries.get(i);
        let on = e.is_some() && i == sel;
        let x = -512.0 + if on { 20.0 } else { 0.0 };
        cx.sym(clipped, origin, false, "j_logs_logsList_item", pg(x, y), |fc| {
            let tl = fc.tl.clone();
            if let Some(b) = fc.clip.child_mut("_bkgd_mc.mc") {
                b.goto(&tl, i % 3, false);
            }
            fc.set_visible("playingIc_mc", false);
            if e.is_none() {
                fc.goto("", "disabled", true);
            } else if on {
                fc.goto("", "over", true);
            }
        });
        let Some(e) = e else { continue };
        if let Some(img) = cx.ui.file(cx.images, "itemsmall", &format!("{}_Small", e.icon)) {
            cx.picture(clipped, origin, img, pg(x + 65.45, y), Vec2::splat(43.5), 0.0, 1.0);
        }
        let playing = js.playing.is_some_and(|p| Some(p) == e.key.strip_prefix("ag:"));
        let name = if playing { format!("{}  ...", e.name) } else { e.name.clone() };
        cx.text(clipped, origin, name, pg(x + 98.5, y - 14.4), Some(483.0), &Look::normal(25.0, if on { DARK } else { PALE }).nowrap());
        cx.hit(clipped, origin, Rect::from_corners(pg(x + 2.35, y - 23.65), pg(x + 630.0, y + 23.65)), JRow(i));
    }
    if entries.is_empty() {
        empty_list(cx, pg(-512.0 + 326.0, -155.0 + 224.0), data.text(BASE, "t_EmptyList"));
    }
    // the details (`j_logs_logDetails`): the name, a rule, the words; a map's picture; the
    // audiograph's play/stop
    let at = pg(355.0, -280.0);
    cx.sym(root, Vec2::ZERO, false, "j_logs_logDetails", at, |_| {});
    let Some(e) = entries.get(sel) else { return };
    let rect = Rect::from_corners(pg(167.0, -205.0), pg(167.0 + 327.0, -205.0 + 450.0));
    let col = list_box(cx, rect);
    cx.c.entity(col).remove::<JScroll>();
    let s = cx.st.s;
    let l = Look::title(26.0, PALE);
    let font = cx.font(&l);
    cx.c.spawn((Text::new(e.name.to_uppercase()), font, TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::WordBoundary), UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Node { width: Val::Px(321.0 * s), margin: UiRect::new(Val::Px(4.0 * s), Val::ZERO, Val::Px(6.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
    cx.c.spawn((Node { width: Val::Px(321.0 * s), height: Val::Px(2.0 * s), margin: UiRect::new(Val::Px(4.0 * s), Val::ZERO, Val::Px(10.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, BackgroundColor(PALE.with_alpha(0.6)), Pickable::IGNORE, ChildOf(col)));
    if let Some((_, image)) = crate::journal::location_map(&e.key) {
        if let Some((h, size)) = cx.ui.file(cx.images, "maps", image) {
            let w = 321.0;
            cx.c.spawn((ImageNode::new(h), Node { width: Val::Px(w * s), height: Val::Px(w * size.y / size.x.max(1.0) * s), margin: UiRect::new(Val::Px(4.0 * s), Val::ZERO, Val::Px(8.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
        }
    }
    if !e.desc.is_empty() {
        let l = Look::normal(25.0, PALE);
        let font = cx.font(&l);
        cx.c.spawn((Text::new(e.desc.clone()), font, TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::WordBoundary), Node { width: Val::Px(321.0 * s), margin: UiRect::new(Val::Px(4.0 * s), Val::ZERO, Val::Px(8.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
    }
    if let Some(k) = e.key.strip_prefix("ag:") {
        let on = js.playing == Some(k);
        let l = Look::title(24.0, PALE).flat();
        let font = cx.font(&l);
        let b = cx.c.spawn((JAct, Button, Node { margin: UiRect::new(Val::Px(4.0 * s), Val::ZERO, Val::Px(14.0 * s), Val::ZERO), padding: UiRect::axes(Val::Px(14.0 * s), Val::Px(6.0 * s)), align_self: AlignSelf::FlexStart, flex_shrink: 0.0, ..default() }, BackgroundColor(DARK.with_alpha(0.85)), ChildOf(col))).id();
        cx.c.spawn((Text::new(data.text(J, if on { "t_stopLog" } else { "t_playLog" })), font, TextColor(PALE), Pickable::IGNORE, ChildOf(b)));
    }
}

/// The Inventory page (`j_InventoryScreen`): its sub tabs over the cards (`sh_itemList_item`, 4
/// across and 3 down), the item's details.
fn inventory(cx: &mut Ctx, js: &JState, data: &Data, stats: &PlayerStats) {
    let root = cx.root;
    cx.sym(root, Vec2::ZERO, false, "j_inventory_mc", pg(0.0, 0.0), |_| {});
    let labels: Vec<String> = crate::journal::INV_TABS.iter().map(|k| data.text(J, k)).collect();
    sub_tabs(cx, &labels, js.inv_tab);
    cx.sym(root, Vec2::ZERO, false, "j_inv_inventoryList", pg(-512.0, -155.0), |fc| fc.set_visible("maskContent", false));
    let items = crate::journal::inventory(data, stats, js.inv_tab);
    item_cards(cx, data, &items, js.inv_sel);
}

/// Item cards (`sh_itemList_item`, 4 across and 3 down, the chosen one's row in view) and the
/// chosen item's details: the Inventory's and the Mission Items'.
fn item_cards(cx: &mut Ctx, data: &Data, items: &[crate::journal::InvEntry], sel: usize) {
    let sel = sel.min(items.len().saturating_sub(1));
    // the rows shown: three, the chosen one's in them
    let rows = items.len().div_ceil(4).max(1);
    let first_row = if rows <= 3 { 0 } else { (sel / 4).saturating_sub(2).min(rows - 3) };
    let clipped = cx.clip_box(Rect::from_corners(pg(-540.0, -155.0), pg(-540.0 + 710.0, -155.0 + 495.0)), false);
    let origin = pg(-540.0, -155.0);
    if items.is_empty() {
        empty_list(cx, pg(-186.0, 75.0), data.text(BASE, "t_EmptyList"));
    } else {
        for v in 0..12 {
            let i = first_row * 4 + v;
            let c = pg([-421.0, -260.0, -99.0, 62.0][v % 4], [-64.0, 103.0, 270.0][v / 4]);
            let it = items.get(i);
            let on = it.is_some() && i == sel;
            let turn = card_turn(i);
            let k = if on { 1.1 } else { 1.0 };
            let (sn, cs) = turn.to_radians().sin_cos();
            let m: Mat = [cs * k, sn * k, -sn * k, cs * k, 0.0, 0.0];
            cx.sym(clipped, origin, false, "sh_itemList_item", c, |fc| {
                fc.m = m;
                let tl = fc.tl.clone();
                for part in ["slotOff_mc", "slotOn_mc"] {
                    if let Some(b) = fc.clip.child_mut(part) {
                        b.goto(&tl, i % 3, false);
                    }
                }
                if it.is_none() {
                    fc.goto("", "disabled", true);
                } else if on {
                    fc.goto("", "selected", true);
                }
                if it.is_none_or(|it| it.count.is_none_or(|n| n == 1)) {
                    fc.set_visible("quantity_mc", false);
                }
            });
            let Some(it) = it else { continue };
            if let Some(img) = cx.ui.file(cx.images, "items", &format!("{}_Large", it.icon)) {
                cx.picture(clipped, origin, img, c + apply(&m, Vec2::new(0.0, -14.5)), Vec2::new(138.0, 100.0) * k, turn, 1.0);
            }
            let t = cx.text_in(clipped, origin, it.name.clone(), Rect::from_center_size(c + apply(&m, Vec2::new(0.0, 62.2)), Vec2::new(149.6, 46.6) * k), &Look::normal(19.0 * k, if on { CARD_NAME_ON } else { CARD_NAME }).center());
            cx.c.entity(t).insert(UiTransform { rotation: Rot2::degrees(turn), ..default() });
            if let Some(n) = it.count.filter(|n| *n != 1) {
                cx.text(clipped, origin, n.to_string(), c + apply(&m, Vec2::new(-69.6, 18.3 - 12.0)), None, &Look::normal(19.0 * k, PALE).nowrap());
            }
            cx.hit(clipped, origin, Rect::from_center_size(c, Vec2::new(165.0, 160.0)), JRow(i));
        }
    }
    let it = items.get(sel);
    let img = it.and_then(|it| cx.ui.file(cx.images, "items", &format!("{}_Large", it.icon)));
    item_details(cx, it.map(|it| (it.name.as_str(), it.desc.as_str(), img, it.count)));
}

/// The Bone Charms page (`j_BoneCharmsScreen`): the charms found, worn ones in their sockets,
/// the sockets bar (`_slots_mc`), the charm's details.
fn charms(cx: &mut Ctx, js: &JState, data: &Data, stats: &PlayerStats) {
    let root = cx.root;
    cx.sym(root, Vec2::ZERO, false, "j_boneCharms_mc", pg(0.0, 0.0), |_| {});
    let unlocked = crate::journal::charm_slots(data, stats);
    let max = data.pawn("m_MaxActivatedBoneCharmCount", 3.0) as usize + data.0.upgrades.iter().filter(|u| u.id.starts_with("Twk_Upgrade_BoneCharms")).count();
    let worn = stats.charms.len();
    let list_at = pg(-505.0, -215.0);
    cx.sym(root, Vec2::ZERO, false, "j_bc_charmsList", list_at, |fc| {
        fc.set_visible("maskContent", false);
        let tl = fc.tl.clone();
        for i in 0..10 {
            let name = format!("_slots_mc.slot{i}_mc");
            if i >= max.max(unlocked) {
                fc.set_visible(&name, false);
                continue;
            }
            let label = if i >= unlocked { "locked" } else if i < worn { "full" } else { "empty" };
            if let Some(c) = fc.clip.child_mut(&name) {
                c.goto_label(&tl, label, false);
            }
        }
    });
    // the bar's words: "ACTIVATED BONE CHARMS: n/m"
    let l = Look::title(26.0, PALE).nowrap();
    let font = cx.font(&l);
    let p = cx.at(root, Vec2::ZERO, list_at + Vec2::new(-82.0 + 95.0 + 2.0, 446.2 + 2.95 + 2.0));
    let t = cx
        .c
        .spawn((Text::new(""), font.clone(), TextColor(ROW), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Pickable::IGNORE, ChildOf(root)))
        .id();
    cx.c.spawn((TextSpan::new(data.text(J, "t_BoneCharmsActivated").to_uppercase()), font.clone(), TextColor(ROW), ChildOf(t)));
    cx.c.spawn((TextSpan::new(format!("{worn}/{unlocked}")), font, TextColor(PALE), ChildOf(t)));
    let owned = &stats.charms_owned;
    let sel = js.charm_sel.min(owned.len().saturating_sub(1));
    let first = window(owned.len(), sel, 8);
    let origin = pg(-504.0, -215.5);
    let clipped = cx.clip_box(Rect::from_corners(origin, origin + Vec2::new(691.0, 445.5)), false);
    let icon = cx.ui.file(cx.images, "itemsmall", "BoneCharms_Small");
    for v in 0..8 {
        let i = first + v;
        let y = -187.0 + 56.0 * v as f32;
        let name = owned.get(i);
        let on = name.is_some() && i == sel;
        let x = -505.0 + if on { 20.0 } else { 0.0 };
        let wearing = name.is_some_and(|n| stats.charms.contains(n));
        cx.sym(clipped, origin, false, "j_bc_charmsList_item", pg(x, y), |fc| {
            let tl = fc.tl.clone();
            if let Some(b) = fc.clip.child_mut("_bkgd_mc.mc") {
                b.goto(&tl, i % 3, false);
            }
            if let Some(c) = fc.clip.child_mut("slot_mc") {
                c.goto_label(&tl, if wearing { "full" } else { "empty" }, false);
            }
            if name.is_none() {
                fc.goto("", "disabled", true);
            } else if on {
                fc.goto("", "over", true);
            }
        });
        let Some(name) = name else { continue };
        if wearing {
            if let Some(img) = icon.clone() {
                cx.picture(clipped, origin, img, pg(x + 65.45, y), Vec2::splat(43.5), 0.0, 1.0);
            }
        }
        cx.text(clipped, origin, name.clone(), pg(x + 98.5, y - 14.4), Some(483.0), &Look::normal(25.0, if on { DARK } else { PALE }).nowrap());
        cx.hit(clipped, origin, Rect::from_corners(pg(x + 0.35, y - 27.65), pg(x + 630.0, y + 27.65)), JRow(i));
    }
    if owned.is_empty() {
        empty_list(cx, pg(-505.0 + 326.0, -215.0 + 223.0), data.text(BASE, "t_EmptyList"));
    }
    let desc = owned.get(sel).and_then(|n| data.charm(n)).map(|(c, i)| c.levels[i].1.clone()).unwrap_or_default();
    let img = owned.get(sel).and_then(|_| cx.ui.file(cx.images, "items", "BoneCharms_Large"));
    item_details(cx, owned.get(sel).map(|n| (n.as_str(), desc.as_str(), img, None)));
    if owned.get(sel).is_some() {
        // the charm's action (A: wear it, or take it off)
        let wearing = owned.get(sel).is_some_and(|n| stats.charms.contains(n));
        let words = data.text(J, if wearing { "t_RemoveBoneCharm" } else { "t_ActivateBoneCharm" });
        action_button(cx, pg(180.0, 265.0), words);
    }
}

/// An action shown as the help bar's would be (the original's `A` prompt), clickable.
fn action_button(cx: &mut Ctx, at: Vec2, words: String) {
    let s = cx.st.s;
    let p = cx.at(cx.root, Vec2::ZERO, at);
    let l = Look::title(22.0, PALE).flat();
    let font = cx.font(&l);
    let b = cx
        .c
        .spawn((JAct, Button, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), padding: UiRect::axes(Val::Px(14.0 * s), Val::Px(6.0 * s)), ..default() }, BackgroundColor(DARK.with_alpha(0.85)), ChildOf(cx.root)))
        .id();
    cx.c.spawn((Text::new(words), font, TextColor(PALE), Pickable::IGNORE, ChildOf(b)));
}

/// The powers' slots in `j_pow_powersList` (slot0..9) by our power names, the movie's power id.
pub const POWER_SLOTS: [(&str, &str); 10] = [
    ("DarkVision", "dark_vision"),
    ("Blink", "blink"),
    ("Possess", "possession"),
    ("BendTime", "bend_time"),
    ("DevouringSwarm", "rat_swarm"),
    ("Windblast", "wind_blast"),
    ("Vitality", "vitality"),
    ("BloodThirsty", "blood_thirst"),
    ("Celerity", "celerity"),
    ("ShadowKill", "shadow_kill"),
];

/// The Powers page (`j_PowersScreen`): the ten discs (six powers, four enhancements), the chosen
/// one ringed with its tooltip, its details.
fn powers(cx: &mut Ctx, js: &JState, data: &Data, stats: &PlayerStats) {
    let root = cx.root;
    cx.sym(root, Vec2::ZERO, false, "j_powers_mc", pg(0.0, 0.0), |_| {});
    let list_at = pg(-170.0, 0.0);
    let sel = js.power_sel.min(9);
    let mut slot_at = [Vec2::ZERO; 10];
    let can_buy = |name: &str| -> Option<u32> {
        let lvl = stats.power(name);
        crate::journal::next_cost(data, name, lvl).filter(|c| stats.runes >= *c)
    };
    cx.sym(root, Vec2::ZERO, false, "j_pow_powersList", list_at, |fc| {
        let tl = fc.tl.clone();
        for (k, (name, id)) in POWER_SLOTS.iter().enumerate() {
            let slot = format!("slot{k}");
            let active = k < 6;
            let lvl = stats.power(name);
            if let Some((m, _)) = fc.clip.placed(&slot) {
                slot_at[k] = Vec2::new(m[4], m[5]);
            }
            if k == sel {
                if let Some((m, _)) = fc.clip.placed_mut(&slot) {
                    m[0] = 1.1;
                    m[3] = 1.1;
                }
            }
            let kind = if active { "active" } else { "passive" };
            if let Some(c) = fc.clip.child_mut(&slot) {
                if let Some(ic) = c.child_mut("ic_mc") {
                    ic.goto_label(&tl, &format!("{kind}_{}", if lvl > 0 { "on" } else { "off" }), false);
                    if let Some(i) = ic.child_mut("ic") {
                        i.goto_label(&tl, id, false);
                    }
                    if let Some(g) = ic.child_mut("glow_mc") {
                        if k == sel {
                            g.goto_label(&tl, "loop", true);
                        }
                    }
                    if let Some(s) = ic.child_mut("_selection_mc") {
                        s.visible = false;
                    }
                }
                if let Some(l) = c.child_mut("level_mc") {
                    l.goto_label(&tl, if lvl > 0 { "on" } else { "off" }, false);
                }
                if can_buy(name).is_none() {
                    if let Some(a) = c.child_mut("arrow_mc") {
                        a.visible = false;
                    }
                } else if let Some(a) = c.child_mut("arrow_mc") {
                    a.goto_label(&tl, "anim", true);
                }
            }
        }
        // the ring on the chosen disc
        if let Some((m, _)) = fc.clip.placed_mut("_selection_mc") {
            m[4] = slot_at[sel].x + 0.5;
            m[5] = slot_at[sel].y;
        }
        if let Some(c) = fc.clip.child_mut("_selection_mc") {
            c.goto_label(&tl, if sel < 6 { "active" } else { "passive" }, false);
        }
        // the tooltip (`t_Acquire` / `t_Upgrade`), below the powers, above the enhancements
        let (name, _) = POWER_SLOTS[sel];
        let lvl = stats.power(name);
        if lvl >= 2 {
            fc.set_visible("_toolTip_mc", false);
        } else {
            let p = slot_at[sel];
            let y = if sel < 6 { p.y + 150.0 } else { p.y - 70.0 - 26.0 };
            if let Some((m, cxf)) = fc.clip.placed_mut("_toolTip_mc") {
                m[4] = p.x;
                m[5] = y;
                if can_buy(name).is_none() {
                    cxf[0] = 0.4;
                    cxf[1] = 0.4;
                    cxf[2] = 0.4;
                }
            }
            if let Some(c) = fc.clip.child_mut("_toolTip_mc") {
                c.goto(&tl, if p.x <= 0.0 { 0 } else { 1 }, false);
            }
            if can_buy(name).is_some() {
                fc.set_visible("_toolTip_mc.strike_mc", false);
            }
            // (the pad's A: on PC the pointer clicks it)
            fc.set_visible("_toolTip_mc.circle_mc", false);
        }
    });
    // the discs' names, levels and the headers
    for (k, (name, _)) in POWER_SLOTS.iter().enumerate() {
        let c = list_at + slot_at[k];
        let lvl = stats.power(name);
        let k_sc = if k == sel { 1.1 } else { 1.0 };
        let title = crate::journal::ENTRIES.iter().find(|e| e.0 == *name).map(|e| data.text(J, e.1)).unwrap_or_default();
        cx.text_in(root, Vec2::ZERO, title, Rect::from_center_size(c + Vec2::new(0.0, (82.0 + 14.5) * k_sc), Vec2::new(204.0, 29.0)), &Look::normal(22.0, POWER_NAME).center().nowrap());
        let roman = ["", "I", "II"][lvl.min(2) as usize];
        cx.text_in(root, Vec2::ZERO, roman, Rect::from_center_size(c + Vec2::new(0.0, 68.35 * k_sc), Vec2::new(40.0, 30.0)), &Look::title(21.0, if lvl > 0 { DARK } else { ROW }).center().flat());
        cx.hit(root, Vec2::ZERO, Rect::from_center_size(c + Vec2::new(0.0, 26.3), Vec2::new(152.0, 160.0)), JRow(k));
    }
    for (y, key) in [(-209.65, "t_ActivePowers"), (287.35, "t_PassivePowers")] {
        cx.text(root, Vec2::ZERO, data.text(J, key).to_uppercase(), list_at + Vec2::new(-409.6 + 42.65 + 2.0, y - 19.1 + 2.0), None, &Look::title(24.0, LABEL));
    }
    // the tooltip's words, and its click buys
    let (name, _) = POWER_SLOTS[sel];
    let lvl = stats.power(name);
    if lvl < 2 {
        let p = list_at + slot_at[sel];
        let y = if sel < 6 { p.y + 150.0 } else { p.y - 70.0 - 26.0 };
        let words = data.text(J, if lvl == 0 { "t_Acquire" } else { "t_Upgrade" });
        let right = slot_at[sel].x > 0.0;
        let rect = if right { Rect::from_corners(Vec2::new(p.x - 230.0 + 50.0 - 31.5, y - 13.0 - 4.0), Vec2::new(p.x + 50.0 - 31.5, y + 18.0)) } else { Rect::from_corners(Vec2::new(p.x + 31.5, y - 13.0 - 4.0), Vec2::new(p.x + 31.5 + 200.0, y + 18.0)) };
        let l = Look::normal(23.0, if can_buy(name).is_some() { PALE } else { PALE.with_alpha(0.4) }).nowrap();
        cx.text_in(root, Vec2::ZERO, words, rect, &if right { l.right() } else { l });
        cx.hit(root, Vec2::ZERO, Rect::from_corners(Vec2::new(p.x - 50.0 - if right { 230.0 } else { 0.0 }, y - 25.0), Vec2::new(p.x + 230.0 - if right { 230.0 } else { 0.0 }, y + 27.0)), JAct);
    }
    power_details(cx, sel, data, stats);
}

/// The chosen power's details (`j_pow_powerDetails` at (420,-290)): its picture, name, mana,
/// runes (cost and owned), its levels' words.
fn power_details(cx: &mut Ctx, sel: usize, data: &Data, stats: &PlayerStats) {
    let root = cx.root;
    let at = pg(420.0, -290.0);
    let (name, _) = POWER_SLOTS[sel];
    let entry = crate::journal::ENTRIES.iter().find(|e| e.0 == name).copied();
    let lvl = stats.power(name);
    let cost = crate::journal::next_cost(data, name, lvl);
    cx.sym(root, Vec2::ZERO, false, "j_pow_powerDetails", at, |fc| {
        fc.set_visible("_scrollViewZone_mc", false);
        let tl = fc.tl.clone();
        if lvl >= 2 {
            fc.set_visible("_runes_mc", false);
        } else if let Some(c) = fc.clip.child_mut("_runes_mc") {
            c.goto_label(&tl, if cost.is_some_and(|c| c > stats.runes) { "locked" } else { "default" }, false);
        }
    });
    let Some((_, tkey, note, art)) = entry else { return };
    if let Some(img) = cx.ui.file(cx.images, "powers", art) {
        cx.picture(root, Vec2::ZERO, img, at + Vec2::new(110.0, 119.4), Vec2::splat(294.0 * 1.15), 4.0, 0.32);
    }
    let name_at = at + Vec2::new(-197.3, 95.05);
    cx.text(root, Vec2::ZERO, data.text(J, tkey).to_uppercase(), name_at + Vec2::new(2.8, 2.5), Some(300.0), &Look::title(32.0, PALE));
    let mut y = name_at.y + 62.2;
    // (in words: `RPG.int`'s `<power>_ManaCost`, "None" for the enhancements)
    let words = data.text(RPG, &format!("{}_ManaCost", rpg_key(name)));
    let words = if words.is_empty() { data.active(name).map(|a| format!("{}%", a.mana.round() as i32)) } else { Some(words) };
    if let Some(words) = words {
        let l = Look::normal(25.0, LABEL).nowrap();
        let font = cx.font(&l);
        let p = cx.at(root, Vec2::ZERO, Vec2::new(name_at.x + 2.5, y));
        let t = cx.c.spawn((Text::new(""), font.clone(), TextColor(LABEL), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root))).id();
        cx.c.spawn((TextSpan::new(data.text(J, "t_ManaConsumption")), font.clone(), TextColor(LABEL), ChildOf(t)));
        cx.c.spawn((TextSpan::new(words), font, TextColor(PALE), ChildOf(t)));
        y += 36.0;
    }
    // the runes: what the next level costs, what Corvo has
    if lvl < 2 {
        let r = at + Vec2::new(-177.8, 188.8);
        let runes = |n: u32| data.text(J, if n == 1 { "t_RuneCount" } else { "t_RunesCount" }).replace("§NB_RUNES§", &n.to_string());
        for (dy, label, value) in [(12.95, "t_Cost", runes(cost.unwrap_or(0))), (40.95, "t_YouHave", runes(stats.runes))] {
            let l = Look::normal(25.0, LABEL).right().nowrap();
            let font = cx.font(&l);
            let bx = cx.text_in(root, Vec2::ZERO, "", Rect::from_corners(r + Vec2::new(0.0, dy - 2.0), r + Vec2::new(208.0, dy + 28.0)), &l);
            let t = cx.c.spawn((Text::new(""), font.clone(), TextColor(LABEL), TextLayout::new(Justify::Right, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(bx))).id();
            cx.c.spawn((TextSpan::new(data.text(BASE, label)), font.clone(), TextColor(LABEL), ChildOf(t)));
            cx.c.spawn((TextSpan::new(value), font, TextColor(if label == "t_Cost" && cost.is_some_and(|c| c > stats.runes) { RED } else { PALE }), ChildOf(t)));
        }
        y = r.y + 72.0 + 5.0;
    }
    // the levels (`j_pow_powerDetails_scrollView`'s `level1_mc`, `level2_mc`, 20 apart): LEVEL,
    // its badge (I, II: lit once acquired), "(acquired)", and the level's words (`RPG.int`'s
    // `<power>_<level>_Description`), pale once acquired, else grey; the wheel scrolls them
    let rect = Rect::from_corners(Vec2::new(at.x - 200.0 + 1.65, y), Vec2::new(at.x - 200.0 + 281.0, at.y + 550.0 + 120.0));
    let col = list_box(cx, rect);
    let s = cx.st.s;
    let level_word = data.text(J, "t_Level").to_uppercase();
    for i in 1..=2u8 {
        let on = lvl >= i;
        let colour = if on { PALE } else { LABEL };
        let mut desc = data.text(RPG, &format!("{}_{i}_Description", rpg_key(name)));
        if desc.is_empty() && i == 1 {
            // (a game data cooked before `RPG.int`: the power's tutorial)
            desc = (0..3).map(|k| data.text(crate::journal::NOTE, &format!("t_tuto_{note}_{k}"))).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n");
        }
        let head = cx.c.spawn((Node { align_items: AlignItems::Center, height: Val::Px(40.0 * s), flex_shrink: 0.0, margin: UiRect::top(Val::Px(if i > 1 { 20.0 * s } else { 0.0 })), ..default() }, Pickable::IGNORE, ChildOf(col))).id();
        let lt = Look::title(25.0, colour).flat();
        let font = cx.font(&lt);
        cx.c.spawn((Text::new(level_word.clone()), font, TextColor(colour), Node { margin: UiRect::left(Val::Px(2.0 * s)), ..default() }, Pickable::IGNORE, ChildOf(head)));
        // the badge (`ic_mc`, 40 square, its middle 20 past the word)
        let badge = cx.c.spawn((Node { width: Val::Px(40.0 * s), height: Val::Px(40.0 * s), flex_shrink: 0.0, justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }, Pickable::IGNORE, ChildOf(head))).id();
        let tl = cx.tl.clone();
        let mut fc = FlashClip::new(MOVIE, tl.clone(), Clip::new(&tl, 151)).real();
        fc.scale = s;
        fc.clip.goto_label(&tl, if on { "on" } else { "off" }, false);
        cx.c.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(20.0 * s), top: Val::Px(20.0 * s), ..default() }, Pickable::IGNORE, ChildOf(badge)));
        let bt = Look::title(21.0, if on { DARK } else { ROW }).flat();
        let font = cx.font(&bt);
        cx.c.spawn((Text::new(if i == 1 { "I" } else { "II" }), font, TextColor(bt.color), Pickable::IGNORE, ChildOf(badge)));
        if on {
            let l = Look::normal(22.0, PALE).nowrap();
            let font = cx.font(&l);
            cx.c.spawn((Text::new(format!("({})", data.text(J, "t_Acquired").to_lowercase())), font, TextColor(PALE), Pickable::IGNORE, ChildOf(head)));
        }
        let l = Look::normal(23.0, colour);
        let font = cx.font(&l);
        cx.c.spawn((Text::new(crate::kismet::clean_text(&desc)), font, TextColor(colour), TextLayout::new(Justify::Left, LineBreak::WordBoundary), Node { width: Val::Px(278.0 * s), margin: UiRect::new(Val::Px(2.0 * s), Val::ZERO, Val::Px(-3.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
    }
}

/// The section of `RPG.int` with the powers' words.
const RPG: &str = "RPG.DisUISelectionType";

/// A power's name in `RPG.int` (Wind Blast is `WindBlast` there).
fn rpg_key(name: &str) -> &str {
    match name {
        "Windblast" => "WindBlast",
        n => n,
    }
}

/// The objectives' selection brackets fitted to the chosen row: corners at its edges.
fn sel_corners(mut q: Query<(&SelCorners, &ChildOf, &mut FlashClip)>, rows: Query<&ComputedNode>) {
    for (sc, parent, mut fc) in &mut q {
        let Ok(row) = rows.get(parent.parent()) else { continue };
        let s = fc.scale.max(1e-3);
        let h = row.size().y * row.inverse_scale_factor() / s;
        let w = sc.0;
        if let Some(parts) = fc.clip.child_mut("parts_mc") {
            for (name, x, y) in [("upLeft_mc", 0.0, -h * 0.5), ("upRight_mc", w, -h * 0.5), ("downLeft_mc", 0.0, h * 0.5), ("downRight_mc", w, h * 0.5)] {
                if let Some((m, _)) = parts.placed_mut(name) {
                    m[4] = x;
                    m[5] = y;
                }
            }
        }
    }
}

/// A scrolled list keeps its chosen row in view (`ScrollBarList`'s rule: the row's edge lined up
/// with the mask's).
fn keep_selected_in_view(mut boxes: Query<(&ComputedNode, &UiGlobalTransform, &mut ScrollPosition), With<JScroll>>, rows: Query<(&ComputedNode, &UiGlobalTransform), (With<JSelRow>, Changed<UiGlobalTransform>)>) {
    for (rn, rt) in &rows {
        for (bn, bt, mut sp) in &mut boxes {
            let (bh, rh) = (bn.size().y, rn.size().y);
            let (btop, rtop) = (bt.translation.y - bh * 0.5, rt.translation.y - rh * 0.5);
            let isf = bn.inverse_scale_factor();
            if rtop + rh > btop + bh + 1.0 {
                sp.y += (rtop + rh - (btop + bh)) * isf;
            } else if rtop < btop - 1.0 {
                sp.y -= (btop - rtop) * isf;
            }
        }
    }
}

/// The wheel scrolls the page's lists.
fn wheel_scroll(mut wheel: MessageReader<bevy::input::mouse::MouseWheel>, mut boxes: Query<(&ComputedNode, &mut ScrollPosition), With<JScroll>>) {
    let dy: f32 = wheel.read().map(|w| if w.unit == bevy::input::mouse::MouseScrollUnit::Line { w.y * 60.0 } else { w.y }).sum();
    if dy == 0.0 {
        return;
    }
    for (n, mut sp) in &mut boxes {
        let max = (n.content_size().y - n.size().y).max(0.0) * n.inverse_scale_factor();
        sp.y = (sp.y - dy).clamp(0.0, max);
    }
}
