//! Room information: avatar, name, badges, topic / announcement / description
//! (rendered like messages) and room actions.

use iced::widget::{column, container, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Message as RcMessage, RoomType};
use rust_i18n::t;
use rustket_core::room_list;
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_TITLE};
use rustket_ui::widget::{ChipKind, divider};

use super::Kind;
use super::common::{self, RoomAction};
use crate::error::Res;
use crate::room::timeline::{Viewer, ViewerMsg};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    View(ViewerMsg),
    Action(RoomAction),
    Confirm,
    Cancel,
    Done(RoomAction, Res<()>),
    Open(Kind),
}

#[derive(Default)]
pub struct Info {
    /// Topic, announcement and description as messages for the renderer.
    texts: Vec<(&'static str, RcMessage)>,
    confirm: Option<RoomAction>,
    busy: bool,
    viewer: Viewer,
}

/// Fake messages so room texts get the same markdown rendering as messages.
fn texts(ws: &Workspace, rid: &str) -> Vec<(&'static str, RcMessage)> {
    let Some(room) = ws.room(rid).and_then(|e| e.room.as_ref()) else {
        return Vec::new();
    };
    [
        ("room_panel.topic", &room.topic),
        ("room_panel.announcement", &room.announcement),
        ("room_panel.description", &room.description),
    ]
    .into_iter()
    .filter_map(|(key, v)| {
        let v = v.as_deref()?.trim();
        (!v.is_empty()).then(|| {
            (
                key,
                RcMessage {
                    id: format!("room-info-{key}"),
                    rid: rid.to_owned(),
                    msg: v.to_owned(),
                    md: Some(crate::e2e::markdown::parse(v)),
                    ..Default::default()
                },
            )
        })
    })
    .collect()
}

impl Info {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.texts = texts(ctx.workspace, ctx.rid);
        Action::none()
    }

    /// The image lightbox, if open, and the message that closes it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        self.viewer
            .overlay(ctx)
            .map(|e| (e.map(Msg::View), Msg::View(ViewerMsg::CloseImage)))
    }

    /// Which room texts are shown (i18n keys), for tests.
    pub fn text_keys(&self) -> Vec<&'static str> {
        self.texts.iter().map(|(k, _)| *k).collect()
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if changes.rooms.contains(ctx.rid) {
            self.texts = texts(ctx.workspace, ctx.rid);
        }
        Action::none()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::View(m) => self.viewer.update(m, ctx).map(Msg::View),
            Msg::Open(kind) => Action::effect(Effect::OpenPanel(kind)),
            Msg::Action(a) if a.needs_confirm() => {
                self.confirm = Some(a);
                Action::none()
            }
            Msg::Action(a) => self.run(a, ctx),
            Msg::Confirm => match self.confirm {
                Some(a) => self.run(a, ctx),
                None => Action::none(),
            },
            Msg::Cancel => {
                self.confirm = None;
                Action::none()
            }
            Msg::Done(a, r) => {
                self.busy = false;
                self.confirm = None;
                match r {
                    Ok(()) => a.done_action(),
                    Err(e) => Action::effect(common::error_toast(&a.label(), &e)),
                }
            }
        }
    }

    fn run(&mut self, a: RoomAction, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        if self.busy {
            return Action::none();
        }
        self.busy = true;
        let rid = ctx.rid.to_owned();
        Action::task(ctx.rest_task(move |rest| a.run(rest, rid), move |r| Msg::Done(a, r)))
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let Some(entry) = ws.room(ctx.rid) else {
            return common::empty(Icon::Info, t!("room_panel.room_unknown").into_owned());
        };
        let use_real = common::use_real_names(ctx);
        let name = room_list::display_name(ws, entry, use_real);
        let t_ = entry.room_type();
        let avatar = match t_ {
            RoomType::Direct => common::user_avatar(ctx, entry.name(), &name, 96.0),
            _ => common::room_avatar(
                ctx,
                ctx.rid,
                entry.room.as_ref().and_then(|r| r.avatar_etag.clone()),
                &name,
                96.0,
            ),
        };
        let mut chips = Vec::new();
        let kind_label = if entry.is_team() {
            t!("room_panel.type_team")
        } else if entry.is_discussion() {
            t!("room_panel.type_discussion")
        } else {
            match t_ {
                RoomType::Private => t!("room_panel.type_private"),
                RoomType::Direct => t!("room_panel.type_direct"),
                _ => t!("room_panel.type_channel"),
            }
        };
        chips.push((kind_label.into_owned(), ChipKind::Primary));
        if entry.is_read_only() {
            chips.push((t!("room_panel.read_only").into_owned(), ChipKind::Warning));
        }
        if entry.is_archived() {
            chips.push((t!("room_panel.archived").into_owned(), ChipKind::Neutral));
        }
        if entry.is_encrypted() {
            chips.push((t!("room_panel.encrypted").into_owned(), ChipKind::Success));
        }

        let mut col = column![
            container(avatar).center_x(Length::Fill),
            container(
                text(name.clone())
                    .size(TEXT_TITLE)
                    .font(FONT_SEMIBOLD)
                    .align_x(Alignment::Center)
            )
            .center_x(Length::Fill),
            container(common::badges(chips)).center_x(Length::Fill),
        ]
        .spacing(SPACE_3)
        .padding(SPACE_4);

        for (key, m) in &self.texts {
            col = col.push(common::field(
                t!(*key).into_owned(),
                self.viewer.body(m, ctx).map(Msg::View),
            ));
        }
        if let Some(n) = entry.room.as_ref().and_then(|r| r.users_count) {
            col = col.push(common::field(
                t!("room_panel.members").into_owned(),
                text(n.to_string()).style(style::text_secondary),
            ));
        }

        let mut actions = column![].spacing(SPACE_1);
        if super::settings::offered(ws, ctx.rid) {
            actions = actions.push(common::action_entry(
                Icon::Pencil,
                t!("room_panel.edit").into_owned(),
                false,
                Some(Msg::Open(Kind::RoomSettings)),
            ));
        }
        if entry.is_team() {
            actions = actions.push(common::action_entry(
                Icon::Hash,
                t!("room_panel.team_channels").into_owned(),
                false,
                Some(Msg::Open(Kind::TeamChannels)),
            ));
        }
        let fav = common::is_favorite(ws, ctx.rid);
        for a in [
            RoomAction::Favorite(!fav),
            RoomAction::Hide,
            RoomAction::Leave,
            RoomAction::Delete,
        ] {
            if common::room_action_allowed(ws, ctx.rid, a) {
                actions = actions.push(common::action_entry(
                    a.icon(),
                    a.label(),
                    a.danger(),
                    (!self.busy).then_some(Msg::Action(a)),
                ));
            }
        }
        col = col.push(divider()).push(actions);
        if let Some(a) = self.confirm {
            col = col.push(common::confirm_card(
                a.confirm_text(&name),
                a.label(),
                a.danger(),
                self.busy,
                Msg::Confirm,
                Msg::Cancel,
            ));
        }
        scrollable(col.spacing(SPACE_2))
            .height(Length::Fill)
            .style(style::scrollable)
            .into()
    }
}
