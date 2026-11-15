//! Mention groups at app level: the sync's results, the user's edits and the
//! server checks the Settings' group editor asks for.

use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::rest::{TeamRef, UserLookup};
use rocketchat::{Error, RestClient};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::mention_groups::{MentionGroupEdit, MentionGroupError};
use rustket_ui::widget::BannerKind;

use super::App;
use crate::error::Failure;
use crate::message::Message;
use crate::screens::settings::{
    self, MentionCheckReport, MentionGroupCheck, MentionIssue, MentionSuggestion, MentionsMessage,
};

/// A lookup the server answered with "not found" (`users.info`: 400 "User
/// not found.", `teams.info`: 400 "Team not found").
fn not_found(e: &Error) -> bool {
    matches!(
        e,
        Error::Api {
            status: 400 | 404,
            ..
        }
    ) && e
        .api_error_code()
        .is_some_and(|c| c.to_ascii_lowercase().contains("not found"))
}

/// Rate-limit answers a save's lookups wait out before giving up.
const MAX_RATE_WAITS: u32 = 3;
/// Wait after a rate-limit answer without a time (the server's default
/// window is a minute), and the longest wait.
const RATE_WAIT: Duration = Duration::from_secs(60);
const MAX_RATE_WAIT: Duration = Duration::from_secs(65);

/// How one `users.info` lookup ended.
enum Lookup {
    Found,
    NotFound,
    /// Rate limited and not waited out (a check only, or waited enough).
    Limited,
    Failed(Error),
}

/// Looks `username` up. `waits`: rate-limit answers still to wait out
/// (0: give up on the first one).
async fn lookup_user(rest: &RestClient, username: &str, waits: &mut u32) -> Lookup {
    loop {
        match rest.users_info(UserLookup::Username(username)).await {
            Ok(_) => return Lookup::Found,
            Err(e) if not_found(&e) => return Lookup::NotFound,
            Err(Error::RateLimited { retry_after, .. }) => {
                if *waits == 0 {
                    return Lookup::Limited;
                }
                *waits -= 1;
                let wait = retry_after
                    .unwrap_or(RATE_WAIT)
                    .clamp(Duration::from_secs(1), MAX_RATE_WAIT);
                tracing::debug!(?wait, "mention groups: lookups rate limited, waiting");
                tokio::time::sleep(wait).await;
            }
            Err(e) => return Lookup::Failed(e),
        }
    }
}

/// Runs the lookups of a check, one at a time (they are rate limited per
/// route, by default to 10 a minute): whether a user or a team has the
/// name, then each username. A save (`patient`) waits out rate limits; a
/// check of an opened group stops at the first one and leaves the rest
/// unknown.
async fn run_checks(
    rest: RestClient,
    name: Option<String>,
    lookup: Vec<String>,
    patient: bool,
) -> MentionCheckReport {
    let mut r = MentionCheckReport::default();
    let mut waits = if patient { MAX_RATE_WAITS } else { 0 };
    let limited = |r: &mut MentionCheckReport| {
        if patient {
            let e = Error::RateLimited {
                retry_after: None,
                message: None,
            };
            r.failed = Some(Failure::from(e).user_text());
        }
    };
    if let Some(name) = name {
        match lookup_user(&rest, &name, &mut waits).await {
            Lookup::Found => {
                r.issue = Some(MentionIssue::User);
                return r;
            }
            Lookup::NotFound => {}
            Lookup::Limited => {
                limited(&mut r);
                return r;
            }
            Lookup::Failed(e) => {
                r.failed = Some(Failure::from(e).user_text());
                return r;
            }
        }
        // Servers without teams (or without access to them) answer with
        // other errors: only a found team counts.
        match rest.teams_info(TeamRef::Name(&name)).await {
            Ok(_) => {
                r.issue = Some(MentionIssue::Team);
                return r;
            }
            Err(e) => {
                if !not_found(&e) {
                    tracing::debug!(error = %e, "mention groups: teams.info failed");
                }
            }
        }
    }
    for u in lookup {
        match lookup_user(&rest, &u, &mut waits).await {
            Lookup::Found => r.found.push(u),
            Lookup::NotFound => r.missing.push(u),
            Lookup::Limited => {
                limited(&mut r);
                break;
            }
            Lookup::Failed(e) => {
                r.failed = Some(Failure::from(e).user_text());
                break;
            }
        }
    }
    r
}

impl App {
    /// A result of the mention group sync; tells the user when it gave up.
    pub(super) fn on_mention_groups(
        &mut self,
        sid: ServerId,
        epoch: u64,
        m: crate::workspace::mention_groups::Msg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        let outs = s.mention_groups.update(m, Instant::now());
        let tasks: Vec<_> = outs
            .into_iter()
            .map(|out| match out {
                crate::workspace::service_sync::Out::Failed(f) => self.toast(
                    BannerKind::Error,
                    t!("mention_groups.sync_failed"),
                    f.user_text(),
                ),
            })
            .collect();
        Task::batch(tasks)
    }

    /// Records a mention group edit of the user: used at once, stored on
    /// the server by the sync (`after_update` persists it until then).
    fn apply_mention_group(
        &mut self,
        sid: &ServerId,
        edit: &MentionGroupEdit,
    ) -> (Task<Message>, Result<bool, MentionGroupError>) {
        let (device, created) = self.config.ensure_device_id();
        let save = if created {
            self.save_config()
        } else {
            Task::none()
        };
        let Some(s) = self.servers.get_mut(sid) else {
            return (save, Ok(false));
        };
        let res = s
            .mention_groups
            .edit(&mut s.workspace, edit, &device, Instant::now());
        if let Err(e) = &res {
            tracing::warn!(server = %sid, error = %e, "mention group edit refused");
        }
        (save, res)
    }

    /// An edit from the Settings (deleting a group).
    pub(super) fn edit_mention_group(
        &mut self,
        sid: &ServerId,
        edit: MentionGroupEdit,
    ) -> Task<Message> {
        let (save, res) = self.apply_mention_group(sid, &edit);
        match res {
            Ok(_) => save,
            Err(e) => Task::batch([
                save,
                self.toast(
                    BannerKind::Error,
                    t!("mention_groups.edit_failed"),
                    MentionIssue::from_error(&e).text(""),
                ),
            ]),
        }
    }

    /// The group editor's checks: what the workspace tells (the user's own
    /// name, another group with the name), then the server's lookups.
    pub(super) fn check_mention_group(
        &mut self,
        sid: ServerId,
        check: MentionGroupCheck,
    ) -> Task<Message> {
        let Some(s) = self.servers.get(&sid) else {
            return Task::none();
        };
        let own = s
            .workspace
            .username()
            .is_some_and(|me| me.eq_ignore_ascii_case(&check.name));
        // Taking another group's name is refused; a group keeps its own
        // even when a copy made on another device has it too.
        let groups = s.workspace.mention_groups();
        let renamed = check
            .id
            .as_deref()
            .and_then(|id| groups.by_id(id))
            .is_none_or(|g| g.name != check.name);
        let duplicate = renamed
            && groups
                .all()
                .iter()
                .any(|g| Some(&g.id) != check.id.as_ref() && g.name == check.name);
        let issue = if own {
            Some(MentionIssue::Own)
        } else if duplicate && check.save.is_some() {
            Some(MentionIssue::Duplicate)
        } else {
            None
        };
        let rest = s.session.as_ref().map(|h| h.rest().clone());
        let (issue, rest) = match (issue, rest) {
            (Some(i), _) => (Some(i), None),
            (None, None) => (Some(MentionIssue::Offline), None),
            (None, Some(r)) => (None, Some(r)),
        };
        let Some(rest) = rest else {
            let report = MentionCheckReport {
                issue,
                ..Default::default()
            };
            return self.mention_group_checked(sid, check, report);
        };
        let name = check.check_name.then(|| check.name.clone());
        let lookup = check.lookup.clone();
        let patient = check.save.is_some();
        Task::perform(run_checks(rest, name, lookup, patient), move |report| {
            Message::MentionGroupChecked(sid.clone(), Box::new(check.clone()), report)
        })
    }

    /// The checks are done: saves the group when they passed and asked to,
    /// then tells the editor.
    pub(super) fn mention_group_checked(
        &mut self,
        sid: ServerId,
        check: MentionGroupCheck,
        mut report: MentionCheckReport,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        let clean = report.issue.is_none() && report.missing.is_empty() && report.failed.is_none();
        if let Some(members) = check.save.clone().filter(|_| clean) {
            // The user is never part of their own groups.
            let me = self
                .servers
                .get(&sid)
                .and_then(|s| s.workspace.username().map(str::to_owned));
            let members: Vec<String> = members
                .into_iter()
                .filter(|m| me.as_deref().is_none_or(|me| !m.eq_ignore_ascii_case(me)))
                .collect();
            if members.is_empty() {
                report.issue = Some(MentionIssue::Empty);
            } else {
                let edit = MentionGroupEdit::Upsert {
                    id: check.id.clone().unwrap_or_else(rocketchat::ids::random_id),
                    name: check.name.clone(),
                    members,
                };
                let (save, res) = self.apply_mention_group(&sid, &edit);
                tasks.push(save);
                match res {
                    Ok(_) => report.saved = true,
                    Err(e) => report.issue = Some(MentionIssue::from_error(&e)),
                }
            }
        }
        tasks.push(
            self.on_settings(settings::Message::Mentions(MentionsMessage::Checked(
                check.seq, report,
            ))),
        );
        Task::batch(tasks)
    }

    /// Users for the group editor's member field.
    pub(super) fn suggest_mention_members(
        &mut self,
        sid: &ServerId,
        seq: u64,
        term: String,
    ) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return Task::none();
        };
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return Task::none();
        };
        let me = s.workspace.username().map(str::to_owned);
        Task::perform(
            async move { rest.users_autocomplete(&term).await },
            move |res| {
                let res = res
                    .map(|list| {
                        list.into_iter()
                            .filter_map(|u| {
                                let username = u.username?;
                                (Some(&username) != me.as_ref()).then_some(MentionSuggestion {
                                    username,
                                    name: u.name,
                                })
                            })
                            .collect()
                    })
                    .map_err(|e| Failure::from(e).user_text());
                Message::Settings(settings::Message::Mentions(MentionsMessage::Suggestions(
                    seq, res,
                )))
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_answers_are_told_apart() {
        let user =
            Error::from_response(400, None, br#"{"success":false,"error":"User not found."}"#);
        assert!(not_found(&user));
        let team =
            Error::from_response(400, None, br#"{"success":false,"error":"Team not found"}"#);
        assert!(not_found(&team));
        let other = Error::from_response(
            400,
            None,
            br#"{"success":false,"error":"Not allowed [error-not-allowed]","errorType":"error-not-allowed"}"#,
        );
        assert!(!not_found(&other));
        assert!(!not_found(&Error::Timeout("users.info")));
    }
}
