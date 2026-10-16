//! The help bar under the original's screens (`lib_dynamicHelpBar`: `_common.HelpBar`): the
//! actions at hand, each its key's icon (`lib_<btn>_Multi` on its `PC` frame, 90%: A the Enter
//! key, B Esc, X Shift, Y Alt) and its words in capitals (`help_txt`, `$NormalFont` 24), set
//! from the right (the last 10 in from the bar's end, the others 20 apart: `setDisplay`) on
//! its torn band (`lib_help_bkgdR`, the tiled `lib_help_bkgdExt` as wide as the entries less
//! 160, `lib_help_bkgdL`: `setBackGround`). It comes in from 20 to the right as it shows
//! (`OpenHelp`). Each entry is clickable (its `mouseArea`: 5 wider, 50 high); under the pointer
//! its key whitens, it and its words rise 2.5 and a line is drawn under the words
//! (`OnButtonOver`). A locked entry is dimmed: its icon at 30%, its words grey (#666666).

use crate::flash::{Clip, Ease, FlashClip, PropsTo};
use crate::ui_images::UiImages;
use bevy::prelude::*;
use dhcook::format::Timelines;
use std::sync::Arc;

pub struct HelpBarPlugin;

impl Plugin for HelpBarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Taps>().add_systems(PreUpdate, taps.after(bevy::input::InputSystems)).add_systems(Update, hover);
    }
}

/// Where the bar ends on the stage (`_root.help` at (1184,651)).
pub const END: Vec2 = Vec2::new(1184.0, 651.0);

/// A pad button a help entry stands for, and its key on the PC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Btn {
    A,
    B,
    X,
    Y,
}

impl Btn {
    fn name(self) -> &'static str {
        match self {
            Btn::A => "A",
            Btn::B => "B",
            Btn::X => "X",
            Btn::Y => "Y",
        }
    }
    /// Its key pressed, as the PC icons show it: Enter, Esc, Shift, Alt (those two tapped
    /// alone: Shift+Tab and Alt+Tab are not theirs).
    pub fn just_pressed(self, keys: &ButtonInput<KeyCode>, taps: &Taps) -> bool {
        match self {
            Btn::A => keys.just_pressed(KeyCode::Enter),
            Btn::B => keys.just_pressed(KeyCode::Escape),
            Btn::X => taps.tapped[0],
            Btn::Y => taps.tapped[1],
        }
    }
}

/// Shift and Alt tapped alone (let go with no other key held meanwhile): X and Y.
#[derive(Resource, Default)]
pub struct Taps {
    down: [bool; 2],
    clean: [bool; 2],
    pub tapped: [bool; 2],
}

const TAP_KEYS: [[KeyCode; 2]; 2] = [[KeyCode::ShiftLeft, KeyCode::ShiftRight], [KeyCode::AltLeft, KeyCode::AltRight]];

fn taps(keys: Res<ButtonInput<KeyCode>>, mut t: ResMut<Taps>) {
    for (i, ks) in TAP_KEYS.iter().enumerate() {
        let down = keys.any_pressed(*ks);
        t.tapped[i] = false;
        if down && !t.down[i] {
            t.clean[i] = true;
        }
        if down && keys.get_pressed().any(|k| !ks.contains(k)) {
            t.clean[i] = false;
        }
        if !down && t.down[i] {
            t.tapped[i] = std::mem::take(&mut t.clean[i]);
        }
        t.down[i] = down;
    }
}

/// An entry of the bar: its button, words, and whether it is locked (`lockState`).
pub struct Entry {
    pub btn: Btn,
    pub words: String,
    pub locked: bool,
}

impl Entry {
    pub fn new(btn: Btn, words: impl Into<String>) -> Entry {
        Entry { btn, words: words.into(), locked: false }
    }
    pub fn locked(mut self, locked: bool) -> Entry {
        self.locked = locked;
        self
    }
}

/// An entry's hit area (`mouseArea`): its button pressed when clicked. Its bar, and its place
/// on the bar (x, the icon's width, the words' width) for the pointer's highlight.
#[derive(Component)]
pub struct HelpButton {
    pub btn: Btn,
    bar: Entity,
    x: f32,
    icon_w: f32,
    words_w: f32,
}

/// The button of an entry clicked this frame, if any.
pub fn clicked(q: &Query<(&Interaction, &HelpButton), Changed<Interaction>>) -> Option<Btn> {
    q.iter().find(|(i, _)| **i == Interaction::Pressed).map(|(_, h)| h.btn)
}

/// The line under the words (`line_mc`: its shape 25.4 wide).
const LINE_W: f32 = 25.4;

/// The bar under `parent`, its end at `at` (the screen's pixels: the stage's [`END`]), drawn
/// `s` pixels a stage unit; its entries' hit areas with it.
#[allow(clippy::too_many_arguments)]
pub fn spawn(commands: &mut Commands, parent: Entity, tl: Arc<Timelines>, s: f32, at: Vec2, entries: &[Entry], ui: &mut UiImages, images: &mut Assets<Image>) -> Option<Entity> {
    const I: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let c = Clip::export(&tl, "lib_dynamicHelpBar")?;
    let mut fc = FlashClip::new("lib", tl.clone(), c).real().with_texts();
    fc.scale = s;
    fc.create_empty("", "_helpContent_mc", I);
    // each entry: its key's icon, half its width in; its words after it (`setBtn`)
    let mut laid = Vec::new();
    for e in entries {
        let n = e.btn.name();
        let path = format!("_helpContent_mc.{n}");
        fc.create_empty("_helpContent_mc", n, I);
        fc.attach(&path, &format!("lib_{n}_Multi"), n, [0.9, 0.0, 0.0, 0.9, 0.0, 0.0]);
        let icon = format!("{path}.{n}");
        fc.goto(&icon, "PC", false);
        let icon_w = fc.size(&icon).map(|v| v.x).unwrap_or(36.0);
        fc.set(&icon, PropsTo::default().x(icon_w * 0.5));
        fc.attach(&path, "help_txt", "txt", [1.0, 0.0, 0.0, 1.0, icon_w, 0.0]);
        let field = format!("{path}.txt.txt");
        let words = e.words.to_uppercase();
        fc.set_text(&field, if e.locked { format!("<font color=\"#666666\">{words}</font>") } else { words });
        fc.autosize.insert(field.clone(), 0);
        if e.locked {
            fc.set(&icon, PropsTo::default().alpha(0.3));
        }
        let words_w = fc.text_width(&field).unwrap_or(0.0);
        laid.push((path, icon_w, words_w));
    }
    // from the right: the last 10 in from the end, the others 20 apart
    let mut xs = vec![0.0; laid.len()];
    for i in (0..laid.len()).rev() {
        let w = laid[i].1 + laid[i].2;
        xs[i] = if i + 1 < laid.len() { xs[i + 1] - w - 20.0 } else { -w - 10.0 };
        fc.set(&laid[i].0, PropsTo::default().x(xs[i]));
    }
    // the band behind: its right end, the middle tiled as wide as the entries (their hit
    // areas 5 past the last) less 160, its left end
    let content_w = xs.first().map(|x| -5.0 - x).unwrap_or(0.0);
    fc.create_empty("bkgd", "bkgd_c", [1.0, 0.0, 0.0, 1.0, 0.0, 1.0]);
    fc.attach("bkgd.bkgd_c", "lib_help_bkgdR", "R", [1.0, 0.0, 0.0, 1.0, -100.0, 0.0]);
    let mut left = -100.0;
    let mid = (content_w - 1.6 * 100.0).round();
    if mid > 0.0 {
        if let Some((image, size)) = tl.exports.get("lib_help_bkgdExt -nopack").and_then(|id| ui.get(images, "lib", *id as u32)) {
            left -= mid;
            fc.create_empty("bkgd.bkgd_c", "M", [1.0, 0.0, 0.0, 1.0, left, -0.5 * size.y]);
            fc.images.insert("bkgd.bkgd_c.M".into(), crate::flash::LoadedImage { image, size: Vec2::new(mid, size.y), at: Vec2::ZERO, replace: false, tile: Some(size) });
        }
    }
    fc.attach("bkgd.bkgd_c", "lib_help_bkgdL", "L", [1.0, 0.0, 0.0, 1.0, left - 100.0, 0.0]);
    // (in from 20 to the right)
    fc.set("", PropsTo::default().x(20.0).alpha(0.0));
    fc.tween("", PropsTo::default().x(0.0).alpha(1.0), 0.3, Ease::StrongOut);
    let bar = commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(at.x), top: Val::Px(at.y), ..default() }, Pickable::IGNORE, ChildOf(parent))).id();
    for (i, (_, icon_w, words_w)) in laid.iter().enumerate() {
        let w = icon_w + words_w + 5.0;
        commands.spawn((
            HelpButton { btn: entries[i].btn, bar, x: xs[i], icon_w: *icon_w, words_w: *words_w },
            Button,
            Node { position_type: PositionType::Absolute, left: Val::Px(at.x + xs[i] * s), top: Val::Px(at.y - 25.0 * s), width: Val::Px(w * s), height: Val::Px(50.0 * s), ..default() },
            ChildOf(parent),
        ));
    }
    Some(bar)
}

/// The pointer over an entry: its key whitened, it and its words raised 2.5, the line drawn
/// out under the words (`OnButtonOver`); and back as it leaves (`OnButtonOut`).
fn hover(buttons: Query<(&Interaction, &HelpButton), Changed<Interaction>>, mut bars: Query<&mut FlashClip>) {
    for (i, h) in &buttons {
        let Ok(mut fc) = bars.get_mut(h.bar) else { continue };
        let n = h.btn.name();
        let (icon, words) = (format!("_helpContent_mc.{n}.{n}"), format!("_helpContent_mc.{n}.txt"));
        if *i == Interaction::None {
            fc.tween(&format!("{icon}._white_mc"), PropsTo::default().alpha(0.0), 0.1, Ease::StrongOut);
            fc.tween(&icon, PropsTo::default().y(0.0), 0.1, Ease::StrongOut);
            fc.tween(&words, PropsTo::default().y(0.0), 0.1, Ease::BackOut);
            fc.tween("line_mc", PropsTo::default().alpha(0.0), 0.25, Ease::StrongOut);
            continue;
        }
        if fc.props(&words).is_some_and(|p| p.y < -1.0) {
            continue;
        }
        fc.tween(&format!("{icon}._white_mc"), PropsTo::default().alpha(1.0), 0.1, Ease::StrongOut);
        fc.tween(&icon, PropsTo::default().y(-2.5), 0.1, Ease::StrongOut);
        // (the words' field 29.65 high: the line 5 above its middle)
        fc.set("line_mc", PropsTo::default().xscale(2.5 / LINE_W).alpha(0.0).y(0.5 * 29.65 - 5.0));
        fc.tween("line_mc", PropsTo::default().xscale(h.words_w / LINE_W).alpha(0.7).x(h.x + h.icon_w), 0.15, Ease::StrongOut);
        fc.tween(&words, PropsTo::default().y(-2.5), 0.1, Ease::StrongOut);
    }
}
