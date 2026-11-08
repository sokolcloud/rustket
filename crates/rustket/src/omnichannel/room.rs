//! The omnichannel part of an open `t: l` room (`docs/omnichannel.md` §7):
//! composer callouts (closed, over the MAC limit, on hold + Resume, preview +
//! Take it!, read-only + Join), the "unknown contact" banner, the header
//! quick actions and their dialogs. Owned by `RoomView` as `omni`; inert for
//! other rooms.

use chrono::Local;
use iced::widget::{column, container, row, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::{Contact, Room, RoomType, Subscription};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::omnichannel::OmnichannelState;
use rustket_core::omnichannel::permissions as perm;
use rustket_core::omnichannel::room::{
    ComposerState, QuickAction, TakeBlock, composer_state, month_key, quick_actions, room_over_mac,
    take_block,
};
use rustket_core::permissions::has_permission;
use rustket_core::workspace::Changes;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, SPACE_4, TEXT_SMALL};
use rustket_ui::widget::{
    BannerKind, TooltipPosition, icon_button_with, primary_button, secondary_button, tooltip,
};

use super::dialogs::{self, Dialog, Outcome};
use super::panel::PanelKind;
use crate::error::Res;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx, panel};

/// Requests to the omnichannel room part, from the header (quick actions),
/// panels and callouts.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Quick(QuickAction),
    /// Open (or toggle) an omnichannel panel.
    Panel(PanelKind),
    Take,
    Join,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Take,
    Join,
    Resume,
    Pdf,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Request(Request),
    Dialog(dialogs::Msg),
    Done(Op, Res<()>),
    Contact(Res<Option<Box<Contact>>>),
    DismissContact,
}

#[derive(Default)]
pub struct OmniRoom {
    rid: String,
    pub dialog: Option<Dialog>,
    busy: Option<Op>,
    contact: Option<Contact>,
    contact_requested: bool,
    contact_dismissed: bool,
}

/// The room document and subscription of a livechat room.
fn live<'a>(ws: &'a Workspace, rid: &str) -> Option<(&'a Room, Option<&'a Subscription>)> {
    let e = ws.room(rid)?;
    if !matches!(e.room_type(), RoomType::Livechat | RoomType::Voip) {
        return None;
    }
    Some((e.room.as_ref()?, e.sub.as_ref()))
}

/// The snapshot of the server's omnichannel state (default before the first
/// publish).
pub fn omni_of(sid: &rustket_core::ServerId) -> std::sync::Arc<OmnichannelState> {
    super::shared::get(sid).unwrap_or_default()
}

/// The composer state of `rid` (§7.1); `None` for other rooms.
pub fn state_of(ws: &Workspace, omni: &OmnichannelState, rid: &str) -> Option<ComposerState> {
    let (room, sub) = live(ws, rid)?;
    let month = month_key(Local::now());
    let over = room_over_mac(room, omni.over_mac, &month);
    Some(composer_state(
        room,
        sub,
        ws.user_id().unwrap_or_default(),
        over,
    ))
}

fn quick_icon(a: QuickAction) -> (Icon, &'static str) {
    match a {
        QuickAction::MoveToQueue => (Icon::Undo2, "omnichannel.qa.move_to_queue"),
        QuickAction::Forward => (Icon::Forward, "omnichannel.qa.forward"),
        QuickAction::TranscriptEmail => (Icon::Mail, "omnichannel.qa.transcript_email"),
        QuickAction::TranscriptPdf { .. } => (Icon::FileText, "omnichannel.qa.transcript_pdf"),
        QuickAction::PlaceOnHold => (Icon::CirclePause, "omnichannel.qa.on_hold"),
        QuickAction::Close => (Icon::CircleX, "omnichannel.qa.close"),
    }
}

/// The header's omnichannel buttons: quick actions (§7.2), then the contact
/// and canned-responses panels. Empty for other rooms.
pub fn header_buttons<'a>(ctx: &ViewCtx<'a>) -> Vec<Element<'a, Request>> {
    let ws = ctx.workspace;
    let Some((room, sub)) = live(ws, ctx.rid) else {
        return Vec::new();
    };
    let omni = omni_of(ctx.server_id);
    let month = month_key(Local::now());
    let mut out = Vec::new();
    for a in quick_actions(ws, &omni, room, sub, &month) {
        let (ic, key) = quick_icon(a);
        let enabled = !matches!(a, QuickAction::TranscriptPdf { enabled: false });
        out.push(icon_button_with(
            ic,
            enabled.then_some(Request::Quick(a)),
            t!(key).into_owned(),
            false,
            32.0,
        ));
    }
    let mut panel_button = |ic: Icon, key: &str, kind: PanelKind| {
        let active = ctx.panel == Some(&panel::Kind::Omnichannel(kind.clone()));
        out.push(icon_button_with(
            ic,
            Some(Request::Panel(kind)),
            t!(key).into_owned(),
            active,
            32.0,
        ));
    };
    if room.contact_id.is_some() && has_permission(ws, perm::VIEW_LIVECHAT_CONTACT, None) {
        panel_button(
            Icon::Contact,
            "omnichannel.contact_info",
            PanelKind::Contact,
        );
    }
    if omni.canned_enabled(ws) {
        panel_button(
            Icon::MessageSquareText,
            "omnichannel.canned",
            PanelKind::Canned,
        );
    }
    out
}

/// A notice replacing the composer.
fn callout<'a>(
    ic: Icon,
    message: String,
    button: Option<Element<'a, Msg>>,
    hint: Option<String>,
) -> Element<'a, Msg> {
    let mut r = row![
        icon::sized(ic, 16.0).style(style::text_secondary),
        text(message)
            .size(TEXT_SMALL)
            .style(style::text_secondary)
            .width(Length::Fill),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if let Some(b) = button {
        r = r.push(match hint {
            Some(h) => tooltip(b, h, TooltipPosition::Top),
            None => b,
        });
    }
    container(
        container(r)
            .padding([SPACE_3, SPACE_4])
            .width(Length::Fill)
            .style(style::container_card),
    )
    .padding([SPACE_2, SPACE_4])
    .width(Length::Fill)
    .into()
}

impl OmniRoom {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            ..Self::default()
        }
    }

    /// A dialog request is in flight (its result must still reach the view).
    pub fn request_in_flight(&self) -> bool {
        self.dialog.as_ref().is_some_and(Dialog::busy)
    }

    fn finish(&mut self, out: Outcome) -> Action<Msg> {
        match out {
            Outcome::Stay => Action::none(),
            Outcome::Dismiss => {
                self.dialog = None;
                Action::none()
            }
            Outcome::Finished { toast, close_room } => {
                self.dialog = None;
                let mut a = Action::none();
                if let Some(t) = toast {
                    a = a.with(Effect::Toast(BannerKind::Success, t, String::new()));
                }
                if close_room {
                    a = a.with(Effect::CloseRoom);
                }
                a
            }
        }
    }

    /// Handles a request from the header, a panel or a callout.
    pub fn request(&mut self, r: Request, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let room = dialogs::room_of(ctx.workspace, &self.rid);
        match r {
            Request::Panel(k) => Action::effect(Effect::OpenPanel(panel::Kind::Omnichannel(k))),
            Request::Take => self.run(Op::Take, ctx),
            Request::Join => self.run(Op::Join, ctx),
            Request::Resume => self.run(Op::Resume, ctx),
            Request::Quick(QuickAction::TranscriptPdf { .. }) => self.run(Op::Pdf, ctx),
            Request::Quick(q) => {
                let Some(room) = room else {
                    return Action::none();
                };
                let task = match q {
                    QuickAction::Close => {
                        let (d, t) = dialogs::close_chat::CloseChatDialog::open(&room, ctx);
                        self.dialog = Some(Dialog::Close(Box::new(d)));
                        t.map(dialogs::Msg::Close)
                    }
                    QuickAction::Forward => {
                        let (d, t) = dialogs::forward_chat::ForwardDialog::open(&room, ctx);
                        self.dialog = Some(Dialog::Forward(Box::new(d)));
                        t.map(dialogs::Msg::Forward)
                    }
                    QuickAction::TranscriptEmail => {
                        let (d, t) = dialogs::transcript::TranscriptDialog::open(&room, ctx);
                        self.dialog = Some(Dialog::Transcript(Box::new(d)));
                        t.map(dialogs::Msg::Transcript)
                    }
                    QuickAction::MoveToQueue => {
                        self.dialog = Some(Dialog::Confirm(dialogs::confirm::Confirm::new(
                            dialogs::confirm::ConfirmKind::ReturnQueue,
                            &self.rid,
                        )));
                        Task::none()
                    }
                    QuickAction::PlaceOnHold => {
                        self.dialog = Some(Dialog::Confirm(dialogs::confirm::Confirm::new(
                            dialogs::confirm::ConfirmKind::OnHold,
                            &self.rid,
                        )));
                        Task::none()
                    }
                    QuickAction::TranscriptPdf { .. } => Task::none(),
                };
                Action::task(task.map(Msg::Dialog))
            }
        }
    }

    fn run(&mut self, op: Op, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        if self.busy.is_some() {
            return Action::none();
        }
        self.busy = Some(op);
        let rid = self.rid.clone();
        let task = match op {
            Op::Take => ctx.rest_task(
                move |rest| async move {
                    match rest.livechat_inquiry_for_room(&rid).await? {
                        Some(inq) => rest.livechat_take_inquiry(&inq.id).await,
                        None => Err(rocketchat::Error::Api {
                            status: 400,
                            error: Some("error-not-found".into()),
                            error_type: Some("error-not-found".into()),
                            message: None,
                            details: None,
                        }),
                    }
                },
                move |r| Msg::Done(op, r),
            ),
            Op::Join => ctx.rest_task(
                move |rest| async move { rest.livechat_room_join(&rid).await },
                move |r| Msg::Done(op, r),
            ),
            Op::Resume => ctx.rest_task(
                move |rest| async move { rest.livechat_resume_on_hold(&rid).await },
                move |r| Msg::Done(op, r),
            ),
            Op::Pdf => ctx.rest_task(
                move |rest| async move { rest.livechat_request_pdf_transcript(&rid).await },
                move |r| Msg::Done(op, r),
            ),
        };
        Action::task(task)
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Request(r) => self.request(r, ctx),
            Msg::Dialog(m) => {
                let Some(d) = self.dialog.as_mut() else {
                    return Action::none();
                };
                let (task, out) = d.update(m, ctx);
                let mut a = self.finish(out);
                a.task = Task::batch([a.task, task.map(Msg::Dialog)]);
                a
            }
            Msg::Done(op, r) => {
                self.busy = None;
                match r {
                    Ok(()) if op == Op::Pdf => Action::effect(Effect::Toast(
                        BannerKind::Success,
                        t!("omnichannel.pdf_requested").into_owned(),
                        String::new(),
                    )),
                    Ok(()) => Action::none(),
                    Err(f) => {
                        let title = match op {
                            Op::Take => t!("omnichannel.take_failed"),
                            Op::Join => t!("omnichannel.join_failed"),
                            Op::Resume => t!("omnichannel.resume_failed"),
                            Op::Pdf => t!("omnichannel.pdf_failed"),
                        };
                        Action::effect(Effect::Toast(
                            BannerKind::Error,
                            title.into_owned(),
                            super::error_text(&f),
                        ))
                    }
                }
            }
            Msg::Contact(r) => {
                match r {
                    Ok(c) => self.contact = c.map(|c| *c),
                    Err(e) => tracing::debug!(error = %e, "contact of the chat"),
                }
                Action::none()
            }
            Msg::DismissContact => {
                self.contact_dismissed = true;
                Action::none()
            }
        }
    }

    /// Loads the contact (for the "unknown contact" banner) once the room is known.
    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let _ = changes;
        if self.contact_requested {
            return Action::none();
        }
        let Some(cid) = live(ctx.workspace, &self.rid).and_then(|(r, _)| r.contact_id.clone())
        else {
            return Action::none();
        };
        if !has_permission(ctx.workspace, perm::VIEW_LIVECHAT_CONTACT, None) {
            return Action::none();
        }
        self.contact_requested = true;
        Action::task(ctx.rest_task(
            move |rest| async move {
                rest.omnichannel_contact(&cid)
                    .await
                    .map(|c| c.map(Box::new))
            },
            Msg::Contact,
        ))
    }

    fn contact_banner<'a>(&'a self) -> Option<Element<'a, Msg>> {
        if self.contact_dismissed || !self.contact.as_ref().is_some_and(|c| c.unknown) {
            return None;
        }
        let r = row![
            icon::sized(Icon::Info, 16.0).style(style::text_secondary),
            text(t!("omnichannel.unknown_contact").into_owned())
                .size(TEXT_SMALL)
                .width(Length::Fill),
            secondary_button(
                t!("omnichannel.contact_info").into_owned(),
                Some(Msg::Request(Request::Panel(PanelKind::Contact)))
            ),
            icon_button_with(
                Icon::X,
                Some(Msg::DismissContact),
                t!("omnichannel.dismiss").into_owned(),
                false,
                28.0
            ),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        Some(
            container(
                container(r)
                    .padding([SPACE_2, SPACE_3])
                    .width(Length::Fill)
                    .style(style::container_banner_info),
            )
            .padding([0.0, SPACE_4])
            .width(Length::Fill)
            .into(),
        )
    }

    fn callout_view<'a>(
        &'a self,
        state: ComposerState,
        ctx: &ViewCtx<'a>,
    ) -> Option<Element<'a, Msg>> {
        let busy = |op: Op| self.busy == Some(op);
        let el = match state {
            ComposerState::Normal => return None,
            ComposerState::Closed => callout(
                Icon::CircleX,
                t!("omnichannel.callout_closed").into_owned(),
                None,
                None,
            ),
            ComposerState::OverMac => callout(
                Icon::TriangleAlert,
                t!("omnichannel.callout_over_mac").into_owned(),
                None,
                None,
            ),
            ComposerState::OnHold => callout(
                Icon::CirclePause,
                t!("omnichannel.callout_on_hold").into_owned(),
                Some(
                    primary_button(
                        t!("omnichannel.resume").into_owned(),
                        (!busy(Op::Resume)).then_some(Msg::Request(Request::Resume)),
                    )
                    .loading(busy(Op::Resume))
                    .into(),
                ),
                None,
            ),
            ComposerState::Preview => {
                let omni = omni_of(ctx.server_id);
                let block = take_block(ctx.workspace.my_status(), omni.agent_status);
                let hint = block.map(|b| match b {
                    TakeBlock::Offline => t!("omnichannel.take_offline").into_owned(),
                    TakeBlock::Unavailable => t!("omnichannel.take_unavailable").into_owned(),
                });
                callout(
                    Icon::Eye,
                    t!("omnichannel.callout_preview").into_owned(),
                    Some(
                        primary_button(
                            t!("omnichannel.take").into_owned(),
                            (block.is_none() && !busy(Op::Take))
                                .then_some(Msg::Request(Request::Take)),
                        )
                        .loading(busy(Op::Take))
                        .into(),
                    ),
                    hint,
                )
            }
            ComposerState::Join => callout(
                Icon::Lock,
                t!("omnichannel.callout_read_only").into_owned(),
                Some(
                    primary_button(
                        t!("omnichannel.join").into_owned(),
                        (!busy(Op::Join)).then_some(Msg::Request(Request::Join)),
                    )
                    .loading(busy(Op::Join))
                    .into(),
                ),
                None,
            ),
        };
        Some(el)
    }

    /// The composer slot of the room: `composer` for other rooms and for a
    /// chat the user can write in, a callout otherwise; the "unknown contact"
    /// banner above it.
    pub fn wrap_composer<'a>(
        &'a self,
        composer: Element<'a, crate::room::Message>,
        ctx: &ViewCtx<'a>,
    ) -> Element<'a, crate::room::Message> {
        let omni = omni_of(ctx.server_id);
        let Some(state) = state_of(ctx.workspace, &omni, ctx.rid) else {
            return composer;
        };
        let main = match self.callout_view(state, ctx) {
            Some(c) => c.map(crate::room::Message::Omni),
            None => composer,
        };
        match self.contact_banner() {
            Some(b) => column![b.map(crate::room::Message::Omni), main]
                .spacing(SPACE_2)
                .into(),
            None => main,
        }
    }

    /// The open dialog (over the whole room), its key and its dismiss message.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(u8, Element<'a, Msg>, Msg)> {
        let d = self.dialog.as_ref()?;
        Some((30, d.view(ctx).map(Msg::Dialog), Msg::Dialog(d.dismiss())))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustket_core::Change;
    use serde_json::json;

    fn ws_with(room: serde_json::Value, sub: Option<serde_json::Value>) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(serde_json::from_value(json!({"_id":"me","username":"me"})).unwrap());
        ws.apply_room(Change::Inserted, serde_json::from_value(room).unwrap());
        if let Some(s) = sub {
            ws.apply_subscription(Change::Inserted, serde_json::from_value(s).unwrap());
        }
        ws
    }

    #[test]
    fn composer_states_of_livechat_rooms() {
        let omni = OmnichannelState::new();
        let sub = json!({"_id":"s","rid":"L","name":"V","t":"l","open":true,"u":{"_id":"me"}});
        let ws = ws_with(
            json!({"_id":"L","t":"l","fname":"V","open":true,"servedBy":{"_id":"me"}}),
            Some(sub.clone()),
        );
        assert_eq!(state_of(&ws, &omni, "L"), Some(ComposerState::Normal));
        let ws = ws_with(json!({"_id":"L","t":"l","fname":"V"}), Some(sub.clone()));
        assert_eq!(state_of(&ws, &omni, "L"), Some(ComposerState::Closed));
        let ws = ws_with(
            json!({"_id":"L","t":"l","fname":"V","open":true,"servedBy":{"_id":"other"}}),
            None,
        );
        assert_eq!(state_of(&ws, &omni, "L"), Some(ComposerState::Join));
        let ws = ws_with(
            json!({"_id":"L","t":"l","fname":"V","open":true,"onHold":true,"servedBy":{"_id":"me"}}),
            Some(sub),
        );
        assert_eq!(state_of(&ws, &omni, "L"), Some(ComposerState::OnHold));
        let ws = ws_with(json!({"_id":"C","t":"c","name":"general"}), None);
        assert_eq!(state_of(&ws, &omni, "C"), None);
    }

    #[test]
    fn dialogs_finish_with_effects() {
        let mut r = OmniRoom::new("L");
        r.dialog = Some(Dialog::Confirm(dialogs::confirm::Confirm::new(
            dialogs::confirm::ConfirmKind::ReturnQueue,
            "L",
        )));
        let a = r.finish(Outcome::Finished {
            toast: Some("ok".into()),
            close_room: true,
        });
        assert!(r.dialog.is_none());
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Toast(..), Effect::CloseRoom]
        ));
    }
}
