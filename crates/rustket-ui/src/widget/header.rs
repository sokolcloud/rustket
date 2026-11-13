//! Room header bar.

use iced::widget::{Row, column, container, row};
use iced::{Alignment, Element, Length};

use super::ellipsis::ellipsis;
use crate::style;
use crate::theme::tokens;
use crate::theme::{
    HEADER_HEIGHT, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SUBTITLE, font_semibold,
};
use iced::Theme;

/// Header with an optional leading element (avatar/icon/back button), title,
/// optional subtitle (topic, presence text) and trailing actions.
pub fn header_bar<'a, Message: 'a>(
    left: Option<Element<'a, Message>>,
    title: impl Into<String>,
    subtitle: Option<String>,
    actions: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut titles = column![
        ellipsis(title.into())
            .size(TEXT_SUBTITLE)
            .font(font_semibold())
    ]
    .spacing(0);
    if let Some(sub) = subtitle {
        titles = titles.push(
            ellipsis(sub)
                .size(TEXT_CAPTION)
                .style(|theme: &Theme| Some(tokens(theme).text_secondary)),
        );
    }
    let mut bar = row![].spacing(SPACE_3).align_y(Alignment::Center);
    if let Some(left) = left {
        bar = bar.push(left);
    }
    bar = bar.push(titles.width(Length::Fill));
    bar = bar.push(
        Row::with_children(actions)
            .spacing(SPACE_1)
            .align_y(Alignment::Center),
    );
    column![
        container(bar)
            .height(HEADER_HEIGHT)
            .width(Length::Fill)
            .padding([0.0, SPACE_4])
            .center_y(HEADER_HEIGHT)
            .style(style::container_header),
        super::misc::divider(),
    ]
    .into()
}

/// Thin spacing used between header action groups.
pub fn header_gap<'a, Message: 'a>() -> Element<'a, Message> {
    iced::widget::space().width(SPACE_2).into()
}
