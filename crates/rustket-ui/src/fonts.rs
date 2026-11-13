//! Resolves a theme's `[fonts]` against the installed fonts.
//!
//! cosmic-text takes a face of the requested family only on an exact
//! weight match, so every UI weight is mapped to a weight the family has
//! ([`weight_fallback`]); a family that is not installed keeps the built-in
//! font. Family names become `&'static str` (iced's `Font`) by interning:
//! each distinct name is leaked once per session.
//!
//! The same exact-weight rule applies to cosmic-text's script fallback
//! (`Noto Sans CJK SC`, `Noto Sans Thai`, …): text in a weight the fallback
//! family lacks (most ship only Regular and Bold) is drawn with whatever
//! other installed face has that weight and the glyph. [`alias_missing_weights`]
//! gives every installed family the UI weights it lacks as aliases of its
//! nearest face.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use iced::Font;
use iced::advanced::graphics::text::cosmic_text::fontdb;
use iced::font::{Family, Weight};

use crate::theme::FontSet;
use crate::theme_file::{FontPrefs, ThemeWarning};

/// The UI weights: regular, medium, semibold, bold.
const UI_WEIGHTS: [u16; 4] = [400, 500, 600, 700];

/// The weight to use for `wanted` among the `available` weights of a
/// family: the exact one, else Medium → Regular and SemiBold → Bold, else
/// the nearest (the lighter one on a tie). `None` when nothing is available.
pub fn weight_fallback(available: &[u16], wanted: u16) -> Option<u16> {
    if available.contains(&wanted) {
        return Some(wanted);
    }
    let preferred = match wanted {
        500 => Some(400),
        600 => Some(700),
        _ => None,
    };
    if let Some(p) = preferred.filter(|p| available.contains(p)) {
        return Some(p);
    }
    available
        .iter()
        .copied()
        .min_by_key(|w| (w.abs_diff(wanted), *w))
}

fn weight(w: u16) -> Weight {
    match w {
        0..=149 => Weight::Thin,
        150..=249 => Weight::ExtraLight,
        250..=349 => Weight::Light,
        350..=449 => Weight::Normal,
        450..=549 => Weight::Medium,
        550..=649 => Weight::Semibold,
        650..=749 => Weight::Bold,
        750..=849 => Weight::ExtraBold,
        _ => Weight::Black,
    }
}

static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// A `'static` copy of a family name (bounded leak: one per distinct name).
fn intern(name: &str) -> &'static str {
    let mut names = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(leaked);
    leaked
}

/// Resolves `prefs` with `lookup`, which returns the canonical family name
/// and its upright weights (`None`: not installed).
pub fn resolve_with(
    prefs: &FontPrefs,
    lookup: impl Fn(&str) -> Option<(String, Vec<u16>)>,
) -> (FontSet, Vec<ThemeWarning>) {
    let mut set = FontSet::BUILTIN;
    let mut warnings = Vec::new();
    if let Some(family) = prefs.ui.as_deref() {
        match lookup(family).filter(|(_, w)| !w.is_empty()) {
            None => warnings.push(ThemeWarning::FontMissing(family.to_owned())),
            Some((name, available)) => {
                let name = intern(&name);
                let mut fonts = [Font::DEFAULT; 4];
                for (slot, wanted) in fonts.iter_mut().zip(UI_WEIGHTS) {
                    let used = weight_fallback(&available, wanted).unwrap_or(wanted);
                    if used != wanted {
                        warnings.push(ThemeWarning::FontWeight {
                            family: name.to_owned(),
                            wanted,
                            used,
                        });
                    }
                    *slot = Font {
                        family: Family::Name(name),
                        weight: weight(used),
                        ..Font::DEFAULT
                    };
                }
                let [regular, medium, semibold, bold] = fonts;
                set.regular = regular;
                set.medium = medium;
                set.semibold = semibold;
                set.bold = bold;
            }
        }
    }
    if let Some(family) = prefs.mono.as_deref() {
        match lookup(family).filter(|(_, w)| !w.is_empty()) {
            None => warnings.push(ThemeWarning::MonoFontMissing(family.to_owned())),
            Some((name, available)) => {
                let used = weight_fallback(&available, 400).unwrap_or(400);
                set.mono = Font {
                    family: Family::Name(intern(&name)),
                    weight: weight(used),
                    ..Font::DEFAULT
                };
            }
        }
    }
    (set, warnings)
}

/// Faces added by [`alias_missing_weights`] (ignored by [`resolve`]).
static ALIASES: OnceLock<HashSet<fontdb::ID>> = OnceLock::new();
/// Largest weight distance an alias bridges (Regular ↔ Bold).
const MAX_ALIAS_DISTANCE: u16 = 300;

/// The faces to add to `faces` so that every family (per style and
/// stretch) has each UI weight: a copy of the face [`weight_fallback`]
/// picks, with the wanted weight. Nothing for a weight further than
/// [`MAX_ALIAS_DISTANCE`] from any face of the family.
fn weight_aliases<'a>(faces: impl Iterator<Item = &'a fontdb::FaceInfo>) -> Vec<fontdb::FaceInfo> {
    let mut groups: HashMap<(&str, fontdb::Style, u16), Vec<&fontdb::FaceInfo>> = HashMap::new();
    for face in faces {
        let Some((family, _)) = face.families.first() else {
            continue;
        };
        groups
            .entry((family.as_str(), face.style, face.stretch.to_number()))
            .or_default()
            .push(face);
    }
    let mut out = Vec::new();
    for group in groups.values() {
        let available: Vec<u16> = group.iter().map(|f| f.weight.0).collect();
        for wanted in UI_WEIGHTS {
            if available.contains(&wanted) {
                continue;
            }
            let Some(used) = weight_fallback(&available, wanted)
                .filter(|used| used.abs_diff(wanted) <= MAX_ALIAS_DISTANCE)
            else {
                continue;
            };
            if let Some(face) = group.iter().find(|f| f.weight.0 == used) {
                out.push(fontdb::FaceInfo {
                    weight: fontdb::Weight(wanted),
                    ..(*face).clone()
                });
            }
        }
    }
    out
}

/// Gives every installed family the UI weights it lacks (see the module
/// docs), so that text in Medium, SemiBold or Bold falls back to the right
/// family for scripts the UI font does not cover. Loads the bundled fonts
/// first (they have every UI weight). Runs once; call it at startup after
/// [`resolve`].
pub fn alias_missing_weights() {
    ALIASES.get_or_init(|| {
        let lock = iced::advanced::graphics::text::font_system();
        let Ok(mut fs) = lock.write() else {
            return HashSet::new();
        };
        for f in crate::theme::fonts() {
            fs.load_font(f);
        }
        let aliases = weight_aliases(fs.raw().db().faces());
        let db = fs.raw().db_mut();
        aliases
            .into_iter()
            .map(|face| db.push_face_info(face))
            .collect()
    });
}

/// Resolves `prefs` against iced's font system (system fonts plus the
/// bundled ones, which are loaded into it first).
pub fn resolve(prefs: &FontPrefs) -> (FontSet, Vec<ThemeWarning>) {
    if prefs.ui.is_none() && prefs.mono.is_none() {
        return (FontSet::BUILTIN, Vec::new());
    }
    let lock = iced::advanced::graphics::text::font_system();
    let Ok(mut fs) = lock.write() else {
        return (FontSet::BUILTIN, Vec::new());
    };
    // iced loads them again at startup; borrowed bytes are loaded once.
    for f in crate::theme::fonts() {
        fs.load_font(f);
    }
    let db = fs.raw().db();
    resolve_with(prefs, |family| {
        let mut name = None;
        let mut weights = Vec::new();
        let aliases = ALIASES.get();
        for face in db.faces() {
            if aliases.is_some_and(|a| a.contains(&face.id)) {
                continue;
            }
            let Some((n, _)) = face
                .families
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(family))
            else {
                continue;
            };
            if face.style != iced::advanced::graphics::text::cosmic_text::fontdb::Style::Normal {
                continue;
            }
            name.get_or_insert_with(|| n.clone());
            if !weights.contains(&face.weight.0) {
                weights.push(face.weight.0);
            }
        }
        name.map(|n| (n, weights))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_fallbacks() {
        let all = [400, 500, 600, 700];
        for w in all {
            assert_eq!(weight_fallback(&all, w), Some(w));
        }
        let rb = [400, 700];
        assert_eq!(weight_fallback(&rb, 500), Some(400));
        assert_eq!(weight_fallback(&rb, 600), Some(700));
        // Neither the preferred nor the exact one: nearest, lighter on a tie.
        assert_eq!(weight_fallback(&[300, 800], 600), Some(800));
        assert_eq!(weight_fallback(&[300, 900], 600), Some(300));
        assert_eq!(weight_fallback(&[100], 700), Some(100));
        assert_eq!(weight_fallback(&[], 400), None);
    }

    #[test]
    fn missing_and_partial_families() {
        let lookup = |f: &str| match f {
            "Two Weights" => Some(("Two Weights".to_owned(), vec![400, 700])),
            "code mono" => Some(("Code Mono".to_owned(), vec![400])),
            _ => None,
        };
        let (set, w) = resolve_with(
            &FontPrefs {
                ui: Some("Nope Sans".into()),
                mono: Some("code mono".into()),
            },
            lookup,
        );
        assert_eq!(set.regular, FontSet::BUILTIN.regular);
        assert_eq!(set.mono.family, Family::Name("Code Mono"));
        assert_eq!(w, [ThemeWarning::FontMissing("Nope Sans".into())]);

        let (set, w) = resolve_with(
            &FontPrefs {
                ui: None,
                mono: Some("Nope Mono".into()),
            },
            lookup,
        );
        assert_eq!(set, FontSet::BUILTIN);
        assert_eq!(w, [ThemeWarning::MonoFontMissing("Nope Mono".into())]);

        let (set, w) = resolve_with(
            &FontPrefs {
                ui: Some("Two Weights".into()),
                mono: None,
            },
            lookup,
        );
        assert_eq!(set.medium.weight, Weight::Normal);
        assert_eq!(set.semibold.weight, Weight::Bold);
        assert_eq!(set.bold.weight, Weight::Bold);
        assert_eq!(set.mono, FontSet::BUILTIN.mono);
        assert_eq!(w.len(), 2, "{w:?}");
    }

    fn face(family: &str, weight: u16, index: u32) -> fontdb::FaceInfo {
        fontdb::FaceInfo {
            id: fontdb::ID::dummy(),
            source: fontdb::Source::Binary(std::sync::Arc::new(Vec::<u8>::new())),
            index,
            families: vec![(family.to_owned(), fontdb::Language::English_UnitedStates)],
            post_script_name: format!("{family}-{weight}"),
            style: fontdb::Style::Normal,
            weight: fontdb::Weight(weight),
            stretch: fontdb::Stretch::Normal,
            monospaced: false,
        }
    }

    #[test]
    fn missing_weights_become_aliases() {
        let faces = [
            face("CJK", 400, 0),
            face("CJK", 700, 1),
            face("Full", 400, 2),
            face("Full", 500, 3),
            face("Full", 600, 4),
            face("Full", 700, 5),
            face("Black Only", 900, 6),
        ];
        let mut got: Vec<(String, u16, u32)> = weight_aliases(faces.iter())
            .into_iter()
            .map(|f| (f.families[0].0.clone(), f.weight.0, f.index))
            .collect();
        got.sort();
        // Medium from Regular, SemiBold from Bold; nothing for a family
        // that has every weight or is too far from them.
        assert_eq!(
            got,
            [
                ("Black Only".to_owned(), 600, 6),
                ("Black Only".to_owned(), 700, 6),
                ("CJK".to_owned(), 500, 0),
                ("CJK".to_owned(), 600, 1),
            ]
        );
    }

    /// The bundled family resolves to exactly the built-in fonts.
    #[test]
    fn bundled_family_resolves_without_warnings() {
        let (set, w) = resolve(&FontPrefs {
            ui: Some("Noto Sans".into()),
            mono: None,
        });
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(set, FontSet::BUILTIN);
    }
}
