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
