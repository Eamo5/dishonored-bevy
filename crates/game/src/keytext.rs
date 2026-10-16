//! The keys a HUD text names, coloured as the original's PC converter colours them
//! (`EmbedImg.ConvertButtonTags`: on PC each `[` opens its `_blueColorTag`,
//! `<font color="#B1C0D7">`, and each `]` closes it): the interaction window's actions
//! (`InteractionsWindow.SetInteractions`), the special moves' words (`SetIconMovieClip`), the
//! context line and the game messages (`InteractionsMessage.InitConvertButtonTags`), the
//! tutorial line (`SetTutorialMessage`) and window (`SetTutorialWindow`), the tutorial notes
//! (`Note.SetTutorial`: `html`). A `KeyText` holds the words; its `Text` none (its spans all),
//! each span in the text's colour or, a key, the key's, at the text's alpha. A pad in use, a
//! key that is a pad's button (`pad::MARK`) is its icon as the converter's image substitutions
//! put it (`setImageSubstitutions`: the library's `X360_*` bitmap, at most 35 square, in the
//! line): the words leave it a gap, and the icon is laid over the gap where the text's layout
//! put it.

use bevy::prelude::*;

pub struct KeyTextPlugin;

impl Plugin for KeyTextPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, (sync.before(bevy::ui::UiSystems::Prepare), place_icons.after(bevy::ui::UiSystems::Layout)));
    }
}

/// The PC converter's key colour (`#B1C0D7`).
pub const KEY: Color = Color::srgb(0xB1 as f32 / 255.0, 0xC0 as f32 / 255.0, 0xD7 as f32 / 255.0);

/// A text whose bracketed keys show in the key colour: the words it shows.
#[derive(Component, Default, Clone, PartialEq)]
pub struct KeyText(pub String);

/// The words in runs, each a key (from `[` to `]`, both in it) or not.
pub fn runs(s: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut cur = String::new();
    let mut key = false;
    for c in s.chars() {
        match c {
            '[' if !key => {
                if !cur.is_empty() {
                    out.push((std::mem::take(&mut cur), false));
                }
                key = true;
                cur.push(c);
            }
            ']' if key => {
                cur.push(c);
                out.push((std::mem::take(&mut cur), true));
                key = false;
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push((cur, false));
    }
    out
}

/// A run that is a pad's button: its name.
fn pad_button(run: &str) -> Option<&str> {
    run.strip_prefix('[')?.strip_suffix(']')?.strip_prefix(crate::pad::MARK)
}

/// The gap a pad button's icon is laid over (an em and a half: the icon's 35 against the
/// fields' 22-25 point words).
const GAP: &str = "\u{2003}\u{2002}";

/// The words as a Flash field's HTML with its keys in the key colour (`_blueColorTag`): for a
/// movie's own field (the tutorial notes' `Note.SetTutorial`).
pub fn html(s: &str) -> String {
    runs(s).into_iter().map(|(t, key)| if key { format!("<font color=\"#B1C0D7\">{t}</font>") } else { t }).collect()
}

#[allow(clippy::type_complexity)]
fn sync(
    mut commands: Commands,
    roots: Query<(Entity, &KeyText, &TextFont, &TextColor, Option<&Children>), Or<(Changed<KeyText>, Changed<TextFont>, Changed<TextColor>)>>,
    mut spans: Query<(&mut TextSpan, &mut TextFont, &mut TextColor), Without<KeyText>>,
) {
    for (e, kt, font, color, children) in &roots {
        // (a pad's button: a gap for its icon)
        let r: Vec<(String, bool)> = runs(&kt.0).into_iter().map(|(t, key)| if key && pad_button(&t).is_some() { (GAP.to_string(), true) } else { (t, key) }).collect();
        let kids: Vec<Entity> = children.into_iter().flatten().copied().filter(|c| spans.contains(*c)).collect();
        let tint = |key: bool| if key { KEY.with_alpha(color.0.alpha()) } else { color.0 };
        if kids.len() == r.len() {
            for (k, (t, key)) in kids.iter().zip(&r) {
                let Ok((mut s, mut f, mut c)) = spans.get_mut(*k) else { continue };
                if s.0 != *t {
                    s.0.clone_from(t);
                }
                if *f != *font {
                    *f = font.clone();
                }
                let want = tint(*key);
                if c.0 != want {
                    c.0 = want;
                }
            }
            continue;
        }
        for k in kids {
            commands.entity(k).despawn();
        }
        for (t, key) in r {
            commands.spawn((TextSpan::new(t), font.clone(), TextColor(tint(key)), ChildOf(e)));
        }
    }
}

/// The pad buttons' icons laid over a key text's gaps (siblings of the text, placed from its
/// layout's runs: the gap's run, centred in it, the line's height).
#[derive(Component)]
struct KeyIcons(Vec<Entity>);

/// A pad button's icon, and the key text it belongs to.
#[derive(Component)]
struct KeyIcon(Entity);

#[allow(clippy::type_complexity)]
fn place_icons(
    mut commands: Commands,
    texts: Query<(Entity, &KeyText, &Node, &TextFont, &TextColor, &Visibility, &bevy::text::TextLayoutInfo, &bevy::text::ComputedTextBlock, &ComputedNode, Option<&Children>, Option<&ChildOf>, Option<&KeyIcons>)>,
    spans: Query<(), With<TextSpan>>,
    mut icons: Query<(Entity, &KeyIcon, &mut Node, &mut ImageNode, &mut Visibility), Without<KeyText>>,
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
) {
    for (e, kt, node, font, color, vis, layout, block, computed, children, parent, made) in &texts {
        let buttons: Vec<String> = runs(&kt.0).iter().filter_map(|(t, key)| key.then(|| pad_button(t)).flatten().map(str::to_string)).collect();
        if buttons.is_empty() {
            if let Some(m) = made {
                for &i in &m.0 {
                    commands.entity(i).try_despawn();
                }
                commands.entity(e).remove::<KeyIcons>();
            }
            continue;
        }
        let Some(parent) = parent else { continue };
        // the gaps' spans, in order, and where the layout put each (its run's bounds)
        let gaps: Vec<Entity> = {
            let kids: Vec<Entity> = children.into_iter().flatten().copied().filter(|c| spans.contains(*c)).collect();
            let r = runs(&kt.0);
            kids.into_iter().zip(r).filter(|(_, (t, key))| *key && pad_button(t).is_some()).map(|(k, _)| k).collect()
        };
        let inv = computed.inverse_scale_factor();
        let rect_of = |span: Entity| -> Option<Rect> {
            let section = block.entities().iter().position(|t| t.entity == span)?;
            layout.run_geometry.iter().filter(|g| g.section_index == section).map(|g| g.bounds).reduce(|a, b| a.union(b))
        };
        let px = |v: Val| if let Val::Px(p) = v { p } else { 0.0 };
        let (left, top) = (px(node.left), px(node.top));
        // (the icon at most 35 against the field's 25: 1.4 of the words' size)
        let size = match font.font_size {
            bevy::text::FontSize::Px(p) => p,
            _ => 24.0,
        } * 1.4;
        let mut list = made.map(|m| m.0.clone()).unwrap_or_default();
        for (k, name) in buttons.iter().enumerate() {
            let icon = match list.get(k) {
                Some(i) => *i,
                None => {
                    let i = commands.spawn((KeyIcon(e), ImageNode::default(), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(parent.parent()))).id();
                    list.push(i);
                    commands.entity(e).insert(KeyIcons(list.clone()));
                    continue;
                }
            };
            let Ok((_, _, mut n, mut img, mut v)) = icons.get_mut(icon) else { continue };
            let (Some(rect), Some((h, isz))) = (gaps.get(k).and_then(|g| rect_of(*g)), crate::pad::icon_id(name).and_then(|id| ui.get(&mut images, "lib", id))) else {
                *v = Visibility::Hidden;
                continue;
            };
            // (at most `size` square, its shape kept)
            let s = (size / isz.x.max(1.0)).min(size / isz.y.max(1.0));
            let (w, hgt) = (isz.x * s, isz.y * s);
            let c = rect.center() * inv;
            n.left = Val::Px(left + c.x - w * 0.5);
            n.top = Val::Px(top + c.y - hgt * 0.5);
            n.width = Val::Px(w);
            n.height = Val::Px(hgt);
            if img.image != h {
                img.image = h;
            }
            img.color = Color::WHITE.with_alpha(color.0.alpha());
            if *v != *vis {
                *v = *vis;
            }
        }
        // (fewer buttons than before: the rest hidden)
        for &i in list.iter().skip(buttons.len()) {
            if let Ok((_, _, _, _, mut v)) = icons.get_mut(i) {
                *v = Visibility::Hidden;
            }
        }
    }
    // (the icons of a text gone, gone)
    for (i, owner, ..) in &icons {
        if !texts.contains(owner.0) {
            commands.entity(i).try_despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_their_own_runs() {
        assert_eq!(runs("[F] Open"), vec![("[F]".to_string(), true), (" Open".to_string(), false)]);
        assert_eq!(runs("Press [J] to read\\n[LMB] Throw"), vec![("Press ".into(), false), ("[J]".into(), true), (" to read\\n".into(), false), ("[LMB]".into(), true), (" Throw".into(), false)]);
        assert_eq!(runs("no keys"), vec![("no keys".to_string(), false)]);
        assert_eq!(runs("[open"), vec![("[open".to_string(), false)]);
        // (a pad's button)
        let b = format!("[{}X] Open", crate::pad::MARK);
        assert_eq!(runs(&b)[0].1, true);
        assert_eq!(pad_button(&runs(&b)[0].0), Some("X"));
    }
}
