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
