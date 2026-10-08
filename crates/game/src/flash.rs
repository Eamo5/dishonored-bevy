//! A player for the interface movies' animated clips as their Flash timelines have them
//! (`ui/<movie>/timeline.json`, cooked by `dhtool cook-ui`): each frame's display list
//! changes (bitmaps and nested clips placed, moved, recoloured and removed), its labels and
//! its scripts' `stop()`, `play()`, `gotoAndPlay/Stop` and `this._visible`. The game drives
//! clips the way the original's code does (`gotoAndPlay("fillIn")` on a named clip within)
//! and they play at the movie's frame rate, nested clips on their own.
//!
//! A clip is a UI node (placed by its owner) whose bitmaps are drawn as its children with
//! their colour transforms; a mask cuts the bitmaps under it to its rectangle (where they are
//! square to it, as interface bars and reveals are).

use bevy::asset::embedded_asset;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use dhcook::format::{Timelines, TlAction, TlOp};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub struct FlashPlugin;

impl Plugin for FlashPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "flash.wgsl");
        app.add_plugins(UiMaterialPlugin::<FlashFill>::default())
            .init_resource::<MovieTimelines>()
            // (after the game has driven its clips, before the interface's layout)
            .add_systems(PostUpdate, play_clips.in_set(PlayClips).before(bevy::ui::UiSystems::Prepare));
    }
}

/// The clips' playing and drawing (what drives them from code goes before).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlayClips;

/// A Flash `_rotation` (degrees, clockwise) and `_xscale`/`_yscale`.
pub fn turn_scale(deg: f32, sx: f32, sy: f32) -> Mat {
    let (sn, cs) = deg.to_radians().sin_cos();
    [cs * sx, sn * sx, -sn * sy, cs * sy, 0.0, 0.0]
}

/// A bitmap with its colour transform.
#[derive(AsBindGroup, Asset, TypePath, Clone, Debug)]
pub struct FlashFill {
    #[uniform(0)]
    mult: Vec4,
    /// (0..1)
    #[uniform(1)]
    add: Vec4,
    /// the part of the bitmap shown: u0, v0, u1, v1
    #[uniform(2)]
    uv: Vec4,
    /// (tiles, -, -, -)
    #[uniform(3)]
    opts: Vec4,
    #[texture(4)]
    #[sampler(5)]
    texture: Handle<Image>,
}

impl UiMaterial for FlashFill {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/flash.wgsl".into()
    }
}

/// The movies' cooked timelines, loaded when first asked for.
#[derive(Resource, Default)]
pub struct MovieTimelines(HashMap<String, Option<Arc<Timelines>>>);

impl MovieTimelines {
    pub fn get(&mut self, movie: &str) -> Option<Arc<Timelines>> {
        self.0
            .entry(movie.to_string())
            .or_insert_with(|| {
                let path = crate::loading::cache_dir().join("ui").join(movie).join("timeline.json");
                let bytes = std::fs::read(&path).ok()?;
                match serde_json::from_slice::<Timelines>(&bytes) {
                    Ok(t) => Some(Arc::new(t)),
                    Err(e) => {
                        warn!("{}: {e}", path.display());
                        None
                    }
                }
            })
            .clone()
    }
}

/// A Flash matrix: x' = a x + c y + tx, y' = b x + d y + ty.
pub type Mat = [f32; 6];
/// A colour transform: multiply (rgba), then add (rgba, 0..255).
pub type Cx = [f32; 8];

pub const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
pub const NO_CX: Cx = [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];

/// `outer` after `inner`.
pub fn concat(o: &Mat, i: &Mat) -> Mat {
    [
        o[0] * i[0] + o[2] * i[1],
        o[1] * i[0] + o[3] * i[1],
        o[0] * i[2] + o[2] * i[3],
        o[1] * i[2] + o[3] * i[3],
        o[0] * i[4] + o[2] * i[5] + o[4],
        o[1] * i[4] + o[3] * i[5] + o[5],
    ]
}

fn concat_cx(o: &Cx, i: &Cx) -> Cx {
    let mut c = NO_CX;
    for k in 0..4 {
        c[k] = i[k] * o[k];
        c[4 + k] = i[4 + k] * o[k] + o[4 + k];
    }
    c
}

#[derive(Clone, Debug)]
struct Placed {
    id: u16,
    name: Option<String>,
    m: Mat,
    cx: Cx,
    /// a mask over the depths up to this one (it is not drawn)
    mask: Option<u16>,
    child: Option<Box<Clip>>,
}

/// A mask: the inverse of its frame (from the root's space) and its rectangle in that frame.
#[derive(Clone, Copy, Debug)]
struct Mask {
    inv: Mat,
    rect: [f32; 4],
}

/// The inverse of a matrix.
pub fn invert(m: &Mat) -> Mat {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return IDENTITY;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    [a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])]
}

/// A leaf cut to a mask's rectangle (where the two are square to each other; else as it is).
fn clip_leaf(mut l: Leaf, mask: &Mask) -> Option<Leaf> {
    let r = concat(&mask.inv, &l.m);
    if r[1].abs() > 1e-3 || r[2].abs() > 1e-3 || r[0] <= 0.0 || r[3] <= 0.0 {
        return Some(l);
    }
    let (w, h) = (l.size.x, l.size.y);
    let (x0, y0, x1, y1) = (r[4], r[5], r[4] + r[0] * w, r[5] + r[3] * h);
    let (cx0, cy0, cx1, cy1) = (x0.max(mask.rect[0]), y0.max(mask.rect[1]), x1.min(mask.rect[2]), y1.min(mask.rect[3]));
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    let (fx0, fy0, fx1, fy1) = ((cx0 - x0) / (x1 - x0), (cy0 - y0) / (y1 - y0), (cx1 - x0) / (x1 - x0), (cy1 - y0) / (y1 - y0));
    let uv = l.uv;
    l.uv = [uv[0] + (uv[2] - uv[0]) * fx0, uv[1] + (uv[3] - uv[1]) * fy0, uv[0] + (uv[2] - uv[0]) * fx1, uv[1] + (uv[3] - uv[1]) * fy1];
    l.m = concat(&l.m, &[1.0, 0.0, 0.0, 1.0, fx0 * w, fy0 * h]);
    l.size = Vec2::new(w * (fx1 - fx0), h * (fy1 - fy0));
    Some(l)
}

/// A clip instance: which frame of its sprite it shows, whether it plays, what stands at each
/// depth.
#[derive(Clone, Debug)]
pub struct Clip {
    pub sprite: u16,
    /// 0-based
    pub frame: usize,
    pub playing: bool,
    pub visible: bool,
    list: BTreeMap<u16, Placed>,
}

/// One bitmap to draw: the image, the rectangle drawn (its size; `m` places its corner, stage
/// units from the clip's origin), the part of the bitmap in it (tiling for a repeating fill)
/// and its colour.
#[derive(Clone, Debug)]
pub struct Leaf {
    pub bitmap: u16,
    pub size: Vec2,
    pub m: Mat,
    pub cx: Cx,
    pub uv: [f32; 4],
    pub repeat: bool,
}

impl Clip {
    /// A new instance of a sprite on its first frame (its script run).
    pub fn new(tl: &Timelines, sprite: u16) -> Clip {
        let mut c = Clip { sprite, frame: 0, playing: true, visible: true, list: BTreeMap::new() };
        c.apply_ops(tl, 0);
        c.run_actions(tl, 0);
        c
    }

    /// An exported symbol's clip.
    pub fn export(tl: &Timelines, name: &str) -> Option<Clip> {
        tl.exports.get(name).filter(|id| tl.sprites.contains_key(id)).map(|&id| Clip::new(tl, id))
    }

    fn frames(tl: &Timelines, sprite: u16) -> usize {
        tl.sprites.get(&sprite).map(|s| s.frames.len()).unwrap_or(0)
    }

    fn apply_ops(&mut self, tl: &Timelines, f: usize) {
        let Some(frame) = tl.sprites.get(&self.sprite).and_then(|s| s.frames.get(f)) else { return };
        for op in &frame.ops {
            match op {
                TlOp::Remove(d) => {
                    self.list.remove(d);
                }
                TlOp::Place { depth, moved, id, name, m, cx, clip } => {
                    let existing = if *moved { self.list.remove(depth) } else { None };
                    let mut p = match (existing, id) {
                        // the same character moved (or a new one put in its place, keeping
                        // its transforms)
                        (Some(mut e), Some(id)) if e.id != *id => {
                            e.id = *id;
                            e.child = tl.sprites.contains_key(id).then(|| Box::new(Clip::new(tl, *id)));
                            e
                        }
                        (Some(e), _) => e,
                        (None, Some(id)) => Placed {
                            id: *id,
                            name: None,
                            m: IDENTITY,
                            cx: NO_CX,
                            mask: None,
                            child: tl.sprites.contains_key(id).then(|| Box::new(Clip::new(tl, *id))),
                        },
                        (None, None) => continue,
                    };
                    if let Some(m) = m {
                        p.m = *m;
                    }
                    if let Some(c) = cx {
                        p.cx = *c;
                    }
                    if name.is_some() {
                        p.name = name.clone();
                    }
                    if clip.is_some() {
                        p.mask = *clip;
                    }
                    self.list.insert(*depth, p);
                }
            }
        }
    }

    /// Show a frame: the display list as the timeline has it there (played forward to it, or
    /// rebuilt from the start, keeping the nested clips that stay), its script queued.
    fn seek(&mut self, tl: &Timelines, target: usize, queue: &mut Vec<usize>) {
        let n = Self::frames(tl, self.sprite);
        if n == 0 {
            return;
        }
        let target = target.min(n - 1);
        if target == self.frame {
            return;
        }
        if target > self.frame {
            for f in self.frame + 1..=target {
                self.apply_ops(tl, f);
            }
        } else {
            let mut old = std::mem::take(&mut self.list);
            for f in 0..=target {
                self.apply_ops(tl, f);
            }
            for (d, p) in self.list.iter_mut() {
                if let Some(o) = old.remove(d) {
                    if o.id == p.id && o.child.is_some() {
                        p.child = o.child;
                    }
                }
            }
        }
        self.frame = target;
        queue.push(target);
    }

    /// Run a frame's script (and those of the frames it sends the clip to).
    fn run_actions(&mut self, tl: &Timelines, first: usize) {
        let mut queue = vec![first];
        let mut runs = 0;
        while let Some(f) = queue.pop() {
            runs += 1;
            if runs > 16 {
                break;
            }
            let Some(frame) = tl.sprites.get(&self.sprite).and_then(|s| s.frames.get(f)) else { continue };
            let mut next = Vec::new();
            for a in &frame.actions {
                match a {
                    TlAction::Stop => self.playing = false,
                    TlAction::Play => self.playing = true,
                    TlAction::Visible(v) => self.visible = *v,
                    TlAction::GotoFrame(n) => self.seek(tl, *n as usize, &mut next),
                    TlAction::GotoLabel(l) => {
                        if let Some(n) = Self::label(tl, self.sprite, l) {
                            self.seek(tl, n, &mut next);
                        }
                    }
                }
            }
            queue.extend(next);
        }
    }

    fn label(tl: &Timelines, sprite: u16, label: &str) -> Option<usize> {
        tl.sprites.get(&sprite)?.frames.iter().position(|f| f.labels.iter().any(|l| l == label))
    }

    /// `gotoAndPlay` / `gotoAndStop` a label (false if the clip has none such).
    pub fn goto_label(&mut self, tl: &Timelines, label: &str, play: bool) -> bool {
        let Some(n) = Self::label(tl, self.sprite, label) else { return false };
        self.goto(tl, n, play);
        true
    }

    /// `gotoAndPlay` / `gotoAndStop` a frame (0-based). As in Flash, the frame's own script
    /// has the last word (a state's `stop()`).
    pub fn goto(&mut self, tl: &Timelines, frame: usize, play: bool) {
        self.playing = play;
        let mut q = Vec::new();
        self.seek(tl, frame, &mut q);
        for f in q {
            self.run_actions(tl, f);
        }
    }

    /// The label of the frame shown, or of the last labelled frame before it.
    pub fn current_label<'a>(&self, tl: &'a Timelines) -> Option<&'a str> {
        let frames = &tl.sprites.get(&self.sprite)?.frames;
        frames[..=self.frame.min(frames.len().saturating_sub(1))].iter().rev().find_map(|f| f.labels.first()).map(|s| s.as_str())
    }

    /// A clip within by its instance names (`right_mc.mc1`).
    pub fn child_mut(&mut self, path: &str) -> Option<&mut Clip> {
        let mut c = self;
        for seg in path.split('.') {
            c = c.list.values_mut().find(|p| p.name.as_deref() == Some(seg))?.child.as_deref_mut()?;
        }
        Some(c)
    }

    pub fn child(&self, path: &str) -> Option<&Clip> {
        let mut c = self;
        for seg in path.split('.') {
            c = c.list.values().find(|p| p.name.as_deref() == Some(seg))?.child.as_deref()?;
        }
        Some(c)
    }

    /// A placed instance's matrix and colour (to move or tint it from code, as the original's
    /// scripts set `_x`, `_alpha`...).
    pub fn placed_mut(&mut self, name: &str) -> Option<(&mut Mat, &mut Cx)> {
        self.list.values_mut().find(|p| p.name.as_deref() == Some(name)).map(|p| (&mut p.m, &mut p.cx))
    }

    /// What stands at each depth: its depth, matrix and colour.
    pub fn placements(&self) -> Vec<(u16, Mat, Cx)> {
        self.list.iter().map(|(d, p)| (*d, p.m, p.cx)).collect()
    }

    /// The instance at a depth (names repeat: `edge11` twice in the menus' backdrops).
    pub fn placed_at_mut(&mut self, depth: u16) -> Option<(&mut Mat, &mut Cx)> {
        self.list.get_mut(&depth).map(|p| (&mut p.m, &mut p.cx))
    }

    pub fn placed(&self, name: &str) -> Option<(Mat, Cx)> {
        self.list.values().find(|p| p.name.as_deref() == Some(name)).map(|p| (p.m, p.cx))
    }

    /// One frame of the movie: the clips within go on, then this one.
    pub fn tick(&mut self, tl: &Timelines) {
        for p in self.list.values_mut() {
            if let Some(c) = p.child.as_deref_mut() {
                c.tick(tl);
            }
        }
        if self.playing {
            let n = Self::frames(tl, self.sprite);
            if n > 1 {
                let next = (self.frame + 1) % n;
                let mut q = Vec::new();
                self.seek(tl, next, &mut q);
                for f in q {
                    self.run_actions(tl, f);
                }
            }
        }
    }

    /// Where its bitmaps (and plain shapes) reach (its own space): the corners of what they cover.
    pub fn bounds(&self, tl: &Timelines) -> Option<(Vec2, Vec2)> {
        let mut out = Vec::new();
        self.leaves(tl, &IDENTITY, &NO_CX, &mut out);
        let mut shapes = Vec::new();
        self.shape_rects(tl, &IDENTITY, &mut shapes);
        let pts = out.iter().flat_map(|l| {
            let m = l.m;
            [(0.0, 0.0), (l.size.x, 0.0), (0.0, l.size.y), (l.size.x, l.size.y)].map(|(x, y)| Vec2::new(m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]))
        });
        pts.chain(shapes).fold(None, |acc: Option<(Vec2, Vec2)>, p| Some(acc.map_or((p, p), |(a, b)| (a.min(p), b.max(p)))))
    }

    /// The corners of the shapes without bitmaps (a mask's own rectangle), in `m`'s space.
    fn shape_rects(&self, tl: &Timelines, m: &Mat, out: &mut Vec<Vec2>) {
        for p in self.list.values() {
            let pm = concat(m, &p.m);
            if let Some(c) = p.child.as_deref() {
                c.shape_rects(tl, &pm, out);
            } else if let (None, Some(b)) = (tl.shapes.get(&p.id), tl.bounds.get(&p.id)) {
                for (x, y) in [(b[0], b[1]), (b[2], b[1]), (b[0], b[3]), (b[2], b[3])] {
                    out.push(Vec2::new(pm[0] * x + pm[2] * y + pm[4], pm[1] * x + pm[3] * y + pm[5]));
                }
            }
        }
    }

    /// The bitmaps to draw, back to front.
    pub fn leaves(&self, tl: &Timelines, m: &Mat, cx: &Cx, out: &mut Vec<Leaf>) {
        self.masked_leaves(tl, m, cx, None, out);
    }

    /// The bitmaps under a mask, cut to its rectangle: a mask placed here covers the depths up
    /// to the one it names.
    fn masked_leaves(&self, tl: &Timelines, m: &Mat, cx: &Cx, outer: Option<Mask>, out: &mut Vec<Leaf>) {
        if !self.visible {
            return;
        }
        let mut masks: Vec<(u16, Mask)> = Vec::new();
        for (&depth, p) in self.list.iter() {
            masks.retain(|(to, _)| depth <= *to);
            // (the buttons' hit areas, `btn`, are faintly tinted in the movies to be seen while
            // authoring them; never drawn)
            if p.name.as_deref() == Some("btn") {
                continue;
            }
            let pm = concat(m, &p.m);
            if let Some(to) = p.mask {
                // its rectangle in its own frame
                let rect = match p.child.as_deref() {
                    Some(c) => c.bounds(tl).map(|(a, b)| [a.x, a.y, b.x, b.y]),
                    None => tl.bounds.get(&p.id).copied(),
                };
                if let Some(rect) = rect {
                    masks.push((to, Mask { inv: invert(&pm), rect }));
                }
                continue;
            }
            let mask = masks.last().map(|m| m.1).or(outer);
            let pc = concat_cx(cx, &p.cx);
            if let Some(c) = p.child.as_deref() {
                c.masked_leaves(tl, &pm, &pc, mask, out);
                continue;
            }
            let start = out.len();
            Self::shape_leaves(tl, p.id, &pm, &pc, out);
            if let Some(mk) = mask {
                let cut: Vec<Leaf> = out.drain(start..).filter_map(|l| clip_leaf(l, &mk)).collect();
                out.extend(cut);
            }
        }
    }

    /// A shape's bitmaps.
    fn shape_leaves(tl: &Timelines, id: u16, pm: &Mat, pc: &Cx, out: &mut Vec<Leaf>) {
        let (pm, pc) = (*pm, *pc);
        {
            if let Some(shape) = tl.shapes.get(&id) {
                for b in shape {
                    let f = b.m;
                    match b.bounds.filter(|_| f[1].abs() < 1e-4 && f[2].abs() < 1e-4 && f[0] > 0.0 && f[3] > 0.0) {
                        // the bitmap within the shape's bounds (tiled across them if it repeats)
                        Some(bd) => {
                            let (bx, by, bw, bh) = (f[4], f[5], f[0] * b.size[0], f[3] * b.size[1]);
                            let (x0, y0, x1, y1) = if b.repeat { (bd[0], bd[1], bd[2], bd[3]) } else { (bd[0].max(bx), bd[1].max(by), bd[2].min(bx + bw), bd[3].min(by + bh)) };
                            if x1 <= x0 || y1 <= y0 || bw <= 0.0 || bh <= 0.0 {
                                continue;
                            }
                            let uv = [(x0 - bx) / bw, (y0 - by) / bh, (x1 - bx) / bw, (y1 - by) / bh];
                            out.push(Leaf { bitmap: b.bitmap, size: Vec2::new(x1 - x0, y1 - y0), m: concat(&pm, &[1.0, 0.0, 0.0, 1.0, x0, y0]), cx: pc, uv, repeat: b.repeat });
                        }
                        None => out.push(Leaf { bitmap: b.bitmap, size: Vec2::new(b.size[0], b.size[1]), m: concat(&pm, &b.m), cx: pc, uv: [0.0, 0.0, 1.0, 1.0], repeat: false }),
                    }
                }
            }
        }
    }
}

/// A playing clip of an interface movie, drawn where its node is put (its origin at the node's
/// top-left; the node is best left without size).
#[derive(Component)]
pub struct FlashClip {
    pub movie: String,
    pub tl: Arc<Timelines>,
    pub clip: Clip,
    /// stage units to the screen's pixels
    pub scale: f32,
    /// over all its bitmaps
    pub alpha: f32,
    /// its own transform on the stage (turned, scaled; before `scale`)
    pub m: Mat,
    /// paused (Flash keeps time; a paused game's menus may not)
    pub frozen: bool,
    /// played in real time (a menu over the paused game), not the game's
    pub real_time: bool,
    acc: f32,
}

impl FlashClip {
    pub fn new(movie: &str, tl: Arc<Timelines>, clip: Clip) -> FlashClip {
        FlashClip { movie: movie.to_string(), tl, clip, scale: 1.0, alpha: 1.0, m: IDENTITY, frozen: false, real_time: false, acc: 0.0 }
    }

    /// Played in real time (a menu's, over a paused game).
    pub fn real(mut self) -> Self {
        self.real_time = true;
        self
    }

    /// `gotoAndPlay` / `gotoAndStop` a label of a clip within (`""`: the clip itself).
    pub fn goto(&mut self, path: &str, label: &str, play: bool) -> bool {
        let tl = self.tl.clone();
        let c = if path.is_empty() { Some(&mut self.clip) } else { self.clip.child_mut(path) };
        c.map(|c| c.goto_label(&tl, label, play)).unwrap_or(false)
    }

    /// `_visible` of a clip within.
    pub fn set_visible(&mut self, path: &str, v: bool) {
        if let Some(c) = self.clip.child_mut(path) {
            c.visible = v;
        }
    }

    /// The label a clip within is at.
    pub fn label(&self, path: &str) -> Option<&str> {
        let c = if path.is_empty() { Some(&self.clip) } else { self.clip.child(path) };
        c.and_then(|c| c.current_label(&self.tl))
    }
}

/// A drawn bitmap of a clip (its children, reused frame to frame).
#[derive(Component)]
struct LeafNode;

#[allow(clippy::type_complexity)]
fn play_clips(
    (time, real): (Res<Time>, Res<Time<Real>>),
    mut commands: Commands,
    mut clips: Query<(Entity, &mut FlashClip, Option<&Children>, &InheritedVisibility)>,
    mut leaves: Query<(&mut Node, &mut UiTransform, &MaterialNode<FlashFill>, &mut Visibility), With<LeafNode>>,
    mut mats: ResMut<Assets<FlashFill>>,
    mut ui: ResMut<crate::ui_images::UiImages>,
    mut images: ResMut<Assets<Image>>,
) {
    let mut out = Vec::new();
    for (e, mut fc, children, shown) in &mut clips {
        let fc = &mut *fc;
        let dt = if fc.real_time { real.delta_secs() } else { time.delta_secs() };
        let step = 1.0 / fc.tl.rate.max(1.0);
        if !fc.frozen {
            fc.acc = (fc.acc + dt).min(step * 4.0);
            while fc.acc >= step {
                fc.acc -= step;
                fc.clip.tick(&fc.tl);
            }
        }
        if !shown.get() {
            continue;
        }
        out.clear();
        fc.clip.leaves(&fc.tl, &concat(&[fc.scale, 0.0, 0.0, fc.scale, 0.0, 0.0], &fc.m), &[1.0, 1.0, 1.0, fc.alpha, 0.0, 0.0, 0.0, 0.0], &mut out);
        let kids: Vec<Entity> = children.map(|c| c.iter().filter(|k| leaves.contains(*k)).collect()).unwrap_or_default();
        for (i, l) in out.iter().enumerate() {
            let Some((tex, _)) = ui.get(&mut images, &fc.movie, l.bitmap as u32) else { continue };
            let mult = Vec4::new(l.cx[0], l.cx[1], l.cx[2], l.cx[3]);
            let add = Vec4::new(l.cx[4], l.cx[5], l.cx[6], l.cx[7]) / 255.0;
            let mut uv = Vec4::from_array(l.uv);
            let opts = Vec4::new(if l.repeat { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0);
            // the bitmap's rectangle turned and scaled about its middle
            let [a, b, c, d, tx, ty] = l.m;
            let (w, h) = (l.size.x, l.size.y);
            let mid = Vec2::new(a * w * 0.5 + c * h * 0.5 + tx, b * w * 0.5 + d * h * 0.5 + ty);
            let (sx, sy, turn) = if b.abs() < 1e-5 && c.abs() < 1e-5 {
                // (mirrored or turned half round, but square to the screen: drawn unturned with
                // its picture mirrored, so a clipping parent still cuts it)
                if a < 0.0 {
                    uv = Vec4::new(uv.z, uv.y, uv.x, uv.w);
                }
                if d < 0.0 {
                    uv = Vec4::new(uv.x, uv.w, uv.z, uv.y);
                }
                (a.abs(), d.abs(), 0.0)
            } else {
                let sx = (a * a + b * b).sqrt();
                (sx, if sx > 1e-6 { (a * d - b * c) / sx } else { 0.0 }, b.atan2(a))
            };
            let place = |n: &mut Node, t: &mut UiTransform| {
                n.left = Val::Px(mid.x - w * 0.5);
                n.top = Val::Px(mid.y - h * 0.5);
                n.width = Val::Px(w);
                n.height = Val::Px(h);
                t.scale = Vec2::new(sx, sy);
                t.rotation = Rot2::radians(turn);
            };
            if let Some(&k) = kids.get(i) {
                let Ok((mut n, mut t, mn, mut v)) = leaves.get_mut(k) else { continue };
                place(&mut n, &mut t);
                if *v != Visibility::Inherited {
                    *v = Visibility::Inherited;
                }
                let stale = mats.get(&mn.0).map(|f| f.mult != mult || f.add != add || f.uv != uv || f.opts != opts || f.texture != tex).unwrap_or(false);
                if stale {
                    if let Some(mut f) = mats.get_mut(&mn.0) {
                        f.mult = mult;
                        f.add = add;
                        f.uv = uv;
                        f.opts = opts;
                        f.texture = tex;
                    }
                }
            } else {
                let mut n = Node { position_type: PositionType::Absolute, ..default() };
                let mut t = UiTransform::default();
                place(&mut n, &mut t);
                commands.spawn((LeafNode, n, t, MaterialNode(mats.add(FlashFill { mult, add, uv, opts, texture: tex })), Pickable::IGNORE, ChildOf(e)));
            }
        }
        for &k in kids.iter().skip(out.len()) {
            if let Ok((_, _, _, mut v)) = leaves.get_mut(k) {
                if *v != Visibility::Hidden {
                    *v = Visibility::Hidden;
                }
            }
        }
    }
}
