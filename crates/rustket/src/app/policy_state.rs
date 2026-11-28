//! The blocking state of the enterprise policy, its sign-in keys and
//! `LaunchAtLogin` in the app (docs/policy.md sections 6, 9.4, 9.6 and 9.7).
//! While the policy is blocked every server is suspended
//! (`crate::policy::servers::standing`), so start-up, refresh, tear-down and
//! restart are the server policy's own paths (`app/server_policy.rs`); this
//! file adds the screen, what it leaves behind and the keys that act on
//! view state and on the OS.

use iced::Task;
use rust_i18n::t;
use rustket_ui::widget::BannerKind;

use super::{App, Screen};
use crate::desktop::autostart::{self, State};
use crate::message::{Go, Message};
use crate::screens::login::SignInRules;
use crate::screens::policy_block;

/// Whether a toast action leads behind the blocking screen (a page, a room,
/// a sign-out): refused while blocked, so such toasts get no button.
pub(super) fn leads_behind_the_block(action: &Message) -> bool {
    match action {
        Message::Go(go) => !matches!(go, Go::Quit),
        Message::ToastAction(_, inner) => leads_behind_the_block(inner),
        _ => false,
    }
}

impl App {
    /// The policy just became blocked (a refresh): dialogs and pages over
    /// the accounts go, toast buttons that lead behind the block too; the
    /// servers are suspended by `apply_server_policy`. A window hidden to
    /// the tray comes back, since the tray falls silent.
    pub(super) fn enter_blocked(&mut self) -> Task<Message> {
        tracing::warn!("policy: blocked, every server is suspended");
        self.modals.clear();
        self.policy_block_details = false;
        if !matches!(self.screen, Screen::Server | Screen::Loading) {
            self.screen = self.workspace_screen();
        }
        for toast in &mut self.toasts {
            if toast
                .action
                .as_ref()
                .is_some_and(|(_, m)| leads_behind_the_block(m))
            {
                toast.action = None;
            }
        }
        if self.desktop.hidden {
            self.desktop.show_window().map(Message::Desktop)
        } else {
            Task::none()
        }
    }

    /// The block ended (`set_policy`, after the servers were reconciled).
    /// One in force since start leaves the server chosen while every server
    /// was suspended: the start rule chooses again (docs/policy.md 9.1, 9.7).
    pub(super) fn policy_block_ended(&mut self) {
        if std::mem::take(&mut self.policy_blocked_at_start) {
            self.active = self.initial_active(self.config.active_server.clone());
        }
    }

    /// The updater's effective mode (docs/policy.md 9.3); `None` in a build
    /// without the updater, and without the key until the install kind is
    /// detected (the default may still turn out `notify`).
    pub(crate) fn update_mode(&self) -> Option<rustket_core::policy::UpdateMode> {
        #[cfg(feature = "self-update")]
        return (self.updater.kind.is_some() || self.policy.update_mode().is_some())
            .then(|| crate::updater::app::mode(self));
        #[cfg(not(feature = "self-update"))]
        None
    }

    pub(super) fn on_policy_block(&mut self, m: policy_block::Message) -> Task<Message> {
        match m {
            policy_block::Message::ToggleDetails => {
                self.policy_block_details = !self.policy_block_details;
                Task::none()
            }
            policy_block::Message::CopyReport => self.copy_policy_report(),
            policy_block::Message::Support => match self.policy.support_url() {
                Some(url) => self.open_url(url.to_owned()),
                None => Task::none(),
            },
            #[cfg(feature = "self-update")]
            policy_block::Message::CheckUpdates
                if policy_block::offers_update(self.update_mode()) =>
            {
                crate::updater::app::update(
                    self,
                    crate::updater::app::Msg::Check(crate::updater::state::CheckOrigin::Manual),
                )
            }
            policy_block::Message::CheckUpdates => Task::none(),
            policy_block::Message::Quit => self.go(Go::Quit),
        }
    }

    /// "Copy report": the `--dump-policy` JSON on the clipboard.
    fn copy_policy_report(&mut self) -> Task<Message> {
        match rustket_core::policy::dump_json(&self.policy) {
            Ok(json) => Task::batch([
                iced::clipboard::write(json),
                self.toast(BannerKind::Success, t!("policy.copied"), String::new()),
            ]),
            Err(e) => self.toast(BannerKind::Error, t!("policy.copy_failed"), e.to_string()),
        }
    }

    /// Section 9.4 at a refresh: every open login screen takes the new
    /// sign-in rules at once; a running session is not affected.
    pub(super) fn apply_sign_in_policy(&mut self) -> Task<Message> {
        let rules = SignInRules::of(&self.policy);
        for s in self.servers.values_mut() {
            if let Some(l) = s.login.as_mut()
                && *l.rules() != rules
            {
                l.set_rules(rules.clone());
            }
        }
        Task::none()
    }

    /// Section 9.6: once at start and once after every refresh that changed
    /// `LaunchAtLogin`, as soon as the accounts may run, the registration is
    /// read and set to the forced value if it differs, on a blocking thread.
    /// One attempt each time; nothing where launch at login is unsupported
    /// or the key is not set (the registration then stays as it is).
    pub(super) fn apply_launch_at_login(&mut self) -> Task<Message> {
        if !self.launch_at_login_due || !self.accounts_live() {
            return Task::none();
        }
        self.launch_at_login_due = false;
        let (Some(on), Some(at)) = (self.policy.launch_at_login(), self.autostart.clone()) else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let state = at.state();
                    if state.is_on() == on {
                        return Ok((state, false));
                    }
                    let exe = autostart::executable().map_err(|e| e.to_string())?;
                    at.set_enabled(on, &exe).map_err(|e| e.to_string())?;
                    Ok((at.state(), true))
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |r| Message::LaunchAtLoginApplied(on, r),
        )
    }

    /// A switch click of the Settings screen that was still running when
    /// the key came finished against the forced value: one more attempt
    /// (its outcome arrives as `LaunchAtLoginApplied`, so this cannot loop).
    pub(super) fn recheck_launch_at_login(
        &mut self,
        m: &crate::screens::settings::Message,
    ) -> Task<Message> {
        if let crate::screens::settings::Message::AutostartSet(_, Ok(state)) = m
            && self
                .policy
                .launch_at_login()
                .is_some_and(|on| on != state.is_on())
        {
            tracing::info!("policy: launch at login changed by a click still running");
            self.launch_at_login_due = true;
            return self.apply_launch_at_login();
        }
        Task::none()
    }

    /// The outcome of [`Self::apply_launch_at_login`]: logged; an open
    /// settings screen shows the state found; a warning only on failure.
    /// "Needs approval" (Mac App Store) is shown and not retried.
    pub(super) fn on_launch_at_login(
        &mut self,
        on: bool,
        r: Result<(State, bool), String>,
    ) -> Task<Message> {
        match r {
            Ok((state, changed)) => {
                tracing::info!(forced = on, ?state, changed, "policy: launch at login");
                if let Screen::Settings(s) = &mut self.screen {
                    s.autostart = Some(state.is_on());
                    s.autostart_approval = state == State::NeedsApproval;
                }
                Task::none()
            }
            Err(e) => {
                tracing::warn!(forced = on, error = %e, "policy: cannot set launch at login");
                self.toast(BannerKind::Warning, t!("policy.launch_at_login_failed"), e)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use rustket_core::policy::{Policy, RawValue, Snapshot, SourceId, Trust, resolve};

    use crate::app::{App, Flags, Screen, SecretsMode};
    use crate::desktop::autostart::{Autostart, State};
    use crate::message::Message;

    fn policy(values: &[(&str, RawValue)]) -> Arc<Policy> {
        let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
        for (k, v) in values {
            snap = snap.with(k, v.clone());
        }
        Arc::new(resolve(&[snap], &crate::policy::context()))
    }

    fn forced(on: bool) -> Arc<Policy> {
        policy(&[("LaunchAtLogin", RawValue::Bool(on))])
    }

    /// A config past the terms in English, saved to `dir`: `App::new`
    /// then selects English itself and never switches the process locale
    /// to the system's language (the tests read English texts).
    fn english_config(dir: &std::path::Path, edit: impl FnOnce(&mut AppConfig)) {
        let mut cfg = AppConfig::default();
        crate::legal::accept_terms(&mut cfg);
        cfg.ui.language = Language::new("en").unwrap();
        edit(&mut cfg);
        cfg.save(dir).unwrap();
    }

    /// An app past the terms with launch at login in `home` (a temporary
    /// directory, which also holds the config) and `policy` in force from
    /// the start.
    fn app(home: &std::path::Path, p: Arc<Policy>) -> (App, Autostart) {
        let dir = home.join("config");
        english_config(&dir, |_| {});
        let (mut a, _) = App::new(Flags {
            config_dir: Some(dir.clone()),
            cache_dir: Some(dir.join("cache")),
            secrets: SecretsMode::Memory,
            policy: p,
            ..Flags::default()
        });
        let at = Autostart::LaunchAgent {
            home: home.to_owned(),
        };
        a.autostart = Some(at.clone());
        (a, at)
    }

    /// Section 8: the refresh timer runs only in a real desktop run
    /// (`main` sets the flag); tests and snapshots never read the machine's
    /// policy on a timer.
    #[test]
    fn refresh_timer_only_when_asked() {
        use iced::advanced::subscription::into_recipes;
        let recipes = |policy_refresh| {
            let dir = tempfile::tempdir().unwrap();
            english_config(dir.path(), |_| {});
            let (a, _) = App::new(Flags {
                config_dir: Some(dir.path().to_owned()),
                cache_dir: Some(dir.path().join("cache")),
                secrets: SecretsMode::Memory,
                policy_refresh,
                ..Flags::default()
            });
            into_recipes(a.subscription()).len()
        };
        assert!(!Flags::default().policy_refresh);
        assert_eq!(recipes(true), recipes(false) + 1, "one more: the timer");
    }

    /// Runs `task` to its end and feeds what it produced back to the app.
    async fn settle(a: &mut App, task: iced::Task<Message>) -> usize {
        use iced::futures::StreamExt;
        let Some(mut s) = iced_runtime::task::into_stream(task) else {
            return 0;
        };
        let mut n = 0;
        let deadline = std::time::Duration::from_millis(500);
        while let Ok(Some(action)) = tokio::time::timeout(deadline, s.next()).await {
            if let iced_runtime::Action::Output(m @ Message::LaunchAtLoginApplied(..)) = action {
                n += 1;
                let _ = a.update(m);
            }
        }
        n
    }

    /// What `task` writes to the clipboard (it is drained only until it
    /// goes quiet: toast timers are not waited for).
    async fn clipboard_writes(task: iced::Task<Message>) -> Vec<String> {
        use iced::futures::StreamExt;
        let Some(mut s) = iced_runtime::task::into_stream(task) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let deadline = std::time::Duration::from_millis(200);
        while let Ok(Some(action)) = tokio::time::timeout(deadline, s.next()).await {
            if let iced_runtime::Action::Clipboard(iced_runtime::clipboard::Action::Write {
                contents,
                ..
            }) = action
            {
                out.push(contents);
            }
        }
        out
    }

    /// The start-up's attempt (`start_workspaces`, run when the secret
    /// store opens).
    fn start(a: &mut App) -> iced::Task<Message> {
        a.start_workspaces()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forced_values_are_set_once_per_start_and_change() {
        let home = tempfile::tempdir().unwrap();
        let (mut a, at) = app(home.path(), forced(true));
        assert_eq!(at.state(), State::Off);
        let task = start(&mut a);
        assert_eq!(settle(&mut a, task).await, 1);
        assert_eq!(at.state(), State::On, "forced on");
        // A second update attempts nothing.
        let task = start(&mut a);
        assert_eq!(settle(&mut a, task).await, 0);
        // The user turns it off outside Rustket: not fought until a change.
        at.set_enabled(false, std::path::Path::new("/x/rustket"))
            .unwrap();
        let task = a.set_policy(forced(true));
        assert_eq!(settle(&mut a, task).await, 0, "the key did not change");
        assert_eq!(at.state(), State::Off);
        // A change of the key: one attempt.
        let task = a.set_policy(forced(false));
        assert_eq!(settle(&mut a, task).await, 1);
        assert_eq!(at.state(), State::Off);
        let task = a.set_policy(forced(true));
        assert_eq!(settle(&mut a, task).await, 1);
        assert_eq!(at.state(), State::On);
        // The key goes away: the registration stays.
        let task = a.set_policy(Arc::default());
        assert_eq!(settle(&mut a, task).await, 0);
        assert_eq!(at.state(), State::On);
        assert!(a.toasts.is_empty(), "no toast on success");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forced_off_removes_the_registration() {
        let home = tempfile::tempdir().unwrap();
        let (mut a, at) = app(home.path(), forced(false));
        at.set_enabled(true, std::path::Path::new("/x/rustket"))
            .unwrap();
        let task = start(&mut a);
        assert_eq!(settle(&mut a, task).await, 1);
        assert_eq!(at.state(), State::Off);
    }

    /// A switch click still running when the key came, finishing against
    /// the forced value, gets one more attempt; one that agrees, or one
    /// without the key, gets none.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_click_still_running_is_undone_once() {
        use crate::screens::settings::Message as S;
        let home = tempfile::tempdir().unwrap();
        let (mut a, at) = app(home.path(), forced(false));
        let task = start(&mut a);
        assert_eq!(settle(&mut a, task).await, 1);
        assert_eq!(at.state(), State::Off);
        // The user's "on" lands after the policy's attempt.
        at.set_enabled(true, std::path::Path::new("/x/rustket"))
            .unwrap();
        let task = a.update(Message::Settings(S::AutostartSet(true, Ok(State::On))));
        assert_eq!(settle(&mut a, task).await, 1);
        assert_eq!(at.state(), State::Off, "the forced value");
        let task = a.update(Message::Settings(S::AutostartSet(false, Ok(State::Off))));
        assert_eq!(settle(&mut a, task).await, 0, "it agrees");
        // Without the key the user's choice stands.
        let task = a.set_policy(Arc::default());
        assert_eq!(settle(&mut a, task).await, 0);
        at.set_enabled(true, std::path::Path::new("/x/rustket"))
            .unwrap();
        let task = a.update(Message::Settings(S::AutostartSet(true, Ok(State::On))));
        assert_eq!(settle(&mut a, task).await, 0);
        assert_eq!(at.state(), State::On);
    }

    /// Nothing before the terms are accepted or while the policy blocks;
    /// the attempt comes once the block ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn waits_for_the_terms_and_the_block() {
        let home = tempfile::tempdir().unwrap();
        let (mut a, at) = app(home.path(), forced(true));
        a.config.accepted_terms = 0;
        let task = start(&mut a);
        assert_eq!(settle(&mut a, task).await, 0);
        crate::legal::accept_terms(&mut a.config);
        let mut values = vec![("LaunchAtLogin", RawValue::Bool(true))];
        values.push(("MinimumVersion", RawValue::String("999.0.0".into())));
        let task = a.set_policy(policy(&values));
        assert_eq!(settle(&mut a, task).await, 0, "blocked");
        assert_eq!(at.state(), State::Off);
        let task = a.set_policy(forced(true));
        assert_eq!(settle(&mut a, task).await, 1, "the block ended");
        assert_eq!(at.state(), State::On);
    }

    /// No launch-at-login location (unsupported, or not a desktop run):
    /// nothing happens and nothing is shown.
    #[tokio::test]
    async fn unsupported_does_nothing() {
        let home = tempfile::tempdir().unwrap();
        let (mut a, _) = app(home.path(), forced(true));
        a.autostart = None;
        let task = start(&mut a);
        assert_eq!(settle(&mut a, task).await, 0);
        assert!(a.toasts.is_empty());
    }

    // --- the blocking state -----------------------------------------------------

    use rustket_core::ServerId;
    use rustket_core::config::{AppConfig, Language, ServerEntry};
    use rustket_core::policy::{SourceError, SourceErrorKind};
    use rustket_core::secrets::{MemoryStore, SecretBackend, SwitchableStore};

    use crate::message::{Go, SecretsReady};
    use crate::screens::policy_block::Message as B;

    const FOREIGN_LINK: &str = "rocketchat://room?host=https://new.example&rid=R";
    const HEADING: &str = "Rustket cannot connect";

    /// The two blocking kinds: a source that cannot be read, and a client
    /// older than `MinimumVersion`.
    fn blocking_kinds() -> [(&'static str, Arc<Policy>); 2] {
        let unreadable = Snapshot::unreadable(
            SourceId::LinuxFile("corp.json".into()),
            SourceError::new(SourceErrorKind::InvalidJson).with_detail("EOF"),
        );
        [
            (
                "unreadable",
                Arc::new(resolve(&[unreadable], &crate::policy::context())),
            ),
            (
                "minimum version",
                policy(&[("MinimumVersion", RawValue::String("999.0.0".into()))]),
            ),
        ]
    }

    /// An app on a config with a signed-in server (`U1`, with an offline
    /// cache) and one without an account, under `p`; the secret store is
    /// open with the tokens the start-up read. `terms`: accepted before.
    fn blocked_app(dir: &std::path::Path, p: Arc<Policy>, terms: bool) -> (App, [ServerId; 2]) {
        let mut cfg = AppConfig::default();
        if terms {
            crate::legal::accept_terms(&mut cfg);
        }
        // English: `App::new` selects it itself and never switches the
        // process locale to the system's language.
        cfg.ui.language = Language::new("en").unwrap();
        let mut e = ServerEntry::new("https://chat.corp.example", "Corp");
        e.user_id = Some("U1".into());
        let signed = cfg.add_server(e);
        let other = cfg.add_server(ServerEntry::new("https://other.corp.example", "Other"));
        cfg.save(dir).unwrap();
        let (mut a, _) = App::new(Flags {
            config_dir: Some(dir.to_owned()),
            cache_dir: Some(dir.join("cache")),
            offline_cache: Some(dir.join("offline")),
            secrets: SecretsMode::Memory,
            policy: p,
            ..Flags::default()
        });
        let tokens = a
            .startup_token_reads()
            .into_iter()
            .map(|(sid, _)| (sid, Some("T-U1".to_owned())))
            .collect();
        let _ = a.update(Message::SecretsReady(SecretsReady {
            store: Arc::new(SwitchableStore::new(
                Box::new(MemoryStore::new()),
                SecretBackend::Memory,
                None,
            )),
            tokens,
            notices: vec![],
        }));
        (a, [signed, other])
    }

    fn texts(a: &App) -> Vec<String> {
        crate::tree_probe::texts(a.view())
    }

    /// No session, no login, no token read, no cache.
    fn stopped(a: &App, sid: &ServerId) -> bool {
        let s = &a.servers[sid];
        s.credentials.is_none()
            && s.spec().is_none()
            && s.login.is_none()
            && !s.restoring
            && !s.cache.is_open()
            && !s.cache.is_opening()
    }

    /// Blocked at start, each kind: the terms come first, then the blocking
    /// screen; no server starts (no token read, no login, no cache); a link
    /// waits; Settings and the add screen do not open; no unread reaches the
    /// title or the tray; the block's end starts the servers and the link.
    #[test]
    fn blocked_at_start() {
        for (kind, p) in blocking_kinds() {
            let dir = tempfile::tempdir().unwrap();
            let (mut a, [signed, other]) = blocked_app(dir.path(), p.clone(), false);
            assert!(a.startup_token_reads().is_empty(), "{kind}: a token read");
            let t = texts(&a);
            assert!(t.iter().any(|t| t == "Before you start"), "{kind}: {t:?}");
            assert!(!t.iter().any(|t| t == HEADING), "{kind}: before the terms");
            let _ = a.update(Message::Terms(crate::screens::terms::Message::Accept));
            let t = texts(&a);
            assert!(t.iter().any(|t| t == HEADING), "{kind}: {t:?}");
            for sid in [&signed, &other] {
                assert!(a.is_suspended(sid), "{kind}");
                assert!(stopped(&a, sid), "{kind}");
            }
            let _ = a.update(Message::Noop);
            assert!(stopped(&a, &signed) && stopped(&a, &other), "{kind}");

            // A link waits.
            let _ = a.update(Message::DeepLinks(vec![FOREIGN_LINK.into()]));
            assert!(a.modals.is_empty() && a.toasts.is_empty(), "{kind}");
            assert_eq!(a.queued_links, vec![FOREIGN_LINK.to_owned()], "{kind}");

            // No settings, no add screen, no shortcuts.
            for m in [
                Message::Rail(crate::workspace::rail::Message::Settings),
                Message::Go(Go::Nav(crate::component::Nav::Settings)),
                Message::Go(Go::Nav(crate::component::Nav::PolicySettings)),
                Message::ShowAddServer,
                Message::Shortcut(crate::keymap::Shortcut::Settings),
            ] {
                let _ = a.update(m);
                assert!(
                    !matches!(a.screen, Screen::Settings(_) | Screen::AddServer(_)),
                    "{kind}"
                );
            }
            assert!(!a.menu_may_navigate(&crate::component::Nav::AboutSettings));

            // Counts an offline cache could have loaded show nowhere.
            a.servers.get_mut(&signed).unwrap().workspace.apply_initial(
                rustket_core::sync::InitialData {
                    subscriptions: vec![rocketchat::models::Subscription {
                        id: "s-R1".into(),
                        rid: "R1".into(),
                        name: "R1".into(),
                        open: true,
                        alert: true,
                        unread: 3,
                        user_mentions: 2,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            );
            let _ = a.update(Message::Noop);
            assert_eq!(a.title(), "Rustket", "{kind}");
            let tray = a.desktop.tray.state();
            assert_eq!((tray.unread, tray.mentions), (0, 0), "{kind}");

            // The block ends: the servers start as at start-up, then the link.
            let task = a.set_policy(Arc::default());
            assert!(task.units() > 0, "{kind}");
            assert!(a.servers[&signed].restoring, "{kind}: no token read");
            assert!(a.servers[&other].login.is_some(), "{kind}: no login");
            assert!(a.queued_links.is_empty(), "{kind}");
            assert!(
                matches!(
                    a.modals.last(),
                    Some(crate::app::Modal::OfferAddServer { .. })
                ),
                "{kind}: the link was not handled"
            );
            assert!(!texts(&a).iter().any(|t| t == HEADING), "{kind}");
        }
    }

    /// Whether `task` exits the application (drained until it goes quiet).
    async fn exits(task: iced::Task<Message>) -> bool {
        use iced::futures::StreamExt;
        let Some(mut s) = iced_runtime::task::into_stream(task) else {
            return false;
        };
        let deadline = std::time::Duration::from_millis(200);
        while let Ok(Some(action)) = tokio::time::timeout(deadline, s.next()).await {
            if matches!(action, iced_runtime::Action::Exit) {
                return true;
            }
        }
        false
    }

    /// A block in force at start ends: the server to show is chosen again by
    /// the start rule (the stored one is suspended, another is allowed). A
    /// block that started at runtime keeps the server shown.
    #[test]
    fn a_block_from_start_ends_into_the_start_rule() {
        let only = |host: &str| {
            policy(&[(
                "AllowedServers",
                RawValue::List(vec![RawValue::String(host.into())]),
            )])
        };
        for (kind, p) in blocking_kinds() {
            let dir = tempfile::tempdir().unwrap();
            let (mut a, [corp, other]) = blocked_app(dir.path(), p.clone(), true);
            assert_eq!(a.active.as_ref(), Some(&corp), "{kind}");
            let _ = a.set_policy(only("other.corp.example"));
            assert!(a.is_suspended(&corp) && !a.is_suspended(&other), "{kind}");
            assert_eq!(a.active.as_ref(), Some(&other), "{kind}: the suspended one");
            assert!(matches!(a.screen, Screen::Server), "{kind}");
            assert!(a.servers[&other].login.is_some(), "{kind}: not started");
            // A block at runtime and its end leave the choice alone.
            let _ = a.set_policy(p.clone());
            let _ = a.set_policy(only("chat.corp.example"));
            assert_eq!(a.active.as_ref(), Some(&other), "{kind}");
        }
    }

    /// "Details" toggles the sources, "Copy report" puts the dump on the
    /// clipboard (the same JSON as `--dump-policy`) and "Quit" quits.
    #[tokio::test]
    async fn screen_actions() {
        let dir = tempfile::tempdir().unwrap();
        let (_, p) = blocking_kinds().into_iter().next().unwrap();
        let (mut a, _) = blocked_app(dir.path(), p.clone(), true);
        assert!(!texts(&a).iter().any(|t| t == "Sources"));
        let _ = a.update(Message::PolicyBlock(B::ToggleDetails));
        assert!(a.policy_block_details);
        assert!(texts(&a).iter().any(|t| t == "Sources"));
        let _ = a.update(Message::PolicyBlock(B::ToggleDetails));
        assert!(!a.policy_block_details);

        let task = a.update(Message::PolicyBlock(B::CopyReport));
        assert_eq!(
            a.toasts.last().unwrap().title,
            "Report copied to the clipboard"
        );
        assert_eq!(
            clipboard_writes(task).await,
            vec![rustket_core::policy::dump_json(&p).unwrap()],
            "not the dump's JSON"
        );

        // The support link needs one; without it nothing opens.
        a.open_urls = true;
        assert_eq!(a.update(Message::PolicyBlock(B::Support)).units(), 0);
        a.open_urls = false;

        let task = a.update(Message::PolicyBlock(B::Quit));
        assert!(exits(task).await, "no exit");
    }

    /// With a `SupportUrl` "Contact support" opens it (the task is counted,
    /// never run: no browser starts).
    #[test]
    fn support_action_opens_the_support_url() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy(&[
            ("MinimumVersion", RawValue::String("999.0.0".into())),
            (
                "SupportUrl",
                RawValue::String("https://help.acme.example".into()),
            ),
        ]);
        let (mut a, _) = blocked_app(dir.path(), p, true);
        assert!(texts(&a).iter().any(|t| t == "Contact support"));
        a.open_urls = true;
        let task = a.update(Message::PolicyBlock(B::Support));
        assert!(task.units() > 0, "nothing opens");
        assert!(a.toasts.is_empty(), "the URL was refused");
    }

    /// "Details" is closed again when a new block starts at runtime.
    #[test]
    fn details_close_when_a_new_block_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (_, p) = blocking_kinds().into_iter().next().unwrap();
        let (mut a, _) = blocked_app(dir.path(), p.clone(), true);
        let _ = a.update(Message::PolicyBlock(B::ToggleDetails));
        assert!(a.policy_block_details);
        let _ = a.set_policy(Arc::default());
        let _ = a.set_policy(p);
        assert!(!a.policy_block_details);
        assert!(!texts(&a).iter().any(|t| t == "Sources"));
    }

    /// The update action of the blocking screen follows the effective
    /// mode, the install kind's default included: offered and run only in
    /// `prompt`.
    #[cfg(feature = "self-update")]
    #[test]
    fn update_action_follows_the_effective_mode() {
        use crate::updater::InstallKind;
        use crate::updater::detect::InnoScope;
        use crate::updater::state::Phase;
        let min = ("MinimumVersion", RawValue::String("999.0.0".into()));
        let inno = |scope| InstallKind::InnoSetup {
            scope,
            dir: "C:/R".into(),
        };
        for (mode, kind, offered) in [
            (None, inno(InnoScope::CurrentUser), true),
            (None, inno(InnoScope::AllUsers), false),
            (None, InstallKind::Msi { dir: "C:/R".into() }, false),
            (
                None,
                InstallKind::MacPkg {
                    bundle: "/Applications/Rustket.app".into(),
                },
                false,
            ),
            (Some("prompt"), inno(InnoScope::AllUsers), true),
            (Some("notify"), inno(InnoScope::CurrentUser), false),
            (Some("off"), inno(InnoScope::CurrentUser), false),
        ] {
            let mut values = vec![min.clone()];
            if let Some(m) = mode {
                values.push(("UpdateMode", RawValue::String(m.into())));
            }
            let dir = tempfile::tempdir().unwrap();
            let (mut a, _) = blocked_app(dir.path(), policy(&values), true);
            a.updater.enabled = true;
            a.updater.kind = Some(kind.clone());
            let shown = texts(&a).iter().any(|t| t == "Check for updates");
            assert_eq!(shown, offered, "{mode:?} {kind:?}");
            let _ = a.update(Message::PolicyBlock(B::CheckUpdates));
            assert_eq!(
                matches!(a.updater.phase, Phase::Checking { .. }),
                offered,
                "{mode:?} {kind:?}"
            );
        }
        // Before the kind is detected, the default may still be `notify`:
        // nothing is offered unless the key decides.
        for (mode, offered) in [(None, false), (Some("prompt"), true), (Some("off"), false)] {
            let mut values = vec![min.clone()];
            if let Some(m) = mode {
                values.push(("UpdateMode", RawValue::String(m.into())));
            }
            let dir = tempfile::tempdir().unwrap();
            let (mut a, _) = blocked_app(dir.path(), policy(&values), true);
            a.updater.enabled = true;
            a.updater.kind = None;
            let shown = texts(&a).iter().any(|t| t == "Check for updates");
            assert_eq!(shown, offered, "{mode:?}");
            let _ = a.update(Message::PolicyBlock(B::CheckUpdates));
            assert_eq!(
                matches!(a.updater.phase, Phase::Checking { .. }),
                offered,
                "{mode:?}"
            );
        }
    }

    /// With the updater, "Check for updates" runs its manual check while
    /// blocked, only when `UpdateMode` lets it install.
    #[cfg(feature = "self-update")]
    #[test]
    fn update_action_runs_the_manual_check() {
        use crate::updater::state::Phase;
        let min = ("MinimumVersion", RawValue::String("999.0.0".into()));
        for (mode, runs) in [
            (None, true),
            (Some("prompt"), true),
            (Some("notify"), false),
            (Some("off"), false),
        ] {
            let mut values = vec![min.clone()];
            if let Some(m) = mode {
                values.push(("UpdateMode", RawValue::String(m.into())));
            }
            let dir = tempfile::tempdir().unwrap();
            let (mut a, _) = blocked_app(dir.path(), policy(&values), true);
            assert!(a.policy.is_blocked());
            a.updater.enabled = true;
            a.updater.kind = Some(crate::updater::InstallKind::AppImage {
                image: "/tmp/x.AppImage".into(),
            });
            let _ = a.update(Message::PolicyBlock(B::CheckUpdates));
            assert_eq!(
                matches!(a.updater.phase, Phase::Checking { .. }),
                runs,
                "{mode:?}"
            );
        }
    }

    /// A failure is a warning; "needs approval" is shown on an open
    /// settings screen and not retried.
    #[tokio::test]
    async fn outcomes() {
        let home = tempfile::tempdir().unwrap();
        let (mut a, _) = app(home.path(), forced(true));
        // The start-up's attempt (its result is delivered by hand below).
        assert!(start(&mut a).units() > 0);
        let _ = a.update(Message::LaunchAtLoginApplied(true, Err("denied".into())));
        assert_eq!(a.toasts.len(), 1);
        assert_eq!(
            a.toasts[0].title,
            rust_i18n::t!("policy.launch_at_login_failed")
        );
        let _ = a.update(Message::Go(crate::message::Go::Nav(
            crate::component::Nav::Settings,
        )));
        let _ = a.update(Message::LaunchAtLoginApplied(
            true,
            Ok((State::NeedsApproval, true)),
        ));
        let Screen::Settings(s) = &a.screen else {
            panic!("settings did not open");
        };
        assert_eq!(s.autostart, Some(true));
        assert!(s.autostart_approval);
        assert!(!a.launch_at_login_due, "not retried");
        assert_eq!(start(&mut a).units(), 0, "a second attempt");
    }
}
