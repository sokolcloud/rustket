//! Style functions for iced widgets, to be passed to `.style(..)`.
//!
//! All of them read the [`Tokens`](crate::theme::Tokens) matching the active theme,
//! so the same function works for light and dark mode.

use iced::widget::checkbox as w_checkbox;
use iced::widget::pick_list as w_pick_list;
use iced::widget::scrollable as w_scrollable;
use iced::widget::text_input as w_text_input;
use iced::widget::toggler as w_toggler;
use iced::widget::{button, container, overlay, text};
use iced::{Background, Border, Color, Shadow, Theme, Vector, border};

use crate::theme::{RADIUS_PILL, Tokens, radius_lg, radius_md, radius_sm, tokens};

fn bg(color: Color) -> Option<Background> {
    Some(Background::Color(color))
}

fn shadow(t: &Tokens, y: f32, blur: f32) -> Shadow {
    Shadow {
        color: t.shadow,
        offset: Vector::new(0.0, y),
        blur_radius: blur,
    }
}

// ---------------------------------------------------------------- text

pub fn text_primary(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).text_primary),
    }
}

pub fn text_secondary(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).text_secondary),
    }
}

pub fn text_hint(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).text_hint),
    }
}

pub fn text_link(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).link),
    }
}

pub fn text_danger(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).danger_text),
    }
}

pub fn text_success(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).success_text),
    }
}

// ---------------------------------------------------------------- buttons

fn filled(
    base: Color,
    hover: Color,
    pressed: Color,
    fg: Color,
    t: &Tokens,
    status: button::Status,
) -> button::Style {
    let (background, text_color) = match status {
        button::Status::Active => (base, fg),
        button::Status::Hovered => (hover, fg),
        button::Status::Pressed => (pressed, fg),
        button::Status::Disabled => (t.pressed_bg, t.text_disabled),
    };
    button::Style {
        background: bg(background),
        text_color,
        border: border::rounded(radius_sm()),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn button_primary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    filled(
        t.primary,
        t.primary_hover,
        t.primary_pressed,
        t.on_primary,
        t,
        status,
    )
}

pub fn button_danger(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    filled(
        t.danger,
        t.danger_hover,
        t.danger_pressed,
        t.on_danger,
        t,
        status,
    )
}

pub fn button_secondary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let (background, text_color, border_color) = match status {
        button::Status::Active => (t.surface_raised, t.text_primary, t.border_strong),
        button::Status::Hovered => (t.hover_bg, t.text_primary, t.border_strong),
        button::Status::Pressed => (t.pressed_bg, t.text_primary, t.border_strong),
        button::Status::Disabled => (t.surface_raised, t.text_disabled, t.border),
    };
    button::Style {
        background: bg(background),
        text_color,
        border: border::rounded(radius_sm()).width(1).color(border_color),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Text-only button; background only on hover/press.
pub fn button_ghost(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let (background, text_color) = match status {
        button::Status::Active => (None, t.text_primary),
        button::Status::Hovered => (bg(t.hover_bg), t.text_primary),
        button::Status::Pressed => (bg(t.pressed_bg), t.text_primary),
        button::Status::Disabled => (None, t.text_disabled),
    };
    button::Style {
        background,
        text_color,
        border: border::rounded(radius_sm()),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Square icon button (toolbar actions); secondary-coloured icon.
pub fn button_icon(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let (background, text_color) = match status {
        button::Status::Active => (None, t.text_secondary),
        button::Status::Hovered => (bg(t.hover_bg), t.text_primary),
        button::Status::Pressed => (bg(t.pressed_bg), t.text_primary),
        button::Status::Disabled => (None, t.text_disabled),
    };
    button::Style {
        background,
        text_color,
        border: border::rounded(radius_sm()),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Icon button in the "on" state (e.g. an open contextual panel).
pub fn button_icon_active(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Pressed => t.pressed_bg,
        _ => t.selected_bg,
    };
    button::Style {
        background: bg(background),
        text_color: if status == button::Status::Disabled {
            t.text_disabled
        } else {
            t.link
        },
        border: border::rounded(radius_sm()),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Inline link-like button.
pub fn button_link(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let text_color = match status {
        button::Status::Disabled => t.text_disabled,
        button::Status::Hovered | button::Status::Pressed => t.primary_hover,
        button::Status::Active => t.link,
    };
    button::Style {
        background: None,
        text_color,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Sidebar row (room list item); `selected` keeps the highlighted background.
pub fn button_list_item(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let background = match (selected, status) {
            (true, button::Status::Pressed) => bg(t.pressed_bg),
            (true, _) => bg(t.sidebar_selected_bg),
            (false, button::Status::Hovered) => bg(t.sidebar_hover_bg),
            (false, button::Status::Pressed) => bg(t.sidebar_selected_bg),
            (false, _) => None,
        };
        button::Style {
            background,
            text_color: if status == button::Status::Disabled {
                t.text_disabled
            } else {
                t.text_primary
            },
            border: border::rounded(radius_sm()),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Item of a popover/context menu.
pub fn button_menu_item(danger: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let fg = if danger {
            t.danger_text
        } else {
            t.text_primary
        };
        let (background, text_color) = match status {
            button::Status::Active => (None, fg),
            button::Status::Hovered => (bg(t.hover_bg), fg),
            button::Status::Pressed => (bg(t.pressed_bg), fg),
            button::Status::Disabled => (None, t.text_disabled),
        };
        button::Style {
            background,
            text_color,
            border: border::rounded(radius_sm()),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Reaction pill; `mine` marks reactions the current user added.
pub fn button_reaction(mine: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let (background, border_color) = match (mine, status) {
            (true, button::Status::Hovered | button::Status::Pressed) => {
                (t.primary_tint, t.primary_hover)
            }
            (true, _) => (t.primary_tint, t.primary),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                (t.hover_bg, t.border_strong)
            }
            (false, _) => (t.surface_sunken, t.border),
        };
        button::Style {
            background: bg(background),
            text_color: match (status, mine) {
                (button::Status::Disabled, _) => t.text_disabled,
                (_, true) => t.link,
                (_, false) => t.text_secondary,
            },
            border: border::rounded(RADIUS_PILL).width(1).color(border_color),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Transparent button used to wrap arbitrary content (rail items, avatars).
pub fn button_bare(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    button::Style {
        background: None,
        text_color: if status == button::Status::Disabled {
            t.text_disabled
        } else {
            t.text_primary
        },
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

// ---------------------------------------------------------------- containers

fn surface(color: Color, text: Color) -> container::Style {
    container::Style {
        text_color: Some(text),
        background: bg(color),
        ..container::Style::default()
    }
}

pub fn container_room(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    surface(t.surface_room, t.text_primary)
}

pub fn container_sidebar(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    surface(t.surface_sidebar, t.text_primary)
}

pub fn container_rail(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    surface(t.surface_rail, t.text_primary)
}

/// Room header / composer area: room surface with a bottom border look.
pub fn container_header(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    surface(t.surface_room, t.text_primary)
}

pub fn container_card(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.text_primary),
        background: bg(t.surface_raised),
        border: border::rounded(radius_lg()).width(1).color(t.border),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn container_popover(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.text_primary),
        background: bg(t.surface_raised),
        border: border::rounded(radius_md()).width(1).color(t.border),
        shadow: shadow(t, 4.0, 16.0),
        snap: true,
    }
}

pub fn container_modal(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.text_primary),
        background: bg(t.surface_raised),
        border: border::rounded(radius_lg()).width(1).color(t.border),
        shadow: shadow(t, 8.0, 32.0),
        snap: true,
    }
}

pub fn container_backdrop(theme: &Theme) -> container::Style {
    container::Style {
        background: bg(tokens(theme).backdrop),
        ..container::Style::default()
    }
}

pub fn container_tooltip(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.tooltip_fg),
        background: bg(t.tooltip_bg),
        border: border::rounded(radius_sm()),
        shadow: shadow(t, 2.0, 8.0),
        snap: true,
    }
}

pub fn container_code_block(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.code_fg),
        background: bg(t.code_bg),
        border: border::rounded(radius_sm()).width(1).color(t.border),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn container_inline_code(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.code_fg),
        background: bg(t.code_bg),
        border: border::rounded(radius_sm()),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Quote body (pair with [`container_quote_bar`] on its left).
pub fn container_quote(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.text_primary),
        background: bg(t.quote_bg),
        border: border::rounded(
            border::Radius::default()
                .top_right(radius_sm())
                .bottom_right(radius_sm()),
        ),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn container_quote_bar(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: bg(t.quote_bar),
        border: border::rounded(
            border::Radius::default()
                .top_left(radius_sm())
                .bottom_left(radius_sm()),
        ),
        ..container::Style::default()
    }
}

pub fn container_mention(me: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let (b, f) = if me {
            (t.mention_me_bg, t.mention_me_fg)
        } else {
            (t.mention_bg, t.mention_fg)
        };
        container::Style {
            text_color: Some(f),
            background: bg(b),
            border: border::rounded(radius_sm()),
            ..container::Style::default()
        }
    }
}

/// Background for a message row: highlighted when it mentions the user or is hovered.
pub fn container_message(highlight: bool, hovered: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let background = if highlight {
            bg(t.highlight_bg)
        } else if hovered {
            bg(t.hover_bg)
        } else {
            None
        };
        container::Style {
            text_color: Some(t.text_primary),
            background,
            ..container::Style::default()
        }
    }
}

pub fn container_divider(theme: &Theme) -> container::Style {
    container::Style {
        background: bg(tokens(theme).border),
        ..container::Style::default()
    }
}

pub fn container_selected(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.link),
        background: bg(t.selected_bg),
        border: border::rounded(radius_sm()),
        ..container::Style::default()
    }
}

fn banner(bg_color: Color, fg: Color, border_color: Color) -> container::Style {
    container::Style {
        text_color: Some(fg),
        background: bg(bg_color),
        border: border::rounded(radius_sm())
            .width(1)
            .color(border_color.scale_alpha(0.35)),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn container_banner_info(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    banner(t.info_bg, t.info_fg, t.primary)
}

pub fn container_banner_warning(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    banner(t.warning_bg, t.warning_fg, t.warning)
}

pub fn container_banner_error(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    banner(t.error_bg, t.error_fg, t.danger)
}

pub fn container_banner_success(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    banner(t.success_bg, t.success_fg, t.success)
}

/// Full-width flat strip (connection status above the room list or timeline).
pub fn container_strip(kind: crate::widget::BannerKind) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let (b, f) = kind.colors(t);
        container::Style {
            text_color: Some(f),
            background: bg(b),
            ..container::Style::default()
        }
    }
}

// ---------------------------------------------------------------- inputs

pub fn text_input(theme: &Theme, status: w_text_input::Status) -> w_text_input::Style {
    input_style(tokens(theme), status, false)
}

pub fn text_input_error(theme: &Theme, status: w_text_input::Status) -> w_text_input::Style {
    input_style(tokens(theme), status, true)
}

/// Borderless input for the composer (the surrounding container draws the frame).
pub fn text_input_bare(theme: &Theme, status: w_text_input::Status) -> w_text_input::Style {
    let t = tokens(theme);
    w_text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: t.text_secondary,
        placeholder: t.text_hint,
        value: if matches!(status, w_text_input::Status::Disabled) {
            t.text_disabled
        } else {
            t.text_primary
        },
        selection: t.selection,
    }
}

fn input_style(t: &Tokens, status: w_text_input::Status, error: bool) -> w_text_input::Style {
    let border_color = match status {
        _ if error => t.danger,
        w_text_input::Status::Active => t.border_strong,
        w_text_input::Status::Hovered => t.text_hint,
        w_text_input::Status::Focused { .. } => t.border_focus,
        w_text_input::Status::Disabled => t.border,
    };
    let width = if matches!(status, w_text_input::Status::Focused { .. }) {
        2.0
    } else {
        1.0
    };
    let (background, value) = match status {
        w_text_input::Status::Disabled => (t.surface_sunken, t.text_disabled),
        _ => (t.surface_raised, t.text_primary),
    };
    w_text_input::Style {
        background: Background::Color(background),
        border: border::rounded(radius_sm())
            .width(width)
            .color(border_color),
        icon: t.text_secondary,
        placeholder: t.text_hint,
        value,
        selection: t.selection,
    }
}

/// Frame around the composer.
pub fn container_composer(focused: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        container::Style {
            text_color: Some(t.text_primary),
            background: bg(t.surface_raised),
            border: border::rounded(radius_md()).width(1).color(if focused {
                t.border_focus
            } else {
                t.border_strong
            }),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

pub fn pick_list(theme: &Theme, status: w_pick_list::Status) -> w_pick_list::Style {
    let t = tokens(theme);
    let border_color = match status {
        w_pick_list::Status::Active => t.border_strong,
        w_pick_list::Status::Hovered => t.text_hint,
        w_pick_list::Status::Opened { .. } => t.border_focus,
    };
    w_pick_list::Style {
        text_color: t.text_primary,
        placeholder_color: t.text_hint,
        handle_color: t.text_secondary,
        background: Background::Color(t.surface_raised),
        border: border::rounded(radius_sm()).width(1).color(border_color),
    }
}

/// Dropdown list of a pick list / combo box (`.menu_style(style::menu)`).
pub fn menu(theme: &Theme) -> overlay::menu::Style {
    let t = tokens(theme);
    overlay::menu::Style {
        background: Background::Color(t.surface_raised),
        border: border::rounded(radius_sm()).width(1).color(t.border),
        text_color: t.text_primary,
        selected_text_color: t.text_primary,
        selected_background: Background::Color(t.hover_bg),
        shadow: shadow(t, 4.0, 16.0),
    }
}

pub fn checkbox(theme: &Theme, status: w_checkbox::Status) -> w_checkbox::Style {
    let t = tokens(theme);
    let (checked, hovered, disabled) = match status {
        w_checkbox::Status::Active { is_checked } => (is_checked, false, false),
        w_checkbox::Status::Hovered { is_checked } => (is_checked, true, false),
        w_checkbox::Status::Disabled { is_checked } => (is_checked, false, true),
    };
    let fill = if disabled {
        t.text_disabled
    } else if hovered {
        t.primary_hover
    } else {
        t.primary
    };
    w_checkbox::Style {
        background: Background::Color(if checked { fill } else { t.surface_raised }),
        icon_color: t.on_primary,
        border: border::rounded(radius_sm())
            .width(if checked { 0.0 } else { 1.0 })
            .color(if hovered {
                t.text_hint
            } else {
                t.border_strong
            }),
        text_color: Some(if disabled {
            t.text_disabled
        } else {
            t.text_primary
        }),
    }
}

pub fn toggler(theme: &Theme, status: w_toggler::Status) -> w_toggler::Style {
    let t = tokens(theme);
    let (on, hovered, disabled) = match status {
        w_toggler::Status::Active { is_toggled } => (is_toggled, false, false),
        w_toggler::Status::Hovered { is_toggled } => (is_toggled, true, false),
        w_toggler::Status::Disabled { is_toggled } => (is_toggled, false, true),
    };
    let track = match (on, hovered, disabled) {
        (_, _, true) => t.pressed_bg,
        (true, true, _) => t.primary_hover,
        (true, false, _) => t.primary,
        (false, true, _) => t.text_hint,
        (false, false, _) => t.border_strong,
    };
    w_toggler::Style {
        background: Background::Color(track),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: Background::Color(Color::WHITE),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: Some(if disabled {
            t.text_disabled
        } else {
            t.text_primary
        }),
        border_radius: None,
        padding_ratio: 0.12,
    }
}

/// Thin, unobtrusive scrollbars.
pub fn scrollable(theme: &Theme, status: w_scrollable::Status) -> w_scrollable::Style {
    let t = tokens(theme);
    let (hovered, dragged) = match status {
        w_scrollable::Status::Active { .. } => (false, false),
        w_scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered,
            is_horizontal_scrollbar_hovered,
            ..
        } => (
            is_vertical_scrollbar_hovered || is_horizontal_scrollbar_hovered,
            false,
        ),
        w_scrollable::Status::Dragged {
            is_vertical_scrollbar_dragged,
            is_horizontal_scrollbar_dragged,
            ..
        } => (
            false,
            is_vertical_scrollbar_dragged || is_horizontal_scrollbar_dragged,
        ),
    };
    let scroller_color = if dragged {
        t.text_hint
    } else if hovered {
        t.text_hint.scale_alpha(0.8)
    } else {
        t.border_strong
    };
    let rail = w_scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: w_scrollable::Scroller {
            background: Background::Color(scroller_color),
            border: border::rounded(RADIUS_PILL),
        },
    };
    w_scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: w_scrollable::AutoScroll {
            background: Background::Color(t.surface_raised),
            border: border::rounded(RADIUS_PILL).width(1).color(t.border),
            shadow: shadow(t, 2.0, 8.0),
            icon: t.text_secondary,
        },
    }
}
