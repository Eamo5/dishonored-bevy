//! Stores (Piero's workshop, Griff's black market): the level scripts open them
//! (`DisSeqAct_OpenCraftsmanStore`); items, prices and prerequisites are the original
//! `DisTweaks_Store` data, drawn on the shop movie's art with its item illustrations.

use crate::audio::PostEvent;
use crate::gamedata::Data;
use crate::gameplay::{HudMessages, PlayerStats};
use crate::hud::Paused;
use crate::ui_fonts::UiFonts;
use crate::ui_images::UiImages;
use crate::GameState;
use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::window::{CursorGrabMode, CursorOptions};
use dhcook::format::StoreItem;

pub struct StorePlugin;

impl Plugin for StorePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<OpenStore>()
            .init_resource::<Store>()
            .add_systems(OnEnter(GameState::InGame), |mut s: ResMut<Store>| *s = Store::default())
            .add_systems(Update, (open, input, refresh).chain().run_if(in_state(GameState::InGame)));
    }
}

/// Open a store by its tweak object's name (`Twk_Store_Craftsman`).
#[derive(Message, Clone)]
pub struct OpenStore(pub String);

#[derive(Resource, Default)]
pub struct Store {
    /// the store shown (index into the game data's stores)
    pub open: Option<usize>,
    /// its tab (purchases, upgrades)
    tab: u8,
    selected: usize,
    dirty: bool,
}

#[derive(Component)]
struct StoreRoot;

#[derive(Component)]
struct ItemRow(usize);

#[derive(Component)]
struct BuyButton;

#[derive(Component)]
struct CloseButton;

#[derive(Component)]
struct TabButton(u8);

const MOVIE: &str = "Shop";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);

const S: &str = "DisGFxMoviePlayerStore_Texts";

/// Is a prerequisite met: the crossbow and sword (weapons), upgrades bought, blueprints found.
fn has(stats: &PlayerStats, req: &str) -> bool {
    if req.starts_with("Twk_Inv_") {
        return stats.weapons;
    }
    stats.upgrades.iter().any(|u| u.eq_ignore_ascii_case(req))
}

/// Shown at all (blueprint-gated upgrades appear once the blueprint is found).
fn visible(stats: &PlayerStats, it: &StoreItem) -> bool {
    it.requires.iter().filter(|r| r.starts_with("BP_")).all(|r| has(stats, r))
}

fn owned(stats: &PlayerStats, it: &StoreItem) -> bool {
    it.item.starts_with("Twk_Upgrade_") && stats.upgrades.iter().any(|u| *u == it.item)
}

fn open(
    mut requests: MessageReader<OpenStore>,
    data: Res<Data>,
    mut store: ResMut<Store>,
    mut paused: ResMut<Paused>,
    mut cursor: Single<&mut CursorOptions>,
    mut msgs: ResMut<HudMessages>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let Some(r) = requests.read().last().cloned() else { return };
    let Some(i) = data.0.stores.iter().position(|s| s.id.eq_ignore_ascii_case(&r.0)).or_else(|| {
        // a store variant of the same shop (Piero's at a later visit)
        let base = r.0.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_');
        data.0.stores.iter().position(|s| s.id.starts_with(base))
    }) else {
        warn!("store {} not cooked", r.0);
        msgs.push("The store is closed");
        return;
    };
    sfx.write(PostEvent::named("UI_S_OpenShop", None));
    store.open = Some(i);
    store.tab = 0;
    store.selected = 0;
    store.dirty = true;
    paused.0 = true;
    cursor.visible = true;
    cursor.grab_mode = CursorGrabMode::None;
}

/// The items of a tab, in the grid's order: (item index, the section header before it).
fn tab_items(def: &dhcook::format::StoreDef, stats: &PlayerStats, tab: u8, tabs: bool) -> Vec<usize> {
    (0..def.items.len()).filter(|&i| visible(stats, &def.items[i]) && (!tabs || (def.items[i].section > 0) == (tab == 1))).collect()
}

/// Whether the store shows its two tabs (`UpgradesTabAvailable`): it sells upgrades.
fn has_tabs(def: &dhcook::format::StoreDef) -> bool {
    def.items.iter().any(|it| it.section > 0)
}

#[allow(clippy::too_many_arguments)]
fn input(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    data: Res<Data>,
    mut store: ResMut<Store>,
    mut stats: ResMut<PlayerStats>,
    mut paused: ResMut<Paused>,
    mut cursor: Single<&mut CursorOptions>,
    (rows, tabs_q): (Query<(&Interaction, &ItemRow), Changed<Interaction>>, Query<(&Interaction, &TabButton), Changed<Interaction>>),
    buy: Query<&Interaction, (With<BuyButton>, Changed<Interaction>)>,
    close: Query<&Interaction, (With<CloseButton>, Changed<Interaction>)>,
    roots: Query<Entity, With<StoreRoot>>,
    (mut sfx, mut msgs, attrs): (MessageWriter<PostEvent>, ResMut<HudMessages>, Res<crate::gamedata::Attrs>),
    scripted: Option<Res<crate::script::Scripted>>,
) {
    let Some(si) = store.open else { return };
    let def = &data.0.stores[si];
    let tabs = has_tabs(def);
    let pointer = scripted.is_none();
    // the tabs: clicked, or Q / E (`LB` / `RB`)
    let mut tab = None;
    for (i, t) in tabs_q.iter().filter(|_| pointer) {
        if *i == Interaction::Pressed && t.0 != store.tab {
            tab = Some(t.0);
        }
    }
    if tabs && (keys.just_pressed(KeyCode::KeyQ) || keys.just_pressed(KeyCode::KeyE) || keys.just_pressed(KeyCode::Tab)) {
        tab = Some(1 - store.tab);
    }
    if let Some(t) = tab {
        store.tab = t;
        store.selected = tab_items(def, &stats, t, tabs).first().copied().unwrap_or(0);
        store.dirty = true;
        sfx.write(PostEvent::named("UI_ChangeMainTab", None));
        return;
    }
    let list = tab_items(def, &stats, store.tab, tabs);
    let mut clicked = false;
    for (i, r) in rows.iter().filter(|_| pointer) {
        if matches!(i, Interaction::Pressed | Interaction::Hovered) && store.selected != r.0 {
            store.selected = r.0;
            store.dirty = true;
        }
        clicked |= *i == Interaction::Pressed;
    }
    // the grid: 4 across
    let pos = list.iter().position(|&i| i == store.selected).unwrap_or(0) as i32;
    let n = list.len().max(1) as i32;
    let step = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
        1
    } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
        -1
    } else if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
        4
    } else if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
        -4
    } else {
        0
    };
    if step != 0 {
        let next = (pos + step).clamp(0, n - 1);
        store.selected = list.get(next as usize).copied().unwrap_or(0);
        store.dirty = true;
        sfx.write(PostEvent::named("UI_Move", None));
    }
    let quit = keys.just_pressed(KeyCode::Escape) || close.iter().filter(|_| pointer).any(|i| *i == Interaction::Pressed);
    if quit {
        store.open = None;
        for e in &roots {
            commands.entity(e).despawn();
        }
        paused.0 = false;
        cursor.visible = false;
        cursor.grab_mode = CursorGrabMode::Locked;
        sfx.write(PostEvent::named("UI_S_CloseShop", None));
        return;
    }
    if !(buy.iter().filter(|_| pointer).any(|i| *i == Interaction::Pressed) || keys.just_pressed(KeyCode::Enter) || clicked && step == 0 && rows.is_empty()) {
        return;
    }
    let Some(it) = def.items.get(store.selected).cloned() else { return };
    if owned(&stats, &it) || !it.requires.iter().all(|r| has(&stats, r)) {
        sfx.write(PostEvent::named("UI_Failure", None));
        return;
    }
    if at_capacity(&stats, &attrs, &it.item) {
        sfx.write(PostEvent::named("UI_Failure", None));
        msgs.push("You cannot carry any more of this item");
        return;
    }
    if stats.coins < it.coins {
        sfx.write(PostEvent::named("UI_Failure", None));
        msgs.push("Not enough coins");
        return;
    }
    stats.coins -= it.coins;
    let q = it.quantity.max(1);
    match it.item.as_str() {
        id if crate::gadgets::ammo_type(id).is_some() => { crate::gadgets::give_ammo(&mut stats, &attrs, crate::gadgets::ammo_type(id).unwrap(), q); }
        "Elixir_Mana_twk" => { stats.give_elixirs(true, q, attrs.elixir_capacity(true)); }
        "Elixir_Health_twk" => { stats.give_elixirs(false, q, attrs.elixir_capacity(false)); }
        "Rune_twk" => stats.runes += q,
        id if id.starts_with("Twk_Upgrade_") => stats.upgrades.push(id.to_string()),
        id if id.starts_with("BP_") => {
            let base = id.trim_end_matches("_twk");
            stats.upgrades.push(format!("{base}_AbsItm"));
        }
        id => *stats.items.entry(id.to_string()).or_default() += q,
    }
    sfx.write(PostEvent::named("UI_S_ItemBuy", None));
    store.dirty = true;
}

/// A store item's picture (`items/<icon>_Large`): its own, or its upgrade's.
fn icon_of<'a>(data: &'a Data, it: &'a StoreItem) -> &'a str {
    if !it.icon.is_empty() {
        return &it.icon;
    }
    data.0.upgrades.iter().find(|u| u.id == it.item).map(|u| u.icon.as_str()).filter(|i| !i.is_empty()).unwrap_or("Rune")
}

/// The purchase handler and help bar share the same inventory-capacity check.
fn at_capacity(stats: &PlayerStats, attrs: &crate::gamedata::Attrs, item: &str) -> bool {
    carried(stats, attrs, item).is_some_and(|(count, maximum)| maximum.is_some_and(|maximum| count >= maximum))
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn shop_capacity_matches_grants_for_every_ammunition_item() {
        let mut stats = PlayerStats::default();
        let mut attrs = crate::gamedata::Attrs::default();
        for item in ["Bullets_Store_Ammo_twk", "ExplosiveBullets_Ammo_twk", "Bolt_Ammo_twk", "Bolts_Ammo_twk", "SleepDart_Ammo_twk", "Flare_Ammo_twk", "SpringRazor_Ammo_WithItem_twk", "Grenade_Ammo_WithItem_twk", "StickyGrenade_Ammo_WithItem_twk"] {
            let ty = crate::gadgets::ammo_type(item).unwrap();
            let cap = attrs.ammo_capacity[ty as usize];
            *crate::gadgets::ammo_mut(&mut stats, ty, true).unwrap() = cap - 1;
            assert!(!at_capacity(&stats, &attrs, item));
            crate::gadgets::give_ammo(&mut stats, &attrs, ty, 5);
            assert_eq!(carried(&stats, &attrs, item), Some((cap, Some(cap))));
            assert!(at_capacity(&stats, &attrs, item));
            attrs.ammo_capacity[ty as usize] += 1;
            assert!(!at_capacity(&stats, &attrs, item));
        }
    }
}

/// How many of a store item Corvo carries, and the most he may (`Owned: n/max`).
fn carried(stats: &PlayerStats, attrs: &crate::gamedata::Attrs, item: &str) -> Option<(u32, Option<u32>)> {
    if let Some(ty) = crate::gadgets::ammo_type(item) {
        return Some((crate::gadgets::ammo_count(stats, ty), Some(attrs.ammo_capacity[ty as usize])));
    }
    Some(match item {
        "Elixir_Mana_twk" => (stats.mana_elixirs, Some(attrs.elixir_capacity(true))),
        "Elixir_Health_twk" => (stats.health_elixirs, Some(attrs.elixir_capacity(false))),
        "Rune_twk" => (stats.runes, None),
        id if id.starts_with("Twk_Upgrade_") || id.starts_with("BP_") => return None,
        id => (stats.items.get(id).copied().unwrap_or(0), None),
    })
}

/// The store as `UI_Shop` draws it: the drifting backdrop (`wh_mainBkgd`), the list panel
/// (`sh_listPanelWithTabs` at (465,120), or `sh_listPanel` at (465,55) when it sells no
/// upgrades) with its two tabs (`lib_mTabs_tab`: PURCHASES, UPGRADES), the cards 4 across
/// (`sh_itemList_item`: 161 x 175 apart, tilted, their price tags; out of reach crossed out,
/// a missing prerequisite padlocked; the chosen one grown), the upgrades under their section
/// names, and the chosen item's details (`sh_details` at (1005.95,67.7): its picture, how many
/// are carried, its cost and the coins carried, its name and words).
#[allow(clippy::too_many_arguments)]
fn refresh(
    mut commands: Commands,
    mut store: ResMut<Store>,
    data: Res<Data>,
    stats: Res<PlayerStats>,
    attrs: Res<crate::gamedata::Attrs>,
    fonts: Res<UiFonts>,
    (mut ui, mut images): (ResMut<UiImages>, ResMut<Assets<Image>>),
    roots: Query<Entity, With<StoreRoot>>,
    (mut timelines, window): (ResMut<crate::flash::MovieTimelines>, Query<&Window, With<bevy::window::PrimaryWindow>>),
    mut last_size: Local<Vec2>,
) {
    use crate::flash::{Clip, FlashClip, Mat};
    use bevy::text::LineBreak;
    let Some(si) = store.open else { return };
    let Ok(w) = window.single() else { return };
    let size = Vec2::new(w.width(), w.height());
    if *last_size != size {
        *last_size = size;
        store.dirty = true;
    }
    if !store.dirty {
        return;
    }
    store.dirty = false;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let Some(tl) = timelines.get(MOVIE) else { return };
    let lib = timelines.get("lib");
    let def = &data.0.stores[si];
    let tabs = has_tabs(def);
    let list = tab_items(def, &stats, store.tab, tabs);
    if !list.contains(&store.selected) {
        store.selected = list.first().copied().unwrap_or(0);
    }
    let sel = store.selected;
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (size - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let root = commands
        .spawn((StoreRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(80), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
        .id();
    let apply = |m: &Mat, p: Vec2| Vec2::new(m[0] * p.x + m[2] * p.y + m[4], m[1] * p.x + m[3] * p.y + m[5]);
    let title = |size: f32| TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(size * s), ..default() };
    let normal = |size: f32| TextFont { font_size: FontSize::Px(size * s), ..default() };
    // a clip on the stage (under a parent placed at `origin`)
    let clip = |commands: &mut Commands, parent: Entity, origin: Vec2, tl: &std::sync::Arc<dhcook::format::Timelines>, movie: &str, sym: &str, at: Vec2, setup: &mut dyn FnMut(&mut FlashClip)| {
        let c = Clip::export(tl, sym)?;
        let mut fc = FlashClip::new(movie, tl.clone(), c).real();
        fc.scale = s;
        setup(&mut fc);
        let p = if parent == root { off + at * s } else { (at - origin) * s };
        Some(commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(parent))).id())
    };
    // words in a box (stage), turned
    let words = |commands: &mut Commands, parent: Entity, origin: Vec2, rect: Rect, turn: f32, text: String, font: TextFont, color: Color, justify: Justify, wrap: bool, tall: bool| {
        let p = if parent == root { off + rect.min * s } else { (rect.min - origin) * s };
        let bx = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(p.x),
                    top: Val::Px(p.y),
                    width: Val::Px(rect.width() * s),
                    height: Val::Px(rect.height() * s),
                    align_items: AlignItems::Center,
                    justify_content: match justify {
                        Justify::Right => JustifyContent::FlexEnd,
                        Justify::Center => JustifyContent::Center,
                        _ => JustifyContent::FlexStart,
                    },
                    ..default()
                },
                UiTransform { rotation: Rot2::degrees(turn), ..default() },
                Pickable::IGNORE,
                ChildOf(parent),
            ))
            .id();
        let t = commands.spawn((Text::new(text), font, TextColor(color), TextLayout::new(justify, if wrap { LineBreak::WordBoundary } else { LineBreak::NoWrap }), Node { max_width: Val::Px(rect.width() * s), ..default() }, Pickable::IGNORE, ChildOf(bx))).id();
        if tall {
            commands.entity(t).insert(UiTransform { scale: Vec2::new(1.0, 1.2), ..default() });
        }
        bx
    };
    // the backdrop, drifting
    if let Some(e) = clip(&mut commands, root, Vec2::ZERO, &tl, MOVIE, "wh_mainBkgd", Vec2::new(640.0, 360.0), &mut |_| {}) {
        commands.queue(move |w: &mut World| {
            let bg = w.get::<FlashClip>(e).and_then(|fc| crate::animbg::AnimatedBackground::new(fc, ""));
            if let (Some(bg), Ok(mut em)) = (bg, w.get_entity_mut(e)) {
                em.insert(bg);
            }
        });
    }
    // the list's panel
    let panel_y = if tabs { 120.0 } else { 55.0 };
    let mask_h = if tabs { 550.0 } else { 610.0 };
    clip(&mut commands, root, Vec2::ZERO, &tl, MOVIE, if tabs { "sh_listPanelWithTabs" } else { "sh_listPanel" }, Vec2::new(465.0, panel_y), &mut |fc| {
        fc.set_visible("_list_mc.maskContent", false);
        fc.goto("_tabsBkgd_mc", "loop", true);
    });
    // the tabs
    if let (true, Some(lib)) = (tabs, lib.as_ref()) {
        for (t, (x, key)) in [(334.0, "t_Purchases"), (596.0, "t_Upgrades")].into_iter().enumerate() {
            let on = t as u8 == store.tab;
            clip(&mut commands, root, Vec2::ZERO, lib, "lib", "lib_mTabs_tab", Vec2::new(x, 87.55), &mut |fc| {
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
            let mid = Vec2::new(x, 87.55 + dy + k * -4.0);
            words(&mut commands, root, Vec2::ZERO, Rect::from_center_size(mid, Vec2::new(280.0, 40.0)), 0.0, data.text(S, key).to_uppercase(), title(20.0 * k), if on { Color::srgb(27.0 / 255.0, 30.0 / 255.0, 31.0 / 255.0) } else { PALE }, Justify::Center, false, true);
            let e = commands.spawn((TabButton(t as u8), Button, Node { position_type: PositionType::Absolute, left: Val::Px(off.x + (x - 120.0) * s), top: Val::Px(off.y + (87.55 - 40.0) * s), width: Val::Px(240.0 * s), height: Val::Px(76.0 * s), ..default() }, ChildOf(root))).id();
            let _ = e;
        }
    }
    // the cards: their places in the list (`MovieClipCreator`), sections first in the upgrades
    let list_at = Vec2::new(465.0 - 342.0, panel_y);
    let mut cells: Vec<(Option<usize>, Vec2)> = Vec::new();
    let mut heads: Vec<(String, f32)> = Vec::new();
    let mut y = 0.0;
    let mut col = 0;
    let mut section = -1;
    let mut row_y = 95.0;
    for (k, &i) in list.iter().enumerate() {
        let it = &def.items[i];
        if tabs && store.tab == 1 && it.section != section {
            // a new section: the last row's ghosts, then its name
            if section >= 0 {
                while col % 4 != 0 {
                    cells.push((None, Vec2::new(85.5 + 161.0 * col as f32, row_y)));
                    col += 1;
                }
                y = row_y + 95.0;
            }
            section = it.section;
            heads.push((def.sections.get(section.max(0) as usize).cloned().unwrap_or_default(), y));
            row_y = y + 127.7;
            col = 0;
        } else if k > 0 && col % 4 == 0 {
            row_y += 175.0;
        }
        if k == 0 && heads.is_empty() {
            row_y = 95.0;
        }
        cells.push((Some(i), Vec2::new(85.5 + 161.0 * (col % 4) as f32, row_y)));
        col += 1;
        if col % 4 == 0 {
            col = 0;
        }
    }
    // ghosts: to a full last row, at least 12
    while cells.len() < 12 || col % 4 != 0 {
        if col % 4 == 0 && !cells.is_empty() && cells.len() >= 12 {
            break;
        }
        if col % 4 == 0 && !cells.is_empty() {
            row_y += 175.0;
        }
        cells.push((None, Vec2::new(85.5 + 161.0 * (col % 4) as f32, row_y)));
        col = (col + 1) % 4;
    }
    // scrolled to keep the chosen card in the mask
    let sel_y = cells.iter().find(|c| c.0 == Some(sel)).map(|c| c.1.y).unwrap_or(95.0);
    let scroll = (sel_y + 95.0 - mask_h).max(0.0);
    let clipped = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(off.x + (list_at.x - 40.0) * s),
                top: Val::Px(off.y + list_at.y * s),
                width: Val::Px(760.0 * s),
                height: Val::Px(mask_h * s),
                overflow: Overflow::clip(),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    let origin = Vec2::new(list_at.x - 40.0, list_at.y);
    for (name, hy) in &heads {
        let at = list_at + Vec2::new(0.0, hy - scroll);
        clip(&mut commands, clipped, origin, &tl, MOVIE, "sh_list_sectionName", at, &mut |_| {});
        words(&mut commands, clipped, origin, Rect::from_corners(at + Vec2::new(2.0, 4.0), at + Vec2::new(682.0, 33.0)), 0.0, name.to_uppercase(), title(21.0), Color::srgb(185.0 / 255.0, 187.0 / 255.0, 171.0 / 255.0), Justify::Left, false, true);
    }
    for (k, (item, c)) in cells.iter().enumerate() {
        let at = list_at + Vec2::new(c.x, c.y - scroll);
        if at.y < list_at.y - 100.0 || at.y > list_at.y + mask_h + 100.0 {
            continue;
        }
        let Some(i) = *item else {
            clip(&mut commands, clipped, origin, &tl, MOVIE, "sh_itemList_item", at, &mut |fc| {
                fc.goto("", "disabled", true);
                for part in ["lock_mc", "lockIc_mc", "price_mc"] {
                    fc.set_visible(part, false);
                }
                let (sn, cs) = ((k as f32 * 2.3).sin() * 5.0).to_radians().sin_cos();
                fc.m = [cs, sn, -sn, cs, 0.0, 0.0];
            });
            continue;
        };
        let it = &def.items[i];
        let on = i == sel;
        let have = owned(&stats, it);
        let missing = it.requires.iter().find(|r| !has(&stats, r)).cloned();
        let available = !have && missing.is_none();
        let turn = [-2.5, -1.5, -0.5, 0.5, 1.5, 2.5][(i * 7 + 3) % 6];
        let sc = if on { 1.1 } else { 1.0 };
        let (sn, cs) = (turn as f32).to_radians().sin_cos();
        let m: Mat = [cs * sc, sn * sc, -sn * sc, cs * sc, 0.0, 0.0];
        clip(&mut commands, clipped, origin, &tl, MOVIE, "sh_itemList_item", at, &mut |fc| {
            fc.m = m;
            let t2 = fc.tl.clone();
            for part in ["slotOff_mc", "slotOn_mc"] {
                if let Some(b) = fc.clip.child_mut(part) {
                    b.goto(&t2, i % 3, false);
                }
            }
            if let Some(b) = fc.clip.child_mut("compass_mc") {
                b.goto(&t2, i % 5, false);
            }
            if on {
                fc.goto("", "over", true);
            }
            match (available, &missing) {
                (true, _) => {
                    fc.set_visible("lock_mc", false);
                    fc.set_visible("lockIc_mc", false);
                }
                (false, Some(_)) => {
                    fc.set_visible("price_mc", false);
                    if let Some(l) = fc.clip.child_mut("lock_mc") {
                        l.goto(&t2, 1, false);
                    }
                }
                (false, None) => {
                    fc.set_visible("lockIc_mc", false);
                    fc.goto("price_mc", "stop_lock", false);
                }
            }
        });
        // its picture (138 x 100 at (0,-14.5), dimmed when out of reach)
        if let Some((h, size)) = ui.file(&mut images, "items", &format!("{}_Large", icon_of(&data, it))) {
            let bx = Vec2::new(138.0, 100.0) * sc;
            let k = (bx.x / size.x.max(1.0)).min(bx.y / size.y.max(1.0));
            let (iw, ih) = (size.x * k, size.y * k);
            let c = at + apply(&m, Vec2::new(0.0, -14.5));
            let p = (c - Vec2::new(iw, ih) * 0.5 - origin) * s;
            let tint = if available { Color::WHITE } else { Color::srgb(0.5, 0.5, 0.5) };
            commands.spawn((
                ImageNode::new(h).with_color(tint),
                Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(iw * s), height: Val::Px(ih * s), ..default() },
                UiTransform { rotation: Rot2::degrees(turn as f32), ..default() },
                Pickable::IGNORE,
                ChildOf(clipped),
            ));
        }
        // its name, two lines under it; its price on the tag
        let name_c = at + apply(&m, Vec2::new(0.0, 38.95 + 23.3));
        words(&mut commands, clipped, origin, Rect::from_center_size(name_c, Vec2::new(149.6, 46.6) * sc), turn as f32, it.name.clone(), normal(19.0 * sc), if on { Color::srgb(200.0 / 255.0, 204.0 / 255.0, 196.0 / 255.0) } else { Color::srgb(133.0 / 255.0, 142.0 / 255.0, 125.0 / 255.0) }, Justify::Center, true, false);
        if missing.is_none() {
            let price_c = at + apply(&m, Vec2::new(-76.25 + 22.85 + 22.0, 16.3 + 12.2));
            let col = if !available { Color::srgb(140.0 / 255.0, 25.0 / 255.0, 24.0 / 255.0) } else if on { DARK } else { PALE };
            words(&mut commands, clipped, origin, Rect::from_center_size(price_c, Vec2::new(44.0, 24.3) * sc), turn as f32, it.coins.to_string(), normal(19.0 * sc), col, Justify::Left, false, false);
        }
        let hp = (at - Vec2::new(80.0, 88.0) - origin) * s;
        commands.spawn((ItemRow(i), Button, Node { position_type: PositionType::Absolute, left: Val::Px(hp.x), top: Val::Px(hp.y), width: Val::Px(160.0 * s), height: Val::Px(176.0 * s), ..default() }, ChildOf(clipped)));
    }
    if list.is_empty() {
        let c = list_at + Vec2::new(360.0, 274.0);
        clip(&mut commands, root, Vec2::ZERO, &tl, MOVIE, "sh_itemsList_empty", c, &mut |_| {});
        words(&mut commands, root, Vec2::ZERO, Rect::from_center_size(c, Vec2::new(360.0, 34.0)), -2.5, data.text("DisGFxMoviePlayerBase_Texts", "t_EmptyList"), normal(26.0), Color::srgb(168.0 / 255.0, 180.0 / 255.0, 158.0 / 255.0), Justify::Center, false, false);
    }
    // the chosen item's details
    let d = Vec2::new(1005.95, 67.7);
    let it = def.items.get(sel).filter(|_| !list.is_empty());
    let carried_q = it.and_then(|it| carried(&stats, &attrs, &it.item));
    let missing = it.and_then(|it| it.requires.iter().find(|r| !has(&stats, r)).cloned());
    let affordable = it.is_some_and(|it| stats.coins >= it.coins);
    clip(&mut commands, root, Vec2::ZERO, &tl, MOVIE, "sh_details", d, &mut |fc| {
        if carried_q.is_none() {
            fc.set_visible("_quantity_mc", false);
        } else if carried_q.is_some_and(|(q, m)| m.is_some_and(|m| q >= m)) {
            fc.goto("_quantity_mc", "locked", false);
        }
        if it.is_none() {
            fc.set_visible("_cost_mc", false);
        } else if missing.is_some() {
            fc.goto("_cost_mc", "unavailable", false);
        } else if !affordable {
            fc.goto("_cost_mc", "locked", false);
        }
    });
    let Some(it) = it else { return };
    if let Some((h, size)) = ui.file(&mut images, "items", &format!("{}_Large", icon_of(&data, it))) {
        let bx = 294.4 * 1.15;
        let k = (bx / size.x.max(1.0)).min(bx / size.y.max(1.0));
        let (iw, ih) = (size.x * k, size.y * k);
        let c = d + Vec2::new(0.0, 130.4);
        commands.spawn((ImageNode::new(h), Node { position_type: PositionType::Absolute, left: Val::Px(off.x + (c.x - iw * 0.5) * s), top: Val::Px(off.y + (c.y - ih * 0.5) * s), width: Val::Px(iw * s), height: Val::Px(ih * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
    }
    let label = Color::srgb(168.0 / 255.0, 180.0 / 255.0, 158.0 / 255.0);
    // "Owned: n/max"
    if let Some((q, m)) = carried_q {
        let r = Rect::from_corners(d + Vec2::new(-170.0, 241.75), d + Vec2::new(100.0, 283.75));
        let bx = words(&mut commands, root, Vec2::ZERO, r, 0.0, String::new(), normal(24.0), label, Justify::Left, false, false);
        let t = commands.spawn((Text::new(""), normal(24.0), TextColor(label), Pickable::IGNORE, ChildOf(bx))).id();
        commands.spawn((TextSpan::new(data.text(S, "t_ItemQuantityOwned")), normal(24.0), TextColor(label), ChildOf(t)));
        commands.spawn((TextSpan::new(match m {
            Some(m) => format!("{q}/{m}"),
            None => q.to_string(),
        }), normal(24.0), TextColor(PALE), ChildOf(t)));
    }
    // "Cost: n" / "You have: n", or the prerequisite
    let r = Rect::from_corners(d + Vec2::new(177.95 - 359.95 - 2.0, 326.8 - 25.6 - 2.0), d + Vec2::new(177.95 - 359.95 + 306.0, 326.8 - 25.6 + 53.0));
    match &missing {
        Some(m) => {
            let pretty = data.0.upgrades.iter().find(|u| u.id == *m).map(|u| u.name.clone()).unwrap_or_else(|| m.trim_start_matches("Twk_Inv_").trim_start_matches("BP_").trim_end_matches("_AbsItm").trim_end_matches("Corvo").to_string());
            words(&mut commands, root, Vec2::ZERO, r, 0.0, data.text(S, "t_PrerequisiteItem").replace("§ITEM§", &pretty), normal(23.0), PALE, Justify::Right, true, false);
        }
        None => {
            let bx = words(&mut commands, root, Vec2::ZERO, r, 0.0, String::new(), normal(23.0), label, Justify::Right, false, false);
            let t = commands.spawn((Text::new(""), normal(23.0), TextColor(label), TextLayout::new(Justify::Right, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(bx))).id();
            commands.spawn((TextSpan::new(data.text("DisGFxMoviePlayerBase_Texts", "t_Cost")), normal(23.0), TextColor(label), ChildOf(t)));
            commands.spawn((TextSpan::new(format!("{}\n", it.coins)), normal(23.0), TextColor(PALE), ChildOf(t)));
            commands.spawn((TextSpan::new(data.text("DisGFxMoviePlayerBase_Texts", "t_YouHave")), normal(23.0), TextColor(label), ChildOf(t)));
            // (short of it, the banner turns red: `_cost_mc` "locked"; the coins stay legible on it)
            commands.spawn((TextSpan::new(stats.coins.to_string()), normal(23.0), TextColor(PALE), ChildOf(t)));
        }
    }
    // its name and words (the scroll view: 327 x 185)
    let at = d + Vec2::new(-179.65 + 1.65, 360.2 + 0.7);
    let col = commands
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(off.x + at.x * s), top: Val::Px(off.y + at.y * s), width: Val::Px(327.0 * s), height: Val::Px(185.0 * s), flex_direction: FlexDirection::Column, overflow: Overflow::clip(), ..default() },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    commands.spawn((Text::new(it.name.to_uppercase()), title(26.0), TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::WordBoundary), UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Node { width: Val::Px(320.9 * s), margin: UiRect::new(Val::Px(4.2 * s), Val::ZERO, Val::Px(4.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
    commands.spawn((Text::new(crate::gamedata::readable(&it.description)), normal(24.0), TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::WordBoundary), Node { width: Val::Px(320.9 * s), margin: UiRect::new(Val::Px(4.2 * s), Val::ZERO, Val::Px(8.0 * s), Val::ZERO), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
    // the help bar's: A purchase, B exit
    let p = off + Vec2::new(1184.0 - 520.0, 651.0 - 18.0) * s;
    let bar = commands.spawn((Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(520.0 * s), justify_content: JustifyContent::FlexEnd, column_gap: Val::Px(20.0 * s), ..default() }, Pickable::IGNORE, ChildOf(root))).id();
    let buy_words = data.text(S, if store.tab == 1 { "t_PurchaseUpgrade" } else { "t_Purchase" }).to_uppercase();
    commands.spawn((BuyButton, Button, ChildOf(bar))).with_child((Text::new(format!("[Enter]  {buy_words}")), title(22.0), TextColor(if missing.is_none() && affordable && !owned(&stats, it) && !at_capacity(&stats, &attrs, &it.item) { PALE } else { PALE.with_alpha(0.4) })));
    commands.spawn((CloseButton, Button, ChildOf(bar))).with_child((Text::new(format!("[Esc]  {}", data.text("DisGFxMoviePlayerBase_Texts", "t_Exit").to_uppercase())), title(22.0), TextColor(PALE)));
}
