//! Turns UIKit messages into store updates and REST calls (docs/uikit.md
//! §4.6): issues interactions through `UiKitStore` (trigger registry, trust
//! rule, busy count), sends them with `RestClient::ui_interaction`, feeds the
//! answers back and reports what the app has to do ([`Effect`]). No iced
//! state here: the app glue runs the returned tasks and effects.

use std::time::{Duration, Instant};

use iced::Task;
use iced::widget::text_editor;
use rocketchat::RestClient;
use rocketchat::models::Message as RcMessage;
use rocketchat::uikit::{Element as El, LayoutBlock, UserInteraction};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::room_list::Avatar;
use rustket_core::uikit::{InteractionResult, UiKitChange, UiKitToast, ViewTarget};
use rustket_ui::widget::BannerKind;
use serde_json::Value;

use super::elements::{parse_date, parse_time};
use super::{ActionRequest, Local, Msg, SessionHook, Suggestion, SurfaceKey, Typed, UiKitMsg};
use crate::error::Failure;
use crate::room::render::RenderMsg;

/// Debounce of `on_character_entered` inputs in views (§5.2).
pub const TEXT_DEBOUNCE: Duration = Duration::from_millis(700);
/// Debounce of `actions/changed` (§6.1).
pub const BUTTONS_DEBOUNCE: Duration = Duration::from_millis(100);

/// A timer that needs a runtime only when it runs.
async fn sleep(d: Duration) {
    tokio::time::sleep(d).await;
}

/// What the controller works on.
pub struct Ctx<'a> {
    pub ws: &'a mut Workspace,
    pub local: &'a mut Local,
    pub rest: Option<RestClient>,
    /// The room shown (bars opened without a room go there).
    pub active_rid: Option<&'a str>,
}

/// What the app has to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Toast(BannerKind, String, String),
    OpenUrl(String),
    /// Show the contextual bar `view_id` in room `rid`'s panel.
    ShowBar {
        rid: String,
        view_id: String,
    },
    /// Close the panel of room `rid` if it shows bar `view_id`.
    HideBar {
        rid: String,
        view_id: String,
    },
    /// The token was refused: re-login.
    Relogin,
    /// Focus the text input with this id (scratch key).
    Focus(String),
    /// A `@user` / `#channel` in a view.
    OpenUser(String),
    OpenRoom(String),
    /// Installed apps changed (`stream-apps`): `commands` = the slash command
    /// list is stale, `admin` = an apps admin list should refresh.
    AppsChanged {
        commands: bool,
        admin: bool,
    },
}

/// Debounce key of the action-buttons refresh.
const BUTTONS_TIMER: &str = "#buttons";

/// Debounce key of the autocomplete query of element `key`.
fn query_timer(key: &str) -> String {
    format!("{key}#query")
}

/// Result of an update.
pub struct Out {
    pub task: Task<Msg>,
    pub effects: Vec<Effect>,
}

impl Out {
    pub fn none() -> Self {
        Self {
            task: Task::none(),
            effects: Vec::new(),
        }
    }

    fn task(task: Task<Msg>) -> Self {
        Self {
            task,
            effects: Vec::new(),
        }
    }

    fn effect(e: Effect) -> Self {
        Self {
            task: Task::none(),
            effects: vec![e],
        }
    }

    fn merge(mut self, other: Out) -> Self {
        self.task = Task::batch([self.task, other.task]);
        self.effects.extend(other.effects);
        self
    }
}

/// Handles one controller message.
pub fn update(ctx: &mut Ctx<'_>, msg: Msg) -> Out {
    let now = Instant::now();
    match msg {
        Msg::Ui { msg, .. } => {
            let message_surface = msg_surface(&msg)
                .filter(|s| matches!(s, SurfaceKey::Message { .. }))
                .map(SurfaceKey::prefix);
            let out = ui(ctx, msg);
            if let Some(prefix) = message_surface {
                ctx.ws.uikit_mut().scratch_mut().trim_messages(&prefix);
            }
            out
        }
        Msg::CommandRan(ran) => {
            super::commands::finished(ctx.ws, &ran);
            ctx.ws.uikit_mut().prune(now);
            Out::none()
        }
        Msg::Finished { trigger, result } => {
            ctx.ws.uikit_mut().scratch_mut().settle(&trigger);
            let changes = ctx.ws.uikit_mut().finish(&trigger, result, now);
            ctx.ws.uikit_mut().prune(now);
            apply_changes(ctx, changes)
        }
        Msg::Debounced { req, stamp } => {
            let key = req.surface.element(&req.action_id);
            if !ctx.ws.uikit_mut().scratch_mut().take_stamp(&key, stamp) {
                return Out::none();
            }
            emit_action(ctx, req)
        }
        Msg::QueryDue { req, users, stamp } => {
            let key = req.surface.element(&req.action_id);
            if !ctx
                .ws
                .uikit_mut()
                .scratch_mut()
                .take_stamp(&query_timer(&key), stamp)
            {
                return Out::none();
            }
            let term = ctx
                .ws
                .uikit()
                .scratch()
                .texts
                .get(&key)
                .cloned()
                .unwrap_or_default();
            let Some(rest) = ctx.rest.clone() else {
                return Out::none();
            };
            let asked = term.clone();
            Out::task(Task::perform(
                async move { suggestions(rest, users, asked).await.map_err(Failure::from) },
                move |result| Msg::Suggestions {
                    key: key.clone(),
                    term: term.clone(),
                    result,
                },
            ))
        }
        Msg::Suggestions { key, term, result } => {
            let s = ctx.ws.uikit_mut().scratch_mut();
            // Only the answer for what the field holds now (an older query
            // may answer later; a cleared field or closed view has no text).
            if s.texts.get(&key) != Some(&term) {
                return Out::none();
            }
            match result {
                Ok(list) => {
                    s.suggestions.insert(key.clone(), list);
                    if s.open.is_none() {
                        s.open = Some(key);
                    }
                }
                Err(e) => tracing::debug!(error = %e, "autocomplete failed"),
            }
            Out::none()
        }
        Msg::RefreshButtons(stamp) => {
            if !ctx
                .ws
                .uikit_mut()
                .scratch_mut()
                .take_stamp(BUTTONS_TIMER, stamp)
            {
                return Out::none();
            }
            fetch_buttons(ctx.rest.clone())
        }
        Msg::Buttons(r) => {
            match r {
                Ok(list) => ctx.ws.uikit_mut().set_buttons(list),
                Err(e) => tracing::debug!(error = %e, "actionButtons failed"),
            }
            Out::none()
        }
        Msg::Banners(r) => match r {
            Ok(list) => {
                let changes = ctx.ws.uikit_mut().set_banners(list);
                apply_changes(ctx, changes)
            }
            Err(e) => {
                tracing::debug!(error = %e, "banners failed");
                Out::none()
            }
        },
        Msg::Languages(r) => {
            match r {
                Ok(list) => ctx.ws.uikit_mut().translations_mut().set_all(list),
                Err(e) => tracing::debug!(error = %e, "apps languages failed"),
            }
            Out::none()
        }
    }
}

/// Fetches what the UIKit needs after login / reconnect: action buttons, app
/// translations and banners.
pub fn bootstrap(rest: Option<RestClient>) -> Task<Msg> {
    let Some(rest) = rest else {
        return Task::none();
    };
    let (r2, r3) = (rest.clone(), rest.clone());
    Task::batch([
        fetch_buttons(Some(rest)).task,
        Task::perform(
            async move { r2.apps_languages().await.map_err(Failure::from) },
            Msg::Languages,
        ),
        Task::perform(
            async move { r3.banners().await.map_err(Failure::from) },
            Msg::Banners,
        ),
    ])
}

fn fetch_buttons(rest: Option<RestClient>) -> Out {
    let Some(rest) = rest else {
        return Out::none();
    };
    Out::task(Task::perform(
        async move { rest.action_buttons().await.map_err(Failure::from) },
        Msg::Buttons,
    ))
}

/// A realtime event for the UIKit (see [`SessionHook`]).
pub fn session(ctx: &mut Ctx<'_>, hook: SessionHook) -> Out {
    match hook {
        SessionHook::Interaction(si) => {
            let changes = ctx.ws.uikit_mut().apply(*si, Instant::now());
            apply_changes(ctx, changes)
        }
        SessionHook::Apps(ev) => {
            let f = ctx.ws.uikit_mut().apply_apps_event(&ev);
            let mut out = Out::none();
            if f.commands || f.admin {
                out.effects.push(Effect::AppsChanged {
                    commands: f.commands,
                    admin: f.admin,
                });
            }
            if f.action_buttons {
                let stamp = ctx.ws.uikit_mut().scratch_mut().stamp(BUTTONS_TIMER);
                out = out.merge(Out::task(Task::perform(
                    sleep(BUTTONS_DEBOUNCE),
                    move |_| Msg::RefreshButtons(stamp),
                )));
            }
            if f.languages
                && let Some(rest) = ctx.rest.clone()
            {
                out = out.merge(Out::task(Task::perform(
                    async move { rest.apps_languages().await.map_err(Failure::from) },
                    Msg::Languages,
                )));
            }
            out
        }
        SessionHook::Banners => match ctx.rest.clone() {
            Some(rest) => Out::task(Task::perform(
                async move { rest.banners().await.map_err(Failure::from) },
                Msg::Banners,
            )),
            None => Out::none(),
        },
    }
}

async fn suggestions(
    rest: RestClient,
    users: bool,
    term: String,
) -> rocketchat::Result<Vec<Suggestion>> {
    if users {
        Ok(rest
            .users_autocomplete(&term)
            .await?
            .into_iter()
            .filter_map(|u| {
                let username = u.username.filter(|n| !n.is_empty())?;
                Some(Suggestion {
                    label: u
                        .name
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| username.clone()),
                    detail: Some(format!("({username})")),
                    avatar: Some(Avatar::User {
                        username: username.clone(),
                    }),
                    value: username,
                })
            })
            .collect())
    } else {
        Ok(rest
            .rooms_autocomplete_channel_and_private(&term)
            .await?
            .into_iter()
            .map(|r| Suggestion {
                label: r
                    .name
                    .clone()
                    .or_else(|| r.fname.clone())
                    .unwrap_or_else(|| r.id.clone()),
                detail: None,
                avatar: Some(Avatar::Room {
                    rid: r.id.clone(),
                    etag: r.avatar_etag.clone(),
                }),
                value: r.id,
            })
            .collect())
    }
}

/// Stores a changed value: view state for modals and bars, the local widget
/// value for messages and banners (banner `state` keeps the initial values).
fn store_value(ws: &mut Workspace, req: &ActionRequest, value: Value) {
    let block = Some(req.block_id.as_str()).filter(|b| !b.is_empty());
    let key = req.surface.element(&req.action_id);
    let store = ws.uikit_mut();
    if let Some((target, id)) = req.surface.view_target()
        && let Some(state) = store.view_state_mut(target, id)
    {
        state.set(&req.action_id, block, value);
        return;
    }
    store.scratch_mut().values.insert(key, value);
}

/// The value [`store_value`] holds for `req`'s element.
fn current_value<'w>(ws: &'w Workspace, req: &ActionRequest) -> Option<&'w Value> {
    let store = ws.uikit();
    if let Some((target, id)) = req.surface.view_target() {
        let state = match target {
            ViewTarget::Modal => store.modal().filter(|m| m.id() == id).map(|m| &m.state),
            ViewTarget::Bar => store.bar(id).map(|b| &b.state),
        };
        if let Some(state) = state {
            return state.get(&req.action_id);
        }
    }
    store
        .scratch()
        .values
        .get(&req.surface.element(&req.action_id))
}

fn ui(ctx: &mut Ctx<'_>, msg: UiKitMsg) -> Out {
    let now = Instant::now();
    match msg {
        UiKitMsg::Change { req, emit, confirm } => {
            let key = req.surface.element(&req.action_id);
            {
                let s = ctx.ws.uikit_mut().scratch_mut();
                if s.open
                    .as_deref()
                    .is_some_and(|o| o.starts_with(&key) && !o.ends_with("#multi"))
                {
                    s.open = None;
                    s.filter.clear();
                }
                if req.value.is_none() && req.url.is_none() {
                    // A tab (the only valueless element without a URL).
                    let tabs = format!("{}#tabs{}", req.surface.prefix(), req.block_id);
                    s.tabs.insert(tabs, req.action_id.clone());
                }
                s.texts.remove(&key);
                // A chosen user / channel keeps its label for the chip.
                if let Some(list) = s.suggestions.remove(&key) {
                    for v in values_of(req.value.as_ref()) {
                        if let Some(sug) = list.iter().find(|x| x.value == v) {
                            s.labels.insert(format!("{key}|{v}"), sug.label.clone());
                        }
                    }
                    if s.open.as_deref() == Some(key.as_str()) {
                        s.open = None;
                    }
                }
            }
            if let Some(v) = req.value.clone() {
                store_value(ctx.ws, &req, v);
            }
            if !emit {
                return Out::none();
            }
            if let Some(c) = confirm {
                ctx.ws.uikit_mut().scratch_mut().confirm = Some((*c, req));
                return Out::none();
            }
            emit_action(ctx, req)
        }
        UiKitMsg::Toggle {
            mut req,
            option,
            emit: send,
        } => {
            if let Some(v) = req.value.clone() {
                store_value(ctx.ws, &req, v);
            }
            if !send {
                return Out::none();
            }
            req.value = Some(Value::String(option));
            emit_action(ctx, req)
        }
        UiKitMsg::Text { req, dispatch } => text_changed(ctx, req, dispatch),
        UiKitMsg::Editor {
            mut req,
            dispatch,
            action,
        } => {
            let key = req.surface.element(&req.action_id);
            let content = ctx.local.editors.entry(key).or_default();
            let edits = action.is_edit();
            content.perform(action);
            if !edits {
                return Out::none();
            }
            req.value = Some(Value::String(content.text()));
            text_changed(ctx, req, dispatch)
        }
        UiKitMsg::Typing {
            mut req,
            text,
            emit: send,
            kind,
        } => {
            let key = req.surface.element(&req.action_id);
            if kind == Typed::Month {
                ctx.ws
                    .uikit_mut()
                    .scratch_mut()
                    .texts
                    .insert(format!("{key}#month"), text);
                return Out::none();
            }
            let valid = match kind {
                Typed::Date => parse_date(&text),
                _ => parse_time(&text).map(|(h, m)| format!("{h:02}:{m:02}")),
            };
            ctx.ws.uikit_mut().scratch_mut().texts.insert(key, text);
            // A cleared or not (yet) valid field holds no value, like the
            // web's native date/time input (""): submitting must not send the
            // previous value the field no longer shows.
            let value = Value::String(valid.unwrap_or_default());
            if current_value(ctx.ws, &req) == Some(&value) {
                return Out::none();
            }
            store_value(ctx.ws, &req, value.clone());
            if !send {
                return Out::none();
            }
            req.value = Some(value);
            emit_action(ctx, req)
        }
        UiKitMsg::Popover(key) => {
            let s = ctx.ws.uikit_mut().scratch_mut();
            s.open = key;
            s.filter.clear();
            Out::none()
        }
        UiKitMsg::Filter(f) => {
            ctx.ws.uikit_mut().scratch_mut().filter = f;
            Out::none()
        }
        UiKitMsg::Query { req, text, users } => {
            let key = req.surface.element(&req.action_id);
            let s = ctx.ws.uikit_mut().scratch_mut();
            s.texts.insert(key.clone(), text.clone());
            let stamp = s.stamp(&query_timer(&key));
            if text.trim().is_empty() {
                s.suggestions.remove(&key);
                s.stamps.remove(&query_timer(&key));
                return Out::none();
            }
            let delay = Duration::from_millis(300);
            Out::task(Task::perform(sleep(delay), move |_| Msg::QueryDue {
                req: req.clone(),
                users,
                stamp,
            }))
        }
        UiKitMsg::ShowMore(key) => {
            ctx.ws.uikit_mut().scratch_mut().expanded.insert(key);
            Out::none()
        }
        UiKitMsg::Submit(surface) => {
            let Some((target, id)) = surface.view_target() else {
                return Out::none();
            };
            match ctx.ws.uikit_mut().view_submit(target, id, now) {
                Some((app, i)) => send(ctx.rest.clone(), app, i),
                None => Out::none(),
            }
        }
        UiKitMsg::Close { surface, cleared } => close(ctx, &surface, cleared),
        UiKitMsg::Confirmed(yes) => match ctx.ws.uikit_mut().scratch_mut().confirm.take() {
            Some((_, req)) if yes => emit_action(ctx, req),
            _ => Out::none(),
        },
        UiKitMsg::Button {
            button,
            rid,
            tmid,
            mid,
            message,
        } => {
            ctx.ws.uikit_mut().scratch_mut().open = None;
            let (app, i) = ctx.ws.uikit_mut().action_button(
                &button,
                rid.as_deref(),
                tmid.as_deref(),
                mid.as_deref(),
                message.as_deref(),
                now,
            );
            send(ctx.rest.clone(), app, i)
        }
        UiKitMsg::Command {
            rid,
            tmid,
            command,
            params,
            app_id,
        } => Out::task(
            super::commands::run(ctx.ws, ctx.rest.clone(), rid, tmid, command, params, app_id)
                .map(Msg::CommandRan),
        ),
        UiKitMsg::Render(r) => match *r {
            RenderMsg::OpenUrl(u) => Out::effect(Effect::OpenUrl(u)),
            RenderMsg::OpenUser(u) => Out::effect(Effect::OpenUser(u)),
            RenderMsg::OpenRoom(r) => Out::effect(Effect::OpenRoom(r)),
            _ => Out::none(),
        },
    }
}

/// The surface a UI message acts on, if it names one.
fn msg_surface(msg: &UiKitMsg) -> Option<&SurfaceKey> {
    match msg {
        UiKitMsg::Change { req, .. }
        | UiKitMsg::Toggle { req, .. }
        | UiKitMsg::Text { req, .. }
        | UiKitMsg::Editor { req, .. }
        | UiKitMsg::Typing { req, .. }
        | UiKitMsg::Query { req, .. } => Some(&req.surface),
        _ => None,
    }
}

fn values_of(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn text_changed(ctx: &mut Ctx<'_>, req: ActionRequest, dispatch: bool) -> Out {
    let Some(value) = req.value.clone() else {
        return Out::none();
    };
    let key = req.surface.element(&req.action_id);
    if let Value::String(s) = &value {
        ctx.ws
            .uikit_mut()
            .scratch_mut()
            .texts
            .insert(key, s.clone());
    }
    store_value(ctx.ws, &req, value);
    if !dispatch {
        return Out::none();
    }
    if matches!(req.surface, SurfaceKey::Message { .. }) {
        // Messages emit on every change, without debounce (§5.2).
        return emit_action(ctx, req);
    }
    let key = req.surface.element(&req.action_id);
    let stamp = ctx.ws.uikit_mut().scratch_mut().stamp(&key);
    Out::task(Task::perform(sleep(TEXT_DEBOUNCE), move |_| {
        Msg::Debounced {
            req: req.clone(),
            stamp,
        }
    }))
}

/// The message of a message surface, from the room or thread timeline.
pub fn find_message(ws: &Workspace, rid: &str, mid: &str, tmid: Option<&str>) -> Option<RcMessage> {
    message_ref(ws, rid, mid, tmid).cloned()
}

fn message_ref<'w>(
    ws: &'w Workspace,
    rid: &str,
    mid: &str,
    tmid: Option<&str>,
) -> Option<&'w RcMessage> {
    if let Some(i) = ws.timeline(rid).and_then(|t| t.get(mid)) {
        return Some(&i.message);
    }
    let tl = tmid.and_then(|t| ws.thread(t))?;
    if let Some(i) = tl.get(mid) {
        return Some(&i.message);
    }
    tl.parent().filter(|p| p.id == mid)
}

/// Whether block actions of message `mid` can be sent: it must be in the
/// loaded room or thread timeline (lists such as search results or pinned
/// messages show other messages read-only).
pub fn knows_message(ws: &Workspace, rid: &str, mid: &str, tmid: Option<&str>) -> bool {
    message_ref(ws, rid, mid, tmid).is_some()
}

/// Builds the interaction of `req` through the store (trust rule, trigger,
/// busy count). `None` when nothing may be sent.
pub fn interaction(
    ws: &mut Workspace,
    req: &ActionRequest,
    now: Instant,
) -> Option<(String, UserInteraction)> {
    let app = req.app_id.as_deref();
    match &req.surface {
        SurfaceKey::Message { rid, mid, tmid } => {
            let msg = find_message(ws, rid, mid, tmid.as_deref())?;
            let eph = ws.is_ephemeral(rid, mid);
            ws.uikit_mut().message_block_action(
                &msg,
                eph,
                req.in_video_conf,
                app,
                &req.block_id,
                &req.action_id,
                req.value.clone(),
                now,
            )
        }
        SurfaceKey::Modal(id) => ws.uikit_mut().view_block_action(
            ViewTarget::Modal,
            id,
            app,
            &req.block_id,
            &req.action_id,
            req.value.clone(),
            now,
        ),
        SurfaceKey::Bar(id) => ws.uikit_mut().view_block_action(
            ViewTarget::Bar,
            id,
            app,
            &req.block_id,
            &req.action_id,
            req.value.clone(),
            now,
        ),
        SurfaceKey::Banner(id) => ws
            .uikit_mut()
            .banner_block_action(
                id,
                app,
                &req.block_id,
                &req.action_id,
                req.value.clone(),
                now,
            )
            .map(|(a, i, _)| (a, i)),
    }
}

/// Emits a block action (opening its URL first, §2.4).
fn emit_action(ctx: &mut Ctx<'_>, req: ActionRequest) -> Out {
    let mut out = Out::none();
    if let Some(u) = req.url.clone() {
        out.effects.push(Effect::OpenUrl(u));
    }
    let Some((app, i)) = interaction(ctx.ws, &req, Instant::now()) else {
        tracing::debug!(action = %req.action_id, "block action not sent (not allowed)");
        return out;
    };
    let key = req.surface.element(&req.action_id);
    ctx.ws
        .uikit_mut()
        .scratch_mut()
        .loading
        .insert(key, i.trigger_id().to_owned());
    if let SurfaceKey::Banner(id) = &req.surface {
        let prefix = SurfaceKey::Banner(id.clone()).prefix();
        ctx.ws.uikit_mut().scratch_mut().forget(&prefix);
    }
    out.merge(send(ctx.rest.clone(), app, i))
}

fn close(ctx: &mut Ctx<'_>, surface: &SurfaceKey, cleared: bool) -> Out {
    let now = Instant::now();
    let Some(id) = surface.view_id() else {
        return Out::none();
    };
    let built = match surface.view_target() {
        Some((target, _)) => ctx.ws.uikit_mut().view_closed(target, id, cleared, now),
        None => ctx.ws.uikit_mut().banner_closed(id, now),
    };
    let Some((app, i, change)) = built else {
        return Out::none();
    };
    let sent = send(ctx.rest.clone(), app, i);
    apply_changes(ctx, vec![change]).merge(sent)
}

/// Sends `interaction` to `app`; the answer comes back as `Msg::Finished`.
pub fn send(rest: Option<RestClient>, app: String, interaction: UserInteraction) -> Out {
    let trigger = interaction.trigger_id().to_owned();
    let Some(rest) = rest else {
        return Out::task(Task::done(Msg::Finished {
            trigger,
            result: InteractionResult::Failed("not connected".into()),
        }));
    };
    Out::task(Task::perform(
        async move { InteractionResult::from(rest.ui_interaction(&app, &interaction).await) },
        move |result| Msg::Finished {
            trigger: trigger.clone(),
            result,
        },
    ))
}

/// The first single-line text input of `blocks` (with an error when
/// `errors` is given) as a focus target.
fn first_input(
    blocks: &[LayoutBlock],
    errors: Option<&std::collections::HashMap<String, String>>,
) -> Option<String> {
    let blocks = rocketchat::uikit::resolve_conditionals(blocks, rocketchat::uikit::ENGINE);
    blocks.iter().find_map(|b| match b {
        LayoutBlock::Input { meta, element, .. } => match element.inherited(meta) {
            El::PlainTextInput {
                a,
                multiline: false,
                ..
            } if errors.is_none_or(|e| e.contains_key(&a.action_id)) => Some(a.action_id),
            _ => None,
        },
        _ => None,
    })
}

/// Creates the editor contents of the modal's multiline inputs.
fn sync_editors(ws: &Workspace, local: &mut Local) {
    let Some(m) = ws.uikit().modal() else {
        return;
    };
    let surface = SurfaceKey::Modal(m.id().to_owned());
    for b in rocketchat::uikit::resolve_conditionals(&m.view.view.blocks, rocketchat::uikit::ENGINE)
    {
        if let LayoutBlock::Input { meta, element, .. } = &b
            && let El::PlainTextInput {
                a, multiline: true, ..
            } = element.inherited(meta)
        {
            let text = m
                .state
                .get(&a.action_id)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            local
                .editors
                .entry(surface.element(&a.action_id))
                .or_insert_with(|| text_editor::Content::with_text(&text));
        }
    }
}

/// The view of room `rid` closed: its app panels go away without
/// `viewClosed` (§5.6), with their widget scratch.
pub fn room_closed(ws: &mut Workspace, rid: &str) {
    for c in ws.uikit_mut().close_room_bars(rid) {
        if let UiKitChange::BarClosed { view_id, .. } = c {
            ws.uikit_mut()
                .scratch_mut()
                .forget(&SurfaceKey::Bar(view_id).prefix());
        }
    }
}

/// Reports store changes as effects (and keeps the scratch/editors tidy).
pub fn apply_changes(ctx: &mut Ctx<'_>, changes: Vec<UiKitChange>) -> Out {
    let mut out = Out::none();
    for c in changes {
        match c {
            UiKitChange::ModalOpened(id) => {
                let prefix = SurfaceKey::Modal(id.clone()).prefix();
                ctx.local.editors.retain(|k, _| !k.starts_with("v:"));
                ctx.ws.uikit_mut().scratch_mut().forget(&prefix);
                sync_editors(ctx.ws, ctx.local);
                if let Some(m) = ctx.ws.uikit().modal()
                    && let Some(a) = first_input(&m.view.view.blocks, None)
                {
                    out.effects
                        .push(Effect::Focus(SurfaceKey::Modal(id).element(&a)));
                }
            }
            UiKitChange::ModalUpdated(_) => sync_editors(ctx.ws, ctx.local),
            UiKitChange::ModalClosed(id) => {
                let prefix = SurfaceKey::Modal(id).prefix();
                ctx.local.editors.retain(|k, _| !k.starts_with(&prefix));
                ctx.ws.uikit_mut().scratch_mut().forget(&prefix);
                if let Some(next) = ctx.ws.uikit_mut().open_announcement_modal() {
                    out = out.merge(apply_changes(ctx, vec![next]));
                }
            }
            UiKitChange::BarOpened { rid, view_id } => match (rid, ctx.active_rid) {
                (Some(rid), Some(active)) if rid != active => {
                    // Its room's view closed while the app answered (§4.6:
                    // origin room, else the active room).
                    let active = active.to_owned();
                    let moved = ctx.ws.uikit_mut().move_bar(&view_id, &active);
                    out = out.merge(apply_changes(ctx, moved));
                }
                (Some(rid), None) => {
                    // No room view to show it in: drop it instead of keeping
                    // it hidden in the store.
                    let closed = ctx.ws.uikit_mut().close_room_bars(&rid);
                    out = out.merge(apply_changes(ctx, closed));
                    out.effects.push(Effect::Toast(
                        BannerKind::Info,
                        t!("uikit.bar_needs_room").into_owned(),
                        String::new(),
                    ));
                }
                (Some(rid), _) => out.effects.push(Effect::ShowBar { rid, view_id }),
                (None, Some(active)) => {
                    let active = active.to_owned();
                    let placed = ctx.ws.uikit_mut().place_bar(&view_id, &active);
                    out = out.merge(apply_changes(ctx, placed));
                }
                (None, None) => out.effects.push(Effect::Toast(
                    BannerKind::Info,
                    t!("uikit.bar_needs_room").into_owned(),
                    String::new(),
                )),
            },
            UiKitChange::BarClosed { rid, view_id } => {
                ctx.ws
                    .uikit_mut()
                    .scratch_mut()
                    .forget(&SurfaceKey::Bar(view_id.clone()).prefix());
                if let Some(rid) = rid {
                    out.effects.push(Effect::HideBar { rid, view_id });
                }
            }
            UiKitChange::ErrorsSet(target, id) => {
                let store = ctx.ws.uikit();
                let (blocks, errors, surface) = match target {
                    ViewTarget::Modal => match store.modal().filter(|m| m.id() == id) {
                        Some(m) => (
                            &m.view.view.blocks,
                            &m.errors,
                            SurfaceKey::Modal(id.clone()),
                        ),
                        None => continue,
                    },
                    ViewTarget::Bar => match store.bar(&id) {
                        Some(b) => (&b.view.view.blocks, &b.errors, SurfaceKey::Bar(id.clone())),
                        None => continue,
                    },
                };
                if let Some(a) = first_input(blocks, Some(errors)) {
                    out.effects.push(Effect::Focus(surface.element(&a)));
                }
            }
            UiKitChange::Toast(t) => out.effects.push(toast(&t)),
            UiKitChange::Unauthorized => out.effects.push(Effect::Relogin),
            UiKitChange::BarUpdated { .. }
            | UiKitChange::BannerOpened(_)
            | UiKitChange::BannerUpdated(_)
            | UiKitChange::BannerClosed(_) => {}
        }
    }
    out
}

fn toast(t: &UiKitToast) -> Effect {
    match t {
        UiKitToast::Timeout => Effect::Toast(
            BannerKind::Error,
            t!("uikit.timeout").into_owned(),
            String::new(),
        ),
        UiKitToast::AppError(e) => Effect::Toast(
            BannerKind::Error,
            t!("uikit.app_error").into_owned(),
            e.clone(),
        ),
        UiKitToast::RateLimited(after) => Effect::Toast(
            BannerKind::Warning,
            t!("uikit.rate_limited").into_owned(),
            after
                .map(|d| t!("uikit.retry_in", seconds = d.as_secs().max(1)).into_owned())
                .unwrap_or_default(),
        ),
        UiKitToast::Unsupported(kind) => Effect::Toast(
            BannerKind::Warning,
            t!("uikit.unsupported").into_owned(),
            kind.clone(),
        ),
    }
}
