//! The original's Bink movies, cooked to MJPEG frames and a WAV soundtrack
//! (`dhcook::movies`): played over everything (the new game's intro with the Empress's
//! letter as subtitles, the Tower's title card, the credits), or looping behind the loading
//! screens (each map's `m_LoadingMovieName`).

use bevy::asset::RenderAssetUsages;
use bevy::audio::{PlaybackMode, Volume};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use dhcook::movies::{MovieFile, MovieIndex};
use std::sync::Arc;

pub struct MoviePlugin;

impl Plugin for MoviePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Movies>()
            .init_resource::<IntroPending>()
            .add_message::<PlayMovie>()
            .add_message::<StopMovie>()
            .add_message::<MovieDone>()
            .add_systems(Update, (start_movies, play_movies).chain());
    }
}

/// Play a cooked movie (by its file name, any case).
#[derive(Message, Clone)]
pub struct PlayMovie {
    pub name: String,
    pub looping: bool,
    pub skippable: bool,
    pub layer: MovieLayer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MovieLayer {
    /// over everything, letterboxed on black
    Fullscreen,
    /// inside the loading screen's backdrop (`MovieBackdrop`), under its title and hints
    Backdrop,
}

/// Stop the movie playing.
#[derive(Message, Clone, Copy)]
pub struct StopMovie;

/// A movie ended (or was skipped).
#[derive(Message, Clone)]
pub struct MovieDone(pub String);

/// A new game was started: its intro plays while the Tower loads.
#[derive(Resource, Default)]
pub struct IntroPending(pub bool);

/// Where the loading screen's movie goes.
#[derive(Component)]
pub struct MovieBackdrop;

#[derive(Component)]
struct MovieUi;

#[derive(Component)]
struct MovieSubtitle;

#[derive(Resource, Default)]
pub struct Movies {
    index: Option<Option<Arc<MovieIndex>>>,
    playing: Option<Playing>,
}

struct Playing {
    name: String,
    file: Arc<MovieFile>,
    t: f32,
    looping: bool,
    skippable: bool,
    layer: MovieLayer,
    image: Handle<Image>,
    shown: Option<usize>,
    task: Option<(usize, Task<Option<Vec<u8>>>)>,
    /// its interface (and camera) and soundtrack
    ui: Vec<Entity>,
    audio: Option<Entity>,
    subtitles: Vec<(f32, f32, String)>,
    sub_shown: Option<usize>,
}

impl Movies {
    /// The cooked movies' index (read once).
    pub fn index(&mut self) -> Option<Arc<MovieIndex>> {
        self.index
            .get_or_insert_with(|| {
                let path = dhcook::movies::index_path(&crate::loading::cache_dir());
                std::fs::read(path).ok().and_then(|d| serde_json::from_slice::<MovieIndex>(&d).ok()).map(Arc::new)
            })
            .clone()
    }
    /// Whether a cooked movie exists.
    pub fn has(&mut self, name: &str) -> bool {
        self.index().is_some_and(|i| i.movies.contains_key(&name.to_ascii_lowercase()))
    }
    /// The loading movie of a map, when cooked.
    pub fn loading_movie(&mut self, map: &str) -> Option<String> {
        let idx = self.index()?;
        let name = idx.maps.get(&map.to_ascii_lowercase()).cloned().unwrap_or_else(|| idx.default_loading.clone());
        idx.movies.contains_key(&name.to_ascii_lowercase()).then_some(name)
    }
    /// A full-screen movie that plays to its end is on (the level waits for it).
    pub fn holding(&self) -> bool {
        self.playing.as_ref().is_some_and(|p| p.layer == MovieLayer::Fullscreen && !p.looping)
    }
    pub fn playing(&self) -> Option<&str> {
        self.playing.as_ref().map(|p| p.name.as_str())
    }
}

fn decode(file: Arc<MovieFile>, i: usize) -> Option<Vec<u8>> {
    let jpg = file.frame(i)?;
    let img = image::load_from_memory_with_format(jpg, image::ImageFormat::Jpeg).ok()?;
    Some(img.to_rgba8().into_raw())
}

#[allow(clippy::too_many_arguments)]
fn start_movies(
    mut commands: Commands,
    mut movies: ResMut<Movies>,
    mut play: MessageReader<PlayMovie>,
    mut stop: MessageReader<StopMovie>,
    mut images: ResMut<Assets<Image>>,
    mut sources: ResMut<Assets<AudioSource>>,
    backdrop: Query<Entity, With<MovieBackdrop>>,
    settings: Res<crate::settings::Settings>,
    mut done: MessageWriter<MovieDone>,
) {
    let stopping = stop.read().count() > 0;
    let request = play.read().last().cloned();
    if stopping || request.is_some() {
        if let Some(p) = movies.playing.take() {
            for e in p.ui.iter().chain(p.audio.iter()) {
                commands.entity(*e).try_despawn();
            }
            done.write(MovieDone(p.name));
        }
    }
    let Some(req) = request else { return };
    let cache = crate::loading::cache_dir();
    let Some(idx) = movies.index() else { return };
    let key = req.name.to_ascii_lowercase();
    let Some(info) = idx.movies.get(&key).cloned() else {
        warn!("movie {} not cooked (dhtool cook-movies)", req.name);
        done.write(MovieDone(req.name));
        return;
    };
    let file = match MovieFile::read(&dhcook::movies::movie_path(&cache, &req.name)) {
        Ok(f) if f.frames() > 0 => Arc::new(f),
        _ => {
            warn!("movie {}: unreadable", req.name);
            done.write(MovieDone(req.name));
            return;
        }
    };
    let image = images.add(Image::new_fill(
        Extent3d { width: file.width, height: file.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    ));
    let aspect = file.width as f32 / file.height.max(1) as f32;
    let mut ui = Vec::new();
    match req.layer {
        MovieLayer::Fullscreen => {
            let cam = commands.spawn((MovieUi, Camera2d, Camera { order: 300, clear_color: ClearColorConfig::Custom(Color::BLACK), ..default() })).id();
            let root = commands
                .spawn((
                    MovieUi,
                    Node { width: percent(100), height: percent(100), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() },
                    BackgroundColor(Color::BLACK),
                    UiTargetCamera(cam),
                    GlobalZIndex(i32::MAX),
                ))
                .with_children(|p| {
                    p.spawn((ImageNode::new(image.clone()), Node { width: percent(100), max_height: percent(100), aspect_ratio: Some(aspect), ..default() }));
                    p.spawn((
                        MovieSubtitle,
                        Text::new(""),
                        TextFont { font_size: FontSize::Px(30.0), ..default() },
                        TextColor(Color::srgb(0.95, 0.93, 0.88)),
                        TextShadow { offset: Vec2::new(2.0, 2.0), color: Color::BLACK.with_alpha(0.95) },
                        TextLayout::justify(Justify::Center),
                        Node { position_type: PositionType::Absolute, bottom: percent(8), left: percent(15), width: percent(70), justify_content: JustifyContent::Center, ..default() },
                    ));
                })
                .id();
            ui.push(cam);
            ui.push(root);
        }
        MovieLayer::Backdrop => {
            let Some(parent) = backdrop.iter().next() else { return };
            let node = commands
                .spawn((ImageNode::new(image.clone()), Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, ChildOf(parent)))
                .id();
            ui.push(node);
        }
    }
    // the soundtrack (the music's volume)
    let audio = info.audio.then(|| std::fs::read(dhcook::movies::audio_path(&cache, &req.name)).ok()).flatten().map(|bytes| {
        let src = sources.add(AudioSource { bytes: Arc::from(bytes) });
        let mode = if req.looping { PlaybackMode::Loop } else { PlaybackMode::Despawn };
        commands
            .spawn((AudioPlayer(src), PlaybackSettings { mode, volume: Volume::Linear(settings.music_volume.max(0.0)), ..default() }))
            .id()
    });
    info!("movie {} ({:.1} s, {}x{}, {:?})", req.name, info.seconds, file.width, file.height, req.layer);
    movies.playing = Some(Playing {
        name: req.name,
        file,
        t: 0.0,
        looping: req.looping,
        skippable: req.skippable,
        layer: req.layer,
        image,
        shown: None,
        task: None,
        ui,
        audio,
        subtitles: idx.subtitles.get(&key).cloned().unwrap_or_default(),
        sub_shown: None,
    });
}

#[allow(clippy::too_many_arguments)]
fn play_movies(
    mut commands: Commands,
    time: Res<Time<bevy::time::Real>>,
    mut movies: ResMut<Movies>,
    mut images: ResMut<Assets<Image>>,
    (keys, mouse): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>),
    alive: Query<()>,
    mut subs: Query<&mut Text, With<MovieSubtitle>>,
    mut done: MessageWriter<MovieDone>,
) {
    let Some(p) = movies.playing.as_mut() else { return };
    // its screen went (the loading screen's, with the loading)
    if p.ui.iter().any(|e| alive.get(*e).is_err()) {
        let p = movies.playing.take().unwrap();
        for e in p.ui.iter().chain(p.audio.iter()) {
            commands.entity(*e).try_despawn();
        }
        return;
    }
    let before = p.t;
    p.t += time.delta_secs().min(0.1);
    // (`DH_MOVIE_SHOT=SECS:PATH`: a screenshot that far in, for tests)
    if let Some((at, path)) = std::env::var("DH_MOVIE_SHOT").ok().and_then(|v| v.split_once(':').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.to_string())))) {
        if before < at && p.t >= at {
            commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(bevy::render::view::screenshot::save_to_disk(path));
        }
    }
    let n = p.file.frames();
    let length = n as f32 / p.file.fps.max(1.0);
    let skipped = p.skippable && p.t > 0.5 && (keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) || mouse.just_pressed(MouseButton::Left));
    if p.t >= length {
        if p.looping {
            p.t %= length.max(0.01);
        } else {
            let p = movies.playing.take().unwrap();
            for e in p.ui.iter().chain(p.audio.iter()) {
                commands.entity(*e).try_despawn();
            }
            done.write(MovieDone(p.name));
            return;
        }
    }
    if skipped {
        let p = movies.playing.take().unwrap();
        info!("movie {} skipped at {:.1} s", p.name, p.t);
        for e in p.ui.iter().chain(p.audio.iter()) {
            commands.entity(*e).try_despawn();
        }
        done.write(MovieDone(p.name));
        return;
    }
    let want = ((p.t * p.file.fps) as usize).min(n - 1);
    // a decoded frame in: shown
    if let Some((i, task)) = p.task.as_mut() {
        if task.is_finished() {
            let i = *i;
            let (_, task) = p.task.take().unwrap();
            if let Some(rgba) = block_on(future::poll_once(task)).flatten() {
                if let Some(mut img) = images.get_mut(&p.image) {
                    if rgba.len() == (p.file.width * p.file.height * 4) as usize {
                        img.data = Some(rgba);
                        p.shown = Some(i);
                    }
                }
            }
        }
    }
    // the next one wanted (frames are dropped when decoding falls behind)
    if p.task.is_none() && p.shown != Some(want) {
        let file = p.file.clone();
        p.task = Some((want, AsyncComputeTaskPool::get().spawn(async move { decode(file, want) })));
    }
    // the subtitle of the moment
    let cur = p.subtitles.iter().position(|(a, b, _)| p.t >= *a && p.t < *b);
    if cur != p.sub_shown {
        p.sub_shown = cur;
        let text = cur.map(|i| p.subtitles[i].2.clone()).unwrap_or_default();
        for mut t in &mut subs {
            t.0 = text.clone();
        }
    }
}
