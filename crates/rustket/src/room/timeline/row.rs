//! One timeline row: author, body, reactions, thread/discussion summaries,
//! hover toolbar and context menu. Everything is owned (`'static`) so rows
//! can live inside `lazy`.

use chrono::{DateTime, Local, TimeZone, Utc};
use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element};
use rocketchat::models::Message as RcMessage;
use rocketchat::{RcDate, ServerUrl};
use rust_i18n::t;
use rustket_core::config::UiPrefs;
use rustket_core::room_list::Avatar;
use rustket_core::timeline::{DeliveryState, TimelineItem};
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    AvatarShape, AvatarSource, ChipKind, MESSAGE_AVATAR, MessageRowProps, Placement,
    TooltipPosition, avatar, chip, context_menu, discussion_summary, icon_button_with, message_row,
    message_toolbar, popover_menu, reaction_pill, sanitize_bidi, system_message_row,
    thread_summary, tooltip,
};

use super::actions::{self, Act};
use super::interact::{self, Msg as Message, Source};
use crate::images::Images;
use crate::room::render::{self, RenderCtx, RenderState, emoji_element, emoji_piece, system};

type El = Element<'static, Message>;

/// Borrowed view data a row needs.
#[derive(Clone, Copy)]
pub(crate) struct Env<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub ws: &'a Workspace,
    pub images: &'a Images,
    pub render: &'a RenderState,
    pub prefs: &'a UiPrefs,
    pub rid: &'a str,
    pub source: &'a Source,
}

impl<'a> Env<'a> {
    pub fn rc(&self) -> RenderCtx<'a> {
        RenderCtx {
            server_id: self.server_id,
            server: self.server,
            workspace: self.ws,
            images: self.images,
            state: self.render,
            clock_24h: self.prefs.clock_24h,
            use_real_names: self.ws.use_real_names(self.prefs.use_real_names),
            interactive: true,
        }
    }
}

/// Per-row view state.
#[derive(Debug, Clone, Copy, Hash, Default)]
pub(crate) struct RowFlags {
    pub sequential: bool,
    pub menu_open: bool,
    pub thread_unread: bool,
    /// Jumped-to message (permalink, search result).
    pub selected: bool,
}

pub(crate) fn time_label(ts: Option<RcDate>, clock_24h: bool) -> String {
    let Some(local) = ts.and_then(|d| Local.timestamp_millis_opt(d.timestamp_millis()).single())
    else {
        return String::new();
    };
    local
        .format(if clock_24h { "%H:%M" } else { "%-I:%M %p" })
        .to_string()
}

/// "Today", "Yesterday" or the long date of `ts`.
pub(crate) fn day_label(ts: RcDate, now: DateTime<Local>) -> String {
    let Some(local) = Local.timestamp_millis_opt(ts.timestamp_millis()).single() else {
        return String::new();
    };
    let days = now
        .date_naive()
        .signed_duration_since(local.date_naive())
        .num_days();
    match days {
        0 => t!("room_timeline.today").into_owned(),
        1 => t!("room_timeline.yesterday").into_owned(),
        _ => {
            let v = rocketchat::models::md::TimestampValue {
                timestamp: (ts.timestamp_millis() / 1000).to_string(),
                format: Some("D".into()),
            };
            render::format_timestamp(&v, true)
        }
    }
}

pub(crate) use render::plural;

/// Display name of a message author (alias, real name or username).
pub(crate) fn author_name(m: &RcMessage, real_names: bool) -> String {
    let name = m
        .alias
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .or_else(|| {
            real_names
                .then_some(m.u.name.as_deref())
                .flatten()
                .filter(|n| !n.trim().is_empty())
        })
        .or(m.u.username.as_deref())
        .unwrap_or_default();
    sanitize_bidi(name).into_owned()
}

/// Which of the images row `m` shows are loaded (or answered with
/// initials): rows rebuild when one of their own images arrives, not on
/// every image load.
pub(crate) fn images_key(env: &Env<'_>, m: &RcMessage) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    let mut probe = |url: &str| {
        (env.images.contains(url) || env.images.shows_initials(url)).hash(&mut h);
    };
    let custom_emoji = |text: &str, probe: &mut dyn FnMut(&str)| {
        if text.len() > 8192 || !text.contains(':') {
            return;
        }
        for code in text.split(':').filter(|c| {
            !c.is_empty()
                && c.len() <= 64
                && c.chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '+'))
        }) {
            if let Some(url) = env
                .ws
                .emoji()
                .resolve(code)
                .and_then(|e| e.custom_url(env.server))
            {
                probe(&url);
            }
        }
    };
    if let Some(e) = m.emoji.as_deref().filter(|e| !e.is_empty()) {
        custom_emoji(&format!(":{}:", e.trim_matches(':')), &mut probe);
    } else if let Some(url) = m.avatar.as_deref().filter(|u| !u.is_empty()) {
        probe(&env.server.absolute(url));
    } else if let Some(u) = m.u.username.as_deref() {
        probe(&Images::avatar_url(
            env.server,
            &Avatar::User {
                username: u.to_owned(),
            },
        ));
    }
    custom_emoji(&m.msg, &mut probe);
    for key in m.reactions.iter().flat_map(|r| r.keys()) {
        custom_emoji(key, &mut probe);
    }
    if m.tcount.unwrap_or(0) > 0 {
        for u in m
            .replies
            .iter()
            .flatten()
            .take(3)
            .filter_map(|uid| username_of(env, uid))
        {
            probe(&Images::avatar_url(
                env.server,
                &Avatar::User { username: u },
            ));
        }
    }
    fn walk(
        list: &[rocketchat::models::MessageAttachment],
        server: &ServerUrl,
        probe: &mut dyn FnMut(&str),
        depth: usize,
    ) {
        if depth > 3 {
            return;
        }
        for a in list {
            for url in [&a.image_url, &a.author_icon, &a.thumb_url]
                .into_iter()
                .flatten()
                .filter(|u| !u.is_empty())
            {
                probe(&server.absolute(url));
            }
            if let Some(n) = &a.attachments {
                walk(n, server, probe, depth + 1);
            }
        }
    }
    if let Some(list) = &m.attachments {
        walk(list, env.server, &mut probe, 0);
    }
    for img in m.urls.iter().flatten().filter_map(render::preview_image) {
        probe(&img);
    }
    h.finish()
}

fn author_avatar(env: &Env<'_>, m: &RcMessage, name: &str) -> El {
    let rc = env.rc();
    let username = m.u.username.clone().unwrap_or_default();
    let face: El = if let Some(e) = m.emoji.as_deref().filter(|e| !e.is_empty()) {
        container(emoji_element(emoji_piece(e, &rc), 28.0))
            .center(MESSAGE_AVATAR)
            .into()
    } else if let Some(url) = m.avatar.as_deref().filter(|u| !u.is_empty()) {
        let abs = env.server.absolute(url);
        let src = AvatarSource::initials(name, username.clone()).with_image(
            env.images
                .get(env.server_id, &abs)
                .map(|l| l.handle.clone()),
        );
        avatar(src, MESSAGE_AVATAR, AvatarShape::Rounded).into()
    } else {
        let src = env.images.avatar(
            env.server_id,
            env.server,
            &Avatar::User {
                username: username.clone(),
            },
            name,
        );
        avatar(src, MESSAGE_AVATAR, AvatarShape::Rounded).into()
    };
    if username.is_empty() {
        return face;
    }
    button(face)
        .padding(0)
        .style(style::button_bare)
        .on_press(Message::OpenUser(username))
        .into()
}

fn mentions_me(env: &Env<'_>, m: &RcMessage) -> bool {
    let me_id = env.ws.user_id();
    if me_id.is_some_and(|id| id == m.u.id) {
        return false;
    }
    m.mentions.as_ref().is_some_and(|list| {
        list.iter()
            .any(|x| x.id == "all" || x.id == "here" || me_id.is_some_and(|id| id == x.id))
    })
}

fn reactions(env: &Env<'_>, m: &RcMessage, can_react: bool) -> Option<El> {
    let list = m.reactions.as_ref()?;
    let rc = env.rc();
    let me = env.ws.username();
    let mut pills: Vec<El> = Vec::new();
    for (key, r) in list {
        if r.usernames.is_empty() {
            continue;
        }
        let piece = emoji_piece(key, &rc);
        let mine = me.is_some_and(|u| r.usernames.iter().any(|x| x == u));
        let who = match (&r.names, rc.use_real_names) {
            (Some(names), true) if names.len() == r.usernames.len() => names.join(", "),
            _ => r.usernames.join(", "),
        };
        let pill = reaction_pill(
            emoji_element::<Message>(piece, 16.0),
            render_count(r.usernames.len()),
            mine,
            can_react.then(|| Message::React(m.id.clone(), key.clone())),
        );
        pills.push(tooltip(
            pill,
            t!(
                "room_timeline.reacted_with",
                users = sanitize_bidi(&who),
                emoji = key.as_str()
            )
            .into_owned(),
            TooltipPosition::Top,
        ));
    }
    if pills.is_empty() {
        return None;
    }
    if can_react {
        pills.push(icon_button_with(
            Icon::SmilePlus,
            Some(Message::Act(m.id.clone(), Act::React)),
            t!("room_timeline.act_react").into_owned(),
            false,
            26.0,
        ));
    }
    Some(
        row(pills)
            .spacing(SPACE_1)
            .wrap()
            .vertical_spacing(SPACE_1)
            .into(),
    )
}

fn render_count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn username_of(env: &Env<'_>, uid: &str) -> Option<String> {
    if env.ws.user_id() == Some(uid) {
        return env.ws.username().map(str::to_owned);
    }
    let thread = match env.source {
        Source::Thread(tmid) => env.ws.thread(tmid),
        Source::Room => None,
    };
    thread
        .into_iter()
        .chain(env.ws.timeline(env.rid))
        .flat_map(|t| t.messages().rev())
        .find(|m| m.u.id == uid)
        .and_then(|m| m.u.username.clone())
}

fn thread_part(env: &Env<'_>, m: &RcMessage, unread: bool) -> Option<El> {
    let count = m.tcount.filter(|c| *c > 0)?;
    let label = t!(
        format!("room_timeline.replies_{}", plural(count)),
        count = count
    )
    .into_owned();
    let last = m
        .tlm
        .map(|d| {
            t!(
                "room_timeline.last_reply",
                time = crate::format::short_time(d, Local::now(), env.prefs.clock_24h)
            )
            .into_owned()
        })
        .unwrap_or_default();
    let participants: Vec<El> = m
        .replies
        .iter()
        .flatten()
        .take(3)
        .filter_map(|uid| username_of(env, uid))
        .map(|u| {
            let src = env.images.avatar(
                env.server_id,
                env.server,
                &Avatar::User {
                    username: u.clone(),
                },
                &u,
            );
            avatar(src, 18.0, AvatarShape::Rounded).into()
        })
        .collect();
    Some(thread_summary(
        label,
        last,
        participants,
        unread,
        Message::OpenThread(m.id.clone()),
    ))
}

fn discussion_part(env: &Env<'_>, m: &RcMessage) -> Option<El> {
    let drid = m.drid.clone()?;
    let count = m.dcount.unwrap_or(0);
    let label = if count == 0 {
        t!("room_timeline.discussion_empty").into_owned()
    } else {
        t!(
            format!("room_timeline.messages_{}", plural(count)),
            count = count
        )
        .into_owned()
    };
    let last = m
        .dlm
        .map(|d| crate::format::short_time(d, Local::now(), env.prefs.clock_24h))
        .unwrap_or_default();
    Some(discussion_summary(label, last, Message::OpenRoom(drid)))
}

fn toolbar(acts: &[Act], mid: &str, menu_open: bool) -> El {
    let mut buttons: Vec<El> = acts
        .iter()
        .filter(|a| a.in_toolbar())
        .map(|a| {
            icon_button_with(
                a.icon(),
                Some(Message::Act(mid.to_owned(), *a)),
                a.label(),
                false,
                28.0,
            )
        })
        .collect();
    let rest: Vec<Act> = acts.iter().copied().filter(|a| !a.in_toolbar()).collect();
    if !rest.is_empty() {
        let entries = actions::menu_entries(&rest, |a| Message::Act(mid.to_owned(), a));
        buttons.push(popover_menu(
            icon_button_with(
                Icon::Ellipsis,
                Some(Message::Menu(if menu_open {
                    None
                } else {
                    Some(mid.to_owned())
                })),
                t!("room_timeline.more").into_owned(),
                menu_open,
                28.0,
            ),
            &entries,
            menu_open,
            Message::Menu(None),
            Placement::BelowEnd,
        ));
    }
    message_toolbar(buttons)
}

/// A system message row (`t` set).
fn system_row(env: &Env<'_>, m: &RcMessage) -> El {
    let rc = env.rc();
    let kind = m.t.as_deref().unwrap_or_default();
    let mut col =
        column![text(system::system_text(m, rc.use_real_names)).size(TEXT_SMALL)].spacing(SPACE_1);
    if kind == "message_pinned" || kind == "message_pinned_e2e" {
        col = col.push(render::message_body_with(m, &rc).map(Message::render));
    }
    if let Some(d) = discussion_part(env, m) {
        col = col.push(d);
    }
    system_message_row(
        system::system_icon(kind),
        col,
        time_label(m.ts, env.prefs.clock_24h),
    )
}

/// Builds the row of `item`.
pub(crate) fn build(env: &Env<'_>, item: &TimelineItem, flags: RowFlags, now: DateTime<Utc>) -> El {
    let m = &item.message;
    if m.is_system() {
        return system_row(env, m);
    }
    let rc = env.rc();
    let name = author_name(m, rc.use_real_names);
    let body: El = if m.is_encrypted() && m.msg.is_empty() {
        text(t!("room_render.encrypted").into_owned())
            .size(TEXT_SMALL)
            .style(style::text_hint)
            .into()
    } else {
        render::message_body_with(m, &rc).map(Message::render)
    };
    let mut p = MessageRowProps::new(
        author_avatar(env, m, &name),
        name,
        time_label(m.ts, env.prefs.clock_24h),
        body,
    );
    if m.bot.is_some() {
        p.author_badges.push(chip(
            t!("room_timeline.bot").into_owned(),
            ChipKind::Primary,
        ));
    }
    if let Some(u) = m.u.username.as_deref().filter(|u| {
        m.alias
            .as_deref()
            .is_some_and(|a| !a.trim().is_empty() && a != *u)
            && !u.is_empty()
    }) {
        p.author_badges.push(
            text(format!("@{}", sanitize_bidi(u)))
                .size(TEXT_CAPTION)
                .style(style::text_hint)
                .into(),
        );
    }
    p.is_sequential = flags.sequential;
    p.edited = m.is_edited() && env.ws.settings().show_edited_status();
    p.pinned = m.pinned == Some(true);
    p.starred = env.ws.user_id().is_some_and(|id| m.is_starred_by(id));
    p.is_mention_highlight = mentions_me(env, m);
    p.is_selected = flags.selected;
    p.edited_label = match (&m.edited_by, m.edited_at) {
        (Some(by), Some(at)) => t!(
            "room_timeline.edited_by",
            user = sanitize_bidi(by.username.as_deref().unwrap_or_default()),
            time = crate::format::short_time(at, Local::now(), env.prefs.clock_24h)
        )
        .into_owned(),
        _ => t!("room_timeline.edited").into_owned(),
    };
    p.pinned_label = t!("room_timeline.pinned").into_owned();
    p.starred_label = t!("room_timeline.starred").into_owned();
    match &item.state {
        DeliveryState::Sent => {
            let acts = interact::available(env.ws, env.source, m, now);
            let can_react = acts.contains(&Act::React);
            p.reactions = reactions(env, m, can_react);
            // The thread view shows the replies below the parent instead.
            if *env.source == Source::Room {
                p.thread_summary = thread_part(env, m, flags.thread_unread);
            }
            if let Some(d) = discussion_part(env, m) {
                p.attachments.push(d);
            }
            if !acts.is_empty() {
                p.actions_toolbar = Some(toolbar(&acts, &m.id, flags.menu_open));
            }
            let row = message_row(p);
            if acts.is_empty() {
                return row;
            }
            let entries = actions::menu_entries(&acts, |a| Message::Act(m.id.clone(), a));
            context_menu(row, entries)
        }
        DeliveryState::Sending => {
            p.is_pending = true;
            message_row(p)
        }
        DeliveryState::Failed { .. } => {
            // Error line with Retry and Discard side by side.
            let link = |label: String, msg: Message| {
                button(text(label).size(TEXT_CAPTION).font(FONT_SEMIBOLD))
                    .padding([0, 4])
                    .style(style::button_link)
                    .on_press(msg)
            };
            p.attachments.push(
                row![
                    icon::sized(Icon::CircleAlert, 14.0).style(style::text_danger),
                    text(t!("room_timeline.not_sent").into_owned())
                        .size(TEXT_CAPTION)
                        .style(style::text_danger),
                    link(
                        t!("room_timeline.retry").into_owned(),
                        Message::Retry(m.id.clone())
                    ),
                    link(
                        t!("room_timeline.discard").into_owned(),
                        Message::Discard(m.id.clone())
                    ),
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center)
                .into(),
            );
            message_row(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocketchat::models::UserRef;

    #[test]
    fn plurals() {
        assert_eq!(plural(1), "one");
        assert_eq!(plural(21), "one");
        assert_eq!(plural(11), "many");
        assert_eq!(plural(3), "few");
        assert_eq!(plural(13), "many");
        assert_eq!(plural(25), "many");
        assert_eq!(plural(0), "many");
    }

    #[test]
    fn author_names() {
        let mut m = RcMessage {
            u: UserRef {
                id: "u".into(),
                username: Some("bob".into()),
                name: Some("Bob Builder".into()),
            },
            ..Default::default()
        };
        assert_eq!(author_name(&m, false), "bob");
        assert_eq!(author_name(&m, true), "Bob Builder");
        m.alias = Some("Deploy Bot".into());
        assert_eq!(author_name(&m, true), "Deploy Bot");
        m.alias = None;
        m.u.name = Some("\u{202E}evil".into());
        assert_eq!(author_name(&m, true), "evil");
    }
}
