//! The theme picker (Appearance) and the language picker (General): a pick
//! list with "Open … folder", "Import…" and "Reload", and below it what is
//! wrong with the selected file.

use iced::widget::{Column, column, pick_list, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::config::{Language, UiPrefs};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{BannerKind, banner, secondary_button};

use super::Message;
use super::widgets::{Choice, field};

/// Most theme or locale warnings listed before "and N more".
const WARNINGS_SHOWN: usize = 5;

/// A pick list with "Open folder", "Import…" and "Reload" (all off without a
/// folder).
fn controls<'a, T: Clone + PartialEq + std::fmt::Display + 'a>(
    choices: Vec<T>,
    current: T,
    on_select: impl Fn(T) -> Message + 'a,
    folder_label: String,
    on_folder: Option<Message>,
    on_import: Option<Message>,
    on_reload: Option<Message>,
) -> Element<'a, Message> {
    row![
        pick_list(choices, Some(current), on_select)
            .text_size(TEXT_BODY)
            .width(Length::Fill)
            .style(style::pick_list)
            .menu_style(style::menu),
        secondary_button(folder_label, on_folder),
        secondary_button(t!("settings.import_files"), on_import),
        secondary_button(t!("settings.themes_reload"), on_reload),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// A warning banner: `title`, then the first lines as bullets and "and N more".
fn warnings_banner<'a>(title: String, lines: &[String]) -> Element<'a, Message> {
    let more = lines.len().saturating_sub(WARNINGS_SHOWN);
    let mut msg = title;
    for l in lines.iter().take(WARNINGS_SHOWN) {
        msg.push_str("\n• ");
        msg.push_str(l);
    }
    if more > 0 {
        msg.push('\n');
        msg.push_str(&t!("settings.theme_more", n = more));
    }
    banner(BannerKind::Warning, msg, None)
}

/// The external theme picker with "Open themes folder" and "Reload", and
/// below it what is wrong with the selected theme.
pub(super) fn theme_picker<'a>(
    ui: &UiPrefs,
    themes: &crate::themes::Themes,
) -> Element<'a, Message> {
    let builtin = Choice {
        value: None,
        label: t!("settings.theme_builtin").into_owned(),
    };
    let mut choices = vec![builtin.clone()];
    let names: Vec<String> = themes.entries.iter().map(|e| e.display_name()).collect();
    choices.extend(themes.entries.iter().zip(&names).map(|(e, name)| {
        let label = if names.iter().filter(|n| *n == name).count() > 1 {
            format!("{name} ({}.toml)", e.id)
        } else {
            name.clone()
        };
        Choice {
            value: Some(e.id.clone()),
            label,
        }
    }));
    let selected = ui.theme_name.as_deref();
    let current = match selected {
        None => builtin,
        Some(id) => choices
            .iter()
            .find(|c| c.value.as_deref() == Some(id))
            .cloned()
            .unwrap_or_else(|| Choice {
                value: Some(id.to_owned()),
                label: id.to_owned(),
            }),
    };
    let mut col: Column<'a, Message> = column![controls(
        choices,
        current,
        |c: Choice<Option<String>>| Message::SetThemeName(c.value),
        t!("settings.themes_folder").into_owned(),
        themes.dir.is_some().then_some(Message::OpenThemesFolder),
        themes
            .dir
            .is_some()
            .then_some(Message::Import(crate::import::Kind::Themes)),
        themes.dir.is_some().then_some(Message::ReloadThemes),
    )]
    .spacing(SPACE_2);
    let caption = |s: String| text(s).size(TEXT_CAPTION).style(style::text_secondary);
    match themes.resolve(selected) {
        None => {}
        Some(Err(e)) => {
            let file = format!("{}.toml", selected.unwrap_or_default());
            let msg = crate::themes::load_error_text(&e, &file);
            col = col.push(banner(BannerKind::Error, msg, None));
        }
        Some(Ok(file)) => {
            if !file.has(rustket_ui::Mode::Light) {
                col = col.push(caption(t!("settings.theme_dark_only").into_owned()));
            } else if !file.has(rustket_ui::Mode::Dark) {
                col = col.push(caption(t!("settings.theme_light_only").into_owned()));
            }
            let entry = selected.and_then(|id| themes.get(id));
            let mut lines: Vec<String> = file.warnings.iter().map(warning_text).collect();
            if let Some(e) = entry {
                lines.extend(e.font_warnings.iter().map(warning_text));
                for (mode, issues) in [
                    (t!("settings.theme_light"), &e.light_issues),
                    (t!("settings.theme_dark"), &e.dark_issues),
                ] {
                    lines.extend(issues.iter().map(|i| {
                        t!(
                            "settings.theme_low_contrast",
                            mode = mode,
                            what = i.what,
                            ratio = format!("{:.2}", i.ratio),
                            min = i.min
                        )
                        .into_owned()
                    }));
                }
            }
            if !lines.is_empty() {
                col = col.push(warnings_banner(
                    t!("settings.theme_warnings").into_owned(),
                    &lines,
                ));
            }
        }
    }
    if themes.fonts_need_restart(selected) {
        col = col.push(caption(t!("settings.theme_fonts_restart").into_owned()));
    }
    field(
        t!("settings.theme").into_owned(),
        Some(t!("settings.theme_help").into_owned()),
        col.into(),
    )
}

/// The language picker (System, bundled, external locales) with "Open
/// languages folder" and "Reload", and below it what is wrong with the
/// selected (or system) locale's file.
pub(super) fn language_picker<'a>(
    ui: &UiPrefs,
    locales: &crate::locales::Locales,
) -> Element<'a, Message> {
    use crate::i18n;
    let with_coverage = |code: &str, name: String| match locales.coverage(code) {
        Some(c) if !c.complete() => format!("{name} ({}%)", c.percent()),
        _ => name,
    };
    let choice = |code: &str, label: String| Choice {
        value: Language::new(code).unwrap_or_default(),
        label,
    };
    let system_code = i18n::system();
    let system_name = i18n::native_name(&system_code).unwrap_or_else(|| system_code.clone());
    let system = choice(
        Language::SYSTEM,
        format!("{} ({system_name})", t!("settings.language_system")),
    );
    let mut choices = vec![system.clone()];
    choices.extend(
        i18n::BUNDLED
            .iter()
            .map(|b| choice(b.code, with_coverage(b.code, b.name.to_owned()))),
    );
    let mut external: Vec<(String, &str)> = locales
        .entries
        .iter()
        .filter(|e| i18n::bundled(&e.code).is_none())
        .map(|e| (e.display_name(), e.code.as_str()))
        .collect();
    external.sort_by_cached_key(|(name, code)| (name.to_lowercase(), (*code).to_owned()));
    // Same-named entries are told apart by their file name.
    let clashes = |name: &str| {
        external.iter().filter(|(n, _)| n == name).count() > 1
            || i18n::BUNDLED
                .iter()
                .any(|b| b.name.to_lowercase() == name.to_lowercase())
    };
    choices.extend(external.iter().map(|(name, code)| {
        let name = if clashes(name) {
            format!("{name} ({code}.toml)")
        } else {
            name.clone()
        };
        choice(code, with_coverage(code, name))
    }));
    let selected = ui.language.as_str();
    let current = choices
        .iter()
        .find(|c| c.value.as_str() == selected)
        .cloned()
        .unwrap_or_else(|| Choice {
            value: ui.language.clone(),
            label: selected.to_owned(),
        });
    let mut col: Column<'a, Message> = column![controls(
        choices,
        current,
        |c: Choice<Language>| Message::SetLanguage(c.value),
        t!("settings.locales_folder").into_owned(),
        locales.dir.is_some().then_some(Message::OpenLocalesFolder),
        locales
            .dir
            .is_some()
            .then_some(Message::Import(crate::import::Kind::Locales)),
        locales.dir.is_some().then_some(Message::ReloadLocales),
    )]
    .spacing(SPACE_2);

    let unusable = !ui.language.is_system() && !i18n::is_available(selected);
    let code = if ui.language.is_system() {
        system_code.as_str()
    } else {
        selected
    };
    let file = format!("{code}.toml");
    let error = match locales.get(code).map(|e| &e.file) {
        None if unusable => Some(t!("settings.locale_not_found", file = file).into_owned()),
        None | Some(Ok(_)) => None,
        Some(Err(e)) => Some(crate::locales::load_error_text(e, &file)),
    };
    if let Some(msg) = error {
        let fallback = if unusable {
            t!("settings.locale_fallback_system", name = system_name)
        } else {
            t!("settings.locale_fallback_builtin")
        };
        col = col.push(banner(
            BannerKind::Error,
            format!("{msg}\n{fallback}"),
            None,
        ));
    } else if let Some(Ok(f)) = locales.get(code).map(|e| &e.file)
        && !f.warnings.is_empty()
    {
        let lines: Vec<String> = f
            .warnings
            .iter()
            .map(|w| crate::locales::warning_text(w, code))
            .collect();
        col = col.push(warnings_banner(
            t!("settings.locale_warnings", file = file).into_owned(),
            &lines,
        ));
    }
    field(
        t!("settings.language_ui").into_owned(),
        Some(t!("settings.language_help").into_owned()),
        col.into(),
    )
}

/// A theme file warning in the UI language (key paths stay as written).
fn warning_text(w: &rustket_ui::ThemeWarning) -> String {
    use rustket_ui::ThemeWarning as W;
    match w {
        W::UnknownKey(k) => t!("settings.theme_w_unknown", key = k),
        W::BadValue { key, value } => t!("settings.theme_w_bad", key = key, value = value),
        W::FlagLength { key, len } => t!("settings.theme_w_flags", key = key, n = len),
        W::NewerVersion(v) => t!("settings.theme_w_version", version = v),
        W::RadiusClamped { value, used } => {
            t!("settings.theme_w_radius", value = value, used = used)
        }
        W::FontMissing(family) => t!("settings.theme_w_font", family = family),
        W::MonoFontMissing(family) => t!("settings.theme_w_mono_font", family = family),
        W::FontWeight {
            family,
            wanted,
            used,
        } => t!(
            "settings.theme_w_weight",
            family = family,
            wanted = wanted,
            used = used
        ),
    }
    .into_owned()
}
