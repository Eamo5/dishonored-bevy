//! A note picked up, read as `UI_Note` shows it (`ReadableNote`): the drifting backdrop dimmed
//! (`n_bkgd`), the paper (`_bkgdNote_mc`), the note's title on its brush (`lib_titleMc`, dark),
//! its words in the scrolling field (`n_Note`: `$NormalFont` 25, pale, 690 wide, the screen
//! turned a degree), or a location map in its frame with the map's name (`n_Map`). It is filed
//! in the journal; Escape (or E, Enter) puts it away. The game is blurred and paused under it
//! (`DisGFxMoviePlayerNote`'s `m_bBlurGameWhileActive`).

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
            .add_systems(Update, (open, close, draw).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The note being read (its key: an abstract item, or `map:` a location map).
#[derive(Resource, Default)]
pub struct NoteScreen {
    pub open: Option<String>,
    drawn: Option<(String, UVec2)>,
    /// (opened this frame: the key that read it does not close it)
    fresh: bool,
}

#[derive(Component)]
struct NoteRoot;

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
            let col = commands
                .spawn((
                    Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(690.0 * s), height: Val::Px(470.0 * s), overflow: Overflow::scroll_y(), ..default() },
                    ScrollPosition::default(),
                    UiTransform { rotation: Rot2::degrees(-1.0), ..default() },
                    Pickable::IGNORE,
                    ChildOf(root),
                ))
                .id();
            commands.spawn((Text::new(words), TextFont { font_size: FontSize::Px(25.0 * s), ..default() }, TextColor(PALE), TextLayout::new(Justify::Left, LineBreak::WordBoundary), Node { width: Val::Px(687.8 * s), flex_shrink: 0.0, ..default() }, Pickable::IGNORE, ChildOf(col)));
        }
    }
    // the help bar's: close
    let help = format!("[Esc]  {}", data.text("DisGFxMoviePlayerBase_Texts", "t_Back").to_uppercase());
    let p = off + Vec2::new(1184.0 - 300.0, 665.0 - 16.0) * s;
    commands.spawn((Text::new(help), TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(22.0 * s), ..default() }, TextColor(PALE), TextLayout::new(Justify::Right, LineBreak::NoWrap), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(300.0 * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
}
