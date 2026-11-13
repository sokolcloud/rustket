//! Apps-engine UIKit UI (docs/uikit.md §10.3): message blocks, modals,
//! contextual bars, banners, app action buttons and app slash commands.
//! Strings: `locales/uikit.yml` (`uikit.*`).
//!
//! The durable state lives in the workspace (`rustket_core::uikit::UiKitStore`:
//! triggers, open views and their values, banners, buttons, plus the
//! transient widget [`Scratch`]); [`controller`] turns [`UiKitMsg`]s into REST
//! calls and store updates, the app glue (`app/uikit.rs`) runs them.
//! Surfaces owned by other components only render through this module and
//! forward [`UiKitMsg`] as `Effect::UiKit`.

pub mod banner;
pub mod bar;
pub mod buttons;
mod cache;
pub mod commands;
pub mod controller;
mod elements;
pub mod modal;
pub mod mrkdwn;
pub mod render;
mod text;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use iced::Element;
use iced::widget::text_editor;
use rocketchat::ServerUrl;
use rocketchat::models::{ActionButton, AppLanguages, Banner, Message as RcMessage};
use rocketchat::uikit::{Confirm, LayoutBlock, ServerInteraction, Surface};
use rustket_core::Workspace;
use rustket_core::uikit::InteractionResult;
pub use rustket_core::uikit::{ActionRequest, Scratch, Suggestion, SurfaceKey};

use crate::error::Res;
use crate::images::Images;
use crate::room::render::{RenderCtx, RenderMsg};

/// An interaction with a UIKit surface or an app button.
#[derive(Debug, Clone, PartialEq)]
pub enum UiKitMsg {
    /// An element changed (button click, selection, date, scale point, tab):
    /// `req.value` is the new value. `emit` sends a block action (after the
    /// confirmation dialog when the element has `confirm`); otherwise the
    /// value is only stored (view state, or the local widget value).
    Change {
        req: ActionRequest,
        emit: bool,
        confirm: Option<Box<Confirm>>,
    },
    /// A checkbox / toggle switch option was toggled: `req.value` is the new
    /// selection (the state keeps the whole array), an emitted action carries
    /// only the toggled `option` (§2.4).
    Toggle {
        req: ActionRequest,
        option: String,
        emit: bool,
    },
    /// Text typed into a plain text input (`req.value` = the text);
    /// `dispatch`: `on_character_entered` (debounced in views, §5.2).
    Text {
        req: ActionRequest,
        dispatch: bool,
    },
    /// Edit of a multiline input of the modal (`text_editor`).
    Editor {
        req: ActionRequest,
        dispatch: bool,
        action: text_editor::Action,
    },
    /// Half-typed date/time text of an element (`key`); applied when valid.
    Typing {
        req: ActionRequest,
        text: String,
        emit: bool,
        kind: Typed,
    },
    /// Open (`Some(key)`) or close a popover (select list, overflow, calendar,
    /// composer app menu).
    Popover(Option<String>),
    /// Filter text of the open select.
    Filter(String),
    /// Autocomplete query of a users / channels select.
    Query {
        req: ActionRequest,
        text: String,
        users: bool,
    },
    /// "Show more..." of an actions block (`key`).
    ShowMore(String),
    Submit(SurfaceKey),
    /// Close a view (`cleared`: X button, backdrop, Escape; §5.3).
    Close {
        surface: SurfaceKey,
        cleared: bool,
    },
    /// Answer of the confirmation dialog.
    Confirmed(bool),
    /// An app action button (§6.3); `message` is the composer text.
    Button {
        button: Box<ActionButton>,
        rid: Option<String>,
        tmid: Option<String>,
        mid: Option<String>,
        message: Option<String>,
    },
    /// Run an app or built-in slash command (`commands.run`, §7) from a
    /// composer; `app_id` from `commands.list`.
    Command {
        rid: String,
        tmid: Option<String>,
        command: String,
        params: String,
        app_id: Option<String>,
    },
    /// A link or mention inside a text object.
    Render(Box<RenderMsg>),
}

/// What a typed field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    Date,
    Time,
    /// The month a date picker's calendar shows (`YYYY-MM`).
    Month,
}

/// App-level messages of the UIKit controller (`Message::UiKit`).
#[derive(Debug, Clone)]
pub enum Msg {
    /// A surface interaction; `rid` is the room it came from, if any.
    Ui {
        rid: Option<String>,
        msg: UiKitMsg,
    },
    /// The HTTP part of interaction `trigger` ended.
    Finished {
        trigger: String,
        result: InteractionResult,
    },
    /// Debounce timer of an `on_character_entered` input.
    Debounced {
        req: ActionRequest,
        stamp: u64,
    },
    /// Debounce timer of an autocomplete query.
    QueryDue {
        req: ActionRequest,
        users: bool,
        stamp: u64,
    },
    /// Autocomplete results of element `key` for the query `term`.
    Suggestions {
        key: String,
        term: String,
        result: Res<Vec<Suggestion>>,
    },
    /// `commands.run` ended.
    CommandRan(commands::Ran),
    /// Refetch the action buttons (debounced `actions/changed`).
    RefreshButtons(u64),
    Buttons(Res<Vec<ActionButton>>),
    Banners(Res<Vec<Banner>>),
    Languages(Res<Vec<AppLanguages>>),
}

/// What the session dispatcher hands to the UIKit glue.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionHook {
    /// `{uid}/uiInteraction` push.
    Interaction(Box<ServerInteraction>),
    /// A `stream-apps` event.
    Apps(Box<rocketchat::models::AppsStreamEvent>),
    /// `{uid}/banners` / `banner-changed`: refetch the banners.
    Banners,
}

/// The `stream-apps` event carried by a session event, if any.
pub fn apps_event(ev: &rocketchat::session::SessionEvent) -> Option<SessionHook> {
    rocketchat::models::AppsStreamEvent::from_session_event(ev)
        .map(|e| SessionHook::Apps(Box::new(e)))
}

/// Per-server UI state that cannot live in the workspace (iced types).
#[derive(Default)]
pub struct Local {
    /// Contents of multiline inputs of the modal, by scratch key.
    pub editors: HashMap<String, text_editor::Content>,
}

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local")
            .field("editors", &self.editors.len())
            .finish()
    }
}

/// Language used for app translations.
pub(crate) fn lang() -> String {
    rust_i18n::locale().to_string()
}

/// The blocks of `msg` rendered on the message surface, `None` when the
/// message has none (then its markdown body shows instead, §3.1).
pub fn message_blocks(msg: &RcMessage, rc: &RenderCtx<'_>) -> Option<Element<'static, RenderMsg>> {
    let blocks = cache::blocks(msg)?;
    let surface = SurfaceKey::Message {
        rid: msg.rid.clone(),
        mid: msg.id.clone(),
        tmid: msg.tmid.clone(),
    };
    let renderable = Surface::Message.renderable(&blocks);
    let env = render::Env::message(msg, surface, rc);
    let el: Element<'static, UiKitMsg> = render::blocks(&env, None, &renderable);
    Some(el.map(|m| match m {
        UiKitMsg::Render(r) => *r,
        m => RenderMsg::UiKit(Box::new(m)),
    }))
}

/// Part of a timeline row's cache key for the UIKit state of `msg`: app
/// buttons and translations (the row's menus list app entries), and for
/// messages with blocks the widget scratch of that message, the ephemeral
/// flag and loaded block images.
pub fn row_key(ws: &Workspace, images: &Images, server: &ServerUrl, msg: &RcMessage) -> u64 {
    let store = ws.uikit();
    let Some(blocks) = cache::blocks(msg) else {
        return store.revision();
    };
    let mut h = DefaultHasher::new();
    store.revision().hash(&mut h);
    let surface = SurfaceKey::Message {
        rid: msg.rid.clone(),
        mid: msg.id.clone(),
        tmid: msg.tmid.clone(),
    };
    store.scratch().digest(&surface.prefix()).hash(&mut h);
    ws.is_ephemeral(&msg.rid, &msg.id).hash(&mut h);
    crate::i18n::cache_tag().hash(&mut h);
    for url in render::image_urls(server, &blocks) {
        images.contains(&url).hash(&mut h);
    }
    h.finish().max(1)
}

/// The decoded, renderable blocks of a view (no size guard: views come from
/// accepted interactions).
pub(crate) fn view_blocks(blocks: &[LayoutBlock], surface: Surface) -> Vec<LayoutBlock> {
    surface.renderable(blocks)
}

/// The confirmation dialog (first) or the modal of `app`, with its key and
/// the message of the backdrop / Escape.
pub fn overlay<'a>(
    app: modal::AppCtx<'a>,
    local: &'a Local,
) -> Option<(String, Element<'a, UiKitMsg>, UiKitMsg)> {
    if let Some((c, _)) = &app.workspace.uikit().scratch().confirm {
        return Some((
            String::from("confirm"),
            modal::confirm_view(app, c),
            UiKitMsg::Confirmed(false),
        ));
    }
    modal::view(app, local)
}

/// Widget-tree key of the overlay [`overlay`] shows: the confirmation
/// dialog or the modal's view id.
pub fn overlay_key(ws: &Workspace) -> Option<String> {
    let store = ws.uikit();
    if store.scratch().confirm.is_some() {
        return Some(String::from("confirm"));
    }
    store.modal().map(|m| format!("modal:{}", m.id()))
}

/// What the app shows above the room area: the banner (the busy pill floats
/// over the area instead, `App::uikit_busy_over`).
pub fn strips<'a>(app: modal::AppCtx<'a>) -> Vec<Element<'a, UiKitMsg>> {
    banner::view(app).into_iter().collect()
}
