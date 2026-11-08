//! Mouse-selectable rich text (iced 0.14 `text`/`rich_text` are not
//! selectable).
//!
//! - [`rich`] is a drop-in for `rich_text`: the same spans render the same
//!   way (fonts, colours, sizes, span backgrounds such as inline code and
//!   mentions, underlines, links), shaped once into iced's cached paragraph
//!   (reshaped only when the spans or the shape change, resized on width
//!   changes). It draws the selection range its scope gives it, behind the
//!   glyphs, in the theme's `selection` colour.
//! - [`scope`] wraps a subtree (a message body) and keeps one selection over
//!   every [`rich`] block inside it, in document (widget traversal) order, so a
//!   drag can run from a paragraph through a list into a code block. Blocks
//!   outside a scope render and handle links but are not selectable. Scopes
//!   must not be nested.
//!
//! Interaction (inside a scope): press and drag selects (also from the gaps
//! between blocks and past them, clamped to the nearest position);
//! Shift+click extends; double-click selects the word under the pointer,
//! triple-click the whole block; Ctrl+A selects everything in the scope that
//! took the last press (only while no press happened elsewhere since, see
//! [`press_guard`]); Ctrl+C or right-click → Copy copies; a finished mouse
//! selection also goes to the PRIMARY selection; a press elsewhere or Escape
//! clears. Plain-text presses are not captured (rows that act on a click use
//! [`click_area`], which ignores drags). A link captures its press and
//! activates on release without a drag (moving more than [`DRAG_THRESHOLD`]
//! makes it a selection); the released drag is captured so buttons around the
//! text do not fire. Right-clicking the selected text opens the scope's Copy
//! menu instead of the menus of the widgets around it (wrapping context menus
//! forward the right press first, see `context_menu`); a right press anywhere
//! else opens those as usual. Concealed spans (text in the colour of its own
//! background: hidden spoilers) are never highlighted nor copied.
//!
//! Copied text is the shown text of the selected blocks joined by each
//! block's joiner (`"\n"`, `"\t"` for table cells), with a block's copy
//! prefix (list markers) when the selection includes the block start.
//!
//! State lives in the widget tree (the scope's anchor/focus, each block's
//! range), so keyed and `lazy` rows keep their selection across rebuilds.
//! Selection across separate scopes (several messages) is not supported:
//! each message is its own scope, like separate documents.

mod core;
mod guard;
mod rich;
mod scope;
#[cfg(test)]
mod tests;

pub use guard::{click_area, press_guard};
pub use rich::{Rich, rich};
pub use scope::{Scope, scope};

/// How far (px) the cursor may move after a press before it is a drag.
pub const DRAG_THRESHOLD: f32 = 3.0;

use iced::Rectangle;
use iced::advanced::widget::{Id, Operation};
use std::any::Any;

/// An operation that selects all text of every scope it reaches (e.g.
/// `UserInterface::operate`).
#[derive(Debug, Default)]
pub struct SelectAll;

impl Operation for SelectAll {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(&mut self, _: Option<&Id>, _: Rectangle, state: &mut dyn Any) {
        if let Some(scope) = state.downcast_mut::<scope::State>() {
            scope.select_all();
        } else if let Some(core) = state.downcast_mut::<core::Core>()
            && core.scoped
        {
            core.selection = (core.len() > 0).then_some(0..core.len());
        }
    }
}

/// An operation that collects what Ctrl+C would copy from each scope it
/// reaches, in order (empty strings for scopes without a selection).
#[derive(Debug, Default)]
pub struct Copied {
    pub texts: Vec<String>,
}

impl Operation for Copied {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(&mut self, _: Option<&Id>, _: Rectangle, state: &mut dyn Any) {
        if state.is::<scope::State>() {
            self.texts.push(String::new());
        } else if let Some(core) = state.downcast_ref::<core::Core>()
            && let Some(out) = self.texts.last_mut()
        {
            scope::copy_piece(out, core);
        }
    }
}
