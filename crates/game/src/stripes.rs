//! The black stripes over what is seen beyond the menus' stage (the Global movie's
//! `g_BlackStripes`, as roshan's r614/blink research decoded `SetStripes` and its switch): while
//! a screen whose player draws them (`m_bDrawBlackStripesWhileActive`: the main and pause menus,
//! the journal, a note, a store, the mission statistics) is up on a screen wider or narrower than
//! 16:9, the empty space either side of the 1280x720 stage is black; the menus are pillarboxed
//! or letterboxed, not drawn over the game.

use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct StripesPlugin;

impl Plugin for StripesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn).add_systems(Update, stripes.run_if(in_state(GameState::InGame)));
    }
}

/// A stripe: 0 left or top, 1 right or bottom.
#[derive(Component)]
struct Stripe(u8);

fn spawn(mut commands: Commands) {
    for k in 0..2 {
        commands.spawn((Stripe(k), Node { position_type: PositionType::Absolute, ..default() }, BackgroundColor(Color::BLACK), GlobalZIndex(89), Visibility::Hidden, Pickable::IGNORE));
    }
}

#[allow(clippy::too_many_arguments)]
fn stripes(
    window: Query<&Window, With<PrimaryWindow>>,
    menu: Res<crate::menu::Menu>,
    journal: Res<crate::journal::Journal>,
    note: Res<crate::notescreen::NoteScreen>,
    store: Res<crate::store::Store>,
    mission_end: Option<Res<crate::mission::MissionEnd>>,
    mut q: Query<(&Stripe, &mut Node, &mut Visibility)>,
) {
    let Ok(w) = window.single() else { return };
    let ms = crate::hud::MovieSpace::of(w);
    let up = menu.open.is_some() || journal.open || note.open.is_some() || store.open.is_some() || mission_end.is_some();
    // (the empty space either side: across on a wide screen, up and down on a narrow one)
    let wide = ms.off.x >= 0.5;
    let narrow = ms.off.y >= 0.5;
    let show = up && (wide || narrow);
    for (s, mut n, mut v) in &mut q {
        let want = if show { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
        if !show {
            continue;
        }
        let (size, far) = (Vec2::new(w.width(), w.height()), s.0 == 1);
        let (left, top, width, height) = if wide {
            (if far { size.x - ms.off.x } else { 0.0 }, 0.0, ms.off.x, size.y)
        } else {
            (0.0, if far { size.y - ms.off.y } else { 0.0 }, size.x, ms.off.y)
        };
        let set = |v: &mut Val, x: f32| {
            if *v != Val::Px(x) {
                *v = Val::Px(x);
            }
        };
        set(&mut n.left, left);
        set(&mut n.top, top);
        set(&mut n.width, width);
        set(&mut n.height, height);
    }
}
