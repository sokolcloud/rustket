//! The `video_conf` block of a `videoconf` message (docs/teams-videoconf.md
//! §7.7, `VideoConferenceBlock.tsx`). Anyone can forge the message and its
//! blocks, so only the call id is taken from it; everything shown comes from
//! `video-conference.info` (the workspace's [`VideoConfStore`]).
//!
//! [`VideoConfStore`]: rustket_core::videoconf::VideoConfStore

use std::hash::{DefaultHasher, Hash, Hasher};

use iced::widget::{column, container, row, space, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Message, VideoConfKind, VideoConfStatus, VideoConference};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::room_list::Avatar;
use rustket_core::videoconf::InfoState;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{AvatarShape, avatar, icon_button_with, primary_button, secondary_button};

use super::CallAction;
use crate::room::render::{RenderCtx, RenderMsg};

/// Avatars shown in the footer.
const FACES: usize = 3;

/// The call id of the first `video_conf` block of `msg`.
pub fn call_id(msg: &Message) -> Option<&str> {
    msg.blocks
        .as_ref()?
        .as_array()?
        .iter()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("video_conf"))?
        .get("callId")?
        .as_str()
        .filter(|c| !c.is_empty() && c.len() <= 64)
}

/// Part of a row's `lazy` key: changes when the call's info changes.
pub fn key(ws: &Workspace, msg: &Message) -> u64 {
    let Some(id) = call_id(msg) else {
        return 0;
    };
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    ws.videoconf().version(id).hash(&mut h);
    h.finish()
}

/// What the block shows for a loaded call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    Calling,
    Ongoing,
    Ended { not_answered: bool, direct: bool },
}

pub fn shown(v: &VideoConference) -> Shown {
    let not_answered = matches!(
        v.status,
        VideoConfStatus::Expired | VideoConfStatus::Declined
    );
    if v.is_ended() {
        return Shown::Ended {
            not_answered,
            direct: v.kind == VideoConfKind::Direct,
        };
    }
    if v.kind == VideoConfKind::Direct && v.status == VideoConfStatus::Calling {
        return Shown::Calling;
    }
    Shown::Ongoing
}

fn joined_text(n: usize) -> String {
    let n = n as u64;
    let form = crate::room::render::plural(n);
    t!(format!("videoconf.joined_{form}"), count = n).into_owned()
}

fn faces(v: &VideoConference, rc: &RenderCtx<'_>) -> Element<'static, RenderMsg> {
    let joined: Vec<_> = v.joined_users().collect();
    let mut r = row![].spacing(SPACE_1 / 2.0).align_y(Alignment::Center);
    for u in joined.iter().take(FACES) {
        let username = u.username.clone().unwrap_or_default();
        let name = u.name.clone().unwrap_or_else(|| username.clone());
        let src = rc.images.avatar(
            rc.server_id,
            rc.server,
            &Avatar::User {
                username: username.clone(),
            },
            &name,
        );
        r = r.push(avatar(src, 20.0, AvatarShape::Rounded));
    }
    if !joined.is_empty() {
        r = r.push(space().width(SPACE_1));
        let label = if joined.len() > FACES {
            t!("videoconf.joined_more", count = joined.len() - FACES).into_owned()
        } else {
            joined_text(joined.len())
        };
        r = r.push(text(label).size(TEXT_CAPTION).style(style::text_secondary));
    }
    r.into()
}

fn card(
    title: String,
    glyph: Icon,
    header_actions: Vec<Element<'static, RenderMsg>>,
    footer: Option<Element<'static, RenderMsg>>,
) -> Element<'static, RenderMsg> {
    let mut head = row![
        icon::sized(glyph, 16.0).style(style::text_secondary),
        text(title)
            .size(TEXT_BODY)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    for a in header_actions {
        head = head.push(a);
    }
    let mut col = column![head].spacing(SPACE_2);
    if let Some(f) = footer {
        col = col.push(f);
    }
    container(col)
        .padding(SPACE_3)
        .max_width(420.0)
        .width(Length::Fill)
        .style(style::container_card)
        .into()
}

/// The block of `msg`, or `None` when it has no `video_conf` block.
pub fn view(msg: &Message, rc: &RenderCtx<'_>) -> Option<Element<'static, RenderMsg>> {
    let call_id = call_id(msg)?.to_owned();
    let store = rc.workspace.videoconf();
    let state = store.state(&call_id).cloned();
    if !matches!(&state, Some(InfoState::Loaded(v)) if v.is_ended()) {
        // Unknown or stale: fetched right after this frame.
        store.want(&call_id);
        crate::images::VIEW_MISSES.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Some(match state {
        None | Some(InfoState::Loading) => card(
            t!("videoconf.loading").into_owned(),
            Icon::Video,
            Vec::new(),
            None,
        ),
        Some(InfoState::Failed) => card(
            t!("videoconf.unavailable").into_owned(),
            Icon::VideoOff,
            Vec::new(),
            None,
        ),
        Some(InfoState::Loaded(v)) => loaded(&v, &call_id, msg, rc),
    })
}

fn loaded(
    v: &VideoConference,
    call_id: &str,
    msg: &Message,
    rc: &RenderCtx<'_>,
) -> Element<'static, RenderMsg> {
    let interactive = rc.interactive;
    let act = |a: CallAction| interactive.then_some(RenderMsg::Call(a));
    let mut actions: Vec<Element<'static, RenderMsg>> = vec![icon_button_with(
        Icon::Info,
        act(CallAction::Info(call_id.to_owned())),
        t!("videoconf.info").into_owned(),
        false,
        24.0,
    )];
    if let Some(d) = v.discussion_rid.clone().filter(|d| !d.is_empty()) {
        actions.push(icon_button_with(
            Icon::MessagesSquare,
            interactive.then_some(RenderMsg::OpenRoom(d)),
            t!("videoconf.discussion").into_owned(),
            false,
            24.0,
        ));
    }
    let me = rc.workspace.user_id().unwrap_or_default();
    match shown(v) {
        Shown::Calling => card(
            t!("videoconf.calling").into_owned(),
            Icon::Phone,
            actions,
            Some(
                text(t!("videoconf.waiting").into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary)
                    .into(),
            ),
        ),
        Shown::Ongoing => {
            let footer = row![
                primary_button(
                    t!("videoconf.join").into_owned(),
                    act(CallAction::Join(call_id.to_owned()))
                )
                .icon(Icon::Video),
                faces(v, rc),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center);
            card(
                t!("videoconf.ongoing").into_owned(),
                Icon::Video,
                actions,
                Some(footer.into()),
            )
        }
        Shown::Ended {
            not_answered,
            direct,
        } => {
            let mut footer = row![].spacing(SPACE_3).align_y(Alignment::Center);
            let can_call = rustket_core::teams::video_call_button(rc.workspace, &msg.rid)
                == rustket_core::teams::CallButton::Enabled;
            if direct && can_call {
                let label = if v.creator_id() == Some(me) {
                    t!("videoconf.call_again")
                } else {
                    t!("videoconf.call_back")
                };
                footer = footer.push(
                    secondary_button(
                        label.into_owned(),
                        act(CallAction::CallBack(msg.rid.clone())),
                    )
                    .icon(Icon::Phone),
                );
            }
            if not_answered {
                footer = footer.push(
                    text(t!("videoconf.not_answered").into_owned())
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                );
            } else if !direct {
                footer = footer.push(faces(v, rc));
            }
            card(
                t!("videoconf.ended").into_owned(),
                Icon::PhoneOff,
                actions,
                Some(footer.into()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn call_id_from_blocks() {
        let m: Message = serde_json::from_value(json!({
            "_id": "m", "rid": "r", "t": "videoconf",
            "blocks": [{"type": "section"}, {"type": "video_conf", "callId": "c1", "blockId": "c1"}]
        }))
        .unwrap();
        assert_eq!(call_id(&m), Some("c1"));
        let m: Message = serde_json::from_value(json!({"_id": "m", "rid": "r"})).unwrap();
        assert_eq!(call_id(&m), None);
    }

    #[test]
    fn states() {
        let mut v = VideoConference {
            kind: VideoConfKind::Direct,
            ..Default::default()
        };
        assert_eq!(shown(&v), Shown::Calling);
        v.status = VideoConfStatus::Started;
        assert_eq!(shown(&v), Shown::Ongoing);
        v.status = VideoConfStatus::Declined;
        v.ended_at = rocketchat::RcDate::from_millis(1);
        assert_eq!(
            shown(&v),
            Shown::Ended {
                not_answered: true,
                direct: true
            }
        );
    }
}
