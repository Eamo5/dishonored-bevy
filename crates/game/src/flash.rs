//! A player for the interface movies' animated clips as their Flash timelines have them
//! (`ui/<movie>/timeline.json`, cooked by `dhtool cook-ui`): each frame's display list
//! changes (bitmaps and nested clips placed, moved, recoloured and removed), its labels and
//! its scripts' `stop()`, `play()`, `gotoAndPlay/Stop` and `this._visible`. The game drives
//! clips the way the original's code does (`gotoAndPlay("fillIn")` on a named clip within)
//! and they play at the movie's frame rate, nested clips on their own.
//!
//! A clip is a UI node (placed by its owner) whose bitmaps are drawn as its children with
//! their colour transforms; a mask cuts the bitmaps under it to its rectangle (where they are
//! square to it, as interface bars and reveals are). Its text fields (`DefineEditText`) are
//! drawn as text in their fonts, sizes, colours and alignments, showing what the game set
//! (by instance path: `_score_mc.txt_mc.txt`, plain or with `<font color>` HTML) or their
//! own text. Code moves, scales, turns and fades instances as the movies' classes do
//! (`_x`... set or tweened, `tweenTo`), and attaches symbols (`attachMovie`).

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
            .add_systems(PostUpdate, play_clips.in_set(PlayClips).before(bevy::ui::UiSystems::Prepare))
            .add_systems(Update, drop_turned_texts);
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
    /// a mask it is cut to as drawn: its quad's place (0..1) into the mask's frame (the
    /// columns, the offset and whether there is one) and the mask's rectangle there
    #[uniform(6)]
    clip_a: Vec4,
    #[uniform(7)]
    clip_b: Vec4,
    #[uniform(8)]
    clip_r: Vec4,
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
    /// attached from code (`attachMovie`): kept through the timeline's changes
    attached: bool,
}

/// A placed instance's Flash properties: `_x`, `_y`, `_xscale` / `_yscale` (1 for 100),
/// `_rotation` (degrees) and `_alpha` (0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Props {
    pub x: f32,
    pub y: f32,
    pub xscale: f32,
    pub yscale: f32,
    pub rotation: f32,
    pub alpha: f32,
}

impl Props {
    pub fn of(m: &Mat, cx: &Cx) -> Props {
        let det = m[0] * m[3] - m[1] * m[2];
        let ys = (m[2] * m[2] + m[3] * m[3]).sqrt();
        Props { x: m[4], y: m[5], xscale: (m[0] * m[0] + m[1] * m[1]).sqrt(), yscale: if det < 0.0 { -ys } else { ys }, rotation: m[1].atan2(m[0]).to_degrees(), alpha: cx[3] }
    }

    fn write(&self, m: &mut Mat, cx: &mut Cx) {
        let t = turn_scale(self.rotation, self.xscale, self.yscale);
        *m = [t[0], t[1], t[2], t[3], self.x, self.y];
        cx[3] = self.alpha;
    }

    fn toward(&self, to: &PropsTo, k: f32) -> Props {
        let l = |a: f32, b: Option<f32>| b.map_or(a, |b| a + (b - a) * k);
        Props { x: l(self.x, to.x), y: l(self.y, to.y), xscale: l(self.xscale, to.xscale), yscale: l(self.yscale, to.yscale), rotation: l(self.rotation, to.rotation), alpha: l(self.alpha, to.alpha) }
    }
}

/// The properties a tween goes to (those it leaves alone: `None`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PropsTo {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub xscale: Option<f32>,
    pub yscale: Option<f32>,
    pub rotation: Option<f32>,
    pub alpha: Option<f32>,
}

impl PropsTo {
    pub fn x(mut self, v: f32) -> Self {
        self.x = Some(v);
        self
    }
    pub fn y(mut self, v: f32) -> Self {
        self.y = Some(v);
        self
    }
    pub fn xscale(mut self, v: f32) -> Self {
        self.xscale = Some(v);
        self
    }
    pub fn yscale(mut self, v: f32) -> Self {
        self.yscale = Some(v);
        self
    }
    /// both scales
    pub fn scale(self, v: f32) -> Self {
        self.xscale(v).yscale(v)
    }
    pub fn rotation(mut self, v: f32) -> Self {
        self.rotation = Some(v);
        self
    }
    pub fn alpha(mut self, v: f32) -> Self {
        self.alpha = Some(v);
        self
    }
}

impl From<Props> for PropsTo {
    fn from(p: Props) -> PropsTo {
        PropsTo { x: Some(p.x), y: Some(p.y), xscale: Some(p.xscale), yscale: Some(p.yscale), rotation: Some(p.rotation), alpha: Some(p.alpha) }
    }
}

/// The movies' easings (`mx.transitions.easing`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ease {
    None,
    /// `Strong.easeOut` (quintic)
    StrongOut,
    StrongIn,
    /// `Strong.easeInOut`
    StrongInOut,
    /// `Regular.easeOut` (quadratic)
    RegularOut,
    BackOut,
    BackInOut,
}

impl Ease {
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        const S: f32 = 1.70158;
        match self {
            Ease::None => t,
            Ease::StrongOut => 1.0 - (1.0 - t).powi(5),
            Ease::StrongIn => t.powi(5),
            Ease::StrongInOut => {
                if t < 0.5 {
                    16.0 * t.powi(5)
                } else {
                    1.0 - 16.0 * (1.0 - t).powi(5)
                }
            }
            Ease::RegularOut => 1.0 - (1.0 - t).powi(2),
            Ease::BackOut => {
                let u = t - 1.0;
                u * u * ((S + 1.0) * u + S) + 1.0
            }
            Ease::BackInOut => {
                let s = S * 1.525;
                let u = t * 2.0;
                if u < 1.0 {
                    0.5 * (u * u * ((s + 1.0) * u - s))
                } else {
                    let u = u - 2.0;
                    0.5 * (u * u * ((s + 1.0) * u + s) + 2.0)
                }
            }
        }
    }
}

/// A property tween of an instance within (`tweenTo`).
#[derive(Clone, Debug)]
struct Tween {
    path: String,
    from: Props,
    to: PropsTo,
    t: f32,
    dur: f32,
    ease: Ease,
}

/// A text field to draw: its field (character id), its instance path within the clip, its
/// placement (stage units, from the clip's origin) and colour transform, and how many
/// bitmaps were drawn before it.
#[derive(Clone, Debug)]
pub struct TextLeaf {
    pub id: u16,
    pub path: String,
    pub m: Mat,
    pub cx: Cx,
    /// the rectangle it is cut to (a clip's mask set from code, `setMask`), on the stage
    pub clip: Option<[f32; 4]>,
    /// that mask itself (its frame: turned to the field, it is cut as drawn)
    pub(crate) mask: Option<Mask>,
}

/// The text fields met walking a clip, and the path walked to; the pictures loaded into its
/// clips, by path.
struct TextWalk<'a> {
    path: String,
    out: &'a mut Vec<TextLeaf>,
    images: &'a HashMap<String, LoadedImage>,
    /// the masks set from code (`setMask`): the clip masked, its mask's path, the mask, its
    /// rectangle on the stage (square to it); the text fields' cut now
    masks: Vec<(String, String, Mask, Option<[f32; 4]>)>,
    clip: Option<[f32; 4]>,
    mask: Option<Mask>,
}

/// A mask: the inverse of its frame (from the root's space) and its rectangle in that frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mask {
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
        // (turned to it or mirrored: cut where it is drawn)
        l.mask = Some(*mask);
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
    /// a picture loaded into a clip (`ImgLoader`) rather than one of the movie's bitmaps
    pub image: Option<Handle<Image>>,
    /// a mask it is turned to (or mirrored in): cut as it is drawn
    pub(crate) mask: Option<Mask>,
}

/// A picture loaded into a clip (`_common.ImgLoader`): drawn this big, from here (its clip's
/// units: fitted into the clip as it was and centred on it).
#[derive(Clone, Debug)]
pub struct LoadedImage {
    pub image: Handle<Image>,
    pub size: Vec2,
    pub at: Vec2,
}

impl Clip {
    /// A new instance of a sprite on its first frame (its script run).
    pub fn new(tl: &Timelines, sprite: u16) -> Clip {
        let mut c = Clip { sprite, frame: 0, playing: true, visible: true, list: BTreeMap::new() };
        c.apply_ops(tl, 0);
        c.run_actions(tl, 0);
        c
    }

    /// A clip with nothing in it (`createEmptyMovieClip`; a symbol with no timeline).
    pub fn empty() -> Clip {
        Clip { sprite: u16::MAX, frame: 0, playing: false, visible: true, list: BTreeMap::new() }
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
                            attached: false,
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
            // (what code attached stays)
            self.list.extend(old.into_iter().filter(|(_, p)| p.attached));
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

    /// `attachMovie`: a new instance of a sprite, named, above everything here
    /// (`getNextHighestDepth`), kept through the timeline's changes.
    pub fn attach(&mut self, tl: &Timelines, sprite: u16, name: &str, m: Mat) {
        let depth = self.list.keys().next_back().map_or(0, |d| d + 1).max(16384);
        let child = tl.sprites.contains_key(&sprite).then(|| Box::new(Clip::new(tl, sprite)));
        self.list.insert(depth, Placed { id: sprite, name: Some(name.to_string()), m, cx: NO_CX, mask: None, child, attached: true });
    }

    /// `createEmptyMovieClip`: a new clip with nothing in it, to attach to.
    pub fn attach_empty(&mut self, name: &str, m: Mat) {
        let depth = self.list.keys().next_back().map_or(0, |d| d + 1).max(16384);
        let child = Clip { sprite: u16::MAX, frame: 0, playing: false, visible: true, list: BTreeMap::new() };
        self.list.insert(depth, Placed { id: u16::MAX, name: Some(name.to_string()), m, cx: NO_CX, mask: None, child: Some(Box::new(child)), attached: true });
    }

    /// The character an instance here shows.
    pub fn placed_id(&self, name: &str) -> Option<u16> {
        self.list.values().find(|p| p.name.as_deref() == Some(name)).map(|p| p.id)
    }

    /// `removeMovieClip` of an instance here.
    pub fn remove(&mut self, name: &str) {
        self.list.retain(|_, p| p.name.as_deref() != Some(name));
    }

    /// The names of what stands here (in depth order).
    pub fn names(&self) -> Vec<String> {
        self.list.values().filter_map(|p| p.name.clone()).collect()
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
        self.masked_leaves(tl, m, cx, None, out, None);
    }

    /// The bitmaps to draw and the text fields.
    pub fn leaves_texts(&self, tl: &Timelines, m: &Mat, cx: &Cx, out: &mut Vec<Leaf>, texts: &mut Vec<TextLeaf>, images: &HashMap<String, LoadedImage>) {
        self.leaves_masked(tl, m, cx, out, texts, images, &[]);
    }

    /// The bitmaps to draw and the text fields, with masks set from code (`setMask`: the clip
    /// masked, the mask's path; the mask itself not drawn).
    #[allow(clippy::too_many_arguments)]
    pub fn leaves_masked(&self, tl: &Timelines, m: &Mat, cx: &Cx, out: &mut Vec<Leaf>, texts: &mut Vec<TextLeaf>, images: &HashMap<String, LoadedImage>, masks: &[(String, String)]) {
        let masks: Vec<(String, String, Mask, Option<[f32; 4]>)> = masks
            .iter()
            .filter_map(|(clip, mask)| {
                let mm = concat(m, &self.path_matrix(mask)?);
                let (a, b) = self.child(mask)?.bounds(tl)?;
                let rect = [a.x, a.y, b.x, b.y];
                // (on the stage: its own rectangle about its middle, square to the screen, the
                // text fields cut so; little turned, else not at all)
                let (kx, ky) = ((mm[0] * mm[0] + mm[1] * mm[1]).sqrt(), (mm[2] * mm[2] + mm[3] * mm[3]).sqrt());
                let on = (mm[1].abs() < 0.17 * kx && mm[2].abs() < 0.17 * ky).then(|| {
                    let mid = (a + b) * 0.5;
                    let (cx, cy) = (mm[0] * mid.x + mm[2] * mid.y + mm[4], mm[1] * mid.x + mm[3] * mid.y + mm[5]);
                    let (hw, hh) = (kx * (b.x - a.x).abs() * 0.5, ky * (b.y - a.y).abs() * 0.5);
                    [cx - hw, cy - hh, cx + hw, cy + hh]
                });
                Some((clip.clone(), mask.clone(), Mask { inv: invert(&mm), rect }, on))
            })
            .collect();
        let mut w = TextWalk { path: String::new(), out: texts, images, masks, clip: None, mask: None };
        self.masked_leaves(tl, m, cx, None, out, Some(&mut w));
    }

    /// An instance's matrix within this clip (through its parents).
    pub fn path_matrix(&self, path: &str) -> Option<Mat> {
        let (mut c, mut m) = (self, IDENTITY);
        for seg in path.split('.') {
            let p = c.list.values().find(|p| p.name.as_deref() == Some(seg))?;
            m = concat(&m, &p.m);
            match p.child.as_deref() {
                Some(k) => c = k,
                None => return Some(m),
            }
        }
        Some(m)
    }

    /// The bitmaps under a mask, cut to its rectangle: a mask placed here covers the depths up
    /// to the one it names.
    fn masked_leaves(&self, tl: &Timelines, m: &Mat, cx: &Cx, outer: Option<Mask>, out: &mut Vec<Leaf>, mut txt: Option<&mut TextWalk>) {
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
            let mut mask = masks.last().map(|m| m.1).or(outer);
            let pc = concat_cx(cx, &p.cx);
            if let Some(c) = p.child.as_deref() {
                match txt.as_deref_mut() {
                    Some(w) => {
                        let len = w.path.len();
                        if len > 0 {
                            w.path.push('.');
                        }
                        w.path.push_str(p.name.as_deref().unwrap_or(""));
                        // (a mask set from code: not drawn; what it masks cut to it)
                        if w.masks.iter().any(|k| k.1 == w.path) {
                            w.path.truncate(len);
                            continue;
                        }
                        let was = (w.clip, w.mask);
                        if let Some(k) = w.masks.iter().find(|k| k.0 == w.path) {
                            mask = Some(k.2);
                            w.clip = k.3;
                            w.mask = Some(k.2);
                        }
                        c.masked_leaves(tl, &pm, &pc, mask, out, Some(&mut *w));
                        (w.clip, w.mask) = was;
                        // (a picture loaded into it, over what it has)
                        if let Some(img) = w.images.get(&w.path).filter(|_| c.visible) {
                            let l = Leaf { bitmap: 0, size: img.size, m: concat(&pm, &[1.0, 0.0, 0.0, 1.0, img.at.x, img.at.y]), cx: pc, uv: [0.0, 0.0, 1.0, 1.0], repeat: false, image: Some(img.image.clone()), mask: None };
                            if let Some(l) = match mask {
                                Some(mk) => clip_leaf(l, &mk),
                                None => Some(l),
                            } {
                                out.push(l);
                            }
                        }
                        w.path.truncate(len);
                    }
                    None => c.masked_leaves(tl, &pm, &pc, mask, out, None),
                }
                continue;
            }
            if let Some(t) = tl.texts.get(&p.id) {
                if let Some(w) = txt.as_deref_mut() {
                    let name = p.name.as_deref().unwrap_or(t.var.as_str());
                    let path = if w.path.is_empty() { name.to_string() } else { format!("{}.{name}", w.path) };
                    w.out.push(TextLeaf { id: p.id, path, m: pm, cx: pc, clip: w.clip, mask: w.mask });
                }
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
                            out.push(Leaf { bitmap: b.bitmap, size: Vec2::new(x1 - x0, y1 - y0), m: concat(&pm, &[1.0, 0.0, 0.0, 1.0, x0, y0]), cx: pc, uv, repeat: b.repeat, image: None, mask: None });
                        }
                        None => out.push(Leaf { bitmap: b.bitmap, size: Vec2::new(b.size[0], b.size[1]), m: concat(&pm, &b.m), cx: pc, uv: [0.0, 0.0, 1.0, 1.0], repeat: false, image: None, mask: None }),
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
    /// its text fields drawn (those the game draws itself are not: the menus' labels...)
    pub show_texts: bool,
    /// what its text fields show, by path (else their own text)
    pub texts: HashMap<String, String>,
    /// the pictures loaded into its clips, by path (`ImgLoader`)
    pub images: HashMap<String, LoadedImage>,
    /// text fields sized to their text (`autoSize`): kept at their left (0), right (1) or
    /// middle (2), by path
    pub autosize: HashMap<String, u8>,
    /// text fields wrapping or not (`wordWrap`) other than as made, by path
    pub wrap: HashMap<String, bool>,
    tweens: Vec<Tween>,
    /// instances turning round (`_utils.RotationAnimation`): path, seconds a turn, clockwise
    spins: Vec<(String, f32, bool)>,
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
    /// the masks set on its clips (`setMask`): the clip masked, its mask's path
    pub masks: Vec<(String, String)>,
    acc: f32,
    /// its fields cut by a turned mask, laid out on pictures of their own (by path)
    turned: HashMap<String, TurnedText>,
}

/// A text field cut by a mask it is turned in (Bevy cuts turned letters wrongly): laid out on
/// a picture of its own by a camera of its own, the picture drawn as a bitmap is (turned, and
/// cut by its mask as drawn). Its picture, camera, layout root and text, the picture's size,
/// and what it was laid out with.
struct TurnedText {
    image: Handle<Image>,
    cam: Entity,
    root: Entity,
    size: UVec2,
    shown: (String, u32, bool, u8, bool),
    /// frames since it was laid out (its camera put away once it has drawn it)
    frames: u8,
}

/// A turned field's camera or layout root (its clip's).
#[derive(Component)]
struct TurnedTextOf(Entity);

/// A turned field's picture goes when its clip does.
fn drop_turned_texts(mut commands: Commands, parts: Query<(Entity, &TurnedTextOf)>, clips: Query<(), With<FlashClip>>) {
    for (e, of) in &parts {
        if !clips.contains(of.0) {
            commands.entity(e).try_despawn();
        }
    }
}

/// A field's box in its own units (its gutter in; a line not wrapped as wide as its text, held
/// at the side it is aligned to; a wrapping field as high as its text may run).
fn field_box(def: &dhcook::format::EditTextDef, content: &str, align: u8, wrap: bool) -> (f32, f32, f32, f32) {
    let (mut x0, y0, mut x1, mut y1) = (def.bounds[0] + 2.0, def.bounds[1] + 2.0, def.bounds[2] - 2.0, def.bounds[3]);
    y1 = y1.max(y0 + def.size * 1.3);
    if wrap {
        y1 = y1.max(y0 + 2000.0);
    }
    if !wrap {
        let title = def.font.contains("Title") || def.font.contains("Emerge");
        let plain: String = if def.html || content.contains('<') { html_spans(content).into_iter().map(|s| s.0).collect() } else { content.to_string() };
        let tw = crate::ui_fonts::measure(title, def.size, &plain) * 1.02 + 1.0;
        (x0, x1) = match align {
            1 => (x1 - tw, x1),
            2 => ((x0 + x1) * 0.5 - tw * 0.5, (x0 + x1) * 0.5 + tw * 0.5),
            _ => (x0, x0 + tw),
        };
    }
    (x0, y0, x1, y1)
}

impl FlashClip {
    pub fn new(movie: &str, tl: Arc<Timelines>, clip: Clip) -> FlashClip {
        FlashClip { show_texts: false, images: HashMap::new(), texts: HashMap::new(), autosize: HashMap::new(), wrap: HashMap::new(), tweens: Vec::new(), spins: Vec::new(), movie: movie.to_string(), tl, clip, scale: 1.0, alpha: 1.0, m: IDENTITY, frozen: false, real_time: false, masks: Vec::new(), acc: 0.0, turned: HashMap::new() }
    }

    /// `clip.setMask(mask)`: a clip drawn only within its mask's rectangle (bitmaps and text
    /// fields), the mask not drawn.
    pub fn set_mask(&mut self, clip: &str, mask: &str) {
        self.masks.retain(|k| k.0 != clip);
        self.masks.push((clip.to_string(), mask.to_string()));
    }

    /// A text field's text (`txt.text = ...`; `htmlText` too).
    pub fn set_text(&mut self, path: &str, text: impl Into<String>) {
        let text = text.into();
        if self.texts.get(path) != Some(&text) {
            self.texts.insert(path.to_string(), text);
        }
    }

    /// An instance's properties (`""`: the clip itself, on the stage).
    pub fn props(&self, path: &str) -> Option<Props> {
        if path.is_empty() {
            return Some(Props::of(&self.m, &[1.0, 1.0, 1.0, self.alpha, 0.0, 0.0, 0.0, 0.0]));
        }
        let (parent, last) = path.rsplit_once('.').unwrap_or(("", path));
        let c = if parent.is_empty() { Some(&self.clip) } else { self.clip.child(parent) };
        c?.placed(last).map(|(m, cx)| Props::of(&m, &cx))
    }

    pub fn set_props(&mut self, path: &str, p: Props) {
        if path.is_empty() {
            let mut cx = NO_CX;
            p.write(&mut self.m, &mut cx);
            self.alpha = cx[3];
            return;
        }
        let (parent, last) = path.rsplit_once('.').unwrap_or(("", path));
        let c = if parent.is_empty() { Some(&mut self.clip) } else { self.clip.child_mut(parent) };
        if let Some((m, cx)) = c.and_then(|c| c.placed_mut(last)) {
            p.write(m, cx);
        }
    }

    /// Some of an instance's properties set (`_x = ...`).
    pub fn set(&mut self, path: &str, to: PropsTo) {
        if let Some(p) = self.props(path) {
            self.set_props(path, p.toward(&to, 1.0));
        }
    }

    /// `tweenTo`: an instance's properties eased to these over a time (seconds), any tween
    /// of it before dropped where it was.
    pub fn tween(&mut self, path: &str, to: PropsTo, dur: f32, ease: Ease) {
        self.tweens.retain(|t| t.path != path);
        if let Some(from) = self.props(path) {
            self.tweens.push(Tween { path: path.to_string(), from, to, t: 0.0, dur, ease });
        }
    }

    /// `tweenEnd`: an instance's tween stopped, at its end (or where it was).
    pub fn tween_end(&mut self, path: &str, finish: bool) {
        let (done, keep): (Vec<Tween>, Vec<Tween>) = std::mem::take(&mut self.tweens).into_iter().partition(|t| t.path == path);
        self.tweens = keep;
        if finish {
            for t in done {
                self.set_props(&t.path, t.from.toward(&t.to, 1.0));
            }
        }
    }

    /// Whether an instance is tweening.
    pub fn tweening(&self, path: &str) -> bool {
        self.tweens.iter().any(|t| t.path == path)
    }

    /// An instance turning round on and on (`StartRotationAnimation`): a turn in so many
    /// seconds.
    pub fn spin(&mut self, path: &str, secs: f32, clockwise: bool) {
        self.spins.retain(|s| s.0 != path);
        self.spins.push((path.to_string(), secs.max(0.01), clockwise));
    }

    pub fn stop_spin(&mut self, path: &str) {
        self.spins.retain(|s| s.0 != path);
    }

    /// A clip within: shown or not, at a frame (`gotoAndStop`, 1-based as in Flash).
    pub fn goto_frame(&mut self, path: &str, frame: usize, play: bool) {
        let tl = self.tl.clone();
        let c = if path.is_empty() { Some(&mut self.clip) } else { self.clip.child_mut(path) };
        if let Some(c) = c {
            c.goto(&tl, frame.saturating_sub(1), play);
        }
    }

    /// `createEmptyMovieClip` on a clip within (`""`: the clip itself).
    pub fn create_empty(&mut self, path: &str, name: &str, m: Mat) {
        let c = if path.is_empty() { Some(&mut self.clip) } else { self.clip.child_mut(path) };
        if let Some(c) = c {
            c.attach_empty(name, m);
        }
    }

    /// `_totalframes` of a clip within (`""`: the clip itself).
    pub fn total_frames(&self, path: &str) -> usize {
        let c = if path.is_empty() { Some(&self.clip) } else { self.clip.child(path) };
        c.map_or(0, |c| Clip::frames(&self.tl, c.sprite))
    }

    /// `removeMovieClip` of an instance within.
    pub fn remove(&mut self, path: &str) {
        let (parent, last) = path.rsplit_once('.').unwrap_or(("", path));
        let c = if parent.is_empty() { Some(&mut self.clip) } else { self.clip.child_mut(parent) };
        if let Some(c) = c {
            c.remove(last);
        }
        self.tweens.retain(|t| t.path != path && !t.path.starts_with(&format!("{path}.")));
    }

    /// A text field's width as shown (its text's, in its own units with the gutter: an
    /// `autoSize`d field's `_width`).
    pub fn text_width(&self, path: &str) -> Option<f32> {
        let (parent, last) = path.rsplit_once('.').unwrap_or(("", path));
        let c = if parent.is_empty() { Some(&self.clip) } else { self.clip.child(parent) };
        let def = self.tl.texts.get(&c?.placed_id(last)?)?;
        let content = self.texts.get(path).map(String::as_str).unwrap_or(def.text.as_str());
        let plain: String = if def.html || content.contains('<') { html_spans(content).into_iter().map(|s| s.0).collect() } else { content.to_string() };
        let title = def.font.contains("Title") || def.font.contains("Emerge");
        Some(crate::ui_fonts::measure(title, def.size, &plain) + 4.0)
    }

    /// A text field's height with its text laid out across its width (`_height` of a field
    /// sized to its text, `autoSize`; wrapped at words if it wraps): its lines at the height
    /// they are drawn at, and its gutters.
    pub fn text_height(&self, path: &str) -> Option<f32> {
        let (parent, last) = path.rsplit_once('.').unwrap_or(("", path));
        let c = if parent.is_empty() { Some(&self.clip) } else { self.clip.child(parent) };
        let def = self.tl.texts.get(&c?.placed_id(last)?)?;
        let content = self.texts.get(path).map(String::as_str).unwrap_or(def.text.as_str());
        let plain: String = if def.html || content.contains('<') { html_spans(content).into_iter().map(|s| s.0).collect() } else { content.to_string() };
        let title = def.font.contains("Title") || def.font.contains("Emerge");
        let wrap = self.wrap.get(path).copied().unwrap_or(def.wrap);
        let width = (def.bounds[2] - def.bounds[0] - 4.0).max(1.0);
        let mut lines = 0;
        for para in plain.replace('\r', "\n").split('\n') {
            lines += 1;
            if !wrap {
                continue;
            }
            let mut line = String::new();
            for word in para.split(' ') {
                let longer = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if !line.is_empty() && crate::ui_fonts::measure(title, def.size, &longer) > width {
                    lines += 1;
                    line = word.to_string();
                } else {
                    line = longer;
                }
            }
        }
        Some(lines as f32 * def.size * 1.2 + 4.0)
    }

    /// The bounds of a clip within, in its parent's units (`_width`, `_height`: its own
    /// bounds through its placement).
    pub fn size(&self, path: &str) -> Option<Vec2> {
        let c = if path.is_empty() { Some(&self.clip) } else { self.clip.child(path) }?;
        let (a, b) = c.bounds(&self.tl)?;
        let p = self.props(path).unwrap_or(Props { x: 0.0, y: 0.0, xscale: 1.0, yscale: 1.0, rotation: 0.0, alpha: 1.0 });
        Some(Vec2::new((b.x - a.x) * p.xscale.abs(), (b.y - a.y) * p.yscale.abs()))
    }

    fn step_tweens(&mut self, dt: f32) {
        for (path, secs, cw) in self.spins.clone() {
            if let Some(mut p) = self.props(&path) {
                p.rotation += 360.0 * dt / secs * if cw { 1.0 } else { -1.0 };
                self.set_props(&path, p);
            }
        }
        let mut tw = std::mem::take(&mut self.tweens);
        for t in tw.iter_mut() {
            t.t += dt;
            let k = t.ease.at(if t.dur > 0.0 { t.t / t.dur } else { 1.0 });
            self.set_props(&t.path, t.from.toward(&t.to, k));
        }
        tw.retain(|t| t.t < t.dur);
        // (tweens started while these ran stay)
        tw.append(&mut self.tweens);
        self.tweens = tw;
    }

    /// `ImgLoader`: a picture (its natural size) into a clip within, fitted into the clip as
    /// it is (made smaller, never larger) and centred on its origin; `None` takes it out.
    pub fn load_image(&mut self, path: &str, image: Option<(Handle<Image>, Vec2)>) {
        let Some((image, natural)) = image else {
            self.images.remove(path);
            return;
        };
        let max = self.size(path).unwrap_or(natural);
        let p = self.props(path).unwrap_or(Props { x: 0.0, y: 0.0, xscale: 1.0, yscale: 1.0, rotation: 0.0, alpha: 1.0 });
        // (in its own units: its bounds through its scale)
        let max = Vec2::new(max.x / p.xscale.abs().max(1e-6), max.y / p.yscale.abs().max(1e-6));
        let k = if natural.x > max.x || natural.y > max.y { (max.x / natural.x).min(max.y / natural.y) } else { 1.0 };
        let size = natural * k;
        self.images.insert(path.to_string(), LoadedImage { image, size, at: -size * 0.5 });
    }

    /// `attachMovie` of an exported symbol on a clip within (`""`: the clip itself).
    pub fn attach(&mut self, path: &str, symbol: &str, name: &str, m: Mat) -> bool {
        let tl = self.tl.clone();
        // (linkage names as the player finds them: in any case)
        let Some(&sprite) = tl.exports.get(symbol).or_else(|| tl.exports.iter().find(|(k, _)| k.eq_ignore_ascii_case(symbol)).map(|(_, v)| v)) else { return false };
        let c = if path.is_empty() { Some(&mut self.clip) } else { self.clip.child_mut(path) };
        c.map(|c| c.attach(&tl, sprite, name, m)).is_some()
    }

    /// With its text fields drawn.
    pub fn with_texts(mut self) -> Self {
        self.show_texts = true;
        self
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

    /// `_visible` of a clip within (`""`: the clip itself).
    pub fn set_visible(&mut self, path: &str, v: bool) {
        let c = if path.is_empty() { Some(&mut self.clip) } else { self.clip.child_mut(path) };
        if let Some(c) = c {
            c.visible = v;
        }
    }

    pub fn visible(&self, path: &str) -> bool {
        let c = if path.is_empty() { Some(&self.clip) } else { self.clip.child(path) };
        c.is_some_and(|c| c.visible)
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

/// A drawn text field's box (a clip's child, after the bitmaps; reused frame to frame): where
/// its field is cut (a mask's rectangle), else nowhere.
#[derive(Component)]
struct TextBox;

/// A drawn text field of a clip (in its box): what it shows (the text it was given).
#[derive(Component)]
struct TextNode {
    shown: String,
    /// its spans' colours (the HTML's `<font color>`; `None`: the field's)
    colors: Vec<Option<Color>>,
}

/// The parts of a field's HTML text with their colours (`<font color="#rrggbb">`; line
/// breaks kept, other tags dropped).
pub fn html_spans(s: &str) -> Vec<(String, Option<Color>)> {
    let mut out: Vec<(String, Option<Color>)> = Vec::new();
    let mut stack: Vec<Option<Color>> = vec![None];
    let mut cur = String::new();
    let mut rest = s;
    let flush = |cur: &mut String, col: Option<Color>, out: &mut Vec<(String, Option<Color>)>| {
        if !cur.is_empty() {
            let t = cur.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&nbsp;", "\u{a0}");
            out.push((t, col));
            cur.clear();
        }
    };
    while let Some(i) = rest.find('<') {
        cur.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            cur.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let tag = rest[i + 1..i + j].trim().to_ascii_lowercase();
        rest = &rest[i + j + 1..];
        let col = *stack.last().unwrap_or(&None);
        if tag.starts_with("font") {
            flush(&mut cur, col, &mut out);
            let c = tag.find("color").and_then(|k| tag[k..].find('#').map(|h| k + h + 1)).and_then(|h| tag.get(h..h + 6)).and_then(|hex| u32::from_str_radix(hex, 16).ok());
            stack.push(c.map(|c| Color::srgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)).or(col));
        } else if tag.starts_with("/font") {
            flush(&mut cur, col, &mut out);
            if stack.len() > 1 {
                stack.pop();
            }
        } else if tag.starts_with("br") || tag == "/p" {
            cur.push('\n');
        }
    }
    cur.push_str(rest);
    flush(&mut cur, *stack.last().unwrap_or(&None), &mut out);
    out
}

#[allow(clippy::type_complexity)]
fn play_clips(
    (time, real): (Res<Time>, Res<Time<Real>>),
    mut commands: Commands,
    mut clips: Query<(Entity, &mut FlashClip, Option<&Children>, &InheritedVisibility)>,
    mut leaves: Query<(&mut Node, &mut UiTransform, &MaterialNode<FlashFill>, &mut Visibility), (With<LeafNode>, Without<TextNode>)>,
    mut text_nodes: Query<(&mut Node, &mut UiTransform, &mut Visibility, &mut TextNode, &mut Text, &mut TextColor, &mut TextLayout, &mut TextFont, Option<&Children>), Without<LeafNode>>,
    mut spans: Query<(&mut TextSpan, &mut TextColor), Without<TextNode>>,
    mut boxes: Query<(&mut Node, &mut Visibility, Option<&Children>), (With<TextBox>, Without<TextNode>, Without<LeafNode>)>,
    (mut mats, mut ui, mut images, fonts): (ResMut<Assets<FlashFill>>, ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>, Res<crate::ui_fonts::UiFonts>),
) {
    let mut out = Vec::new();
    let mut texts = Vec::new();
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
            if !fc.tweens.is_empty() || !fc.spins.is_empty() {
                fc.step_tweens(dt);
            }
        }
        if !shown.get() {
            continue;
        }
        out.clear();
        texts.clear();
        let view = concat(&[fc.scale, 0.0, 0.0, fc.scale, 0.0, 0.0], &fc.m);
        let tint = [1.0, 1.0, 1.0, fc.alpha, 0.0, 0.0, 0.0, 0.0];
        if (!fc.show_texts || fc.tl.texts.is_empty()) && fc.images.is_empty() && fc.masks.is_empty() {
            fc.clip.leaves(&fc.tl, &view, &tint, &mut out);
        } else {
            fc.clip.leaves_masked(&fc.tl, &view, &tint, &mut out, &mut texts, &fc.images, &fc.masks);
            if !fc.show_texts {
                texts.clear();
            }
        }
        // the fields cut by a mask they are turned in: their pictures drawn as bitmaps are
        if !texts.is_empty() && fc.show_texts {
            let base = fc.scale.max(1e-3);
            let mut drawn = Vec::new();
            for (i, t) in texts.iter().enumerate() {
                let Some(mask) = t.mask else { continue };
                let [a, b, c, d, _, _] = t.m;
                if b.abs() < 1e-4 && c.abs() < 1e-4 && a > 0.0 && d > 0.0 {
                    continue;
                }
                let Some(def) = fc.tl.texts.get(&t.id) else { continue };
                let content = fc.texts.get(&t.path).map(String::as_str).unwrap_or(def.text.as_str());
                let align = fc.autosize.get(&t.path).copied().unwrap_or(def.align);
                let wrap = fc.wrap.get(&t.path).copied().unwrap_or(def.wrap);
                let (x0, y0, x1, y1) = field_box(def, content, align, wrap);
                let size = UVec2::new(((x1 - x0) * base).ceil().clamp(1.0, 4096.0) as u32, ((y1 - y0) * base).ceil().clamp(1.0, 4096.0) as u32);
                let px = (def.size * base).round().max(1.0) as u32;
                let shown = (content.to_string(), px, wrap, align, true);
                let stale = fc.turned.get(&t.path).is_none_or(|tt| tt.size != size || tt.shown != shown);
                if stale {
                    if let Some(old) = fc.turned.remove(&t.path) {
                        commands.entity(old.cam).try_despawn();
                        commands.entity(old.root).try_despawn();
                    }
                    // (its own picture, laid out as the field would be, square and whole)
                    let image = images.add(Image::new_target_texture(size.x, size.y, bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb, None));
                    let cam = commands
                        .spawn((
                            Camera2d,
                            Camera { order: -90, clear_color: ClearColorConfig::Custom(Color::NONE), ..default() },
                            bevy::camera::RenderTarget::Image(image.clone().into()),
                            Msaa::Off,
                            TurnedTextOf(e),
                        ))
                        .id();
                    let title = def.font.contains("Title") || def.font.contains("Emerge");
                    let font = TextFont {
                        font: if title { fonts.title.clone().into() } else { Handle::<Font>::default().into() },
                        font_size: bevy::text::FontSize::Px(px as f32),
                        ..default()
                    };
                    let justify = match align {
                        1 => Justify::Right,
                        2 => Justify::Center,
                        3 => Justify::Justified,
                        _ => Justify::Left,
                    };
                    let layout = TextLayout::new(justify, if wrap { bevy::text::LineBreak::WordBoundary } else { bevy::text::LineBreak::NoWrap });
                    let field = Color::srgba(def.color[0], def.color[1], def.color[2], def.color[3]);
                    let p = if def.html || content.contains("<font") || content.contains("<br") { html_spans(content) } else { vec![(content.replace('\r', "\n"), None)] };
                    let root = commands.spawn((Node { width: Val::Px(size.x as f32), height: Val::Px(size.y as f32), ..default() }, bevy::ui::UiTargetCamera(cam), TurnedTextOf(e))).id();
                    let k = commands
                        .spawn((
                            Text::new(p.first().map(|s| s.0.clone()).unwrap_or_default()),
                            font.clone(),
                            TextColor(p.first().and_then(|s| s.1).unwrap_or(field)),
                            layout,
                            Node { width: Val::Px(size.x as f32), ..default() },
                            ChildOf(root),
                        ))
                        .id();
                    for s in p.iter().skip(1) {
                        commands.spawn((TextSpan::new(s.0.clone()), font.clone(), TextColor(s.1.unwrap_or(field)), ChildOf(k)));
                    }
                    if std::env::var("DH_FLASH_TEXT_LOG").is_ok() {
                        info!("flash text {} laid out on its own {}x{} picture (turned in its mask)", t.path, size.x, size.y);
                    }
                    fc.turned.insert(t.path.clone(), TurnedText { image, cam, root, size, shown, frames: 0 });
                }
                let Some(tt) = fc.turned.get_mut(&t.path) else { continue };
                // (drawn: its camera stops)
                tt.frames = tt.frames.saturating_add(1);
                if tt.frames == 4 {
                    commands.entity(tt.cam).try_insert(Camera { order: -90, is_active: false, clear_color: ClearColorConfig::Custom(Color::NONE), ..default() });
                }
                out.push(Leaf {
                    bitmap: 0,
                    size: Vec2::new(tt.size.x as f32 / base, tt.size.y as f32 / base),
                    m: concat(&t.m, &[1.0, 0.0, 0.0, 1.0, x0, y0]),
                    cx: t.cx,
                    uv: [0.0, 0.0, 1.0, 1.0],
                    repeat: false,
                    image: Some(tt.image.clone()),
                    mask: Some(mask),
                });
                drawn.push(i);
            }
            // (and not as fields)
            if !drawn.is_empty() {
                let mut i = 0;
                texts.retain(|_| {
                    i += 1;
                    !drawn.contains(&(i - 1))
                });
            }
        }
        let kids: Vec<Entity> = children.map(|c| c.iter().filter(|k| leaves.contains(*k)).collect()).unwrap_or_default();
        for (i, l) in out.iter().enumerate() {
            let tex = match &l.image {
                Some(h) => h.clone(),
                None => {
                    let Some((tex, _)) = ui.get(&mut images, &fc.movie, l.bitmap as u32) else { continue };
                    tex
                }
            };
            let mult = Vec4::new(l.cx[0], l.cx[1], l.cx[2], l.cx[3]);
            let add = Vec4::new(l.cx[4], l.cx[5], l.cx[6], l.cx[7]) / 255.0;
            let mut uv = Vec4::from_array(l.uv);
            // (a turned field's picture: drawn by a camera, its colours premultiplied)
            let drawn_by_camera = l.image.as_ref().is_some_and(|h| fc.turned.values().any(|tt| tt.image == *h));
            let opts = Vec4::new(if l.repeat { 1.0 } else { 0.0 }, if drawn_by_camera { 1.0 } else { 0.0 }, 0.0, 0.0);
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
            // (cut to a mask as drawn: the quad's corner and sides into the mask's frame)
            let (clip_a, clip_b, clip_r) = match &l.mask {
                Some(mk) => {
                    let [ia, ib, ic, id, itx, ity] = mk.inv;
                    let (sn, cs) = turn.sin_cos();
                    let (c0, c1) = (Vec2::new(cs * sx * w, sn * sx * w), Vec2::new(-sn * sy * h, cs * sy * h));
                    let lin = |v: Vec2| Vec2::new(ia * v.x + ic * v.y, ib * v.x + id * v.y);
                    let (a0, a1, t0) = (lin(c0), lin(c1), lin(mid - (c0 + c1) * 0.5) + Vec2::new(itx, ity));
                    (Vec4::new(a0.x, a0.y, a1.x, a1.y), Vec4::new(t0.x, t0.y, 1.0, 0.0), Vec4::from_array(mk.rect))
                }
                None => (Vec4::ZERO, Vec4::ZERO, Vec4::ZERO),
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
                let stale = mats.get(&mn.0).map(|f| f.mult != mult || f.add != add || f.uv != uv || f.opts != opts || f.texture != tex || f.clip_a != clip_a || f.clip_b != clip_b || f.clip_r != clip_r).unwrap_or(false);
                if stale {
                    if let Some(mut f) = mats.get_mut(&mn.0) {
                        f.mult = mult;
                        f.add = add;
                        f.uv = uv;
                        f.opts = opts;
                        f.texture = tex;
                        (f.clip_a, f.clip_b, f.clip_r) = (clip_a, clip_b, clip_r);
                    }
                }
            } else {
                let mut n = Node { position_type: PositionType::Absolute, ..default() };
                let mut t = UiTransform::default();
                place(&mut n, &mut t);
                commands.spawn((LeafNode, n, t, MaterialNode(mats.add(FlashFill { mult, add, uv, opts, texture: tex, clip_a, clip_b, clip_r })), Pickable::IGNORE, ChildOf(e)));
            }
        }
        for &k in kids.iter().skip(out.len()) {
            if let Ok((_, _, _, mut v)) = leaves.get_mut(k) {
                if *v != Visibility::Hidden {
                    *v = Visibility::Hidden;
                }
            }
        }
        // the text fields (over the bitmaps)
        let tkids: Vec<Entity> = children.map(|c| c.iter().filter(|k| boxes.contains(*k)).collect()).unwrap_or_default();
        let base = fc.scale.max(1e-3);
        for (i, t) in texts.iter().enumerate() {
            let Some(def) = fc.tl.texts.get(&t.id) else { continue };
            let content = fc.texts.get(&t.path).map(String::as_str).unwrap_or(def.text.as_str());
            let align = fc.autosize.get(&t.path).copied().unwrap_or(def.align);
            let wrap = fc.wrap.get(&t.path).copied().unwrap_or(def.wrap);
            // its box within its gutter; a line not wrapped is drawn on a wide box held at
            // the side it is aligned to (where Flash would let it run out)
            // (a line laid out only where the box holds it: the box at least a line high; a
            // wrapping field's as high as its text may run; a line not wrapped as wide as it is,
            // held at the side it is aligned to)
            let (x0, y0, x1, y1) = field_box(def, content, align, wrap);
            let title = def.font.contains("Title") || def.font.contains("Emerge");
            let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
            let [a, b, c, d, tx, ty] = t.m;
            let (cx_, cy_) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
            let mid = Vec2::new(a * cx_ + c * cy_ + tx, b * cx_ + d * cy_ + ty);
            let sx = (a * a + b * b).sqrt();
            let sy = if sx > 1e-6 { (a * d - b * c) / sx } else { 0.0 };
            let turn = b.atan2(a);
            let tint = |col: Color| {
                let l = col.to_srgba();
                Color::srgba(l.red * t.cx[0] + t.cx[4] / 255.0, l.green * t.cx[1] + t.cx[5] / 255.0, l.blue * t.cx[2] + t.cx[6] / 255.0, l.alpha * t.cx[3] + t.cx[7] / 255.0)
            };
            let field = Color::srgba(def.color[0], def.color[1], def.color[2], def.color[3]);
            let font = TextFont {
                font: if title { fonts.title.clone().into() } else { Handle::<Font>::default().into() },
                font_size: bevy::text::FontSize::Px((def.size * base).round().max(1.0)),
                ..default()
            };
            let justify = match align {
                1 => Justify::Right,
                2 => Justify::Center,
                3 => Justify::Justified,
                _ => Justify::Left,
            };
            let layout = TextLayout::new(justify, if wrap { bevy::text::LineBreak::WordBoundary } else { bevy::text::LineBreak::NoWrap });
            // (within its box: a mask's rectangle, else the clip's origin)
            let (ox, oy) = t.clip.map_or((0.0, 0.0), |r| (r[0], r[1]));
            let boxed = |n: &mut Node| {
                let (l, tp, bw, bh, ov) = match t.clip {
                    Some(r) => (r[0], r[1], (r[2] - r[0]).max(0.0), (r[3] - r[1]).max(0.0), Overflow::clip()),
                    None => (0.0, 0.0, 0.0, 0.0, Overflow::visible()),
                };
                if n.left != Val::Px(l) || n.top != Val::Px(tp) || n.width != Val::Px(bw) || n.height != Val::Px(bh) || n.overflow != ov {
                    (n.left, n.top, n.width, n.height, n.overflow) = (Val::Px(l), Val::Px(tp), Val::Px(bw), Val::Px(bh), ov);
                }
            };
            // (a field cut to a mask is drawn square and unscaled: Bevy cuts a turned or scaled
            // field's letters wrongly)
            let (sx, sy, turn) = if t.clip.is_some() { (base, base, 0.0) } else { (sx, sy, turn) };
            let place = |n: &mut Node, tf: &mut UiTransform| {
                n.left = Val::Px(mid.x - w * base * 0.5 - ox);
                n.top = Val::Px(mid.y - h * base * 0.5 - oy);
                n.width = Val::Px(w * base);
                n.height = Val::Px(h * base);
                tf.scale = Vec2::new(sx / base, sy / base);
                tf.rotation = Rot2::radians(turn);
            };
            let parts = |s: &str| -> Vec<(String, Option<Color>)> {
                if def.html || s.contains("<font") || s.contains("<br") {
                    html_spans(s)
                } else {
                    vec![(s.replace('\r', "\n"), None)]
                }
            };
            if let Some(&bx) = tkids.get(i) {
                let Ok((mut bn, mut bv, bkids)) = boxes.get_mut(bx) else { continue };
                boxed(&mut bn);
                if *bv != Visibility::Inherited {
                    *bv = Visibility::Inherited;
                }
                let Some(k) = bkids.and_then(|c| c.first().copied()) else { continue };
                let Ok((mut n, mut tf, mut v, mut tn, mut text, mut tc, mut tl_, mut tfont, kids)) = text_nodes.get_mut(k) else { continue };
                place(&mut n, &mut tf);
                if *v != Visibility::Inherited {
                    *v = Visibility::Inherited;
                }
                if tfont.font_size != font.font_size || tfont.font != font.font {
                    *tfont = font.clone();
                }
                if tl_.justify != layout.justify || tl_.linebreak != layout.linebreak {
                    *tl_ = layout;
                }
                if tn.shown != content {
                    // (spans anew)
                    let p = parts(content);
                    if let Some(ks) = kids {
                        for s in ks.iter() {
                            commands.entity(s).try_despawn();
                        }
                    }
                    text.0 = p.first().map(|s| s.0.clone()).unwrap_or_default();
                    for s in p.iter().skip(1) {
                        commands.spawn((TextSpan::new(s.0.clone()), font.clone(), TextColor(tint(s.1.unwrap_or(field))), ChildOf(k)));
                    }
                    tn.colors = p.iter().map(|s| s.1).collect();
                    tn.shown = content.to_string();
                }
                let c0 = tint(tn.colors.first().copied().flatten().unwrap_or(field));
                if tc.0 != c0 {
                    tc.0 = c0;
                }
                if let Some(ks) = kids {
                    for (j, s) in ks.iter().enumerate() {
                        if let Ok((_, mut sc)) = spans.get_mut(s) {
                            let c = tint(tn.colors.get(j + 1).copied().flatten().unwrap_or(field));
                            if sc.0 != c {
                                sc.0 = c;
                            }
                        }
                    }
                }
            } else {
                let p = parts(content);
                if std::env::var("DH_FLASH_TEXT_LOG").is_ok() {
                    info!("flash text {} {:?} at {mid} size {}x{} font {} scale {sx},{sy}", t.path, content, w * base, h * base, def.size * base);
                }
                let mut bn = Node { position_type: PositionType::Absolute, ..default() };
                boxed(&mut bn);
                let bx = commands.spawn((TextBox, bn, ZIndex(1), Visibility::Inherited, Pickable::IGNORE, ChildOf(e))).id();
                let mut n = Node { position_type: PositionType::Absolute, ..default() };
                let mut tf = UiTransform::default();
                place(&mut n, &mut tf);
                let k = commands
                    .spawn((
                        TextNode { shown: content.to_string(), colors: p.iter().map(|s| s.1).collect() },
                        Text::new(p.first().map(|s| s.0.clone()).unwrap_or_default()),
                        font.clone(),
                        TextColor(tint(p.first().and_then(|s| s.1).unwrap_or(field))),
                        layout,
                        n,
                        tf,
                        Visibility::Inherited,
                        Pickable::IGNORE,
                        ChildOf(bx),
                    ))
                    .id();
                for s in p.iter().skip(1) {
                    commands.spawn((TextSpan::new(s.0.clone()), font.clone(), TextColor(tint(s.1.unwrap_or(field))), ChildOf(k)));
                }
            }
        }
        for &k in tkids.iter().skip(texts.len()) {
            if let Ok((_, mut v, _)) = boxes.get_mut(k) {
                if *v != Visibility::Hidden {
                    *v = Visibility::Hidden;
                }
            }
        }
    }
}
