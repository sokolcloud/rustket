//! UI-agnostic UIKit state (docs/uikit.md §4.6, §5, §6, §10.2): trigger ids,
//! the open modal / contextual bars / banners with their input values, app
//! action buttons and app translations. Plain data; the UI sends the
//! interactions this store builds and feeds the answers back.

pub mod buttons;
pub mod i18n;
pub mod scratch;
pub mod trigger;
pub mod trust;
pub mod view_state;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use rocketchat::models::{
    ActionButton, AppsStreamEvent, Banner, BannerSurface, ButtonContext, Message,
};
use rocketchat::uikit::{
    ActionButtonPayload, BannerView, BlockActionPayload, Container, ContextualBarView,
    InteractionResponse, ModalView, RawView, ServerInteraction, UserInteraction, ViewKind,
};
use serde_json::Value;

pub use buttons::{ButtonCategory, ButtonQuery, button_visible, visible_buttons};
pub use i18n::AppTranslations;
pub use scratch::{ActionRequest, Scratch, Suggestion, SurfaceKey};
pub use trigger::{Origin, OriginKind, Trigger, TriggerRegistry, ViewTarget};
pub use trust::{
    Permit, actions_allowed, element_permit, is_local_videoconf_action, is_openable_url,
    openable_url,
};
pub use view_state::ViewState;

/// App id of banners without one.
pub const CLOUD_ANNOUNCEMENTS_APP: &str = "cloud-announcements-core";

/// A modal or contextual bar on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenView<V> {
    pub app_id: String,
    pub view: RawView<V>,
    pub state: ViewState,
    /// Validation errors by action id.
    pub errors: HashMap<String, String>,
    /// A `viewSubmit` is in flight (blocks a second one).
    pub submitting: bool,
    /// Room of a contextual bar (`None`: not placed yet; the UI shows it in
    /// the active room and records that with [`UiKitStore::place_bar`]).
    pub rid: Option<String>,
    /// Instance counter: a view reopened with the same id gets a new one.
    pub generation: u64,
}

impl<V: ViewKind> OpenView<V> {
    /// `None` when neither the view, the interaction nor the trigger names an app.
    fn new(
        fallback_apps: [&str; 2],
        view: RawView<V>,
        rid: Option<String>,
        generation: u64,
    ) -> Option<Self> {
        let app_id = std::iter::once(view.view.app_id())
            .chain(fallback_apps)
            .find(|a| !a.is_empty())?
            .to_owned();
        Some(Self {
            app_id,
            state: ViewState::from_blocks(view.view.blocks()),
            view,
            errors: HashMap::new(),
            submitting: false,
            rid,
            generation,
        })
    }

    pub fn id(&self) -> &str {
        self.view.id()
    }

    /// Replaces the view as received, keeps values (adding initial values of
    /// new action ids) and clears errors (§5.2, §5.4).
    fn update(&mut self, view: RawView<V>) {
        self.state.merge_initials(view.view.blocks());
        self.view = view;
        self.errors.clear();
    }
}

/// A banner on screen (`GET /api/v1/banners` or `banner.open`).
#[derive(Debug, Clone, PartialEq)]
pub struct BannerEntry {
    /// `IBanner._id` (`None` for banners opened by an interaction).
    pub banner_id: Option<String>,
    pub app_id: String,
    pub surface: BannerSurface,
    pub view: RawView<BannerView>,
    /// Modal-surface announcements carry a modal view instead.
    pub modal: Option<RawView<ModalView>>,
}

impl BannerEntry {
    pub fn view_id(&self) -> &str {
        &self.view.view.view_id
    }
}

/// A client-side message caused by an interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiKitToast {
    /// "App has failed to respond" (`UIKit_Interaction_Timeout`).
    Timeout,
    /// The request failed (HTTP error); the text is for logs/details.
    AppError(String),
    /// Rate limited (HTTP 429); retry after this long when known.
    RateLimited(Option<Duration>),
    /// A server interaction `type` this client does not support.
    Unsupported(String),
}

/// What changed in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiKitChange {
    ModalOpened(String),
    ModalUpdated(String),
    ModalClosed(String),
    BarOpened {
        rid: Option<String>,
        view_id: String,
    },
    BarUpdated {
        rid: Option<String>,
        view_id: String,
    },
    BarClosed {
        rid: Option<String>,
        view_id: String,
    },
    ErrorsSet(ViewTarget, String),
    BannerOpened(String),
    BannerUpdated(String),
    BannerClosed(String),
    Toast(UiKitToast),
    /// The session token was refused (HTTP 401): start the re-login flow.
    Unauthorized,
}

/// Result of `RestClient::ui_interaction`, cloneable for UI messages.
#[derive(Debug, Clone, PartialEq)]
pub enum InteractionResult {
    Response(InteractionResponse),
    Timeout,
    RateLimited(Option<Duration>),
    Unauthorized,
    Failed(String),
}

impl From<rocketchat::Result<InteractionResponse>> for InteractionResult {
    fn from(r: rocketchat::Result<InteractionResponse>) -> Self {
        match r {
            Ok(resp) => Self::Response(resp),
            Err(rocketchat::Error::Timeout(_)) => Self::Timeout,
            Err(rocketchat::Error::RateLimited { retry_after, .. }) => {
                Self::RateLimited(retry_after)
            }
            Err(rocketchat::Error::Unauthorized { .. }) => Self::Unauthorized,
            Err(e) => Self::Failed(e.to_string()),
        }
    }
}

/// How the HTTP part of an interaction ended (for [`close_decision`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpOutcome {
    Timeout,
    Failed,
    /// 2xx with an empty body: the app returned nothing.
    Empty,
    /// 2xx with a body (`{}` included).
    Body,
}

/// Whether the view a `viewSubmit` came from closes (§4.6, RN rule):
/// kept open by `errors` / `*.update` answers, never closed after a
/// `modal.open` (which already replaced it), closed after another accepted
/// interaction, else closed only for a non-empty 2xx body or a timeout.
/// `viewClosed` closes locally at once, so it never needs a decision.
pub fn close_decision(kind: OriginKind, handled: Option<&str>, outcome: HttpOutcome) -> bool {
    if kind != OriginKind::ViewSubmit {
        return false;
    }
    match handled {
        Some("errors" | "modal.update" | "contextual_bar.update" | "modal.open") => false,
        Some(_) => true,
        None => matches!(outcome, HttpOutcome::Body | HttpOutcome::Timeout),
    }
}

/// What the app should refetch after a `stream-apps` event (§6.5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppsFollowup {
    pub action_buttons: bool,
    pub commands: bool,
    pub languages: bool,
    /// Refresh the apps admin list if it is open.
    pub admin: bool,
}

/// Per-workspace UIKit state.
#[derive(Debug, Clone, Default)]
pub struct UiKitStore {
    triggers: TriggerRegistry,
    modal: Option<OpenView<ModalView>>,
    bars: Vec<OpenView<ContextualBarView>>,
    banners: Vec<BannerEntry>,
    buttons: Vec<ActionButton>,
    translations: AppTranslations,
    /// Triggers of app requests in flight (built-in slash commands excluded).
    busy: HashSet<String>,
    generations: u64,
    scratch: Scratch,
    /// Bumped when action buttons or app translations change (message rows
    /// that show app entries key on it).
    revision: u64,
    /// Banner ids of modal-surface announcements already opened as the modal
    /// (the server lists them until its dismiss ran; a refetch must not
    /// reopen them).
    announced: HashSet<String>,
}

impl UiKitStore {
    pub fn new() -> Self {
        Self::default()
    }

    // --- accessors ----------------------------------------------------------------

    pub fn triggers(&self) -> &TriggerRegistry {
        &self.triggers
    }

    pub fn modal(&self) -> Option<&OpenView<ModalView>> {
        self.modal.as_ref()
    }

    pub fn modal_mut(&mut self) -> Option<&mut OpenView<ModalView>> {
        self.modal.as_mut()
    }

    pub fn bars(&self) -> &[OpenView<ContextualBarView>] {
        &self.bars
    }

    pub fn bar(&self, view_id: &str) -> Option<&OpenView<ContextualBarView>> {
        self.bars.iter().find(|b| b.id() == view_id)
    }

    pub fn bar_mut(&mut self, view_id: &str) -> Option<&mut OpenView<ContextualBarView>> {
        self.bars.iter_mut().find(|b| b.id() == view_id)
    }

    /// The app panel of a room.
    pub fn bar_for_room(&self, rid: &str) -> Option<&OpenView<ContextualBarView>> {
        self.bars.iter().find(|b| b.rid.as_deref() == Some(rid))
    }

    /// The state of the open modal or bar `view_id`.
    pub fn view_state_mut(&mut self, target: ViewTarget, view_id: &str) -> Option<&mut ViewState> {
        match target {
            ViewTarget::Modal => self
                .modal
                .as_mut()
                .filter(|m| m.id() == view_id)
                .map(|m| &mut m.state),
            ViewTarget::Bar => self.bar_mut(view_id).map(|b| &mut b.state),
        }
    }

    pub fn banners(&self) -> &[BannerEntry] {
        &self.banners
    }

    /// The banner to show (the web shows one at a time, the first).
    pub fn current_banner(&self) -> Option<&BannerEntry> {
        self.banners
            .iter()
            .find(|b| b.surface == BannerSurface::Banner)
    }

    pub fn buttons(&self) -> &[ActionButton] {
        &self.buttons
    }

    /// Replaces the action buttons (`GET /api/apps/actionButtons`).
    pub fn set_buttons(&mut self, buttons: Vec<ActionButton>) {
        self.buttons = buttons;
        self.bump();
    }

    pub fn translations(&self) -> &AppTranslations {
        &self.translations
    }

    pub fn translations_mut(&mut self) -> &mut AppTranslations {
        self.bump();
        &mut self.translations
    }

    /// Changes with the action buttons and app translations.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Interactions in flight (the "Loading" pill shows while > 0).
    pub fn busy(&self) -> u32 {
        u32::try_from(self.busy.len()).unwrap_or(u32::MAX)
    }

    /// Transient widget state of the surfaces (see [`Scratch`]).
    pub fn scratch(&self) -> &Scratch {
        &self.scratch
    }

    pub fn scratch_mut(&mut self) -> &mut Scratch {
        &mut self.scratch
    }

    fn next_generation(&mut self) -> u64 {
        self.generations += 1;
        self.generations
    }

    // --- issuing interactions -----------------------------------------------------

    /// Registers a trigger and, for app requests (`app_id` set), counts it as
    /// in flight. Pair with [`Self::finish`] (or [`Self::finish_command`]).
    pub fn begin(&mut self, app_id: Option<&str>, origin: Origin, now: Instant) -> String {
        let id = self.triggers.issue(app_id, origin, now);
        if app_id.is_some() {
            self.busy.insert(id.clone());
        }
        id
    }

    /// A block action on a message. `None` when the trust rule (§3.2) forbids
    /// it, including `video_conf` `join` / `callBack`, which the client handles
    /// itself; `is_ephemeral` must come from `Workspace::is_ephemeral`.
    #[allow(clippy::too_many_arguments)]
    pub fn message_block_action(
        &mut self,
        msg: &Message,
        is_ephemeral: bool,
        in_video_conf_block: bool,
        app_id: Option<&str>,
        block_id: &str,
        action_id: &str,
        value: Option<Value>,
        now: Instant,
    ) -> Option<(String, UserInteraction)> {
        if !actions_allowed(msg, is_ephemeral, in_video_conf_block, app_id, action_id) {
            return None;
        }
        let app = app_id?.to_owned();
        let origin = Origin::new(OriginKind::MessageBlock)
            .rid(Some(&msg.rid))
            .tmid(msg.tmid.as_deref())
            .mid(Some(&msg.id))
            .action(action_id);
        let trigger_id = self.begin(Some(&app), origin, now);
        Some((
            app,
            UserInteraction::BlockAction {
                action_id: action_id.to_owned(),
                payload: BlockActionPayload {
                    block_id: block_id.to_owned(),
                    value,
                },
                container: Container::Message(msg.id.clone()),
                mid: Some(msg.id.clone()),
                tmid: None,
                rid: Some(msg.rid.clone()),
                trigger_id,
            },
        ))
    }

    /// A block action inside the open modal or bar `view_id`. The element's
    /// `app_id` falls back to the view's; `None` when the view is not open.
    #[allow(clippy::too_many_arguments)]
    pub fn view_block_action(
        &mut self,
        target: ViewTarget,
        view_id: &str,
        app_id: Option<&str>,
        block_id: &str,
        action_id: &str,
        value: Option<Value>,
        now: Instant,
    ) -> Option<(String, UserInteraction)> {
        let (view_app, rid) = match target {
            ViewTarget::Modal => {
                let m = self.modal.as_ref().filter(|m| m.id() == view_id)?;
                (m.app_id.clone(), None)
            }
            ViewTarget::Bar => {
                let b = self.bar(view_id)?;
                (b.app_id.clone(), b.rid.clone())
            }
        };
        let app = app_id
            .filter(|a| !a.is_empty())
            .map_or(view_app, str::to_owned);
        let origin = Origin::new(OriginKind::ViewBlock)
            .rid(rid.as_deref())
            .view_in(target, view_id)
            .action(action_id);
        let trigger_id = self.begin(Some(&app), origin, now);
        Some((
            app,
            UserInteraction::BlockAction {
                action_id: action_id.to_owned(),
                payload: BlockActionPayload {
                    block_id: block_id.to_owned(),
                    value,
                },
                container: Container::View(view_id.to_owned()),
                mid: None,
                tmid: None,
                rid,
                trigger_id,
            },
        ))
    }

    /// `viewSubmit` for the open modal or bar `view_id`; `None` when it is not
    /// open or a submit is already in flight.
    pub fn view_submit(
        &mut self,
        target: ViewTarget,
        view_id: &str,
        now: Instant,
    ) -> Option<(String, UserInteraction)> {
        let (app, raw, state, rid, generation) = match target {
            ViewTarget::Modal => {
                let m = self.modal.as_mut().filter(|m| m.id() == view_id)?;
                if m.submitting {
                    return None;
                }
                m.submitting = true;
                (
                    m.app_id.clone(),
                    m.view.raw.clone(),
                    m.state.to_state_json(),
                    None,
                    m.generation,
                )
            }
            ViewTarget::Bar => {
                let b = self.bar_mut(view_id)?;
                if b.submitting {
                    return None;
                }
                b.submitting = true;
                (
                    b.app_id.clone(),
                    b.view.raw.clone(),
                    b.state.to_state_json(),
                    b.rid.clone(),
                    b.generation,
                )
            }
        };
        let origin = Origin::new(OriginKind::ViewSubmit)
            .rid(rid.as_deref())
            .view_in(target, view_id)
            .generation(generation);
        let trigger_id = self.begin(Some(&app), origin, now);
        let i = UserInteraction::view_submit(&raw, state, view_id, &app, rid, trigger_id);
        Some((app, i))
    }

    /// Closes the modal or bar `view_id` locally at once and builds its
    /// `viewClosed` (`cleared`: X button, backdrop, Escape).
    pub fn view_closed(
        &mut self,
        target: ViewTarget,
        view_id: &str,
        cleared: bool,
        now: Instant,
    ) -> Option<(String, UserInteraction, UiKitChange)> {
        let (app, raw, state, rid, change) = match target {
            ViewTarget::Modal => {
                if !self.modal.as_ref().is_some_and(|m| m.id() == view_id) {
                    return None;
                }
                let m = self.modal.take()?;
                (
                    m.app_id,
                    m.view.raw,
                    m.state.to_state_json(),
                    None,
                    UiKitChange::ModalClosed(view_id.to_owned()),
                )
            }
            ViewTarget::Bar => {
                let i = self.bars.iter().position(|b| b.id() == view_id)?;
                let b = self.bars.remove(i);
                let change = UiKitChange::BarClosed {
                    rid: b.rid.clone(),
                    view_id: view_id.to_owned(),
                };
                (b.app_id, b.view.raw, b.state.to_state_json(), b.rid, change)
            }
        };
        let origin = Origin::new(OriginKind::ViewClosed)
            .rid(rid.as_deref())
            .view_in(target, view_id);
        let trigger_id = self.begin(Some(&app), origin, now);
        let i = UserInteraction::view_closed(&raw, state, view_id, cleared, rid, trigger_id);
        Some((app, i, change))
    }

    /// An app action button. `message` is the composer text for
    /// `messageBoxAction`; `mid` the message for `messageAction`.
    pub fn action_button(
        &mut self,
        button: &ActionButton,
        rid: Option<&str>,
        tmid: Option<&str>,
        mid: Option<&str>,
        message: Option<&str>,
        now: Instant,
    ) -> (String, UserInteraction) {
        let ctx = button.context;
        let in_room = ctx != ButtonContext::UserDropdownAction;
        let rid = rid.filter(|_| in_room);
        let tmid = tmid.filter(|_| {
            matches!(
                ctx,
                ButtonContext::MessageAction | ButtonContext::MessageBoxAction
            )
        });
        let mid = mid.filter(|_| ctx == ButtonContext::MessageAction);
        let origin = Origin::new(OriginKind::ActionButton(ctx))
            .rid(rid)
            .tmid(tmid)
            .mid(mid)
            .action(&button.action_id);
        let trigger_id = self.begin(Some(&button.app_id), origin, now);
        (
            button.app_id.clone(),
            UserInteraction::ActionButton {
                action_id: button.action_id.clone(),
                payload: ActionButtonPayload {
                    context: ctx,
                    message: (ctx == ButtonContext::MessageBoxAction)
                        .then(|| message.unwrap_or_default().to_owned()),
                },
                mid: mid.map(str::to_owned),
                tmid: tmid.map(str::to_owned),
                rid: rid.map(str::to_owned),
                trigger_id,
            },
        )
    }

    /// A block action on a banner; the banner closes right after (web).
    pub fn banner_block_action(
        &mut self,
        view_id: &str,
        app_id: Option<&str>,
        block_id: &str,
        action_id: &str,
        value: Option<Value>,
        now: Instant,
    ) -> Option<(String, UserInteraction, UiKitChange)> {
        let i = self.banners.iter().position(|b| b.view_id() == view_id)?;
        let b = self.banners.remove(i);
        let app = app_id
            .filter(|a| !a.is_empty())
            .map_or(b.app_id, str::to_owned);
        let origin = Origin::new(OriginKind::ViewBlock)
            .view(view_id)
            .action(action_id);
        let trigger_id = self.begin(Some(&app), origin, now);
        Some((
            app,
            UserInteraction::BlockAction {
                action_id: action_id.to_owned(),
                payload: BlockActionPayload {
                    block_id: block_id.to_owned(),
                    value,
                },
                container: Container::View(view_id.to_owned()),
                mid: None,
                tmid: None,
                rid: None,
                trigger_id,
            },
            UiKitChange::BannerClosed(view_id.to_owned()),
        ))
    }

    /// Closes a banner locally and builds its `viewClosed` (to `banner-core`
    /// or `cloud-announcements-core`, which dismiss it server-side). Banner
    /// input values are never stored, so `state` holds the initial values.
    pub fn banner_closed(
        &mut self,
        view_id: &str,
        now: Instant,
    ) -> Option<(String, UserInteraction, UiKitChange)> {
        let i = self.banners.iter().position(|b| b.view_id() == view_id)?;
        let b = self.banners.remove(i);
        let state = ViewState::from_blocks(&b.view.view.blocks).to_state_json();
        let origin = Origin::new(OriginKind::ViewClosed).view(view_id);
        let trigger_id = self.begin(Some(&b.app_id), origin, now);
        let i = UserInteraction::banner_closed(&b.view.raw, state, view_id, trigger_id);
        Some((b.app_id, i, UiKitChange::BannerClosed(view_id.to_owned())))
    }

    /// Trigger for `commands.run` (`app_id` `None` for built-in commands, whose
    /// pushes are ignored). Call [`Self::finish_command`] when the call ends.
    pub fn slash_command_trigger(
        &mut self,
        app_id: Option<&str>,
        rid: &str,
        tmid: Option<&str>,
        now: Instant,
    ) -> String {
        let origin = Origin::new(OriginKind::SlashCommand)
            .rid(Some(rid))
            .tmid(tmid);
        self.begin(app_id, origin, now)
    }

    pub fn finish_command(&mut self, trigger_id: &str, now: Instant) {
        self.busy.remove(trigger_id);
        self.triggers.settle(trigger_id, now);
    }

    // --- answers ------------------------------------------------------------------

    /// The HTTP call of `trigger_id` ended: applies an interaction in the
    /// body, then the `viewSubmit` close decision (§4.6).
    pub fn finish(
        &mut self,
        trigger_id: &str,
        result: InteractionResult,
        now: Instant,
    ) -> Vec<UiKitChange> {
        self.busy.remove(trigger_id);
        self.triggers.settle(trigger_id, now);
        let mut changes = Vec::new();
        let outcome = match result {
            InteractionResult::Response(InteractionResponse::Empty) => HttpOutcome::Empty,
            InteractionResult::Response(InteractionResponse::Ack) => HttpOutcome::Body,
            InteractionResult::Response(InteractionResponse::Interaction(si)) => {
                changes.extend(self.apply(si, now));
                HttpOutcome::Body
            }
            InteractionResult::Timeout => {
                changes.push(UiKitChange::Toast(UiKitToast::Timeout));
                HttpOutcome::Timeout
            }
            InteractionResult::RateLimited(after) => {
                changes.push(UiKitChange::Toast(UiKitToast::RateLimited(after)));
                HttpOutcome::Failed
            }
            InteractionResult::Unauthorized => {
                changes.push(UiKitChange::Unauthorized);
                HttpOutcome::Failed
            }
            InteractionResult::Failed(e) => {
                tracing::debug!(error = %e, "app interaction failed");
                changes.push(UiKitChange::Toast(UiKitToast::AppError(e)));
                HttpOutcome::Failed
            }
        };
        let Some(t) = self.triggers.get(trigger_id) else {
            return changes;
        };
        let (kind, handled, view_id, generation) = (
            t.origin.kind,
            t.handled,
            t.origin.view_id.clone(),
            t.origin.generation,
        );
        if kind != OriginKind::ViewSubmit {
            return changes;
        }
        let Some(view_id) = view_id else {
            return changes;
        };
        let close = close_decision(kind, handled, outcome);
        // Only the view instance that was submitted: one reopened with the same
        // id since (another trigger's `*.open`) keeps its state.
        let same = |id: &str, g: u64| id == view_id && generation.is_none_or(|x| x == g);
        if let Some(m) = self.modal.as_mut().filter(|m| same(m.id(), m.generation)) {
            m.submitting = false;
            if close {
                self.modal = None;
                changes.push(UiKitChange::ModalClosed(view_id));
            }
        } else if let Some(i) = self.bars.iter().position(|b| same(b.id(), b.generation)) {
            self.bars[i].submitting = false;
            if close {
                let b = self.bars.remove(i);
                changes.push(UiKitChange::BarClosed {
                    rid: b.rid,
                    view_id,
                });
            }
        }
        changes
    }

    /// Applies a server interaction (HTTP body or `{uid}/uiInteraction` push).
    /// Ignored unless its trigger was issued here, is unexpired and unused.
    pub fn apply(&mut self, si: ServerInteraction, now: Instant) -> Vec<UiKitChange> {
        let Some(t) = self.triggers.accept(si.trigger_id(), now) else {
            tracing::debug!(
                kind = si.type_name(),
                "ignoring interaction without a valid trigger"
            );
            return Vec::new();
        };
        t.handled = Some(si.type_name());
        let origin = t.origin.clone();
        let trigger_app = t.app_id.clone().unwrap_or_default();
        let app_id = si.app_id().to_owned();
        let mut out = Vec::new();
        match si {
            ServerInteraction::ModalOpen { view, .. } => {
                let id = view.id().to_owned();
                let g = self.next_generation();
                match OpenView::new([&app_id, &trigger_app], *view, None, g) {
                    Some(m) => {
                        self.modal = Some(m);
                        out.push(UiKitChange::ModalOpened(id));
                    }
                    None => tracing::debug!("ignoring modal.open without an app id"),
                }
            }
            ServerInteraction::ModalUpdate { view, .. } => {
                if let Some(m) = self.modal.as_mut().filter(|m| m.id() == view.id()) {
                    let id = view.id().to_owned();
                    m.update(*view);
                    out.push(UiKitChange::ModalUpdated(id));
                }
            }
            ServerInteraction::ModalClose { view_id, .. } => {
                let matches = self.modal.as_ref().is_some_and(|m| match &view_id {
                    Some(id) => m.id() == id,
                    None => m.app_id == app_id,
                });
                if matches && let Some(m) = self.modal.take() {
                    out.push(UiKitChange::ModalClosed(m.id().to_owned()));
                }
            }
            ServerInteraction::ContextualBarOpen { view, .. } => {
                let rid = origin.rid.clone();
                let id = view.id().to_owned();
                let g = self.next_generation();
                let Some(bar) = OpenView::new([&app_id, &trigger_app], *view, rid.clone(), g)
                else {
                    tracing::debug!("ignoring contextual_bar.open without an app id");
                    return out;
                };
                // One bar per id and per room (one unplaced bar at most).
                let replaced = |b: &OpenView<ContextualBarView>| b.id() == id || b.rid == rid;
                for b in self.bars.iter().filter(|b| replaced(b)) {
                    // Same id in the same room is a replacement in place.
                    if !(b.id() == id && b.rid == rid) {
                        out.push(UiKitChange::BarClosed {
                            rid: b.rid.clone(),
                            view_id: b.id().to_owned(),
                        });
                    }
                }
                self.bars.retain(|b| !replaced(b));
                self.bars.push(bar);
                out.push(UiKitChange::BarOpened { rid, view_id: id });
            }
            ServerInteraction::ContextualBarUpdate { view, .. } => {
                let id = view.id().to_owned();
                if let Some(b) = self.bar_mut(&id) {
                    b.update(*view);
                    let rid = b.rid.clone();
                    out.push(UiKitChange::BarUpdated { rid, view_id: id });
                }
            }
            ServerInteraction::ContextualBarClose { view_id, .. } => {
                if let Some(i) = self.bars.iter().position(|b| b.id() == view_id) {
                    let b = self.bars.remove(i);
                    out.push(UiKitChange::BarClosed {
                        rid: b.rid,
                        view_id,
                    });
                }
            }
            ServerInteraction::Errors {
                view_id, errors, ..
            } => {
                // The view the trigger came from when it names this id;
                // otherwise the modal, then a bar.
                let from = (origin.view_id.as_deref() == Some(view_id.as_str()))
                    .then_some(origin.target)
                    .flatten();
                let in_modal = match from {
                    Some(t) => t == ViewTarget::Modal,
                    None => self.modal.as_ref().is_some_and(|m| m.id() == view_id),
                };
                let target = if in_modal {
                    self.modal
                        .as_mut()
                        .filter(|m| m.id() == view_id)
                        .map(|m| (ViewTarget::Modal, &mut m.errors))
                } else {
                    self.bars
                        .iter_mut()
                        .find(|b| b.id() == view_id)
                        .map(|b| (ViewTarget::Bar, &mut b.errors))
                };
                if let Some((t, e)) = target {
                    *e = errors;
                    out.push(UiKitChange::ErrorsSet(t, view_id));
                }
            }
            ServerInteraction::BannerOpen { view, .. } => {
                let id = view.id().to_owned();
                self.banners.retain(|b| b.view_id() != id);
                let app = Some(view.view.app_id.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or(app_id);
                self.banners.push(BannerEntry {
                    banner_id: None,
                    app_id: app,
                    surface: BannerSurface::Banner,
                    view: *view,
                    modal: None,
                });
                out.push(UiKitChange::BannerOpened(id));
            }
            ServerInteraction::BannerUpdate { view, .. } => {
                let id = view.id().to_owned();
                if let Some(b) = self.banners.iter_mut().find(|b| b.view_id() == id) {
                    b.view = *view;
                    out.push(UiKitChange::BannerUpdated(id));
                }
            }
            ServerInteraction::BannerClose { view_id, .. } => {
                let before = self.banners.len();
                self.banners.retain(|b| b.view_id() != view_id);
                if self.banners.len() != before {
                    out.push(UiKitChange::BannerClosed(view_id));
                }
            }
            ServerInteraction::Unknown(v) => {
                let kind = v
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                tracing::debug!(%kind, "unsupported app interaction");
                out.push(UiKitChange::Toast(UiKitToast::Unsupported(kind)));
            }
        }
        out
    }

    /// Records the room the UI shows an unplaced bar in (opened from the user
    /// menu, a modal or with no room open), so its interactions carry `rid`
    /// like the web's. Replaces another bar of that room. Empty when the bar is
    /// unknown or already placed.
    pub fn place_bar(&mut self, view_id: &str, rid: &str) -> Vec<UiKitChange> {
        let mut out = Vec::new();
        if !self
            .bars
            .iter()
            .any(|b| b.id() == view_id && b.rid.is_none())
        {
            return out;
        }
        self.bars.retain(|b| {
            let keep = b.rid.as_deref() != Some(rid);
            if !keep {
                out.push(UiKitChange::BarClosed {
                    rid: b.rid.clone(),
                    view_id: b.id().to_owned(),
                });
            }
            keep
        });
        if let Some(b) = self.bar_mut(view_id) {
            b.rid = Some(rid.to_owned());
        }
        out.push(UiKitChange::BarOpened {
            rid: Some(rid.to_owned()),
            view_id: view_id.to_owned(),
        });
        out
    }

    /// Moves bar `view_id`, opened for a room whose view has closed since,
    /// to room `rid` (the one shown; the web opens it in the current room).
    /// Replaces another bar of that room. Empty when the bar is unknown or
    /// already there.
    pub fn move_bar(&mut self, view_id: &str, rid: &str) -> Vec<UiKitChange> {
        let mut out = Vec::new();
        if !self
            .bars
            .iter()
            .any(|b| b.id() == view_id && b.rid.as_deref() != Some(rid))
        {
            return out;
        }
        self.bars.retain(|b| {
            let keep = b.id() == view_id || b.rid.as_deref() != Some(rid);
            if !keep {
                out.push(UiKitChange::BarClosed {
                    rid: b.rid.clone(),
                    view_id: b.id().to_owned(),
                });
            }
            keep
        });
        if let Some(b) = self.bar_mut(view_id) {
            b.rid = Some(rid.to_owned());
        }
        out.push(UiKitChange::BarOpened {
            rid: Some(rid.to_owned()),
            view_id: view_id.to_owned(),
        });
        out
    }

    /// The room view `rid` closed: its app panels go away without `viewClosed`
    /// (reported as `BarClosed`).
    pub fn close_room_bars(&mut self, rid: &str) -> Vec<UiKitChange> {
        let mut out = Vec::new();
        self.bars.retain(|b| {
            let keep = b.rid.as_deref() != Some(rid);
            if !keep {
                out.push(UiKitChange::BarClosed {
                    rid: b.rid.clone(),
                    view_id: b.id().to_owned(),
                });
            }
            keep
        });
        out
    }

    /// Replaces the banners from `GET /api/v1/banners?platform=web` (keeps
    /// banners opened by interactions). Views without `appId` belong to
    /// `cloud-announcements-core`; dictionaries are merged into that app's
    /// namespace. A modal-surface announcement opens as the store modal when
    /// none is open (state, submit and close then work like any app modal);
    /// otherwise it waits for [`Self::open_announcement_modal`].
    pub fn set_banners(&mut self, banners: Vec<Banner>) -> Vec<UiKitChange> {
        self.banners.retain(|b| b.banner_id.is_none());
        self.announced
            .retain(|id| banners.iter().any(|b| &b.id == id));
        for b in banners {
            if b.active == Some(false)
                || (b.surface == BannerSurface::Modal && self.announced.contains(&b.id))
            {
                continue;
            }
            let mut raw = b.view.clone();
            if let Value::Object(o) = &mut raw
                && o.get("appId")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                o.insert(
                    "appId".into(),
                    Value::String(CLOUD_ANNOUNCEMENTS_APP.into()),
                );
            }
            let dict = b.dictionary_map();
            if !dict.is_empty() {
                self.translations.merge_app(CLOUD_ANNOUNCEMENTS_APP, dict);
            }
            let modal = (b.surface == BannerSurface::Modal)
                .then(|| RawView::<ModalView>::from_value(raw.clone()))
                .flatten();
            let Some(mut view) = RawView::<BannerView>::from_value(raw) else {
                continue;
            };
            if view.view.view_id.is_empty() {
                // Modal announcements carry `id`; banners `viewId`.
                view.view.view_id = modal
                    .as_ref()
                    .map(|m| m.view.id.clone())
                    .filter(|i| !i.is_empty())
                    .unwrap_or_else(|| b.id.clone());
            }
            self.banners.push(BannerEntry {
                banner_id: Some(b.id),
                app_id: view.view.app_id.clone(),
                surface: b.surface,
                view,
                modal,
            });
        }
        self.open_announcement_modal().into_iter().collect()
    }

    /// Opens the next pending modal-surface announcement as the store modal
    /// when no modal is open (call after a modal closes). The announcement
    /// leaves the banner list for the session (later refetches skip it while
    /// the server still lists it); closing it sends `viewClosed` to its app,
    /// which dismisses it server-side.
    pub fn open_announcement_modal(&mut self) -> Option<UiKitChange> {
        if self.modal.is_some() {
            return None;
        }
        let i = self
            .banners
            .iter()
            .position(|b| b.surface == BannerSurface::Modal && b.modal.is_some())?;
        let b = self.banners.remove(i);
        if let Some(banner_id) = &b.banner_id {
            self.announced.insert(banner_id.clone());
        }
        let view = b.modal?;
        let id = view.id().to_owned();
        let g = self.next_generation();
        self.modal = Some(OpenView::new(
            [b.app_id.as_str(), CLOUD_ANNOUNCEMENTS_APP],
            view,
            None,
            g,
        )?);
        Some(UiKitChange::ModalOpened(id))
    }

    /// Reacts to a `stream-apps` event; returns what to refetch.
    pub fn apply_apps_event(&mut self, ev: &AppsStreamEvent) -> AppsFollowup {
        match ev {
            AppsStreamEvent::ActionsChanged => AppsFollowup {
                action_buttons: true,
                ..Default::default()
            },
            e if e.affects_commands() => AppsFollowup {
                commands: true,
                ..Default::default()
            },
            AppsStreamEvent::Added(_) => AppsFollowup {
                languages: true,
                admin: true,
                ..Default::default()
            },
            AppsStreamEvent::Removed(id) => {
                self.translations.remove_app(id);
                self.buttons.retain(|b| &b.app_id != id);
                self.bump();
                AppsFollowup {
                    admin: true,
                    ..Default::default()
                }
            }
            AppsStreamEvent::StatusUpdate { app_id, status } => {
                if !status.is_enabled() {
                    self.buttons.retain(|b| &b.app_id != app_id);
                    self.bump();
                }
                AppsFollowup {
                    admin: true,
                    ..Default::default()
                }
            }
            AppsStreamEvent::Updated(_) | AppsStreamEvent::SettingUpdated { .. } => AppsFollowup {
                admin: true,
                ..Default::default()
            },
            _ => AppsFollowup::default(),
        }
    }

    /// Forgets expired triggers (call now and then).
    /// Requests whose trigger expired no longer count as busy.
    pub fn prune(&mut self, now: Instant) {
        self.triggers.prune(now);
        let triggers = &self.triggers;
        self.busy.retain(|t| triggers.get(t).is_some());
    }
}

#[cfg(test)]
mod tests;
