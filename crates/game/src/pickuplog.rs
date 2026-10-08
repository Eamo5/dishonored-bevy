//! The HUD's pickup log (the HUD movie's `PickupLog` class on `hud_pLog_`, at the screen's
//! right): what Corvo picks up comes in at the bottom slot (its name and the item's small icon
//! over a brush stroke, sliding in from 30 px over 0.4 s), the older ones rise a slot (50 px,
//! 0.25 s) and dim (60%, 20%, then gone); each stays 2 s and fades over 0.4 s.

use crate::GameState;
use bevy::prelude::*;
use bevy::ui::widget::NodeImageMode;
use bevy::window::PrimaryWindow;

pub struct PickupLogPlugin;

impl Plugin for PickupLogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PickupLog>()
            .add_systems(OnEnter(GameState::InGame), spawn_log)
            .add_systems(Update, show_log.run_if(in_state(GameState::InGame)));
    }
}

/// The log's place on the HUD's 1280x720 stage (`pickupLog_mc`), its slots' rise and alphas
/// (`_itemProps`), the times (`_fadeInDuration`, `_itemLifeDuration`, `_fadeOutDuration`).
const AT: Vec2 = Vec2::new(1181.4, 286.8);
const RISE: f32 = 50.0;
const ROW_ALPHA: [f32; 4] = [1.0, 0.6, 0.2, 0.0];
const FADE_IN: f32 = 0.4;
const LIFE: f32 = 2.0;
const FADE_OUT: f32 = 0.4;
const MOVE: f32 = 0.25;
/// the slots' brush stroke (the HUD movie's bitmap)
const STROKE: u32 = 490;

/// An entry: its text and icon (`UI_ItemIcons_Small`), age, and row (animated to its place).
struct Entry {
    name: String,
    icon: Option<std::borrow::Cow<'static, str>>,
    age: f32,
    row: f32,
    target: f32,
}

/// What was picked up, for the log.
#[derive(Resource, Default)]
pub struct PickupLog {
    entries: Vec<Entry>,
    queued: Vec<(String, Option<std::borrow::Cow<'static, str>>)>,
}

impl PickupLog {
    pub fn add(&mut self, name: impl Into<String>, icon: Option<&'static str>) {
        self.queued.push((name.into(), icon.map(std::borrow::Cow::Borrowed)));
    }
    /// An entry whose icon is named at run time (a mission item's `m_JournalIconName`).
    pub fn add_named(&mut self, name: impl Into<String>, icon: Option<String>) {
        self.queued.push((name.into(), icon.map(std::borrow::Cow::Owned)));
    }
}

/// An item kind's small icon (`UI_ItemIcons_Small`, the original's names).
pub fn ammo_icon(ty: u8) -> Option<&'static str> {
    Some(match ty {
        0 => "RegularBullets_Small",
        1 => "ExplosiveBullets_Small",
        2 => "CrossbowRegularBolt_Small",
        3 => "CrossbowSleepDart_Small",
        4 => "CrossbowFlareBolt_Small",
        5 => "SpringRazor_Small",
        6 => "CorvoGrenade_Small",
        7 => "CorvoStickyGrenade_Small",
        _ => return None,
    })
}

#[derive(Component)]
struct Slot(usize);
#[derive(Component)]
struct SlotText;
#[derive(Component)]
struct SlotIcon;
#[derive(Component)]
struct SlotStroke;

fn spawn_log(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>, mut log: ResMut<PickupLog>) {
    *log = PickupLog::default();
    let stroke = ui.get(&mut images, "HUD", STROKE).map(|(h, _)| h);
    for i in 0..4 {
        commands
            .spawn((Slot(i), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, GlobalZIndex(4), Pickable::IGNORE, DespawnOnExit(GameState::InGame), crate::hud::HudPart("DHE_Equipment")))
            .with_children(|c| {
                if let Some(s) = &stroke {
                    c.spawn((SlotStroke, ImageNode::new(s.clone()).with_color(Color::srgba(1.0, 1.0, 1.0, 0.0)).with_mode(NodeImageMode::Stretch), Node { position_type: PositionType::Absolute, ..default() }));
                }
                c.spawn((
                    SlotText,
                    Text::new(""),
                    TextFont { font_size: FontSize::Px(18.0), ..default() },
                    TextColor(Color::srgba(0.92, 0.9, 0.82, 0.0)),
                    TextLayout::justify(Justify::Right),
                    Node { position_type: PositionType::Absolute, ..default() },
                ));
                c.spawn((SlotIcon, ImageNode::default().with_color(Color::srgba(1.0, 1.0, 1.0, 0.0)).with_mode(NodeImageMode::Stretch), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden));
            });
    }
}

#[allow(clippy::type_complexity)]
fn show_log(
    (time, settings): (Res<Time<Real>>, Res<crate::settings::Settings>),
    window: Query<&Window, With<PrimaryWindow>>,
    mut log: ResMut<PickupLog>,
    mut ui: ResMut<crate::ui_images::UiImages>,
    mut images: ResMut<Assets<Image>>,
    mut slots: Query<(&Slot, &mut Node, &mut Visibility, &Children)>,
    mut strokes: Query<(&mut Node, &mut ImageNode), (With<SlotStroke>, Without<Slot>, Without<SlotIcon>)>,
    mut texts: Query<(&mut Node, &mut Text, &mut TextColor), (With<SlotText>, Without<Slot>, Without<SlotStroke>, Without<SlotIcon>)>,
    mut icons: Query<(&mut Node, &mut ImageNode, &mut Visibility), (With<SlotIcon>, Without<Slot>, Without<SlotStroke>)>,
) {
    let dt = time.delta_secs();
    let log = &mut *log;
    // (`PSI_HUD_bShowPickupLog` off: none come in)
    if !settings.pickup_log {
        log.queued.clear();
    }
    // a new one comes in at the bottom, the others rise a slot
    for (name, icon) in log.queued.drain(..) {
        for e in &mut log.entries {
            e.target += 1.0;
        }
        log.entries.insert(0, Entry { name, icon, age: 0.0, row: 0.0, target: 0.0 });
    }
    let ease = |x: f32| 1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5);
    for e in &mut log.entries {
        e.age += dt;
        let step = dt / MOVE;
        e.row = if e.row < e.target { (e.row + step).min(e.target) } else { e.row };
    }
    log.entries.retain(|e| e.age < FADE_IN + LIFE + FADE_OUT && e.row < 3.0);
    let Ok(w) = window.single() else { return };
    let size = Vec2::new(w.width(), w.height());
    let s = (size.x / 1280.0).min(size.y / 720.0).max(0.01);
    let off = (size - Vec2::new(1280.0, 720.0) * s) * 0.5;
    for (slot, mut node, mut vis, children) in &mut slots {
        let Some(e) = log.entries.get(slot.0) else {
            *vis = Visibility::Hidden;
            continue;
        };
        *vis = Visibility::Inherited;
        // the slot's own fade (in from 30 px to the right, out at the end of its life) and its
        // row's dimming
        let life = if e.age < FADE_IN {
            ease(e.age / FADE_IN)
        } else if e.age < FADE_IN + LIFE {
            1.0
        } else {
            1.0 - ease((e.age - FADE_IN - LIFE) / FADE_OUT)
        };
        let r = e.row.clamp(0.0, 3.0);
        let (lo, hi) = (r.floor() as usize, (r.ceil() as usize).min(3));
        let row_alpha = ROW_ALPHA[lo] + (ROW_ALPHA[hi] - ROW_ALPHA[lo]) * (r - r.floor());
        let a = life * row_alpha;
        let slide = if e.age < FADE_IN { 30.0 * (1.0 - ease(e.age / FADE_IN)) } else { 0.0 };
        let p = off + (AT + Vec2::new(slide, -RISE * r - 24.1)) * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        let place = |n: &mut Node, min: Vec2, size: Vec2| {
            n.left = Val::Px(min.x * s);
            n.top = Val::Px(min.y * s);
            n.width = Val::Px(size.x * s);
            n.height = Val::Px(size.y * s);
        };
        for c in children.iter() {
            if let Ok((mut n, mut im)) = strokes.get_mut(c) {
                place(&mut n, Vec2::new(-201.15, -22.15), Vec2::new(271.0, 74.0));
                im.color.set_alpha(a);
            }
            if let Ok((mut n, mut t, mut col)) = texts.get_mut(c) {
                place(&mut n, Vec2::new(-275.0, -10.0), Vec2::new(270.0, 30.0));
                if t.0 != e.name {
                    t.0 = e.name.clone();
                }
                col.0.set_alpha(a);
            }
            if let Ok((mut n, mut im, mut iv)) = icons.get_mut(c) {
                place(&mut n, Vec2::new(29.7 - 25.0, 12.0 - 25.0), Vec2::new(50.0, 50.0));
                match e.icon.as_deref().and_then(|i| ui.file(&mut images, "itemsmall", i)) {
                    Some((h, _)) => {
                        if im.image != h {
                            im.image = h;
                        }
                        im.color.set_alpha(a);
                        *iv = Visibility::Inherited;
                    }
                    None => *iv = Visibility::Hidden,
                }
            }
        }
    }
}
