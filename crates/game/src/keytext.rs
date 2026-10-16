//! The keys a HUD text names, coloured as the original's PC converter colours them
//! (`EmbedImg.ConvertButtonTags`: on PC each `[` opens its `_blueColorTag`,
//! `<font color="#B1C0D7">`, and each `]` closes it): the interaction window's actions
//! (`InteractionsWindow.SetInteractions`), the special moves' words (`SetIconMovieClip`), the
//! context line and the game messages (`InteractionsMessage.InitConvertButtonTags`), the
//! tutorial line (`SetTutorialMessage`) and window (`SetTutorialWindow`), the tutorial notes
//! (`Note.SetTutorial`: `html`). A `KeyText` holds the words; its `Text` none (its spans all),
//! each span in the text's colour or, a key, the key's, at the text's alpha.

use bevy::prelude::*;

pub struct KeyTextPlugin;

impl Plugin for KeyTextPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync.before(bevy::ui::UiSystems::Prepare));
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
        let r = runs(&kt.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_their_own_runs() {
        assert_eq!(runs("[F] Open"), vec![("[F]".to_string(), true), (" Open".to_string(), false)]);
        assert_eq!(runs("Press [J] to read\\n[LMB] Throw"), vec![("Press ".into(), false), ("[J]".into(), true), (" to read\\n".into(), false), ("[LMB]".into(), true), (" Throw".into(), false)]);
        assert_eq!(runs("no keys"), vec![("no keys".to_string(), false)]);
        assert_eq!(runs("[open"), vec![("[open".to_string(), false)]);
    }
}
