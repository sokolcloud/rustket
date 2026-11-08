//! Canned responses (EE, `docs/omnichannel.md` §16): store with the server's
//! visibility rule, stream updates and the composer `!` popup.

use std::collections::{HashMap, HashSet};

use rocketchat::models::{CannedResponse, CannedScope};
use rocketchat::session::OmnichannelEvent;

/// Maximum entries of the `!` popup.
pub const SUGGEST_LIMIT: usize = 11;

/// Whose responses are visible.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CannedContext {
    pub me: String,
    /// Departments the user belongs to.
    pub departments: HashSet<String>,
    /// `view-all-canned-responses`: every department's responses.
    pub view_all: bool,
}

/// The canned responses the user may use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CannedResponses {
    ctx: CannedContext,
    by_id: HashMap<String, CannedResponse>,
    revision: u64,
}

impl CannedResponses {
    pub fn new(ctx: CannedContext) -> Self {
        Self {
            ctx,
            ..Default::default()
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&CannedResponse> {
        self.by_id.get(id)
    }

    pub fn set_context(&mut self, ctx: CannedContext) {
        if ctx != self.ctx {
            self.ctx = ctx;
            let ctx = &self.ctx;
            self.by_id.retain(|_, r| visible(ctx, r));
            self.revision += 1;
        }
    }

    /// `canned-responses.get`'s rule: own `user` responses, `global` ones, and
    /// `department` ones of the user's departments (all with `view_all`). The
    /// stream broadcasts every saved response, so events are filtered with it.
    pub fn is_visible(&self, r: &CannedResponse) -> bool {
        visible(&self.ctx, r)
    }

    /// Replaces the store with `canned-responses.get`.
    pub fn replace(&mut self, list: Vec<CannedResponse>) {
        self.by_id = list
            .into_iter()
            .filter(|r| !r.id.is_empty() && visible(&self.ctx, r))
            .map(|r| (r.id.clone(), r))
            .collect();
        self.revision += 1;
    }

    fn concerns_me(&self, agents: Option<&Vec<String>>) -> bool {
        agents.is_none_or(|a| a.contains(&self.ctx.me))
    }

    /// Applies a stream event; returns whether the store changed.
    pub fn apply_event(&mut self, ev: &OmnichannelEvent) -> bool {
        match ev {
            OmnichannelEvent::CannedResponseChanged { response, agents } => {
                if !self.concerns_me(agents.as_ref()) {
                    return false;
                }
                if !visible(&self.ctx, response) {
                    // e.g. moved to another department or scope.
                    return self.remove(&response.id);
                }
                if self.by_id.get(&response.id) == Some(response) {
                    return false;
                }
                self.by_id.insert(response.id.clone(), (**response).clone());
                self.revision += 1;
                true
            }
            OmnichannelEvent::CannedResponseRemoved { id, agents } => {
                self.concerns_me(agents.as_ref()) && self.remove(id)
            }
            _ => false,
        }
    }

    fn remove(&mut self, id: &str) -> bool {
        let removed = self.by_id.remove(id).is_some();
        if removed {
            self.revision += 1;
        }
        removed
    }

    /// Responses whose shortcut contains `filter` (case-insensitive, literal),
    /// sorted by shortcut, at most [`SUGGEST_LIMIT`].
    pub fn suggest(&self, filter: &str) -> Vec<&CannedResponse> {
        let f = filter.to_lowercase();
        let mut out: Vec<&CannedResponse> = self
            .by_id
            .values()
            .filter(|r| r.shortcut.to_lowercase().contains(&f))
            .collect();
        out.sort_by(|a, b| {
            a.shortcut
                .to_lowercase()
                .cmp(&b.shortcut.to_lowercase())
                .then_with(|| a.shortcut.cmp(&b.shortcut))
                .then_with(|| a.id.cmp(&b.id))
        });
        out.truncate(SUGGEST_LIMIT);
        out
    }

    /// All responses sorted by shortcut (the panel list).
    pub fn all(&self) -> Vec<&CannedResponse> {
        let mut out: Vec<&CannedResponse> = self.by_id.values().collect();
        out.sort_by(|a, b| {
            a.shortcut
                .to_lowercase()
                .cmp(&b.shortcut.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }
}

fn visible(ctx: &CannedContext, r: &CannedResponse) -> bool {
    match r.scope {
        CannedScope::Global => true,
        CannedScope::User => r.user_id.as_deref() == Some(ctx.me.as_str()),
        CannedScope::Department => {
            ctx.view_all
                || r.department_id
                    .as_deref()
                    .is_some_and(|d| ctx.departments.contains(d))
        }
        CannedScope::Unknown => false,
    }
}

/// The `!` trigger before the caret: `(?:^| |\n)(!)([^\s]*$)`. Returns the byte
/// offset of the `!` and the filter text after it.
pub fn trigger(text_before_caret: &str) -> Option<(usize, &str)> {
    let bang = text_before_caret.rfind('!')?;
    let filter = &text_before_caret[bang + 1..];
    if filter.chars().any(char::is_whitespace) {
        return None;
    }
    let ok_before =
        bang == 0 || matches!(text_before_caret[..bang].chars().last(), Some(' ' | '\n'));
    ok_before.then_some((bang, filter))
}

/// Replaces the `!filter` before the caret (at byte `caret`) with the response
/// text plus a space. Returns the new text and caret byte offset; `None` when
/// there is no trigger.
pub fn apply_selection(
    text: &str,
    caret: usize,
    response: &CannedResponse,
) -> Option<(String, usize)> {
    let caret = caret.min(text.len());
    let before = text.get(..caret)?;
    let (bang, _) = trigger(before)?;
    let insert = format!("{} ", response.text);
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..bang]);
    out.push_str(&insert);
    let new_caret = out.len();
    out.push_str(&text[caret..]);
    Some((out, new_caret))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn cr(id: &str, shortcut: &str, scope: CannedScope) -> CannedResponse {
        CannedResponse {
            id: id.into(),
            shortcut: shortcut.into(),
            text: format!("text of {shortcut}"),
            scope,
            ..Default::default()
        }
    }

    fn ctx() -> CannedContext {
        CannedContext {
            me: "me".into(),
            departments: ["d1".to_owned()].into_iter().collect(),
            view_all: false,
        }
    }

    #[test]
    fn visibility() {
        let s = CannedResponses::new(ctx());
        assert!(s.is_visible(&cr("g", "g", CannedScope::Global)));
        let mut u = cr("u", "u", CannedScope::User);
        u.user_id = Some("other".into());
        assert!(!s.is_visible(&u), "someone else's private response");
        u.user_id = Some("me".into());
        assert!(s.is_visible(&u));
        let mut d = cr("d", "d", CannedScope::Department);
        d.department_id = Some("d2".into());
        assert!(!s.is_visible(&d));
        let mut all = ctx();
        all.view_all = true;
        assert!(CannedResponses::new(all).is_visible(&d));
        d.department_id = Some("d1".into());
        assert!(s.is_visible(&d));
        assert!(!s.is_visible(&cr("x", "x", CannedScope::Unknown)));
    }

    #[test]
    fn stream_events() {
        let mut s = CannedResponses::new(ctx());
        s.replace(vec![cr("a", "hello", CannedScope::Global)]);
        let changed = |r: CannedResponse, agents: Option<Vec<String>>| {
            OmnichannelEvent::CannedResponseChanged {
                response: Box::new(r),
                agents,
            }
        };
        assert!(s.apply_event(&changed(cr("b", "bye", CannedScope::Global), None)));
        assert!(!s.apply_event(&changed(
            cr("c", "c", CannedScope::Global),
            Some(vec!["other".into()])
        )));
        let mut foreign = cr("f", "f", CannedScope::User);
        foreign.user_id = Some("other".into());
        assert!(!s.apply_event(&changed(foreign, None)), "not inserted");
        // A response moved out of reach is removed.
        let mut moved = cr("b", "bye", CannedScope::Department);
        moved.department_id = Some("d9".into());
        assert!(s.apply_event(&changed(moved, None)));
        assert!(s.get("b").is_none());
        assert!(s.apply_event(&OmnichannelEvent::CannedResponseRemoved {
            id: "a".into(),
            agents: None
        }));
        assert!(s.is_empty());
    }

    #[test]
    fn suggestions() {
        let mut s = CannedResponses::new(ctx());
        let mut list: Vec<CannedResponse> = (0..15)
            .map(|i| {
                cr(
                    &format!("id{i}"),
                    &format!("greet{i:02}"),
                    CannedScope::Global,
                )
            })
            .collect();
        list.push(cr("x", "Thanks", CannedScope::Global));
        list.push(cr("y", "a.b", CannedScope::Global));
        s.replace(list);
        let names =
            |f: &str| -> Vec<String> { s.suggest(f).iter().map(|r| r.shortcut.clone()).collect() };
        assert_eq!(names("GREET").len(), SUGGEST_LIMIT);
        assert_eq!(names("greet")[0], "greet00");
        assert_eq!(names("thank"), ["Thanks"]);
        assert_eq!(names("."), ["a.b"], "literal, not a regex");
        assert_eq!(names("").len(), SUGGEST_LIMIT);
    }

    #[test]
    fn trigger_and_apply() {
        assert_eq!(trigger("!"), Some((0, "")));
        assert_eq!(trigger("hi !gre"), Some((3, "gre")));
        assert_eq!(trigger("line\n!x"), Some((5, "x")));
        assert_eq!(
            trigger("wow!x"),
            None,
            "needs start, space or newline before"
        );
        assert_eq!(trigger("!x y"), None);
        assert_eq!(trigger("no bang"), None);
        let r = cr("a", "greet", CannedScope::Global);
        let (t, c) = apply_selection("hi !gr tail", 6, &r).unwrap();
        assert_eq!(t, "hi text of greet  tail");
        assert_eq!(c, "hi text of greet ".len());
        assert!(apply_selection("hi", 2, &r).is_none());
    }
}
