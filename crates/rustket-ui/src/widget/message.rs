//! Timeline message layouts. The app renders the body (markdown, attachments…);
//! these functions provide the Rocket.Chat row structure, hover behaviour and
//! states (sequential, highlighted, pending, failed).

use iced::widget::{Column, button, container, row, space, stack, text};
use iced::{Alignment, Element, Length, Padding, Theme, alignment, border};

use super::ellipsis::ellipsis;
use super::hover_box::hover_box;
use super::misc::{TooltipPosition, tooltip};
use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{
    IndicatorLook, SPACE_1, SPACE_2, SPACE_5, TEXT_BODY, TEXT_CAPTION, TEXT_MICRO, font_bold,
    font_semibold, indicator_look, radius_sm, tokens,
};

/// Width of the avatar column (avatar 36 + gap).
pub const GUTTER: f32 = 48.0;
/// Avatar size used in message rows.
pub const MESSAGE_AVATAR: f32 = 36.0;

/// Everything a message row shows. Build with [`MessageRowProps::new`] and set
/// the public fields.
pub struct MessageRowProps<'a, Message> {
    pub avatar: Element<'a, Message>,
    pub author: String,
    /// Role/bot chips next to the author name.
    pub author_badges: Vec<Element<'a, Message>>,
    pub timestamp: String,
    pub edited: bool,
    pub pinned: bool,
    pub starred: bool,
    pub body: Element<'a, Message>,
    pub attachments: Vec<Element<'a, Message>>,
    pub reactions: Option<Element<'a, Message>>,
    pub thread_summary: Option<Element<'a, Message>>,
    /// Grouped with the previous message: no avatar/header; the time shows in the
    /// gutter on hover.
    pub is_sequential: bool,
    /// Mentions the current user.
    pub is_mention_highlight: bool,
    /// Written by the current user.
    pub is_own_highlight: bool,
    /// Selected/jumped-to message.
    pub is_selected: bool,
    /// Jumped-to message (permalink, search result): Rocket.Chat paints it
    /// `rc_jump_bg` instead of `selected_bg`.
    pub is_jump_target: bool,
    /// Floating toolbar shown top-right while the row is hovered.
    pub actions_toolbar: Option<Element<'a, Message>>,
    /// A menu of the toolbar is open: the toolbar stays shown.
    pub actions_menu_open: bool,
    /// Not yet confirmed by the server: text is dimmed and a clock is shown.
    pub is_pending: bool,
    /// Sending failed: error line with `failed_label` and a retry action.
    pub is_failed: bool,
    pub failed_label: String,
    pub retry: Option<(String, Message)>,
    /// Tooltips for the edited/pinned/starred markers (i18n).
    pub edited_label: String,
    pub pinned_label: String,
    pub starred_label: String,
}

impl<'a, Message> MessageRowProps<'a, Message> {
    pub fn new(
        avatar: impl Into<Element<'a, Message>>,
        author: impl Into<String>,
        timestamp: impl Into<String>,
        body: impl Into<Element<'a, Message>>,
    ) -> Self {
        Self {
            avatar: avatar.into(),
            author: author.into(),
            author_badges: Vec::new(),
            timestamp: timestamp.into(),
            edited: false,
            pinned: false,
            starred: false,
            body: body.into(),
            attachments: Vec::new(),
            reactions: None,
            thread_summary: None,
            is_sequential: false,
            is_mention_highlight: false,
            is_own_highlight: false,
            is_selected: false,
            is_jump_target: false,
            actions_toolbar: None,
            actions_menu_open: false,
            is_pending: false,
            is_failed: false,
            failed_label: "Message not sent".into(),
            retry: None,
            edited_label: "Edited".into(),
            pinned_label: "Pinned".into(),
            starred_label: "Starred".into(),
        }
    }
}

fn marker<'a, Message: 'a>(i: Icon, label: &str) -> Element<'a, Message> {
    tooltip(
        icon::sized(i, 12.0).style(style::text_hint),
        label.to_owned(),
        TooltipPosition::Top,
    )
}

/// Row background: selected > mention > own > hover, in the look active
/// when the row is built.
fn row_style(
    selected: bool,
    jump: bool,
    mention: bool,
    own: bool,
    pending: bool,
) -> impl Fn(&Theme, bool) -> container::Style {
    row_style_in(indicator_look(), selected, jump, mention, own, pending)
}

/// [`row_style`] under `look`. Rocket.Chat: no mention background, and a
/// selected row that is a jump target (`jump`) takes the pale yellow of its
/// editing look (`rc_jump_bg`); other selections keep `selected_bg`.
pub fn row_style_in(
    look: IndicatorLook,
    selected: bool,
    jump: bool,
    mention: bool,
    own: bool,
    pending: bool,
) -> impl Fn(&Theme, bool) -> container::Style {
    let rc = look == IndicatorLook::RocketChat;
    let mention = mention && !rc;
    move |theme, hovered| {
        let t = tokens(theme);
        let background = if selected {
            Some(if rc && jump {
                t.rc_jump_bg
            } else {
                t.selected_bg
            })
        } else if mention {
            Some(t.highlight_bg)
        } else if hovered {
            Some(t.hover_bg)
        } else if own {
            Some(t.own_message_bg)
        } else {
            None
        };
        container::Style {
            text_color: Some(if pending { t.text_hint } else { t.text_primary }),
            background: background.map(Into::into),
            ..container::Style::default()
        }
    }
}

/// A timeline message row.
pub fn message_row<'a, Message: Clone + 'a>(
    p: MessageRowProps<'a, Message>,
) -> Element<'a, Message> {
    let mut main = Column::new().spacing(2).width(Length::Fill);

    if !p.is_sequential {
        let mut header = row![
            container(
                ellipsis(p.author.clone())
                    .size(TEXT_BODY)
                    .font(font_bold())
                    .width(Length::Shrink)
            )
            .max_width(320)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        for b in p.author_badges {
            header = header.push(b);
        }
        header = header.push(
            text(p.timestamp.clone())
                .size(TEXT_CAPTION)
                .wrapping(text::Wrapping::None)
                .style(style::text_hint),
        );
        if p.edited {
            header = header.push(marker(Icon::Pencil, &p.edited_label));
        }
        if p.pinned {
            header = header.push(marker(Icon::Pin, &p.pinned_label));
        }
        if p.starred {
            header = header.push(marker(Icon::Star, &p.starred_label));
        }
        if p.is_pending {
            header = header.push(icon::sized(Icon::Clock, 12.0).style(style::text_hint));
        }
        main = main.push(header);
    }

    let mut body_row = row![container(p.body).width(Length::Fill)]
        .spacing(SPACE_1)
        .align_y(Alignment::Start);
    // Sequential rows show their markers at the end of the body.
    if p.is_sequential {
        if p.edited {
            body_row = body_row.push(marker(Icon::Pencil, &p.edited_label));
        }
        if p.pinned {
            body_row = body_row.push(marker(Icon::Pin, &p.pinned_label));
        }
        if p.starred {
            body_row = body_row.push(marker(Icon::Star, &p.starred_label));
        }
        if p.is_pending {
            body_row = body_row.push(icon::sized(Icon::Clock, 12.0).style(style::text_hint));
        }
    }
    main = main.push(body_row);

    for a in p.attachments {
        main = main.push(container(a).padding(Padding::ZERO.top(SPACE_1)));
    }
    if let Some(r) = p.reactions {
        main = main.push(container(r).padding(Padding::ZERO.top(SPACE_1)));
    }
    if let Some(ts) = p.thread_summary {
        main = main.push(container(ts).padding(Padding::ZERO.top(SPACE_1)));
    }
    if p.is_failed {
        let mut failed = row![
            icon::sized(Icon::CircleAlert, 14.0).style(style::text_danger),
            text(p.failed_label.clone())
                .size(TEXT_CAPTION)
                .style(style::text_danger),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        if let Some((label, msg)) = p.retry {
            failed = failed.push(
                button(text(label).size(TEXT_CAPTION).font(font_semibold()))
                    .padding([0, 4])
                    .style(style::button_link)
                    .on_press(msg),
            );
        }
        main = main.push(failed);
    }

    let gutter: Element<'a, Message> = if p.is_sequential {
        space().width(GUTTER).into()
    } else {
        container(p.avatar).width(GUTTER).into()
    };
    let content = row![gutter, main]
        .padding(Padding::from([
            if p.is_sequential { 2.0 } else { SPACE_2 },
            SPACE_5,
        ]))
        .width(Length::Fill);

    let mut on_hover: Vec<Element<'a, Message>> = Vec::new();
    if p.is_sequential {
        on_hover.push(
            container(
                text(p.timestamp)
                    .size(TEXT_MICRO)
                    .wrapping(text::Wrapping::None)
                    .style(style::text_hint),
            )
            .width(Length::Fixed(SPACE_5 + GUTTER))
            .padding(Padding::ZERO.left(SPACE_5).top(5.0))
            .into(),
        );
    }
    if let Some(toolbar) = p.actions_toolbar {
        on_hover.push(
            container(toolbar)
                .width(Length::Fill)
                .align_x(alignment::Horizontal::Right)
                .padding(Padding::ZERO.right(SPACE_5))
                .into(),
        );
    }
    hover_box(
        content,
        on_hover,
        row_style(
            p.is_selected,
            p.is_jump_target,
            p.is_mention_highlight,
            p.is_own_highlight,
            p.is_pending,
        ),
    )
    .pinned(p.actions_menu_open)
    .into()
}

/// The floating hover toolbar frame for message actions (pass icon buttons).
pub fn message_toolbar<'a, Message: 'a>(
    actions: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(row(actions).spacing(2).align_y(Alignment::Center))
        .padding(2)
        .style(style::container_popover)
        .into()
}

/// Centred-ish system message ("Alice joined the channel").
pub fn system_message_row<'a, Message: 'a>(
    icon: Icon,
    text_element: impl Into<Element<'a, Message>>,
    timestamp: impl Into<String>,
) -> Element<'a, Message> {
    let content = row![
        container(icon::sized(icon, 16.0).style(style::text_hint))
            .width(GUTTER)
            .align_x(alignment::Horizontal::Center),
        container(text_element).width(Length::Fill),
        text(timestamp.into())
            .size(TEXT_CAPTION)
            .wrapping(text::Wrapping::None)
            .style(style::text_hint),
    ]
    .spacing(0)
    .align_y(Alignment::Center)
    .padding([SPACE_1, SPACE_5]);
    hover_box(content, Vec::new(), |theme, hovered| container::Style {
        text_color: Some(tokens(theme).text_secondary),
        background: hovered.then(|| tokens(theme).hover_bg.into()),
        ..container::Style::default()
    })
    .into()
}

/// Thread summary under a message: participants, reply count (as a link-button),
/// last reply time and an unread dot.
pub fn thread_summary<'a, Message: Clone + 'a>(
    count_label: impl Into<String>,
    last_reply: impl Into<String>,
    participants: Vec<Element<'a, Message>>,
    unread: bool,
    on_open: Message,
) -> Element<'a, Message> {
    thread_summary_kind(
        count_label,
        last_reply,
        participants,
        unread.then_some(super::badge::BadgeKind::Thread),
        false,
        None,
        on_open,
    )
}

/// [`thread_summary`] with the unread dot of `kind` (content palette).
/// `filled`: Rocket.Chat's unread thread, a button filled with
/// `rc_button_primary` and white text, the dot next to it; each avatar gets
/// a white outline so one in the fill's own blue stays visible.
/// `dot_tip`: the dot's tooltip (Rocket.Chat's "Mentions you" / "Unread").
pub fn thread_summary_kind<'a, Message: Clone + 'a>(
    count_label: impl Into<String>,
    last_reply: impl Into<String>,
    participants: Vec<Element<'a, Message>>,
    kind: Option<super::badge::BadgeKind>,
    filled: bool,
    dot_tip: Option<String>,
    on_open: Message,
) -> Element<'a, Message> {
    let mut avatars = row![].spacing(2).align_y(Alignment::Center);
    for p in participants.into_iter().take(5) {
        let p: Element<'a, Message> = if filled {
            // The first child sizes the stack: the avatar keeps its size.
            stack![
                p,
                container(space())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(|theme: &Theme| container::Style {
                        border: border::rounded(3).width(1).color(tokens(theme).rc_badge_fg),
                        ..container::Style::default()
                    }),
            ]
            .into()
        } else {
            p
        };
        avatars = avatars.push(p);
    }
    let last = text(last_reply.into()).size(TEXT_CAPTION);
    let mut content = row![
        icon::sized(Icon::MessageCircle, 14.0),
        avatars,
        text(count_label.into())
            .size(TEXT_CAPTION + 1.0)
            .font(font_semibold()),
        if filled {
            last
        } else {
            last.style(style::text_hint)
        },
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    let dot = kind.map(|k| {
        let d = super::badge::dot_in(k, 8.0, super::badge::BadgePlace::Content);
        match dot_tip {
            Some(tip) => tooltip(d, tip, TooltipPosition::Top),
            None => d,
        }
    });
    if !filled {
        if let Some(dot) = dot {
            content = content.push(dot);
        }
        return summary_button(content.into(), on_open);
    }
    let mut line = row![filled_summary_button(content.into(), on_open)]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
    if let Some(dot) = dot {
        line = line.push(dot);
    }
    line.into()
}

/// Discussion summary under a "discussion created" message.
pub fn discussion_summary<'a, Message: Clone + 'a>(
    count_label: impl Into<String>,
    last_message: impl Into<String>,
    on_open: Message,
) -> Element<'a, Message> {
    let content = row![
        icon::sized(Icon::MessagesSquare, 14.0),
        text(count_label.into())
            .size(TEXT_CAPTION + 1.0)
            .font(font_semibold()),
        text(last_message.into())
            .size(TEXT_CAPTION)
            .style(style::text_hint),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    summary_button(content.into(), on_open)
}

fn summary_button<'a, Message: Clone + 'a>(
    content: Element<'a, Message>,
    on_open: Message,
) -> Element<'a, Message> {
    let b = button(content)
        .padding([4, 8])
        .style(|theme: &Theme, status| {
            let t = tokens(theme);
            let background = match status {
                button::Status::Hovered | button::Status::Pressed => Some(t.primary_tint.into()),
                _ => None,
            };
            button::Style {
                background,
                text_color: t.link,
                border: border::rounded(radius_sm()).width(1).color(t.border),
                ..button::Style::default()
            }
        })
        .on_press(on_open.clone());
    super::focus_ring::focus_ring(b, Some(on_open)).into()
}

/// The summary button filled like Rocket.Chat's primary "View thread".
fn filled_summary_button<'a, Message: Clone + 'a>(
    content: Element<'a, Message>,
    on_open: Message,
) -> Element<'a, Message> {
    let b = button(content)
        .padding([4, 8])
        .style(filled_summary_style)
        .on_press(on_open.clone());
    super::focus_ring::focus_ring(b, Some(on_open)).into()
}

/// [`filled_summary_button`]'s style: darker while hovered or pressed, like
/// Rocket.Chat's primary button.
fn filled_summary_style(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let shade = match status {
        button::Status::Hovered => 0.2,
        button::Status::Pressed => 0.35,
        _ => 0.0,
    };
    let fill = crate::palette::mix(t.rc_button_primary, iced::Color::BLACK, shade);
    button::Style {
        background: Some(fill.into()),
        text_color: t.rc_badge_fg,
        border: border::rounded(radius_sm()).width(1).color(fill),
        ..button::Style::default()
    }
}

#[cfg(test)]
mod rc_tests {
    use super::*;
    use crate::theme::with_indicator_look;

    fn bg(style: container::Style) -> Option<iced::Color> {
        style.background.and_then(|b| match b {
            iced::Background::Color(c) => Some(c),
            _ => None,
        })
    }

    /// The filled "View thread" darkens on hover and press.
    #[test]
    fn filled_summary_gives_feedback() {
        let theme = Theme::Light;
        let fill = |status| match filled_summary_style(&theme, status).background {
            Some(iced::Background::Color(c)) => c,
            other => panic!("{other:?}"),
        };
        let active = fill(button::Status::Active);
        assert_eq!(active, tokens(&theme).rc_button_primary);
        let hovered = fill(button::Status::Hovered);
        let pressed = fill(button::Status::Pressed);
        assert!(hovered.b < active.b && pressed.b < hovered.b);
    }

    /// H6/H7: Rocket.Chat drops the mention background and paints the
    /// jumped-to row `rc_jump_bg`; the own tint stays; Rustket unchanged.
    #[test]
    fn row_background_per_look() {
        let theme = Theme::Light;
        let t = tokens(&theme);
        let rk = IndicatorLook::Rustket;
        let rc = IndicatorLook::RocketChat;
        let case = |look, selected, jump, mention, own| {
            bg(row_style_in(look, selected, jump, mention, own, false)(
                &theme, false,
            ))
        };
        assert_eq!(case(rk, true, false, true, true), Some(t.selected_bg));
        assert_eq!(case(rk, true, true, true, true), Some(t.selected_bg));
        assert_eq!(case(rk, false, false, true, true), Some(t.highlight_bg));
        assert_eq!(case(rk, false, false, false, true), Some(t.own_message_bg));
        assert_eq!(case(rc, true, true, true, true), Some(t.rc_jump_bg));
        // A plain selection (the Later cursor) keeps `selected_bg`.
        assert_eq!(case(rc, true, false, false, false), Some(t.selected_bg));
        assert_eq!(case(rc, false, false, true, false), None);
        assert_eq!(case(rc, false, false, true, true), Some(t.own_message_bg));
        assert_eq!(
            bg(row_style_in(rc, false, false, true, false, false)(
                &theme, true
            )),
            Some(t.hover_bg)
        );
        // `row_style` follows the look active when the row is built.
        with_indicator_look(rc, || {
            assert_eq!(
                bg(row_style(true, true, false, false, false)(&theme, false)),
                Some(t.rc_jump_bg)
            );
            assert_eq!(
                bg(row_style(true, false, false, false, false)(&theme, false)),
                Some(t.selected_bg)
            );
        });
    }

    /// H2: the unread summary builds filled (with its dot) and plain.
    #[test]
    fn thread_summary_variants_build() {
        for look in [IndicatorLook::Rustket, IndicatorLook::RocketChat] {
            with_indicator_look(look, || {
                for (kind, filled) in [
                    (None, false),
                    (Some(super::super::BadgeKind::Thread), false),
                    (Some(super::super::BadgeKind::Mention), true),
                ] {
                    let _: Element<'_, ()> =
                        thread_summary_kind("2 replies", "", Vec::new(), kind, filled, None, ());
                }
                // Filled with avatars (outlined) and a dot tooltip.
                let avatars = || -> Vec<Element<'_, ()>> {
                    vec![space().width(18).height(18).into(), text("A").into()]
                };
                let _: Element<'_, ()> = thread_summary_kind(
                    "2 replies",
                    "",
                    avatars(),
                    Some(super::super::BadgeKind::Mention),
                    true,
                    Some("Mentions you".into()),
                    (),
                );
                let _: Element<'_, ()> = thread_summary("2 replies", "", Vec::new(), true, ());
            });
        }
    }
}
