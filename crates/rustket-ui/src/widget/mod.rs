//! Reusable widgets. Every function returns an `Element` (or a builder convertible
//! into one) and takes plain data plus messages.

mod arrow_keys;
mod avatar;
mod badge;
mod button;
mod ellipsis;
mod feedback;
mod header;
mod hover_box;
mod input;
mod keyed_list;
mod layer;
mod menu;
mod message;
mod misc;
mod overlay;
mod popover;
mod presence;
mod sidebar;
mod spinner;
mod split;
mod timeline;

pub use arrow_keys::arrow_keys;
pub use avatar::{
    AVATAR_COLORS, Avatar, AvatarShape, AvatarSource, avatar, avatar_color, initials,
};
pub use badge::{BADGE_HEIGHT, BadgeKind, badge, badge_label, dot};
pub use button::{
    ActionButton, ButtonKind, ButtonSize, action_button, danger_button, ghost_button, icon_button,
    icon_button_with, primary_button, secondary_button,
};
pub use ellipsis::{Ellipsis, ellipsis, sanitize_bidi};
pub use feedback::{BannerKind, banner, status_strip};
pub use header::{header_bar, header_gap};
pub use hover_box::{HoverBox, hover_box};
pub use input::{TextField, text_field};
pub use keyed_list::{KeyedColumn, keyed_list, keyed_list_keys};
pub use layer::focus_cycle;
pub use menu::{
    MENU_MAX_WIDTH, MENU_WIDTH, MenuEntry, MenuItem, context_menu, menu_panel, menu_width,
    popover_menu,
};
pub use message::{
    GUTTER, MESSAGE_AVATAR, MessageRowProps, discussion_summary, message_row, message_toolbar,
    system_message_row, thread_summary,
};
pub use misc::{
    ChipKind, TooltipPosition, above, chip, divider, kbd, slot, tooltip, vertical_divider,
};
pub use overlay::{TOAST_WIDTH, Toast, dialog, modal, modal_keyed, toasts, toasts_with};
pub use popover::{Placement, popover};
pub use presence::{Presence, presence_dot};
pub use sidebar::{
    Density, RailIcon, RoomListItemProps, add_server_button, room_list_item, section_header,
    workspace_rail_item,
};
pub use spinner::{empty_state, loading_placeholder, spinner, spinner_colored};
pub use split::{HANDLE_WIDTH, split};
pub use timeline::{date_separator, emoji_text, reaction_pill, unread_separator};
