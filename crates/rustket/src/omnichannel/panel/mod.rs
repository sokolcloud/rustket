//! Omnichannel panels of the room's right panel (`docs/omnichannel.md` §7.3):
//! room information (§8), edit (§8.1), contact information (§9), contact
//! history and a past chat (§10), canned responses (§16, EE). Shown by
//! `room::panel::Panel` for `Kind::Omnichannel(kind)`.

pub mod canned;
pub mod contact;
pub mod edit;
pub mod history;
pub mod info;

use iced::Element;
use rust_i18n::t;
use rustket_core::workspace::Changes;

use crate::room::{Action, UpdateCtx, ViewCtx};

/// Which omnichannel panel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Info,
    Edit,
    Contact,
    ContactHistory,
    /// A past chat (`rid`) from the contact history.
    ContactHistoryChat(String),
    Canned,
}

impl PanelKind {
    pub fn i18n_key(&self) -> &'static str {
        match self {
            PanelKind::Info => "omnichannel.panel.info",
            PanelKind::Edit => "omnichannel.panel.edit",
            PanelKind::Contact => "omnichannel.panel.contact",
            PanelKind::ContactHistory => "omnichannel.panel.history",
            PanelKind::ContactHistoryChat(_) => "omnichannel.panel.history_chat",
            PanelKind::Canned => "omnichannel.panel.canned",
        }
    }

    /// Panels reached from another one get a back button to it.
    pub fn back_target(&self, from: &PanelKind) -> bool {
        matches!(
            (from, self),
            (PanelKind::Info, PanelKind::Edit | PanelKind::Contact)
                | (PanelKind::Contact, PanelKind::ContactHistory)
                | (PanelKind::ContactHistory, PanelKind::ContactHistoryChat(_))
        )
    }
}

/// State of an open omnichannel panel.
pub enum OmniPanel {
    Info(info::InfoPanel),
    Edit(edit::EditPanel),
    Contact(contact::ContactPanel),
    History(history::HistoryPanel),
    Canned(canned::CannedPanel),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Info(info::Msg),
    Edit(edit::Msg),
    Contact(contact::Msg),
    History(history::Msg),
    Canned(canned::Msg),
}

impl OmniPanel {
    pub fn new(kind: &PanelKind) -> Self {
        match kind {
            PanelKind::Info => OmniPanel::Info(info::InfoPanel::default()),
            PanelKind::Edit => OmniPanel::Edit(edit::EditPanel::default()),
            PanelKind::Contact => OmniPanel::Contact(contact::ContactPanel::default()),
            PanelKind::ContactHistory => OmniPanel::History(history::HistoryPanel::list()),
            PanelKind::ContactHistoryChat(rid) => {
                OmniPanel::History(history::HistoryPanel::chat(rid))
            }
            PanelKind::Canned => OmniPanel::Canned(canned::CannedPanel::default()),
        }
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match self {
            OmniPanel::Info(p) => p.activate(ctx).map(Msg::Info),
            OmniPanel::Edit(p) => p.activate(ctx).map(Msg::Edit),
            OmniPanel::Contact(p) => p.activate(ctx).map(Msg::Contact),
            OmniPanel::History(p) => p.activate(ctx).map(Msg::History),
            OmniPanel::Canned(p) => p.activate(ctx).map(Msg::Canned),
        }
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match (self, msg) {
            (OmniPanel::Info(p), Msg::Info(m)) => p.update(m, ctx).map(Msg::Info),
            (OmniPanel::Edit(p), Msg::Edit(m)) => p.update(m, ctx).map(Msg::Edit),
            (OmniPanel::Contact(p), Msg::Contact(m)) => p.update(m, ctx).map(Msg::Contact),
            (OmniPanel::History(p), Msg::History(m)) => p.update(m, ctx).map(Msg::History),
            (OmniPanel::Canned(p), Msg::Canned(m)) => p.update(m, ctx).map(Msg::Canned),
            _ => Action::none(),
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match self {
            OmniPanel::Info(p) => p.on_changes(changes, ctx).map(Msg::Info),
            _ => Action::none(),
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        match self {
            OmniPanel::Info(p) => p.view(ctx).map(Msg::Info),
            OmniPanel::Edit(p) => p.view(ctx).map(Msg::Edit),
            OmniPanel::Contact(p) => p.view(ctx).map(Msg::Contact),
            OmniPanel::History(p) => p.view(ctx).map(Msg::History),
            OmniPanel::Canned(p) => p.view(ctx).map(Msg::Canned),
        }
    }
}

/// `1h 2m`, `3m 5s`, `12s`.
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        t!("omnichannel.duration_hm", h = h, m = m).into_owned()
    } else if m > 0 {
        t!("omnichannel.duration_ms", m = m, s = s).into_owned()
    } else {
        t!("omnichannel.duration_s", s = s).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_back_targets() {
        rust_i18n::set_locale("en");
        assert_eq!(duration(12), "12s");
        assert_eq!(duration(185), "3m 5s");
        assert_eq!(duration(3720), "1h 2m");
        assert_eq!(duration(-4), "0s");
        assert!(PanelKind::Edit.back_target(&PanelKind::Info));
        assert!(PanelKind::ContactHistoryChat("r".into()).back_target(&PanelKind::ContactHistory));
        assert!(!PanelKind::Canned.back_target(&PanelKind::Info));
    }
}
