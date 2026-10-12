//! Soft text shadows: the original's text fields carry Flash's drop-shadow filter (the
//! subtitles' blurred 8 x 8 at strength 1.9, no distance: a dark smudge behind the letters
//! that keeps them readable over a bright scene). Bevy's text shadow is a single hard copy;
//! a `SoftShadow` is drawn as faint copies of the text on two rings about it (and its
//! filter's distance off), beneath it, which build up where the letters are close as the
//! blurred alpha does.

use bevy::prelude::*;

pub struct SoftShadowPlugin;

impl Plugin for SoftShadowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_soft_shadows.before(bevy::ui::UiSystems::Prepare));
    }
}

/// A text's drop shadow, in screen pixels.
#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub struct SoftShadow {
    /// the filter's colour (its alpha, the filter's own)
    pub color: Color,
    /// its blur (Flash's `blurX`, `blurY`)
    pub blur: Vec2,
    /// its distance along its angle
    pub offset: Vec2,
    /// its strength (Flash multiplies the blurred alpha by it)
    pub strength: f32,
}

impl SoftShadow {
    /// A Flash drop shadow (stage units, at `scale` screen pixels to the unit): colour, blur,
    /// distance at 45 degrees (Flash's default angle), strength.
    pub fn flash(color: Color, blur: f32, distance: f32, strength: f32, scale: f32) -> SoftShadow {
        let d = distance * std::f32::consts::FRAC_1_SQRT_2 * scale;
        SoftShadow { color, blur: Vec2::splat(blur * scale), offset: Vec2::splat(d), strength }
    }
}

/// The copies drawing a text's shadow.
#[derive(Component)]
struct ShadowCopies(Vec<Entity>);

/// A copy of a text drawing part of its shadow (`SoftShadow`).
#[derive(Component)]
struct ShadowCopy(Entity);

/// Copies on each ring (the second turned half a step).
const RING: usize = 8;

/// The copies' offsets about the text, in blurs: two rings, at a third and two thirds of the
/// blur (Flash's three-pass box blur spreads about half its width; most of it nearer).
fn ring_offsets(blur: Vec2) -> [Vec2; RING * 2] {
    let mut out = [Vec2::ZERO; RING * 2];
    for (i, o) in out.iter_mut().enumerate() {
        let (ring, k) = (i / RING, i % RING);
        let a = (k as f32 + ring as f32 * 0.5) / RING as f32 * std::f32::consts::TAU;
        let r = if ring == 0 { 0.32 } else { 0.64 };
        *o = Vec2::new(a.cos() * blur.x, a.sin() * blur.y) * r;
    }
    out
}

/// Each copy's opacity: inside the letters some seven copies overlap, building up to the
/// filter's strength's share of its colour (strength 1 about half, 1.9 most of it).
fn copy_alpha(strength: f32) -> f32 {
    let whole = (0.45 * strength).clamp(0.0, 0.95);
    1.0 - (1.0 - whole).powf(1.0 / 7.0)
}

#[allow(clippy::type_complexity)]
fn sync_soft_shadows(
    mut commands: Commands,
    sources: Query<
        (Entity, &SoftShadow, &Text, &TextFont, &TextLayout, &Node, &TextColor, &Visibility, Option<&UiTransform>, Option<&ChildOf>, Option<&Children>, Option<&ShadowCopies>),
        Without<ShadowCopy>,
    >,
    spans: Query<&TextSpan>,
    mut copies: Query<(&mut Text, &mut TextFont, &mut TextLayout, &mut Node, &mut TextColor, &mut Visibility, &mut UiTransform), With<ShadowCopy>>,
    stray: Query<(Entity, &ShadowCopy)>,
    mut gone: RemovedComponents<SoftShadow>,
    owners: Query<&ShadowCopies>,
) {
    // a text that no longer casts one: its copies go
    for e in gone.read() {
        if let Ok(c) = owners.get(e) {
            for &k in &c.0 {
                commands.entity(k).try_despawn();
            }
            commands.entity(e).try_remove::<ShadowCopies>();
        }
    }
    // (and the copies of a text gone)
    for (k, c) in &stray {
        if !sources.contains(c.0) {
            commands.entity(k).try_despawn();
        }
    }
    for (e, sh, text, font, layout, node, color, vis, tf, parent, children, made) in &sources {
        // (its spans as one: the shadow is one colour)
        let mut whole = text.0.clone();
        for s in children.into_iter().flatten().filter_map(|c| spans.get(*c).ok()) {
            whole.push_str(&s.0);
        }
        let shown = color.0.alpha() * sh.color.alpha();
        let tint = sh.color.with_alpha(copy_alpha(sh.strength) * shown);
        let base = tf.copied().unwrap_or_default();
        let offsets = ring_offsets(sh.blur);
        let Some(made) = made else {
            let mut list = Vec::with_capacity(offsets.len());
            for o in offsets {
                let mut t = base;
                t.translation = shifted(base.translation, sh.offset + o);
                let mut c = commands.spawn((
                    ShadowCopy(e),
                    Text::new(whole.clone()),
                    font.clone(),
                    *layout,
                    node.clone(),
                    TextColor(tint),
                    *vis,
                    t,
                    ZIndex(-1),
                    Pickable::IGNORE,
                ));
                if let Some(p) = parent {
                    c.insert(ChildOf(p.parent()));
                }
                list.push(c.id());
            }
            commands.entity(e).insert(ShadowCopies(list));
            continue;
        };
        for (&k, o) in made.0.iter().zip(offsets) {
            let Ok((mut t, mut f, mut l, mut n, mut c, mut v, mut x)) = copies.get_mut(k) else { continue };
            if t.0 != whole {
                t.0.clone_from(&whole);
            }
            if *f != *font {
                *f = font.clone();
            }
            if l.justify != layout.justify || l.linebreak != layout.linebreak {
                *l = *layout;
            }
            if *n != *node {
                *n = node.clone();
            }
            if c.0 != tint {
                c.0 = tint;
            }
            if *v != *vis {
                *v = *vis;
            }
            let mut want = base;
            want.translation = shifted(base.translation, sh.offset + o);
            if *x != want {
                *x = want;
            }
        }
    }
}

/// A translation moved by `by` pixels (on what is given in pixels; the rest as it is).
fn shifted(v: Val2, by: Vec2) -> Val2 {
    let px = |v: Val, d: f32| match v {
        Val::Px(p) => Val::Px(p + d),
        Val::Auto => Val::Px(d),
        other => other,
    };
    Val2 { x: px(v.x, by.x), y: px(v.y, by.y) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rings_spread_with_the_blur() {
        let o = ring_offsets(Vec2::splat(10.0));
        let far = o.iter().map(|v| v.length()).fold(0.0, f32::max);
        assert!((far - 6.4).abs() < 1e-3);
        assert!(o.iter().all(|v| v.length() > 3.0));
        // (the copies' opacity builds to about the strength's share)
        let a = copy_alpha(1.9);
        let built = 1.0 - (1.0 - a).powi(7);
        assert!((built - 0.855).abs() < 1e-3);
    }
}
