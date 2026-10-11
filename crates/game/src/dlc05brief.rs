//! Dunwall City Trials' briefing (`UI_Brief_DLC05.Brief`, cooked as `Brief_DLC05`): once a
//! challenge's level is up, before its opening, the game waits on the briefing
//! (`B_BriefScreen.Show`) over the animated backdrop: the challenge's details on the right (its
//! picture, kind, name and mode, the scores its stars take) and, on the left, its page
//! (`B_BriefPage_<challenge>`): the sections Objectives, Scoring, Special and Game over
//! (`B_Brief_Section`'s title bars), each its lines (`t_<challenge>_<N|E>_<section>_<d|0..>`,
//! an expert mode's own in red over the normal ones) and the pictures some pages show
//! between them (`UI_Brf_<challenge>`: `Arena_Sp0`...), scrolled with the wheel or the arrows
//! within the page's mask. Any of Enter, Space, E or Escape (`A`, `Back`) closes it and the
//! challenge begins.

use crate::challenge::Challenge;
use crate::flash::{html_spans, Clip, Ease, FlashClip, MovieTimelines, PropsTo};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};

pub struct Dlc05BriefPlugin;

impl Plugin for Dlc05BriefPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (open, run).chain().before(crate::challenge::ChallengeSet).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "Brief_DLC05";
const BR_TEXTS: &str = "DisDLC05MoviePlayerBrief_Texts";
const B_TEXTS: &str = "DisGFxMoviePlayerBase_Texts";
/// `B_BriefPage_Base`: the lines' indent, the gap between sections and around pictures
const TXT_X: f32 = 25.0;
const SECTION_GAP: f32 = 20.0;
const IMG_GAP: f32 = 20.0;
/// a section's title bar (`B_Brief_Section.title_mc`)
const TITLE_H: f32 = 52.0;
/// the lines' colour (`B_Brief_Section.txt`) and an expert mode's (`_expertColorTag`)
const LINES: Color = Color::srgb(0.66, 0.71, 0.62);
const EXPERT: &str = "#C35128";

#[derive(Component)]
struct BriefRoot;

/// The page's column (scrolled within its mask).
#[derive(Component)]
struct BriefColumn;

#[derive(Component)]
struct BriefScreen;

/// The briefing up: since when, closing since when, how far scrolled.
#[derive(Resource)]
struct Brief {
    root: Entity,
    closing: Option<f32>,
    scroll: f32,
}

/// The pictures a challenge's page shows, after which of its lines (`_<section>_imgList`:
/// the picture before the line of its index).
fn pictures(id: &str) -> &'static [(&'static str, usize, &'static str)] {
    match id {
        "Arena" => &[("Sp", 0, "Arena_Sp0"), ("Sp", 1, "Arena_Sp1"), ("Sp", 2, "Arena_Sp2")],
        "Thief" => &[("Ob", 1, "Thief_Ob0")],
        "OilRain" => &[("Sp", 0, "OilRain_Sp0")],
        "DropAttack" => &[("Sp", 0, "DropAttack_Sp0")],
        "Countdown" => &[("Ob", 0, "Countdown_Ob0"), ("Ob", 1, "Countdown_Ob1")],
        "ChainKill" => &[("Ob", 0, "ChainKill_Ob0")],
        _ => &[],
    }
}

/// The level is up: the briefing (the game held, the pointer free).
#[allow(clippy::too_many_arguments)]
fn open(
    mut commands: Commands,
    ch: Res<Challenge>,
    brief: Option<Res<Brief>>,
    warm: Option<Res<crate::warmup::Warmup>>,
    window: Query<&Window>,
    mut timelines: ResMut<MovieTimelines>,
    data: Res<crate::gamedata::Data>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
    mut paused: ResMut<crate::hud::Paused>,
) {
    if !ch.active() || ch.briefed || brief.is_some() || warm.is_some() {
        return;
    }
    let Some(def) = ch.def.clone() else { return };
    let (Ok(w), Some(tl)) = (window.single(), timelines.get(MOVIE)) else { return };
    paused.0 = true;
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let at = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
    let root = commands.spawn((BriefRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, BackgroundColor(Color::BLACK), GlobalZIndex(65), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    // the backdrop's shards and blades
    if let Some(c) = Clip::export(&tl, "R_AnimatedBkgd") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        commands.spawn((fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
    }
    // the screen: the details (`B_BriefScreen_Details.SetDetails`)
    let Some(c) = Clip::export(&tl, "B_BriefScreen") else { return };
    let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
    fc.scale = s;
    let kind = match def.kind.as_str() {
        "DDCT_Mobility" => (1, "t_MobilityType"),
        "DDCT_Stealth" => (2, "t_StealthType"),
        "DDCT_Action" => (3, "t_ActionType"),
        _ => (4, "t_PuzzleType"),
    };
    let mut name = def.name.clone();
    if ch.expert {
        name = format!("{name} - <font color=\"{EXPERT}\">{}</font>", data.text(B_TEXTS, "t_ExpertMode"));
    }
    fc.set_text("_details_mc._name_mc.txt", name.to_uppercase());
    fc.wrap.insert("_details_mc._name_mc.txt".into(), false);
    let typ = data.text(B_TEXTS, "t_Type");
    fc.set_text("_details_mc._name_mc.type_txt", format!("{}{}", if typ.is_empty() { "Type: ".to_string() } else { typ }, data.text(B_TEXTS, kind.1)));
    fc.goto_frame("_details_mc._icon_mc", kind.0, false);
    let large = ui.file(&mut images, "dlc05", &format!("ChallengeImg_{}_Large", def.id));
    fc.load_image("_details_mc._thumb_mc", large);
    let medals = if ch.expert && def.expert_medals.iter().any(|m| *m > 0) { def.expert_medals } else { def.medals };
    let pts = data.text(B_TEXTS, "t_Pts");
    for (l, m) in medals.iter().enumerate() {
        let line = format!("_details_mc._hintScore_mc._line{l}_mc");
        fc.set_text(&format!("{line}.txt"), format!("{m} {pts}"));
        for k in 0..3 {
            fc.goto(&format!("{line}.mc{k}"), if k <= l { "on" } else { "off" }, false);
        }
    }
    // (the page's own lines are drawn into its mask below)
    let mask = fc.size("_brief_mc._scrollView_mc.mask_mc").unwrap_or(Vec2::new(620.0, 560.0));
    let page_at = Vec2::new(640.0, 360.0) + Vec2::new(-547.0 + 41.0, -306.1 + 8.1);
    fc.set_visible("_brief_mc._scrollView_mc", false);
    // `Open`: the parts in
    for (p, from, secs) in [
        ("_details_mc._icon_mc", PropsTo::default().alpha(0.0).xscale(2.0).yscale(3.5), 0.25),
        ("_details_mc._frame_mc", PropsTo::default().alpha(0.0).xscale(2.5).yscale(2.0), 0.25),
        ("_details_mc._thumb_mc", PropsTo::default().alpha(0.0), 0.25),
        ("_bkgd_mc", PropsTo::default().alpha(0.0), 0.25),
    ] {
        if let Some(sp) = fc.props(p) {
            let back: PropsTo = sp.into();
            fc.set(p, from);
            fc.tween(p, back, secs, Ease::BackInOut);
        }
    }
    commands.spawn((BriefScreen, fc, at(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
    // the page: its sections in a column, clipped to the mask
    let p = off + page_at * s;
    let viewport = commands
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(mask.x * s), height: Val::Px(mask.y * s), overflow: Overflow::clip(), ..default() },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    let column = commands
        .spawn((BriefColumn, Node { position_type: PositionType::Absolute, top: Val::Px(0.0), width: Val::Px(mask.x * s), flex_direction: FlexDirection::Column, row_gap: Val::Px(0.0), ..default() }, Pickable::IGNORE, ChildOf(viewport)))
        .id();
    let font = TextFont { font_size: bevy::text::FontSize::Px((25.0 * s).round()), ..default() };
    let line = |key: &str| -> Option<String> {
        let text = |k: String| data.0.texts.get(&format!("{BR_TEXTS}.{k}")).filter(|t| !t.is_empty()).cloned();
        let expert = text(format!("t_{}_E_{key}", def.id)).filter(|_| ch.expert).map(|t| format!("<font color=\"{EXPERT}\">{t}</font>"));
        expert.or_else(|| text(format!("t_{}_N_{key}", def.id)))
    };
    for (si, sec) in ["Ob", "Sc", "Sp", "Go"].into_iter().enumerate() {
        let mut lines: Vec<(Option<usize>, String)> = Vec::new();
        if let Some(t) = line(&format!("{sec}_d")) {
            lines.push((None, t));
        }
        for i in 0..10 {
            match line(&format!("{sec}_{i}")) {
                Some(t) => lines.push((Some(i), t)),
                None => break,
            }
        }
        if lines.is_empty() {
            continue;
        }
        // the section's title bar
        let bar = commands.spawn((Node { width: Val::Px(mask.x * s), height: Val::Px(TITLE_H * s), margin: UiRect::top(Val::Px(if si > 0 { SECTION_GAP * s } else { 0.0 })), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(column))).id();
        if let Some(c) = Clip::export(&tl, "B_Brief_Section") {
            let mut sf = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
            sf.scale = s;
            // (its own lines' field is the page's to fill: empty here, the lines below)
            sf.set_text("txt", String::new());
            sf.set_text("title_mc.txt", data.text(BR_TEXTS, &format!("t_Section_{sec}")).to_uppercase());
            sf.wrap.insert("title_mc.txt".into(), false);
            commands.spawn((sf, Node { position_type: PositionType::Absolute, left: Val::Px(0.0), top: Val::Px(0.0), ..default() }, Pickable::IGNORE, ChildOf(bar)));
        }
        for (i, t) in lines {
            // a picture before the line of its index
            if let Some((_, _, img)) = pictures(&def.id).iter().find(|(s2, k, _)| *s2 == sec && Some(*k) == i) {
                if let Some((h, size)) = ui.file(&mut images, "dlc05", img) {
                    let wmax = (mask.x - 2.0 * TXT_X).max(10.0);
                    let k = (wmax / size.x).min(1.0);
                    commands.spawn((
                        ImageNode::new(h),
                        Node { width: Val::Px(size.x * k * s), height: Val::Px(size.y * k * s), margin: UiRect::new(Val::Px(TXT_X * s), Val::Px(0.0), Val::Px(IMG_GAP * s), Val::Px(IMG_GAP * s)), flex_shrink: 0.0, ..default() },
                        Pickable::IGNORE,
                        ChildOf(column),
                    ));
                }
            }
            let spans = html_spans(&t);
            let txt = commands
                .spawn((
                    Text::new(spans.first().map(|p| p.0.clone()).unwrap_or_default()),
                    font.clone(),
                    TextColor(spans.first().and_then(|p| p.1).unwrap_or(LINES)),
                    Node { width: Val::Px((mask.x - 2.0 * TXT_X) * s), margin: UiRect::new(Val::Px(TXT_X * s), Val::Px(0.0), Val::Px(2.0 * s), Val::Px(2.0 * s)), flex_shrink: 0.0, ..default() },
                    Pickable::IGNORE,
                    ChildOf(column),
                ))
                .id();
            for sp in spans.iter().skip(1) {
                commands.spawn((TextSpan::new(sp.0.clone()), font.clone(), TextColor(sp.1.unwrap_or(LINES)), ChildOf(txt)));
            }
        }
    }
    let _ = &fonts;
    commands.insert_resource(Brief { root, closing: None, scroll: 0.0 });
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut commands: Commands,
    brief: Option<ResMut<Brief>>,
    mut ch: ResMut<Challenge>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    mut columns: Query<(&mut Node, &ComputedNode, &ChildOf), With<BriefColumn>>,
    viewports: Query<&ComputedNode, Without<BriefColumn>>,
    mut screens: Query<&mut FlashClip, With<BriefScreen>>,
    time: Res<Time<Real>>,
    mut cursor: Single<&mut CursorOptions>,
    mut paused: ResMut<crate::hud::Paused>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
) {
    let Some(mut b) = brief else { return };
    if cursor.grab_mode != CursorGrabMode::None {
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
    }
    paused.0 = true;
    if let Some(t) = b.closing.as_mut() {
        *t += time.delta_secs();
        if *t >= 0.25 {
            commands.entity(b.root).despawn();
            commands.remove_resource::<Brief>();
            ch.briefed = true;
            paused.0 = false;
            cursor.visible = false;
            cursor.grab_mode = CursorGrabMode::Locked;
        }
        return;
    }
    // the page scrolled (the wheel, the arrows), within its height
    let mut d = 0.0;
    for ev in wheel.read() {
        d -= ev.y * 40.0;
    }
    if keys.pressed(KeyCode::ArrowDown) || keys.pressed(KeyCode::KeyS) {
        d += 600.0 * time.delta_secs();
    }
    if keys.pressed(KeyCode::ArrowUp) || keys.pressed(KeyCode::KeyW) {
        d -= 600.0 * time.delta_secs();
    }
    if let Ok((mut n, cn, parent)) = columns.single_mut() {
        let view = viewports.get(parent.parent()).map(|v| v.size().y * v.inverse_scale_factor()).unwrap_or(0.0);
        let full = cn.size().y * cn.inverse_scale_factor();
        b.scroll = (b.scroll + d).clamp(0.0, (full - view).max(0.0));
        let top = Val::Px(-b.scroll);
        if n.top != top {
            n.top = top;
        }
    }
    let close = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) || keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Left);
    if close {
        // `Close`: the parts out
        sfx.write(crate::audio::PostEvent::named("UI_Validation", None));
        if let Ok(mut fc) = screens.single_mut() {
            for (p, dx) in [("_details_mc", 150.0), ("_bkgd_mc", -150.0), ("_brief_mc", -150.0)] {
                if let Some(sp) = fc.props(p) {
                    fc.tween(p, PropsTo::default().x(sp.x + dx).alpha(0.0), 0.25, Ease::StrongOut);
                }
            }
        }
        b.closing = Some(0.0);
    }
}
