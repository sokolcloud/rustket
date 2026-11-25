//! General tab: language, window and startup, keyboard, messages,
//! `rocketchat://` links.

use iced::widget::{column, text};
use iced::{Element, Length};
use rust_i18n::t;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, TEXT_BODY};

use super::pickers::language_picker;
use super::widgets::{action_row, caption, section, switch};
use super::{Message, Settings, SettingsView};

pub(super) fn view<'a>(s: &'a Settings, v: &SettingsView<'a>) -> Vec<Element<'a, Message>> {
    let ui = &v.config.ui;
    let autostart: Element<'a, Message> = match s.autostart {
        Some(on) => switch(
            t!("settings.autostart").into_owned(),
            s.autostart_approval
                .then(|| t!("settings.autostart_approval").into_owned()),
            on,
            (!s.autostart_busy && s.autostart_at.is_some()).then_some(Message::SetAutostart),
        ),
        // Still loading, or no config directory at all.
        None => switch(
            t!("settings.autostart").into_owned(),
            Some(if s.autostart_at.is_none() {
                t!("settings.autostart_unknown").into_owned()
            } else {
                t!("settings.autostart_loading").into_owned()
            }),
            false,
            None,
        ),
    };
    let tray = v.tray;
    let close_help = if !tray.available {
        t!("settings.no_tray")
    } else if !tray.can_hide {
        t!("settings.tray_help_wayland")
    } else {
        t!("settings.tray_help")
    };
    let behaviour = section(
        t!("settings.behaviour").into_owned(),
        vec![
            switch(
                t!("settings.close_to_tray").into_owned(),
                Some(close_help.into_owned()),
                ui.close_to_tray && tray.available,
                tray.available.then_some(Message::SetCloseToTray),
            ),
            switch(
                t!("settings.minimize_to_tray").into_owned(),
                (tray.available && !tray.can_hide)
                    .then(|| t!("settings.minimize_wayland").into_owned()),
                ui.minimize_to_tray && tray.can_hide,
                (tray.available && tray.can_hide).then_some(Message::SetMinimizeToTray),
            ),
            switch(
                t!("settings.start_minimized").into_owned(),
                None,
                ui.start_minimized,
                Some(Message::SetStartMinimized),
            ),
        ]
        .into_iter()
        .chain((!s.autostart_hidden()).then_some(autostart))
        .collect(),
    );

    // macOS: the app bundle declares the scheme; nothing to register.
    let links = if crate::url_handler::MANAGED_BY_BUNDLE {
        column![
            text(t!("settings.links")).size(TEXT_BODY),
            caption(t!("settings.handler_bundle").into_owned()),
        ]
        .spacing(SPACE_1)
        .width(Length::Fill)
        .into()
    } else {
        let registered = v.handler_registered;
        action_row(
            t!("settings.links").into_owned(),
            caption(if registered {
                t!("settings.handler_on").into_owned()
            } else {
                t!("settings.handler_off").into_owned()
            })
            .style(if registered {
                style::text_success
            } else {
                style::text_secondary
            })
            .into(),
            t!("settings.handler_register").into_owned(),
            Some(Message::RegisterHandler),
        )
    };

    let keyboard = section(
        t!("settings.keyboard").into_owned(),
        vec![
            switch(
                t!("settings.escape_marks_read").into_owned(),
                Some(t!("settings.escape_marks_read_help").into_owned()),
                ui.escape_marks_read,
                Some(Message::SetEscapeMarksRead),
            ),
            action_row(
                t!("shortcuts.title").into_owned(),
                caption(
                    t!(
                        "settings.shortcuts_help",
                        keys = crate::keymap::display(&[
                            crate::keymap::Token::Cmd,
                            crate::keymap::Token::Char('/'),
                        ])
                    )
                    .into_owned(),
                )
                .into(),
                t!("settings.shortcuts_show").into_owned(),
                Some(Message::ShowShortcuts),
            ),
        ],
    );

    let messages = section(
        t!("settings.messages").into_owned(),
        vec![
            switch(
                t!("settings.confirm_broad_mentions").into_owned(),
                Some(
                    t!(
                        "settings.confirm_broad_mentions_help",
                        count = crate::room::composer::broad::MIN_MEMBERS
                    )
                    .into_owned(),
                ),
                ui.confirm_broad_mentions,
                Some(Message::SetConfirmBroadMentions),
            ),
            switch(
                t!("bookmarks.setting").into_owned(),
                Some(t!("bookmarks.setting_help").into_owned()),
                ui.room_bookmarks,
                Some(Message::SetRoomBookmarks),
            ),
        ],
    );

    vec![
        section(
            t!("settings.language").into_owned(),
            vec![language_picker(ui, v.locales)],
        ),
        behaviour,
        keyboard,
        messages,
        section(t!("settings.integration").into_owned(), vec![links]),
    ]
}
