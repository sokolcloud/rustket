//! The update card in the corner, the Settings "Updates" data and the
//! translated error texts. Strings: `locales/updater.yml` (`updater.*`).

use iced::widget::{button, column, container, progress_bar, row, space, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION,
};
use rustket_ui::widget::{
    ButtonSize, focus_ring, icon_button_with, primary_button, secondary_button,
};

use super::UpdateError;
use super::app::Msg;
use super::detect::{InnoScope, InstallKind};
use super::state::{Phase, Updater};
use crate::app::App;
use crate::message::Message;

fn m(msg: Msg) -> Message {
    Message::Updater(msg)
}

/// The user-facing text of an updater error.
pub fn error_text(e: &UpdateError) -> String {
    match e {
        UpdateError::Network(_) => t!("updater.error_network").into_owned(),
        UpdateError::RateLimited { .. } => t!("updater.error_rate_limited").into_owned(),
        UpdateError::Http(code) => t!("updater.error_http", code = code).into_owned(),
        UpdateError::BadRelease(_) => t!("updater.error_bad_release").into_owned(),
        UpdateError::NoAsset => t!("updater.error_no_asset").into_owned(),
        UpdateError::NoChecksum(_) => t!("updater.error_no_checksum").into_owned(),
        UpdateError::ChecksumMismatch => t!("updater.error_checksum").into_owned(),
        UpdateError::SizeMismatch => t!("updater.error_size").into_owned(),
        UpdateError::Io { kind, .. } => match kind {
            std::io::ErrorKind::PermissionDenied => t!("updater.error_permission").into_owned(),
            std::io::ErrorKind::StorageFull => t!("updater.error_disk").into_owned(),
            _ => t!("updater.error_io").into_owned(),
        },
        UpdateError::AuthCancelled => t!("updater.auth_cancelled").into_owned(),
        UpdateError::NoAuthAgent => t!("updater.error_auth_agent").into_owned(),
        UpdateError::Tool { tool, code } => {
            let code = code.map_or_else(|| String::from("?"), |c| c.to_string());
            t!("updater.error_tool", tool = tool, code = code).into_owned()
        }
        UpdateError::NotWritable(path) => t!(
            "updater.error_not_writable",
            path = path.display().to_string()
        )
        .into_owned(),
        UpdateError::Translocated => t!("updater.error_translocated").into_owned(),
        UpdateError::Unsupported => t!("updater.error_unsupported").into_owned(),
        UpdateError::Cancelled => t!("updater.error_cancelled").into_owned(),
    }
}

/// How this copy is updated (Settings).
pub fn kind_label(kind: Option<&InstallKind>) -> String {
    match kind {
        None => t!("updater.kind_detecting").into_owned(),
        Some(InstallKind::Deb { .. }) => t!("updater.kind_deb").into_owned(),
        Some(InstallKind::Rpm { .. }) => t!("updater.kind_rpm").into_owned(),
        Some(InstallKind::AppImage { .. }) => t!("updater.kind_appimage").into_owned(),
        Some(InstallKind::InnoSetup {
            scope: InnoScope::CurrentUser,
            ..
        }) => t!("updater.kind_inno_user").into_owned(),
        Some(InstallKind::InnoSetup {
            scope: InnoScope::AllUsers,
            ..
        }) => t!("updater.kind_inno_all").into_owned(),
        Some(InstallKind::PortableZip { .. }) => t!("updater.kind_zip").into_owned(),
        Some(InstallKind::MacApp { .. }) => t!("updater.kind_mac").into_owned(),
        Some(InstallKind::Unmanaged) => t!("updater.kind_unmanaged").into_owned(),
    }
}

/// Installing asks for an administrator password.
fn needs_admin(kind: Option<&InstallKind>) -> bool {
    matches!(
        kind,
        Some(
            InstallKind::Deb { .. }
                | InstallKind::Rpm { .. }
                | InstallKind::InnoSetup {
                    scope: InnoScope::AllUsers,
                    ..
                }
        )
    )
}

fn install_label(u: &Updater) -> String {
    if u.can_install() {
        t!("updater.install").into_owned()
    } else {
        t!("updater.download").into_owned()
    }
}

/// Decimal megabytes with one decimal ("12.3").
pub fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

fn downloading_text(received: u64, total: u64) -> String {
    t!(
        "updater.downloading",
        done = mb(received),
        total = mb(total)
    )
    .into_owned()
}

/// A failed install's fallback for the downloaded file: packages and
/// installers open with the system handler, anything else is shown in its
/// folder.
fn file_action(path: &std::path::Path) -> (String, Msg) {
    let package = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["deb", "rpm", "exe", "dmg"].contains(&e.to_ascii_lowercase().as_str()));
    if package {
        (
            t!("updater.open_package").into_owned(),
            Msg::OpenFile(path.to_owned()),
        )
    } else {
        (
            t!("updater.show_file").into_owned(),
            Msg::RevealFile(path.to_owned()),
        )
    }
}

fn caption<'a>(s: String) -> Element<'a, Message> {
    text(s)
        .size(TEXT_CAPTION)
        .style(style::text_secondary)
        .into()
}

fn small_link<'a>(label: String, msg: Msg) -> Element<'a, Message> {
    let msg = m(msg);
    focus_ring(
        button(text(label).size(TEXT_CAPTION).style(style::text_link))
            .padding(0)
            .style(style::button_link)
            .on_press(msg.clone()),
        Some(msg),
    )
    .into()
}

/// The corner card (stacked with call popups above everything), when the
/// phase has something to show.
pub fn card(app: &App) -> Option<Element<'_, Message>> {
    let u = &app.updater;
    if !u.card_visible(app.ui_state.update_skipped.as_deref()) {
        return None;
    }
    let kind = u.kind.as_ref();
    let current = env!("CARGO_PKG_VERSION");
    let mut body: Vec<Element<'_, Message>> = Vec::new();
    let mut links: Vec<Element<'_, Message>> = Vec::new();
    let mut buttons: Vec<Element<'_, Message>> = Vec::new();
    let mut close = Some(Msg::Later);
    let title = match u.shown() {
        Phase::Available { release, .. } => {
            body.push(caption(
                t!("updater.available_body", current = current).into_owned(),
            ));
            if u.can_install() && needs_admin(kind) {
                body.push(caption(t!("updater.admin_hint").into_owned()));
            }
            links.push(small_link(
                t!("updater.release_notes").into_owned(),
                Msg::OpenReleasePage,
            ));
            links.push(small_link(t!("updater.skip").into_owned(), Msg::Skip));
            buttons.push(
                secondary_button(t!("updater.later").into_owned(), Some(m(Msg::Later)))
                    .size(ButtonSize::Small)
                    .into(),
            );
            buttons.push(
                primary_button(install_label(u), Some(m(Msg::Install)))
                    .icon(if u.can_install() {
                        Icon::Download
                    } else {
                        Icon::ExternalLink
                    })
                    .size(ButtonSize::Small)
                    .into(),
            );
            t!(
                "updater.available_title",
                version = release.version.to_string()
            )
            .into_owned()
        }
        Phase::Downloading {
            release,
            received,
            total,
        } => {
            close = None;
            let pct = if *total == 0 {
                0.0
            } else {
                (*received as f32 / *total as f32).clamp(0.0, 1.0)
            };
            body.push(caption(downloading_text(*received, *total)));
            body.push(progress_bar(0.0..=1.0, pct).girth(6.0).into());
            buttons.push(
                secondary_button(
                    t!("updater.cancel").into_owned(),
                    Some(m(Msg::CancelDownload)),
                )
                .size(ButtonSize::Small)
                .into(),
            );
            t!(
                "updater.available_title",
                version = release.version.to_string()
            )
            .into_owned()
        }
        Phase::Ready { release, .. } => {
            body.push(caption(t!("updater.ready").into_owned()));
            buttons.push(
                secondary_button(t!("updater.later").into_owned(), Some(m(Msg::Later)))
                    .size(ButtonSize::Small)
                    .into(),
            );
            buttons.push(
                primary_button(
                    t!("updater.install").into_owned(),
                    Some(m(Msg::InstallReady)),
                )
                .icon(Icon::Download)
                .size(ButtonSize::Small)
                .into(),
            );
            t!(
                "updater.available_title",
                version = release.version.to_string()
            )
            .into_owned()
        }
        Phase::Installing { release, .. } => {
            close = None;
            body.push(caption(t!("updater.installing").into_owned()));
            if needs_admin(kind) {
                body.push(caption(t!("updater.admin_hint").into_owned()));
            }
            buttons.push(
                primary_button(t!("updater.install").into_owned(), None)
                    .loading(true)
                    .size(ButtonSize::Small)
                    .into(),
            );
            t!(
                "updater.available_title",
                version = release.version.to_string()
            )
            .into_owned()
        }
        Phase::Failed {
            version,
            error,
            file,
            ..
        } => {
            body.push(caption(error_text(error)));
            if let Some(f) = file {
                let (label, msg) = file_action(f);
                links.push(small_link(label, msg));
            }
            links.push(small_link(
                t!("updater.release_page").into_owned(),
                Msg::OpenReleasePage,
            ));
            buttons.push(
                secondary_button(t!("updater.dismiss").into_owned(), Some(m(Msg::Later)))
                    .size(ButtonSize::Small)
                    .into(),
            );
            t!("updater.failed_title", version = version.to_string()).into_owned()
        }
        Phase::InstalledNoRestart { release } => {
            body.push(caption(t!("updater.installed_body").into_owned()));
            buttons.push(
                secondary_button(t!("updater.dismiss").into_owned(), Some(m(Msg::Later)))
                    .size(ButtonSize::Small)
                    .into(),
            );
            t!(
                "updater.installed_title",
                version = release.version.to_string()
            )
            .into_owned()
        }
        Phase::Idle | Phase::Checking { .. } | Phase::UpToDate | Phase::CheckFailed(_) => {
            return None;
        }
    };
    let mut head = row![
        text(title)
            .size(TEXT_BODY)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill)
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if let Some(c) = close {
        head = head.push(icon_button_with(
            Icon::X,
            Some(m(c)),
            t!("updater.later").into_owned(),
            false,
            24.0,
        ));
    }
    let mut col = column![head].spacing(SPACE_3);
    col = col.push(column(body).spacing(SPACE_1));
    if !links.is_empty() {
        col = col.push(row(links).spacing(SPACE_4).align_y(Alignment::Center));
    }
    col = col.push(
        row![space().width(Length::Fill)]
            .extend(buttons)
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
    );
    Some(
        container(col)
            .padding(SPACE_4)
            .width(Length::Fill)
            .style(style::container_popover)
            .into(),
    )
}

/// What the Settings "Updates" card shows (texts already translated).
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsData {
    /// How updates are installed.
    pub method: String,
    pub status: String,
    /// "Last checked …" / "Never checked".
    pub last_checked: String,
    /// A check, download or install runs ("Check now" is disabled).
    pub busy: bool,
    /// Label of the install button, when a release can be installed.
    pub install: Option<String>,
}

/// Local date and time of a unix timestamp.
fn local_time(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn settings_data(u: &Updater, last_check: Option<i64>, skipped: Option<&str>) -> SettingsData {
    let available = |v: &semver::Version| {
        let s = t!("updater.status_available", version = v.to_string()).into_owned();
        if skipped == Some(v.to_string().as_str()) {
            format!("{s} {}", t!("updater.skipped"))
        } else {
            s
        }
    };
    let status = match &u.phase {
        Phase::Idle => String::new(),
        Phase::Checking { .. } => t!("updater.checking").into_owned(),
        Phase::UpToDate => t!("updater.up_to_date").into_owned(),
        Phase::CheckFailed(e) => error_text(e),
        Phase::Available { release, .. } => available(&release.version),
        Phase::Downloading {
            received, total, ..
        } => downloading_text(*received, *total),
        Phase::Ready { .. } => t!("updater.ready").into_owned(),
        Phase::Installing { .. } => t!("updater.installing").into_owned(),
        Phase::Failed { version, error, .. } => format!(
            "{} {}",
            t!("updater.failed_title", version = version.to_string()),
            error_text(error)
        ),
        Phase::InstalledNoRestart { .. } => t!("updater.installed_body").into_owned(),
    };
    let last_checked = match last_check {
        Some(s) => t!("updater.last_checked", time = local_time(s)).into_owned(),
        None => t!("updater.never_checked").into_owned(),
    };
    SettingsData {
        method: kind_label(u.kind.as_ref()),
        status,
        last_checked,
        busy: u.busy(),
        install: u.installable().map(|_| install_label(u)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::updater::github::Release;

    fn release(v: &str) -> Arc<Release> {
        Arc::new(Release {
            version: semver::Version::parse(v).unwrap(),
            tag: format!("v{v}"),
            html_url: String::new(),
            notes: String::new(),
            prerelease: false,
            assets: Vec::new(),
            sums: None,
        })
    }

    #[test]
    fn error_texts_are_translated() {
        let all = [
            UpdateError::Network("x".into()),
            UpdateError::RateLimited { reset: None },
            UpdateError::Http(502),
            UpdateError::BadRelease("x".into()),
            UpdateError::NoAsset,
            UpdateError::NoChecksum("x".into()),
            UpdateError::ChecksumMismatch,
            UpdateError::SizeMismatch,
            UpdateError::Io {
                op: "x",
                kind: std::io::ErrorKind::PermissionDenied,
            },
            UpdateError::Io {
                op: "x",
                kind: std::io::ErrorKind::StorageFull,
            },
            UpdateError::Io {
                op: "x",
                kind: std::io::ErrorKind::Other,
            },
            UpdateError::AuthCancelled,
            UpdateError::NoAuthAgent,
            UpdateError::Tool {
                tool: "dnf".into(),
                code: Some(1),
            },
            UpdateError::NotWritable(PathBuf::from("/opt/x")),
            UpdateError::Translocated,
            UpdateError::Unsupported,
            UpdateError::Cancelled,
        ];
        let texts: Vec<String> = all.iter().map(error_text).collect();
        for (e, t) in all.iter().zip(&texts) {
            assert!(!t.is_empty() && !t.contains("updater."), "{e:?}: {t}");
            assert!(!t.contains("%{"), "{e:?}: {t}");
        }
        assert!(texts[2].contains("502"));
        assert!(texts[13].contains("dnf") && texts[13].contains('1'));
        assert!(texts[14].contains("/opt/x"));
        let none = error_text(&UpdateError::Tool {
            tool: "swap (mv)".into(),
            code: None,
        });
        assert!(none.contains("swap (mv)") && none.contains('?'));
    }

    #[test]
    fn kind_labels_and_admin_hint() {
        let kinds = [
            InstallKind::Deb {
                exe: "/usr/bin/rustket".into(),
            },
            InstallKind::Rpm {
                exe: "/usr/bin/rustket".into(),
            },
            InstallKind::AppImage { image: "/a".into() },
            InstallKind::InnoSetup {
                scope: InnoScope::CurrentUser,
                dir: "c".into(),
            },
            InstallKind::InnoSetup {
                scope: InnoScope::AllUsers,
                dir: "c".into(),
            },
            InstallKind::PortableZip { dir: "c".into() },
            InstallKind::MacApp {
                bundle: "/b".into(),
            },
            InstallKind::Unmanaged,
        ];
        let mut labels: Vec<String> = kinds.iter().map(|k| kind_label(Some(k))).collect();
        labels.push(kind_label(None));
        for l in &labels {
            assert!(!l.is_empty() && !l.starts_with("updater."), "{l}");
        }
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), kinds.len() + 1, "labels differ");
        let admin: Vec<bool> = kinds.iter().map(|k| needs_admin(Some(k))).collect();
        assert_eq!(admin, [true, true, false, false, true, false, false, false]);
    }

    #[test]
    fn sizes_and_file_actions() {
        assert_eq!(mb(0), "0.0");
        assert_eq!(mb(12_345_678), "12.3");
        assert!(matches!(
            file_action(std::path::Path::new("/c/rustket_0.3.0-1_amd64.deb")).1,
            Msg::OpenFile(_)
        ));
        assert!(matches!(
            file_action(std::path::Path::new("/c/Rustket-0.3.0-x64-setup.EXE")).1,
            Msg::OpenFile(_)
        ));
        assert!(matches!(
            file_action(std::path::Path::new("/c/Rustket-0.3.0-x86_64.AppImage")).1,
            Msg::RevealFile(_)
        ));
        assert!(matches!(
            file_action(std::path::Path::new("/c/Rustket.app")).1,
            Msg::RevealFile(_)
        ));
    }

    #[test]
    fn settings_status() {
        let mut u = Updater::new(false);
        let d = settings_data(&u, None, None);
        assert_eq!(d.method, kind_label(None));
        assert!(d.status.is_empty() && !d.busy && d.install.is_none());
        assert_eq!(d.last_checked, t!("updater.never_checked"));
        let d = settings_data(&u, Some(1_700_000_000), None);
        assert!(d.last_checked.contains("2023-11-1"), "{}", d.last_checked);

        u.phase = Phase::Available {
            release: release("0.3.0"),
            manual: false,
        };
        let d = settings_data(&u, None, Some("0.3.0"));
        assert!(d.status.contains("0.3.0") && d.status.ends_with(&*t!("updater.skipped")));
        assert_eq!(d.install, Some(t!("updater.download").into_owned()));
        u.kind = Some(InstallKind::AppImage { image: "/a".into() });
        let d = settings_data(&u, None, None);
        assert!(!d.status.contains(&*t!("updater.skipped")));
        assert_eq!(d.install, Some(t!("updater.install").into_owned()));

        u.phase = Phase::Downloading {
            release: release("0.3.0"),
            received: 1_500_000,
            total: 3_000_000,
        };
        let d = settings_data(&u, None, None);
        assert!(d.busy && d.install.is_none());
        assert!(d.status.contains("1.5") && d.status.contains("3.0"));
    }
}
