//! Message row interactions shared by the room timeline and the thread
//! view: the actions menu, permission-gated message actions, retry/discard
//! of failed sends, delete/report dialogs and the body [`Viewer`].
//!
//! Composer requests (`Effect::Compose`) are returned to the owner, which
//! routes them to its composer (the room's or the thread's); failed sends
//! are retried and dropped by that composer's outbox
//! (`ComposeIntent::Retry`/`Discard`), the only send path.

use chrono::Utc;
use iced::Element;
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::timeline::{DeliveryState, TimelineItem};
use rustket_ui::widget::BannerKind;
use serde_json::json;

use super::actions::{self, Act};
use super::dialogs::{self, Dialog};
use super::viewer::{Viewer, ViewerMsg};
use crate::error::Res;
use crate::room::panel::Kind;
use crate::room::render::{self, RenderMsg};
use crate::room::{Action, ComposeIntent, Effect, UpdateCtx, ViewCtx};

/// Where the rows of a list come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The room timeline.
    Room,
    /// Thread `tmid` (its parent and replies).
    Thread(String),
}

#[derive(Debug, Clone)]
pub enum Msg {
    View(ViewerMsg),
    Act(String, Act),
    /// Open (`Some(mid)`) or close the "more" menu of a message.
    Menu(Option<String>),
    /// Toggle reaction `emoji` on message `mid`.
    React(String, String),
    OpenThread(String),
    OpenRoom(String),
    /// `rooms.info` of a room the workspace does not have (a discussion the
    /// user is not in): opened as a preview with the Join bar.
    RoomFetched(Box<Res<crate::screens::directory::open::Opened>>),
    OpenUser(String),
    Retry(String),
    Discard(String),
    ActionDone(Act, Res<()>),
    CloseDialog,
    ConfirmDelete,
    Deleted(String, Res<()>),
    ReportText(String),
    SubmitReport,
    Reported(String, Res<()>),
    /// An app action button of the message menu (`crate::uikit::buttons`).
    UiKit(Box<crate::uikit::UiKitMsg>),
}

impl Msg {
    pub fn render(r: RenderMsg) -> Self {
        Msg::View(ViewerMsg::Render(r))
    }
}

pub struct Interactions {
    source: Source,
    pub viewer: Viewer,
    pub menu: Option<String>,
    dialog: Option<Dialog>,
}

fn toast_error(title: String, body: String) -> Effect {
    Effect::Toast(BannerKind::Error, title, body)
}

fn toast_ok(title: String) -> Effect {
    Effect::Toast(BannerKind::Success, title, String::new())
}

/// Message `mid` of `source` (a thread's parent included).
pub fn find_item(ws: &Workspace, rid: &str, source: &Source, mid: &str) -> Option<TimelineItem> {
    match source {
        Source::Room => ws.timeline(rid)?.get(mid).cloned(),
        Source::Thread(tmid) => {
            let t = ws.thread(tmid)?;
            if mid == tmid {
                return t.parent().map(|m| TimelineItem {
                    message: m.clone(),
                    state: DeliveryState::Sent,
                });
            }
            t.get(mid).cloned()
        }
    }
}

impl Interactions {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            viewer: Viewer::new(),
            menu: None,
            dialog: None,
        }
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    fn item(&self, ctx: &UpdateCtx<'_>, mid: &str) -> Option<TimelineItem> {
        find_item(ctx.workspace, ctx.rid, &self.source, mid)
    }

    /// Whether a modal (dialog or lightbox) is open.
    pub fn has_overlay(&self) -> bool {
        self.dialog.is_some() || self.viewer.image_open()
    }

    /// The open dialog or lightbox with a key for the modal and the message
    /// that dismisses it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(u8, Element<'a, Msg>, Msg)> {
        match &self.dialog {
            Some(Dialog::Delete { busy, .. }) => {
                Some((1, dialogs::delete(*busy), Msg::CloseDialog))
            }
            Some(Dialog::Report {
                description, busy, ..
            }) => Some((2, dialogs::report(description, *busy), Msg::CloseDialog)),
            None => self
                .viewer
                .overlay(ctx)
                .map(|e| (3, e.map(Msg::View), Msg::View(ViewerMsg::CloseImage))),
        }
    }

    /// Forgets state of messages that are gone (`contains` tells which exist).
    pub fn prune(&mut self, contains: impl Fn(&str) -> bool) {
        if let Some(Dialog::Delete { mid, .. } | Dialog::Report { mid, .. }) = &self.dialog
            && !contains(mid)
        {
            self.dialog = None;
        }
        if self.menu.as_deref().is_some_and(|m| !contains(m)) {
            self.menu = None;
        }
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::View(m) => self.viewer.update(m, ctx).map(Msg::View),
            Msg::Act(mid, act) => {
                self.menu = None;
                self.act(&mid, act, ctx)
            }
            Msg::Menu(m) => {
                self.menu = m;
                Action::none()
            }
            Msg::UiKit(m) => {
                self.menu = None;
                Action::effect(Effect::UiKit(m))
            }
            Msg::React(mid, emoji) => {
                if !rustket_core::permissions::can_react(ctx.workspace, ctx.rid) {
                    return Action::none();
                }
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.react(&mid, &emoji, None).await },
                    |r| Msg::ActionDone(Act::React, r),
                ))
            }
            Msg::OpenThread(mid) => Action::effect(Effect::OpenPanel(Kind::Thread(mid))),
            Msg::OpenRoom(rid) => {
                if ctx.workspace.room(&rid).is_some() {
                    return Action::effect(Effect::OpenRoom(rid));
                }
                // Members of the parent may read and join its discussions,
                // like in the web client.
                use crate::screens::directory::open;
                Action::task(ctx.rest_task(
                    move |rest| open::resolve(rest, open::Target::Room(rid)),
                    |r| Msg::RoomFetched(Box::new(r)),
                ))
            }
            Msg::RoomFetched(r) => match *r {
                Ok(o) => {
                    let rid = crate::screens::directory::open::apply(ctx.workspace, o);
                    Action::effect(Effect::OpenRoom(rid))
                }
                Err(e) => Action::effect(toast_error(
                    t!("room_timeline.not_a_member").into_owned(),
                    e.user_text(),
                )),
            },
            Msg::OpenUser(u) => Action::effect(Effect::OpenPanel(Kind::UserInfo(u))),
            Msg::Retry(mid) => Action::effect(Effect::Compose(ComposeIntent::Retry(mid))),
            Msg::Discard(mid) => Action::effect(Effect::Compose(ComposeIntent::Discard(mid))),
            Msg::ActionDone(act, r) => match r {
                Ok(()) => Action::none(),
                Err(e) => Action::effect(toast_error(
                    t!("room_timeline.action_failed", action = act.label()).into_owned(),
                    e.user_text(),
                )),
            },
            Msg::CloseDialog => {
                self.dialog = None;
                Action::none()
            }
            Msg::ConfirmDelete => {
                let Some(Dialog::Delete { mid, busy }) = &mut self.dialog else {
                    return Action::none();
                };
                *busy = true;
                let (rid, mid) = (ctx.rid.to_owned(), mid.clone());
                let done = mid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.delete_message(&rid, &mid).await },
                    move |r| Msg::Deleted(done.clone(), r),
                ))
            }
            Msg::Deleted(mid, r) => {
                if matches!(&self.dialog, Some(Dialog::Delete { mid: m, .. }) if *m == mid) {
                    self.dialog = None;
                }
                match r {
                    Ok(()) => Action::none(),
                    Err(e) => Action::effect(toast_error(
                        t!("room_timeline.delete_failed").into_owned(),
                        e.user_text(),
                    )),
                }
            }
            Msg::ReportText(s) => {
                if let Some(Dialog::Report { description, .. }) = &mut self.dialog {
                    *description = s;
                }
                Action::none()
            }
            Msg::SubmitReport => {
                let Some(Dialog::Report {
                    mid,
                    description,
                    busy,
                }) = &mut self.dialog
                else {
                    return Action::none();
                };
                if description.trim().is_empty() || *busy {
                    return Action::none();
                }
                *busy = true;
                let body = json!({"messageId": mid, "description": description.trim()});
                let done = mid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.post_ok("chat.reportMessage", &body).await },
                    move |r| Msg::Reported(done.clone(), r),
                ))
            }
            Msg::Reported(mid, r) => match r {
                Ok(()) => {
                    if matches!(&self.dialog, Some(Dialog::Report { mid: m, .. }) if *m == mid) {
                        self.dialog = None;
                    }
                    Action::effect(toast_ok(t!("room_timeline.reported").into_owned()))
                }
                Err(e) => {
                    if let Some(Dialog::Report { mid: m, busy, .. }) = &mut self.dialog
                        && *m == mid
                    {
                        *busy = false;
                    }
                    Action::effect(toast_error(
                        t!("room_timeline.report_failed").into_owned(),
                        e.user_text(),
                    ))
                }
            },
        }
    }

    fn act(&mut self, mid: &str, act: Act, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let Some(item) = self.item(ctx, mid) else {
            return Action::none();
        };
        if item.state != DeliveryState::Sent
            || !available(ctx.workspace, &self.source, &item.message, Utc::now()).contains(&act)
        {
            // The toolbar was built before a time limit or a permission changed.
            return Action::effect(toast_error(
                t!("room_timeline.action_unavailable").into_owned(),
                String::new(),
            ));
        }
        let m = item.message;
        let mid = mid.to_owned();
        // A reply follows its parent thread.
        let thread = m.tmid.clone().unwrap_or_else(|| mid.clone());
        let rest_act = |ctx: &UpdateCtx<'_>, act: Act| {
            let mid = mid.clone();
            let thread = thread.clone();
            Action::task(ctx.rest_task(
                move |rest| async move {
                    match act {
                        Act::Pin => rest.pin_message(&mid).await.map(|_| ()),
                        Act::Unpin => rest.unpin_message(&mid).await,
                        Act::Star => rest.star_message(&mid).await,
                        Act::Unstar => rest.unstar_message(&mid).await,
                        Act::Follow => rest.follow_message(&thread).await,
                        Act::Unfollow => rest.unfollow_message(&thread).await,
                        _ => Ok(()),
                    }
                },
                move |r| Msg::ActionDone(act, r),
            ))
        };
        match act {
            Act::React => Action::effect(Effect::Compose(ComposeIntent::React(mid))),
            Act::ReplyInThread => {
                Action::effect(Effect::Compose(ComposeIntent::ReplyInThread(mid)))
            }
            Act::Quote => Action::effect(Effect::Compose(ComposeIntent::Quote(mid))),
            Act::Edit => Action::effect(Effect::Compose(ComposeIntent::Edit(mid))),
            // The app leaves the room first, like the web client.
            Act::MarkUnread => Action::effect(Effect::MarkUnread(mid)),
            Act::StartDiscussion => Action::effect(Effect::StartDiscussion { pmid: Some(mid) }),
            // The app applies it to the sidebar at once and reverts a failure.
            Act::StarThread | Act::UnstarThread => Action::effect(Effect::StarThread {
                rid: m.rid.clone(),
                tmid: mid,
                on: act == Act::StarThread,
            }),
            Act::KeepInSidebar | Act::RemoveFromSidebar => match actions::kept_thread_of(&m) {
                Some(tmid) => Action::effect(Effect::KeepThread {
                    rid: m.rid.clone(),
                    tmid: tmid.to_owned(),
                    keep: act == Act::KeepInSidebar,
                }),
                None => Action::none(),
            },
            Act::CopyText => Action::task(iced::clipboard::write(render::plain_text(&m)))
                .with(toast_ok(t!("room_timeline.copied").into_owned())),
            Act::CopyLink => Action::task(iced::clipboard::write(actions::permalink(
                ctx.server,
                ctx.workspace,
                &m,
            )))
            .with(toast_ok(t!("room_timeline.link_copied").into_owned())),
            Act::Delete => {
                self.dialog = Some(Dialog::Delete { mid, busy: false });
                Action::none()
            }
            Act::Report => {
                self.dialog = Some(Dialog::Report {
                    mid,
                    description: String::new(),
                    busy: false,
                });
                Action::task(iced::widget::operation::focus(dialogs::REPORT_INPUT_ID))
            }
            Act::Pin | Act::Unpin | Act::Star | Act::Unstar | Act::Follow | Act::Unfollow => {
                rest_act(ctx, act)
            }
        }
    }
}

/// Actions on `m` in a list of `source`: the thread view does not offer
/// "reply in thread" (its composer is right there) nor "start a discussion"
/// (the web client's `threads` context does not have it).
pub fn available(
    ws: &Workspace,
    source: &Source,
    m: &RcMessage,
    now: chrono::DateTime<Utc>,
) -> Vec<Act> {
    let mut acts = actions::available(ws, m, now);
    crate::e2e::room::filter_actions(ws, m, &mut acts);
    if matches!(source, Source::Thread(_)) {
        acts.retain(|a| !matches!(a, Act::ReplyInThread | Act::StartDiscussion));
    }
    acts
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::RcDate;
    use rocketchat::models::UserRef;

    fn reply(id: &str, tmid: &str) -> RcMessage {
        RcMessage {
            id: id.into(),
            rid: "r".into(),
            msg: id.into(),
            tmid: Some(tmid.into()),
            ts: RcDate::from_millis(1_800_000_000_000),
            u: UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn thread_source_finds_parent_and_replies() {
        let mut ws = Workspace::new();
        ws.open_thread("r", "p");
        let mut parent = reply("p", "x");
        parent.tmid = None;
        ws.apply_message(parent);
        ws.apply_message(reply("a", "p"));
        let src = Source::Thread("p".into());
        assert_eq!(
            find_item(&ws, "r", &src, "p").map(|i| i.message.id),
            Some("p".to_owned())
        );
        assert!(find_item(&ws, "r", &src, "a").is_some());
        assert!(find_item(&ws, "r", &Source::Room, "a").is_none());
    }

    #[test]
    fn retry_and_discard_go_to_the_composer() {
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let sid = rustket_core::ServerId("s".into());
        let url = rocketchat::ServerUrl::parse("http://localhost:3000").unwrap();
        let mut ws = Workspace::new();
        ws.open_thread("r", "p");
        let mut m = reply("local", "p");
        m.temp = Some(true);
        ws.add_pending(m);
        ws.mark_pending_failed("r", Some("p"), "local", "boom");
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "r",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut i = Interactions::new(Source::Thread("p".into()));
        let a = i.update(Msg::Retry("local".into()), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Compose(ComposeIntent::Retry(m))] if m == "local"
        ));
        let a = i.update(Msg::Discard("local".into()), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Compose(ComposeIntent::Discard(m))] if m == "local"
        ));
        // Nothing changed here: the composer's outbox owns the message.
        assert_eq!(ctx.workspace.pending_messages("r").len(), 1);
    }
    #[test]
    fn discussion_not_joined_opens_as_a_preview() {
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let sid = rustket_core::ServerId("s".into());
        let url = rocketchat::ServerUrl::parse("http://localhost:3000").unwrap();
        let mut ws = Workspace::new();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "r",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut i = Interactions::new(Source::Room);
        // Unknown room: fetched (no "not a member" dead end).
        let a = i.update(Msg::OpenRoom("d1".into()), &mut ctx);
        assert!(a.effects.is_empty());
        let room: rocketchat::models::Room = serde_json::from_value(json!({
            "_id": "d1", "t": "c", "name": "disc", "prid": "r"
        }))
        .unwrap();
        let a = i.update(
            Msg::RoomFetched(Box::new(Ok(crate::screens::directory::open::Opened {
                room,
                sub: None,
            }))),
            &mut ctx,
        );
        assert!(matches!(a.effects.as_slice(), [Effect::OpenRoom(r)] if r == "d1"));
        assert!(ctx.workspace.room("d1").is_some());
        assert!(ctx.workspace.subscription("d1").is_none());
    }

    #[test]
    fn keep_in_sidebar_on_a_reply_keeps_its_thread() {
        use rocketchat::models::{Room, RoomType, Setting, Subscription, User};
        use rustket_core::config::ThreadsMode;
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let sid = rustket_core::ServerId("s".into());
        let url = rocketchat::ServerUrl::parse("http://localhost:3000").unwrap();
        let ts = RcDate::from_millis(1_800_000_000_000);
        let mut ws = Workspace::new();
        ws.apply_initial(rustket_core::sync::InitialData {
            me: User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            },
            subscriptions: vec![Subscription {
                id: "s1".into(),
                rid: "r".into(),
                name: "dev".into(),
                t: RoomType::Channel,
                open: true,
                updated_at: ts,
                ..Default::default()
            }],
            rooms: vec![Room {
                id: "r".into(),
                t: RoomType::Channel,
                name: Some("dev".into()),
                updated_at: ts,
                ..Default::default()
            }],
            settings: vec![Setting {
                id: "Threads_enabled".into(),
                value: json!(true),
                ..Default::default()
            }],
            next_since: ts,
            ..Default::default()
        });
        ws.set_thread_config(ThreadsMode::UnreadKept, []);
        ws.open_thread("r", "p");
        ws.apply_message(reply("a", "p"));
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "r",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut i = Interactions::new(Source::Thread("p".into()));
        let a = i.update(Msg::Act("a".into(), Act::KeepInSidebar), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::KeepThread { rid, tmid, keep: true }] if rid == "r" && tmid == "p"
        ));
        // Not offered (already kept): refused with a toast, no effect.
        ctx.workspace
            .set_thread_config(ThreadsMode::UnreadKept, [("r", "p")]);
        let a = i.update(Msg::Act("a".into(), Act::KeepInSidebar), &mut ctx);
        assert!(matches!(a.effects.as_slice(), [Effect::Toast(..)]));
        let a = i.update(Msg::Act("a".into(), Act::RemoveFromSidebar), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::KeepThread { tmid, keep: false, .. }] if tmid == "p"
        ));
        // Starring the parent goes through the app (applied to the sidebar
        // at once).
        ctx.workspace.set_starred_threads(true);
        let mut parent = reply("p", "p");
        parent.tmid = None;
        parent.tcount = Some(1);
        ctx.workspace.apply_message(parent);
        let a = i.update(Msg::Act("p".into(), Act::StarThread), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::StarThread { rid, tmid, on: true }] if rid == "r" && tmid == "p"
        ));
    }
}
