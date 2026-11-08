//! Room lists: discussions of the room (`rooms.getDiscussions`) and the
//! channels of a team (`teams.listRooms`). Opening a room the user is not in
//! asks for confirmation, then joins it.

use iced::widget::{column, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Paginated, Room, RoomType};
use rust_i18n::t;
use rustket_core::workspace::Changes;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{FONT_MEDIUM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::ellipsis;

use super::api;
use super::common;
use super::debounce;
use super::paged::{LoadState, PageReq, Paged};
use crate::error::Res;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Discussions,
    TeamChannels,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Page(PageReq, Res<Paginated<Room>>),
    More,
    Retry,
    Open(String),
    ConfirmJoin,
    CancelJoin,
    Joined(String, Res<()>),
}

pub struct Rooms {
    mode: Mode,
    query: String,
    query_seq: u64,
    list: Paged<Room>,
    /// Joined, waiting for the room to show up in the workspace.
    pending_open: Option<String>,
    joining: Option<String>,
    /// Not-joined room waiting for the user's confirmation (rid, name).
    confirm_join: Option<(String, String)>,
}

impl Rooms {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            query: String::new(),
            query_seq: 0,
            list: Paged::default(),
            pending_open: None,
            joining: None,
            confirm_join: None,
        }
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&mut self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let q = self.query.clone();
        let map = move |r| Msg::Page(req, r);
        match self.mode {
            Mode::Discussions => {
                let rid = ctx.rid.to_owned();
                Action::task(
                    ctx.rest_task(move |rest| api::discussions(rest, rid, q, req.offset), map),
                )
            }
            Mode::TeamChannels => {
                let team = ctx.workspace.room(ctx.rid).and_then(|e| {
                    e.room
                        .as_ref()
                        .and_then(|r| r.team_id.clone())
                        .or_else(|| e.sub.as_ref().and_then(|s| s.team_id.clone()))
                });
                let Some(team) = team else {
                    self.list.finish(
                        req,
                        Ok(Paginated {
                            items: Vec::new(),
                            count: 0,
                            offset: 0,
                            total: 0,
                            raw_len: 0,
                            dropped: 0,
                        }),
                    );
                    return Action::none();
                };
                Action::task(
                    ctx.rest_task(move |rest| api::team_rooms(rest, team, q, req.offset), map),
                )
            }
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if let Some(rid) = &self.pending_open
            && changes.rooms.contains(rid)
            && ctx.workspace.room(rid).is_some()
        {
            let rid = self.pending_open.take().unwrap_or_default();
            return Action::effect(Effect::OpenRoom(rid));
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
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::Open(rid) => {
                if ctx.workspace.subscription(&rid).is_some() {
                    return Action::effect(Effect::OpenRoom(rid));
                }
                if self.joining.is_some() {
                    return Action::none();
                }
                // Joining subscribes the user and posts a "joined" message,
                // so it is never a side effect of a plain click.
                let name = self
                    .list
                    .items
                    .iter()
                    .find(|r| r.id == rid)
                    .map(|r| r.display_name().to_owned())
                    .unwrap_or_default();
                self.confirm_join = Some((rid, name));
                Action::none()
            }
            Msg::CancelJoin => {
                if self.joining.is_none() {
                    self.confirm_join = None;
                }
                Action::none()
            }
            Msg::ConfirmJoin => {
                let Some((rid, _)) = self.confirm_join.clone() else {
                    return Action::none();
                };
                if self.joining.is_some() {
                    return Action::none();
                }
                self.joining = Some(rid.clone());
                let r2 = rid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.rooms_join(&r2, None).await.map(|_| ()) },
                    move |r| Msg::Joined(rid, r),
                ))
            }
            Msg::Joined(rid, r) => {
                self.joining = None;
                self.confirm_join = None;
                match r {
                    Ok(_) if ctx.workspace.subscription(&rid).is_some() => {
                        Action::effect(Effect::OpenRoom(rid))
                    }
                    Ok(_) => {
                        self.pending_open = Some(rid);
                        Action::none()
                    }
                    Err(e) => {
                        Action::effect(common::error_toast(&t!("room_panel.join_failed"), &e))
                    }
                }
            }
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let placeholder = match self.mode {
            Mode::Discussions => t!("room_panel.search_discussions"),
            Mode::TeamChannels => t!("room_panel.search_channels"),
        };
        let top = column![common::search_field(
            &self.query,
            placeholder.into_owned(),
            Msg::Query
        )]
        .padding([SPACE_3, SPACE_4]);
        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            _ if self.list.loaded && self.list.items.is_empty() => match self.mode {
                Mode::Discussions => common::empty(
                    Icon::MessagesSquare,
                    t!("room_panel.no_discussions").into_owned(),
                ),
                Mode::TeamChannels => {
                    common::empty(Icon::Hash, t!("room_panel.no_channels").into_owned())
                }
            },
            _ => {
                let mut col = column![].spacing(0);
                for r in &self.list.items {
                    let name = r.display_name().to_owned();
                    let joined = ctx.workspace.subscription(&r.id).is_some();
                    let mut meta = Vec::new();
                    if self.mode == Mode::Discussions {
                        meta.push(
                            t!("room_panel.messages_count", count = r.msgs.unwrap_or(0))
                                .into_owned(),
                        );
                        if let Some(lm) = r.lm.or(r.ts) {
                            meta.push(common::time_label(Some(lm), ctx));
                        }
                    } else {
                        meta.push(
                            t!(
                                "room_panel.members_count",
                                count = r.users_count.unwrap_or(0)
                            )
                            .into_owned(),
                        );
                    }
                    if !joined {
                        meta.push(t!("room_panel.not_joined").into_owned());
                    }
                    let glyph = match (self.mode, r.t) {
                        (Mode::Discussions, _) => Icon::MessagesSquare,
                        (_, RoomType::Private) => Icon::Lock,
                        _ => Icon::Hash,
                    };
                    let line = row![
                        common::room_avatar(ctx, &r.id, r.avatar_etag.clone(), &name, 32.0),
                        column![
                            row![
                                icon::sized(glyph, 14.0).style(style::text_secondary),
                                ellipsis(name).size(TEXT_BODY).font(FONT_MEDIUM),
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
                    col = col.push(common::list_row(
                        line,
                        (self.joining.is_none()).then(|| Msg::Open(r.id.clone())),
                    ));
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
                t!("room_panel.confirm_join", room = name).into_owned(),
                t!("room_panel.join").into_owned(),
                false,
                self.joining.is_some(),
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
    use rocketchat::ServerUrl;
    use rustket_core::ServerId;
    use rustket_core::workspace::Workspace;

    #[test]
    fn open_not_joined_asks_first() {
        let mut ws = Workspace::new();
        let (sid, url) = (
            ServerId("s".into()),
            ServerUrl::parse("http://localhost:3000").unwrap(),
        );
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "r",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut r = Rooms::new(Mode::Discussions);
        let req = r.list.reset();
        r.list.finish(
            req,
            Ok(Paginated {
                items: vec![
                    serde_json::from_value(
                        serde_json::json!({"_id": "d1", "t": "c", "name": "x", "fname": "Disc"}),
                    )
                    .unwrap(),
                ],
                count: 1,
                offset: 0,
                total: 1,
                raw_len: 1,
                dropped: 0,
            }),
        );
        let a = r.update(Msg::Open("d1".into()), &mut ctx);
        assert!(a.effects.is_empty());
        assert_eq!(r.confirm_join, Some(("d1".into(), "Disc".into())));
        assert!(r.joining.is_none(), "no join before confirmation");
        let _ = r.update(Msg::CancelJoin, &mut ctx);
        assert!(r.confirm_join.is_none());
        let _ = r.update(Msg::Open("d1".into()), &mut ctx);
        let _ = r.update(Msg::ConfirmJoin, &mut ctx);
        assert_eq!(r.joining.as_deref(), Some("d1"));
    }

    /// The join confirmation used to be inserted before the list: the list
    /// was rebuilt (scrolled back to the top) when the card appeared and
    /// again when it went away.
    #[test]
    fn join_confirmation_keeps_the_list_scrollable() {
        use crate::tree_probe::{assert_kept, scrollable_tag};
        use iced::advanced::widget::Tree;
        let mut ws = Workspace::new();
        let (sid, url) = (
            ServerId("s".into()),
            ServerUrl::parse("http://localhost:3000").unwrap(),
        );
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let mut r = Rooms::new(Mode::Discussions);
        let req = r.list.reset();
        r.list.finish(
            req,
            Ok(Paginated {
                items: vec![
                    serde_json::from_value(
                        serde_json::json!({"_id": "d1", "t": "c", "name": "x", "fname": "Disc"}),
                    )
                    .unwrap(),
                ],
                count: 1,
                offset: 0,
                total: 1,
                raw_len: 1,
                dropped: 0,
            }),
        );
        let view = |r: &Rooms, ws: &Workspace| -> Tree {
            let ctx = ViewCtx {
                server_id: &sid,
                server: &url,
                rid: "r",
                workspace: ws,
                images: &images,
                prefs: &prefs,
                connected: true,
                panel: None,
                now: std::time::Instant::now(),
            };
            Tree::new(r.view(&ctx).as_widget())
        };
        let mut tree = view(&r, &ws);
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
            let _ = r.update(msg, &mut ctx);
            let ctx = ViewCtx {
                server_id: &sid,
                server: &url,
                rid: "r",
                workspace: &ws,
                images: &images,
                prefs: &prefs,
                connected: true,
                panel: None,
                now: std::time::Instant::now(),
            };
            assert_kept(&mut tree, &r.view(&ctx), scrollable_tag(), step);
        }
    }
}
