//! Modal dialogs and toast notifications.

use std::hash::{DefaultHasher, Hash, Hasher};

use iced::widget::{column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Theme, border};

use super::button::icon_button_with;
use super::feedback::BannerKind;
use super::layer::{Kind, Layered};
use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_6, TEXT_BODY, TEXT_TITLE,
    tokens,
};

/// Shows `overlay` centred over `base` on a dimmed backdrop, as a real iced
/// overlay: it is drawn after the whole tree and blocks all input to `base`
/// (clicks, scrolling, hover, keyboard except Tab). Clicking the backdrop or
/// pressing Escape publishes `on_dismiss` (Escape is consumed, so app-level
/// `keyboard::listen()` subscriptions do not see it while the modal is open;
/// one Escape dismisses even when an input in the dialog has focus).
///
/// Always call it (with `overlay = None` when closed): the wrapper stays in the
/// tree so opening/closing never resets the base's widget state. Content taller
/// than the window is limited to the window minus margins ([`dialog`] scrolls).
/// Tab/Shift+Tab cycle focus inside the content; focus operations do not reach
/// the base while the modal is open. Escape goes to the content first unless an
/// input in it has focus. Use [`modal_keyed`] when different dialogs can replace
/// each other while the modal stays open.
pub fn modal<'a, Message: Clone + 'a>(
    base: impl Into<Element<'a, Message>>,
    overlay: Option<Element<'a, Message>>,
    on_dismiss: Option<Message>,
) -> Element<'a, Message> {
    Layered::new(
        base.into(),
        overlay,
        Kind::Modal {
            on_dismiss,
            backdrop: |theme| tokens(theme).backdrop,
            padding: SPACE_6,
        },
    )
    .into()
}

/// Like [`modal`], with an identity for the dialog content: when the key changes
/// (e.g. one dialog replaced by another while the modal stays open) the content's
/// widget state (focus, scroll, inputs) starts fresh instead of being inherited.
pub fn modal_keyed<'a, Message: Clone + 'a, K: Hash>(
    base: impl Into<Element<'a, Message>>,
    overlay: Option<(K, Element<'a, Message>)>,
    on_dismiss: Option<Message>,
) -> Element<'a, Message> {
    let (key, overlay) = match overlay {
        Some((key, element)) => {
            let mut hasher = DefaultHasher::new();
            key.hash(&mut hasher);
            (Some(hasher.finish()), Some(element))
        }
        None => (None, None),
    };
    Layered::new(
        base.into(),
        overlay,
        Kind::Modal {
            on_dismiss,
            backdrop: |theme| tokens(theme).backdrop,
            padding: SPACE_6,
        },
    )
    .key(key)
    .into()
}

/// Standard dialog card: title row (with optional close button), body and a
/// right-aligned row of actions.
pub fn dialog<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    body: impl Into<Element<'a, Message>>,
    actions: Vec<Element<'a, Message>>,
    on_close: Option<Message>,
) -> Element<'a, Message> {
    let mut header = row![
        text(title.into())
            .size(TEXT_TITLE)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill)
    ]
    .align_y(Alignment::Center)
    .spacing(SPACE_2);
    if let Some(msg) = on_close {
        header = header.push(icon_button_with(Icon::X, Some(msg), "", false, 28.0));
    }
    let mut content = column![header, body.into()].spacing(SPACE_4);
    if !actions.is_empty() {
        content = content.push(
            row![space().width(Length::Fill)]
                .extend(actions)
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
        );
    }
    // The whole card scrolls when it is taller than the window, so the actions
    // stay reachable.
    container(
        scrollable(content.padding(SPACE_6))
            .height(Length::Shrink)
            .style(style::scrollable),
    )
    .width(Length::Fixed(460.0))
    .style(style::container_modal)
    .into()
}

/// A transient notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub kind: BannerKind,
    pub title: String,
    pub body: String,
}

/// Stacks `list` (oldest first) in the bottom-right corner over `base`. Expiry is up
/// to the caller (e.g. a `Task` that sleeps and publishes the close message).
/// Toasts are an overlay above everything (including a [`modal`]); the base stays
/// interactive outside the toast cards. Always call it, even with an empty list,
/// so the base keeps its widget state when toasts come and go.
pub fn toasts<'a, Message: Clone + 'a>(
    base: impl Into<Element<'a, Message>>,
    list: &'a [Toast],
    on_close: impl Fn(u64) -> Message + 'a,
) -> Element<'a, Message> {
    toasts_with(base, Vec::new(), list, on_close)
}

/// [`toasts`] with `pinned` cards (e.g. ringing calls) under the toasts, closest
/// to the corner, in the same column: neither covers the other, and presses on
/// any card stop there. Like the toasts, the pinned cards are drawn above every
/// modal of `base`.
pub fn toasts_with<'a, Message: Clone + 'a>(
    base: impl Into<Element<'a, Message>>,
    pinned: Vec<Element<'a, Message>>,
    list: &'a [Toast],
    on_close: impl Fn(u64) -> Message + 'a,
) -> Element<'a, Message> {
    let cards = (!list.is_empty() || !pinned.is_empty()).then(|| {
        column(list.iter().map(|t| toast_card(t, on_close(t.id))))
            .extend(pinned)
            .spacing(SPACE_2)
            .width(Length::Fixed(TOAST_WIDTH))
            .into()
    });
    Layered::new(base.into(), cards, Kind::Corner { padding: SPACE_5 }).into()
}

/// Width of the corner column of [`toasts`] and [`toasts_with`].
pub const TOAST_WIDTH: f32 = 340.0;

fn toast_card<'a, Message: Clone + 'a>(
    toast: &'a Toast,
    on_close: Message,
) -> Element<'a, Message> {
    let kind = toast.kind;
    let mut texts =
        column![text(&toast.title).size(TEXT_BODY).font(FONT_SEMIBOLD)].spacing(SPACE_1);
    if !toast.body.is_empty() {
        texts = texts.push(
            text(&toast.body)
                .size(TEXT_BODY - 1.0)
                .style(style::text_secondary),
        );
    }
    let accent = icon::sized(kind.icon(), 18.0).style(move |theme: &Theme| {
        let t = tokens(theme);
        iced::widget::text::Style {
            color: Some(match kind {
                BannerKind::Info => t.link,
                BannerKind::Success => t.success_text,
                BannerKind::Warning => t.warning_icon,
                BannerKind::Error => t.danger_text,
            }),
        }
    });
    Element::from(
        container(
            row![
                accent,
                texts.width(Length::Fill),
                icon_button_with(Icon::X, Some(on_close), "", false, 24.0)
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Start),
        )
        .padding([SPACE_3, SPACE_4])
        .width(Length::Fill)
        .style(move |theme: &Theme| {
            let mut s = style::container_popover(theme);
            let t = tokens(theme);
            let edge = match kind {
                BannerKind::Info => t.link,
                BannerKind::Success => t.success_text,
                BannerKind::Warning => t.warning_icon,
                BannerKind::Error => t.danger_text,
            };
            s.border = border::rounded(crate::theme::RADIUS_MD)
                .width(1)
                .color(edge.scale_alpha(0.6));
            s
        }),
    )
}
