//! Avatars: image or initials on a deterministic colour, with presence/badge overlays.

use iced::widget::{container, image, stack, text};
use iced::{Color, Element, Length, Theme, alignment, border};

use super::presence::{Presence, presence_dot};
use unicode_segmentation::UnicodeSegmentation;

use crate::emoji::{EMOJI_FONT, first_emoji, is_emoji_grapheme};
use crate::theme::{Tokens, font_semibold, tokens};

/// What to show in an avatar.
#[derive(Debug, Clone)]
pub struct AvatarSource {
    /// Display name used for initials.
    pub name: String,
    /// Stable key (user id, username, room id) used for the background colour.
    pub key: String,
    /// Loaded image, if any; initials are shown otherwise.
    pub image: Option<image::Handle>,
}

impl AvatarSource {
    pub fn initials(name: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            key: key.into(),
            image: None,
        }
    }

    pub fn with_image(mut self, handle: Option<image::Handle>) -> Self {
        self.image = handle;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AvatarShape {
    /// Rounded square (Rocket.Chat style).
    #[default]
    Rounded,
    Circle,
}

type RingColor<'a> = Box<dyn Fn(&Tokens) -> Color + 'a>;

/// Avatar builder; convert into an `Element` when done.
pub struct Avatar<'a, Message> {
    source: AvatarSource,
    size: f32,
    shape: AvatarShape,
    presence: Option<Presence>,
    ring: Option<RingColor<'a>>,
    badge: Option<Element<'a, Message>>,
}

/// Creates an avatar of `size` logical pixels.
pub fn avatar<'a, Message>(
    source: AvatarSource,
    size: f32,
    shape: AvatarShape,
) -> Avatar<'a, Message> {
    Avatar {
        source,
        size,
        shape,
        presence: None,
        ring: None,
        badge: None,
    }
}

impl<'a, Message: 'a> Avatar<'a, Message> {
    /// Adds a presence dot in the bottom-right corner.
    pub fn presence(mut self, presence: Option<Presence>) -> Self {
        self.presence = presence;
        self
    }

    /// Colour of the ring around the presence dot; should match the surface the
    /// avatar sits on (defaults to the room surface).
    pub fn ring(mut self, color: Color) -> Self {
        self.ring = Some(Box::new(move |_| color));
        self
    }

    /// Like [`Avatar::ring`], picking the colour from the active tokens.
    pub fn ring_with(mut self, color: impl Fn(&Tokens) -> Color + 'a) -> Self {
        self.ring = Some(Box::new(color));
        self
    }

    /// Adds a small overlay (e.g. a [`badge`](super::badge)) in the top-right corner.
    pub fn badge(mut self, badge: impl Into<Element<'a, Message>>) -> Self {
        self.badge = Some(badge.into());
        self
    }
}

pub(crate) fn corner_radius(size: f32, shape: AvatarShape) -> f32 {
    match shape {
        AvatarShape::Circle => size / 2.0,
        AvatarShape::Rounded => (size / 8.0).clamp(2.0, 8.0),
    }
}

/// The avatar picture alone, without overlays.
pub(crate) fn face<'a, Message: 'a>(
    source: &AvatarSource,
    size: f32,
    shape: AvatarShape,
) -> Element<'a, Message> {
    let radius = corner_radius(size, shape);
    if let Some(handle) = &source.image {
        return image(handle.clone())
            .width(size)
            .height(size)
            .content_fit(iced::ContentFit::Cover)
            .border_radius(radius)
            .into();
    }
    let label = initials(&source.name);
    let is_emoji = is_emoji_grapheme(&label);
    let font_size = if label.chars().count() > 1 && !is_emoji {
        size * 0.38
    } else {
        size * 0.46
    };
    let background = avatar_color(&source.key);
    container(
        text(label)
            .size(font_size.max(8.0))
            .font(if is_emoji {
                EMOJI_FONT
            } else {
                font_semibold()
            })
            .color(Color::WHITE)
            .wrapping(text::Wrapping::None),
    )
    .width(size)
    .height(size)
    .center(size)
    .style(move |_theme: &Theme| container::Style {
        background: Some(background.into()),
        border: border::rounded(radius),
        text_color: Some(Color::WHITE),
        ..container::Style::default()
    })
    .into()
}

impl<'a, Message: 'a> From<Avatar<'a, Message>> for Element<'a, Message> {
    fn from(a: Avatar<'a, Message>) -> Self {
        let base = face(&a.source, a.size, a.shape);
        if a.presence.is_none() && a.badge.is_none() {
            return base;
        }
        let mut layers = stack![base].width(a.size).height(a.size).clip(false);
        if let Some(presence) = a.presence {
            let dot = (a.size * 0.3).clamp(8.0, 14.0);
            let ring = a.ring;
            let ringed =
                container(presence_dot(presence, dot))
                    .padding(2)
                    .style(move |theme: &Theme| {
                        let t = tokens(theme);
                        let color = ring.as_ref().map_or(t.surface_room, |f| f(t));
                        container::Style {
                            background: Some(color.into()),
                            border: border::rounded(dot),
                            ..container::Style::default()
                        }
                    });
            layers = layers.push(
                container(ringed)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(alignment::Horizontal::Right)
                    .align_y(alignment::Vertical::Bottom)
                    .padding(iced::Padding::ZERO.right(-3.0).bottom(-3.0)),
            );
        }
        if let Some(badge) = a.badge {
            layers = layers.push(
                container(badge)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(alignment::Horizontal::Right)
                    .align_y(alignment::Vertical::Top)
                    .padding(iced::Padding::ZERO.right(-6.0).top(-6.0)),
            );
        }
        layers.into()
    }
}

/// Up to two initials from a display name: first letters of the first two words,
/// or the first emoji when the name has no letters/digits; `"?"` when empty.
pub fn initials(name: &str) -> String {
    let mut words = name
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '@'))
        .filter(|w| !w.is_empty())
        .peekable();
    // A leading emoji (incl. keycaps like 1️⃣) is the whole avatar label.
    if let Some(first) = words.peek()
        && let Some(g) = first.graphemes(true).next()
        && is_emoji_grapheme(g)
    {
        return g.to_owned();
    }
    let mut out = String::new();
    let mut taken = 0;
    for word in words {
        let letter = word
            .graphemes(true)
            .find(|g| !is_emoji_grapheme(g) && g.chars().next().is_some_and(char::is_alphanumeric));
        if let Some(g) = letter {
            out.push_str(&upper_grapheme(g));
            taken += 1;
            if taken == 2 {
                break;
            }
        }
    }
    if !out.is_empty() {
        return out;
    }
    first_emoji(name).map_or_else(|| "?".to_owned(), str::to_owned)
}

/// Uppercases the first character of a grapheme only when that maps one
/// character to one character (keeps "ß", "ﬁ" instead of "SS", "FI").
fn upper_grapheme(g: &str) -> String {
    let mut chars = g.chars();
    let Some(c) = chars.next() else {
        return String::new();
    };
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) => std::iter::once(u).chain(chars).collect(),
        _ => g.to_owned(),
    }
}

/// Avatar background palette: saturated mid-tones that keep white initials readable.
pub const AVATAR_COLORS: [Color; 12] = [
    Color::from_rgb8(0x15, 0x6F, 0xF5),
    Color::from_rgb8(0x10, 0x52, 0x9E),
    Color::from_rgb8(0x12, 0x79, 0x5A),
    Color::from_rgb8(0x0E, 0x6E, 0x7A),
    Color::from_rgb8(0x7B, 0x3F, 0xE4),
    Color::from_rgb8(0x9B, 0x1D, 0xB5),
    Color::from_rgb8(0xC2, 0x18, 0x5B),
    Color::from_rgb8(0xD4, 0x0C, 0x26),
    Color::from_rgb8(0xB3, 0x4A, 0x00),
    Color::from_rgb8(0x8A, 0x5A, 0x00),
    Color::from_rgb8(0x52, 0x5B, 0x66),
    Color::from_rgb8(0x2E, 0x7D, 0x32),
];

/// Deterministic colour for a key (FNV-1a hash, stable across runs and platforms).
pub fn avatar_color(key: &str) -> Color {
    AVATAR_COLORS[(fnv1a(key) % AVATAR_COLORS.len() as u64) as usize]
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::contrast_ratio;

    #[test]
    fn latin_initials() {
        assert_eq!(initials("Alice Smith"), "AS");
        assert_eq!(initials("alice"), "A");
        assert_eq!(initials("john.doe"), "JD");
        assert_eq!(initials("e\u{301}lodie"), "E\u{301}");
        assert_eq!(initials("  Bob   the Builder "), "BT");
        assert_eq!(initials("@carol"), "C");
        assert_eq!(initials("rocket.cat"), "RC");
    }

    #[test]
    fn cyrillic_initials() {
        assert_eq!(initials("Иван Петров"), "ИП");
        assert_eq!(initials("ёжик"), "Ё");
        assert_eq!(initials("général"), "G");
        assert_eq!(initials("ßtraße x"), "ßX");
        assert_eq!(initials("ﬁnn x"), "ﬁX");
        assert_eq!(initials("Jean-Luc Picard"), "JP");
        assert_eq!(initials("ǉubica"), "ǈ".to_uppercase());
    }

    #[test]
    fn emoji_and_symbols() {
        assert_eq!(initials("🚀 Launch team"), "🚀");
        assert_eq!(initials("1️⃣ squad"), "1️⃣");
        assert_eq!(initials("team 1️⃣ squad"), "TS");
        assert_eq!(initials("#general"), "G");
        assert_eq!(initials("🚀"), "🚀");
        assert_eq!(initials("👍🏽"), "👍🏽");
        assert_eq!(initials("👩‍💻 "), "👩‍💻");
        assert_eq!(initials("🇷🇺🇺🇸"), "🇷🇺");
        assert_eq!(initials("❤️"), "❤️");
        assert_eq!(initials("-- !!"), "?");
        assert_eq!(initials("#️⃣"), "#️⃣");
    }

    #[test]
    fn empty_initials() {
        assert_eq!(initials(""), "?");
        assert_eq!(initials("   "), "?");
    }

    #[test]
    fn digits_and_cjk() {
        assert_eq!(initials("42 bots"), "4B");
        assert_eq!(initials("山田 太郎"), "山太");
    }

    #[test]
    fn colour_is_deterministic() {
        assert_eq!(avatar_color("alice"), avatar_color("alice"));
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        let distinct: std::collections::HashSet<_> = [
            "alice", "bob", "carol", "dave", "eve", "frank", "general", "random",
        ]
        .iter()
        .map(|k| (fnv1a(k) % AVATAR_COLORS.len() as u64) as usize)
        .collect();
        assert!(distinct.len() >= 4);
    }

    #[test]
    fn avatar_colours_fit_white_text() {
        for c in AVATAR_COLORS {
            assert!(contrast_ratio(Color::WHITE, c) >= 4.5, "{c:?}");
        }
    }
}
