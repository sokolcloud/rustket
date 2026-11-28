//! The policy refresh (docs/policy.md section 8): a timer re-reads the
//! sources on a blocking thread; [`Debounce`] holds a source (or the whole
//! Linux directory) that turned empty or unreadable for one poll, since a
//! Group Policy refresh deletes and rewrites its keys and a file can be
//! caught half-written.

use std::time::Duration;

use rustket_core::policy::{Snapshot, SourceId};

/// How often the sources are read again.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// What the debounce compares as one source. The Linux directory is one
/// unit for "had values, now has none" (every file is its own snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unit {
    LinuxDirectory,
    Source(SourceId),
}

fn unit(id: &SourceId) -> Unit {
    match id {
        SourceId::LinuxDirectory | SourceId::LinuxFile(_) => Unit::LinuxDirectory,
        other => Unit::Source(other.clone()),
    }
}

/// A change held back for one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// The unit had values and now has none, and every source of it is
    /// readable.
    Emptied(UnitId),
    /// The source was readable and now is not.
    Unreadable(SourceId),
}

/// A unit, public only as part of [`Hold`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitId(Unit);

/// The snapshots last accepted and the changes held back.
#[derive(Debug, Clone, Default)]
pub struct Debounce {
    accepted: Vec<Snapshot>,
    held: Vec<Hold>,
}

impl Debounce {
    /// Starts from the snapshots the start-up policy was resolved from:
    /// nothing is delayed at start-up.
    pub fn new(accepted: Vec<Snapshot>) -> Self {
        Self {
            accepted,
            held: Vec::new(),
        }
    }

    /// Takes a fresh read and returns the snapshots to resolve (which
    /// become the accepted ones).
    pub fn accept(&mut self, fresh: Vec<Snapshot>) -> &[Snapshot] {
        let (out, held) = debounce(&self.accepted, &self.held, fresh);
        self.accepted = out;
        self.held = held;
        &self.accepted
    }

    /// Changes held back by the last poll.
    pub fn held(&self) -> &[Hold] {
        &self.held
    }
}

fn has_values<'a>(mut snaps: impl Iterator<Item = &'a Snapshot>) -> bool {
    snaps.any(|s| !s.values.is_empty())
}

/// Compared with `accepted`: a unit that had values and now has none while
/// its fresh read is wholly readable ("emptied"), and a source that was
/// readable and now is not, keep their accepted state unless the same change
/// was already held at the previous poll (`held`). Under a whole-unit hold
/// (below) the unit's changes are accepted together, once every one of them
/// was held; under per-source holds each source is confirmed on its own.
/// Returns the snapshots to use, in priority order, and the changes held now.
/// A change that went away during the hold is simply not held any more.
///
/// Two kinds of hold:
/// - Whole-unit hold: the unit's accepted snapshots replace all of its fresh
///   ones. Used for an emptied unit, for a Linux file that turned unreadable
///   with another trust (trust decides its rank), and for the Linux directory
///   turning unlistable (it has no snapshot while it lists, so it counts as
///   readable unless the accepted unit has a directory error). A new source
///   of the unit that cannot be read is still added at once.
/// - Per-source hold: otherwise each unreadable source keeps only its own
///   accepted snapshot, at its own place; the unit's other changes pass.
pub fn debounce(
    accepted: &[Snapshot],
    held: &[Hold],
    fresh: Vec<Snapshot>,
) -> (Vec<Snapshot>, Vec<Hold>) {
    let mut units: Vec<Unit> = Vec::new();
    for s in accepted.iter().chain(&fresh) {
        let u = unit(&s.id);
        if !units.contains(&u) {
            units.push(u);
        }
    }
    let mut out = fresh;
    let mut now_held = Vec::new();
    for u in units {
        let prev = || accepted.iter().filter(|s| unit(&s.id) == u);
        let mut changes: Vec<Hold> = Vec::new();
        let mut whole = false;
        let fresh_unit = || out.iter().filter(|s| unit(&s.id) == u);
        // An unreadable read does not count as emptied: its content is unknown.
        let readable = fresh_unit().all(|s| s.error.is_none());
        if readable && has_values(prev()) && !has_values(fresh_unit()) {
            changes.push(Hold::Emptied(UnitId(u.clone())));
            whole = true;
        }
        for slot in out.iter().filter(|s| unit(&s.id) == u && s.error.is_some()) {
            let readable_before = match prev().find(|p| p.id == slot.id) {
                Some(before) => {
                    if before.error.is_none() && before.trust != slot.trust {
                        whole = true;
                    }
                    before.error.is_none()
                }
                None if slot.id == SourceId::LinuxDirectory => {
                    whole = true;
                    true
                }
                None => false,
            };
            let hold = Hold::Unreadable(slot.id.clone());
            if readable_before && !changes.contains(&hold) {
                changes.push(hold);
            }
        }
        if changes.iter().all(|c| held.contains(c)) {
            continue;
        }
        if whole {
            now_held.extend(changes);
            let kept = keep_unit(prev(), fresh_unit());
            replace_unit(&mut out, &u, kept);
            continue;
        }
        // Only unreadable sources with a readable snapshot of the same trust:
        // each keeps its own, at its own place.
        for slot in out.iter_mut().filter(|s| unit(&s.id) == u) {
            let hold = Hold::Unreadable(slot.id.clone());
            if !changes.contains(&hold) || held.contains(&hold) {
                continue;
            }
            if let Some(before) = prev().find(|p| p.id == slot.id) {
                *slot = before.clone();
                now_held.push(hold);
            }
        }
    }
    (out, now_held)
}

/// The snapshots of a unit held as a whole: the accepted ones, plus each new
/// source of the fresh read that cannot be read (it has no readable state to
/// keep, so it applies at once), placed after the nearest accepted source that
/// precedes it in the fresh read. An accepted file missing from a partial
/// listing is never such a neighbour, so a new file can land before a file
/// that outranks it by name; only the order of the sources and of the
/// blocking entries in the report changes, as an unreadable snapshot carries
/// no values.
fn keep_unit<'a, 'b>(
    accepted: impl Iterator<Item = &'a Snapshot>,
    fresh: impl Iterator<Item = &'b Snapshot>,
) -> Vec<Snapshot> {
    let mut kept: Vec<Snapshot> = accepted.cloned().collect();
    let mut at = 0;
    for s in fresh {
        if let Some(i) = kept.iter().position(|k| k.id == s.id) {
            at = i + 1;
        } else if s.error.is_some() && s.id != SourceId::LinuxDirectory {
            kept.insert(at, s.clone());
            at += 1;
        }
    }
    kept
}

/// Replaces `u`'s snapshots in `out` by `with`, at the place of the first.
fn replace_unit(out: &mut Vec<Snapshot>, u: &Unit, with: Vec<Snapshot>) {
    let at = out
        .iter()
        .position(|s| unit(&s.id) == *u)
        .unwrap_or(out.len());
    out.retain(|s| unit(&s.id) != *u);
    let at = at.min(out.len());
    out.splice(at..at, with);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rustket_core::policy::{RawValue, SourceErrorKind, Trust};

    use super::*;

    fn win(id: SourceId, https: Option<i64>) -> Snapshot {
        let trust = id.default_trust();
        let snap = Snapshot::new(id, trust);
        match https {
            Some(v) => snap.with("RequireHttps", RawValue::Integer(v)),
            None => snap,
        }
    }

    fn hklm(https: Option<i64>) -> Snapshot {
        win(SourceId::WindowsMachine, https)
    }

    fn hkcu(https: Option<i64>) -> Snapshot {
        win(SourceId::WindowsUser, https)
    }

    fn unreadable(id: SourceId) -> Snapshot {
        Snapshot::unreadable(id, SourceErrorKind::CannotRead)
    }

    fn file(name: &str, https: Option<i64>) -> Snapshot {
        let snap = Snapshot::new(SourceId::LinuxFile(name.into()), Trust::Full);
        match https {
            Some(v) => snap.with("RequireHttps", RawValue::Integer(v)),
            None => snap,
        }
    }

    /// Runs the polls through one debounce; returns the snapshots used at
    /// each poll.
    fn polls(start: Vec<Snapshot>, reads: Vec<Vec<Snapshot>>) -> Vec<Vec<Snapshot>> {
        let mut d = Debounce::new(start);
        reads.into_iter().map(|r| d.accept(r).to_vec()).collect()
    }

    #[test]
    fn unchanged_and_ordinary_changes_pass_at_once() {
        let start = vec![hklm(Some(1)), hkcu(None)];
        let got = polls(
            start.clone(),
            vec![
                start.clone(),
                vec![hklm(Some(0)), hkcu(None)],
                vec![hklm(Some(0)), hkcu(Some(1))],
            ],
        );
        assert_eq!(got[0], start);
        assert_eq!(got[1], [hklm(Some(0)), hkcu(None)]);
        assert_eq!(got[2], [hklm(Some(0)), hkcu(Some(1))]);
    }

    #[test]
    fn a_source_appearing_with_values_or_unreadable_is_not_held() {
        // HKCU was not seen before: nothing to compare with.
        let got = polls(
            vec![hklm(None)],
            vec![vec![hklm(Some(1)), unreadable(SourceId::WindowsUser)]],
        );
        assert_eq!(got[0], [hklm(Some(1)), unreadable(SourceId::WindowsUser)]);
        // At start-up the empty debounce holds nothing either.
        assert_eq!(
            polls(vec![], vec![vec![unreadable(SourceId::MacManaged)]])[0],
            [unreadable(SourceId::MacManaged)]
        );
    }

    #[test]
    fn emptied_source_is_held_for_one_poll() {
        let start = vec![hklm(Some(1)), hkcu(Some(0))];
        let emptied = vec![hklm(None), hkcu(Some(0))];
        let got = polls(start.clone(), vec![emptied.clone(), emptied.clone()]);
        assert_eq!(got[0], start, "held");
        assert_eq!(got[1], emptied, "accepted at the second poll");
    }

    #[test]
    fn unreadable_source_is_held_for_one_poll() {
        let start = vec![hklm(Some(1)), hkcu(None)];
        let broken = vec![hklm(Some(1)), unreadable(SourceId::WindowsUser)];
        let mut d = Debounce::new(start.clone());
        assert_eq!(d.accept(broken.clone()), start.as_slice(), "held");
        assert_eq!(d.held(), [Hold::Unreadable(SourceId::WindowsUser)]);
        assert_eq!(d.accept(broken.clone()), broken.as_slice());
        assert!(d.held().is_empty());
        // Unreadable once accepted: later polls are not held again.
        assert_eq!(d.accept(broken.clone()), broken.as_slice());
    }

    #[test]
    fn unreadable_and_empty_is_held_once() {
        let start = vec![hklm(Some(1))];
        let broken = vec![unreadable(SourceId::WindowsMachine)];
        let got = polls(start.clone(), vec![broken.clone(), broken.clone()]);
        assert_eq!(got, [start, broken]);
    }

    #[test]
    fn recovery_during_the_hold_cancels_it() {
        let start = vec![hklm(Some(1))];
        let got = polls(
            start.clone(),
            vec![
                vec![hklm(None)],
                vec![hklm(Some(1))],
                vec![hklm(None)],
                vec![hklm(None)],
            ],
        );
        assert_eq!(got[0], start, "held");
        assert_eq!(got[1], start, "back: nothing to hold");
        assert_eq!(got[2], start, "held again, not accepted at once");
        assert_eq!(got[3], [hklm(None)]);

        let broken = || vec![unreadable(SourceId::WindowsMachine)];
        let got = polls(
            start.clone(),
            vec![broken(), vec![hklm(Some(0))], broken(), broken()],
        );
        assert_eq!(got[0], start);
        assert_eq!(got[1], [hklm(Some(0))], "a recovery with a new value");
        assert_eq!(got[2], [hklm(Some(0))], "held again");
        assert_eq!(got[3], broken());
    }

    #[test]
    fn a_different_change_at_the_second_poll_is_held_on_its_own() {
        // Emptied first, then unreadable: each is held once.
        let start = vec![hklm(Some(1)), hkcu(None)];
        let got = polls(
            start.clone(),
            vec![
                vec![hklm(None), hkcu(None)],
                vec![hklm(Some(1)), unreadable(SourceId::WindowsUser)],
                vec![hklm(Some(1)), unreadable(SourceId::WindowsUser)],
            ],
        );
        assert_eq!(got[0], start);
        assert_eq!(got[1], start);
        assert_eq!(got[2][1], unreadable(SourceId::WindowsUser));
    }

    #[test]
    fn linux_directory_emptied_as_a_whole() {
        let start = vec![file("b.json", Some(1)), file("a.json", Some(0))];
        // Every file gone.
        let got = polls(start.clone(), vec![vec![], vec![]]);
        assert_eq!(got[0], start, "held");
        assert!(got[1].is_empty());
        // Files left without values.
        let empty = vec![file("b.json", None)];
        let got = polls(start.clone(), vec![empty.clone(), empty.clone()]);
        assert_eq!(got, [start.clone(), empty]);
        // The directory turned unlistable.
        let dir = vec![unreadable(SourceId::LinuxDirectory)];
        let got = polls(start.clone(), vec![dir.clone(), dir.clone()]);
        assert_eq!(got, [start, dir]);
    }

    #[test]
    fn linux_file_disappearing_while_others_remain_passes_at_once() {
        let start = vec![file("b.json", Some(1)), file("a.json", Some(0))];
        let got = polls(start, vec![vec![file("a.json", Some(0))]]);
        assert_eq!(got[0], [file("a.json", Some(0))]);
    }

    #[test]
    fn linux_file_turning_unreadable_is_held_per_file() {
        let start = vec![file("b.json", Some(1)), file("a.json", Some(0))];
        let broken = vec![
            unreadable(SourceId::LinuxFile("b.json".into())),
            file("a.json", Some(1)),
        ];
        let got = polls(start, vec![broken.clone(), broken.clone()]);
        assert_eq!(
            got[0],
            [file("b.json", Some(1)), file("a.json", Some(1))],
            "the other file's change passes"
        );
        assert_eq!(got[1], broken);
        // A new file that cannot be read is not held.
        let got = polls(
            vec![file("a.json", Some(0))],
            vec![vec![
                unreadable(SourceId::LinuxFile("c.json".into())),
                file("a.json", Some(0)),
            ]],
        );
        assert!(got[0][0].error.is_some());
    }

    #[test]
    fn held_linux_file_keeps_full_trust_above_limited() {
        let mut limited_broken = unreadable(SourceId::LinuxFile("b.json".into()));
        limited_broken.trust = Trust::Limited;
        let mut limited = file("a.json", Some(0));
        limited.trust = Trust::Limited;
        let start = vec![file("b.json", Some(1)), limited.clone()];
        let got = polls(start.clone(), vec![vec![limited.clone(), limited_broken]]);
        assert_eq!(got[0], start);
    }

    #[test]
    fn emptied_then_unreadable_drops_the_emptied_hold() {
        let start = vec![hklm(Some(1))];
        let broken = || vec![unreadable(SourceId::WindowsMachine)];
        let got = polls(start.clone(), vec![vec![hklm(None)], broken(), broken()]);
        assert_eq!(got[0], start, "emptied: held");
        assert_eq!(got[1], start, "unreadable seen once: held");
        assert_eq!(got[2], broken(), "unreadable seen at two consecutive polls");
        let mut d = Debounce::new(start.clone());
        d.accept(vec![hklm(None)]);
        d.accept(broken());
        // The unreadable read does not count as emptied, so the emptied
        // change went away and only the unreadable one is held.
        assert_eq!(d.held(), [Hold::Unreadable(SourceId::WindowsMachine)]);
    }

    #[test]
    fn an_unreadable_read_does_not_count_as_emptied() {
        // Unreadable, then readable and empty: the empty state is held on its
        // own and applies only once seen at two consecutive polls.
        let start = vec![hklm(Some(1))];
        let broken = || vec![unreadable(SourceId::WindowsMachine)];
        let mut d = Debounce::new(start.clone());
        assert_eq!(d.accept(broken()), start.as_slice());
        assert_eq!(d.held(), [Hold::Unreadable(SourceId::WindowsMachine)]);
        assert_eq!(d.accept(vec![hklm(None)]), start.as_slice(), "seen once");
        assert_eq!(
            d.held(),
            [Hold::Emptied(UnitId(Unit::Source(
                SourceId::WindowsMachine
            )))]
        );
        assert_eq!(d.accept(vec![hklm(None)]), [hklm(None)].as_slice());

        // The same for a Linux file.
        let b = |v| vec![file("b.json", v)];
        let got = polls(
            b(Some(1)),
            vec![
                vec![unreadable(SourceId::LinuxFile("b.json".into()))],
                b(None),
                b(None),
            ],
        );
        assert_eq!(got[0], b(Some(1)));
        assert_eq!(got[1], b(Some(1)), "the readable empty file seen once");
        assert_eq!(got[2], b(None));
    }

    #[test]
    fn new_unreadable_file_applies_while_the_directory_is_held() {
        let start = vec![file("b.json", Some(1)), file("a.json", Some(0))];
        let c_broken = || unreadable(SourceId::LinuxFile("c.json".into()));
        let dir_err = || unreadable(SourceId::LinuxDirectory);
        let fresh = vec![dir_err(), c_broken(), file("a.json", Some(0))];
        let mut d = Debounce::new(start.clone());
        assert_eq!(
            d.accept(fresh.clone()),
            [c_broken(), file("b.json", Some(1)), file("a.json", Some(0))].as_slice(),
            "the directory is held, the new unreadable file is not"
        );
        assert_eq!(d.held(), [Hold::Unreadable(SourceId::LinuxDirectory)]);
        assert_eq!(d.accept(fresh.clone()), fresh.as_slice());

        // A new readable file is part of the held directory.
        let fresh = vec![dir_err(), file("c.json", Some(1)), file("a.json", Some(0))];
        assert_eq!(polls(start.clone(), vec![fresh])[0], start);
    }

    #[test]
    fn linux_unreadable_file_then_emptied_directory() {
        let start = vec![file("b.json", Some(1)), file("a.json", Some(0))];
        let b_broken = || unreadable(SourceId::LinuxFile("b.json".into()));
        let first_b = || vec![b_broken(), file("a.json", Some(0))];
        let first = first_b();
        let second = vec![b_broken(), file("a.json", None)];
        let got = polls(start.clone(), vec![first, second.clone(), second.clone()]);
        assert_eq!(got[0], start, "b held");
        // b is still unreadable, so the directory does not count as emptied:
        // b's change was seen twice and a's change passes with it.
        assert_eq!(got[1], second);
        assert_eq!(got[2], second);
        // Once every file is readable, the emptied directory is held.
        let empty = vec![file("b.json", None), file("a.json", None)];
        let got = polls(start.clone(), vec![first_b(), empty.clone(), empty.clone()]);
        assert_eq!(got[0], start);
        assert_eq!(got[1], start, "the directory emptied: held as a whole");
        assert_eq!(got[2], empty);
    }

    #[test]
    fn linux_directory_turning_unlistable_is_held() {
        let dir_err = || unreadable(SourceId::LinuxDirectory);
        // Part of the listing fails while another file still has values.
        let start = vec![file("b.json", Some(1)), file("a.json", Some(0))];
        let partial = vec![dir_err(), file("a.json", Some(0))];
        let got = polls(start.clone(), vec![partial.clone(), partial.clone()]);
        assert_eq!(got, [start, partial.clone()]);
        // The directory had only files without values.
        let start = vec![file("a.json", None)];
        let got = polls(start.clone(), vec![vec![dir_err()], vec![dir_err()]]);
        assert_eq!(got, [start, vec![dir_err()]]);
        // The directory was listed with no files (or was missing).
        let got = polls(vec![], vec![vec![dir_err()], vec![dir_err()]]);
        assert_eq!(got, [vec![], vec![dir_err()]]);
        // Unlistable already when accepted: nothing to hold.
        let got = polls(partial.clone(), vec![partial.clone()]);
        assert_eq!(got, [partial]);
    }

    #[test]
    fn held_linux_file_whose_trust_changed_keeps_the_accepted_order() {
        let mut c_broken = unreadable(SourceId::LinuxFile("c.json".into()));
        c_broken.trust = Trust::Limited;
        let start = vec![file("c.json", Some(1)), file("b.json", Some(0))];
        let fresh = vec![file("b.json", Some(0)), c_broken];
        let got = polls(start.clone(), vec![fresh.clone(), fresh.clone()]);
        assert_eq!(got[0], start, "c still outranks b during the hold");
        let ctx = rustket_core::policy::Context {
            client_version: "0.4.0".into(),
            os: rustket_core::policy::Os::Linux,
            mas: false,
            self_update: false,
        };
        assert!(rustket_core::policy::resolve(&got[0], &ctx).require_https());
        assert_eq!(got[1], fresh);
    }
}
