//! App action buttons (docs/uikit.md §6): menu entries for the room header
//! kebab, the message menu, the composer and the user menu, filtered with
//! `rustket_core::uikit::visible_buttons`. Labels come from the app's
//! language bundle (raw `labelI18n` as fallback); `variant: danger` is
//! applied everywhere.

use rocketchat::models::{ActionButton, Message as RcMessage};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::uikit::{ButtonQuery, visible_buttons};
use rustket_ui::icon::Icon;
use rustket_ui::widget::{MenuEntry, MenuItem};

use super::UiKitMsg;

fn label(ws: &Workspace, b: &ActionButton) -> String {
    ws.uikit().translations().button_label(b, &super::lang())
}

fn danger(b: &ActionButton) -> bool {
    b.variant.as_deref() == Some("danger")
}

fn item<M>(ws: &Workspace, b: &ActionButton, msg: M) -> MenuEntry<M> {
    MenuItem::new(label(ws, b), msg).danger(danger(b)).into()
}

/// A section: a header and the buttons (nothing when there are none).
fn section<M>(
    ws: &Workspace,
    title: String,
    buttons: &[&ActionButton],
    msg: &dyn Fn(&ActionButton) -> M,
) -> Vec<MenuEntry<M>> {
    if buttons.is_empty() {
        return Vec::new();
    }
    let mut v = vec![MenuEntry::Separator, MenuEntry::Header(title)];
    v.extend(buttons.iter().map(|b| item(ws, b, msg(b))));
    v
}

/// Appends app sections to `entries` (without a leading separator when
/// `entries` is empty).
pub fn append<M: Clone>(entries: &mut Vec<MenuEntry<M>>, apps: &[MenuEntry<M>]) {
    let skip =
        usize::from(entries.is_empty() && matches!(apps.first(), Some(MenuEntry::Separator)));
    entries.extend(apps.iter().skip(skip).cloned());
}

/// Room header kebab: an "Apps" section and an "AI actions" section.
pub fn room_entries<M>(
    ws: &Workspace,
    rid: &str,
    wrap: impl Fn(UiKitMsg) -> M,
) -> Vec<MenuEntry<M>> {
    let buttons = ws.uikit().buttons();
    let msg = |b: &ActionButton| {
        wrap(UiKitMsg::Button {
            button: Box::new(b.clone()),
            rid: Some(rid.to_owned()),
            tmid: None,
            mid: None,
            message: None,
        })
    };
    let mut v = section(
        ws,
        t!("uikit.apps").into_owned(),
        &visible_buttons(ws, buttons, &ButtonQuery::room(rid)),
        &msg,
    );
    v.extend(section(
        ws,
        t!("uikit.ai_actions").into_owned(),
        &visible_buttons(ws, buttons, &ButtonQuery::room(rid).ai()),
        &msg,
    ));
    v
}

/// Message menu (`list`: `message` for the room timeline, `threads` for the
/// thread view, §6.2). End-to-end encrypted messages get one disabled
/// "Unavailable" entry instead of the buttons (§6.3).
pub fn message_entries<M: Clone>(
    ws: &Workspace,
    msg: &RcMessage,
    list: &str,
    wrap: impl Fn(UiKitMsg) -> M,
    noop: M,
) -> Vec<MenuEntry<M>> {
    let buttons = ws.uikit().buttons();
    let rid = msg.rid.as_str();
    let mut found = visible_buttons(ws, buttons, &ButtonQuery::message(rid, list));
    found.extend(visible_buttons(
        ws,
        buttons,
        &ButtonQuery::message(rid, list).ai(),
    ));
    if found.is_empty() {
        return Vec::new();
    }
    let mut v = vec![
        MenuEntry::Separator,
        MenuEntry::Header(t!("uikit.apps").into_owned()),
    ];
    if msg.is_encrypted() {
        v.push(
            MenuItem::new(t!("uikit.unavailable").into_owned(), noop)
                .disabled(true)
                .into(),
        );
        return v;
    }
    for b in found {
        v.push(item(
            ws,
            b,
            wrap(UiKitMsg::Button {
                button: Box::new(b.clone()),
                rid: Some(rid.to_owned()),
                tmid: msg.tmid.clone(),
                mid: Some(msg.id.clone()),
                message: None,
            }),
        ));
    }
    v
}

/// Composer app buttons (`category: ai` is not shown, §6.2); `text` is the
/// composer text sent as `payload.message`.
pub fn composer_entries<M>(
    ws: &Workspace,
    rid: &str,
    tmid: Option<&str>,
    text: &str,
    wrap: impl Fn(UiKitMsg) -> M,
) -> Vec<MenuEntry<M>> {
    visible_buttons(ws, ws.uikit().buttons(), &ButtonQuery::composer(rid))
        .into_iter()
        .map(|b| {
            item(
                ws,
                b,
                wrap(UiKitMsg::Button {
                    button: Box::new(b.clone()),
                    rid: Some(rid.to_owned()),
                    tmid: tmid.map(str::to_owned),
                    mid: None,
                    message: Some(text.to_owned()),
                }),
            )
        })
        .collect()
}

/// Whether the composer of `rid` has app buttons.
pub fn composer_has_buttons(ws: &Workspace, rid: &str) -> bool {
    !visible_buttons(ws, ws.uikit().buttons(), &ButtonQuery::composer(rid)).is_empty()
}

/// Popover key of the composer's app menu.
pub fn composer_menu_key(rid: &str, tmid: Option<&str>) -> String {
    format!("composer:{rid}:{}", tmid.unwrap_or_default())
}

/// Whether the composer app menu of `rid`/`tmid` is open.
pub fn composer_menu_open(ws: &Workspace, rid: &str, tmid: Option<&str>) -> bool {
    ws.uikit().scratch().open.as_deref() == Some(composer_menu_key(rid, tmid).as_str())
}

/// Icon of the composer's app menu button.
pub const COMPOSER_ICON: Icon = Icon::Blocks;

/// User menu entries: (label, message, danger). Only the permission/role
/// filter applies, so `ai` buttons are listed too (§6.2).
pub fn user_menu(ws: &Workspace) -> Vec<(String, UiKitMsg, bool)> {
    let buttons = ws.uikit().buttons();
    visible_buttons(ws, buttons, &ButtonQuery::user_menu())
        .into_iter()
        .map(|b| {
            (
                label(ws, b),
                UiKitMsg::Button {
                    button: Box::new(b.clone()),
                    rid: None,
                    tmid: None,
                    mid: None,
                    message: None,
                },
                danger(b),
            )
        })
        .collect()
}
