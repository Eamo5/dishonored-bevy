//! The location banner (`DisSeqAct_ShowLocationDiscovery`): the HUD movie's `hud_location`
//! clip driven as its `Location` class does (disassembled). The name is split into a big first
//! letter, the rest and a big last letter (`hud_location_BigLetter` / `hud_location_letter`:
//! the title font at 42 and 31, stretched 1.2 high), laid side by side and centred; a mask
//! opens on them from the middle (10 units to their width + 20 over 0.95 s) as they fade in.
//! Meanwhile the separators slide in across each other (0.65 s), the ornaments at either end
//! open from the middle (`open`), and the backing grows out from 30% of its width (0.4 s).
//! After the HUD tweak's `m_fLocationDiscoveryDuration` (5 s) it all fades (0.6 s) as the
//! separators draw back to the middle; it comes with the HUD tweak's
//! `m_LocationDiscoverySoundEvent` (`Discovery`).

use crate::flash::{Clip, FlashClip, MovieTimelines, Mat};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct LocationPlugin;

impl Plugin for LocationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Banner>()
            .add_systems(OnEnter(GameState::InGame), |mut b: ResMut<Banner>| *b = Banner::default())
            .add_systems(Update, (start_banner, play_banner).chain().run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";
const SYMBOL: &str = "hud_location";
/// `location_mc` on the stage
const AT: Vec2 = Vec2::new(640.0, 206.75);
/// `m_fLocationDiscoveryDuration` (the HUD tweak's class default)
const DURATION: f32 = 5.0;
const SOUND: &str = "Discovery";
/// the letters' text: colour, sizes (big, normal), their fields' tops, the vertical stretch
const COLOR: Color = Color::srgb(225.0 / 255.0, 240.0 / 255.0, 212.0 / 255.0);
const BIG: f32 = 42.0;
const NORMAL: f32 = 31.0;
const TOP_BIG: f32 = -28.65;
const TOP_NORMAL: f32 = -21.15;
const STRETCH: f32 = 1.2;
/// the backing's own width (`_bkgd_mc`)
const BKGD_W: f32 = 400.0;

/// `Strong.easeOut`
fn strong_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5)
}

/// `Back.easeOut`
fn back_out(x: f32) -> f32 {
    let s = 1.70158;
    let x = x.clamp(0.0, 1.0) - 1.0;
    x * x * ((s + 1.0) * x + s) + 1.0
}

/// A tween from `from` to `to` over `secs`, `t` seconds in.
fn tween(from: f32, to: f32, t: f32, secs: f32, ease: fn(f32) -> f32) -> f32 {
    from + (to - from) * ease(if secs > 0.0 { t / secs } else { 1.0 })
}

/// The laid-out name: each part's left and width (stage units, centred), the mask's full
/// width, the separators' and ornaments' places, the backing's width.
struct Layout {
    parts: Vec<(f32, f32)>,
    mask_w: f32,
    sep_up: f32,
    sep_down: f32,
    tag: f32,
    bkgd_w: f32,
}

#[derive(Resource, Default)]
struct Banner {
    root: Option<Entity>,
    /// seconds since it opened (None: measuring the letters)
    t: Option<f32>,
    layout: Option<Layout>,
    /// the clip's children's own matrices
    base: Vec<(&'static str, Mat)>,
}

#[derive(Component)]
struct BannerClip;
#[derive(Component)]
struct BannerMask;
/// a part of the name: 0 the first letter, 1 the middle, 2 the last
#[derive(Component)]
struct BannerPart(usize, bool);

const CHILDREN: [&str; 5] = ["_bkgd_mc", "_sepUp_mc", "_sepDown_mc", "_tagLeft_mc", "_tagRight_mc"];

/// `SetLocation`: a new banner (replacing any up).
fn start_banner(
    mut commands: Commands,
    script_ui: Option<ResMut<crate::kismet::ScriptUi>>,
    mut banner: ResMut<Banner>,
    mut timelines: ResMut<MovieTimelines>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut sounds: MessageWriter<crate::audio::PostEvent>,
) {
    let Some(mut ui) = script_ui else { return };
    let Some(name) = ui.location.take() else { return };
    // (a banner up goes; one from before a load is gone already)
    if let Some(e) = banner.root.take() {
        commands.entity(e).try_despawn();
    }
    let Ok(w) = window.single() else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let Some(clip) = Clip::export(&tl, SYMBOL) else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    // the parts: the first letter big, the middle, the last letter big (a short name: its
    // letters)
    let chars: Vec<char> = name.chars().collect();
    let n = chars.len().min(3);
    let parts: Vec<(String, bool)> = (0..n)
        .map(|i| {
            if i > 0 && i + 1 < n {
                (chars[1..chars.len() - 1].iter().collect::<String>(), false)
            } else if i == 0 {
                (chars[0].to_string(), true)
            } else {
                (chars[chars.len() - 1].to_string(), true)
            }
        })
        .map(|(p, big)| (p.to_uppercase(), big))
        .collect();
    banner.base = CHILDREN.iter().filter_map(|c| clip.placed(c).map(|p| (*c, p.0))).collect();
    let mut fc = FlashClip::new(MOVIE, tl.clone(), clip);
    fc.scale = s;
    for c in CHILDREN {
        fc.set_visible(c, false);
    }
    let root = commands
        .spawn((Node { position_type: PositionType::Absolute, ..default() }, GlobalZIndex(-1), Pickable::IGNORE, DespawnOnExit(GameState::InGame), Visibility::Hidden))
        .with_children(|r| {
            r.spawn((BannerClip, fc, Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
            r.spawn((BannerMask, Node { position_type: PositionType::Absolute, overflow: Overflow::clip(), ..default() }, Pickable::IGNORE)).with_children(|m| {
                for (i, (p, big)) in parts.iter().enumerate() {
                    let size = if *big { BIG } else { NORMAL };
                    m.spawn((
                        BannerPart(i, *big),
                        Text::new(p.clone()),
                        TextFont { font: fonts.title.clone().into(), font_size: bevy::text::FontSize::Px(size * s), ..default() },
                        TextColor(COLOR.with_alpha(0.0)),
                        TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap),
                        Node { position_type: PositionType::Absolute, ..default() },
                        UiTransform { scale: Vec2::new(1.0, STRETCH), ..default() },
                        Pickable::IGNORE,
                    ));
                }
            });
        })
        .id();
    banner.root = Some(root);
    banner.t = None;
    banner.layout = None;
    sounds.write(crate::audio::PostEvent::named(SOUND, None));
}

#[allow(clippy::type_complexity)]
fn play_banner(
    mut commands: Commands,
    time: Res<Time>,
    mut banner: ResMut<Banner>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut roots: Query<(&mut Node, &mut Visibility), (Without<BannerMask>, Without<BannerPart>, Without<BannerClip>)>,
    mut clip: Query<&mut FlashClip, With<BannerClip>>,
    mut mask: Query<&mut Node, (With<BannerMask>, Without<BannerPart>)>,
    mut parts: Query<(&BannerPart, &ComputedNode, &mut Node, &mut TextColor), Without<BannerMask>>,
    hud_hidden: Res<crate::menu::HudHidden>,
) {
    let Some(root) = banner.root else { return };
    let Ok(w) = window.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    // the letters measured (laid out once): their widths in stage units
    if banner.layout.is_none() {
        let mut widths: Vec<(usize, f32)> = parts.iter().map(|(p, c, _, _)| (p.0, c.size().x * c.inverse_scale_factor() / s)).collect();
        if widths.is_empty() || widths.iter().any(|w| w.1 <= 0.0) {
            return;
        }
        widths.sort_by_key(|w| w.0);
        // each letter clip after the last (its field's width, autosized, less 3.5)
        let field = |tw: f32| tw + 4.0;
        let mut x = 0.0;
        let mut lefts = Vec::new();
        for i in 0..widths.len() {
            if i > 0 {
                x += field(widths[i - 1].1) - 3.5;
            }
            lefts.push(x);
        }
        let total = lefts.last().copied().unwrap_or(0.0) + field(widths.last().map(|w| w.1).unwrap_or(0.0));
        let r7 = total / 2.0;
        let first_w = field(widths[0].1);
        let last_w = field(widths[widths.len() - 1].1);
        // the ornaments' width (their own frame's bitmaps)
        let tag_w = clip.single().ok().and_then(|fc| fc.clip.child("_tagLeft_mc").and_then(|c| c.bounds(&fc.tl))).map(|(a, b)| b.x - a.x).unwrap_or(20.0);
        banner.layout = Some(Layout {
            parts: lefts.iter().zip(widths.iter()).map(|(l, w)| (l - r7 + 2.0, w.1)).collect(),
            mask_w: 2.0 * r7 + 20.0,
            sep_up: r7 - last_w,
            sep_down: -r7 + first_w,
            tag: r7 + 2.5,
            bkgd_w: total + 2.0 * tag_w + 50.0,
        });
        banner.t = Some(0.0);
    }
    let t = banner.t.map(|t| t + time.delta_secs()).unwrap_or(0.0);
    banner.t = Some(t);
    let Some(l) = banner.layout.as_ref() else { return };
    // done: away
    if t > DURATION + 0.6 {
        commands.entity(root).try_despawn();
        banner.root = None;
        return;
    }
    if let Ok((mut n, mut v)) = roots.get_mut(root) {
        let p = off + AT * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
        // (put away with the HUD: under a menu, while the kill cam rides a bolt)
        *v = if hud_hidden.0 { Visibility::Hidden } else { Visibility::Inherited };
    }
    // `OpenElements` (after 15 ms a letter), `Close` / `FadeOut` after the duration
    let te = t - 0.015 * l.parts.len() as f32;
    let tc = t - DURATION;
    let alpha = if tc > 0.0 { tween(1.0, 0.0, tc, 0.6, strong_out) } else { 1.0 };
    if let Ok(mut fc) = clip.single_mut() {
        fc.scale = s;
        fc.alpha = alpha;
        let open = te >= 0.0;
        for c in CHILDREN {
            fc.set_visible(c, open);
        }
        if open && !fc.label("_tagLeft_mc").is_some_and(|l| l == "open") {
            fc.goto("_tagLeft_mc", "open", true);
            fc.goto("_tagRight_mc", "open", true);
        }
        let te = te.max(0.0);
        let base = |name: &str| banner.base.iter().find(|b| b.0 == name).map(|b| b.1).unwrap_or(crate::flash::IDENTITY);
        // the separators: in from across the middle; drawing back to it as it closes
        let sep = |from: f32, to: f32| if tc > 0.0 { tween(to, 0.0, tc, 0.45, back_out) } else { tween(from, to, te, 0.65, strong_out) };
        let fade = tween(0.0, 1.0, te, 0.65, strong_out);
        let set = |fc: &mut FlashClip, name: &str, x: f32, xscale: Option<f32>, a: f32| {
            let b = base(name);
            if let Some((m, cx)) = fc.clip.placed_mut(name) {
                *m = b;
                m[4] = x;
                if let Some(k) = xscale {
                    m[0] = b[0] * k;
                    m[1] = b[1] * k;
                }
                cx[3] = a;
            }
        };
        set(&mut fc, "_sepUp_mc", sep(-50.0, l.sep_up), None, fade);
        set(&mut fc, "_sepDown_mc", sep(50.0, l.sep_down), None, fade);
        set(&mut fc, "_tagLeft_mc", tween(0.0, -l.tag, te, 0.65, strong_out), None, fade);
        set(&mut fc, "_tagRight_mc", tween(0.0, l.tag, te, 0.65, strong_out), None, fade);
        let k = l.bkgd_w / BKGD_W;
        set(&mut fc, "_bkgd_mc", 0.0, Some(tween(0.3, k, te, 0.4, strong_out)), tween(0.0, 1.0, te, 0.4, strong_out));
    }
    // `OpenLetter`: the mask opens from the middle as the letters fade in
    let te = te.max(0.0);
    let mw = if te > 0.0 { tween(10.0, l.mask_w, te, 0.95, strong_out) } else { 0.0 };
    if let Ok(mut n) = mask.single_mut() {
        n.left = Val::Px(-mw * 0.5 * s);
        n.top = Val::Px(-40.0 * s);
        n.width = Val::Px(mw * s);
        n.height = Val::Px(80.0 * s);
    }
    let la = tween(0.0, 1.0, te, 0.35, strong_out) * alpha;
    for (p, c, mut n, mut col) in &mut parts {
        let Some(&(x, _)) = l.parts.get(p.0) else { continue };
        let h = c.size().y * c.inverse_scale_factor();
        let top = if p.1 { TOP_BIG } else { TOP_NORMAL };
        // (the stretch is about the node's middle; the field's is about its top)
        n.left = Val::Px((x + mw * 0.5) * s);
        n.top = Val::Px((top + 40.0) * s + (STRETCH - 1.0) * 0.5 * h);
        col.0 = COLOR.with_alpha(la);
    }
}
