//! The HUD movie's prompts beside the crosshair's: the special moves (`interactions_ic_mc`,
//! class `InteractionsIcons`, bottom right: a `hud_interactionIc` for each — its icon `ic` at
//! the move's label (`mantle`, `pickpocket`, `choke`, `assassination`, `drop_assassination`,
//! `adrenaline_kill`), its keys in `txt` — stacked up from the bottom 5 units apart, all faded
//! in over 0.35 s and out over 0.25 s), and the context line (`interactions_txt_mc`, class
//! `InteractionsMessage`, at the left: what Corvo holds or is in the middle of — drop, throw,
//! leave the keyhole, end a power — sliding in under a rule that slides in with it). Their
//! words are the HUD tweak's `m_InteractionTexts` by `EDisUIInteraction`.

use crate::flash::{Clip, FlashClip, MovieTimelines};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct PromptsPlugin;

impl Plugin for PromptsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Prompts>()
            .add_systems(OnEnter(GameState::InGame), |mut p: ResMut<Prompts>| *p = Prompts::default())
            .add_systems(Update, (special_icons, context_line).before(crate::flash::PlayClips).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";

/// `EDisUIInteraction`: the special moves, and the context's
const MANTLE: usize = 1;
const STEAL: usize = 2;
const CHOKE: usize = 3;
const ASSASSINATE: usize = 4;
const DROP_ASSASSINATE: usize = 5;
const ADRENALINE_KILL: usize = 6;
const MOVABLE_DROP: usize = 8;
const MOVABLE_THROW: usize = 9;
const CORPSE_DROP: usize = 10;
const CORPSE_THROW: usize = 11;
const CLIMB_DROP: usize = 12;
const CLIMB_JUMP: usize = 13;
const EXIT_KEYHOLE: usize = 14;
const VERSUS: usize = 15;
const MINIGAME: usize = 16;
const SPYGLASS_EXIT: usize = 17;
const SPYGLASS_ZOOM: usize = 18;
const CANCEL_COOKING: usize = 20;
const END_BEND_TIME: usize = 21;
const END_DARK_VISION: usize = 23;
const END_POSSESS: usize = 25;

/// The words when the tweak's are missing (`Twk_InGameUI.int`).
fn fallback(i: usize) -> &'static str {
    match i {
        MANTLE => "`GBA_Jump`",
        STEAL => "`GBA_Use`",
        CHOKE => "`GBA_Block` [Hold]",
        ASSASSINATE | DROP_ASSASSINATE => "`GBA_Primary`",
        ADRENALINE_KILL => "`GBA_Block` + `GBA_Primary`",
        MOVABLE_DROP | CORPSE_DROP => "`GBA_Use` Drop",
        MOVABLE_THROW | CORPSE_THROW => "`GBA_Primary` Throw",
        CLIMB_DROP => "`GBA_Use` Drop",
        CLIMB_JUMP => "`GBA_Jump` Jump off",
        EXIT_KEYHOLE => "`GBA_Use` Exit Keyhole",
        MINIGAME => "Mash `GBA_Primary` !",
        SPYGLASS_EXIT => "`GBA_Zoom` Exit Spyglass",
        SPYGLASS_ZOOM => "`GBA_Use` Change zoom level",
        CANCEL_COOKING => "`GBA_Use` Cancel Cooking",
        END_BEND_TIME => "`GBA_Secondary` End Bend Time",
        END_DARK_VISION => "`GBA_Secondary` End Dark Vision",
        END_POSSESS => "`GBA_Secondary` End Possess",
        _ => "",
    }
}

/// An interaction's words, its keys named as bound (`ConvertButtonTags`).
fn words(data: &crate::gamedata::Data, i: usize) -> String {
    use crate::bindings::{hint, Act};
    let raw = data.0.texts.get(&format!("HUD.m_InteractionTexts[{i}]")).map(|s| s.as_str()).unwrap_or(fallback(i));
    let mut out = raw.replace("`GBA_Primary`", "[LMB]").replace("`GBA_Secondary`", "[RMB]");
    for (k, a) in [("`GBA_Use`", Act::Use), ("`GBA_Block`", Act::Block), ("`GBA_Jump`", Act::Jump), ("`GBA_Zoom`", Act::Zoom)] {
        if out.contains(k) {
            out = out.replace(k, &hint(a));
        }
    }
    out
}

/// The icon's label in `hud_interactionIc.ic` for a special move.
fn icon_label(i: usize) -> &'static str {
    match i {
        MANTLE => "mantle",
        STEAL => "pickpocket",
        CHOKE => "choke",
        ASSASSINATE => "assassination",
        DROP_ASSASSINATE => "drop_assassination",
        _ => "adrenaline_kill",
    }
}

/// `interactions_ic_mc` and `interactions_txt_mc` on the stage
const ICONS_AT: Vec2 = Vec2::new(1184.0 - 368.0, 665.5);
const LINE_AT: Vec2 = Vec2::new(96.0, 288.2);
/// a `hud_interactionIc`: its height (the icon), the gap, its words' place, size and colour
const ICON_H: f32 = 79.0;
const ICON_GAP: f32 = 5.0;
const ICON_TXT: Vec2 = Vec2::new(41.15 + 2.0, -12.3 + 2.0);
const TEXT_SIZE: f32 = 23.0;
const TEXT_COLOR: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const SHADOW: Color = Color::srgba(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0, 0.85);
/// the context line: `_interactionsMsg_mc` (0, 4.9) + `txt` (2, 2), its width; the rule
/// (`_sep_mc` at (0, -4), sprite 195)
const LINE_TXT: Vec2 = Vec2::new(2.0, 6.9);
const LINE_W: f32 = 360.0;
const RULE: u16 = 195;
const RULE_AT: Vec2 = Vec2::new(0.0, -4.0);
const MAX_ICONS: usize = 6;

#[derive(Resource, Default)]
struct Prompts {
    icons_root: Option<Entity>,
    /// the moves shown, the fade (0..1) and whether opening
    shown: Vec<usize>,
    fade: f32,
    line_root: Option<Entity>,
    /// the context line shown, how far in (0..1) it and the rule are, and whether open
    line: String,
    line_t: f32,
    rule_t: f32,
    line_open: bool,
}

#[derive(Component)]
struct IconsRoot;
#[derive(Component)]
struct Icon(usize);
#[derive(Component)]
struct IconText(usize);
#[derive(Component)]
struct LineRoot;
#[derive(Component)]
struct Rule;
#[derive(Component)]
struct LineText;

fn strong_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5)
}

fn back_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) - 1.0;
    let s = 1.70158;
    x * x * ((s + 1.0) * x + s) + 1.0
}

/// The special moves Corvo could make now.
#[allow(clippy::too_many_arguments)]
fn wanted_specials(
    specials: &crate::combat::Specials,
    mantle: &crate::player::MantleSpot,
    focus: &crate::interact::InteractFocus,
    pockets: &Query<&crate::pickpocket::Pocket>,
    npcs: &Query<&crate::npc::Npc>,
) -> Vec<usize> {
    let mut out = Vec::new();
    if specials.assassinate {
        out.push(ASSASSINATE);
    }
    if specials.choke {
        out.push(CHOKE);
    }
    // a pocket of someone standing (from the body it's only loot)
    if focus.0 && focus.1.and_then(|e| pockets.get(e).ok()).is_some_and(|p| npcs.get(p.npc).is_ok_and(|n| !n.is_down())) {
        out.push(STEAL);
    }
    if specials.drop {
        out.push(DROP_ASSASSINATE);
    }
    if specials.adrenaline {
        out.push(ADRENALINE_KILL);
    }
    if mantle.0 {
        out.push(MANTLE);
    }
    out
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn special_icons(
    mut commands: Commands,
    time: Res<Time>,
    mut st: ResMut<Prompts>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    (specials, mantle, focus, data): (Res<crate::combat::Specials>, Res<crate::player::MantleSpot>, Res<crate::interact::InteractFocus>, Res<crate::gamedata::Data>),
    (pockets, npcs, settings): (Query<&crate::pickpocket::Pocket>, Query<&crate::npc::Npc>, Res<crate::settings::Settings>),
    mut roots: Query<(&mut Node, &mut Visibility), (With<IconsRoot>, Without<Icon>, Without<IconText>)>,
    mut icons: Query<(&Icon, &mut FlashClip, &mut Node, &mut Visibility), (Without<IconsRoot>, Without<IconText>)>,
    mut texts: Query<(&IconText, &mut Text, &mut TextFont, &mut TextColor, &mut TextShadow, &mut Node, &mut Visibility), (Without<IconsRoot>, Without<Icon>)>,
) {
    let Ok(w) = window.single() else { return };
    // built once under the HUD: a pool of icons, each with its words
    let root = match st.icons_root.filter(|r| roots.contains(*r)) {
        Some(r) => r,
        None => {
            let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
            let r = commands
                .spawn((IconsRoot, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h)))
                .with_children(|r| {
                    for i in 0..MAX_ICONS {
                        if let Some(c) = Clip::export(&tl, "hud_interactionIc") {
                            let mut fc = FlashClip::new(MOVIE, tl.clone(), c);
                            // (the words are ours)
                            fc.set_visible("txt", false);
                            r.spawn((Icon(i), fc, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE));
                        }
                        r.spawn((
                            IconText(i),
                            Text::new(""),
                            TextFont::default(),
                            TextColor(TEXT_COLOR),
                            TextShadow { offset: Vec2::splat(1.5), color: SHADOW },
                            TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap),
                            Node { position_type: PositionType::Absolute, ..default() },
                            Visibility::Hidden,
                            Pickable::IGNORE,
                        ));
                    }
                })
                .id();
            st.icons_root = Some(r);
            st.shown.clear();
            return;
        }
    };
    let want = if settings.contextual_icons { wanted_specials(&specials, &mantle, &focus, &pockets, &npcs) } else { Vec::new() };
    // `SetInteractions`: a new list is shown as it is; none fades the last out
    let dt = time.delta_secs();
    if !want.is_empty() {
        if st.shown != want {
            st.shown = want.clone();
        }
        st.fade = (st.fade + dt / 0.35).min(1.0);
    } else {
        st.fade = (st.fade - dt / 0.25).max(0.0);
    }
    let alpha = if want.is_empty() { 1.0 - strong_out(1.0 - st.fade) } else { strong_out(st.fade) };
    let open = st.fade > 0.0 && !st.shown.is_empty();
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    if let Ok((mut n, mut v)) = roots.get_mut(root) {
        let p = off + ICONS_AT * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
        let vis = if open { Visibility::Inherited } else { Visibility::Hidden };
        if *v != vis {
            *v = vis;
        }
    }
    if !open {
        return;
    }
    // stacked up from the bottom: the first centred `ICON_H / 2` up, each next above
    let y_of = |k: usize| -ICON_H * 0.5 - k as f32 * (ICON_H + ICON_GAP);
    for (ic, mut fc, mut n, mut v) in &mut icons {
        let Some(&what) = st.shown.get(ic.0) else {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
            continue;
        };
        fc.scale = s;
        fc.alpha = alpha;
        if fc.label("ic") != Some(icon_label(what)) {
            fc.goto("ic", icon_label(what), false);
        }
        n.left = Val::Px(0.0);
        n.top = Val::Px(y_of(ic.0) * s);
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
    }
    for (t, mut text, mut f, mut c, mut sh, mut n, mut v) in &mut texts {
        let Some(&what) = st.shown.get(t.0) else {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
            continue;
        };
        let wds = words(&data, what);
        if text.0 != wds {
            text.0 = wds;
        }
        let fs = bevy::text::FontSize::Px(TEXT_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        c.0 = TEXT_COLOR.with_alpha(alpha);
        sh.color = SHADOW.with_alpha(SHADOW.alpha() * alpha);
        sh.offset = Vec2::splat(1.5 * s);
        n.left = Val::Px(ICON_TXT.x * s);
        n.top = Val::Px((y_of(t.0) + ICON_TXT.y) * s);
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
    }
}

/// What Corvo holds or is in the middle of: the context's interactions.
#[allow(clippy::too_many_arguments)]
fn wanted_context(
    carry: &crate::carry::Carry,
    held: &crate::props::Held,
    peek: &crate::keyhole::Peek,
    climb: &crate::climb::Climb,
    grabbed: bool,
    zoom: &crate::zoom::Zoom,
    stats: &crate::gameplay::PlayerStats,
    powers: &crate::powers::Powers,
    tc: &crate::gameplay::TimeControl,
    possessing: bool,
) -> Vec<usize> {
    use crate::powers::Power;
    let mut out = Vec::new();
    if carry.body.is_some() && carry.phase == crate::carry::CarryPhase::Hold {
        out.extend([CORPSE_DROP, CORPSE_THROW]);
    }
    if held.0.is_some() {
        out.extend([MOVABLE_DROP, MOVABLE_THROW]);
    }
    if climb.on.is_some() {
        out.extend([CLIMB_DROP, CLIMB_JUMP]);
    }
    if peek.at.is_some() {
        out.push(EXIT_KEYHOLE);
    }
    if grabbed {
        out.push(MINIGAME);
    }
    if zoom.level > 0 {
        out.push(SPYGLASS_EXIT);
        if stats.upgrades.iter().any(|u| u == "Twk_Upgrade_Spyglass2") {
            out.push(SPYGLASS_ZOOM);
        }
    }
    if powers.cooking.is_some() {
        out.push(CANCEL_COOKING);
    }
    if possessing {
        out.push(END_POSSESS);
    } else if powers.selected == Power::BendTime && tc.bend_remaining > 0.0 {
        out.push(END_BEND_TIME);
    } else if powers.selected == Power::DarkVision && powers.dark_vision {
        out.push(END_DARK_VISION);
    }
    out
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn context_line(
    mut commands: Commands,
    time: Res<Time>,
    mut st: ResMut<Prompts>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    data: Res<crate::gamedata::Data>,
    (carry, held, peek, climb, zoom, stats, powers, tc, possession): (
        Res<crate::carry::Carry>,
        Res<crate::props::Held>,
        Res<crate::keyhole::Peek>,
        Res<crate::climb::Climb>,
        Res<crate::zoom::Zoom>,
        Res<crate::gameplay::PlayerStats>,
        Res<crate::powers::Powers>,
        Res<crate::gameplay::TimeControl>,
        Res<crate::possession::Possession>,
    ),
    (grabbed, versus): (Query<(), With<crate::npc::Grabbed>>, Res<crate::combat::Versus>),
    mut roots: Query<(&mut Node, &mut Visibility), (With<LineRoot>, Without<Rule>, Without<LineText>)>,
    mut rule: Query<(&mut FlashClip, &mut Node), (With<Rule>, Without<LineRoot>, Without<LineText>)>,
    mut line: Query<(&mut Text, &mut TextFont, &mut TextColor, &mut TextShadow, &mut Node), (With<LineText>, Without<LineRoot>, Without<Rule>)>,
) {
    let Ok(w) = window.single() else { return };
    let root = match st.line_root.filter(|r| roots.contains(*r)) {
        Some(r) => r,
        None => {
            let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
            let r = commands
                .spawn((LineRoot, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h)))
                .with_children(|r| {
                    r.spawn((Rule, FlashClip::new(MOVIE, tl.clone(), Clip::new(&tl, RULE)), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                    r.spawn((
                        LineText,
                        Text::new(""),
                        TextFont::default(),
                        TextColor(TEXT_COLOR),
                        TextShadow { offset: Vec2::splat(1.5), color: SHADOW },
                        Node { position_type: PositionType::Absolute, ..default() },
                        Pickable::IGNORE,
                    ));
                })
                .id();
            st.line_root = Some(r);
            return;
        }
    };
    let mut want = wanted_context(&carry, &held, &peek, &climb, !grabbed.is_empty(), &zoom, &stats, &powers, &tc, possession.host.is_some());
    if versus.npc.is_some() {
        want.insert(0, VERSUS);
    }
    let text = want.iter().map(|i| words(&data, *i)).collect::<Vec<_>>().join("\n");
    let dt = time.delta_secs();
    // `OpenInteractionMsg` (0.3 s, Back.easeOut, from 25 to the left; its rule from 35) /
    // `CloseInteractionMsg` (0.15 s to 20 left; the rule in 0.2 s to 10 left)
    if !text.is_empty() {
        if !st.line_open {
            st.line_open = true;
            st.line_t = 0.0;
            st.rule_t = 0.0;
        }
        st.line = text;
        st.line_t = (st.line_t + dt / 0.3).min(1.0);
        st.rule_t = (st.rule_t + dt / 0.3).min(1.0);
    } else if st.line_open {
        st.line_open = false;
        st.line_t = 1.0;
        st.rule_t = 1.0;
    }
    let (lx, la, rx, ra) = if st.line_open {
        let l = back_out(st.line_t);
        let r = back_out(st.rule_t);
        (-25.0 * (1.0 - l), st.line_t.min(1.0), -35.0 * (1.0 - r), st.rule_t.min(1.0))
    } else {
        st.line_t = (st.line_t - dt / 0.15).max(0.0);
        st.rule_t = (st.rule_t - dt / 0.2).max(0.0);
        let l = strong_out(1.0 - st.line_t);
        let r = strong_out(1.0 - st.rule_t);
        (-20.0 * l, 1.0 - l, -10.0 * r, 1.0 - r)
    };
    let shown = st.line_open || st.line_t > 0.0 || st.rule_t > 0.0;
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    if let Ok((mut n, mut v)) = roots.get_mut(root) {
        let p = off + LINE_AT * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
        let vis = if shown { Visibility::Inherited } else { Visibility::Hidden };
        if *v != vis {
            *v = vis;
        }
    }
    if !shown {
        return;
    }
    if let Ok((mut fc, mut n)) = rule.single_mut() {
        fc.scale = s;
        fc.alpha = ra;
        n.left = Val::Px((RULE_AT.x + rx) * s);
        n.top = Val::Px(RULE_AT.y * s);
    }
    if let Ok((mut t, mut f, mut c, mut sh, mut n)) = line.single_mut() {
        if t.0 != st.line {
            t.0 = st.line.clone();
        }
        let fs = bevy::text::FontSize::Px(TEXT_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        c.0 = TEXT_COLOR.with_alpha(la);
        sh.color = SHADOW.with_alpha(SHADOW.alpha() * la);
        sh.offset = Vec2::splat(1.5 * s);
        n.left = Val::Px((LINE_TXT.x + lx) * s);
        n.top = Val::Px(LINE_TXT.y * s);
        n.width = Val::Px(LINE_W * s);
    }
}
