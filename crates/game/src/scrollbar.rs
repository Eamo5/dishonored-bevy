//! The scroll bar beside the original's lists whose items overrun them (`ScrollBarList`:
//! `lib_scrollBar`, `ScrollBarBase.InitScrollBar`): its track as tall as the list's mask less an
//! arrow (`_track_mc`, from the up arrow's middle), its arrows at either end dark and bobbing
//! (`over`, `loop_over`), and its lifter (`lib_LS_scroll_Multi` on its PC frame) along the track
//! by how far the list has scrolled (`Scroll`: from 20 below the track's top to 20 above its
//! foot). It stands the mask's width and 5 along the list (or 25 before its origin,
//! `_sbHAlignment` "left"), middled on the mask's height.

use crate::flash::{Clip, FlashClip, Mat, PropsTo};
use bevy::prelude::*;
use dhcook::format::Timelines;
use std::sync::Arc;

/// Where the up arrow stands (`_arrowUp_mc._y`), an arrow's height turned (its `btn` 45.05
/// across), and the track's natural height (`_track_mc`'s bitmap).
const ARROW_Y: f32 = 19.35;
const ARROW_H: f32 = 45.05;
const TRACK_H: f32 = 256.0;

/// A list's bar: the list's mask (width and height), the side it stands (`left`), how far the
/// list has scrolled (0..1).
pub struct Bar {
    pub mask_w: f32,
    pub mask_h: f32,
    pub left: bool,
    pub ratio: f32,
}

/// How far a list of `n` items `pitch` apart has scrolled with `first` of them at its top, in a
/// mask `mask_h` deep (`Scroll`: its offset over what overruns the mask).
pub fn ratio(n: usize, first: usize, pitch: f32, mask_h: f32) -> f32 {
    let over = n as f32 * pitch - mask_h;
    if over <= 0.0 {
        0.0
    } else {
        (first as f32 * pitch / over).clamp(0.0, 1.0)
    }
}

/// The bar under `parent`: the list's origin at `origin` (stage), its turn `m` (the list's
/// matrix, no offset), the stage drawn `s` pixels a unit from `off`.
#[allow(clippy::too_many_arguments)]
pub fn spawn(commands: &mut Commands, parent: Entity, lib: Arc<Timelines>, s: f32, off: Vec2, origin: Vec2, m: Mat, bar: &Bar) -> Option<Entity> {
    let c = Clip::export(&lib, "lib_scrollBar")?;
    let mut fc = FlashClip::new("lib", lib.clone(), c).real();
    fc.scale = s;
    let h = bar.mask_h;
    let track_h = h - ARROW_H;
    fc.set("_track_mc", PropsTo::default().y(ARROW_Y).yscale(track_h / TRACK_H));
    fc.set("_arrowDown_mc", PropsTo::default().y(ARROW_Y + track_h));
    for a in ["_arrowUp_mc", "_arrowDown_mc"] {
        fc.goto(a, "over", true);
    }
    // its lifter (on the track's middle)
    let (lo, hi) = (ARROW_Y + 20.0, ARROW_Y + track_h - 20.0);
    fc.attach("", "lib_LS_scroll_Multi", "_lifter_mc", [1.0, 0.0, 0.0, 1.0, 10.0, lo + bar.ratio.clamp(0.0, 1.0) * (hi - lo)]);
    fc.goto("_lifter_mc", "PC", false);
    // (the mask's width and 5 along, or 25 before; middled on its height: the bar is as tall
    // less 0.25)
    let x = if bar.left { -25.0 } else { bar.mask_w + 5.0 };
    let local = Vec2::new(x, 0.125);
    let p = origin + Vec2::new(m[0] * local.x + m[2] * local.y, m[1] * local.x + m[3] * local.y);
    fc.m = [m[0], m[1], m[2], m[3], 0.0, 0.0];
    let at = off + p * s;
    Some(commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(at.x), top: Val::Px(at.y), ..default() }, Pickable::IGNORE, ChildOf(parent))).id())
}
