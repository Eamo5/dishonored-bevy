//! A frame profiler for development builds (`--features profile`, which turns on Bevy's
//! tracing spans): each span's time (a system's run, a schedule's, the render graph's nodes) is
//! summed by its name, and with `DH_PROFILE=1` the costliest are logged every few seconds, as
//! milliseconds a frame (summed across the threads that ran them).

use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing::Subscriber;
use bevy::log::tracing_subscriber::layer::Context;
use bevy::log::tracing_subscriber::registry::LookupSpan;
use bevy::log::tracing_subscriber::Layer;
use bevy::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

static TOTALS: Mutex<Option<HashMap<String, (f64, u64)>>> = Mutex::new(None);

struct SpanName(String);
struct Entered(Instant);

struct NameVisitor(Option<String>);

impl Visit for NameVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "name" || field.name() == "system" {
            self.0 = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if (field.name() == "name" || field.name() == "system") && self.0.is_none() {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

pub struct ProfileLayer;

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for ProfileLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut v = NameVisitor(None);
        attrs.record(&mut v);
        let name = match v.0 {
            Some(n) => format!("{}: {}", attrs.metadata().name(), n.trim_matches('"')),
            None => attrs.metadata().name().to_string(),
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanName(name));
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().replace(Entered(Instant::now()));
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let ext = span.extensions();
        let (Some(name), Some(at)) = (ext.get::<SpanName>(), ext.get::<Entered>()) else { return };
        let ms = at.0.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut t) = TOTALS.lock() {
            let e = t.get_or_insert_with(HashMap::new).entry(name.0.clone()).or_default();
            e.0 += ms;
            e.1 += 1;
        }
    }
}

/// The layer for `LogPlugin::custom_layer`.
pub fn layer(_app: &mut App) -> Option<bevy::log::BoxedLayer> {
    Some(Box::new(ProfileLayer))
}

pub struct ProfilePlugin;

impl Plugin for ProfilePlugin {
    fn build(&self, app: &mut App) {
        if std::env::var_os("DH_PROFILE").is_some() {
            app.add_systems(Last, report);
        }
    }
}

/// Every five seconds, the costliest spans' milliseconds a frame.
fn report(time: Res<Time<bevy::time::Real>>, mut since: Local<f32>, mut frames: Local<u32>) {
    *since += time.delta_secs();
    *frames += 1;
    if *since < 5.0 {
        return;
    }
    let Ok(mut t) = TOTALS.lock() else { return };
    let Some(map) = t.take() else { return };
    let n = (*frames).max(1) as f64;
    let mut rows: Vec<(String, f64, u64)> = map.into_iter().map(|(k, (ms, c))| (k, ms / n, c)).collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    info!("profile: {:.2} ms a frame over {} frames", *since * 1000.0 / n as f32, *frames);
    let top = std::env::var("DH_PROFILE").ok().and_then(|v| v.parse::<usize>().ok()).filter(|n| *n > 1).unwrap_or(40);
    for (name, ms, c) in rows.iter().take(top) {
        info!("profile: {ms:7.3} ms/frame {:6} runs  {name}", c);
    }
    *since = 0.0;
    *frames = 0;
}
