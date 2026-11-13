//! Discussions of a room (§6.3): the `discussion-created` messages from
//! `chat.getDiscussions` (title, author, message count, last message), kept
//! live from message events with `drid`. A deleted discussion drops out of
//! the list (the server unsets `drid`); the timeline marks its message as
//! deleted ([`deleted_label`]).

use iced::widget::{column, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Message, Paginated};
use rocketchat::rest::Page;
use rust_i18n::t;
use rustket_core::workspace::Changes;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, font_medium};
use rustket_ui::widget::ellipsis;

use crate::error::Res;
use crate::room::panel::paged::{LoadState, PageReq, Paged};
use crate::room::panel::{common, debounce};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Page(PageReq, Res<Paginated<Message>>),
    More,
    Retry,
    Open(String),
    ConfirmJoin,
    CancelJoin,
    Joined(String, Res<()>),
    /// `rooms.info` of a discussion that may have been deleted.
    Checked(String, Res<()>),
}

#[derive(Default)]
pub struct Discussions {
    query: String,
    query_seq: u64,
    list: Paged<Message>,
    confirm_join: Option<(String, String)>,
    joining: bool,
    pending_open: Option<String>,
}

/// "Discussion deleted" line for a `discussion-created` message whose
/// discussion is gone (`drid` unset by the server), for the timeline.
pub fn deleted_label(m: &Message) -> Option<String> {
    (m.t.as_deref() == Some("discussion-created") && m.drid.is_none())
        .then(|| t!("teams.discussion_deleted").into_owned())
}

/// `rooms.info` says the discussion no longer exists (or is not accessible).
fn is_gone(e: &crate::error::Failure) -> bool {
    matches!(
        e.error().api_error_code(),
        Some("error-room-not-found" | "not-allowed" | "error-not-allowed")
    )
}

impl Discussions {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let (rid, q) = (ctx.rid.to_owned(), self.query.trim().to_owned());
        Action::task(ctx.rest_task(
            move |rest| async move {
                rest.chat_get_discussions(&rid, Some(&q), Page::new(req.offset, super::PAGE))
                    .await
            },
            move |r| Msg::Page(req, r),
        ))
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if let Some(rid) = &self.pending_open
            && changes.rooms.contains(rid)
            && ctx.workspace.subscription(rid).is_some()
        {
            let rid = self.pending_open.take().unwrap_or_default();
            return Action::effect(Effect::OpenRoom(rid));
        }
        // Counters (`dcount`/`dlm`) and new discussions arrive as message
        // events of the room.
        if changes.timelines.contains(ctx.rid)
            && let Some(t) = ctx.workspace.timeline(ctx.rid)
        {
            // Only discussions newer than the list head are new; older ones
            // (the timeline scrolled back) arrive with their page.
            let newest = self.list.items.first().and_then(|m| m.ts);
            let mut added = Vec::new();
            for i in t.iter() {
                let m = &i.message;
                if m.drid.is_none() {
                    continue;
                }
                match self.list.items.iter_mut().find(|x| x.id == m.id) {
                    Some(x) => {
                        if x != m {
                            *x = m.clone();
                        }
                    }
                    None if self.query.trim().is_empty()
                        && self.list.loaded
                        && (newest.is_none() || m.ts > newest) =>
                    {
                        added.push(m.clone());
                    }
                    None => {}
                }
            }
            if !added.is_empty() {
                self.list.total += added.len() as u64;
                added.sort_by_key(|a| std::cmp::Reverse(a.ts));
                self.list.items.splice(0..0, added);
            }
        }
        Action::none()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Query(q) => {
                self.query = q;
                self.query_seq += 1;
                Action::task(debounce(Msg::Search(self.query_seq)))
            }
            Msg::Search(seq) if seq == self.query_seq => self.reload(ctx),
            Msg::Search(_) => Action::none(),
            Msg::Page(req, r) => {
                self.list.finish(req, r);
                // Live inserts shift the server's offsets: drop repeats.
                let mut seen = std::collections::HashSet::new();
                self.list.items.retain(|m| seen.insert(m.id.clone()));
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::Open(drid) => {
                if ctx.workspace.subscription(&drid).is_some() {
                    return Action::effect(Effect::OpenRoom(drid));
                }
                let name = self
                    .list
                    .items
                    .iter()
                    .find(|m| m.drid.as_deref() == Some(drid.as_str()))
                    .map(|m| m.msg.clone())
                    .unwrap_or_default();
                // Not subscribed: make sure it still exists before offering
                // to join (a deleted discussion keeps a stale `drid`).
                let d2 = drid.clone();
                self.confirm_join = Some((drid.clone(), name));
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.rooms_info(&d2).await.map(drop) },
                    move |r| Msg::Checked(drid, r),
                ))
            }
            Msg::Checked(drid, Err(e)) if !is_gone(&e) => {
                if self.confirm_join.as_ref().is_some_and(|(d, _)| *d == drid) {
                    self.confirm_join = None;
                }
                Action::effect(common::error_toast(&t!("teams.discussion"), &e))
            }
            Msg::Checked(drid, r) => {
                if r.is_err() {
                    if self.confirm_join.as_ref().is_some_and(|(d, _)| *d == drid) {
                        self.confirm_join = None;
                    }
                    self.list
                        .items
                        .retain(|m| m.drid.as_deref() != Some(drid.as_str()));
                    return Action::effect(Effect::Toast(
                        rustket_ui::widget::BannerKind::Info,
                        t!("teams.discussion_deleted").into_owned(),
                        String::new(),
                    ));
                }
                Action::none()
            }
            Msg::CancelJoin => {
                if !self.joining {
                    self.confirm_join = None;
                }
                Action::none()
            }
            Msg::ConfirmJoin => {
                let Some((drid, _)) = self.confirm_join.clone() else {
                    return Action::none();
                };
                if self.joining {
                    return Action::none();
                }
                self.joining = true;
                let d2 = drid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.rooms_join(&d2, None).await.map(drop) },
                    move |r| Msg::Joined(drid, r),
                ))
            }
            Msg::Joined(drid, r) => {
                self.joining = false;
                self.confirm_join = None;
                match r {
                    Ok(()) if ctx.workspace.subscription(&drid).is_some() => {
                        Action::effect(Effect::OpenRoom(drid))
                    }
                    Ok(()) => {
                        self.pending_open = Some(drid);
                        Action::none()
                    }
                    Err(e) => {
                        Action::effect(super::error_toast(t!("teams.join_failed").into_owned(), &e))
                    }
                }
            }
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let top = column![common::search_field(
            &self.query,
            t!("teams.search_discussions").into_owned(),
            Msg::Query
        )]
        .padding([SPACE_3, SPACE_4]);
        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            _ if self.list.loaded && self.list.items.is_empty() => common::empty(
                Icon::MessagesSquare,
                t!("teams.no_discussions").into_owned(),
            ),
            _ => {
                let mut col = column![].spacing(0);
                let use_real = common::use_real_names(ctx);
                for m in &self.list.items {
                    let Some(drid) = m.drid.clone() else { continue };
                    let author_username = m.u.username.clone().unwrap_or_default();
                    let author = common::user_display(
                        ctx.workspace,
                        use_real,
                        &author_username,
                        m.u.name.as_deref(),
                    );
                    let count = m.dcount.unwrap_or(0);
                    let mut meta = vec![
                        author.clone(),
                        if count == 0 {
                            t!("teams.discussion_empty").into_owned()
                        } else {
                            super::count_text("teams.messages_count", count)
                        },
                    ];
                    if let Some(lm) = m.dlm.or(m.ts) {
                        meta.push(common::time_label(Some(lm), ctx));
                    }
                    if ctx.workspace.subscription(&drid).is_none() {
                        meta.push(t!("teams.not_joined").into_owned());
                    }
                    let title = if m.msg.trim().is_empty() {
                        t!("teams.discussion").into_owned()
                    } else {
                        m.msg.clone()
                    };
                    let line = row![
                        common::user_avatar(ctx, &author_username, &author, 32.0),
                        column![
                            row![
                                icon::sized(Icon::MessagesSquare, 14.0)
                                    .style(style::text_secondary),
                                ellipsis(title).size(TEXT_BODY).font(font_medium()),
                            ]
                            .spacing(SPACE_1)
                            .align_y(Alignment::Center),
                            text(meta.join(" · "))
                                .size(TEXT_CAPTION)
                                .style(style::text_hint),
                        ]
                        .width(Length::Fill),
                    ]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center);
                    col = col.push(common::list_row(line, Some(Msg::Open(drid))));
                }
                col = col.push(common::load_more(
                    self.list.state == LoadState::LoadingMore,
                    self.list.has_more(),
                    Msg::More,
                ));
                scrollable(col.spacing(SPACE_2 / 4.0))
                    .height(Length::Fill)
                    .style(style::scrollable)
                    .into()
            }
        };
        // The card has a slot of its own: the list below keeps its scroll
        // offset while the card is shown.
        let confirm = self.confirm_join.as_ref().map(|(_, name)| {
            iced::widget::container(common::confirm_card(
                t!("teams.join_confirm", room = name.as_str()).into_owned(),
                t!("teams.join").into_owned(),
                false,
                self.joining,
                Msg::ConfirmJoin,
                Msg::CancelJoin,
            ))
            .padding([0.0, SPACE_4])
            .center_x(Length::Fill)
            .into()
        });
        column![top, rustket_ui::widget::slot(confirm, 0.0), body]
            .height(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn join_confirmation_keeps_the_list_scrollable() {
        use crate::room::{UpdateCtx, ViewCtx};
        use crate::tree_probe::{assert_kept, scrollable_tag};
        use iced::advanced::widget::Tree;
        use rocketchat::ServerUrl;
        use rustket_core::ServerId;
        use rustket_core::workspace::Workspace;
        let mut ws = Workspace::new();
        let (sid, url) = (
            ServerId("s".into()),
            ServerUrl::parse("http://localhost:3000").unwrap(),
        );
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let mut d = Discussions::default();
        let req = d.list.reset();
        d.list.finish(
            req,
            Ok(Paginated {
                items: vec![
                    serde_json::from_value(serde_json::json!({
                        "_id": "m1", "rid": "r", "t": "discussion-created",
                        "msg": "Disc", "drid": "d1", "u": {"_id": "u", "username": "u"}
                    }))
                    .unwrap(),
                ],
                count: 1,
                offset: 0,
                total: 1,
                raw_len: 1,
                dropped: 0,
            }),
        );
        macro_rules! view_ctx {
            ($ws:expr) => {
                ViewCtx {
                    server_id: &sid,
                    server: &url,
                    rid: "r",
                    workspace: $ws,
                    images: &images,
                    prefs: &prefs,
                    connected: true,
                    panel: None,
                    now: std::time::Instant::now(),
                }
            };
        }
        let mut tree = Tree::new(d.view(&view_ctx!(&ws)).as_widget());
        for (step, msg) in [
            ("card shown", Msg::Open("d1".into())),
            ("card gone", Msg::CancelJoin),
        ] {
            let mut ctx = UpdateCtx {
                server_id: &sid,
                server: &url,
                rid: "r",
                workspace: &mut ws,
                session: None,
                images: &images,
                prefs: &prefs,
            };
            let _ = d.update(msg, &mut ctx);
            assert_kept(&mut tree, &d.view(&view_ctx!(&ws)), scrollable_tag(), step);
        }
    }

    #[test]
    fn deleted_discussion_label() {
        let m: Message = serde_json::from_value(serde_json::json!({
            "_id": "m", "rid": "r", "t": "discussion-created", "msg": "Disc"
        }))
        .unwrap();
        assert!(deleted_label(&m).is_some());
        let m: Message = serde_json::from_value(serde_json::json!({
            "_id": "m", "rid": "r", "t": "discussion-created", "msg": "Disc", "drid": "d"
        }))
        .unwrap();
        assert!(deleted_label(&m).is_none());
    }
}
