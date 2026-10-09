//! The original user interface fonts (cooked from the Scaleform font library to TrueType by
//! `dhtool cook-ui`): Chalet Comprimé (body text, HUD) replaces the default font everywhere;
//! Emerge BF (titles, location banners, menus) is available through [`UiFonts`].

use bevy::prelude::*;

pub struct UiFontsPlugin;

impl Plugin for UiFontsPlugin {
    fn build(&self, app: &mut App) {
        // loaded right away: the first screen (loading) is built before any startup system
        let mut ui = UiFonts::default();
        if let Some(mut fonts) = app.world_mut().get_resource_mut::<Assets<Font>>() {
            match read("ChaletComprimeCologneEighty.ttf") {
                Some(body) => {
                    let _ = fonts.insert(AssetId::default(), body);
                }
                None => warn!("original fonts not cooked (run `dhtool cook-ui`); using the default font"),
            }
            if let Some(title) = read("EmergeBF.ttf") {
                ui.title = fonts.add(title);
            }
        }
        app.insert_resource(ui);
    }
}

#[derive(Resource, Default, Clone)]
pub struct UiFonts {
    /// display face (falls back to the default font)
    pub title: Handle<Font>,
}

fn read(name: &str) -> Option<Font> {
    let path = crate::loading::cache_dir().join("ui").join("fonts").join(name);
    let bytes = std::fs::read(&path).ok()?;
    Some(Font::from_bytes(bytes))
}

/// The fonts' files, for measuring (body, title).
static FACES: std::sync::OnceLock<(Vec<u8>, Vec<u8>)> = std::sync::OnceLock::new();

/// How wide a line of text is in a font (`title`: the display face) at a size, in the
/// size's units: its glyphs' advances.
pub fn measure(title: bool, size: f32, text: &str) -> f32 {
    let faces = FACES.get_or_init(|| {
        let file = |n: &str| std::fs::read(crate::loading::cache_dir().join("ui").join("fonts").join(n)).unwrap_or_default();
        (file("ChaletComprimeCologneEighty.ttf"), file("EmergeBF.ttf"))
    });
    let data = if title { &faces.1 } else { &faces.0 };
    let Ok(face) = ttf_parser::Face::parse(data, 0) else { return text.chars().count() as f32 * size * 0.5 };
    let em = face.units_per_em().max(1) as f32;
    text.lines()
        .map(|l| l.chars().map(|c| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g)).unwrap_or((em * 0.5) as u16) as f32).sum::<f32>() * size / em)
        .fold(0.0, f32::max)
}
