//! The HUD's target card (the HUD movie's `TargetNotification` on `hud_tgt`, at the top right):
//! a target's portrait in its red stroked frame over a brush stroke, the target's name, and what
//! became of them ("TARGET ASSASSINATED" in red, "NEUTRALIZED", "RESCUED", "SPARED"), with its
//! sound (`UI_H_target*`); it fades in over 0.25 s, stays `m_fTargetNotificationDuration` (5 s)
//! and fades out.

use crate::GameState;
use bevy::prelude::*;
use bevy::ui::widget::NodeImageMode;
use bevy::window::PrimaryWindow;

pub struct TargetCardPlugin;

impl Plugin for TargetCardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Card>()
            .add_systems(OnEnter(GameState::InGame), spawn_card)
            .add_systems(Update, show_card.run_if(in_state(GameState::InGame)));
    }
}

const AT: Vec2 = Vec2::new(1184.0, 54.0);
const DURATION: f32 = 5.0;
const FADE: f32 = 0.25;
/// the card's brush stroke, the portrait's two frame strokes (the HUD movie's bitmaps)
const STROKE: u32 = 1;
const FRAME0: u32 = 545;
const FRAME1: u32 = 548;

/// (`t_targetAssassinated` .. `t_targetSpared`), their colours and sounds
const TEXTS: [&str; 4] = ["Target assassinated", "Target neutralized", "Target rescued", "Target spared"];
const COLORS: [u32; 4] = [8524821, 6575694, 4936975, 6575694];
const SOUNDS: [&str; 4] = ["UI_H_targetAssassinated", "UI_H_targetNeutralized", "UI_H_targetRescued", "UI_H_targetNeutralized"];

#[derive(Resource, Default)]
struct Card {
    queue: Vec<(String, String, u8)>,
    shown: Option<f32>,
    /// the shown target has a portrait
    portrait: bool,
}

#[derive(Component)]
struct CardRoot;
/// a part: top-left and size on the stage, relative to the card
#[derive(Component)]
struct Part(Vec2, Vec2);
#[derive(Component)]
struct Portrait;
#[derive(Component)]
struct NameText;
#[derive(Component)]
struct StateText;

fn spawn_card(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>, mut card: ResMut<Card>) {
    *card = Card::default();
    let mut img = |id: u32| ui.get(&mut images, "HUD", id).map(|(h, _)| h);
    let (stroke, f0, f1) = (img(STROKE), img(FRAME0), img(FRAME1));
    commands
        .spawn((CardRoot, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, GlobalZIndex(4), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
        .with_children(|c| {
            let node = || Node { position_type: PositionType::Absolute, ..default() };
            if let Some(s) = stroke {
                c.spawn((Part(Vec2::new(-613.6, -14.65), Vec2::new(557.0, 245.0)), ImageNode::new(s).with_mode(NodeImageMode::Stretch), node()));
            }
            // the portrait (its middle at the portrait clip's place) and its frame strokes
            let mid = Vec2::new(-160.3, 100.45);
            c.spawn((Portrait, Part(mid + Vec2::new(-16.65, 10.35) - Vec2::splat(80.0), Vec2::splat(160.0)), ImageNode::default().with_mode(NodeImageMode::Stretch), node()));
            if let Some(f) = f1 {
                c.spawn((Part(mid + Vec2::new(-10.4, -0.7) - Vec2::new(89.0, 100.0), Vec2::new(178.0, 200.0)), ImageNode::new(f).with_mode(NodeImageMode::Stretch), node()));
            }
            if let Some(f) = f0 {
                c.spawn((Part(mid + Vec2::new(-17.35, 8.9) - Vec2::new(84.0, 95.0), Vec2::new(168.0, 190.0)), ImageNode::new(f).with_mode(NodeImageMode::Stretch), node()));
            }
            c.spawn((
                StateText,
                Part(Vec2::new(-708.0, 72.0), Vec2::new(460.0, 40.0)),
                Text::new(""),
                TextFont { font_size: FontSize::Px(30.0), ..default() },
                TextColor(Color::WHITE),
                TextLayout::justify(Justify::Right),
                node(),
            ));
            c.spawn((
                NameText,
                Part(Vec2::new(-520.0, 116.0), Vec2::new(266.0, 30.0)),
                Text::new(""),
                TextFont { font_size: FontSize::Px(22.0), ..default() },
                TextColor(Color::srgb(0.92, 0.9, 0.82)),
                TextLayout::justify(Justify::Right),
                node(),
            ));
        });
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn show_card(
    time: Res<Time<Real>>,
    window: Query<&Window, With<PrimaryWindow>>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    mut card: ResMut<Card>,
    mut ui: ResMut<crate::ui_images::UiImages>,
    mut images: ResMut<Assets<Image>>,
    mut root: Query<(&mut Node, &mut Visibility), With<CardRoot>>,
    mut parts: Query<(&Part, &mut Node, Option<&mut ImageNode>, Option<&mut TextColor>, Option<&mut Text>, Has<Portrait>, Has<NameText>, Has<StateText>), Without<CardRoot>>,
    mut sounds: MessageWriter<crate::audio::PostEvent>,
) {
    if let Some(mut vm) = vm {
        card.queue.append(&mut vm.target_cards);
    }
    let dt = time.delta_secs();
    // the next one once this one is done
    if card.shown.is_none_or(|t| t >= DURATION + 2.0 * FADE) && !card.queue.is_empty() {
        let (name, portrait, kind) = card.queue.remove(0);
        let k = kind.min(3) as usize;
        sounds.write(crate::audio::PostEvent::named(SOUNDS[k], None));
        let portrait_img = (!portrait.is_empty()).then(|| ui.file(&mut images, "portraits", &portrait)).flatten();
        let c = COLORS[k];
        let color = Color::srgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8);
        for (_, _, im, col, text, is_portrait, is_name, is_state) in &mut parts {
            if is_portrait {
                if let (Some(mut im), Some((h, _))) = (im, portrait_img.clone()) {
                    im.image = h;
                }
            } else if is_name {
                if let Some(mut t) = text {
                    t.0 = name.clone();
                }
            } else if is_state {
                if let Some(mut t) = text {
                    t.0 = TEXTS[k].to_uppercase();
                }
                if let Some(mut col) = col {
                    col.0 = color;
                }
            }
        }
        card.portrait = portrait_img.is_some();
        card.shown = Some(0.0);
    }
    let Ok((mut node, mut vis)) = root.single_mut() else { return };
    let Some(t) = card.shown.as_mut() else {
        *vis = Visibility::Hidden;
        return;
    };
    *t += dt;
    let t = *t;
    if t >= DURATION + 2.0 * FADE {
        *vis = Visibility::Hidden;
        if card.queue.is_empty() {
            card.shown = None;
        }
        return;
    }
    *vis = Visibility::Inherited;
    let a = if t < FADE { 1.0 - (1.0 - t / FADE).powi(5) } else if t > DURATION + FADE { 1.0 - (t - DURATION - FADE) / FADE } else { 1.0 };
    let Ok(w) = window.single() else { return };
    let size = Vec2::new(w.width(), w.height());
    let s = (size.x / 1280.0).min(size.y / 720.0).max(0.01);
    let off = (size - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let p = off + AT * s;
    node.left = Val::Px(p.x);
    node.top = Val::Px(p.y);
    let has_portrait = card.portrait;
    for (part, mut n, im, col, _, is_portrait, _, _) in &mut parts {
        n.left = Val::Px(part.0.x * s);
        n.top = Val::Px(part.0.y * s);
        n.width = Val::Px(part.1.x * s);
        n.height = Val::Px(part.1.y * s);
        if let Some(mut im) = im {
            im.color.set_alpha(if is_portrait && !has_portrait { 0.0 } else { a });
        }
        if let Some(mut col) = col {
            col.0.set_alpha(a);
        }
    }
}
