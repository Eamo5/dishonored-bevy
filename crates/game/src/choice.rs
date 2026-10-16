//! The level scripts' choices (`DisSeqAct_DialogScriptedChoice`): going on to the next area
//! or staying, and the story's decisions ("[I'll go to sleep now.]", "[Infect Bootleg
//! Elixir]"). The options are listed on screen; as the HUD movie's menu takes them on PC
//! (`InputsManager`, `SelectionHandler`, as roshan's r614/blink research decoded them), Up and
//! Down move a line at a time (`Move`; `Stop` at the ends, no wrap), Enter (the pad's A)
//! answers (`Validation`, the answer 150 ms on, the inputs off meanwhile; the number keys are
//! the movie's dock bar, not the menu's), and the script carries on down that output. Walking
//! away leaves it unanswered.

use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct ChoicePlugin;

impl Plugin for ChoicePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Choice>()
            .init_resource::<ui::ChoiceUi>()
            .add_systems(OnExit(GameState::InGame), |mut c: ResMut<Choice>, mut u: ResMut<ui::ChoiceUi>| {
                *c = Choice::default();
                *u = ui::ChoiceUi::default();
            })
            .add_systems(Update, (choose, ui::draw_choice).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The choice on screen: the op, its options (output index, text), the one highlighted, and
/// where Corvo stood when it was put to him.
#[derive(Resource, Default)]
pub struct Choice {
    pub pending: Option<(u32, Vec<(usize, String)>)>,
    sel: usize,
    at: Option<Vec3>,
    /// answered: the option, and the time before the answer goes to the script
    confirmed: Option<(usize, f32)>,
}

/// From the answer to the script (`OnPlayerChoiceConfirm` 150 ms after `Validation`).
const CONFIRM_DELAY: f32 = 0.15;

/// A line down or up among `n` (`SelectionHandler`: it stays at the ends).
fn step(sel: usize, n: usize, down: bool) -> usize {
    if down { (sel + 1).min(n.saturating_sub(1)) } else { sel.saturating_sub(1) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_selection_stops_at_the_ends() {
        assert_eq!(super::step(0, 3, false), 0);
        assert_eq!(super::step(0, 3, true), 1);
        assert_eq!(super::step(2, 3, true), 2);
        assert_eq!(super::step(2, 3, false), 1);
    }
}

impl Choice {
    pub fn ask(&mut self, op: u32, options: Vec<(usize, String)>) {
        self.pending = Some((op, options));
        self.sel = 0;
        self.at = None;
        self.confirmed = None;
    }

    /// The option highlighted.
    pub fn selected(&self) -> usize {
        self.sel
    }
}

#[allow(clippy::too_many_arguments)]
fn choose(
    (keys, time): (Res<ButtonInput<KeyCode>>, Res<Time>),
    mut choice: ResMut<Choice>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    player: Query<&Transform, With<Player>>,
    paused: Res<crate::hud::Paused>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
) {
    let pos = player.single().map(|t| t.translation).ok();
    if choice.pending.is_some() && !paused.0 {
        if choice.at.is_none() {
            choice.at = pos;
        }
        let n = choice.pending.as_ref().map(|p| p.1.len()).unwrap_or(0);
        // answered: to the script once the delay is out
        if let Some((i, left)) = choice.confirmed {
            let left = left - time.delta_secs();
            if left > 0.0 {
                choice.confirmed = Some((i, left));
                return;
            }
            choice.confirmed = None;
            if let (Some(mut vm), Some((op, opts))) = (vm, choice.pending.take()) {
                if let Some((out, _)) = opts.get(i) {
                    vm.choose(op, *out);
                }
            }
            return;
        }
        let ui = |sfx: &mut MessageWriter<crate::audio::PostEvent>, event: &str| {
            sfx.write(crate::audio::PostEvent::named(event, None));
        };
        for (key, down) in [(KeyCode::ArrowDown, true), (KeyCode::ArrowUp, false)] {
            if keys.just_pressed(key) {
                let to = step(choice.sel, n, down);
                ui(&mut sfx, if to != choice.sel { "UI_Move" } else { "UI_Stop" });
                choice.sel = to;
            }
        }
        let mut pick = (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter)).then_some(choice.sel);
        // test runs answer at once (`DH_CHOICE`: which, 1-based; default the first)
        if std::env::var("DH_SCRIPT").is_ok() && std::env::var("DH_CHOICE").as_deref() != Ok("show") {
            pick = Some(std::env::var("DH_CHOICE").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).saturating_sub(1).min(n.saturating_sub(1)));
        }
        if let (Some(i), true) = (pick, vm.is_some()) {
            choice.sel = i;
            choice.confirmed = Some((i, CONFIRM_DELAY));
            ui(&mut sfx, "UI_Validation");
        } else if let (Some(a), Some(p)) = (choice.at, pos) {
            // walked away: no answer
            if a.distance(p) > 6.0 {
                choice.pending = None;
            }
        }
    }
}

/// The choice as the HUD movie draws it (`hud_playerChoice`, class `PlayerChoice`, a
/// `GenericMenu`): the options stacked up from the bottom left of the middle (each a
/// `hud_plC_button`: a dim circle and the option in the body font at 23), the arc beside them
/// (`_indic_mc`); the highlighted one plays `over` (its circle grows bright, a bar slides in)
/// and the arc's `arche_mc` turns to it.
mod ui {
    use super::Choice;
    use crate::flash::{Clip, FlashClip, MovieTimelines};
    use crate::GameState;
    use bevy::prelude::*;
    use bevy::window::PrimaryWindow;

    const MOVIE: &str = "HUD";
    /// `playerChoice_mc` on the stage (at 95%)
    const AT: Vec2 = Vec2::new(473.0, 665.0);
    const SCALE: f32 = 0.95;
    /// a button's height and the gap (`_offsetBtnY`), the buttons' x (past the arc)
    const BTN_H: f32 = 28.6;
    const GAP: f32 = 2.5;
    const BTN_X: f32 = 20.0;
    /// the label (`_txt_mc.txt`, inside its gutter), size, colour
    const LABEL_AT: Vec2 = Vec2::new(8.9, -10.35);
    const LABEL_SIZE: f32 = 23.0;
    const LABEL_COLOR: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
    const SHADOW: Color = Color::srgba(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0, 0.9);

    #[derive(Resource, Default)]
    pub struct ChoiceUi {
        root: Option<Entity>,
        shown: Vec<String>,
        sel: Option<usize>,
        /// the letterbox while a choice is up (the choice state's `m_bUseLetterboxing`: the
        /// HUD movie's `hud_blackStripes`, 70 stage units top and bottom)
        stripes: Option<Entity>,
    }

    #[derive(Component)]
    pub struct Stripes;

    #[derive(Component)]
    pub struct Menu;
    #[derive(Component)]
    pub struct Button(usize);
    #[derive(Component)]
    pub struct Label(usize);

    #[allow(clippy::type_complexity)]
    pub fn draw_choice(
        mut commands: Commands,
        choice: Res<Choice>,
        mut ui: ResMut<ChoiceUi>,
        mut timelines: ResMut<MovieTimelines>,
        window: Query<&Window, With<PrimaryWindow>>,
        hud: Query<Entity, With<crate::hud::HudRoot>>,
        mut menus: Query<(&mut FlashClip, &mut Node), (With<Menu>, Without<Button>, Without<Label>, Without<Stripes>)>,
        mut buttons: Query<(&Button, &mut FlashClip, &mut Node), (Without<Menu>, Without<Label>, Without<Stripes>)>,
        mut labels: Query<(&Label, &mut TextFont, &mut Node), (Without<Menu>, Without<Button>, Without<Stripes>)>,
        (stripes, mut bars): (Query<&Children, With<Stripes>>, Query<&mut Node, (Without<Stripes>, Without<Menu>, Without<Button>, Without<Label>)>),
    ) {
        let options: Vec<String> = choice.pending.as_ref().map(|p| p.1.iter().map(|(_, t)| t.trim().trim_start_matches('[').trim_end_matches(']').trim().to_string()).collect()).unwrap_or_default();
        // a new set of options: the menu built afresh (`SetMenu`)
        if options != ui.shown {
            if let Some(r) = ui.root.take() {
                commands.entity(r).try_despawn();
            }
            if let Some(r) = ui.stripes.take() {
                commands.entity(r).try_despawn();
            }
            ui.shown = options.clone();
            ui.sel = None;
            if options.is_empty() {
                return;
            }
            let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
            // (its two stripes are plain black fills, 1280 x 70 at the stage's top and bottom)
            ui.stripes = Some(
                commands
                    .spawn((Stripes, Node { position_type: PositionType::Absolute, ..default() }, ZIndex(-1), Pickable::IGNORE, ChildOf(h), DespawnOnExit(GameState::InGame)))
                    .with_children(|r| {
                        for _ in 0..2 {
                            r.spawn((Node { position_type: PositionType::Absolute, ..default() }, BackgroundColor(Color::BLACK), Pickable::IGNORE));
                        }
                    })
                    .id(),
            );
            let Some(menu) = Clip::export(&tl, "hud_playerChoice") else { return };
            let root = commands
                .spawn((Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE, ChildOf(h), DespawnOnExit(GameState::InGame)))
                .with_children(|r| {
                    let mut m = FlashClip::new(MOVIE, tl.clone(), menu);
                    // (no title: only the arc)
                    m.set_visible("_title_mc.ttip", false);
                    r.spawn((Menu, m, Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                    for (i, o) in options.iter().enumerate() {
                        if let Some(b) = Clip::export(&tl, "hud_plC_button") {
                            r.spawn((Button(i), FlashClip::new(MOVIE, tl.clone(), b), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                        }
                        r.spawn((
                            Label(i),
                            Text::new(o.clone()),
                            TextFont::default(),
                            TextColor(LABEL_COLOR),
                            TextShadow { offset: Vec2::splat(1.5), color: SHADOW },
                            TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap),
                            Node { position_type: PositionType::Absolute, ..default() },
                            Pickable::IGNORE,
                        ));
                    }
                })
                .id();
            ui.root = Some(root);
            return;
        }
        let Some(root) = ui.root else { return };
        let Ok(w) = window.single() else { return };
        let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01) * SCALE;
        let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * (s / SCALE)) * 0.5;
        let n = options.len() as f32;
        // stacked up from the bottom; the arc at their middle
        let height = n * BTN_H + (n - 1.0) * GAP;
        let top = -height;
        // (`playerChoice_mc` at the stripes' top, 720 - `_stripeH`, as roshan's r614/blink research
        // decoded the HUD's layout)
        let p = off + Vec2::new(AT.x, 720.0 - crate::hud::STRIPE_H) * (s / SCALE);
        commands.entity(root).insert(Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() });
        // the letterbox across the window, at the stage's 0 and 650 (70 tall), each out to the
        // window's edge over what is seen beyond the stage (`BlackStripes.SetStripesPosition`:
        // 70 + the empty space high)
        if let Some(kids) = ui.stripes.and_then(|e| stripes.get(e).ok()) {
            let h = crate::hud::STRIPE_H * s / SCALE;
            for (i, k) in kids.iter().enumerate() {
                if let Ok(mut n) = bars.get_mut(k) {
                    n.left = Val::Px(0.0);
                    n.width = Val::Px(w.width());
                    n.height = Val::Px(h + off.y);
                    n.top = Val::Px(if i == 0 { 0.0 } else { w.height() - off.y - h });
                }
            }
        }
        if let Ok((mut m, mut mn)) = menus.single_mut() {
            m.scale = s;
            // `_title_mc` sits at the buttons' middle
            let ty = top + height * 0.5;
            mn.left = Val::Px(0.0);
            mn.top = Val::Px((ty + 102.55) * s);
        }
        let sel = choice.selected();
        for (b, mut fc, mut bn) in &mut buttons {
            fc.scale = s;
            bn.left = Val::Px(BTN_X * s);
            bn.top = Val::Px((top + b.0 as f32 * (BTN_H + GAP) + BTN_H * 0.5) * s);
            // `playSelectionOn` / `playSelectionOut`
            if ui.sel != Some(sel) {
                if b.0 == sel {
                    fc.goto("", "over", true);
                } else if ui.sel == Some(b.0) {
                    fc.goto("", "out", true);
                }
            }
            // (the A glyph, `lib_A_Multi` forced to its Xenon frame on PC, on the one chosen
            // only while a pad is in use: `bUsingGamepad`)
            if fc.clip.child("_a_mc").is_some_and(|a| a.frame == 0) {
                fc.goto("_a_mc", "Xenon", false);
            }
            fc.set_visible("_a_mc", b.0 == sel && crate::pad::using());
        }
        if ui.sel != Some(sel) {
            if let Ok((mut m, _)) = menus.single_mut() {
                let tl = m.tl.clone();
                if let Some(c) = m.clip.child_mut("_title_mc._indic_mc.arche_mc") {
                    c.goto(&tl, 1, true);
                }
            }
            ui.sel = Some(sel);
        }
        for (l, mut f, mut ln) in &mut labels {
            let fs = bevy::text::FontSize::Px(LABEL_SIZE * s);
            if f.font_size != fs {
                f.font_size = fs;
            }
            ln.left = Val::Px((BTN_X + LABEL_AT.x) * s);
            ln.top = Val::Px((top + l.0 as f32 * (BTN_H + GAP) + BTN_H * 0.5 + LABEL_AT.y) * s);
        }
    }
}

