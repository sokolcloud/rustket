//! Building a message list (room timeline or thread): which rows to show
//! with day separators, the unread divider and grouping ([`plan`]), and the
//! `lazy` row widgets with everything they depend on in their key ([`row`]).

use std::hash::{Hash, Hasher};

use chrono::{DateTime, Datelike, Local, Utc};
use iced::advanced::widget::operation::{Operation, Outcome};
use iced::widget::{column, lazy};
use iced::{Element, Length, Rectangle, Task, Vector};
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::timeline::{
    DeliveryState, Timeline as CoreTimeline, TimelineItem, is_new_day, is_sequential,
};
use rustket_core::workspace::Dirty;
use rustket_ui::widget::{date_separator, unread_separator};

use super::interact::Msg;
use super::row::{self, Env, RowFlags};
use super::system;
use crate::room::ViewCtx;
use crate::room::render;

pub(crate) fn item_key(item: &TimelineItem) -> (i64, &str) {
    let m = &item.message;
    (
        m.ts.or(m.updated_at).map_or(0, |d| d.timestamp_millis()),
        m.id.as_str(),
    )
}

/// Key of the top sensor in a keyed column (rows use [`id_key`]).
pub(crate) const TOP_KEY: u64 = 0;

pub(crate) fn id_key(id: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    id.hash(&mut h);
    h.finish().max(1)
}

/// Whether system message `m` is shown in room `rid` (hidden types).
pub(crate) fn visible_in<'a>(ws: &'a Workspace, rid: &str) -> impl Fn(&RcMessage) -> bool + 'a {
    let hidden = ws.settings().hide_system_messages();
    let room_sys = ws
        .room(rid)
        .and_then(|e| e.room.as_ref())
        .and_then(|r| r.sys_mes.as_ref());
    move |m: &RcMessage| {
        !m.t.as_deref()
            .is_some_and(|t| m.is_system() && system::is_hidden(t, &hidden, room_sys))
    }
}

/// Room/subscription state the row actions depend on (read-only, muted,
/// room roles …); `Dirty::ROOMS` would change with every new message.
fn room_state_key(ws: &Workspace, rid: &str) -> u64 {
    use rustket_core::permissions as perm;
    let mut h = std::hash::DefaultHasher::new();
    (
        perm::can_post(ws, rid),
        perm::can_react(ws, rid),
        perm::can_pin(ws, rid),
        perm::can_star(ws, rid),
        perm::is_read_only(ws, rid),
    )
        .hash(&mut h);
    let sub = ws.subscription(rid);
    sub.is_some().hash(&mut h);
    sub.and_then(|s| s.roles.as_ref()).hash(&mut h);
    h.finish()
}

fn locale_key() -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    rust_i18n::locale().hash(&mut h);
    h.finish()
}

/// One row to build, with its separators and grouping.
#[derive(Debug)]
pub(crate) struct PlannedRow<'a> {
    pub item: &'a TimelineItem,
    pub new_day: bool,
    pub unread_before: bool,
    pub sequential: bool,
}

/// What a list shows.
#[derive(Debug)]
pub(crate) struct Plan<'a> {
    pub rows: Vec<PlannedRow<'a>>,
    /// Visible messages newer than the frozen view (behind "jump to latest").
    pub new_below: usize,
    /// More visible loaded messages exist above the window.
    pub more_above: bool,
}

/// Picks the newest `window` visible messages (skipping what waits below a
/// frozen view), with day separators, the unread divider (above the first
/// visible unread message) and grouping. One extra older row is used as
/// grouping context only.
pub(crate) fn plan<'a, Tz: chrono::TimeZone>(
    tl: &'a CoreTimeline,
    visible: impl Fn(&RcMessage) -> bool,
    frozen: Option<(i64, &str)>,
    window: usize,
    me: &str,
    grouping_secs: i64,
    tz: &Tz,
) -> Plan<'a> {
    let divider_key = tl
        .unread_divider(me)
        .and_then(|id| tl.get(id))
        .map(item_key);
    let mut divider: Option<&'a str> = None;
    let mut reached_divider = divider_key.is_none();
    let mut new_below = 0usize;
    let mut picked: Vec<&'a TimelineItem> = Vec::with_capacity(window.min(1024) + 1);
    let mut exhausted = true;
    for item in tl.iter().rev() {
        let key = item_key(item);
        if let Some(dk) = divider_key
            && key <= dk
        {
            reached_divider = true;
        }
        if !visible(&item.message) {
            continue;
        }
        if let Some(dk) = divider_key
            && key >= dk
            && !item.is_pending()
            && item.message.u.id != me
        {
            divider = Some(item.message.id.as_str());
        }
        if let Some(f) = frozen
            && key > f
        {
            new_below += 1;
            continue;
        }
        if picked.len() > window {
            exhausted = false;
            break;
        }
        picked.push(item);
    }
    if !(reached_divider || exhausted) {
        divider = None;
    }
    let more_above = picked.len() > window;
    picked.reverse();
    let mut rows = Vec::with_capacity(picked.len());
    for (i, item) in picked.iter().enumerate() {
        if more_above && i == 0 {
            continue;
        }
        let m = &item.message;
        let prev = i.checked_sub(1).map(|p| &picked[p].message);
        rows.push(PlannedRow {
            item,
            new_day: m.ts.is_some() && is_new_day(prev, m, tz),
            unread_before: divider == Some(m.id.as_str()),
            sequential: prev.is_some_and(|p| is_sequential(p, m, grouping_secs, tz, divider)),
        });
    }
    Plan {
        rows,
        new_below,
        more_above,
    }
}

/// View-wide inputs of every row key, computed once per frame.
#[derive(Clone, Copy)]
pub(crate) struct Common {
    dark: bool,
    revs: (u64, u64, u64, u64),
    prefs: (bool, bool),
    locale: u64,
    pub now: DateTime<Utc>,
    minute: i64,
    now_local: DateTime<Local>,
    today: i32,
}

impl Common {
    pub fn new(ctx: &ViewCtx<'_>, rid: &str) -> Self {
        let ws = ctx.workspace;
        let now = Utc::now();
        let now_local = Local::now();
        Self {
            dark: render::mode() == rustket_ui::theme::Mode::Dark,
            revs: (
                ws.revision(Dirty::SETTINGS | Dirty::PERMISSIONS | Dirty::ROLES | Dirty::ME),
                ws.revision(Dirty::EMOJI),
                room_state_key(ws, rid),
                ctx.images.generation(ctx.server_id),
            ),
            prefs: (
                ctx.prefs.clock_24h,
                ws.use_real_names(ctx.prefs.use_real_names),
            ),
            locale: locale_key(),
            now,
            // Edit/delete limits are time based: own rows rebuild every minute.
            minute: now.timestamp() / 60,
            now_local,
            today: now_local.date_naive().num_days_from_ce(),
        }
    }
}

/// `lazy` dependency of a row.
#[derive(Clone, Copy)]
struct RowKey {
    msg: u64,
    state: u8,
    render: u64,
    flags: RowFlags,
    /// Day separator, unread divider above the row.
    separators: (bool, bool),
    dark: bool,
    prefs: (bool, bool),
    revs: (u64, u64, u64, u64),
    /// Which of the row's images are loaded.
    images: u64,
    locale: u64,
    /// Current minute for own rows (time-limited edit/delete), else 0.
    minute: i64,
    /// Current day for rows with a day separator ("Today"/"Yesterday").
    today: i32,
}

impl Hash for RowKey {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.msg.hash(h);
        self.state.hash(h);
        self.render.hash(h);
        self.flags.hash(h);
        self.separators.hash(h);
        self.dark.hash(h);
        self.prefs.hash(h);
        self.revs.hash(h);
        self.images.hash(h);
        self.locale.hash(h);
        self.minute.hash(h);
        self.today.hash(h);
    }
}

/// The `lazy` widget of planned row `r` (with its day separator and the
/// unread divider above it).
pub(crate) fn row<'a>(
    env: Env<'a>,
    common: &Common,
    r: &PlannedRow<'a>,
    flags: RowFlags,
) -> Element<'a, Msg> {
    let item: &'a TimelineItem = r.item;
    let m = &item.message;
    let state = match &item.state {
        DeliveryState::Sent => 0u8,
        DeliveryState::Sending => 1,
        DeliveryState::Failed { .. } => 2,
    };
    let own = env.ws.user_id().is_some_and(|me| m.u.id == me);
    let day = r
        .new_day
        .then(|| m.ts.map(|ts| row::day_label(ts, common.now_local)));
    let dep = RowKey {
        msg: render::message_key(m),
        state,
        render: env.render.key(&m.id),
        flags,
        separators: (r.new_day, r.unread_before),
        dark: common.dark,
        prefs: common.prefs,
        revs: common.revs,
        images: row::images_key(&env, m),
        locale: common.locale,
        minute: if own { common.minute } else { 0 },
        today: if r.new_day { common.today } else { 0 },
    };
    let unread_before = r.unread_before;
    let now = common.now;
    lazy(dep, move |_| {
        let row = row::build(&env, item, flags, now);
        if day.is_none() && !unread_before {
            return row;
        }
        let mut c = column![].width(Length::Fill);
        if let Some(Some(label)) = &day {
            c = c.push(date_separator(label.clone()));
        }
        if unread_before {
            c = c.push(unread_separator(t!("room_timeline.unread").into_owned()));
        }
        c.push(row).into()
    })
    .into()
}

/// Finds the bounds of a scrollable's viewport and content and of a row in it.
struct Locate {
    scroll: iced::widget::Id,
    row: iced::widget::Id,
    viewport: Option<(Rectangle, Rectangle)>,
    found: Option<Rectangle>,
}

impl Operation<(Rectangle, Rectangle, Rectangle)> for Locate {
    fn traverse(
        &mut self,
        operate: &mut dyn FnMut(&mut dyn Operation<(Rectangle, Rectangle, Rectangle)>),
    ) {
        operate(self);
    }

    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: Rectangle) {
        if id == Some(&self.row) {
            self.found = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        _translation: Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.viewport = Some((bounds, content_bounds));
        }
    }

    fn finish(&self) -> Outcome<(Rectangle, Rectangle, Rectangle)> {
        match (self.viewport, self.found) {
            (Some((v, c)), Some(r)) => Outcome::Some((v, c, r)),
            _ => Outcome::None,
        }
    }
}

/// Offset (from the start, or from the end when `anchor_bottom`) that
/// centres `row` in the viewport of a scrollable.
pub(crate) fn centre_offset(
    viewport: Rectangle,
    content: Rectangle,
    row: Rectangle,
    anchor_bottom: bool,
) -> f32 {
    let max = (content.height - viewport.height).max(0.0);
    let from_top =
        (row.y - content.y - (viewport.height - row.height).max(0.0) / 2.0).clamp(0.0, max);
    if anchor_bottom {
        max - from_top
    } else {
        from_top
    }
}

/// Scrolls row `row` (a container with that id) of scrollable `scroll`
/// into the middle of the view.
pub(crate) fn reveal<M: Send + 'static>(
    scroll: iced::widget::Id,
    row: iced::widget::Id,
    anchor_bottom: bool,
) -> Task<M> {
    let target = scroll.clone();
    iced::advanced::widget::operate(Locate {
        scroll,
        row,
        viewport: None,
        found: None,
    })
    .then(move |(viewport, content, row)| {
        let y = centre_offset(viewport, content, row, anchor_bottom);
        iced::widget::operation::scroll_to(
            target.clone(),
            iced::widget::operation::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn centring() {
        let r = |y: f32, h: f32| Rectangle {
            x: 0.0,
            y,
            width: 100.0,
            height: h,
        };
        // Content 1000 px tall from y=50 in a 200 px viewport; row at 500.
        let (v, c, row) = (r(50.0, 200.0), r(50.0, 1000.0), r(550.0, 40.0));
        assert_eq!(centre_offset(v, c, row, false), 420.0);
        assert_eq!(centre_offset(v, c, row, true), 800.0 - 420.0);
        // Clamped at both ends.
        assert_eq!(centre_offset(v, c, r(50.0, 40.0), false), 0.0);
        assert_eq!(centre_offset(v, c, r(1040.0, 10.0), false), 800.0);
    }

    use rocketchat::RcDate;
    use rocketchat::models::UserRef;

    fn m(id: &str, uid: &str, ts: i64, t: Option<&str>) -> RcMessage {
        RcMessage {
            id: id.into(),
            rid: "r".into(),
            msg: id.into(),
            t: t.map(Into::into),
            ts: RcDate::from_millis(ts),
            u: UserRef {
                id: uid.into(),
                username: Some(uid.into()),
                name: None,
            },
            ..Default::default()
        }
    }

    fn ids<'a>(p: &Plan<'a>) -> Vec<&'a str> {
        p.rows.iter().map(|r| r.item.message.id.as_str()).collect()
    }

    const T0: i64 = 1_800_000_000_000;

    fn timeline() -> CoreTimeline {
        let mut tl = CoreTimeline::new_room("r");
        tl.extend([
            m("a", "bob", T0, None),
            m("b", "bob", T0 + 1_000, None),
            m("j", "carol", T0 + 2_000, Some("uj")),
            m("c", "carol", T0 + 3_000, None),
            m("d", "me", T0 + 4_000, None),
            m("e", "bob", T0 + 5_000, None),
        ]);
        tl
    }

    #[test]
    fn plan_groups_and_hides() {
        let tl = timeline();
        let visible = |m: &RcMessage| m.t.as_deref() != Some("uj");
        let p = plan(&tl, visible, None, 60, "me", 300, &Utc);
        assert_eq!(ids(&p), ["a", "b", "c", "d", "e"]);
        assert!(!p.more_above);
        assert_eq!(p.new_below, 0);
        let seq: Vec<bool> = p.rows.iter().map(|r| r.sequential).collect();
        assert_eq!(seq, [false, true, false, false, false]);
        assert!(p.rows[0].new_day && !p.rows[1].new_day);
        // Window of 2: one extra row of context, not shown.
        let p = plan(&tl, visible, None, 2, "me", 300, &Utc);
        assert_eq!(ids(&p), ["d", "e"]);
        assert!(p.more_above);
        assert!(!p.rows[0].new_day, "the context row continues the day");
        // Frozen at "c": newer ones wait below.
        let p = plan(&tl, visible, Some((T0 + 3_000, "c")), 60, "me", 300, &Utc);
        assert_eq!(ids(&p), ["a", "b", "c"]);
        assert_eq!(p.new_below, 2);
    }

    #[test]
    fn plan_of_a_thread() {
        // Thread timelines hold replies only (grouped like room messages);
        // pending replies are planned too, and there is no unread divider.
        let mut tl = CoreTimeline::new_thread("r", "p");
        let reply = |id: &str, uid: &str, ts: i64| {
            let mut x = m(id, uid, ts, None);
            x.tmid = Some("p".into());
            x
        };
        tl.extend([reply("a", "bob", T0), reply("b", "bob", T0 + 1_000)]);
        tl.insert_pending(reply("c", "me", T0 + 2_000));
        let p = plan(&tl, |_| true, None, usize::MAX - 1, "me", 300, &Utc);
        assert_eq!(ids(&p), ["a", "b", "c"]);
        assert!(p.rows.iter().all(|r| !r.unread_before));
        // Replies of one author group like room messages.
        let seq: Vec<bool> = p.rows.iter().map(|r| r.sequential).collect();
        assert_eq!(seq, [false, true, false]);
    }

    #[test]
    fn unread_divider_skips_hidden_messages() {
        let mut tl = timeline();
        // Last seen after "b": the first unread is the hidden join.
        tl.set_unread_since(RcDate::from_millis(T0 + 1_500));
        assert_eq!(tl.unread_divider("me"), Some("j"));
        let visible = |m: &RcMessage| m.t.as_deref() != Some("uj");
        let p = plan(&tl, visible, None, 60, "me", 300, &Utc);
        let marked: Vec<&str> = p
            .rows
            .iter()
            .filter(|r| r.unread_before)
            .map(|r| r.item.message.id.as_str())
            .collect();
        assert_eq!(marked, ["c"]);
        // Shown when the join is visible.
        let p = plan(&tl, |_| true, None, 60, "me", 300, &Utc);
        assert!(
            p.rows
                .iter()
                .any(|r| r.unread_before && r.item.message.id == "j")
        );
        // Older than the window: no divider (and no wrong one).
        let p = plan(&tl, visible, None, 1, "me", 300, &Utc);
        assert!(p.rows.iter().all(|r| !r.unread_before));
    }
}
