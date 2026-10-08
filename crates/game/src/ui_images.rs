//! The original interface bitmaps (Scaleform atlas pieces cooked to `cache/ui/<movie>/<id>.png`
//! by `dhtool cook-ui`), loaded on demand by movie and character id.

use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;
use std::collections::HashMap;

#[derive(Resource, Default)]
pub struct UiImages(HashMap<(String, u32), Option<(Handle<Image>, Vec2)>>);

impl UiImages {
    /// An image of a movie and its pixel size (None if not cooked).
    pub fn get(&mut self, images: &mut Assets<Image>, movie: &str, id: u32) -> Option<(Handle<Image>, Vec2)> {
        self.0
            .entry((movie.to_string(), id))
            .or_insert_with(|| {
                let path = crate::loading::cache_dir().join("ui").join(movie).join(format!("{id}.png"));
                let bytes = std::fs::read(&path).ok()?;
                let img = Image::from_buffer(&bytes, ImageType::Extension("png"), CompressedImageFormats::NONE, true, ImageSampler::linear(), RenderAssetUsages::RENDER_WORLD).ok()?;
                let size = img.size_f32();
                Some((images.add(img), size))
            })
            .clone()
    }
}

impl UiImages {
    /// A named image of a cooked folder (`ui/powers/BlinkBig.png`, `ui/icons/ic_pow_Blink.png`).
    pub fn file(&mut self, images: &mut Assets<Image>, dir: &str, name: &str) -> Option<(Handle<Image>, Vec2)> {
        self.0
            .entry((format!("{dir}/{name}"), u32::MAX))
            .or_insert_with(|| {
                let path = crate::loading::cache_dir().join("ui").join(dir).join(format!("{name}.png"));
                let bytes = std::fs::read(&path).ok()?;
                let img = Image::from_buffer(&bytes, ImageType::Extension("png"), CompressedImageFormats::NONE, true, ImageSampler::linear(), RenderAssetUsages::RENDER_WORLD).ok()?;
                let size = img.size_f32();
                Some((images.add(img), size))
            })
            .clone()
    }

    /// A white version of an image (its brightest channel as intensity), for tinting the
    /// way Scaleform colour transforms recolour shared art.
    pub fn get_white(&mut self, images: &mut Assets<Image>, movie: &str, id: u32) -> Option<(Handle<Image>, Vec2)> {
        let key = (format!("{movie}#white"), id);
        if let Some(v) = self.0.get(&key) {
            return v.clone();
        }
        let v = self.get(images, movie, id).and_then(|(h, size)| {
            let mut img = images.get(&h)?.clone();
            if let Some(data) = img.data.as_mut() {
                for px in data.chunks_exact_mut(4) {
                    let m = px[0].max(px[1]).max(px[2]);
                    px[0] = m;
                    px[1] = m;
                    px[2] = m;
                }
            }
            Some((images.add(img), size))
        });
        self.0.insert(key, v.clone());
        v
    }
}

pub struct UiImagesPlugin;

impl Plugin for UiImagesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiImages>();
    }
}
