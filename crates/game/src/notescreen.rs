//! A note picked up, read as `UI_Note` shows it (`ReadableNote`): the drifting backdrop dimmed
//! (`n_bkgd`), the paper (`_bkgdNote_mc`), the note's title on its brush (`lib_titleMc`, dark),
//! its words in the scrolling field (`n_Note`: `$NormalFont` 25, pale, 690 wide, the screen
//! turned a degree), or a location map in its frame with the map's name (`n_Map`). It is filed
//! in the journal; Escape (or E, Enter) puts it away. The game is blurred and paused under it
//! (`DisGFxMoviePlayerNote`'s `m_bBlurGameWhileActive`).
//!
//! The field (`_common.AnalogScrollView`: the words under a mask, both turned) scrolls with the
//! wheel and the arrows. A turned node can't be clipped here, so the words are laid out once
//! (a span a word, to learn each one's line) and shown a window of whole lines at a time.

use crate::audio::PostEvent;
use crate::flash::{Clip, FlashClip};
use crate::gamedata::Data;
use crate::hud::Paused;
use crate::GameState;
use bevy::prelude::*;
use bevy::text::{FontSize, LineBreak};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

pub struct NoteScreenPlugin;

impl Plugin for NoteScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoteScreen>()
            .add_systems(OnEnter(GameState::InGame), |mut n: ResMut<NoteScreen>| *n = NoteScreen::default())
            .add_systems(Update, (open, close, draw, scroll).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The note being read (its key: an abstract item, or `map:` a location map).
#[derive(Resource, Default)]
pub struct NoteScreen {
    pub open: Option<String>,
    drawn: Option<(String, UVec2)>,
    /// (opened this frame: the key that read it does not close it)
    fresh: bool,
    /// the words as laid out in the field (its lines), the first shown, how many fit, and
    /// the first drawn
    lines: Vec<String>,
    first: usize,
    fit: usize,
    shown: Option<usize>,
    /// the field's height (px)
    page: f32,
}

#[derive(Component)]
struct NoteRoot;

/// The words laid out unseen, a span a word, to learn their lines.
#[derive(Component)]
struct NoteMeasure;

/// The lines shown.
#[derive(Component)]
struct NoteWords;

/// A note's words as spans: each word with the space after it, each line's end a span.
fn tokens(words: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in words.split('\n').enumerate() {
        if i > 0 {
            out.push("\n".to_string());
        }
        let mut cur = String::new();
        for c in line.chars() {
            if c == ' ' {
                cur.push(c);
            } else {
                if cur.ends_with(' ') {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

const MOVIE: &str = "Note";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);

fn open(mut read: MessageReader<crate::journal::ReadNote>, mut note: ResMut<NoteScreen>, mut paused: ResMut<Paused>, mut cursor: Single<&mut CursorOptions>, mut sfx: MessageWriter<PostEvent>) {
    let Some(r) = read.read().last() else { return };
    // (the audiographs play; they are not read)
    if r.0.starts_with("ag:") {
        return;
    }
    note.open = Some(r.0.clone());
    note.fresh = true;
    paused.0 = true;
    cursor.visible = true;
    cursor.grab_mode = CursorGrabMode::None;
    sfx.write(PostEvent::named("Snd_UI_Ingame_Note_Open", None));
}

fn close(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut note: ResMut<NoteScreen>,
    mut paused: ResMut<Paused>,
    mut cursor: Single<&mut CursorOptions>,
    roots: Query<Entity, With<NoteRoot>>,
    mut sfx: MessageWriter<PostEvent>,
    scripted: Option<Res<crate::script::Scripted>>,
) {
    if note.open.is_none() {
        return;
    }
    if note.fresh {
        note.fresh = false;
        return;
    }
    let clicked = scripted.is_none() && mouse.just_pressed(MouseButton::Right);
    if !(keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::KeyE) || clicked) {
        return;
    }
    note.open = None;
    note.drawn = None;
    note.lines.clear();
    for e in &roots {
        commands.entity(e).despawn();
    }
    paused.0 = false;
    cursor.visible = false;
    cursor.grab_mode = CursorGrabMode::Locked;
    sfx.write(PostEvent::named("Snd_UI_Ingame_Note_Close", None));
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    mut note: ResMut<NoteScreen>,
    data: Res<Data>,
    mut timelines: ResMut<crate::flash::MovieTimelines>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<NoteRoot>>,
) {
    let Some(key) = note.open.clone() else { return };
    let Ok(w) = window.single() else { return };
    let want = Some((key.clone(), UVec2::new(w.width() as u32, w.height() as u32)));
    if note.drawn == want {
        return;
    }
    note.drawn = want;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let Some(tl) = timelines.get(MOVIE) else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let root = commands.spawn((NoteRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(85), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    let map = crate::journal::location_map(&key);
    let (title, words) = match &map {
        Some((t, _)) => (t.clone(), String::new()),
        None => {
            let (n, d) = data.0.abstract_items.get(&key).cloned().unwrap_or_default();
            (if n.is_empty() { key.rsplit('.').next().unwrap_or(&key).replace('_', " ") } else { n }, crate::gamedata::readable(&d))
        }
    };
    // the screen: its dimmed backdrop drifting, the paper, the note's or the map's frame
    if let Some(c) = Clip::export(&tl, "n_readableNote") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        fc.set_visible("_tutorial_mc", false);
        fc.set_visible(if map.is_some() { "_note_mc" } else { "_map_mc" }, false);
        // (the map's legend: its places aren't kept)
        fc.set_visible("_map_mc._legend_mc", false);
        let bg = crate::animbg::AnimatedBackground::new(&fc, "_bkgd_mc");
        let p = off + Vec2::new(640.0, 360.0) * s;
        let mut e = commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
        if let Some(bg) = bg {
            e.insert(bg);
        }
    }
    // the title, dark on its brush (`_title_mc` at (-543.05,-294.2))
    let c = Vec2::new(640.0 - 543.05 + 2.0 + 345.0, 360.0 - 294.2 + 5.3 + 24.7);
    let p = off + (c - Vec2::new(345.0, 24.7)) * s;
    let bx = commands
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(690.0 * s), height: Val::Px(49.4 * s), align_items: AlignItems::Center, ..default() },
            UiTransform { rotation: Rot2::degrees(-1.7), ..default() },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    commands.spawn((Text::new(title.to_uppercase()), TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(38.0 * s), ..default() }, TextColor(DARK), TextLayout::new(Justify::Left, LineBreak::NoWrap), UiTransform { scale: Vec2::new(1.0, 1.2), ..default() }, Pickable::IGNORE, ChildOf(bx)));
    match map {
        // the map in its frame (800 x 500 at (461,398), turned a degree)
        Some((_, image)) => {
            if let Some((h, size)) = ui.file(&mut images, "maps", image) {
                let k = (800.0 / size.x.max(1.0)).min(500.0 / size.y.max(1.0));
                let (iw, ih) = (size.x * k, size.y * k);
                let c = Vec2::new(461.05, 398.15);
                let p = off + (c - Vec2::new(iw, ih) * 0.5) * s;
                commands.spawn((
                    ImageNode::new(h),
                    Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(iw * s), height: Val::Px(ih * s), ..default() },
                    UiTransform { rotation: Rot2::degrees(-1.0), ..default() },
                    Pickable::IGNORE,
                    ChildOf(root),
                ));
            }
        }
        // the words, in the scrolling field (`_note_mc` at (100,22), its view at (-547.45,-234.5))
        None => {
            let at = Vec2::new(640.0 + 100.0 - 547.45 + 2.0, 360.0 + 22.0 - 234.5 + 2.0);
            let p = off + at * s;
            let font = TextFont { font_size: FontSize::Px(25.0 * s), ..default() };
            // (laid out unseen first, as wide as the field)
            let measure = commands
                .spawn((
                    NoteMeasure,
                    Text::new(""),
                    font.clone(),
                    TextColor(PALE),
                    TextLayout::new(Justify::Left, LineBreak::WordBoundary),
                    Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(687.8 * s), ..default() },
                    Visibility::Hidden,
                    Pickable::IGNORE,
                    ChildOf(root),
                ))
                .id();
            for t in tokens(&words) {
                commands.spawn((TextSpan::new(t), font.clone(), TextColor(PALE), ChildOf(measure)));
            }
            let field = commands
                .spawn((
                    Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(690.0 * s), height: Val::Px(470.0 * s), ..default() },
                    UiTransform { rotation: Rot2::degrees(-1.0), ..default() },
                    Pickable::IGNORE,
                    ChildOf(root),
                ))
                .id();
            commands.spawn((NoteWords, Text::new(""), font, TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(field)));
            note.lines.clear();
            note.first = 0;
            note.shown = None;
            note.page = 470.0 * s;
        }
    }
    // the help bar's: close
    let help = format!("[Esc]  {}", data.text("DisGFxMoviePlayerBase_Texts", "t_Back").to_uppercase());
    let p = off + Vec2::new(1184.0 - 300.0, 665.0 - 16.0) * s;
    commands.spawn((Text::new(help), TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(22.0 * s), ..default() }, TextColor(PALE), TextLayout::new(Justify::Right, LineBreak::NoWrap), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(300.0 * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
}

/// The field's lines, once laid out; the window of them shown, moved by the wheel (three
/// lines a notch), the arrows (one) and the page keys.
#[allow(clippy::too_many_arguments)]
fn scroll(
    mut commands: Commands,
    mut note: ResMut<NoteScreen>,
    measure: Query<(Entity, &bevy::text::TextLayoutInfo, &Children), With<NoteMeasure>>,
    spans: Query<&TextSpan>,
    mut words: Query<&mut Text, With<NoteWords>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
) {
    let notches: f32 = wheel.read().map(|w| if w.unit == bevy::input::mouse::MouseScrollUnit::Line { w.y } else { w.y / 40.0 }).sum();
    if note.open.is_none() {
        return;
    }
    if let Ok((e, info, kids)) = measure.single() {
        if info.glyphs.is_empty() {
            return;
        }
        // each word's line (by its first letter), the lines between with none blank
        let count = info.glyphs.iter().map(|g| g.line_index).max().unwrap_or(0) + 1;
        let mut line_of = vec![None; kids.len() + 1];
        let (mut top, mut bottom) = (f32::INFINITY, f32::NEG_INFINITY);
        for g in &info.glyphs {
            if let Some(l) = line_of.get_mut(g.section_index) {
                l.get_or_insert(g.line_index);
            }
            if g.line_index == 0 {
                top = top.min(g.position.y);
            }
            if g.line_index + 1 == count {
                bottom = bottom.max(g.position.y);
            }
        }
        let mut lines = vec![String::new(); count];
        for (k, kid) in kids.iter().enumerate() {
            // (section 0 is the empty root)
            if let (Some(Some(l)), Ok(span)) = (line_of.get(k + 1), spans.get(kid)) {
                lines[*l].push_str(&span.0);
            }
        }
        for l in &mut lines {
            l.truncate(l.trim_end().len());
        }
        let step = if count > 1 { (bottom - top) / (count - 1) as f32 } else { info.size.y.max(1.0) };
        let page = note.page * info.scale_factor;
        note.fit = ((page / step.max(1.0)).floor() as usize).max(1);
        note.lines = lines;
        note.first = 0;
        note.shown = None;
        commands.entity(e).despawn();
    }
    if note.lines.is_empty() {
        return;
    }
    let last = note.lines.len().saturating_sub(note.fit);
    let mut d = -(notches * 3.0).round() as i64;
    let fit = note.fit as i64;
    for (k, by) in [(KeyCode::ArrowDown, 1), (KeyCode::KeyS, 1), (KeyCode::ArrowUp, -1), (KeyCode::KeyW, -1), (KeyCode::PageDown, fit - 1), (KeyCode::PageUp, 1 - fit)] {
        if keys.just_pressed(k) {
            d += by;
        }
    }
    note.first = (note.first as i64 + d).clamp(0, last as i64) as usize;
    if note.shown != Some(note.first) {
        note.shown = Some(note.first);
        let end = (note.first + note.fit).min(note.lines.len());
        let shown = note.lines[note.first..end].join("\n");
        if let Ok(mut t) = words.single_mut() {
            t.0 = shown;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens() {
        assert_eq!(super::tokens("Dear Sir,\n\nThe end. "), vec!["Dear ", "Sir,", "\n", "\n", "The ", "end. "]);
    }
}
