//! Preferences overlay (docs/policy.md section 7): the `UiPrefs` fields a
//! policy forces. Nothing else in `UiPrefs` is touched.

use super::Policy;
use super::registry::{Key, UpdateMode};
use crate::config::UiPrefs;

/// The `UiPrefs` fields a policy can force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockedPref {
    NotificationPreview,
    HardwareAcceleration,
    SecretsMemoryOnly,
    CheckUpdates,
    UpdatePrereleases,
}

impl LockedPref {
    pub const ALL: [LockedPref; 5] = [
        LockedPref::NotificationPreview,
        LockedPref::HardwareAcceleration,
        LockedPref::SecretsMemoryOnly,
        LockedPref::CheckUpdates,
        LockedPref::UpdatePrereleases,
    ];

    /// The key that forces this field.
    pub fn key(self) -> Key {
        match self {
            LockedPref::NotificationPreview => Key::NotificationPreview,
            LockedPref::HardwareAcceleration => Key::HardwareAcceleration,
            LockedPref::SecretsMemoryOnly => Key::AllowSecretsFile,
            LockedPref::CheckUpdates | LockedPref::UpdatePrereleases => Key::UpdateMode,
        }
    }

    fn field(self, prefs: &mut UiPrefs) -> &mut bool {
        match self {
            LockedPref::NotificationPreview => &mut prefs.notification_preview,
            LockedPref::HardwareAcceleration => &mut prefs.hardware_acceleration,
            LockedPref::SecretsMemoryOnly => &mut prefs.secrets_memory_only,
            LockedPref::CheckUpdates => &mut prefs.check_updates,
            LockedPref::UpdatePrereleases => &mut prefs.update_prereleases,
        }
    }
}

impl Policy {
    /// The value the policy forces on `field`, if it forces one.
    pub fn forced(&self, field: LockedPref) -> Option<bool> {
        match field {
            LockedPref::NotificationPreview => self.notification_preview(),
            LockedPref::HardwareAcceleration => self.hardware_acceleration(),
            LockedPref::SecretsMemoryOnly => match self.allow_secrets_file() {
                Some(false) => Some(true),
                Some(true) | None => None,
            },
            LockedPref::CheckUpdates => self.update_mode().map(|m| m != UpdateMode::Off),
            LockedPref::UpdatePrereleases => self.update_mode().map(|_| false),
        }
    }

    /// Whether `field` is forced.
    pub fn locks(&self, field: LockedPref) -> bool {
        self.forced(field).is_some()
    }

    /// The effective preferences: `user` with every forced field set.
    pub fn apply(&self, user: &UiPrefs) -> UiPrefs {
        let mut out = user.clone();
        for field in LockedPref::ALL {
            if let Some(v) = self.forced(field) {
                *field.field(&mut out) = v;
            }
        }
        out
    }

    /// `effective` with every forced field put back to `user`'s value: the
    /// user's view after code wrote into the effective copy.
    ///
    /// `self` must be the policy that produced `effective`. When the policy
    /// changed since, use [`Policy::reconcile`]: unapplying with the new
    /// policy would copy a value forced by the old one into the user's view.
    pub fn unapply(&self, effective: &UiPrefs, user: &UiPrefs) -> UiPrefs {
        let mut out = effective.clone();
        let mut user = user.clone();
        for field in LockedPref::ALL {
            if self.locks(field) {
                *field.field(&mut out) = *field.field(&mut user);
            }
        }
        out
    }

    /// Section 7's reconcile across a policy change: `effective` was produced
    /// by `old`. Returns the user's view and the new effective preferences:
    /// `(old.unapply(effective, user), new.apply(that))`. With `old == new`
    /// this is the reconcile after an ordinary update.
    pub fn reconcile(
        old: &Policy,
        new: &Policy,
        effective: &UiPrefs,
        user: &UiPrefs,
    ) -> (UiPrefs, UiPrefs) {
        let user = old.unapply(effective, user);
        let effective = new.apply(&user);
        (user, effective)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::policy::{Context, Os, RawValue, Snapshot, SourceId, Trust, resolve};

    fn ctx() -> Context {
        Context {
            client_version: "0.4.0".into(),
            os: Os::Linux,
            mas: false,
            self_update: true,
        }
    }

    fn policy(values: &[(&str, RawValue)]) -> Policy {
        let mut snap = Snapshot::new(SourceId::Test("a".into()), Trust::Full);
        for (k, v) in values {
            snap = snap.with(k, v.clone());
        }
        resolve(&[snap], &ctx())
    }

    fn s(v: &str) -> RawValue {
        RawValue::String(v.into())
    }

    /// User preferences with every combination of the five lockable fields
    /// and some free fields changed.
    fn users() -> Vec<UiPrefs> {
        let mut out = Vec::new();
        for bits in 0u8..32 {
            let mut u = UiPrefs::default();
            for (i, f) in LockedPref::ALL.into_iter().enumerate() {
                *f.field(&mut u) = bits & (1 << i) != 0;
            }
            u.sound = bits % 3 == 0;
            u.zoom = 1.0 + f32::from(bits) / 10.0;
            out.push(u);
        }
        out
    }

    fn policies() -> Vec<Policy> {
        let b = RawValue::Bool;
        vec![
            Policy::default(),
            policy(&[("NotificationPreview", b(false))]),
            policy(&[
                ("NotificationPreview", b(true)),
                ("HardwareAcceleration", b(false)),
            ]),
            policy(&[("AllowSecretsFile", b(false))]),
            policy(&[("AllowSecretsFile", b(true))]),
            policy(&[("UpdateMode", s("off"))]),
            policy(&[("UpdateMode", s("notify")), ("AllowSecretsFile", b(false))]),
            policy(&[("UpdateMode", s("garbage"))]),
            policy(&[
                ("NotificationPreview", b(true)),
                ("HardwareAcceleration", b(true)),
                ("AllowSecretsFile", b(false)),
                ("UpdateMode", s("prompt")),
            ]),
        ]
    }

    #[test]
    fn unapply_of_apply_is_identity() {
        for p in policies() {
            for u in users() {
                assert_eq!(p.unapply(&p.apply(&u), &u), u, "{p:?}");
            }
        }
    }

    #[test]
    fn apply_touches_only_forced_fields() {
        for p in policies() {
            for u in users() {
                let e = p.apply(&u);
                let mut e2 = e.clone();
                let mut u2 = u.clone();
                for f in LockedPref::ALL {
                    match p.forced(f) {
                        Some(v) => assert_eq!(*f.field(&mut e2), v),
                        None => assert_eq!(*f.field(&mut e2), *f.field(&mut u2)),
                    }
                    *f.field(&mut e2) = false;
                    *f.field(&mut u2) = false;
                }
                assert_eq!(e2, u2, "a field outside the table changed");
            }
        }
    }

    #[test]
    fn writes_to_free_fields_survive_forced_ones_are_undone() {
        let p = policy(&[
            ("NotificationPreview", RawValue::Bool(false)),
            ("UpdateMode", s("off")),
        ]);
        let user = UiPrefs::default();
        let mut effective = p.apply(&user);
        assert!(!effective.notification_preview);
        assert!(!effective.check_updates);
        effective.sound = !user.sound;
        effective.secrets_memory_only = !user.secrets_memory_only;
        effective.notification_preview = true;
        effective.check_updates = true;
        effective.update_prereleases = true;
        let new_user = p.unapply(&effective, &user);
        assert_eq!(new_user.sound, !user.sound);
        assert_eq!(new_user.secrets_memory_only, !user.secrets_memory_only);
        assert_eq!(new_user.notification_preview, user.notification_preview);
        assert_eq!(new_user.check_updates, user.check_updates);
        assert_eq!(new_user.update_prereleases, user.update_prereleases);
        let reapplied = p.apply(&new_user);
        assert!(!reapplied.notification_preview);
        assert!(!reapplied.check_updates);
        assert!(!reapplied.update_prereleases);
        assert_eq!(reapplied.sound, !user.sound);
    }

    #[test]
    fn secrets_file() {
        let allowed = policy(&[("AllowSecretsFile", RawValue::Bool(true))]);
        assert!(!allowed.locks(LockedPref::SecretsMemoryOnly));
        assert!(LockedPref::ALL.iter().all(|f| !allowed.locks(*f)));
        let u = UiPrefs::default();
        assert_eq!(allowed.apply(&u), u);

        let denied = policy(&[("AllowSecretsFile", RawValue::Bool(false))]);
        assert_eq!(denied.forced(LockedPref::SecretsMemoryOnly), Some(true));
        assert!(denied.apply(&u).secrets_memory_only);
        // An invalid value applies the strict `false`.
        let invalid = policy(&[("AllowSecretsFile", s("no"))]);
        assert_eq!(invalid.forced(LockedPref::SecretsMemoryOnly), Some(true));

        // The Mac App Store flavor never writes the file: not applicable.
        let mas = Context {
            os: Os::MacOs,
            mas: true,
            self_update: false,
            ..ctx()
        };
        for value in [RawValue::Bool(false), s("no")] {
            let snap = Snapshot::new(SourceId::Test("a".into()), Trust::Full)
                .with("AllowSecretsFile", value);
            let p = resolve(&[snap], &mas);
            assert_eq!(p.rows()[0].status, crate::policy::RowStatus::NotApplicable);
            assert_eq!(p.allow_secrets_file(), None);
            assert!(!p.locks(LockedPref::SecretsMemoryOnly));
            assert_eq!(p.apply(&u), u);
        }
    }

    #[test]
    fn update_mode_values() {
        for (mode, check) in [
            ("prompt", true),
            ("notify", true),
            ("off", false),
            ("bad", false),
        ] {
            let p = policy(&[("UpdateMode", s(mode))]);
            assert_eq!(p.forced(LockedPref::CheckUpdates), Some(check), "{mode}");
            assert_eq!(
                p.forced(LockedPref::UpdatePrereleases),
                Some(false),
                "{mode}"
            );
            let u = UiPrefs {
                check_updates: !check,
                update_prereleases: true,
                ..UiPrefs::default()
            };
            let e = p.apply(&u);
            assert_eq!(e.check_updates, check);
            assert!(!e.update_prereleases);
        }
        let none = Policy::default();
        assert!(!none.locks(LockedPref::CheckUpdates));
        assert!(!none.locks(LockedPref::UpdatePrereleases));
        // Without the updater the key does not apply and forces nothing.
        let snap =
            Snapshot::new(SourceId::Test("a".into()), Trust::Full).with("UpdateMode", s("off"));
        let p = resolve(
            &[snap],
            &Context {
                self_update: false,
                ..ctx()
            },
        );
        assert!(!p.locks(LockedPref::CheckUpdates));
    }

    #[test]
    fn keys_of_fields() {
        let forcing = |key: Key| match key {
            Key::NotificationPreview | Key::HardwareAcceleration => RawValue::Bool(true),
            Key::AllowSecretsFile => RawValue::Bool(false),
            Key::UpdateMode => s("off"),
            other => panic!("{other} forces no field"),
        };
        for f in LockedPref::ALL {
            assert!(!Policy::default().locks(f));
            let p = policy(&[(f.key().name(), forcing(f.key()))]);
            assert!(p.locks(f), "{f:?}");
            for g in LockedPref::ALL {
                assert_eq!(p.locks(g), g.key() == f.key(), "{f:?} / {g:?}");
            }
        }
    }

    #[test]
    fn reconcile_across_a_policy_change() {
        // The forced `false` of the old policy must not reach the user's view
        // when the new policy no longer forces the field.
        let old = policy(&[("UpdateMode", s("off"))]);
        let new = Policy::default();
        let user = UiPrefs {
            check_updates: true,
            ..UiPrefs::default()
        };
        let mut effective = old.apply(&user);
        assert!(!effective.check_updates);
        effective.sound = !user.sound;
        let (user2, effective2) = Policy::reconcile(&old, &new, &effective, &user);
        assert!(
            user2.check_updates,
            "the forced value leaked into the user copy"
        );
        assert_eq!(user2.sound, !user.sound);
        assert_eq!(effective2, user2);
        // Same policy: the ordinary reconcile.
        let (user3, effective3) = Policy::reconcile(&old, &old, &effective, &user);
        assert!(user3.check_updates);
        assert!(!effective3.check_updates);
        // A policy that starts forcing a field.
        let (user4, effective4) = Policy::reconcile(&new, &old, &user, &user);
        assert_eq!(user4, user);
        assert!(!effective4.check_updates);
    }
}
