//! Reusable widgets. Every function returns an `Element` (or a builder convertible
//! into one) and takes plain data plus messages.

mod arrow_keys;
mod avatar;
mod badge;
mod button;
mod ellipsis;
mod feedback;
mod focus_ring;
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
pub mod selectable;
mod side_panel;
mod sidebar;
mod spinner;
mod split;
mod timeline;
mod window_chrome;

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
pub use focus_ring::{
    FOCUS_RING_WIDTH, FocusRegion, FocusRing, draw_ring, focus_region, focus_ring, focus_within,
    ring_colors,
};
pub use header::{header_bar, header_gap};
pub use hover_box::{HoverBox, hover_box};
pub use input::{TextField, text_field};
pub use keyed_list::{KeyedColumn, keyed_list, keyed_list_keys};
pub use layer::focus_cycle;
pub use menu::{
    MENU_MAX_WIDTH, MENU_WIDTH, MenuEntry, MenuItem, context_menu, menu_nav, menu_panel,
    menu_width, popover_menu,
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
pub use side_panel::{MIN_MAIN_WIDTH, MIN_SIDE_WIDTH, side_panel};
pub use sidebar::{
    Density, RailIcon, RoomListItemProps, add_server_button, room_list_item, section_header,
    workspace_rail_item,
};
pub use spinner::{empty_state, loading_placeholder, spinner, spinner_colored};
pub use split::{HANDLE_WIDTH, split};
pub use timeline::{date_separator, emoji_text, reaction_pill, unread_separator};
pub use window_chrome::{
    RESIZE_EDGE, TITLE_BAR_HEIGHT, TITLE_BAR_ITEM, WINDOW_CONTROL_WIDTH, WindowControlLabels,
    resize_cursor, title_bar_button, title_bar_workspace, window_controls, window_frame,
};
