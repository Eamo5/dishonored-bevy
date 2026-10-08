//! The menus' drifting backdrop (`_common.AnimatedBackground`, on the pause menu's and the
//! journal's `p_bkgd` and the store's `wh_mainBkgd`): its grain (`_white_mc`) fades in to 15%
//! and breathes between 15% and 12.5%; the shards (`_particles_mc`) and blades (`_blades_mc`)
//! fly in from 300-700 x 200-400 out, turned 30-80 degrees and half seen, over 1-2 s; then both
//! layers sway: right 100-150 (blades 100-120), half faded, grown to 110-115% (102.5-107.5%)
//! over 5 s (5.5 s), back left 30-80, whole, 95-105% (95-100%) over 3 s (4 s), and so on,
//! each shard turning a little at every swing. All eased `Strong.easeOut`, in real time (the
//! game is paused under it).

use crate::flash::{concat, Cx, FlashClip, Mat};
use bevy::prelude::*;

pub struct AnimBgPlugin;

impl Plugin for AnimBgPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, animate.before(crate::flash::PlayClips));
    }
}

/// `_minRandomRotationRange`... of the class.
const ROT: (f32, f32) = (-75.5, 37.5);
const ROT_LOOP: (f32, f32) = (-15.5, 7.5);
const XY: (f32, f32) = (-5.0, 5.0);
const X_OPEN: (f32, f32) = (300.0, 700.0);
const Y_OPEN: (f32, f32) = (200.0, 400.0);

/// `mx.transitions.easing.Strong.easeOut`
fn strong_out(k: f32) -> f32 {
    1.0 - (1.0 - k.clamp(0.0, 1.0)).powi(5)
}

/// A tween of a few numbers: from, to, start, seconds.
#[derive(Clone, Copy)]
struct Tw<const N: usize> {
    from: [f32; N],
    to: [f32; N],
    t0: f32,
    dur: f32,
}

impl<const N: usize> Default for Tw<N> {
    fn default() -> Self {
        Tw { from: [0.0; N], to: [0.0; N], t0: 0.0, dur: 0.0 }
    }
}

impl<const N: usize> Tw<N> {
    fn at(&self, t: f32) -> [f32; N] {
        let k = strong_out(if self.dur > 0.0 { (t - self.t0) / self.dur } else { 1.0 });
        std::array::from_fn(|i| self.from[i] + (self.to[i] - self.from[i]) * k)
    }
}

/// A shard: where it is in its layer (depth), how it was placed (x, y, rotation, alpha 0-100),
/// its tween.
struct Elem {
    layer: usize,
    depth: u16,
    m0: Mat,
    cx0: Cx,
    start: [f32; 4],
    tw: Tw<4>,
}

/// A layer (`_particles_mc`, `_blades_mc`): x, y-turn (degrees), alpha, scale (%); when it
/// swings next and which way.
struct Layer {
    name: &'static str,
    m0: Mat,
    cx0: Cx,
    tw: Tw<4>,
    next: f32,
    back: bool,
}

#[derive(Component)]
pub struct AnimatedBackground {
    /// the backdrop clip within the movie clip ("": itself)
    path: String,
    t: f32,
    /// opening until (the shards fly in)
    opened_at: f32,
    elems: Vec<Elem>,
    layers: Vec<Layer>,
    /// the grain: x, alpha
    white: Tw<2>,
    white_m0: Mat,
    white_cx0: Cx,
    seed: u32,
}

impl AnimatedBackground {
    /// The backdrop of a clip, opening (`Open`).
    pub fn new(fc: &FlashClip, path: &str) -> Option<Self> {
        let bg = if path.is_empty() { Some(&fc.clip) } else { fc.clip.child(path) }?;
        let (white_m0, white_cx0) = bg.placed("_white_mc")?;
        let seed = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1) | 1).wrapping_mul(2654435761);
        let mut a = AnimatedBackground { path: path.to_string(), t: 0.0, opened_at: 0.0, elems: Vec::new(), layers: Vec::new(), white: Tw::default(), white_m0, white_cx0, seed };
        for (li, name) in ["_particles_mc", "_blades_mc"].into_iter().enumerate() {
            let Some((m0, cx0)) = bg.placed(name) else { continue };
            a.layers.push(Layer { name, m0, cx0, tw: Tw { from: [0.0, 0.0, 100.0, 100.0], to: [0.0, 0.0, 100.0, 100.0], t0: 0.0, dur: 0.0 }, next: 0.0, back: false });
            let Some(layer) = bg.child(name) else { continue };
            for (depth, m0, cx0) in layer.placements() {
                let start = [m0[4], m0[5], m0[1].atan2(m0[0]).to_degrees(), cx0[3] * 100.0];
                a.elems.push(Elem { layer: li, depth, m0, cx0, start, tw: Tw { from: start, to: start, t0: 0.0, dur: 0.0 } });
            }
        }
        // the shards fly in over 1-2 s
        let d = 0.01 * a.rand(100.0, 200.0);
        a.opened_at = d;
        for i in 0..a.elems.len() {
            let s = a.elems[i].start;
            let x = if s[0] < 0.0 { s[0] + a.rand(-X_OPEN.1, -X_OPEN.0) } else { s[0] + a.rand(X_OPEN.0, X_OPEN.1) };
            let y = if s[1] < 0.0 { s[1] + a.rand(-Y_OPEN.1, -Y_OPEN.0) } else { s[1] + a.rand(Y_OPEN.0, Y_OPEN.1) };
            let rot = a.rand(30.0, 80.0);
            a.elems[i].tw = Tw { from: [x, y, rot, 50.0], to: s, t0: 0.0, dur: d };
        }
        a.white = Tw { from: [0.0, 0.0], to: [0.0, 0.0], t0: 0.0, dur: 0.0 };
        Some(a)
    }

    /// `randRange(min, max)`: `floor(random * (max - min + 1)) + min`
    fn rand(&mut self, min: f32, max: f32) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        let r = (self.seed >> 8) as f32 / (1u32 << 24) as f32;
        (r * (max - min + 1.0)).floor() + min
    }

    /// A layer's next swing (`AnimateParticlesMc`/`_loop`, `AnimateBladesMc`/`_loop`).
    fn swing(&mut self, li: usize) {
        let t = self.t;
        let now = self.layers[li].tw.at(t);
        let particles = self.layers[li].name == "_particles_mc";
        let back = self.layers[li].back;
        let (to, dur, next) = match (particles, back) {
            (true, false) => ([self.rand(100.0, 150.0), self.rand(-10.0, -5.0), 50.0, self.rand(110.0, 115.0)], 5.0, 4.5),
            (true, true) => ([self.rand(-80.0, -30.0), self.rand(0.0, 10.0), 100.0, self.rand(95.0, 105.0)], 3.0, 2.8),
            (false, false) => ([self.rand(100.0, 120.0), self.rand(-10.0, -5.0), 50.0, self.rand(102.5, 107.5)], 5.5, 5.0),
            (false, true) => ([self.rand(-80.0, -30.0), self.rand(0.0, 10.0), 100.0, self.rand(95.0, 100.0)], 4.0, 3.65),
        };
        self.layers[li].tw = Tw { from: now, to, t0: t, dur };
        self.layers[li].next = t + next;
        self.layers[li].back = !back;
        if !particles {
            return;
        }
        // the grain breathes with the shards
        let w = self.white.at(t);
        self.white = if back { Tw { from: w, to: [0.0, 12.5], t0: t, dur } } else { Tw { from: w, to: [w[0] + self.rand(-10.0, 15.0), 15.0], t0: t, dur } };
        // the shards turn a little (not while flying in)
        if t < self.opened_at {
            return;
        }
        for i in 0..self.elems.len() {
            if self.elems[i].layer != li {
                continue;
            }
            let (s, cur) = (self.elems[i].start, self.elems[i].tw.at(t));
            let to = if back {
                [s[0] + self.rand(XY.0, XY.1), s[1] + self.rand(XY.0, XY.1), (s[2] + self.rand(ROT_LOOP.0, ROT_LOOP.1)).clamp(-170.0, 170.0), s[3]]
            } else {
                [cur[0] + self.rand(XY.0, XY.1), cur[1] + self.rand(XY.0, XY.1), (cur[2] + self.rand(ROT.0, ROT.1)).clamp(-170.0, 170.0), s[3]]
            };
            self.elems[i].tw = Tw { from: cur, to, t0: t, dur };
        }
    }
}

/// A rotation (degrees, Flash's: clockwise on screen).
fn rot(deg: f32) -> Mat {
    let (s, c) = deg.to_radians().sin_cos();
    [c, s, -s, c, 0.0, 0.0]
}

fn animate(time: Res<Time<bevy::time::Real>>, mut q: Query<(&mut FlashClip, &mut AnimatedBackground)>) {
    let dt = time.delta_secs().min(0.1);
    for (mut fc, mut a) in &mut q {
        let a = &mut *a;
        a.t += dt;
        for li in 0..a.layers.len() {
            if a.t >= a.layers[li].next {
                a.swing(li);
            }
        }
        let t = a.t;
        let fc = &mut *fc;
        let Some(bg) = (if a.path.is_empty() { Some(&mut fc.clip) } else { fc.clip.child_mut(&a.path) }) else { continue };
        let w = a.white.at(t);
        if let Some((m, cx)) = bg.placed_mut("_white_mc") {
            *m = a.white_m0;
            m[4] += w[0];
            *cx = a.white_cx0;
            cx[3] *= w[1] / 100.0;
        }
        for l in &a.layers {
            let [x, yrot, alpha, scale] = l.tw.at(t);
            if let Some((m, cx)) = bg.placed_mut(l.name) {
                let s = scale / 100.0;
                // (its turn about y seen flat: narrower)
                *m = concat(&[s * yrot.to_radians().cos(), 0.0, 0.0, s, x, 0.0], &l.m0);
                *cx = l.cx0;
                cx[3] *= alpha / 100.0;
            }
        }
        for e in &a.elems {
            let Some(layer) = bg.child_mut(a.layers[e.layer].name) else { continue };
            let [x, y, r, alpha] = e.tw.at(t);
            if let Some((m, cx)) = layer.placed_at_mut(e.depth) {
                let mut lin = e.m0;
                lin[4] = 0.0;
                lin[5] = 0.0;
                *m = concat(&rot(r - e.start[2]), &lin);
                m[4] = x;
                m[5] = y;
                *cx = e.cx0;
                cx[3] = e.cx0[3] * alpha / e.start[3].max(1e-3);
            }
        }
    }
}
