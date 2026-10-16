//! The pause menu's help (`t_Help`: "Tutorials") as `UI_PauseMenu` draws it (`p_tuto_screen`
//! at the stage's middle over the pause screen's drifting backdrop, `TutorialsScreen` /
//! `TutorialsList`): its title on the library's brush
//! ("TUTORIALS", dark), the list's rules, and a `p_tuto_list_item` a tutorial note received
//! (its title, `$NormalFont` 28; the chosen one `over`: its brush lit, its words dark), in a
//! masked column of nine. Choosing one reads it (`tutorials`).

use crate::flash::{Clip, FlashClip};
use crate::menu::{Menu, MenuItem};
use crate::GameState;
use bevy::prelude::*;
use bevy::text::{FontSize, LineBreak};
use bevy::window::PrimaryWindow;

pub struct HelpScreenPlugin;

impl Plugin for HelpScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, draw.after(crate::menu::MenuSet).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Component)]
struct HelpRoot;

const MOVIE: &str = "PauseMenu";
const PALE: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);
/// The list's place on the stage (`_list_mc` at (-522.95, -183.15) from the middle), its rows'
/// pitch (48 high, 5 apart) and how many its mask shows (481 high).
const LIST_AT: Vec2 = Vec2::new(640.0 - 522.95, 360.0 - 183.15);
const ROW_PITCH: f32 = 53.0;
const ROWS: usize = 9;
/// The help's code among the menu's screens (`Menu::front_page`).
pub const CODE: u8 = 15;

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    menu: Res<Menu>,
    mut timelines: ResMut<crate::flash::MovieTimelines>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    data: Res<crate::gamedata::Data>,
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<HelpRoot>>,
    mut shown: Local<Option<(usize, Vec<String>, UVec2)>>,
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
) {
    let Ok(w) = window.single() else { return };
    let page = menu.front_page().filter(|p| p.0 == CODE);
    let key = page.as_ref().map(|p| (menu.selected(), p.1.clone(), UVec2::new(w.width() as u32, w.height() as u32)));
    if key == *shown {
        return;
    }
    // (its help bar comes in as the screen opens, then stays)
    let opening = shown.is_none();
    *shown = key;
    for e in &roots {
        commands.entity(e).try_despawn();
    }
    let Some((_, labels)) = page else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let ms = crate::hud::MovieSpace::of(w);
    let (s, off) = (ms.s, ms.off);
    let root = commands.spawn((HelpRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(55), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    let node = |p: Vec2| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + p.x * s), top: Val::Px(off.y + p.y * s), ..default() };
    // a text box on the stage, its words middled up and down
    let text = |commands: &mut Commands, at: Vec2, size: Vec2, turn: f32, words: String, font: TextFont, color: Color, tall: bool| {
        let bx = commands
            .spawn((
                Node { position_type: PositionType::Absolute, left: Val::Px(off.x + at.x * s), top: Val::Px(off.y + at.y * s), width: Val::Px(size.x * s), height: Val::Px(size.y * s), align_items: AlignItems::Center, ..default() },
                UiTransform { rotation: Rot2::degrees(turn), ..default() },
                Pickable::IGNORE,
                ChildOf(root),
            ))
            .id();
        let t = commands.spawn((Text::new(words), font, TextColor(color), TextLayout::new(Justify::Left, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(bx))).id();
        if tall {
            commands.entity(t).insert(UiTransform { scale: Vec2::new(1.0, 1.2), ..default() });
        }
    };
    let sel = menu.selected();
    let n = labels.len();
    let first = if n <= ROWS { 0 } else { sel.saturating_sub(ROWS - 1).min(n - ROWS) };
    // the pause screen's backdrop behind it (`bkgd_mc`, drifting: `AnimatedBackground`)
    if let Some(c) = Clip::export(&tl, "p_bkgd") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        let bg = crate::animbg::AnimatedBackground::new(&fc, "");
        let mut e = commands.spawn((fc, node(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
        if let Some(bg) = bg {
            e.insert(bg);
        }
    }
    // the screen: its rules, its title's brush
    if let Some(c) = Clip::export(&tl, "p_tuto_screen") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        fc.set_visible("_list_mc.maskContent", false);
        commands.spawn((fc, node(Vec2::new(640.0, 360.0)), Pickable::IGNORE, ChildOf(root)));
    }
    // the title, dark on its brush (`_title_mc` at (-544.45, -266.7), turned -1.7 degrees)
    let title = data.text("DisGFxMoviePlayerMenuBase_Texts", "t_Help").to_uppercase();
    text(&mut commands, Vec2::new(640.0 - 544.45 + 2.0, 360.0 - 266.7 + 5.3), Vec2::new(690.0, 49.4), -1.7, title, TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(38.0 * s), ..default() }, DARK, true);
    // the rows
    for v in 0..ROWS.min(n.saturating_sub(first)) {
        let i = first + v;
        let at = LIST_AT + Vec2::new(0.0, 24.0 + ROW_PITCH * v as f32);
        let on = i == sel;
        if let Some(c) = Clip::export(&tl, "p_tuto_list_item") {
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
            fc.scale = s;
            let t2 = fc.tl.clone();
            // (its brush one of three: `_bkgd_mc.mc`'s frames)
            if let Some(b) = fc.clip.child_mut("_bkgd_mc.mc") {
                b.goto(&t2, i % 3, false);
            }
            if on {
                fc.goto("", "over", true);
            }
            commands.spawn((fc, node(at), Pickable::IGNORE, ChildOf(root)));
        }
        // its words (`txt_mc.txt` at (48.85 + 2, -14.95), inside its 2-unit gutter)
        text(&mut commands, at + Vec2::new(52.85, -14.95), Vec2::new(826.95, 31.95), 0.0, labels[i].clone(), TextFont { font_size: FontSize::Px(28.0 * s), ..default() }, if on { DARK } else { PALE }, false);
        // its hit area (`btn`: the row's width)
        commands.spawn((MenuItem(i), Button, Node { position_type: PositionType::Absolute, left: Val::Px(off.x + (at.x - 7.65) * s), top: Val::Px(off.y + (at.y - 24.0) * s), width: Val::Px(935.0 * s), height: Val::Px(48.0 * s), ..default() }, ChildOf(root)));
    }
    // the help bar (`TutorialsScreen`: A Read Tutorial, B Back)
    if let Some(lib) = timelines.get("lib") {
        use crate::helpbar::{Btn, Entry};
        let entries = [Entry::new(Btn::A, data.text("DisGFxMoviePlayerMenuBase_Texts", "t_ReadTutorial")), Entry::new(Btn::B, data.text("DisGFxMoviePlayerBase_Texts", "t_Back"))];
        crate::helpbar::spawn(&mut commands, root, lib, s, off + crate::helpbar::END * s, &entries, &mut ui, &mut images, !opening);
    }
}
