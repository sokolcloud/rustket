//! Apps-engine UIKit: the JSON model of message blocks and views, and the
//! interaction protocol (docs/uikit.md). Decoding is tolerant: anything
//! unknown or malformed becomes an `Unknown(Value)` node that renders nothing.

pub mod blocks;
pub mod interaction;
pub mod text;
pub mod view;

pub use blocks::{
    Actionable, BlockMeta, CalloutVariant, Confirm, ENGINE, Element, Icon, InfoCardRow,
    LayoutBlock, OptionGroup, PreviewBlock, PreviewImage, Slot, Surface, UiOption,
    resolve_conditionals,
};
pub use interaction::{
    ActionButtonPayload, BlockActionPayload, Container, InteractionResponse, ServerInteraction,
    UserInteraction, ViewClosedPayload, ViewPayload, normalize_errors,
};
pub use text::{StrOrText, TextI18n, TextKind, TextObject};
pub use view::{BannerView, ContextualBarView, ModalView, RawView, ViewKind};

use serde_json::Value;

use crate::models::Message;

/// Decodes a `blocks` array. Lists outside the size guard (the `md` limits:
/// depth, value count, string bytes) decode to nothing, so a hostile message
/// cannot make decoding expensive; at most [`blocks::MAX_BLOCKS`] blocks are kept.
pub fn decode_blocks(v: &Value) -> Vec<LayoutBlock> {
    let Value::Array(items) = v else {
        return Vec::new();
    };
    if !crate::models::md::within_limits(v) {
        tracing::debug!("dropping oversized UIKit blocks");
        return Vec::new();
    }
    items
        .iter()
        .take(blocks::MAX_BLOCKS)
        .map(LayoutBlock::from_value)
        .collect()
}

impl Message {
    /// Decoded UIKit `blocks` (empty when absent or invalid).
    pub fn uikit_blocks(&self) -> Vec<LayoutBlock> {
        self.blocks.as_ref().map(decode_blocks).unwrap_or_default()
    }

    /// Whether the message has a non-empty `blocks` array, which hides the
    /// markdown body (§3.1; `blocks: []` does not). Lists dropped by the size
    /// guard count as absent, so the body is shown instead.
    pub fn has_blocks(&self) -> bool {
        self.blocks.as_ref().is_some_and(|v| {
            v.as_array().is_some_and(|a| !a.is_empty()) && crate::models::md::within_limits(v)
        })
    }
}
