//! The flag block shared by the sidebar rows, the room header menu and the
//! thread panel: a caption, one checked item per color (with its swatch and
//! the user's name) and "Remove flag". Strings: `locales/sidebar.yml`.

use rocketchat::models::RoomType;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::flags::{FlagColor, Flags, valid_id};
use rustket_ui::icon::Icon;
use rustket_ui::widget::{FlagMark, MenuEntry, MenuItem};

/// The user's name of `c`, else its default name.
pub fn flag_name(flags: &Flags, c: FlagColor) -> String {
    match flags.name(c) {
        Some(n) => n.to_owned(),
        None => t!(c.i18n_key()).into_owned(),
    }
}

/// The row mark of `c` (its name is the tooltip).
pub fn mark(flags: &Flags, c: FlagColor) -> FlagMark {
    FlagMark::new(c.index(), flag_name(flags, c))
}

/// Hash of every color's name: menus listing them rebuild when one changes.
pub fn names_key(flags: &Flags) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    for c in FlagColor::ALL {
        flags.name(c).hash(&mut h);
    }
    crate::i18n::cache_tag().hash(&mut h);
    h.finish()
}

/// Whether room `rid` can carry a flag: the user is in it, and it is not an
/// omnichannel or service room.
pub fn can_flag_room(ws: &Workspace, rid: &str) -> bool {
    let Some(e) = ws.room(rid) else {
        return false;
    };
    e.sub.is_some()
        && valid_id(rid)
        && !e.is_service_room()
        && !matches!(e.room_type(), RoomType::Livechat | RoomType::Voip)
}

/// Whether threads of room `rid` can carry a flag (flagged threads are
/// listed only while threads are enabled on the server).
pub fn can_flag_thread(ws: &Workspace, rid: &str) -> bool {
    ws.settings().threads_enabled() && can_flag_room(ws, rid)
}

/// The flag block: "Flag", the six colors (`current` checked) and "Remove
/// flag" while one is set. `on` builds the message of a choice.
pub fn entries<M>(
    flags: &Flags,
    current: Option<FlagColor>,
    on: impl Fn(Option<FlagColor>) -> M,
) -> Vec<MenuEntry<M>> {
    let mut v = vec![MenuEntry::Header(t!("sidebar.menu_flag").into_owned())];
    for c in FlagColor::ALL {
        v.push(
            MenuItem::new(flag_name(flags, c), on(Some(c)))
                .swatch(c.index())
                .checked(current == Some(c))
                .into(),
        );
    }
    if current.is_some() {
        v.push(
            MenuItem::new(t!("sidebar.menu_flag_clear"), on(None))
                .icon(Icon::FlagOff)
                .into(),
        );
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels<M>(v: &[MenuEntry<M>]) -> Vec<String> {
        v.iter()
            .map(|e| match e {
                MenuEntry::Item(i) => i.label.clone(),
                MenuEntry::Header(h) => format!("# {h}"),
                MenuEntry::Separator => "--".into(),
            })
            .collect()
    }

    #[test]
    fn entries_list_every_color_and_the_current_one() {
        rust_i18n::set_locale("en");
        let flags = Flags::default();
        let v = entries(&flags, None, |c| c);
        assert_eq!(
            labels(&v),
            [
                "# Flag", "Red", "Orange", "Yellow", "Green", "Blue", "Purple"
            ]
        );
        let v = entries(&flags, Some(FlagColor::Blue), |c| c);
        assert_eq!(labels(&v).last().map(String::as_str), Some("Remove flag"));
        let checked: Vec<_> = v
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) if i.checked == Some(true) => Some((i.swatch, i.on_select)),
                _ => None,
            })
            .collect();
        assert_eq!(
            checked,
            [(Some(FlagColor::Blue.index()), Some(FlagColor::Blue))]
        );
        let MenuEntry::Item(clear) = &v[7] else {
            panic!("no clear item");
        };
        assert_eq!(clear.on_select, None);
        assert!(matches!(clear.icon, Some(Icon::FlagOff)));
    }
}
