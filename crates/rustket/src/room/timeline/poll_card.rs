//! The card of a poll message (`rustket_core::poll`): the question, each
//! option with its votes as a bar, the user's own votes highlighted and the
//! voters in a tooltip. A click toggles the user's reaction for the option.
//! Strings: `locales/poll.yml` (`poll.*`).

use iced::widget::{button, column, container, progress_bar, row, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::poll;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, font_semibold};
use rustket_ui::widget::{TooltipPosition, sanitize_bidi, tooltip};

use super::interact::Msg as Message;
use super::row::Env;
use crate::room::panel::common;
use crate::room::render::{emoji_element, emoji_piece, plural};

type El = Element<'static, Message>;

/// Voters shown by name in an option's tooltip; the rest are counted.
const TOOLTIP_NAMES: usize = 15;

/// The poll card of `m`, if its text is a poll. Messages with attachments
/// (quotes, files), UIKit blocks (an app's message: its blocks are the
/// body) and undecrypted ones render normally.
pub(crate) fn card(env: &Env<'_>, m: &RcMessage, can_vote: bool) -> Option<El> {
    if !is_card(m) {
        return None;
    }
    let p = poll::parse(&m.msg)?;
    let rc = env.rc();
    let votes = poll::tally(&p, m, env.ws.presence(), env.ws.username());
    let total: usize = votes.iter().map(|v| v.voters.len()).sum();
    let max = votes
        .iter()
        .map(|v| v.voters.len())
        .max()
        .unwrap_or(0)
        .max(1);
    let mut col = column![
        row![
            icon::sized(Icon::ChartColumn, 16.0).style(style::text_link),
            text(p.question.clone())
                .size(TEXT_BODY)
                .font(font_semibold())
                .width(Length::Fill),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
    ]
    .spacing(SPACE_2);
    for (option, v) in p.options.iter().zip(&votes) {
        let n = v.voters.len();
        let keycap = emoji_element::<Message>(emoji_piece(v.reaction, &rc), 16.0);
        let mut line = row![
            keycap,
            text(option.clone()).size(TEXT_BODY).width(Length::Fill)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        if v.mine {
            line = line.push(icon::sized(Icon::Check, 14.0).style(style::text_link));
        }
        line = line.push(
            text(n.to_string())
                .size(TEXT_CAPTION)
                .font(font_semibold())
                .style(style::text_secondary),
        );
        let bar = progress_bar(0.0..=max as f32, n as f32).girth(4.0);
        let inner = column![line, bar].spacing(SPACE_1);
        let b = button(inner)
            .width(Length::Fill)
            .padding([SPACE_2, SPACE_3])
            .style(style::button_reaction(v.mine))
            .on_press_maybe(can_vote.then(|| Message::React(m.id.clone(), v.reaction.to_owned())));
        let tip = voters_line(env, v);
        col = col.push(match tip {
            Some(tip) => tooltip(b, tip, TooltipPosition::Top),
            None => b.into(),
        });
    }
    let n = total as u64;
    col = col.push(
        text(t!(format!("poll.votes_{}", plural(n)), count = n).into_owned())
            .size(TEXT_CAPTION)
            .style(style::text_hint),
    );
    Some(
        container(col)
            .padding(SPACE_3)
            .max_width(480.0)
            .style(style::container_card)
            .into(),
    )
}

/// Whether `m` is drawn as a poll card.
pub(crate) fn is_card(m: &RcMessage) -> bool {
    m.attachments.as_ref().is_none_or(|a| a.is_empty())
        && m.file.is_none()
        && m.blocks
            .as_ref()
            .is_none_or(|b| b.is_null() || b.as_array().is_some_and(Vec::is_empty))
        && !crate::e2e::shown::undecrypted(m)
        && poll::parse(&m.msg).is_some()
}

/// "Alice, Bob and 3 more" for an option's tooltip.
fn voters_line(env: &Env<'_>, v: &poll::OptionVotes) -> Option<String> {
    if v.voters.is_empty() {
        return None;
    }
    let real = env.ws.use_real_names(env.prefs.use_real_names);
    let names: Vec<String> = v
        .voters
        .iter()
        .take(TOOLTIP_NAMES)
        .map(|u| {
            sanitize_bidi(&common::user_display(
                env.ws,
                real,
                &u.username,
                u.name.as_deref(),
            ))
            .into_owned()
        })
        .collect();
    let more = v.voters.len().saturating_sub(TOOLTIP_NAMES);
    let list = names.join(", ");
    Some(if more > 0 {
        t!("poll.voters_more", users = list, count = more).into_owned()
    } else {
        list
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn app_messages_with_blocks_are_not_cards() {
        let mut m = RcMessage {
            id: "m1".into(),
            msg: "📊 Lunch?\n1️⃣ Pizza\n2️⃣ Sushi".into(),
            ..Default::default()
        };
        assert!(is_card(&m));
        m.blocks = Some(serde_json::json!([]));
        assert!(is_card(&m));
        m.blocks = Some(serde_json::json!([{"type": "section"}]));
        assert!(!is_card(&m), "the blocks are the body");
    }
}
