//! Loading indicators and empty states.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::time::Instant;
use iced::widget::{column, container, text};
use iced::{
    Alignment, Color, Element, Event, Length, Rectangle, Size, Theme, border, mouse, window,
};

use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{FONT_SEMIBOLD, SPACE_2, SPACE_3, TEXT_BODY, TEXT_SUBTITLE};

/// Animated ring of dots; takes the surrounding text colour.
pub fn spinner<'a, Message: 'a>(size: f32) -> Element<'a, Message> {
    Element::new(Spinner { size, color: None })
}

/// Spinner with an explicit colour.
pub fn spinner_colored<'a, Message: 'a>(size: f32, color: Color) -> Element<'a, Message> {
    Element::new(Spinner {
        size,
        color: Some(color),
    })
}

const DOTS: usize = 8;
const PERIOD: Duration = Duration::from_millis(900);
const FRAME: Duration = Duration::from_millis(33);

struct Spinner {
    size: f32,
    color: Option<Color>,
}

struct SpinnerState {
    start: Instant,
    now: Instant,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Spinner {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size), Length::Fixed(self.size))
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SpinnerState>()
    }

    fn state(&self) -> tree::State {
        let now = Instant::now();
        tree::State::new(SpinnerState { start: now, now })
    }

    fn layout(
        &mut self,
        _tree: &mut widget::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, self.size, self.size)
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = tree.state.downcast_mut::<SpinnerState>();
            state.now = *now;
            // Animate only while visible; scrolling it back into view redraws anyway.
            if layout.bounds().intersects(viewport) {
                shell.request_redraw_at(window::RedrawRequest::At(*now + FRAME));
            }
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let state = tree.state.downcast_ref::<SpinnerState>();
        let elapsed = state
            .now
            .saturating_duration_since(state.start)
            .as_secs_f32();
        let phase = (elapsed / PERIOD.as_secs_f32()).fract();
        let head = (phase * DOTS as f32).floor() as usize;
        let color = self.color.unwrap_or(style.text_color);
        let bounds = layout.bounds();
        let center = bounds.center();
        let dot = (self.size / 5.0).max(2.0);
        let radius = self.size / 2.0 - dot / 2.0;
        for i in 0..DOTS {
            let angle =
                i as f32 / DOTS as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let age = (head + DOTS - i) % DOTS;
            let alpha = 1.0 - age as f32 / DOTS as f32 * 0.85;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: center.x + radius * angle.cos() - dot / 2.0,
                        y: center.y + radius * angle.sin() - dot / 2.0,
                        width: dot,
                        height: dot,
                    },
                    border: border::rounded(dot / 2.0),
                    snap: false,
                    ..renderer::Quad::default()
                },
                color.scale_alpha(alpha),
            );
        }
    }
}

/// Centred spinner with an optional caption, filling the available space.
pub fn loading_placeholder<'a, Message: 'a>(caption: Option<String>) -> Element<'a, Message> {
    let mut content = column![spinner(28.0)]
        .spacing(SPACE_3)
        .align_x(Alignment::Center);
    if let Some(caption) = caption {
        content = content.push(text(caption).size(TEXT_BODY).style(style::text_secondary));
    }
    container(content).center(Length::Fill).into()
}

/// Large icon, title and subtitle, centred.
pub fn empty_state<'a, Message: 'a>(
    icon: Icon,
    title: impl Into<String>,
    subtitle: impl Into<String>,
) -> Element<'a, Message> {
    let subtitle = subtitle.into();
    let mut content = column![
        icon::sized(icon, 48.0).style(style::text_hint),
        text(title.into()).size(TEXT_SUBTITLE).font(FONT_SEMIBOLD),
    ]
    .spacing(SPACE_2)
    .align_x(Alignment::Center)
    .max_width(360);
    if !subtitle.is_empty() {
        content = content.push(
            text(subtitle)
                .size(TEXT_BODY)
                .style(style::text_secondary)
                .align_x(iced::alignment::Horizontal::Center),
        );
    }
    container(content)
        .center(Length::Fill)
        .padding(SPACE_3)
        .into()
}
