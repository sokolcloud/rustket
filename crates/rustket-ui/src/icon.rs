//! Lucide icons rendered with the bundled lucide font.
//!
//! The font must be registered at startup (see [`crate::theme::fonts`]); icons are
//! plain `Text` widgets using the `lucide` font family, so `.size()`, `.color()` and
//! `.style()` work as for text.
//!
//! Icons used by the app (all variants of [`Icon`] are available):
//! `Send`, `Paperclip`, `Smile`, `AtSign`, `Hash`, `Lock`, `Users`, `User`,
//! `MessageSquare`, `MessageCircle` (threads), `MessagesSquare` (discussions),
//! `Search`, `Settings`, `Bell`, `BellOff`, `Star`, `Pin`, `Pencil`, `Trash2`, `Copy`,
//! `Link`, `Reply`/`CornerUpLeft`, `Forward`, `Ellipsis`, `EllipsisVertical`, `X`,
//! `Check`, `CheckCheck`, `ChevronDown`/`Right`/`Left`/`Up`, `Plus`, `LogOut`, `Globe`,
//! `Shield`, `Key`, `Video`, `Phone`, `Mic`, `Image`, `File`, `Download`,
//! `ExternalLink`, `Eye`, `EyeOff`, `Info`, `TriangleAlert`, `RefreshCw`, `House`,
//! `Inbox`, `List`, `Funnel`, `Sun`, `Moon`, `Menu`, `ArrowDown`, `LoaderCircle`.

use iced::Font;
use iced::alignment;
use iced::widget::Text;
use iced::widget::text::LineHeight;

pub use lucide_icons::Icon;

/// Font family of the bundled lucide font.
pub const ICON_FONT: Font = Font::with_name("lucide");

/// Default icon size used across the UI.
pub const ICON_SIZE: f32 = 18.0;

/// Creates an icon glyph. The glyph box is square (line height 1) so icons align
/// predictably with text when centred in rows.
pub fn icon<'a>(icon: Icon) -> Text<'a> {
    sized(icon, ICON_SIZE)
}

/// Creates an icon glyph of the given size.
pub fn sized<'a>(icon: Icon, size: f32) -> Text<'a> {
    Text::new(char::from(icon).to_string())
        .font(ICON_FONT)
        .size(size)
        .line_height(LineHeight::Relative(1.0))
        .width(size)
        .height(size)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
}

/// Icons the app relies on; used by tests to make sure the font contains them.
pub const APP_ICONS: &[Icon] = &[
    Icon::Send,
    Icon::Paperclip,
    Icon::Smile,
    Icon::AtSign,
    Icon::Hash,
    Icon::Lock,
    Icon::Users,
    Icon::User,
    Icon::MessageSquare,
    Icon::MessageCircle,
    Icon::MessagesSquare,
    Icon::Search,
    Icon::Settings,
    Icon::Bell,
    Icon::BellOff,
    Icon::Star,
    Icon::Pin,
    Icon::Pencil,
    Icon::Trash2,
    Icon::Copy,
    Icon::Link,
    Icon::Reply,
    Icon::CornerUpLeft,
    Icon::Forward,
    Icon::Ellipsis,
    Icon::EllipsisVertical,
    Icon::X,
    Icon::Check,
    Icon::CheckCheck,
    Icon::ChevronDown,
    Icon::ChevronRight,
    Icon::ChevronLeft,
    Icon::ChevronUp,
    Icon::Plus,
    Icon::LogOut,
    Icon::Globe,
    Icon::Shield,
    Icon::Key,
    Icon::Video,
    Icon::Phone,
    Icon::Mic,
    Icon::Image,
    Icon::File,
    Icon::Download,
    Icon::ExternalLink,
    Icon::Eye,
    Icon::EyeOff,
    Icon::Info,
    Icon::TriangleAlert,
    Icon::RefreshCw,
    Icon::House,
    Icon::Inbox,
    Icon::List,
    Icon::Funnel,
    Icon::Sun,
    Icon::Moon,
    Icon::Menu,
    Icon::ArrowDown,
    Icon::LoaderCircle,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icons_map_to_private_use_codepoints() {
        for icon in APP_ICONS {
            let c = u32::from(char::from(*icon));
            assert!((0xE000..=0xF8FF).contains(&c), "{icon:?} -> {c:#x}");
        }
    }
}
