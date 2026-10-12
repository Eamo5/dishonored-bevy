//! The HUD movie's lines of text at the bottom of the screen, placed and styled as its fields
//! are: the subtitles (`subtitles_mc._txt`: the body font at 24, `#e8f7db`, centred in a box
//! from 582 units down), above them the game messages (`gameMsg_mc`: 24, `#e3f2d6`, from 522
//! down) and above those the tutorial (`tutorialMsg_mc`, class `TutorialMessage`: fading in
//! 0.35 s as it settles from 110%, out in 0.15 s); all scaled with the stage. Their fields'
//! dark drop-shadow filters (`softshadow`): the subtitles' blurred 8 at strength 1.9, the
//! game messages' 5 at 1.5, the tutorial's 5 at 1.2 a unit off.

use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use crate::softshadow::SoftShadow;

pub struct HudTextPlugin;

impl Plugin for HudTextPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (place_hud_text, tutorial_line).run_if(in_state(GameState::InGame)));
    }
}

/// a field: its box's top-left and width on the stage, size, colour, and its drop shadow's
/// blur, distance and strength
struct Field {
    at: Vec2,
    width: f32,
    size: f32,
    color: Color,
    shadow: (f32, f32, f32),
}

/// `subtitles_mc` (640, 665) + `_txt` (-289, -84.6), inside its 2-unit gutter
const SUBTITLES: Field = Field { at: Vec2::new(353.0, 582.4), width: 578.0, size: 24.0, color: Color::srgb(232.0 / 255.0, 247.0 / 255.0, 219.0 / 255.0), shadow: (8.0, 0.0, 1.9) };
/// `gameMsg_mc` (640, 665) + `_gameMsg_mc` (0, -145.15) + `txt` (-288.85, 2)
const MESSAGES: Field = Field { at: Vec2::new(353.15, 523.85), width: 577.65, size: 24.0, color: Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0), shadow: (5.0, 0.0, 1.5) };
/// `tutorialMsg_mc` (640, 665) + `_txt_mc` (0, -174.65) + `txt` (-378.45, 2)
const TUTORIAL: Field = Field { at: Vec2::new(263.55, 494.35), width: 752.95, size: 24.0, color: Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0), shadow: (5.0, 1.0, 1.2) };
/// the fields' filter colour
pub const SHADOW: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);

/// A field's drop shadow at the stage's scale `s`.
fn shadow_of(field: &Field, s: f32) -> SoftShadow {
    SoftShadow::flash(SHADOW, field.shadow.0, field.shadow.1, field.shadow.2, s)
}

#[allow(clippy::type_complexity)]
fn place_hud_text(
    mut commands: Commands,
    window: Query<&Window, With<PrimaryWindow>>,
    mut subs: Query<(Entity, &mut Node, &mut TextFont, &mut TextColor, Option<&SoftShadow>), (With<crate::kismet::SubtitleText>, Without<crate::hud::MessagesText>)>,
    mut msgs: Query<(Entity, &mut Node, &mut TextFont, &mut TextColor, Option<&SoftShadow>), (With<crate::hud::MessagesText>, Without<crate::kismet::SubtitleText>)>,
) {
    let Ok(w) = window.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let mut place = |e: Entity, n: &mut Node, f: &mut TextFont, c: &mut TextColor, shadow: Option<&SoftShadow>, field: &Field| {
        let p = off + field.at * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
        n.bottom = Val::Auto;
        n.width = Val::Px(field.width * s);
        let fs = bevy::text::FontSize::Px(field.size * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        // (keeping what fades them)
        let want = field.color.with_alpha(c.0.alpha());
        if c.0 != want {
            c.0 = want;
        }
        // (its shadow fades with it: `softshadow`)
        let sh = shadow_of(field, s);
        if shadow.is_none_or(|x| *x != sh) {
            commands.entity(e).remove::<TextShadow>().insert(sh);
        }
    };
    for (e, mut n, mut f, mut c, sh) in &mut subs {
        place(e, &mut n, &mut f, &mut c, sh, &SUBTITLES);
    }
    for (e, mut n, mut f, mut c, sh) in &mut msgs {
        place(e, &mut n, &mut f, &mut c, sh, &MESSAGES);
    }
}

#[derive(Component)]
struct TutorialText;

/// The tutorial up (`SetTutorialMessage`): opened as it changes, closed as it runs out.
#[allow(clippy::type_complexity)]
fn tutorial_line(
    mut commands: Commands,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    (msgs, settings): (Res<crate::gameplay::HudMessages>, Res<crate::settings::Settings>),
    mut line: Query<(&mut Text, &mut Node, &mut TextFont, &mut TextColor, &mut UiTransform, &mut SoftShadow), With<TutorialText>>,
) {
    let Ok(w) = window.single() else { return };
    let Ok((mut text, mut n, mut f, mut c, mut tf, mut sh)) = line.single_mut() else {
        if let Ok(h) = hud.single() {
            commands.spawn((
                TutorialText,
                Text::new(""),
                TextFont::default(),
                TextColor(TUTORIAL.color.with_alpha(0.0)),
                TextLayout::justify(Justify::Center),
                shadow_of(&TUTORIAL, 1.0),
                Node { position_type: PositionType::Absolute, ..default() },
                UiTransform::default(),
                Pickable::IGNORE,
                ChildOf(h),
            ));
        }
        return;
    };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let p = off + TUTORIAL.at * s;
    n.left = Val::Px(p.x);
    n.top = Val::Px(p.y);
    n.width = Val::Px(TUTORIAL.width * s);
    let fs = bevy::text::FontSize::Px(TUTORIAL.size * s);
    if f.font_size != fs {
        f.font_size = fs;
    }
    let ease = |x: f32| 1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5);
    let (a, k) = match msgs.tutorial.as_ref().filter(|_| settings.tutorials) {
        Some((t, left, shown)) => {
            if text.0 != *t {
                text.0 = t.clone();
            }
            let a = if *left < 0.15 { ease(1.0 - left / 0.15) * -1.0 + 1.0 } else { ease(shown / 0.35) };
            (a, 1.1 - 0.1 * ease(shown / 0.15))
        }
        None => {
            // (closed: nothing left showing, its shadow neither)
            if !text.0.is_empty() {
                text.0.clear();
            }
            (0.0, 1.0)
        }
    };
    c.0 = TUTORIAL.color.with_alpha(a);
    let want = shadow_of(&TUTORIAL, s);
    if *sh != want {
        *sh = want;
    }
    tf.scale = Vec2::splat(k);
}
