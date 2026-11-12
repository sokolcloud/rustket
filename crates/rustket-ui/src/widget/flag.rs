//! Flag marks (sidebar rows) and colour swatches (section headers, menus).
//!
//! Colours are palette indices into [`Tokens::flag`](crate::theme::Tokens::flag),
//! resolved at draw time so a theme switch needs no rebuild of the data.

use iced::widget::{column, container, row, space};
use iced::{Element, Theme, border};

use super::misc::{TooltipPosition, tooltip};
use crate::theme::tokens;

/// Flag of a row: the palette index and its (user or default) name, shown
/// in the mark's tooltip so the colour is not the only cue.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FlagMark {
    pub index: usize,
    pub name: String,
}

impl FlagMark {
    pub fn new(index: usize, name: impl Into<String>) -> Self {
        Self {
            index,
            name: name.into(),
        }
    }
}

/// A filled flag (pole and pennant) in flag colour `index`, `size` square.
pub fn flag_glyph<'a, Message: 'a>(index: usize, size: f32) -> Element<'a, Message> {
    let fill = move |radius: border::Radius| {
        move |theme: &Theme| container::Style {
            background: Some(tokens(theme).flag_color(index).into()),
            border: border::rounded(radius),
            ..container::Style::default()
        }
    };
    let pole_w = (size * 0.14).max(1.5);
    let body_w = (size * 0.56).round();
    let body_h = (size * 0.5).round();
    let pole = container(space().width(pole_w).height(size * 0.86))
        .style(fill(border::Radius::from(pole_w / 2.0)));
    let body = container(space().width(body_w).height(body_h)).style(fill(
        border::Radius::default()
            .top_right(size * 0.12)
            .bottom_right(size * 0.12),
    ));
    container(row![pole, column![space().height(size * 0.04), body]])
        .width(size)
        .height(size)
        .padding(iced::Padding::ZERO.left(size * 0.2).top(size * 0.07))
        .into()
}

/// [`flag_glyph`] with the flag's name as a tooltip.
pub fn flag_mark<'a, Message: 'a>(mark: &FlagMark, size: f32) -> Element<'a, Message> {
    tooltip(
        flag_glyph(mark.index, size),
        mark.name.clone(),
        TooltipPosition::Top,
    )
}

/// A filled circle in flag colour `index`.
pub fn flag_swatch<'a, Message: 'a>(index: usize, size: f32) -> Element<'a, Message> {
    container(space().width(size).height(size))
        .style(move |theme: &Theme| container::Style {
            background: Some(tokens(theme).flag_color(index).into()),
            border: border::rounded(size),
            ..container::Style::default()
        })
        .into()
}
