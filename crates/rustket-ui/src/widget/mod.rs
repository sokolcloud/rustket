//! Reusable widgets. Every function returns an `Element` (or a builder convertible
//! into one) and takes plain data plus messages.

mod arrow_keys;
mod avatar;
mod badge;
mod button;
mod clamp;
mod ellipsis;
mod feedback;
mod flag;
mod focus_ring;
mod header;
mod hover_box;
mod inert;
mod input;
mod keyed_list;
mod layer;
mod menu;
mod message;
mod misc;
mod overlay;
mod popover;
mod presence;
mod reorder;
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
pub use badge::{
    BADGE_HEIGHT, BULLET_SIZE, BadgeCap, BadgeKind, BadgePlace, BadgeShape, GHOST_SHADOW,
    WorkspaceAt, WorkspaceBadge, badge, badge_in, badge_label, badge_label_in, bullet_in, dot,
    dot_in, workspace_badge,
};
pub use button::{
    ActionButton, ButtonKind, ButtonSize, action_button, danger_button, ghost_button, icon_button,
    icon_button_with, primary_button, secondary_button,
};
pub use clamp::{Clamp, clamp};
pub use ellipsis::{Ellipsis, ellipsis, sanitize_bidi};
pub use feedback::{BannerKind, banner, status_strip};
pub use flag::{FlagMark, flag_glyph, flag_mark, flag_swatch};
pub use focus_ring::{
    FOCUS_RING_WIDTH, FocusRegion, FocusRing, draw_ring, focus_region, focus_ring, focus_within,
    ring_colors,
};
pub use header::{header_bar, header_gap};
pub use hover_box::{HoverBox, hover_box};
pub use inert::{Inert, inert};
pub use input::{TextField, text_field};
pub use keyed_list::{KeyedColumn, keyed_list, keyed_list_keys};
pub use layer::focus_cycle;
pub use menu::{
    MENU_MAX_WIDTH, MENU_WIDTH, MenuEntry, MenuItem, context_menu, menu_nav, menu_panel,
    menu_width, popover_menu,
};
pub use message::{
    GUTTER, MESSAGE_AVATAR, MessageRowProps, discussion_summary, message_row, message_toolbar,
    row_style_in, system_message_row, thread_summary, thread_summary_kind,
};
pub use misc::{
    ChipKind, TooltipPosition, above, chip, divider, kbd, slot, tooltip, vertical_divider,
};
pub use overlay::{TOAST_WIDTH, Toast, dialog, modal, modal_keyed, toasts, toasts_with};
pub use popover::{Placement, popover};
pub use presence::{Presence, presence_dot};
pub use reorder::{ReorderArea, ReorderHandle, reorder_area};
pub use side_panel::{MIN_MAIN_WIDTH, MIN_SIDE_WIDTH, side_panel};
pub use sidebar::{
    Density, RailIcon, RoomListItemProps, RowColors, add_server_button, room_list_item, row_colors,
    row_fonts, section_header, section_header_full, section_header_rc, section_header_with,
    workspace_rail_item, workspace_rail_item_with,
};
pub use spinner::{empty_state, loading_placeholder, spinner, spinner_colored};
pub use split::{HANDLE_WIDTH, split};
pub use timeline::{date_separator, emoji_text, reaction_pill, unread_separator};
pub use window_chrome::{
    RESIZE_EDGE, TITLE_BAR_HEIGHT, TITLE_BAR_ITEM, WINDOW_CONTROL_WIDTH, WindowControlLabels,
    resize_cursor, title_bar_button, title_bar_workspace, title_bar_workspace_with,
    window_controls, window_frame, window_titled,
};
