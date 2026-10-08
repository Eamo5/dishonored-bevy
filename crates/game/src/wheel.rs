//! The Quick-access Wheel (`GBA_Wheel`, middle mouse held): the world slows to a crawl
//! (`m_fBendTimeFactor` 0.05) and the original `UI_PowerWheel` movie comes up: its backdrop
//! (`white_bkgd`, `glass_bkgd`), the wheel (`wheel_mc` at 640, 354: its circle, strokes and
//! the chosen item's illustration and name), and around it everything Corvo carries as
//! `WheelHandler` places it — the weapons (`weaponsOrder`: flare bolts, bolts, sleep darts,
//! the Heart, spring razors, grenades, sticky grenades, bullets, explosive bullets) centred
//! at the top and the powers (`powersOrder`: Windblast, Dark Vision, Blink, the Swarm, Bend
//! Time, Possession) on round from them, each `360 / count` degrees, its icon at 75% on the
//! 500-wide circle's rim, the separators between the two, the indicator turned to the one
//! pointed at. The mouse points; letting go equips it.

use crate::audio::PostEvent;
use crate::flash::{turn_scale, Clip, FlashClip, MovieTimelines};
use crate::gameplay::{PlayerStats, TimeControl};
use crate::powers::{Power, PowerEquipped, Powers};
use crate::ui_fonts::UiFonts;
use crate::ui_images::UiImages;
use crate::GameState;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct WheelPlugin;

impl Plugin for WheelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wheel>().add_systems(Update, (wheel, layout).chain().run_if(in_state(GameState::InGame)));
    }
}

#[derive(Resource, Default)]
pub struct Wheel {
    pub open: bool,
    /// where the mouse points (accumulated, pixels)
    aim: Vec2,
    /// the items and their angles (degrees clockwise from the top)
    items: Vec<(Power, f32)>,
    hover: Option<usize>,
    /// the separators' angles
    seps: Vec<f32>,
}

/// The original's time scale while the wheel is open.
pub const WHEEL_TIME: f32 = 0.05;

const MOVIE: &str = "powerwheel";
const WHEEL_AT: Vec2 = Vec2::new(640.0, 354.0);
/// half `wheel_diameter` (the circle's width, 500), the icons' scale and drawn width
const WHEEL_R: f32 = 250.0;
const IC_SCALE: f32 = 0.75;
const IC_W: f32 = 103.0;
/// the icon art's box within its disc
const IC_IMAGE: f32 = 100.0;
/// the chosen one's name and ammunition (`item_infos`: `name_txt`, `ammo_txt`), their middles
const NAME_AT: Vec2 = Vec2::new(0.0, 21.7);
const AMMO_AT: Vec2 = Vec2::new(0.0, 65.5);
const NAME_COLOR: Color = Color::srgb(0xe1 as f32 / 255.0, 0xf0 as f32 / 255.0, 0xd4 as f32 / 255.0);
const AMMO_COLOR: Color = Color::srgba(0xeb as f32 / 255.0, 1.0, 0xde as f32 / 255.0, 0.6);

/// `WheelHandler`'s orders.
const WEAPONS: [Power; 9] = [Power::IncendiaryBolt, Power::Crossbow, Power::SleepDart, Power::Heart, Power::SpringRazor, Power::Grenade, Power::StickyGrenade, Power::Pistol, Power::ExplosiveBullet];
const POWERS: [Power; 6] = [Power::Windblast, Power::DarkVision, Power::Blink, Power::DevouringSwarm, Power::BendTime, Power::Possess];

/// The items Corvo has and where each sits (`ComputeWeaponAngle` / `ComputePowerAngle`), and
/// the separators between the groups.
fn arrange(stats: &PlayerStats) -> (Vec<(Power, f32)>, Vec<f32>) {
    let w: Vec<Power> = WEAPONS.iter().copied().filter(|p| p.owned(stats)).collect();
    let p: Vec<Power> = POWERS.iter().copied().filter(|p| p.owned(stats)).collect();
    let (nw, np) = (w.len() as f32, p.len() as f32);
    if nw + np == 0.0 {
        return (Vec::new(), Vec::new());
    }
    let item = 360.0 / (nw + np);
    let (wr, pr) = (nw * item, np * item);
    let mut out = Vec::new();
    for (i, x) in w.iter().enumerate() {
        out.push((*x, (i as f32 / nw * wr + 0.5 * item - 0.5 * wr).rem_euclid(360.0)));
    }
    for (i, x) in p.iter().enumerate() {
        out.push((*x, (0.5 * wr + pr - (i as f32 / np * pr + 0.5 * item)).rem_euclid(360.0)));
    }
    let seps = if nw > 0.0 && np > 0.0 { vec![(-0.5 * wr).rem_euclid(360.0), 0.5 * wr] } else { Vec::new() };
    (out, seps)
}

/// The illustration (`item_schema`'s label) for an item.
fn schema(p: Power) -> &'static str {
    match p {
        Power::IncendiaryBolt | Power::Crossbow | Power::SleepDart => "crossbow",
        Power::Pistol | Power::ExplosiveBullet => "gun",
        Power::StickyGrenade => "sticky_grenade",
        Power::Grenade => "regular_grenade",
        Power::SpringRazor => "spring_razor",
        Power::Heart => "heart",
        Power::Empty => "default",
        _ => "outsider",
    }
}

fn is_power(p: Power) -> bool {
    POWERS.contains(&p)
}

#[derive(Component)]
struct WheelRoot;
#[derive(Component)]
struct Backdrop(Vec2);
#[derive(Component)]
struct WheelClip;
#[derive(Component)]
struct ItemIcon(usize);
#[derive(Component)]
struct ItemArt(usize);
#[derive(Component)]
struct ItemCount(usize);
#[derive(Component)]
struct Indicator;
#[derive(Component)]
struct Separator(usize);
#[derive(Component)]
struct NameText;
#[derive(Component)]
struct AmmoText;

#[allow(clippy::too_many_arguments)]
fn wheel(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    stats: Res<PlayerStats>,
    mut w: ResMut<Wheel>,
    mut powers: ResMut<Powers>,
    mut tc: ResMut<TimeControl>,
    fonts: Res<UiFonts>,
    (mut ui, mut images, mut timelines): (ResMut<UiImages>, ResMut<Assets<Image>>, ResMut<MovieTimelines>),
    roots: Query<Entity, With<WheelRoot>>,
    (mut equipped, mut sfx, vm): (MessageWriter<PowerEquipped>, MessageWriter<PostEvent>, Option<Res<crate::kismet::Vm>>),
) {
    // (the scripts may shut it: `DisSeqAct_TogglePowerWheel`)
    let held = mouse.pressed(MouseButton::Middle) && !stats.dead && !vm.as_ref().is_some_and(|v| v.wheel_off);
    if held && !w.open {
        w.open = true;
        w.aim = Vec2::ZERO;
        let (items, seps) = arrange(&stats);
        w.items = items;
        w.seps = seps;
        // (it opens on what is in hand)
        w.hover = w.items.iter().position(|(p, _)| *p == powers.selected);
        tc.wheel = WHEEL_TIME;
        sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Hand_In", None));
        let Some(tl) = timelines.get(MOVIE) else { return };
        let title: bevy::text::FontSource = fonts.title.clone().into();
        let node = || Node { position_type: PositionType::Absolute, ..default() };
        let root = commands.spawn((WheelRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(70), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
        for (sym, at) in [("_noise", Vec2::new(640.0, 360.0))] {
            if let Some(c) = Clip::export(&tl, sym) {
                commands.spawn((Backdrop(at), FlashClip::new(MOVIE, tl.clone(), c).real(), node(), Pickable::IGNORE, ChildOf(root)));
            }
        }
        if let Some(c) = Clip::export(&tl, "wheel_mc") {
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
            // (its own placeholder name fields are drawn as text)
            fc.set_visible("item_infos", false);
            commands.spawn((WheelClip, fc, node(), Pickable::IGNORE, ChildOf(root)));
        }
        for i in 0..w.seps.len() {
            if let Some(c) = Clip::export(&tl, "wheel_separator") {
                commands.spawn((Separator(i), FlashClip::new(MOVIE, tl.clone(), c).real(), node(), Pickable::IGNORE, ChildOf(root)));
            }
        }
        if let Some(c) = Clip::export(&tl, "wheel_indicator") {
            commands.spawn((Indicator, FlashClip::new(MOVIE, tl.clone(), c).real(), node(), Visibility::Hidden, Pickable::IGNORE, ChildOf(root)));
        }
        for (i, (p, _)) in w.items.iter().enumerate() {
            // the item's disc (`ic`: sprite 29) and its art
            let mut fc = FlashClip::new(MOVIE, tl.clone(), Clip::new(&tl, 29)).real();
            fc.goto("", if is_power(*p) { "stop_pow" } else { "default" }, false);
            commands.spawn((ItemIcon(i), fc, node(), Pickable::IGNORE, ChildOf(root)));
            if let Some((img, _)) = ui.file(&mut images, "icons", p.icon()) {
                commands.spawn((ItemArt(i), ImageNode::new(img), node(), Pickable::IGNORE, ChildOf(root)));
            }
            if let Some(n) = p.ammo(&stats) {
                commands.spawn((ItemCount(i), Text::new(n.to_string()), TextFont { font: title.clone(), ..default() }, TextColor(NAME_COLOR), node(), Pickable::IGNORE, ChildOf(root)));
            }
        }
        commands.spawn((NameText, Text::new(""), TextFont { font: title.clone(), ..default() }, TextColor(NAME_COLOR), TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap), node(), UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Pickable::IGNORE, ChildOf(root)));
        commands.spawn((AmmoText, Text::new(""), TextFont::default(), TextColor(AMMO_COLOR), TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap), node(), Pickable::IGNORE, ChildOf(root)));
        return;
    }
    if !w.open {
        return;
    }
    // point with the mouse: the item nearest the direction (`stickinessAngle` aside)
    w.aim += motion.delta;
    let len = w.aim.length();
    if len > 160.0 {
        w.aim *= 160.0 / len;
    }
    if w.aim.length() > 30.0 && !w.items.is_empty() {
        let a = w.aim.x.atan2(-w.aim.y).to_degrees().rem_euclid(360.0);
        let d = |b: f32| ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs();
        let k = (0..w.items.len()).min_by(|&i, &j| d(w.items[i].1).total_cmp(&d(w.items[j].1)));
        if k.is_some() && w.hover != k {
            w.hover = k;
            sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Select", None));
        }
    }
    if !held {
        // let go: equip what is pointed at
        if let Some(p) = w.hover.and_then(|k| w.items.get(k).map(|x| x.0)) {
            if p != powers.selected {
                powers.selected = p;
                powers.aiming = false;
                equipped.write(PowerEquipped(p));
                sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Validation", None));
            }
        }
        w.open = false;
        tc.wheel = 1.0;
        for e in &roots {
            commands.entity(e).despawn();
        }
        sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Hand_Out", None));
    }
}

/// The wheel placed on the screen: the clips, the icons on the rim, the chosen one's name.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn layout(
    w: Res<Wheel>,
    stats: Res<PlayerStats>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut clips: Query<(&mut FlashClip, &mut Node, Option<&Backdrop>, Option<&WheelClip>, Option<&ItemIcon>, Option<&Separator>, Option<&Indicator>, &mut Visibility), Without<ItemArt>>,
    mut arts: Query<(&ItemArt, &mut Node, &mut ImageNode), Without<FlashClip>>,
    mut counts: Query<(&ItemCount, &mut TextFont, &mut Node, &ComputedNode), (Without<FlashClip>, Without<ItemArt>, Without<NameText>, Without<AmmoText>)>,
    mut name: Query<(&mut Text, &mut TextFont, &mut Node, &ComputedNode), (With<NameText>, Without<FlashClip>, Without<ItemArt>, Without<ItemCount>, Without<AmmoText>)>,
    mut ammo: Query<(&mut Text, &mut TextFont, &mut Node, &ComputedNode), (With<AmmoText>, Without<FlashClip>, Without<ItemArt>, Without<ItemCount>, Without<NameText>)>,
) {
    if !w.open {
        return;
    }
    let Ok(win) = window.single() else { return };
    let s = (win.width() / 1280.0).min(win.height() / 720.0).max(0.01);
    let off = (Vec2::new(win.width(), win.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let rim = WHEEL_R - 0.5 * IC_W * IC_SCALE;
    let pos = |deg: f32| {
        let r = deg.to_radians();
        WHEEL_AT + Vec2::new(r.sin(), -r.cos()) * rim
    };
    let hovered = w.hover.and_then(|k| w.items.get(k).copied());
    for (mut fc, mut n, back, wheel, icon, sep, ind, mut vis) in &mut clips {
        fc.scale = s;
        let at = if let Some(b) = back {
            b.0
        } else if wheel.is_some() {
            // its illustration: the one pointed at
            let want = hovered.map(|h| schema(h.0)).unwrap_or("default");
            if fc.label("item_schema") != Some(want) {
                fc.goto("item_schema", want, true);
            }
            WHEEL_AT
        } else if let Some(i) = icon {
            let Some(&(p, a)) = w.items.get(i.0) else { continue };
            fc.m = [IC_SCALE, 0.0, 0.0, IC_SCALE, 0.0, 0.0];
            // over and out (an item's or a power's)
            let on = w.hover == Some(i.0);
            let (over, out) = if is_power(p) { ("over_pow", "out_pow") } else { ("over", "out") };
            let l = fc.label("").map(|l| l.to_string());
            if on && l.as_deref() != Some(over) && l.as_deref() != Some("stop_over") && l.as_deref() != Some("stop_pow_over") {
                fc.goto("", over, true);
            } else if !on && matches!(l.as_deref(), Some("over") | Some("over_pow") | Some("stop_over") | Some("stop_pow_over")) {
                fc.goto("", out, true);
            }
            pos(a)
        } else if let Some(sp) = sep {
            let Some(&a) = w.seps.get(sp.0) else { continue };
            fc.m = turn_scale(a, 1.0, 1.0);
            WHEEL_AT
        } else if ind.is_some() {
            match hovered {
                Some((_, a)) => {
                    fc.m = turn_scale(a, 1.0, 1.0);
                    *vis = Visibility::Inherited;
                }
                None => *vis = Visibility::Hidden,
            }
            WHEEL_AT
        } else {
            continue;
        };
        let p = off + at * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
    }
    let art = IC_IMAGE * IC_SCALE * s;
    for (a, mut n, mut img) in &mut arts {
        let Some(&(_, deg)) = w.items.get(a.0) else { continue };
        // (the one pointed at: dark on its light disc, as the over frames turn its art)
        img.color = if w.hover == Some(a.0) { Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0) } else { Color::WHITE };
        let p = off + pos(deg) * s;
        n.left = Val::Px(p.x - art * 0.5);
        n.top = Val::Px(p.y - art * 0.5);
        n.width = Val::Px(art);
        n.height = Val::Px(art);
    }
    // the counts on the discs (`quantity`, at their lower left)
    for (c, mut f, mut n, _) in &mut counts {
        let Some(&(_, deg)) = w.items.get(c.0) else { continue };
        let fs = bevy::text::FontSize::Px(18.0 * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let p = off + (pos(deg) + Vec2::new(-52.4, 31.55) * IC_SCALE) * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y - 18.0 * s);
    }
    let (label, sub) = match hovered {
        Some((p, _)) => (p.name().to_uppercase(), p.ammo(&stats).map(|n| n.to_string()).unwrap_or_default()),
        None => (String::new(), String::new()),
    };
    for (mut t, mut f, mut n, cn) in &mut name {
        if t.0 != label {
            t.0 = label.clone();
        }
        let fs = bevy::text::FontSize::Px(24.0 * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let size = cn.size() * cn.inverse_scale_factor();
        let p = off + (WHEEL_AT + NAME_AT) * s;
        n.left = Val::Px(p.x - size.x * 0.5);
        n.top = Val::Px(p.y - size.y * 0.5);
    }
    for (mut t, mut f, mut n, cn) in &mut ammo {
        if t.0 != sub {
            t.0 = sub.clone();
        }
        let fs = bevy::text::FontSize::Px(24.0 * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let size = cn.size() * cn.inverse_scale_factor();
        let p = off + (WHEEL_AT + AMMO_AT) * s;
        n.left = Val::Px(p.x - size.x * 0.5);
        n.top = Val::Px(p.y - size.y * 0.5);
    }
}
