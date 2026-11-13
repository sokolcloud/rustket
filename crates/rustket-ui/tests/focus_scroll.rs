//! Tab scrolls the focused widget into view, whichever end a scrollable is
//! anchored at.
#![allow(clippy::unwrap_used)]
use iced::advanced::widget::{Id, Operation, operation};
use iced::futures::{FutureExt as _, StreamExt as _};
use iced::widget::{button, column, container, scrollable, text};
use iced::{Element, Rectangle, Size, Vector};
use iced_runtime::user_interface::{Cache, UserInterface};
use rustket_ui::widget::focus_ring;

#[derive(Debug, Clone)]
struct M;

fn view(bottom: bool) -> Element<'static, M> {
    let rows = (0..30).map(|i| {
        focus_ring(button(text(format!("row {i}"))).height(40), Some(M))
            .id(format!("row-{i}"))
            .into()
    });
    let list = scrollable(column(rows)).height(200);
    let list = if bottom { list.anchor_bottom() } else { list };
    container(list).into()
}

struct Probe {
    focused: Option<(Id, Rectangle)>,
    scroll: Option<(Rectangle, Vector)>,
}

impl Operation for Probe {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn focusable(&mut self, id: Option<&Id>, b: Rectangle, state: &mut dyn operation::Focusable) {
        if state.is_focused() {
            self.focused = id.cloned().map(|id| (id, b));
        }
    }
    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        b: Rectangle,
        _c: Rectangle,
        t: Vector,
        _s: &mut dyn operation::Scrollable,
    ) {
        self.scroll = Some((b, t));
    }
}

/// Focuses `start`, presses Tab (or Shift+Tab) and returns the focused id
/// and whether it is inside the scrollable's viewport.
fn tab(bottom: bool, start: &'static str, forward: bool) -> (Id, bool) {
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        iced::Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let mut ui = UserInterface::build(
        view(bottom),
        Size::new(400.0, 400.0),
        Cache::default(),
        &mut renderer,
    );
    let mut op = operation::focusable::focus::<()>(Id::new(start));
    ui.operate(&renderer, &mut op);
    let task = rustket_ui::widget::focus_cycle::<M>(forward);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    while let Some(Some(action)) = stream.next().now_or_never() {
        if let iced_runtime::Action::Widget(op) = action {
            let mut current = Some(op);
            while let Some(mut op) = current.take() {
                ui.operate(&renderer, op.as_mut());
                if let operation::Outcome::Chain(next) = op.finish() {
                    current = Some(next);
                }
            }
        }
    }
    let mut p = Probe {
        focused: None,
        scroll: None,
    };
    ui.operate(&renderer, &mut p);
    let ((id, b), (vp, t)) = (p.focused.unwrap(), p.scroll.unwrap());
    let shown = b.y - t.y >= vp.y - 0.5 && b.y + b.height - t.y <= vp.y + vp.height + 0.5;
    (id, shown)
}

#[test]
fn tab_scrolls_the_focused_row_into_view() {
    // Top-anchored list at the top: row 4 is visible, row 5 is not.
    assert_eq!(tab(false, "row-3", true), (Id::new("row-4"), true));
    assert_eq!(tab(false, "row-4", true), (Id::new("row-5"), true));
    assert_eq!(tab(false, "row-28", true), (Id::new("row-29"), true));
    // Bottom-anchored list (a timeline) at the bottom.
    assert_eq!(tab(true, "row-1", false), (Id::new("row-0"), true));
    assert_eq!(tab(true, "row-20", false), (Id::new("row-19"), true));
    assert_eq!(tab(true, "row-28", true), (Id::new("row-29"), true));
}
