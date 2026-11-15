//! Test helpers that look into iced widget trees: whether a view change kept
//! the widget state of the scrollables and editors in it.

use iced::Element;
use iced::advanced::widget::Tree;
use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::tree::{State, Tag};

type EditorState = iced::widget::text_editor::State<iced::advanced::text::highlighter::PlainText>;
type InputState =
    iced::widget::text_input::State<<iced::Renderer as iced::advanced::text::Renderer>::Paragraph>;

/// Tree tag of a `scrollable`.
pub fn scrollable_tag() -> Tag {
    let e: Element<'_, ()> = iced::widget::scrollable(iced::widget::space()).into();
    e.as_widget().tag()
}

/// Tree tag of a `text_editor` (plain text).
pub fn text_editor_tag() -> Tag {
    let content = iced::widget::text_editor::Content::new();
    let e: Element<'_, ()> = iced::widget::text_editor(&content).into();
    e.as_widget().tag()
}

/// Tree tag of a `text_input`.
pub fn text_input_tag() -> Tag {
    let e: Element<'_, ()> = iced::widget::text_input("", "").into();
    e.as_widget().tag()
}

/// Addresses of the state objects of the trees tagged `tag` in `tree`,
/// depth first.
pub fn states(tree: &Tree, tag: Tag) -> Vec<usize> {
    let mut out = Vec::new();
    collect(tree, tag, &mut out);
    out
}

fn collect(tree: &Tree, tag: Tag, out: &mut Vec<usize>) {
    if tree.tag == tag
        && let State::Some(s) = &tree.state
    {
        out.push((&raw const **s).cast::<()>() as usize);
    }
    for c in &tree.children {
        collect(c, tag, out);
    }
}

/// The focusable state of `tree` when it is a text editor or a text input.
fn focusable(tree: &mut Tree) -> Option<&mut dyn Focusable> {
    if tree.tag == Tag::of::<EditorState>() {
        return Some(tree.state.downcast_mut::<EditorState>());
    }
    if tree.tag == Tag::of::<InputState>() {
        return Some(tree.state.downcast_mut::<InputState>());
    }
    None
}

/// Focuses every editor and input tagged `tag` in `tree`. A freed state's
/// memory can be reused by its replacement, so the address alone may miss a
/// rebuild; a rebuilt state is not focused.
fn focus_all(tree: &mut Tree, tag: Tag) {
    if tree.tag == tag
        && let Some(f) = focusable(tree)
    {
        f.focus();
    }
    for c in &mut tree.children {
        focus_all(c, tag);
    }
}

/// How many editors and inputs tagged `tag` in `tree` have focus, and how
/// many there are.
fn focused(tree: &mut Tree, tag: Tag) -> (usize, usize) {
    let mut n = (0, 0);
    if tree.tag == tag
        && let Some(f) = focusable(tree)
    {
        n = (usize::from(f.is_focused()), 1);
    }
    for c in &mut tree.children {
        let (a, b) = focused(c, tag);
        n = (n.0 + a, n.1 + b);
    }
    n
}

/// `tree` has the shape and widget types of `fresh`.
pub fn tags_match(tree: &Tree, fresh: &Tree) -> bool {
    tree.tag == fresh.tag
        && tree.children.len() == fresh.children.len()
        && tree
            .children
            .iter()
            .zip(&fresh.children)
            .all(|(a, b)| tags_match(a, b))
}

/// Diffs `view` into `tree` and asserts that the result is what a fresh
/// tree of `view` would be and that every widget tagged `tag` before the
/// diff (there must be one) kept its own state object.
pub fn assert_kept<M>(tree: &mut Tree, view: &Element<'_, M>, tag: Tag, step: &str) {
    assert_first_kept(tree, view, tag, usize::MAX, step);
}

/// [`assert_kept`] for the first `n` widgets tagged `tag` (depth first), for
/// changes that legitimately drop later ones (another panel).
pub fn assert_first_kept<M>(
    tree: &mut Tree,
    view: &Element<'_, M>,
    tag: Tag,
    n: usize,
    step: &str,
) {
    let mut before = states(tree, tag);
    before.truncate(n);
    assert!(!before.is_empty(), "{step}: nothing to keep");
    focus_all(tree, tag);
    let (_, inputs) = focused(tree, tag);
    tree.diff(view.as_widget());
    assert!(
        tags_match(tree, &Tree::new(view.as_widget())),
        "{step}: tree does not match the view"
    );
    let after = states(tree, tag);
    // Each widget kept its own state (in the same order: not swapped with
    // another widget of the same type).
    let kept: Vec<usize> = after
        .iter()
        .copied()
        .filter(|s| before.contains(s))
        .collect();
    assert_eq!(
        kept, before,
        "{step}: widget state rebuilt or moved ({before:x?} -> {after:x?})"
    );
    if inputs > 0 {
        let (focused, _) = focused(tree, tag);
        assert!(
            focused >= inputs.min(before.len()),
            "{step}: an editor or input lost its focus (rebuilt)"
        );
    }
}

/// Lays `view` out at 1280x800, focuses `focused` (when given), runs the
/// widget operations of `task` on it (`focus`, `focus_next`…) and returns the
/// ids of the focused widgets afterwards.
pub fn focus_after<M: Send + 'static>(
    view: Element<'_, M>,
    focused: Option<&'static str>,
    task: iced::Task<M>,
) -> Vec<Option<iced::advanced::widget::Id>> {
    use iced::advanced::widget::{Id, Operation};
    use iced::futures::{FutureExt as _, StreamExt as _};
    use iced::{Pixels, Rectangle, Size};
    use iced_runtime::user_interface::{Cache, UserInterface};

    struct Ids(Vec<Option<Id>>);
    impl Operation for Ids {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn Focusable) {
            if state.is_focused() {
                self.0.push(id.cloned());
            }
        }
    }

    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let mut ui = UserInterface::build(
        view,
        Size::new(1280.0, 800.0),
        Cache::default(),
        &mut renderer,
    );
    if let Some(id) = focused {
        let mut op = iced::advanced::widget::operation::focusable::focus::<()>(Id::new(id));
        ui.operate(&renderer, &mut op);
    }
    if let Some(mut stream) = iced_runtime::task::into_stream(task) {
        while let Some(Some(action)) = stream.next().now_or_never() {
            if let iced_runtime::Action::Widget(op) = action {
                // Like the runtime: run chained stages (`focus_next` counts first).
                let mut current = Some(op);
                while let Some(mut op) = current.take() {
                    ui.operate(&renderer, op.as_mut());
                    if let iced::advanced::widget::operation::Outcome::Chain(next) = op.finish() {
                        current = Some(next);
                    }
                }
            }
        }
    }
    let mut ids = Ids(Vec::new());
    ui.operate(&renderer, &mut ids);
    ids.0
}

fn probe_ui<'a, M>(
    view: Element<'a, M>,
) -> (
    iced_runtime::user_interface::UserInterface<'a, M, iced::Theme, iced::Renderer>,
    iced::Renderer,
) {
    use iced::{Pixels, Size};
    use iced_runtime::user_interface::{Cache, UserInterface};
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let ui = UserInterface::build(
        view,
        Size::new(1280.0, 800.0),
        Cache::default(),
        &mut renderer,
    );
    (ui, renderer)
}

/// The texts of the text widgets of `view` laid out at 1280x800.
pub fn texts<M>(view: Element<'_, M>) -> Vec<String> {
    use iced::Rectangle;
    use iced::advanced::widget::{Id, Operation};

    struct Texts(Vec<String>);
    impl Operation for Texts {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, _: Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
    }

    let (mut ui, renderer) = probe_ui(view);
    let mut texts = Texts(Vec::new());
    ui.operate(&renderer, &mut texts);
    texts.0
}

/// Whether a press of `key` (no modifiers) is captured by `view` once the
/// widget `focused` has the focus: an uncaptured key reaches the app's
/// `keyboard::listen()`.
pub fn key_captured<M>(
    view: Element<'_, M>,
    focused: &'static str,
    key: iced::keyboard::Key,
) -> bool {
    use iced::advanced::widget::Id;
    use iced::keyboard;

    struct NoClip;
    impl iced::advanced::Clipboard for NoClip {
        fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
            None
        }
        fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
    }

    let (mut ui, mut renderer) = probe_ui(view);
    let mut op = iced::advanced::widget::operation::focusable::focus::<()>(Id::new(focused));
    ui.operate(&renderer, &mut op);
    let event = iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key.clone(),
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    });
    let mut out = Vec::new();
    let (_, status) = ui.update(
        &[event],
        iced::mouse::Cursor::Unavailable,
        &mut renderer,
        &mut NoClip,
        &mut out,
    );
    status.first() == Some(&iced::event::Status::Captured)
}
