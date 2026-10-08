//! Keyholes: a shut door whose mesh has a keyhole (`KeyHole` socket) can be looked through.
//! At the door, a tap of [Use] opens it and holding it ("Hold [Use] Keyhole") puts Corvo's eye
//! to the keyhole: the view is from the far side of the door, through the keyhole's shape
//! (the HUD movie's keyhole), at the door's field of view (`m_fKeyHoleFOV`), with the look held
//! near straight through; [Use] leaves ("[Use] Exit Keyhole"). The level scripts hear of it
//! (`DisSeqEvent_KeyHoleUsed`: Used, Unused).

use crate::bindings::{hint, Act, Bindings};
use crate::interact::{Door, InteractFocus, Interaction};
use crate::level::{LevelInfo, LevelInstance};
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;

pub struct KeyholePlugin;

impl Plugin for KeyholePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Peek>()
            .add_systems(OnEnter(GameState::InGame), (|mut p: ResMut<Peek>| *p = Peek::default(), spawn_mask))
            .add_systems(Update, mark_doors.run_if(in_state(GameState::InGame)))
            .add_systems(Update, keyhole_input.after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, keyhole_view.after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// How long [Use] is held for the keyhole (a shorter press opens the door).
const HOLD: f32 = 0.4;
/// How far the eye can turn from straight through (yaw, pitch).
const LOOK: (f32, f32) = (0.45, 0.3);
/// The HUD movie's keyhole shape.
const HUD_KEYHOLE: u32 = 556;

/// A door with a keyhole (index into `scene.keyholes`).
#[derive(Component)]
pub struct KeyholeDoor(pub usize);

/// Looking through a keyhole.
#[derive(Resource, Default)]
pub struct Peek {
    /// the keyhole, the way through it, and the yaw straight through
    pub at: Option<(usize, Vec3, f32)>,
    /// [Use] held at a keyhole door for
    hold: f32,
    /// this press went to the keyhole: its release opens nothing
    pub consumed: bool,
}

#[derive(Component)]
struct Mask;

fn mark_doors(mut commands: Commands, level: Option<Res<LevelInfo>>, doors: Query<(Entity, &LevelInstance), Added<Door>>) {
    let Some(level) = level else { return };
    for (e, li) in &doors {
        if let Some(i) = level.scene.keyholes.iter().position(|k| k.instance == li.index) {
            commands.entity(e).try_insert(KeyholeDoor(i));
        }
    }
}

/// The keyhole's frame: black all round the HUD movie's keyhole shape.
fn spawn_mask(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>) {
    let shape = ui.get(&mut images, "HUD", HUD_KEYHOLE).and_then(|(h, _)| {
        let img = images.get(&h)?;
        let mut out = img.clone();
        let data = out.data.as_mut()?;
        // the shape's alpha (up to ~0.4) becomes the opening
        let max = data.chunks(4).map(|p| p[3]).max().unwrap_or(255).max(1) as f32;
        for p in data.chunks_mut(4) {
            let open = (p[3] as f32 / max).clamp(0.0, 1.0);
            p[0] = 0;
            p[1] = 0;
            p[2] = 0;
            p[3] = ((1.0 - open) * 255.0) as u8;
        }
        Some(images.add(out))
    });
    let black = Color::BLACK;
    commands
        .spawn((
            Mask,
            Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), flex_direction: FlexDirection::Row, ..default() },
            Visibility::Hidden,
            Pickable::IGNORE,
            GlobalZIndex(-1),
            DespawnOnExit(GameState::InGame),
        ))
        .with_children(|c| {
            c.spawn((Node { flex_grow: 1.0, height: percent(100), ..default() }, BackgroundColor(black)));
            match shape {
                // the keyhole fills the height, at its own proportions (356 x 400)
                Some(h) => c.spawn((ImageNode::new(h), Node { height: percent(100), aspect_ratio: Some(356.0 / 400.0), ..default() })),
                None => c.spawn((Node { height: percent(100), aspect_ratio: Some(0.9), ..default() },)),
            };
            c.spawn((Node { flex_grow: 1.0, height: percent(100), ..default() }, BackgroundColor(black)));
        });
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn keyhole_input(
    time: Res<Time>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<Bindings>),
    level: Option<Res<LevelInfo>>,
    mut peek: ResMut<Peek>,
    mut focus: ResMut<InteractFocus>,
    doors: Query<(&Door, &KeyholeDoor, &LevelInstance)>,
    all_doors: Query<(&Door, &LevelInstance)>,
    mut player: Query<(&Transform, &mut Player)>,
    (stats, carry, possession): (Res<crate::gameplay::PlayerStats>, Res<crate::carry::Carry>, Res<crate::possession::Possession>),
    mut mask: Query<&mut Visibility, With<Mask>>,
    mut used: MessageWriter<Interaction>,
) {
    let Some(level) = level else { return };
    let Ok((pt, mut p)) = player.single_mut() else { return };
    let use_key = bind.key(Act::Use);
    let set_vis = |q: &mut Query<&mut Visibility, With<Mask>>, v: Visibility| {
        for mut m in q.iter_mut() {
            if *m != v {
                *m = v;
            }
        }
    };
    // looking through
    if let Some((i, _, _)) = peek.at {
        let k = &level.scene.keyholes[i];
        let opened = all_doors.iter().any(|(d, li)| li.index == k.instance && d.target > 0.5);
        // (leaving it: the context line's, `prompts`)
        focus.0 = true;
        focus.1 = None;
        if keys.just_pressed(use_key) || opened || stats.dead || stats.damage_flash > 0.9 {
            peek.at = None;
            p.locked = false;
            set_vis(&mut mask, Visibility::Hidden);
            used.write(Interaction::Keyhole { instance: k.instance, used: false });
        }
        return;
    }
    if keys.just_pressed(use_key) {
        peek.consumed = false;
        peek.hold = 0.0;
    }
    // at a shut keyhole door
    let Some((door, kd, _)) = focus.1.and_then(|e| doors.get(e).ok()) else {
        peek.hold = 0.0;
        return;
    };
    if door.target > 0.5 || carry.carrying() || possession.host.is_some() {
        return;
    }
    if !focus.2.contains("Keyhole") {
        focus.2 = format!("{}\nHold {} Keyhole", focus.2, hint(Act::Use));
    }
    if keys.pressed(use_key) && !peek.consumed {
        peek.hold += time.delta_secs();
        if peek.hold >= HOLD {
            let k = &level.scene.keyholes[kd.0];
            let n = Vec3::from(k.normal).with_y(0.0).normalize_or(Vec3::X);
            // through the door, away from Corvo
            let through = if n.dot(Vec3::from(k.center) - pt.translation) >= 0.0 { n } else { -n };
            let yaw = (-through.x).atan2(-through.z);
            peek.at = Some((kd.0, through, yaw));
            peek.consumed = true;
            p.locked = true;
            p.velocity = Vec3::ZERO;
            p.yaw = yaw;
            p.pitch = 0.0;
            set_vis(&mut mask, Visibility::Inherited);
            used.write(Interaction::Keyhole { instance: k.instance, used: true });
        }
    }
}

/// The eye at the keyhole: just past the door, looking through, within the keyhole's reach.
fn keyhole_view(
    level: Option<Res<LevelInfo>>,
    peek: Res<Peek>,
    mut player: Query<(&mut Transform, &mut Player), Without<PlayerCamera>>,
    mut cam: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
) {
    let (Some(level), Some((i, through, yaw0))) = (level, peek.at) else { return };
    let Some(k) = level.scene.keyholes.get(i) else { return };
    let Ok((mut pt, mut p)) = player.single_mut() else { return };
    let Ok((mut ct, mut proj)) = cam.single_mut() else { return };
    // the look stays near straight through
    let mut dy = p.yaw - yaw0;
    dy = (dy + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    p.yaw = yaw0 + dy.clamp(-LOOK.0, LOOK.0);
    p.pitch = p.pitch.clamp(-LOOK.1, LOOK.1);
    pt.rotation = Quat::from_rotation_y(p.yaw);
    let eye = Vec3::from(k.center) + through * 0.07;
    ct.translation = Quat::from_rotation_y(-p.yaw) * (eye - pt.translation);
    ct.rotation = Quat::from_rotation_x(p.pitch);
    // the original's field of view is horizontal
    if let Projection::Perspective(pp) = proj.as_mut() {
        let h = k.fov.clamp(30.0, 120.0).to_radians();
        pp.fov = 2.0 * ((h * 0.5).tan() / pp.aspect_ratio.max(0.1)).atan();
    }
}
