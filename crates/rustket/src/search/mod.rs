//! Global message search (`rustket_core::search`): a view in place of the
//! room area (`workspace::MainView::Search`) with the query field, the
//! filter builder and the results grouped by room. Strings:
//! `locales/search.yml`.
//!
//! The query string is the source of truth: the builder parses it, changes
//! one part and writes it back, and the user may type operators directly.
//! The component turns input into [`Out`] requests; the app wiring
//! (`app/search.rs`) starts runs, feeds them the paced server requests and
//! the local hits (offline cache, loaded timelines) and opens messages.

pub mod view;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::time::{Duration, Instant};

use rocketchat::models::{Message as RcMessage, SearchProvider};
use rustket_core::Workspace;
use rustket_core::search::{Has, Pacing, SearchQuery, SearchRun, Target, parse_date};

use crate::error::Failure;

/// Typing pause before a search starts.
pub const DEBOUNCE: Duration = Duration::from_millis(400);
/// Rows shown at first and added by "Show more".
pub const PAGE: usize = 100;
/// Local hits asked of the offline cache.
pub const LOCAL_LIMIT: usize = 200;
/// Id of the query field (focused when the view opens).
pub const QUERY_ID: &str = "global-search-query";
/// Id of the picker fields.
pub const PICKER_ID: &str = "global-search-picker";

/// Suggestions shown by a picker.
const SUGGESTIONS: usize = 8;

/// What the account knows about the server's search, kept while the view
/// is closed (per session; cleared with the account's state).
#[derive(Debug, Default)]
pub struct SearchState {
    /// The session the state belongs to (its pointer).
    pub session: Option<usize>,
    pub provider: Provider,
    /// Id of the next run.
    pub next_run: u64,
    /// The pacing timer is armed for then.
    pub timer: Option<Instant>,
    /// When requests may start, across runs.
    pub pacing: Pacing,
    /// Requests in flight by `(run, target)`: dropping one aborts it
    /// (a run replaced by the next edit).
    pub requests: HashMap<(u64, Target), iced::task::Handle>,
    /// The view closed last, with its run (results, progress): a result
    /// opened and the view reopened shows them again instead of searching
    /// every room anew.
    pub stash: Option<Box<SearchView>>,
}

impl SearchState {
    /// Aborts the requests of every run but `keep`.
    pub fn abort_except(&mut self, keep: &[u64]) {
        self.requests.retain(|(run, _), _| keep.contains(run));
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Provider {
    #[default]
    Unknown,
    /// The lookup is running; `failures`: lookups failed in a row before
    /// it (a retry keeps counting for the back-off).
    Loading {
        failures: u32,
    },
    Known(Option<SearchProvider>),
    /// `rocketchatSearch.getProvider` failed (`failures` times in a row):
    /// `chat.search` per room meanwhile, asked again from `retry_at`.
    Failed {
        failures: u32,
        retry_at: Instant,
    },
}

impl Provider {
    /// The lookup should be (re)started at `now`.
    pub fn due(&self, now: Instant) -> bool {
        match self {
            Provider::Unknown => true,
            Provider::Failed { retry_at, .. } => *retry_at <= now,
            Provider::Loading { .. } | Provider::Known(_) => false,
        }
    }

    /// The state while a lookup started from this one runs.
    pub fn loading(&self) -> Provider {
        let failures = match self {
            Provider::Failed { failures, .. } | Provider::Loading { failures } => *failures,
            Provider::Unknown | Provider::Known(_) => 0,
        };
        Provider::Loading { failures }
    }

    /// The state after a failed lookup: retried after a back-off (30 s
    /// doubling up to 10 min), or the server's rate-limit wait if longer.
    pub fn failed(&self, now: Instant, wait: Option<Duration>) -> Provider {
        let failures = match self {
            Provider::Failed { failures, .. } | Provider::Loading { failures } => failures + 1,
            Provider::Unknown | Provider::Known(_) => 1,
        };
        let backoff = Duration::from_secs(30u64 << failures.saturating_sub(1).min(5))
            .min(Duration::from_secs(600));
        Provider::Failed {
            failures,
            retry_at: now + wait.map_or(backoff, |w| w.max(backoff)),
        }
    }
}

/// A filter picker of the builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    From,
    In,
    After,
    Before,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    /// The debounce of query edit `seq` ran out.
    Debounced(u64),
    /// Enter in the query field.
    Submit,
    Clear,
    Close,
    /// Builder: open a picker (`None`: close it).
    Pick(Option<Picker>),
    PickerInput(String),
    /// Enter in a picker field (the typed value, or the first suggestion).
    PickerSubmit,
    AddFrom(String),
    RemoveFrom(String),
    AddRoom(String),
    RemoveRoom(String),
    /// A date preset of the After picker: this many days back.
    AfterDaysAgo(i64),
    ClearDate(Picker),
    ToggleHas(Has),
    ToggleMentionsMe,
    Open(String),
    ShowMore,
    /// A request of run `run` ended.
    Fetched {
        run: u64,
        target: Target,
        res: Result<Vec<RcMessage>, Failure>,
    },
    /// The offline cache's hits of run `run`.
    Cached {
        run: u64,
        found: Vec<RcMessage>,
    },
    Provider(Result<Option<SearchProvider>, Failure>),
    /// The pacing timer.
    Pace,
}

/// Requests to the app.
#[derive(Debug, Clone)]
pub enum Out {
    Close,
    /// Start a search for the current query (an empty one clears).
    Start,
    /// Arm the debounce for edit `seq`.
    Debounce(u64),
    Open(Box<RcMessage>),
    /// Focus a field.
    Focus(&'static str),
}

/// The open search view.
#[derive(Debug)]
pub struct SearchView {
    pub query: String,
    seq: u64,
    pub run: Option<SearchRun>,
    /// The query `run` was started for (the field may have been edited
    /// since, its debounce not run out).
    ran: SearchQuery,
    pub picker: Option<Picker>,
    pub picker_input: String,
    /// The open picker's suggestions for its input (computed on input,
    /// not per frame).
    pub suggestions: Vec<(String, String)>,
    /// Rows shown (a multiple of [`PAGE`]).
    pub shown: usize,
    /// The local search is still reading the offline cache.
    pub cache_pending: bool,
    /// The run's grouped rows, rebuilt when its revision changes.
    groups: RefCell<Option<(u64, u64, Groups)>>,
    /// Snippets with their highlights by message id, for the run in
    /// `.0`, with the [`text_tag`] of the text they were made of (a
    /// server copy may replace a stale cached one within a run).
    snippets: RefCell<(u64, Snippets)>,
}

/// A hash of a message's text: what tells a server copy that replaced a
/// stale cached hit from the text a snippet was made of.
pub fn text_tag(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// A result's text: one line with its matches.
pub type Snippet = std::sync::Arc<(String, Vec<Range<usize>>)>;

/// Snippets by message id with the [`text_tag`] of their text.
type Snippets = HashMap<String, (u64, Snippet)>;

/// Hits grouped by room: `(rid, message ids)`.
pub type Groups = std::sync::Arc<[(String, Vec<String>)]>;

impl SearchView {
    pub fn new(query: String) -> Self {
        Self {
            query,
            seq: 0,
            run: None,
            ran: SearchQuery::default(),
            picker: None,
            picker_input: String::new(),
            suggestions: Vec::new(),
            shown: PAGE,
            cache_pending: false,
            groups: RefCell::new(None),
            snippets: RefCell::new((0, HashMap::new())),
        }
    }

    /// The snippet of `m` for `words`, computed once per run and text.
    pub fn snippet(&self, m: &RcMessage, words: &[String], max: usize) -> Snippet {
        let run = self.run.as_ref().map_or(0, |r| r.id);
        let tag = text_tag(&m.msg);
        let mut cache = self.snippets.borrow_mut();
        if cache.0 != run {
            *cache = (run, HashMap::new());
        }
        match cache.1.get(&m.id) {
            Some((t, snip)) if *t == tag => snip.clone(),
            _ => {
                let snip: Snippet = std::sync::Arc::new(hit_snippet(&m.msg, words, max));
                cache.1.insert(m.id.clone(), (tag, snip.clone()));
                snip
            }
        }
    }

    /// Reopened, the view goes on with its run: it was started for the
    /// query shown (not one edited before the view closed).
    pub fn resumable(&self) -> bool {
        self.run.is_some() && self.ran == self.parsed()
    }

    /// The view put away when it closes: the open picker closes with it.
    pub fn stashed(mut self: Box<Self>) -> Box<Self> {
        self.picker = None;
        self.picker_input.clear();
        self.suggestions.clear();
        self
    }

    pub fn parsed(&self) -> SearchQuery {
        SearchQuery::parse(&self.query)
    }

    /// The run's hits grouped by room (rooms by newest hit).
    pub fn groups(&self) -> Groups {
        let Some(run) = &self.run else {
            return Groups::from(Vec::new());
        };
        let key = (run.id, run.revision);
        let mut cache = self.groups.borrow_mut();
        if let Some((id, rev, g)) = cache.as_ref()
            && (*id, *rev) == key
        {
            return g.clone();
        }
        let g: Groups = run
            .groups()
            .into_iter()
            .map(|(rid, hits)| {
                (
                    rid,
                    hits.into_iter().map(|h| h.message.id.clone()).collect(),
                )
            })
            .collect::<Vec<_>>()
            .into();
        *cache = Some((key.0, key.1, g.clone()));
        g
    }

    /// A builder edit: the query is rewritten and searched at once.
    fn edit(&mut self, f: impl FnOnce(&mut SearchQuery)) -> Vec<Out> {
        let mut q = self.parsed();
        f(&mut q);
        self.query = q.to_query();
        self.seq += 1;
        vec![Out::Start]
    }

    pub fn update(&mut self, m: Msg, ws: &Workspace) -> Vec<Out> {
        match m {
            Msg::Query(q) => {
                self.query = q;
                self.seq += 1;
                vec![Out::Debounce(self.seq)]
            }
            Msg::Debounced(seq) if seq == self.seq => vec![Out::Start],
            Msg::Debounced(_) => Vec::new(),
            Msg::Submit => {
                self.seq += 1;
                vec![Out::Start]
            }
            Msg::Clear => {
                self.query.clear();
                self.seq += 1;
                vec![Out::Start, Out::Focus(QUERY_ID)]
            }
            Msg::Close => vec![Out::Close],
            Msg::Pick(p) => {
                self.picker = if self.picker == p { None } else { p };
                self.picker_input.clear();
                self.refresh_suggestions(ws);
                match self.picker {
                    Some(_) => vec![Out::Focus(PICKER_ID)],
                    None => Vec::new(),
                }
            }
            Msg::PickerInput(v) => {
                self.picker_input = v;
                self.refresh_suggestions(ws);
                Vec::new()
            }
            Msg::PickerSubmit => self.picker_submit(ws),
            Msg::AddFrom(u) => {
                self.picker = None;
                self.edit(|q| {
                    if !q.from.iter().any(|x| x.eq_ignore_ascii_case(&u)) {
                        q.from.push(u);
                    }
                })
            }
            Msg::RemoveFrom(u) => self.edit(|q| q.from.retain(|x| *x != u)),
            Msg::AddRoom(r) => {
                self.picker = None;
                self.edit(|q| {
                    if !q.rooms.iter().any(|x| x.eq_ignore_ascii_case(&r)) {
                        q.rooms.push(r);
                    }
                })
            }
            Msg::RemoveRoom(r) => self.edit(|q| q.rooms.retain(|x| *x != r)),
            Msg::AfterDaysAgo(days) => {
                self.picker = None;
                // `after:` starts the day after the date given.
                let d = chrono::Local::now().date_naive() - chrono::Duration::days(days + 1);
                self.edit(|q| {
                    q.after = Some(d);
                    q.on = None;
                })
            }
            Msg::ClearDate(p) => self.edit(|q| match p {
                Picker::After => q.after = None,
                Picker::Before => q.before = None,
                Picker::From | Picker::In => {}
            }),
            Msg::ToggleHas(h) => self.edit(|q| {
                let on = !q.has(h);
                q.set_has(h, on);
            }),
            Msg::ToggleMentionsMe => {
                let Some(me) = ws.username().map(str::to_owned) else {
                    return Vec::new();
                };
                self.edit(|q| {
                    if q.mentions.iter().any(|m| m.eq_ignore_ascii_case(&me)) {
                        q.mentions.retain(|m| !m.eq_ignore_ascii_case(&me));
                    } else {
                        q.mentions.push(me);
                    }
                })
            }
            Msg::Open(mid) => self
                .run
                .as_ref()
                .and_then(|r| r.hit(&mid))
                .map(|h| Out::Open(Box::new(h.message.clone())))
                .into_iter()
                .collect(),
            Msg::ShowMore => {
                self.shown = self.shown.saturating_add(PAGE);
                Vec::new()
            }
            // Routed by the app.
            Msg::Fetched { .. } | Msg::Cached { .. } | Msg::Provider(_) | Msg::Pace => Vec::new(),
        }
    }

    fn refresh_suggestions(&mut self, ws: &Workspace) {
        self.suggestions = match self.picker {
            Some(Picker::From) => user_suggestions(ws, &self.picker_input),
            Some(Picker::In) => room_suggestions(ws, &self.picker_input),
            Some(Picker::After | Picker::Before) | None => Vec::new(),
        };
    }

    fn picker_submit(&mut self, ws: &Workspace) -> Vec<Out> {
        let Some(p) = self.picker else {
            return Vec::new();
        };
        let input = self.picker_input.trim().to_owned();
        match p {
            Picker::From => {
                let pick = user_suggestions(ws, &input)
                    .into_iter()
                    .next()
                    .map(|(u, _)| u)
                    .or_else(|| {
                        let u = input.trim_start_matches('@');
                        rustket_core::search::SearchQuery::parse(&format!("from:{u}"))
                            .from
                            .first()
                            .cloned()
                    });
                match pick {
                    Some(u) => self.update(Msg::AddFrom(u), ws),
                    None => Vec::new(),
                }
            }
            Picker::In => match room_suggestions(ws, &input).into_iter().next() {
                Some((name, _)) => self.update(Msg::AddRoom(name), ws),
                None => Vec::new(),
            },
            Picker::After | Picker::Before => {
                let Some(d) = parse_date(&input) else {
                    return Vec::new();
                };
                self.picker = None;
                self.edit(|q| {
                    q.on = None;
                    if p == Picker::After {
                        q.after = Some(d);
                    } else {
                        q.before = Some(d);
                    }
                })
            }
        }
    }

    /// Starts showing a new run (rows from the top).
    pub fn set_run(&mut self, run: Option<SearchRun>) {
        self.ran = match run {
            Some(_) => self.parsed(),
            None => SearchQuery::default(),
        };
        self.run = run;
        self.shown = PAGE;
        *self.groups.borrow_mut() = None;
    }
}

/// The snippet of a result's text with its matches. Bidi controls go
/// first, so the ranges index the text that is shown.
pub fn hit_snippet(text: &str, words: &[String], max: usize) -> (String, Vec<Range<usize>>) {
    let clean = rustket_ui::widget::sanitize_bidi(text);
    rustket_core::search::snippet(&clean, words, max)
}

/// Usernames to offer in the From picker for `input`: the current user,
/// 1:1 DM partners and authors of loaded messages, `(username, name)`.
pub fn user_suggestions(ws: &Workspace, input: &str) -> Vec<(String, String)> {
    let needle = input.trim().trim_start_matches('@').to_lowercase();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    // Whether the list is full.
    let mut push = |u: &str, name: Option<&str>| -> bool {
        if out.len() >= SUGGESTIONS {
            return true;
        }
        if u.is_empty() || !seen.insert(u.to_lowercase()) {
            return false;
        }
        let name = name.unwrap_or_default();
        if needle.is_empty()
            || u.to_lowercase().contains(&needle)
            || name.to_lowercase().contains(&needle)
        {
            out.push((u.to_owned(), name.to_owned()));
        }
        out.len() >= SUGGESTIONS
    };
    if let Some(me) = ws.me() {
        push(
            me.username.as_deref().unwrap_or_default(),
            me.name.as_deref(),
        );
    }
    let mut dms: Vec<_> = ws
        .rooms()
        .values()
        .filter(|e| {
            e.room_type() == rocketchat::models::RoomType::Direct
                && e.sub.is_some()
                && e.room
                    .as_ref()
                    .and_then(|r| r.uids.as_ref())
                    .is_none_or(|u| u.len() <= 2)
        })
        .collect();
    dms.sort_by_key(|e| std::cmp::Reverse(e.room.as_ref().and_then(|r| r.lm)));
    for e in dms {
        if push(e.name(), e.fname()) {
            return out;
        }
    }
    for m in ws.loaded_messages() {
        if push(
            m.u.username.as_deref().unwrap_or_default(),
            m.u.name.as_deref(),
        ) {
            break;
        }
    }
    out
}

/// Rooms to offer in the In picker for `input`: `(name, display name)`,
/// most recently active first.
pub fn room_suggestions(ws: &Workspace, input: &str) -> Vec<(String, String)> {
    let needle = input.trim().trim_start_matches(['#', '@']).to_lowercase();
    let mut rooms: Vec<_> = ws
        .rooms()
        .values()
        .filter(|e| e.sub.is_some() && !e.is_service_room() && !e.name().is_empty())
        .filter(|e| {
            needle.is_empty()
                || e.name().to_lowercase().contains(&needle)
                || e.fname()
                    .is_some_and(|f| f.to_lowercase().contains(&needle))
        })
        .collect();
    rooms.sort_by_key(|e| std::cmp::Reverse(e.room.as_ref().and_then(|r| r.lm)));
    rooms
        .into_iter()
        .take(SUGGESTIONS)
        .map(|e| {
            (
                e.name().to_owned(),
                e.fname().unwrap_or(e.name()).to_owned(),
            )
        })
        .collect()
}
