//! Video conferences (docs/teams-videoconf.md §7): the "Video call" header
//! action, `video_conf` message blocks, direct-call ringing (popups, sounds,
//! desktop notifications) and the provider error dialog. Calls always open in
//! the **external** browser; Rustket never hosts the call.
//!
//! App-level state lives in [`Calls`] (one [`CallManager`] per server); the
//! app forwards session events ([`on_session_event`]), drains the block info
//! requests after each update ([`fetch_wanted`]) and routes [`Message`]s to
//! [`update`]. Strings: `locales/videoconf.yml` (`videoconf.*`).

pub mod block;
pub mod notify;
pub mod view;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::{Subscription, Task};
use rocketchat::models::{
    JoinResult, ProviderCapabilities, StartInstructions, VideoConfAction, VideoConfCapabilities,
    VideoConference,
};
use rocketchat::rest::videoconf::provider_error;
use rocketchat::session::SessionEvent;
use rocketchat::{RestClient, ServerUrl};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::videoconf::{CallCommand, CallManager, CallNotice};
use rustket_ui::widget::BannerKind;

use crate::app::App;
use crate::component::Effect;
use crate::error::{Failure, Res};
use crate::session::SessionHandle;

/// The header "Video call" button of `rid` (§7.9): `None` when hidden,
/// disabled with a tooltip for federated, read-only or archived rooms.
pub fn header_button<'a, M: Clone + 'a>(
    ws: &rustket_core::Workspace,
    rid: &str,
    on_press: M,
) -> Option<iced::Element<'a, M>> {
    use rustket_core::teams::{CallButton, video_call_button};
    let (enabled, tip) = match video_call_button(ws, rid) {
        CallButton::Hidden => return None,
        CallButton::Disabled => (false, t!("videoconf.call_unavailable")),
        CallButton::Enabled => (true, t!("videoconf.call")),
    };
    Some(rustket_ui::widget::icon_button_with(
        rustket_ui::icon::Icon::Video,
        enabled.then_some(on_press),
        tip.into_owned(),
        false,
        32.0,
    ))
}

/// What a room component asks for (`room::Effect::Call`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallAction {
    /// The header "Video call" button of room `rid`.
    Start(String),
    /// "Join" on a `video_conf` block (call id).
    Join(String),
    /// "Call back" / "Call again" on an ended direct call (room id).
    CallBack(String),
    /// The info icon of a block (call id).
    Info(String),
}

/// A modal dialog of the call layer.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    /// "Start a call" with mic/cam toggles where the provider supports them.
    Start {
        rid: String,
        caps: VideoConfCapabilities,
        busy: bool,
    },
    /// No usable provider (§7.4): `code` is the provider error.
    NoProvider { code: String, admin: bool },
    /// Call information (from the block cache).
    Info { call_id: String },
}

#[derive(Debug, Default)]
struct ServerCalls {
    mgr: CallManager,
    /// Calls being joined (one `join` at a time per call).
    joining: HashSet<String>,
}

/// App-level call state.
#[derive(Debug)]
pub struct Calls {
    servers: HashMap<ServerId, ServerCalls>,
    pub dialog: Option<(ServerId, Dialog)>,
    /// Last mic/cam choice (the web default is mic on, camera off).
    pub mic: bool,
    pub cam: bool,
    notify: notify::CallNotifier,
    /// The embedded-provider hint was shown this run.
    embedded_hint: bool,
    /// When the armed info-drain timer fires.
    info_timer: Option<Instant>,
}

impl Default for Calls {
    fn default() -> Self {
        Self {
            servers: HashMap::new(),
            dialog: None,
            mic: true,
            cam: false,
            notify: notify::CallNotifier::default(),
            embedded_hint: false,
            info_timer: None,
        }
    }
}

impl Calls {
    /// Ringing state of `sid` (popups).
    pub fn manager(&self, sid: &ServerId) -> Option<&CallManager> {
        self.servers.get(sid).map(|s| &s.mgr)
    }

    fn server(&mut self, sid: &ServerId) -> &mut ServerCalls {
        self.servers.entry(sid.clone()).or_default()
    }

    fn active(&self) -> bool {
        self.servers.values().any(|s| s.mgr.is_active())
    }

    /// Timers while something rings.
    pub fn subscription(&self) -> Subscription<Message> {
        if self.active() {
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    /// Test hook: puts a verified incoming call into the manager of `sid`.
    pub fn inject_incoming(&mut self, sid: &ServerId, me: &str, info: &VideoConference) {
        let now = Instant::now();
        let mgr = &mut self.server(sid).mgr;
        mgr.set_me(me);
        let signal = rocketchat::models::VideoConfSignal {
            action: VideoConfAction::Call,
            call_id: info.id.clone(),
            uid: info.creator_id().unwrap_or_default().to_owned(),
            rid: info.rid.clone(),
        };
        mgr.on_signal(signal, true, now);
        mgr.on_verified(&info.id, Ok(info), now);
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Action(ServerId, CallAction),
    /// `video-conference.info` for a block.
    Info(ServerId, String, Res<VideoConference>),
    /// `video-conference.info` verifying an incoming signal.
    Verified(ServerId, String, Res<VideoConference>),
    Capabilities(ServerId, String, Res<ProviderCapabilities>),
    /// "Start call" in the start dialog.
    Confirm,
    Started(ServerId, String, Res<StartInstructions>),
    Joined(ServerId, String, Res<JoinResult>),
    ToggleMic,
    ToggleCam,
    Accept(ServerId, String),
    Decline(ServerId, String),
    /// "Mute and dismiss" (local only).
    Mute(ServerId, String),
    Hangup(ServerId),
    CloseDialog,
    /// The admin's "Open settings" in the no-provider dialog.
    OpenAdmin(ServerId),
    OpenRoom(ServerId, String),
    Tick,
    /// Wakes the `video-conference.info` drain (rate limit, retries).
    InfoTimer,
    /// A publish / cancel / decline finished (only logged).
    Done(&'static str, Res<()>),
    Notify(notify::Msg),
}

type Out = (Task<crate::message::Message>, Vec<(ServerId, Effect)>);

fn wrap(t: Task<Message>) -> Task<crate::message::Message> {
    t.map(crate::message::Message::Calls)
}

fn session(app: &App, sid: &ServerId) -> Option<SessionHandle> {
    app.servers.get(sid).and_then(|s| s.session.clone())
}

fn rest(app: &App, sid: &ServerId) -> Option<RestClient> {
    session(app, sid).map(|s| s.rest().clone())
}

fn rest_task<T, F, Fut>(
    app: &App,
    sid: &ServerId,
    call: F,
    map: impl FnOnce(Res<T>) -> Message + Send + 'static,
) -> Task<Message>
where
    T: Send + 'static,
    F: FnOnce(RestClient) -> Fut + Send + 'static,
    Fut: Future<Output = rocketchat::Result<T>> + Send + 'static,
{
    let rest = rest(app, sid);
    Task::perform(
        async move {
            match rest {
                Some(r) => call(r).await.map_err(Failure::from),
                None => Err(Failure::from(rocketchat::Error::Unauthorized {
                    message: Some("not connected".into()),
                    code: None,
                })),
            }
        },
        map,
    )
}

/// `true` for an admin (the no-provider texts differ, §7.4).
fn is_admin(app: &App, sid: &ServerId) -> bool {
    app.servers
        .get(sid)
        .and_then(|s| s.workspace.me())
        .and_then(|m| m.roles.as_ref())
        .is_some_and(|r| r.iter().any(|r| r == "admin"))
}

/// Localised text of a provider error code (`admin-` variants for admins).
pub fn provider_error_text(code: &str, admin: bool) -> (String, String) {
    let (title, body) = match (code, admin) {
        ("no-videoconf-provider-app", false) => {
            ("videoconf.no_provider_title", "videoconf.no_provider_body")
        }
        ("no-videoconf-provider-app", true) => (
            "videoconf.admin_no_provider_title",
            "videoconf.admin_no_provider_body",
        ),
        (_, false) => (
            "videoconf.not_configured_title",
            "videoconf.not_configured_body",
        ),
        (_, true) => (
            "videoconf.admin_not_configured_title",
            "videoconf.admin_not_configured_body",
        ),
    };
    (t!(title).into_owned(), t!(body).into_owned())
}

/// The URL `join` gave, if Rustket may open it: absolute `https`, or `http`
/// when the server itself is plain `http`. An empty URL (embedded provider)
/// becomes the server's `/conference/<callId>` page.
pub fn call_url(server: &ServerUrl, call_id: &str, url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        let mut u = url::Url::parse(&server.join("conference")).ok()?;
        u.path_segments_mut().ok()?.push(call_id);
        return Some(u.to_string());
    }
    let parsed = url::Url::parse(url).ok()?;
    let server_http = server.as_str().starts_with("http:");
    match parsed.scheme() {
        "https" => Some(parsed.to_string()),
        "http" if server_http => Some(parsed.to_string()),
        _ => None,
    }
}

/// Handles a call message for the app.
pub fn update(app: &mut App, msg: Message) -> Out {
    match msg {
        Message::Action(sid, a) => on_action(app, sid, a),
        Message::Info(sid, call_id, r) => {
            if let Some(s) = app.servers.get_mut(&sid) {
                s.workspace.videoconf_mut().finish(
                    &call_id,
                    r.map_err(|e| e.to_string()),
                    Instant::now(),
                );
            }
            (Task::none(), Vec::new())
        }
        Message::Verified(sid, call_id, r) => {
            let now = Instant::now();
            if let (Ok(info), Some(s)) = (&r, app.servers.get_mut(&sid)) {
                s.workspace
                    .videoconf_mut()
                    .finish(&call_id, Ok(info.clone()), now);
            }
            let cmds =
                app.calls
                    .server(&sid)
                    .mgr
                    .on_verified(&call_id, r.as_ref().map_err(|_| ()), now);
            run(app, &sid, cmds)
        }
        Message::Capabilities(sid, rid, r) => {
            let now = Instant::now();
            let cached = match &r {
                Ok(c) => Ok(c.clone()),
                Err(e) => Err(provider_error(e.error()).unwrap_or_default().to_owned()),
            };
            if let Some(s) = app.servers.get_mut(&sid)
                && (cached.is_ok() || cached.as_ref().is_err_and(|c| !c.is_empty()))
            {
                s.workspace.videoconf_mut().set_capabilities(now, cached);
            }
            match r {
                Ok(c) => {
                    app.calls.dialog = Some((
                        sid,
                        Dialog::Start {
                            rid,
                            caps: c.capabilities,
                            busy: false,
                        },
                    ));
                    (Task::none(), Vec::new())
                }
                Err(e) => failed(app, sid, &e),
            }
        }
        Message::Confirm => {
            let Some((sid, Dialog::Start { rid, busy, .. })) = app.calls.dialog.as_mut() else {
                return (Task::none(), Vec::new());
            };
            if *busy {
                return (Task::none(), Vec::new());
            }
            *busy = true;
            let (sid, rid) = (sid.clone(), rid.clone());
            // Like the web client: always ask to ring; the server applies
            // `videoconf-ring-users` and the room type (DMs incl. group DMs).
            let ring = true;
            let r2 = rid.clone();
            let s2 = sid.clone();
            let t = rest_task(
                app,
                &sid,
                move |rest| async move { rest.videoconf_start(&r2, None, ring).await },
                move |r| Message::Started(s2, rid, r),
            );
            (wrap(t), Vec::new())
        }
        Message::Started(sid, rid, r) => {
            if matches!(&app.calls.dialog, Some((d, Dialog::Start { .. })) if *d == sid) {
                app.calls.dialog = None;
            }
            match r {
                Ok(instr) => {
                    let now = Instant::now();
                    if let Some(me) = app
                        .servers
                        .get(&sid)
                        .and_then(|s| s.workspace.user_id())
                        .map(str::to_owned)
                    {
                        app.calls.server(&sid).mgr.set_me(&me);
                    }
                    let cmds = app.calls.server(&sid).mgr.on_started(&rid, &instr, now);
                    run(app, &sid, cmds)
                }
                Err(e) => failed(app, sid, &e),
            }
        }
        Message::Joined(sid, call_id, r) => {
            app.calls.server(&sid).joining.remove(&call_id);
            match r {
                Ok(j) => {
                    let Some(server) = app.servers.get(&sid).map(|s| s.url.clone()) else {
                        return (Task::none(), Vec::new());
                    };
                    match call_url(&server, &call_id, &j.url) {
                        Some(url) => {
                            let mut fx = vec![(sid.clone(), Effect::OpenUrl(url))];
                            if j.url.trim().is_empty() && !app.calls.embedded_hint {
                                app.calls.embedded_hint = true;
                                fx.push((
                                    sid,
                                    Effect::Toast(
                                        BannerKind::Info,
                                        t!("videoconf.embedded_hint_title").into_owned(),
                                        t!("videoconf.embedded_hint_body").into_owned(),
                                    ),
                                ));
                            }
                            (Task::none(), fx)
                        }
                        None => (
                            Task::none(),
                            vec![(
                                sid,
                                Effect::Toast(
                                    BannerKind::Error,
                                    t!("videoconf.join_failed").into_owned(),
                                    t!("videoconf.bad_url").into_owned(),
                                ),
                            )],
                        ),
                    }
                }
                Err(e) => (
                    Task::none(),
                    vec![(
                        sid,
                        Effect::Toast(
                            BannerKind::Error,
                            t!("videoconf.join_failed").into_owned(),
                            e.user_text(),
                        ),
                    )],
                ),
            }
        }
        Message::ToggleMic => {
            app.calls.mic = !app.calls.mic;
            (Task::none(), Vec::new())
        }
        Message::ToggleCam => {
            app.calls.cam = !app.calls.cam;
            (Task::none(), Vec::new())
        }
        Message::Accept(sid, id) => {
            let cmds = app.calls.server(&sid).mgr.accept(&id, Instant::now());
            run(app, &sid, cmds)
        }
        Message::Decline(sid, id) => {
            let cmds = app.calls.server(&sid).mgr.decline(&id, Instant::now());
            run(app, &sid, cmds)
        }
        Message::Mute(sid, id) => {
            let cmds = app.calls.server(&sid).mgr.mute(&id, Instant::now());
            run(app, &sid, cmds)
        }
        Message::Hangup(sid) => {
            let cmds = app.calls.server(&sid).mgr.hangup();
            run(app, &sid, cmds)
        }
        Message::CloseDialog => {
            app.calls.dialog = None;
            (Task::none(), Vec::new())
        }
        Message::OpenAdmin(sid) => {
            app.calls.dialog = None;
            let Some(url) = app
                .servers
                .get(&sid)
                .map(|s| s.url.join("admin/settings/Video_Conference"))
            else {
                return (Task::none(), Vec::new());
            };
            (Task::none(), vec![(sid, Effect::OpenUrl(url))])
        }
        Message::OpenRoom(sid, rid) => {
            app.calls.dialog = None;
            (Task::none(), vec![(sid, Effect::OpenRoom(rid))])
        }
        Message::Tick => {
            let now = Instant::now();
            let sids: Vec<ServerId> = app.calls.servers.keys().cloned().collect();
            let mut tasks = Vec::new();
            let mut fx = Vec::new();
            for sid in sids {
                let cmds = app.calls.server(&sid).mgr.tick(now);
                let (t, f) = run(app, &sid, cmds);
                tasks.push(t);
                fx.extend(f);
            }
            (Task::batch(tasks), fx)
        }
        Message::InfoTimer => {
            // `fetch_wanted` runs after this update.
            app.calls.info_timer = None;
            (Task::none(), Vec::new())
        }
        Message::Done(what, r) => {
            if let Err(e) = r {
                tracing::debug!(what, error = %e, "call request failed");
            }
            (Task::none(), Vec::new())
        }
        Message::Notify(m) => {
            let (activated, task) = notify::update(&mut app.calls.notify, m);
            let task = wrap(task.map(Message::Notify));
            match activated {
                Some((sid, call_id, accept)) => {
                    let show = app
                        .desktop
                        .show_window()
                        .map(crate::message::Message::Desktop);
                    let cmds = if accept {
                        app.calls.server(&sid).mgr.accept(&call_id, Instant::now())
                    } else {
                        Vec::new()
                    };
                    let (t, f) = run(app, &sid, cmds);
                    (Task::batch([task, show, t]), f)
                }
                None => (task, Vec::new()),
            }
        }
    }
}

/// A start/capabilities failure: the provider dialog or a toast.
fn failed(app: &mut App, sid: ServerId, e: &Failure) -> Out {
    if let Some(code) = provider_error(e.error()) {
        let admin = is_admin(app, &sid);
        app.calls.dialog = Some((
            sid,
            Dialog::NoProvider {
                code: code.to_owned(),
                admin,
            },
        ));
        return (Task::none(), Vec::new());
    }
    app.calls.dialog = None;
    (
        Task::none(),
        vec![(
            sid,
            Effect::Toast(
                BannerKind::Error,
                t!("videoconf.start_failed").into_owned(),
                e.user_text(),
            ),
        )],
    )
}

fn on_action(app: &mut App, sid: ServerId, a: CallAction) -> Out {
    match a {
        CallAction::Start(rid) | CallAction::CallBack(rid) => {
            let allowed = app.servers.get(&sid).is_some_and(|s| {
                rustket_core::teams::video_call_button(&s.workspace, &rid)
                    == rustket_core::teams::CallButton::Enabled
            });
            if !allowed {
                return (Task::none(), Vec::new());
            }
            if app.calls.server(&sid).mgr.is_busy() {
                return notice(sid, CallNotice::Busy);
            }
            let now = Instant::now();
            let cached = app
                .servers
                .get(&sid)
                .and_then(|s| s.workspace.videoconf().capabilities(now).cloned());
            match cached {
                // Capabilities first (no ephemeral spam from `start`, §7.4).
                Some(Err(code)) if !code.is_empty() => {
                    let admin = is_admin(app, &sid);
                    app.calls.dialog = Some((sid, Dialog::NoProvider { code, admin }));
                    (Task::none(), Vec::new())
                }
                Some(Ok(c)) => {
                    app.calls.dialog = Some((
                        sid,
                        Dialog::Start {
                            rid,
                            caps: c.capabilities,
                            busy: false,
                        },
                    ));
                    (Task::none(), Vec::new())
                }
                _ => {
                    let s2 = sid.clone();
                    let t = rest_task(
                        app,
                        &sid,
                        |rest| async move { rest.videoconf_capabilities().await },
                        move |r| Message::Capabilities(s2, rid, r),
                    );
                    (wrap(t), Vec::new())
                }
            }
        }
        CallAction::Join(call_id) => join(app, &sid, call_id),
        CallAction::Info(call_id) => {
            if let Some(s) = app.servers.get(&sid) {
                s.workspace.videoconf().want(&call_id);
            }
            app.calls.dialog = Some((sid, Dialog::Info { call_id }));
            (Task::none(), Vec::new())
        }
    }
}

fn join(app: &mut App, sid: &ServerId, call_id: String) -> Out {
    if !app.calls.server(sid).joining.insert(call_id.clone()) {
        return (Task::none(), Vec::new());
    }
    let (mic, cam) = (app.calls.mic, app.calls.cam);
    let (s2, c2) = (sid.clone(), call_id.clone());
    let t = rest_task(
        app,
        sid,
        move |rest| async move { rest.videoconf_join(&c2, mic, cam).await },
        move |r| Message::Joined(s2, call_id, r),
    );
    (wrap(t), Vec::new())
}

fn notice(sid: ServerId, n: CallNotice) -> Out {
    let key = match n {
        CallNotice::Rejected => "videoconf.notice_rejected",
        CallNotice::NoAnswer => "videoconf.notice_no_answer",
        CallNotice::AcceptTimeout => "videoconf.notice_accept_timeout",
        CallNotice::Busy => "videoconf.notice_busy",
    };
    (
        Task::none(),
        vec![(
            sid,
            Effect::Toast(BannerKind::Info, t!(key).into_owned(), String::new()),
        )],
    )
}

/// Which call alerts the user's preferences allow on `sid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alerts {
    pub sound: bool,
    pub notify: bool,
}

/// Call sounds and notifications follow the desktop preferences (sound,
/// notifications), the account's sound and volume preferences, and stay
/// silent while the user is Busy (the popup still shows).
pub fn alerts_for(ws: &rustket_core::Workspace, prefs: &rustket_core::config::UiPrefs) -> Alerts {
    use rocketchat::models::UserStatus;
    if ws.my_status() == UserStatus::Busy {
        return Alerts {
            sound: false,
            notify: false,
        };
    }
    let pref = |k: &str| crate::desktop::user_pref(ws, k);
    let muted = pref("newMessageNotification").and_then(|v| v.as_str()) == Some("none")
        || ["notificationsSoundVolume", "masterVolume"]
            .iter()
            .any(|k| pref(k).and_then(|v| v.as_f64()).is_some_and(|v| v <= 0.0));
    Alerts {
        sound: prefs.sound && !muted,
        notify: prefs.desktop_notifications,
    }
}

fn alerts(app: &App, sid: &ServerId) -> Alerts {
    match app.servers.get(sid) {
        Some(s) => alerts_for(&s.workspace, &app.config.ui),
        None => Alerts {
            sound: false,
            notify: false,
        },
    }
}

/// Turns manager commands into requests, sounds and notifications.
fn run(app: &mut App, sid: &ServerId, cmds: Vec<CallCommand>) -> Out {
    let mut tasks = Vec::new();
    let mut fx = Vec::new();
    for c in cmds {
        match c {
            CallCommand::FetchInfo(id) => {
                // Shares the server's `video-conference.info` rate limit with
                // the blocks; without budget the caller's next `call` retries.
                let allowed = app
                    .servers
                    .get_mut(sid)
                    .is_some_and(|s| s.workspace.videoconf_mut().try_acquire(Instant::now()));
                if !allowed {
                    app.calls.server(sid).mgr.drop_unverified(&id);
                    continue;
                }
                let (s2, c2) = (sid.clone(), id.clone());
                tasks.push(wrap(rest_task(
                    app,
                    sid,
                    move |rest| async move { rest.videoconf_info(&c2).await },
                    move |r| Message::Verified(s2, id, r),
                )));
            }
            CallCommand::Publish {
                to,
                action,
                call_id,
                rid,
            } => {
                let Some(h) = session(app, sid) else { continue };
                tasks.push(wrap(Task::perform(
                    async move {
                        h.session()
                            .notify_video_conference(&to, action, &call_id, &rid)
                            .await
                            .map_err(Failure::from)
                    },
                    |r| Message::Done("publish", r),
                )));
            }
            CallCommand::RestCancel(id) => tasks.push(wrap(rest_task(
                app,
                sid,
                move |rest| async move { rest.videoconf_cancel(&id).await },
                |r| Message::Done("cancel", r),
            ))),
            CallCommand::RestDecline(id) => tasks.push(wrap(rest_task(
                app,
                sid,
                move |rest| async move { rest.videoconf_decline(&id).await },
                |r| Message::Done("decline", r),
            ))),
            CallCommand::Join(id) => {
                let (t, f) = join(app, sid, id);
                tasks.push(t);
                fx.extend(f);
            }
            CallCommand::HideIncoming(id) => {
                tasks.push(wrap(app.calls.notify.close(sid, &id).map(Message::Notify)));
            }
            CallCommand::ShowIncoming(_)
            | CallCommand::ShowOutgoing(_)
            | CallCommand::HideOutgoing(_)
            | CallCommand::StopRinger
            | CallCommand::StopDialer => {}
            CallCommand::PlayRinger => {
                if alerts(app, sid).sound {
                    app.desktop.sound.play_theme("phone-incoming-call");
                }
            }
            CallCommand::PlayDialer => {
                if alerts(app, sid).sound {
                    app.desktop.sound.play_theme("phone-outgoing-calling");
                }
            }
            CallCommand::Notify(id) => {
                if !app.focused && alerts(app, sid).notify {
                    let caller = app
                        .calls
                        .manager(sid)
                        .and_then(|m| m.incoming_call(&id))
                        .and_then(|c| c.caller.clone())
                        .unwrap_or_default();
                    let backend = app.desktop.notifications.backend();
                    tasks.push(wrap(
                        app.calls
                            .notify
                            .show(backend, sid, &id, &caller)
                            .map(Message::Notify),
                    ));
                }
            }
            CallCommand::Notice(n) => {
                let (_, f) = notice(sid.clone(), n);
                fx.extend(f);
            }
        }
    }
    (Task::batch(tasks), fx)
}

/// Session events the call layer cares about; call before `apply_event`.
/// Drops everything while the server is not logged in.
pub fn on_session_event(app: &mut App, sid: &ServerId, ev: &SessionEvent) -> Out {
    let Some(s) = app.servers.get_mut(sid) else {
        return (Task::none(), Vec::new());
    };
    if !s.is_logged_in() || s.session.is_none() {
        return (Task::none(), Vec::new());
    }
    match ev {
        SessionEvent::VideoConference(signal) => {
            let me = s.workspace.user_id().unwrap_or_default().to_owned();
            let known = s.workspace.room(&signal.rid).is_some();
            let mgr = &mut app.calls.server(sid).mgr;
            mgr.set_me(&me);
            mgr.honour_ring = app.servers.get(sid).is_some_and(|s| {
                s.workspace
                    .settings()
                    .bool_or("VideoConf_Conference_Window_Enabled", false)
            });
            let cmds = app
                .calls
                .server(sid)
                .mgr
                .on_signal(signal.clone(), known, Instant::now());
            run(app, sid, cmds)
        }
        SessionEvent::RoomVideoConference { call_id, .. } => {
            s.workspace.videoconf_mut().invalidate(call_id);
            (Task::none(), Vec::new())
        }
        SessionEvent::Resync { .. } => {
            s.workspace.videoconf_mut().invalidate_all();
            (Task::none(), Vec::new())
        }
        SessionEvent::Unknown { collection, .. } if collection == "stream-apps" => {
            // Apps installed/removed/enabled: providers may have changed.
            s.workspace.videoconf_mut().clear_capabilities();
            (Task::none(), Vec::new())
        }
        _ => (Task::none(), Vec::new()),
    }
}

/// Forgets the call state of servers that were removed or logged out.
fn drop_gone_servers(app: &mut App) -> Vec<Task<crate::message::Message>> {
    let gone: Vec<ServerId> = app
        .calls
        .servers
        .keys()
        .filter(|sid| !app.servers.get(*sid).is_some_and(|s| s.is_logged_in()))
        .cloned()
        .collect();
    let mut tasks = Vec::new();
    for sid in gone {
        if let Some(sc) = app.calls.servers.remove(&sid) {
            for c in sc.mgr.incoming() {
                tasks.push(wrap(
                    app.calls
                        .notify
                        .close(&sid, &c.call_id)
                        .map(Message::Notify),
                ));
            }
        }
        if app.calls.dialog.as_ref().is_some_and(|(d, _)| *d == sid) {
            app.calls.dialog = None;
        }
    }
    tasks
}

/// Fetches the `video-conference.info` documents blocks asked for (within
/// the rate limit; a timer wakes the next drain) and forgets the call state
/// of servers that are gone. Runs after every update.
pub fn fetch_wanted(app: &mut App) -> Task<crate::message::Message> {
    let now = Instant::now();
    let mut tasks = drop_gone_servers(app);
    let mut wake: Option<Instant> = None;
    let sids: Vec<ServerId> = app.servers.keys().cloned().collect();
    for sid in sids {
        let Some(s) = app.servers.get_mut(&sid) else {
            continue;
        };
        if s.session.is_none() {
            continue;
        }
        let ids = s.workspace.videoconf_mut().take_wanted(now);
        if let Some(at) = s.workspace.videoconf().next_wakeup(now) {
            wake = Some(wake.map_or(at, |w| w.min(at)));
        }
        for id in ids {
            let (s2, c2) = (sid.clone(), id.clone());
            tasks.push(wrap(rest_task(
                app,
                &sid,
                move |rest| async move { rest.videoconf_info(&c2).await },
                move |r| Message::Info(s2, id, r),
            )));
        }
    }
    if let Some(at) = wake
        && app.calls.info_timer.is_none_or(|t| at < t)
    {
        app.calls.info_timer = Some(at);
        let delay = at
            .saturating_duration_since(now)
            .max(Duration::from_millis(50));
        tasks.push(wrap(Task::perform(tokio::time::sleep(delay), |_| {
            Message::InfoTimer
        })));
    }
    Task::batch(tasks)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_safe_call_urls_open() {
        let https = ServerUrl::parse("https://chat.example.org").unwrap();
        let http = ServerUrl::parse("http://localhost:3000").unwrap();
        assert_eq!(
            call_url(
                &https,
                "c1",
                "https://meet.example/x#config.startWithVideoMuted=true"
            )
            .as_deref(),
            Some("https://meet.example/x#config.startWithVideoMuted=true")
        );
        assert_eq!(call_url(&https, "c1", "http://meet.example/x"), None);
        assert_eq!(call_url(&https, "c1", "javascript:alert(1)"), None);
        assert_eq!(call_url(&https, "c1", "meet.example/x"), None);
        assert!(call_url(&http, "c1", "http://meet.local/x").is_some());
        assert_eq!(
            call_url(&https, "c 1", "").as_deref(),
            Some("https://chat.example.org/conference/c%201")
        );
    }

    #[test]
    fn call_alerts_follow_prefs_and_busy() {
        use rocketchat::models::UserStatus;
        use rustket_core::config::UiPrefs;
        let mut ws = rustket_core::Workspace::new();
        ws.apply_me(
            serde_json::from_value(serde_json::json!({
                "_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {}}
            }))
            .unwrap(),
        );
        let prefs = UiPrefs::default();
        let on = Alerts {
            sound: true,
            notify: true,
        };
        assert_eq!(alerts_for(&ws, &prefs), on);
        let quiet = UiPrefs {
            sound: false,
            desktop_notifications: false,
            ..UiPrefs::default()
        };
        assert_eq!(
            alerts_for(&ws, &quiet),
            Alerts {
                sound: false,
                notify: false
            }
        );
        ws.apply_me(
            serde_json::from_value(serde_json::json!({
                "_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {"masterVolume": 0}}
            }))
            .unwrap(),
        );
        assert!(!alerts_for(&ws, &prefs).sound);
        ws.apply_user_status("me", Some("me"), UserStatus::Busy, None);
        assert!(!alerts_for(&ws, &prefs).notify);
    }

    #[test]
    fn provider_texts_differ_for_admins() {
        let (u, _) = provider_error_text("no-videoconf-provider-app", false);
        let (a, _) = provider_error_text("no-videoconf-provider-app", true);
        assert_ne!(u, a);
        let (c, _) = provider_error_text("no-active-video-conf-provider", false);
        assert_ne!(c, u);
    }
}
