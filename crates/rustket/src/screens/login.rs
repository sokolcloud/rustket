//! Login for one server: form (password / LDAP), 2FA dialog, login services
//! (OAuth legacy/modern, SAML, CAS), "sign in with browser", personal access
//! token. Strings: `locales/login.yml`.

use std::time::{Duration, Instant};

use iced::task::Handle;
use iced::widget::{button, column, container, row, rule, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Task, Theme};
use rocketchat::auth::{
    OAuthOptions, TwoFactorCode, browser_login_url, cas_login, modern_oauth_url, oauth_login,
    saml_login,
};
use rocketchat::models::{LoginServiceConfig, LoginServiceKind, ServerInfo, User};
use rocketchat::rest::{LdapLogin, LoginRequest, LoginResult};
use rocketchat::tls::Fingerprint;
use rocketchat::{
    Credentials, Error as RcError, RestClient, ServerUrl, TlsContext, TwoFactorMethod,
};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, font_medium, radius_md, tokens,
};
use rustket_ui::widget::{
    BannerKind, above, banner, dialog, ghost_button, primary_button, secondary_button, spinner,
    text_field,
};

use crate::deeplink::{PendingKind, PendingLogin};
use crate::error::{Failure, Res};
use crate::session::tls_for;
use rustket_core::config::TokenKind;

/// What the enterprise policy lets the screen offer (docs/policy.md
/// section 9.4): it shapes the client only, the server still accepts every
/// method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInRules {
    /// `AllowPasswordLogin`: the form (password or LDAP) and its messages.
    pub password: bool,
    /// `AllowTokenLogin`: the personal access token entry and its messages.
    pub token: bool,
    /// `OrganizationName`, named in the notice line.
    pub organization: Option<String>,
}

impl Default for SignInRules {
    fn default() -> Self {
        Self {
            password: true,
            token: true,
            organization: None,
        }
    }
}

impl SignInRules {
    pub fn of(policy: &rustket_core::policy::Policy) -> Self {
        Self {
            password: policy.allow_password_login(),
            token: policy.allow_token_login(),
            organization: policy.organization_name().map(str::to_owned),
        }
    }

    /// "Some sign-in methods are turned off by …".
    fn notice(&self) -> String {
        match &self.organization {
            Some(name) => t!("login.methods_off_by", name = name).into_owned(),
            None => t!("login.methods_off").into_owned(),
        }
    }
}

/// Whether `request` sends a password (the form's request).
fn sends_password(request: &LoginRequest) -> bool {
    matches!(
        request,
        LoginRequest::Password { .. } | LoginRequest::Ldap(_)
    )
}

/// How long SAML/CAS logins are polled.
pub const POLL_LIMIT: Duration = Duration::from_secs(5 * 60);

/// Public settings that shape the login screen.
#[derive(Debug, Clone, Default)]
pub struct LoginMeta {
    pub site_name: String,
    /// `Accounts_ShowFormLogin`.
    pub show_form: bool,
    /// `LDAP_Enable`: the form logs in with `{ldap: true}`.
    pub ldap: bool,
    /// `Accounts_OAuth_Use_Modern_Flow`.
    pub modern_oauth: bool,
    pub cas_login_url: Option<String>,
    pub gitlab_url: Option<String>,
    /// Visible login services (`settings.oauth`, filtered by `showButton`).
    pub services: Vec<LoginServiceConfig>,
}

/// Result of checking a server before adding it.
#[derive(Debug, Clone)]
pub struct ServerProbe {
    pub url: ServerUrl,
    pub info: ServerInfo,
    pub meta: LoginMeta,
}

#[derive(Debug, Clone)]
pub enum ProbeError {
    /// Untrusted certificate; `chain` is what the server presented (leaf first).
    Certificate {
        host: String,
        chain: Vec<rocketchat::CertificateInfo>,
    },
    Other(Failure),
}

impl ProbeError {
    pub fn user_text(&self) -> String {
        match self {
            ProbeError::Certificate { host, .. } => {
                t!("errors.certificate", host = host).into_owned()
            }
            ProbeError::Other(f) => f.user_text(),
        }
    }
}

const META_SETTINGS: &[&str] = &[
    "Site_Name",
    "Accounts_ShowFormLogin",
    "LDAP_Enable",
    "Accounts_OAuth_Use_Modern_Flow",
    "CAS_enabled",
    "CAS_login_url",
    "API_Gitlab_URL",
];

/// `/api/info` + public settings + login services.
pub async fn fetch_meta(rest: &RestClient) -> rocketchat::Result<(ServerInfo, LoginMeta)> {
    let info = rest.info().await?;
    let (settings, services) = tokio::join!(
        rest.settings_public_by_ids(META_SETTINGS),
        rest.settings_oauth()
    );
    let settings = settings.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "public settings unavailable");
        Vec::new()
    });
    let services = services.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "login services unavailable");
        Vec::new()
    });
    let get = |id: &str| settings.iter().find(|s| s.id == id);
    let cas_enabled = get("CAS_enabled").and_then(|s| s.as_bool()) == Some(true);
    let meta = LoginMeta {
        site_name: get("Site_Name")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Rocket.Chat")
            .to_owned(),
        show_form: get("Accounts_ShowFormLogin")
            .and_then(|s| s.as_bool())
            .unwrap_or(true),
        ldap: get("LDAP_Enable")
            .and_then(|s| s.as_bool())
            .unwrap_or(false),
        modern_oauth: modern_oauth(
            &info,
            get("Accounts_OAuth_Use_Modern_Flow").and_then(|s| s.as_bool()),
        ),
        cas_login_url: cas_enabled
            .then(|| {
                get("CAS_login_url")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned)
            })
            .flatten()
            .filter(|s| !s.is_empty()),
        gitlab_url: get("API_Gitlab_URL")
            .and_then(|s| s.as_str())
            .map(str::to_owned)
            .filter(|s| !s.is_empty()),
        services: services.into_iter().filter(|s| s.is_visible()).collect(),
    };
    Ok((info, meta))
}

/// Whether OAuth buttons use the server-side flow (`/oauth/{service}?loginClient=desktop`).
/// Rocket.Chat 9.0 removed the legacy Meteor flow together with
/// `Accounts_OAuth_Use_Modern_Flow`; older servers follow the setting.
fn modern_oauth(info: &ServerInfo, setting: Option<bool>) -> bool {
    info.version_tuple().is_some_and(|(major, ..)| major >= 9) || setting == Some(true)
}

/// Checks a server for the add-server screen.
pub async fn probe(
    url: ServerUrl,
    pins: Vec<(String, Fingerprint)>,
) -> Result<ServerProbe, ProbeError> {
    let tls = tls_for(&pins).map_err(|e| ProbeError::Other(e.into()))?;
    let rest = RestClient::new(url.clone(), &tls).map_err(|e| ProbeError::Other(e.into()))?;
    match fetch_meta(&rest).await {
        Ok((info, meta)) => Ok(ServerProbe { url, info, meta }),
        Err(RcError::Certificate { host, reason }) => {
            let host = host.unwrap_or_else(|| url.url().host_str().unwrap_or_default().to_owned());
            let chain = match tls.take_rejected_chain(&host) {
                Some(c) => c,
                None => rocketchat::tls::probe_certificate(&url)
                    .await
                    .unwrap_or_default(),
            };
            if chain.is_empty() {
                return Err(ProbeError::Other(
                    RcError::Certificate {
                        host: Some(host),
                        reason,
                    }
                    .into(),
                ));
            }
            Err(ProbeError::Certificate { host, chain })
        }
        Err(e) => Err(ProbeError::Other(e.into())),
    }
}

/// Widget ids of the sign-in form and the 2FA code field.
pub const USER_ID: &str = "login-user";
pub const PASSWORD_ID: &str = "login-password";
pub const CODE_ID: &str = "login-2fa";

#[derive(Clone)]
pub enum Message {
    MetaLoaded(Res<(ServerInfo, LoginMeta)>),
    RetryMeta,
    User(String),
    Password(String),
    /// Enter in the user field: sign in, or move to an empty password field.
    SubmitUser,
    Submit,
    LoginDone(Res<LoginResult>),
    /// Result of a login started by an unauthenticated link
    /// ([`Login::complete_unconfirmed`]).
    LinkLoginDone(Res<LoginResult>),
    Code(String),
    SubmitCode,
    ResendCode,
    CodeResent(Res<()>),
    CancelCode,
    Service(usize),
    BrowserLogin,
    OpenAgain,
    CancelPending,
    Paste(String),
    SubmitPaste,
    RegisterHandler,
    ShowToken(bool),
    PatUserId(String),
    PatToken(String),
    SubmitPat,
    PatChecked(Credentials, Res<User>),
}

impl std::fmt::Debug for Message {
    /// Variant names only: passwords, codes, tokens and pasted links never
    /// reach logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::MetaLoaded(r) => return write!(f, "MetaLoaded(ok={})", r.is_ok()),
            Self::RetryMeta => "RetryMeta",
            Self::User(u) => return write!(f, "User({u:?})"),
            Self::Password(_) => "Password(<redacted>)",
            Self::SubmitUser => "SubmitUser",
            Self::Submit => "Submit",
            Self::LoginDone(r) => return write!(f, "LoginDone(ok={})", r.is_ok()),
            Self::LinkLoginDone(r) => return write!(f, "LinkLoginDone(ok={})", r.is_ok()),
            Self::Code(_) => "Code(<redacted>)",
            Self::SubmitCode => "SubmitCode",
            Self::ResendCode => "ResendCode",
            Self::CodeResent(_) => "CodeResent",
            Self::CancelCode => "CancelCode",
            Self::Service(i) => return write!(f, "Service({i})"),
            Self::BrowserLogin => "BrowserLogin",
            Self::OpenAgain => "OpenAgain",
            Self::CancelPending => "CancelPending",
            Self::Paste(_) => "Paste(<redacted>)",
            Self::SubmitPaste => "SubmitPaste",
            Self::RegisterHandler => "RegisterHandler",
            Self::ShowToken(b) => return write!(f, "ShowToken({b})"),
            Self::PatUserId(u) => return write!(f, "PatUserId({u:?})"),
            Self::PatToken(_) => "PatToken(<redacted>)",
            Self::SubmitPat => "SubmitPat",
            Self::PatChecked(c, r) => {
                return write!(f, "PatChecked({}, ok={})", c.user_id, r.is_ok());
            }
        };
        f.write_str(name)
    }
}

/// Requests to the app.
#[derive(Debug, Clone)]
pub enum Out {
    None,
    LoggedIn(Box<LoginResult>, TokenKind),
    /// A login started by an unauthenticated link succeeded: the account has
    /// to be confirmed by the user before it is used.
    Unconfirmed(Box<LoginResult>),
    OpenUrl(String),
    /// A pasted `rocketchat://auth` link: route it like a received deep link.
    DeepLink(String),
    RegisterHandler,
}

#[derive(Debug, Clone)]
pub struct TwoFactor {
    pub request: LoginRequest,
    pub method: TwoFactorMethod,
    pub email_or_username: Option<String>,
    pub code: String,
    pub busy: bool,
    pub error: Option<String>,
    pub resent: bool,
    /// The challenged login was started by an unauthenticated link: its
    /// result needs the account confirmation.
    pub unconfirmed: bool,
}

#[derive(Debug)]
pub struct Pending {
    pub login: PendingLogin,
    pub label: String,
    pub url: String,
    pub paste: String,
    pub poll: Option<Handle>,
    pub error: Option<String>,
}

/// Untrusted text (e.g. from a deep link) for display: control and bidi
/// characters removed, one line, at most `max` characters, quoted.
pub fn quote_untrusted(s: &str, max: usize) -> String {
    let clean = rustket_ui::widget::sanitize_bidi(s);
    let mut out: String = clean
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect();
    if clean.chars().count() > max {
        out.push('…');
    }
    format!("“{}”", out.trim())
}

pub struct Login {
    pub server_id: ServerId,
    pub server: ServerUrl,
    tls: TlsContext,
    rest: RestClient,
    pub meta: Option<LoginMeta>,
    pub meta_error: Option<String>,
    pub reason: Option<String>,
    pub user: String,
    pub password: String,
    pub busy: bool,
    pub error: Option<String>,
    pub two_factor: Option<TwoFactor>,
    pub pending: Option<Pending>,
    pub token_mode: bool,
    pub pat_user_id: String,
    pub pat_token: String,
    /// Shown in the pending view: whether `rocketchat://` already opens Rustket.
    pub handler_registered: bool,
    /// The sign-in methods the policy allows ([`Self::set_rules`]).
    rules: SignInRules,
    last_request: Option<LoginRequest>,
    /// The request of [`Self::complete_unconfirmed`], for its 2FA round.
    link_request: Option<LoginRequest>,
}

impl Login {
    /// Creates the screen; returns a task loading the server's login meta when
    /// `meta` is not known yet.
    pub fn new(
        server_id: ServerId,
        server: ServerUrl,
        tls: TlsContext,
        meta: Option<LoginMeta>,
        reason: Option<String>,
    ) -> Result<(Self, Task<Message>), rocketchat::Error> {
        let rest = RestClient::new(server.clone(), &tls)?;
        let task = if meta.is_none() {
            meta_task(rest.clone())
        } else {
            Task::none()
        };
        Ok((
            Self {
                server_id,
                server,
                tls,
                rest,
                meta,
                meta_error: None,
                reason,
                user: String::new(),
                password: String::new(),
                busy: false,
                error: None,
                two_factor: None,
                pending: None,
                token_mode: false,
                pat_user_id: String::new(),
                pat_token: String::new(),
                handler_registered: false,
                rules: SignInRules::default(),
                last_request: None,
                link_request: None,
            },
            task,
        ))
    }

    /// Uses `tls` from the next request on (the certificate rule of the
    /// policy changed); requests already running keep theirs.
    pub fn set_tls(&mut self, tls: TlsContext) -> Result<(), rocketchat::Error> {
        self.rest = RestClient::new(self.server.clone(), &tls)?;
        self.tls = tls;
        Ok(())
    }

    /// The TLS context of the screen's requests.
    #[cfg(test)]
    pub(crate) fn tls(&self) -> &TlsContext {
        &self.tls
    }

    /// The sign-in methods the policy allows.
    pub fn rules(&self) -> &SignInRules {
        &self.rules
    }

    /// The policy removes something this screen would show: the token
    /// entry, or a form the server offers (or may offer: its settings are
    /// not loaded yet).
    fn policy_hides_something(&self) -> bool {
        !self.rules.token
            || (!self.rules.password && self.meta.as_ref().is_none_or(|m| m.show_form))
    }

    /// Applies the policy's sign-in rules at once (they are view state): a
    /// method turned off leaves the screen, a typed password or token is
    /// dropped, and a code dialog of a password sign-in closes. A request
    /// already sent finishes.
    pub fn set_rules(&mut self, rules: SignInRules) {
        if !rules.password {
            self.password.clear();
            if self
                .two_factor
                .as_ref()
                .is_some_and(|tf| !tf.unconfirmed && sends_password(&tf.request))
            {
                self.two_factor = None;
                self.busy = false;
            }
        }
        if !rules.token {
            self.token_mode = false;
            self.pat_token.clear();
        }
        self.rules = rules;
    }

    pub fn pending_login(&self) -> Option<&PendingLogin> {
        self.pending.as_ref().map(|p| &p.login)
    }

    pub fn cancel_pending(&mut self) {
        if let Some(p) = self.pending.take()
            && let Some(h) = p.poll
        {
            h.abort();
        }
    }

    /// Finishes a pending login with credentials from a matched deep link.
    pub fn complete_with(&mut self, request: LoginRequest) -> Task<Message> {
        self.cancel_pending();
        self.run_login(request, None)
    }

    /// Logs in with credentials from an unauthenticated link (a SAML token the
    /// app did not generate). The result, also after a 2FA round, comes back
    /// as [`Out::Unconfirmed`], never as [`Out::LoggedIn`]. The pending login
    /// stays: the confirmation is bound to it.
    pub fn complete_unconfirmed(&mut self, request: LoginRequest) -> Task<Message> {
        if let Some(h) = self.pending.as_mut().and_then(|p| p.poll.take()) {
            h.abort();
        }
        self.two_factor = None;
        self.run_login_from(request, None, true)
    }

    /// An `error` deep link for this server while a browser login is pending:
    /// shown as a notice; the pending login stays (the link is unauthenticated,
    /// any web page can send one).
    pub fn show_link_error(&mut self, error: &str) {
        let text = t!("login.link_error", error = quote_untrusted(error, 120)).into_owned();
        match self.pending.as_mut() {
            Some(p) => p.error = Some(text),
            None => self.error = Some(text),
        }
    }

    /// Marks the pending login as having received its one SAML link with a
    /// server-generated token.
    pub fn claim_saml(&mut self, credential_token: &str) {
        if let Some(p) = self.pending.as_mut() {
            p.login.saml_claim = Some(credential_token.to_owned());
        }
    }

    pub fn saml_claim_matches(&self, credential_token: &str, now: Instant) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            !p.login.is_expired(now) && p.login.saml_claim.as_deref() == Some(credential_token)
        })
    }

    /// Marks the pending login as having received its one Token link.
    pub fn claim_token(&mut self, credentials: &Credentials) {
        if let Some(p) = self.pending.as_mut() {
            p.login.token_claim = Some(credentials.clone());
        }
    }

    /// Releases the claim of `credentials` (its lookup failed transiently), so
    /// the same link can be delivered again.
    pub fn release_token(&mut self, credentials: &Credentials) {
        if let Some(p) = self.pending.as_mut()
            && p.login.token_claim.as_ref() == Some(credentials)
        {
            p.login.token_claim = None;
        }
    }

    /// The live pending login that accepted exactly `credentials`: from a
    /// Token link, or from the login with a link-supplied SAML token.
    pub fn token_claim_matches(&self, credentials: &Credentials, now: Instant) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            let saml_link =
                matches!(p.login.kind, PendingKind::Saml { .. }) && p.login.saml_claim.is_some();
            (p.login.kind.expects_token() || saml_link)
                && !p.login.is_expired(now)
                && p.login.token_claim.as_ref() == Some(credentials)
        })
    }

    fn run_login(&mut self, request: LoginRequest, code: Option<TwoFactorCode>) -> Task<Message> {
        self.run_login_from(request, code, false)
    }

    /// `link`: the request comes from an unauthenticated link; its result is
    /// reported as [`Message::LinkLoginDone`].
    fn run_login_from(
        &mut self,
        request: LoginRequest,
        code: Option<TwoFactorCode>,
        link: bool,
    ) -> Task<Message> {
        self.busy = true;
        self.error = None;
        let rest = self.rest.clone();
        if let Some(tf) = self.two_factor.as_mut() {
            tf.busy = true;
            tf.error = None;
        }
        // Remember the request for a possible 2FA round.
        if link {
            self.link_request = Some(request.clone());
        } else {
            self.last_request = Some(request.clone());
        }
        Task::perform(
            async move {
                rest.login(&request, code.as_ref())
                    .await
                    .map_err(Failure::from)
            },
            move |r| {
                if link {
                    Message::LinkLoginDone(r)
                } else {
                    Message::LoginDone(r)
                }
            },
        )
    }

    /// A login call finished; `link`: it was started by an unauthenticated
    /// link, so success is [`Out::Unconfirmed`] and the pending login stays.
    fn login_done(&mut self, result: Res<LoginResult>, link: bool) -> (Task<Message>, Out) {
        self.busy = false;
        match result {
            Ok(r) => {
                self.two_factor = None;
                if link {
                    return (Task::none(), Out::Unconfirmed(Box::new(r)));
                }
                self.cancel_pending();
                self.password.clear();
                (Task::none(), Out::LoggedIn(Box::new(r), TokenKind::Login))
            }
            Err(f) => match f.error() {
                RcError::TwoFactorRequired(ch) => {
                    let request = if link {
                        self.link_request.clone()
                    } else {
                        self.cancel_pending();
                        self.last_request.clone()
                    };
                    let request = request.unwrap_or_else(|| self.form_request());
                    // The policy turned the form off while the password was
                    // on its way: no code dialog that could not be sent, and
                    // no error line (the notice line already says why).
                    if !link && !self.rules.password && sends_password(&request) {
                        self.two_factor = None;
                        self.last_request = None;
                        self.error = None;
                        return (Task::none(), Out::None);
                    }
                    self.two_factor = Some(TwoFactor {
                        request,
                        method: match &ch.method {
                            TwoFactorMethod::Unknown => TwoFactorMethod::Totp,
                            m => m.clone(),
                        },
                        email_or_username: ch.email_or_username.clone().or_else(|| {
                            Some(self.user.trim().to_owned()).filter(|u| !u.is_empty())
                        }),
                        code: String::new(),
                        busy: false,
                        error: None,
                        resent: false,
                        unconfirmed: link,
                    });
                    (iced::widget::operation::focus(CODE_ID), Out::None)
                }
                RcError::TwoFactorInvalid(_) if self.two_factor.is_some() => {
                    if let Some(tf) = self.two_factor.as_mut() {
                        tf.busy = false;
                        tf.error = Some(f.user_text());
                    }
                    (iced::widget::operation::focus(CODE_ID), Out::None)
                }
                _ => {
                    if let Some(tf) = self.two_factor.as_mut() {
                        tf.busy = false;
                        tf.error = Some(f.user_text());
                        return (iced::widget::operation::focus(CODE_ID), Out::None);
                    } else if let Some(p) = self.pending.as_mut() {
                        p.poll = None;
                        p.error = Some(f.user_text());
                    } else {
                        self.error = Some(f.user_text());
                    }
                    (Task::none(), Out::None)
                }
            },
        }
    }

    fn form_request(&self) -> LoginRequest {
        let ldap = self.meta.as_ref().is_some_and(|m| m.ldap);
        if ldap {
            LoginRequest::Ldap(LdapLogin {
                username: self.user.trim().to_owned(),
                password: self.password.clone(),
            })
        } else {
            LoginRequest::Password {
                user: self.user.trim().to_owned(),
                password: self.password.clone(),
            }
        }
    }

    fn start_pending(&mut self, kind: PendingKind, label: String, url: String) -> Out {
        self.cancel_pending();
        self.error = None;
        self.pending = Some(Pending {
            login: PendingLogin::new(self.server_id.clone(), self.server.clone(), kind),
            label,
            url: url.clone(),
            paste: String::new(),
            poll: None,
            error: None,
        });
        Out::OpenUrl(url)
    }

    fn start_poll(&mut self, request: LoginRequest) -> Task<Message> {
        // A 2FA challenge from the poll must retry this request, not the form.
        self.last_request = Some(request.clone());
        let rest = self.rest.clone();
        let (task, handle) =
            Task::perform(poll_login(rest, request), Message::LoginDone).abortable();
        if let Some(p) = self.pending.as_mut() {
            p.poll = Some(handle);
        }
        task
    }

    pub fn update(&mut self, msg: Message) -> (Task<Message>, Out) {
        let none = (Task::none(), Out::None);
        match msg {
            Message::MetaLoaded(Ok((_, meta))) => {
                self.meta = Some(meta);
                self.meta_error = None;
            }
            Message::MetaLoaded(Err(e)) => self.meta_error = Some(e.user_text()),
            Message::RetryMeta => {
                self.meta_error = None;
                return (meta_task(self.rest.clone()), Out::None);
            }
            Message::User(s) => self.user = s,
            // The policy turned the form off: nothing takes or submits a
            // password.
            Message::Password(s) => {
                if self.rules.password {
                    self.password = s;
                }
            }
            Message::SubmitUser | Message::Submit if !self.rules.password => return none,
            Message::SubmitCode
                if !self.rules.password
                    && self
                        .two_factor
                        .as_ref()
                        .is_some_and(|tf| sends_password(&tf.request)) =>
            {
                return none;
            }
            // The token login is off: no entry, no check, no sign-in.
            Message::ShowToken(true)
            | Message::PatUserId(_)
            | Message::PatToken(_)
            | Message::SubmitPat
                if !self.rules.token =>
            {
                return none;
            }
            Message::PatChecked(..) if !self.rules.token => {
                self.busy = false;
                return none;
            }
            Message::SubmitUser if self.password.is_empty() && !self.user.trim().is_empty() => {
                return (iced::widget::operation::focus(PASSWORD_ID), Out::None);
            }
            Message::SubmitUser => return self.update(Message::Submit),
            Message::Submit => {
                if self.busy || self.user.trim().is_empty() || self.password.is_empty() {
                    return none;
                }
                self.two_factor = None;
                let req = self.form_request();
                return (self.run_login(req, None), Out::None);
            }
            Message::LoginDone(result) => return self.login_done(result, false),
            Message::LinkLoginDone(result) => return self.login_done(result, true),
            Message::Code(s) => {
                if let Some(tf) = self.two_factor.as_mut() {
                    tf.code = s;
                }
            }
            Message::SubmitCode => {
                let Some(tf) = self.two_factor.as_ref() else {
                    return none;
                };
                if tf.busy || tf.code.trim().is_empty() {
                    return none;
                }
                let code = match &tf.method {
                    TwoFactorMethod::Password => TwoFactorCode::password(&tf.code),
                    m => TwoFactorCode::new(&tf.code, m.clone()),
                };
                let (req, link) = (tf.request.clone(), tf.unconfirmed);
                return (self.run_login_from(req, Some(code), link), Out::None);
            }
            Message::ResendCode => {
                let Some(who) = self
                    .two_factor
                    .as_ref()
                    .and_then(|tf| tf.email_or_username.clone())
                else {
                    return none;
                };
                let rest = self.rest.clone();
                return (
                    Task::perform(
                        async move { rest.send_email_2fa_code(&who).await.map_err(Failure::from) },
                        Message::CodeResent,
                    ),
                    Out::None,
                );
            }
            Message::CodeResent(r) => {
                if let Some(tf) = self.two_factor.as_mut() {
                    match r {
                        Ok(()) => tf.resent = true,
                        Err(e) => tf.error = Some(e.user_text()),
                    }
                    // The resend button took the focus from the code field.
                    return (iced::widget::operation::focus(CODE_ID), Out::None);
                }
            }
            Message::CancelCode => {
                // The pending login of a link-supplied token has nothing
                // left to wait for.
                if self.two_factor.as_ref().is_some_and(|tf| tf.unconfirmed) {
                    self.cancel_pending();
                }
                self.two_factor = None;
                self.busy = false;
            }
            Message::Service(i) => {
                let Some(service) = self.meta.as_ref().and_then(|m| m.services.get(i)).cloned()
                else {
                    return none;
                };
                return self.start_service(&service);
            }
            Message::BrowserLogin => {
                let url = browser_login_url(&self.server);
                let out =
                    self.start_pending(PendingKind::Browser, t!("login.browser").into_owned(), url);
                return (Task::none(), out);
            }
            Message::OpenAgain => {
                if let Some(p) = &self.pending {
                    return (Task::none(), Out::OpenUrl(p.url.clone()));
                }
            }
            Message::CancelPending => self.cancel_pending(),
            Message::Paste(s) => {
                if let Some(p) = self.pending.as_mut() {
                    p.paste = s;
                    p.error = None;
                }
            }
            Message::SubmitPaste => {
                if let Some(p) = self.pending.as_mut() {
                    let link = p.paste.trim().to_owned();
                    if rocketchat::auth::parse_deep_link(&link).is_none() {
                        p.error = Some(t!("login.paste_invalid").into_owned());
                        return none;
                    }
                    p.paste.clear();
                    return (Task::none(), Out::DeepLink(link));
                }
            }
            Message::RegisterHandler => return (Task::none(), Out::RegisterHandler),
            Message::ShowToken(on) => {
                self.token_mode = on;
                self.error = None;
            }
            Message::PatUserId(s) => self.pat_user_id = s,
            Message::PatToken(s) => self.pat_token = s,
            Message::SubmitPat => {
                let creds = Credentials::new(self.pat_user_id.trim(), self.pat_token.trim());
                if creds.user_id.is_empty() || creds.auth_token.is_empty() || self.busy {
                    return none;
                }
                self.busy = true;
                self.error = None;
                let tls = self.tls.clone();
                let server = self.server.clone();
                let c = creds.clone();
                return (
                    Task::perform(
                        async move {
                            let rest = RestClient::new(server, &tls)?;
                            rest.set_credentials(Some(c));
                            rest.me().await
                        },
                        move |r| Message::PatChecked(creds.clone(), r.map_err(Failure::from)),
                    ),
                    Out::None,
                );
            }
            Message::PatChecked(creds, r) => {
                self.busy = false;
                match r {
                    Ok(me) if me.id == creds.user_id => {
                        self.pat_token.clear();
                        return (
                            Task::none(),
                            Out::LoggedIn(
                                Box::new(LoginResult {
                                    user_id: creds.user_id,
                                    auth_token: creds.auth_token,
                                    me: Some(me),
                                }),
                                TokenKind::Pat,
                            ),
                        );
                    }
                    Ok(_) => self.error = Some(t!("login.pat_mismatch").into_owned()),
                    Err(e) => self.error = Some(e.user_text()),
                }
            }
        }
        none
    }

    fn start_service(&mut self, service: &LoginServiceConfig) -> (Task<Message>, Out) {
        let label = service.button_label();
        let modern = self.meta.as_ref().is_some_and(|m| m.modern_oauth);
        match service.kind() {
            LoginServiceKind::Saml => {
                let provider = service
                    .client_config
                    .as_ref()
                    .and_then(|c| c.provider.clone())
                    .unwrap_or_else(|| service.service_name().to_owned());
                let bl = saml_login(&self.server, &provider);
                let token = bl.credential_token.clone().unwrap_or_default();
                let out = self.start_pending(
                    PendingKind::Saml {
                        credential_token: token.clone(),
                    },
                    label,
                    bl.url,
                );
                let poll = self.start_poll(LoginRequest::Saml {
                    credential_token: token,
                });
                (poll, out)
            }
            LoginServiceKind::Cas => {
                let Some(cas_url) = service
                    .login_url
                    .clone()
                    .or_else(|| self.meta.as_ref().and_then(|m| m.cas_login_url.clone()))
                else {
                    self.error = Some(t!("login.service_unavailable").into_owned());
                    return (Task::none(), Out::None);
                };
                let bl = cas_login(&self.server, &cas_url);
                let token = bl.credential_token.clone().unwrap_or_default();
                let out = self.start_pending(
                    PendingKind::Cas {
                        credential_token: token.clone(),
                    },
                    label,
                    bl.url,
                );
                let poll = self.start_poll(LoginRequest::Cas {
                    credential_token: token,
                });
                (poll, out)
            }
            LoginServiceKind::OAuth | LoginServiceKind::CustomOAuth if modern => {
                let url = modern_oauth_url(&self.server, service.service_name());
                (
                    Task::none(),
                    self.start_pending(PendingKind::ModernOAuth, label, url),
                )
            }
            LoginServiceKind::OAuth | LoginServiceKind::CustomOAuth => {
                let opts = OAuthOptions {
                    gitlab_url: self.meta.as_ref().and_then(|m| m.gitlab_url.clone()),
                };
                match oauth_login(service, &self.server, &opts) {
                    Ok(bl) => {
                        let kind = PendingKind::OAuth {
                            credential_token: bl.credential_token.clone().unwrap_or_default(),
                        };
                        (Task::none(), self.start_pending(kind, label, bl.url))
                    }
                    Err(e) => {
                        self.error = Some(crate::error::user_text(&e));
                        (Task::none(), Out::None)
                    }
                }
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let title = match &self.meta {
            Some(m) => t!("login.title", site = m.site_name).into_owned(),
            None => t!("login.title_generic").into_owned(),
        };
        let mut body = column![].spacing(SPACE_4);
        if let Some(reason) = &self.reason {
            body = body.push(banner(BannerKind::Warning, reason.clone(), None));
        }
        if self.policy_hides_something() {
            let line = self.rules.notice();
            body = body.push(
                row![
                    rustket_ui::icon::sized(Icon::Lock, TEXT_CAPTION).style(style::text_secondary),
                    text(line)
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center),
            );
        }
        if let Some(p) = &self.pending {
            body = body.push(self.pending_view(p));
        } else if let Some(err) = &self.meta_error {
            body = body.push(banner(
                BannerKind::Error,
                err.clone(),
                Some((t!("login.retry").into_owned(), Message::RetryMeta)),
            ));
        } else if self.meta.is_none() {
            body = body.push(
                container(spinner(24.0))
                    .center_x(Length::Fill)
                    .padding(SPACE_4),
            );
        } else if self.token_mode && self.rules.token {
            body = body.push(self.token_view());
        } else {
            body = body.push(self.form_view());
        }
        super::card(title, Some(self.server.host()), body.into())
    }

    fn form_view(&self) -> Element<'_, Message> {
        let Some(meta) = &self.meta else {
            return column![].into();
        };
        let mut col = column![].spacing(SPACE_3);
        // The policy hides the form whatever the server's setting says.
        let show_form = meta.show_form && self.rules.password;
        if show_form {
            let (user_label, user_ph) = if meta.ldap {
                (t!("login.ldap_user"), t!("login.ldap_user_placeholder"))
            } else {
                (t!("login.user"), t!("login.user_placeholder"))
            };
            let can_submit = !self.user.trim().is_empty() && !self.password.is_empty();
            col = col
                .push(
                    text_field(user_label, &self.user, Message::User)
                        .placeholder(user_ph)
                        .id(USER_ID)
                        .on_submit(Message::SubmitUser),
                )
                .push(
                    text_field(t!("login.password"), &self.password, Message::Password)
                        .password(true)
                        .id(PASSWORD_ID)
                        .on_submit(Message::Submit),
                )
                .push(
                    primary_button(t!("login.sign_in"), can_submit.then_some(Message::Submit))
                        .loading(self.busy && self.two_factor.is_none())
                        .width(Length::Fill),
                );
        }
        if !meta.services.is_empty() {
            if show_form {
                col = col.push(or_divider());
            }
            for (i, s) in meta.services.iter().enumerate() {
                col = col.push(service_button(s, Message::Service(i)));
            }
        }
        if show_form && meta.services.is_empty() {
            col = col.push(or_divider());
        }
        // Always offered: with neither the form nor a service it is the
        // way in.
        col = col.push(
            secondary_button(t!("login.browser"), Some(Message::BrowserLogin))
                .icon(Icon::Globe)
                .width(Length::Fill),
        );
        if self.rules.token {
            col = col.push(
                ghost_button(t!("login.use_token"), Some(Message::ShowToken(true)))
                    .icon(Icon::Key)
                    .width(Length::Fill),
            );
        }
        // A failed sign-in must not shift the fields (and move the focus).
        above(self.error_banner(), SPACE_3, col).into()
    }

    fn error_banner(&self) -> Option<Element<'_, Message>> {
        self.error
            .as_ref()
            .map(|err| banner(BannerKind::Error, err.clone(), None))
    }

    fn token_view(&self) -> Element<'_, Message> {
        let can = !self.pat_user_id.trim().is_empty() && !self.pat_token.trim().is_empty();
        let help = text(t!("login.token_help"))
            .size(TEXT_CAPTION)
            .style(style::text_secondary);
        let fields = column![]
            .spacing(SPACE_3)
            .push(
                text_field(
                    t!("login.token_user_id"),
                    &self.pat_user_id,
                    Message::PatUserId,
                )
                .on_submit(Message::SubmitPat),
            )
            .push(
                text_field(t!("login.token"), &self.pat_token, Message::PatToken)
                    .password(true)
                    .on_submit(Message::SubmitPat),
            )
            .push(
                primary_button(t!("login.sign_in"), can.then_some(Message::SubmitPat))
                    .loading(self.busy)
                    .width(Length::Fill),
            )
            .push(
                ghost_button(t!("login.back"), Some(Message::ShowToken(false))).width(Length::Fill),
            );
        column![help, above(self.error_banner(), SPACE_3, fields)]
            .spacing(SPACE_3)
            .into()
    }

    fn pending_view<'a>(&'a self, p: &'a Pending) -> Element<'a, Message> {
        let polling = matches!(
            p.login.kind,
            PendingKind::Saml { .. } | PendingKind::Cas { .. }
        );
        let col = column![
            row![
                spinner(18.0),
                text(t!("login.pending_title", service = p.label))
                    .size(TEXT_BODY + 1.0)
                    .font(font_medium())
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
            text(if polling {
                t!("login.pending_polling")
            } else {
                t!("login.pending_help")
            })
            .size(TEXT_BODY)
            .style(style::text_secondary),
        ]
        .spacing(SPACE_3);
        let mut rest = column![].spacing(SPACE_3).push(
            row![
                secondary_button(t!("login.open_again"), Some(Message::OpenAgain))
                    .icon(Icon::ExternalLink)
                    .width(Length::Fill),
                ghost_button(t!("login.cancel"), Some(Message::CancelPending)),
            ]
            .spacing(SPACE_2),
        );
        // Without the modern flow, Rocket.Chat 8.9.1+ signs the browser in
        // with a SAML token it generated and never hands it to the app; the
        // browser session can still be passed on through "sign in with browser".
        let saml_handoff = self.meta.as_ref().is_some_and(|m| m.modern_oauth);
        if matches!(p.login.kind, PendingKind::Saml { .. }) && !saml_handoff {
            rest = rest.push(banner(
                BannerKind::Info,
                t!("login.saml_browser_hint"),
                Some((
                    t!("login.saml_browser_continue").into_owned(),
                    Message::BrowserLogin,
                )),
            ));
        }
        if !polling {
            rest = rest.push(rule::horizontal(1)).push(
                text_field(t!("login.paste_label"), &p.paste, Message::Paste)
                    .placeholder("rocketchat://auth?…")
                    .help(t!("login.paste_help"))
                    .on_submit(Message::SubmitPaste),
            );
            rest = rest.push(
                secondary_button(
                    t!("login.paste_submit"),
                    (!p.paste.trim().is_empty()).then_some(Message::SubmitPaste),
                )
                .width(Length::Fill),
            );
            if !self.handler_registered && !crate::url_handler::MANAGED_BY_BUNDLE {
                rest = rest.push(banner(
                    BannerKind::Info,
                    t!("login.handler_hint"),
                    Some((
                        t!("login.register_handler").into_owned(),
                        Message::RegisterHandler,
                    )),
                ));
            }
        }
        let error = p
            .error
            .as_ref()
            .map(|err| banner(BannerKind::Error, err.clone(), None));
        col.push(above(error, SPACE_3, rest)).into()
    }

    /// The 2FA dialog.
    pub fn overlay(&self) -> Option<Element<'_, Message>> {
        let tf = self.two_factor.as_ref()?;
        let (help, label) = match tf.method {
            TwoFactorMethod::Email => (
                t!(
                    "login.tfa_email_help",
                    who = tf.email_or_username.clone().unwrap_or_default()
                ),
                t!("login.tfa_code"),
            ),
            TwoFactorMethod::Password => (t!("login.tfa_password_help"), t!("login.password")),
            _ => (t!("login.tfa_totp_help"), t!("login.tfa_code")),
        };
        let mut body =
            column![text(help).size(TEXT_BODY).style(style::text_secondary)].spacing(SPACE_3);
        body = body.push(
            text_field(label, &tf.code, Message::Code)
                .password(tf.method == TwoFactorMethod::Password)
                .placeholder(if tf.method == TwoFactorMethod::Password {
                    String::new()
                } else {
                    "123456".to_owned()
                })
                .id(CODE_ID)
                .error(tf.error.clone())
                .on_submit(Message::SubmitCode),
        );
        if tf.method == TwoFactorMethod::Email {
            body = body.push(
                row![
                    ghost_button(t!("login.tfa_resend"), Some(Message::ResendCode))
                        .icon(Icon::RefreshCw),
                    text(if tf.resent {
                        t!("login.tfa_resent").into_owned()
                    } else {
                        String::new()
                    })
                    .size(TEXT_CAPTION)
                    .style(style::text_success),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            );
        }
        Some(dialog(
            t!("login.tfa_title"),
            body,
            vec![
                secondary_button(t!("login.cancel"), Some(Message::CancelCode)).into(),
                primary_button(
                    t!("login.tfa_verify"),
                    (!tf.code.trim().is_empty()).then_some(Message::SubmitCode),
                )
                .loading(tf.busy)
                .into(),
            ],
            Some(Message::CancelCode),
        ))
    }
}

fn meta_task(rest: RestClient) -> Task<Message> {
    Task::perform(
        async move { fetch_meta(&rest).await.map_err(Failure::from) },
        Message::MetaLoaded,
    )
}

/// Polls a SAML/CAS login until it succeeds, needs 2FA, or [`POLL_LIMIT`] passes.
async fn poll_login(rest: RestClient, request: LoginRequest) -> Res<LoginResult> {
    let started = Instant::now();
    let mut delay = Duration::from_secs(2);
    loop {
        tokio::time::sleep(delay).await;
        match rest.login(&request, None).await {
            Ok(r) => return Ok(r),
            Err(e @ RcError::TwoFactorRequired(_)) => return Err(e.into()),
            Err(e) if e.is_certificate_error() => return Err(e.into()),
            Err(e) => tracing::debug!(error = %e, "login poll: not finished yet"),
        }
        if started.elapsed() >= POLL_LIMIT {
            return Err(RcError::Timeout("browser login").into());
        }
        delay = (delay.mul_f32(1.5)).min(Duration::from_secs(10));
    }
}

fn or_divider<'a>() -> Element<'a, Message> {
    row![
        rule::horizontal(1),
        text(t!("login.or"))
            .size(TEXT_CAPTION)
            .style(style::text_hint),
        rule::horizontal(1),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// `#rgb` / `#rrggbb` → colour.
pub fn parse_color(s: &str) -> Option<Color> {
    let h = s.trim().strip_prefix('#')?;
    if !h.is_ascii() {
        return None;
    }
    let expand = |c: char| u8::from_str_radix(&format!("{c}{c}"), 16).ok();
    match h.len() {
        3 => {
            let mut it = h.chars();
            Some(Color::from_rgb8(
                expand(it.next()?)?,
                expand(it.next()?)?,
                expand(it.next()?)?,
            ))
        }
        6 => Some(Color::from_rgb8(
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        )),
        _ => None,
    }
}

fn service_button<'a>(s: &LoginServiceConfig, msg: Message) -> Element<'a, Message> {
    let bg = s.button_color.as_deref().and_then(parse_color);
    let fg = s.button_label_color.as_deref().and_then(parse_color);
    let label = t!("login.continue_with", service = s.button_label()).into_owned();
    button(
        container(text(label).size(TEXT_BODY).font(font_medium()))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fill)
    .height(36)
    .padding([0.0, SPACE_1])
    .style(move |theme: &Theme, status| {
        let t = tokens(theme);
        let base = bg.unwrap_or(t.surface_raised);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let bgc = if hovered {
            Color { a: 0.88, ..base }
        } else {
            base
        };
        button::Style {
            background: Some(Background::Color(bgc)),
            text_color: fg.unwrap_or(t.text_primary),
            border: Border {
                radius: radius_md().into(),
                width: if bg.is_some() { 0.0 } else { 1.0 },
                color: t.border_strong,
            },
            ..button::Style::default()
        }
    })
    .on_press(msg)
    .into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_color("#FFFFFF"), Some(Color::WHITE));
        assert_eq!(parse_color("#000"), Some(Color::BLACK));
        assert_eq!(parse_color("1d74f5"), None);
        assert_eq!(parse_color("#12"), None);
    }

    #[test]
    fn modern_oauth_on_9_0_regardless_of_setting() {
        let info = |v: &str| ServerInfo {
            version: Some(v.into()),
            ..Default::default()
        };
        assert!(modern_oauth(&info("9.0"), None));
        assert!(modern_oauth(&info("9.0.0-rc.8"), Some(false)));
        assert!(!modern_oauth(&info("8.9.1"), None));
        assert!(!modern_oauth(&info("8.9.1"), Some(false)));
        assert!(modern_oauth(&info("8.9.1"), Some(true)));
        assert!(!modern_oauth(&ServerInfo::default(), None));
    }

    /// Review #14: no panic on multibyte input.
    #[test]
    fn colour_with_multibyte_chars() {
        assert_eq!(parse_color("#aééa"), None);
        assert_eq!(parse_color("#ééé"), None);
    }

    fn login_with(services: Vec<LoginServiceConfig>) -> Login {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let tls = rocketchat::TlsConfig::new().build().unwrap();
        let meta = LoginMeta {
            site_name: "x".into(),
            show_form: true,
            services,
            ..Default::default()
        };
        Login::new(ServerId("s".into()), server, tls, Some(meta), None)
            .unwrap()
            .0
    }

    /// A 2FA challenge from a SAML poll retries the SAML request.
    #[tokio::test]
    async fn saml_poll_two_factor_uses_saml_request() {
        let mut l = login_with(vec![LoginServiceConfig {
            service: Some("saml".into()),
            ..Default::default()
        }]);
        let (_poll, out) = l.update(Message::Service(0));
        assert!(matches!(out, Out::OpenUrl(_)));
        let challenge = rocketchat::TwoFactorChallenge::from_details(Some(
            &serde_json::json!({"method": "totp"}),
        ));
        let _ = l.update(Message::LoginDone(Err(Failure::from(
            RcError::TwoFactorRequired(challenge),
        ))));
        let tf = l.two_factor.as_ref().expect("2fa dialog");
        assert!(matches!(tf.request, LoginRequest::Saml { .. }));
    }

    fn totp_required() -> Res<LoginResult> {
        let challenge = rocketchat::TwoFactorChallenge::from_details(Some(
            &serde_json::json!({"method": "totp"}),
        ));
        Err(Failure::from(RcError::TwoFactorRequired(challenge)))
    }

    fn login_result(uid: &str, token: &str) -> LoginResult {
        LoginResult {
            user_id: uid.into(),
            auth_token: token.into(),
            me: None,
        }
    }

    /// F13: a login with a link-supplied SAML token never ends in
    /// `Out::LoggedIn`, also after its 2FA round, and keeps the pending login
    /// its confirmation is bound to.
    #[tokio::test]
    async fn link_login_stays_unconfirmed_after_two_factor() {
        let mut l = login_with(vec![LoginServiceConfig {
            service: Some("saml".into()),
            ..Default::default()
        }]);
        let _ = l.update(Message::Service(0));
        l.claim_saml("ATTACKER");
        let _ = l.complete_unconfirmed(LoginRequest::Saml {
            credential_token: "ATTACKER".into(),
        });
        assert!(l.pending.as_ref().is_some_and(|p| p.poll.is_none()));
        let (_, out) = l.update(Message::LinkLoginDone(totp_required()));
        assert!(matches!(out, Out::None));
        let tf = l.two_factor.as_ref().expect("2fa dialog");
        assert!(tf.unconfirmed);
        let LoginRequest::Saml { credential_token } = &tf.request else {
            panic!("the 2FA round retries the SAML request");
        };
        assert_eq!(credential_token, "ATTACKER");
        assert!(l.pending.is_some(), "the pending login stays");
        let _ = l.update(Message::Code("123456".into()));
        let _ = l.update(Message::SubmitCode);
        let (_, out) = l.update(Message::LinkLoginDone(Ok(login_result("EVE", "T"))));
        assert!(matches!(out, Out::Unconfirmed(_)), "{out:?}");
        assert!(l.two_factor.is_none());
        let creds = Credentials::new("EVE", "T");
        assert!(!l.token_claim_matches(&creds, Instant::now()));
        l.claim_token(&creds);
        assert!(l.token_claim_matches(&creds, Instant::now()));
        assert!(!l.token_claim_matches(&Credentials::new("EVE", "T2"), Instant::now()));
    }

    /// The 2FA round of a regular login is not affected.
    #[tokio::test]
    async fn regular_two_factor_logs_in() {
        let mut l = login_with(vec![]);
        l.user = "alice".into();
        l.password = "pw".into();
        let _ = l.update(Message::Submit);
        let _ = l.update(Message::LoginDone(totp_required()));
        assert!(l.two_factor.as_ref().is_some_and(|tf| !tf.unconfirmed));
        let (_, out) = l.update(Message::LoginDone(Ok(login_result("U", "T"))));
        assert!(matches!(out, Out::LoggedIn(_, TokenKind::Login)));
    }

    /// A SAML login without an accepted link holds no token claim.
    #[tokio::test]
    async fn saml_pending_without_link_matches_no_token() {
        let mut l = login_with(vec![LoginServiceConfig {
            service: Some("saml".into()),
            ..Default::default()
        }]);
        let _ = l.update(Message::Service(0));
        let creds = Credentials::new("EVE", "T");
        l.claim_token(&creds);
        assert!(!l.token_claim_matches(&creds, Instant::now()));
    }

    /// Cancelling the 2FA dialog of a link login ends its pending login.
    #[tokio::test]
    async fn cancelling_link_two_factor_cancels_pending() {
        let mut l = login_with(vec![LoginServiceConfig {
            service: Some("saml".into()),
            ..Default::default()
        }]);
        let _ = l.update(Message::Service(0));
        l.claim_saml("ATTACKER");
        let _ = l.complete_unconfirmed(LoginRequest::Saml {
            credential_token: "ATTACKER".into(),
        });
        let _ = l.update(Message::LinkLoginDone(totp_required()));
        let _ = l.update(Message::CancelCode);
        assert!(l.pending.is_none() && l.two_factor.is_none());
    }

    /// Review #6: an error link shows a quoted notice and keeps the login.
    #[tokio::test]
    async fn link_error_keeps_pending_login() {
        let mut l = login_with(vec![]);
        let _ = l.update(Message::BrowserLogin);
        l.show_link_error("Session expired.\u{202e} Visit https://evil.example\nnow");
        let p = l.pending.as_ref().expect("still pending");
        let e = p.error.clone().unwrap();
        assert!(e.starts_with("The browser reported"), "{e}");
        assert!(!e.contains('\u{202e}') && !e.contains('\n'), "{e:?}");
        assert!(e.contains('“'));
    }

    /// The error banner used to be inserted before the fields: a failed
    /// sign-in moved the password field's state (focus, cursor) to the user
    /// field and rebuilt the password field.
    #[test]
    fn failed_sign_in_keeps_the_fields_state() {
        use crate::tree_probe::{assert_kept, text_input_tag};
        use iced::advanced::widget::Tree;
        let mut l = login_with(vec![]);
        let mut tree = Tree::new(l.view().as_widget());
        let _ = l.update(Message::LoginDone(Err(Failure::from(RcError::Timeout(
            "x",
        )))));
        assert!(l.error.is_some());
        assert_kept(&mut tree, &l.view(), text_input_tag(), "error shown");
    }

    fn rules(password: bool, token: bool, organization: Option<&str>) -> SignInRules {
        SignInRules {
            password,
            token,
            organization: organization.map(str::to_owned),
        }
    }

    fn texts(l: &Login) -> Vec<String> {
        crate::tree_probe::texts(l.view())
    }

    fn has(texts: &[String], needle: &str) -> bool {
        texts.iter().any(|t| t == needle)
    }

    const NOTICE: &str = "Some sign-in methods are turned off by your organization";

    /// The policy's defaults change nothing: form, browser, token, no notice.
    #[test]
    fn default_rules_offer_every_method() {
        rust_i18n::set_locale("en");
        let l = login_with(vec![]);
        let t = texts(&l);
        for needle in [
            "Password",
            "Sign in",
            "Sign in with browser",
            "Use a personal access token",
        ] {
            assert!(has(&t, needle), "{needle}: {t:?}");
        }
        assert!(!has(&t, NOTICE));
    }

    /// `AllowPasswordLogin = false`: no form (LDAP included) whatever the
    /// server says, its messages are refused; the browser sign-in stays.
    #[test]
    fn password_off_hides_the_form_and_refuses_it() {
        rust_i18n::set_locale("en");
        for ldap in [false, true] {
            let mut l = login_with(vec![]);
            if let Some(m) = l.meta.as_mut() {
                m.ldap = ldap;
            }
            let _ = l.update(Message::User("alice".into()));
            let _ = l.update(Message::Password("secret".into()));
            l.set_rules(rules(false, true, None));
            assert!(l.password.is_empty(), "a typed password stays");
            let t = texts(&l);
            for gone in ["Password", "Sign in", "Username or email", "LDAP username"] {
                assert!(!has(&t, gone), "{gone}: {t:?}");
            }
            assert!(has(&t, "Sign in with browser"), "{t:?}");
            assert!(has(&t, "Use a personal access token"));
            assert!(has(&t, NOTICE), "{t:?}");
            let _ = l.update(Message::Password("secret".into()));
            assert!(l.password.is_empty(), "the field takes input");
            for m in [Message::Submit, Message::SubmitUser] {
                // Filled fields: only the policy's guard stops the sign-in.
                l.user = "alice".into();
                l.password = "secret".into();
                let (task, out) = l.update(m);
                assert_eq!(task.units(), 0);
                assert!(matches!(out, Out::None));
                assert!(!l.busy);
            }
        }
    }

    /// A code challenge for a password sent before the policy turned the
    /// form off opens no dialog that could not be submitted.
    #[test]
    fn password_off_drops_a_late_code_challenge() {
        rust_i18n::set_locale("en");
        let mut l = login_with(vec![]);
        let _ = l.update(Message::User("carol".into()));
        let _ = l.update(Message::Password("pw".into()));
        let _ = l.update(Message::Submit);
        assert!(l.busy);
        l.set_rules(rules(false, true, None));
        let (task, out) = l.update(two_factor_challenge());
        assert_eq!(task.units(), 0);
        assert!(matches!(out, Out::None));
        assert!(l.two_factor.is_none() && !l.busy);
        // The notice line says it once; the error line stays empty.
        assert_eq!(l.error, None);
        let t = texts(&l);
        assert_eq!(t.iter().filter(|t| *t == NOTICE).count(), 1, "{t:?}");
    }

    /// The notice names only what the policy removed: a password key alone
    /// on a server whose own setting hides the form shows nothing.
    #[test]
    fn notice_only_when_something_is_removed() {
        rust_i18n::set_locale("en");
        for (show_form, password, token, notice) in [
            (true, false, true, true),
            (false, false, true, false),
            (false, true, false, true),
            (false, false, false, true),
            (true, true, true, false),
        ] {
            let mut l = login_with(vec![]);
            if let Some(m) = l.meta.as_mut() {
                m.show_form = show_form;
            }
            l.set_rules(rules(password, token, None));
            assert_eq!(
                has(&texts(&l), NOTICE),
                notice,
                "{show_form} {password} {token}"
            );
        }
    }

    /// A server without a single sign-on service and without its own form
    /// still offers the browser sign-in when the policy hides the form.
    #[test]
    fn browser_sign_in_stays_without_form_or_services() {
        rust_i18n::set_locale("en");
        for show_form in [true, false] {
            let mut l = login_with(vec![]);
            if let Some(m) = l.meta.as_mut() {
                m.show_form = show_form;
            }
            l.set_rules(rules(false, false, None));
            let t = texts(&l);
            assert!(has(&t, "Sign in with browser"), "{show_form}: {t:?}");
            assert!(!has(&t, "Use a personal access token"));
            let (_task, out) = l.update(Message::BrowserLogin);
            assert!(matches!(out, Out::OpenUrl(_)));
        }
    }

    /// `AllowTokenLogin = false`: no token entry, its messages refused, an
    /// open token view closes at once.
    #[test]
    fn token_off_hides_the_entry_and_refuses_it() {
        rust_i18n::set_locale("en");
        let mut l = login_with(vec![]);
        let _ = l.update(Message::ShowToken(true));
        let _ = l.update(Message::PatToken("tok".into()));
        assert!(l.token_mode);
        l.set_rules(rules(true, false, Some("Acme")));
        assert!(!l.token_mode && l.pat_token.is_empty());
        let t = texts(&l);
        assert!(!has(&t, "Use a personal access token"), "{t:?}");
        assert!(has(&t, "Password"));
        assert!(
            has(&t, "Some sign-in methods are turned off by Acme"),
            "{t:?}"
        );
        let _ = l.update(Message::ShowToken(true));
        assert!(!l.token_mode);
        let _ = l.update(Message::PatUserId("U1".into()));
        let _ = l.update(Message::PatToken("tok".into()));
        assert!(l.pat_user_id.is_empty() && l.pat_token.is_empty());
        l.pat_user_id = "U1".into();
        l.pat_token = "tok".into();
        let (task, _) = l.update(Message::SubmitPat);
        assert_eq!(task.units(), 0);
        assert!(!l.busy);
        // A check sent before the change does not sign in.
        let me = User {
            id: "U1".into(),
            ..Default::default()
        };
        let (_, out) = l.update(Message::PatChecked(Credentials::new("U1", "tok"), Ok(me)));
        assert!(matches!(out, Out::None));
    }

    /// The code dialog of a password sign-in closes when the policy turns
    /// the form off, and a code for it is refused; a SAML sign-in's stays.
    #[test]
    fn password_off_closes_the_code_dialog_of_a_password_sign_in() {
        let mut l = login_with(vec![]);
        let _ = l.update(Message::User("carol".into()));
        let _ = l.update(Message::Password("pw".into()));
        let _ = l.update(Message::Submit);
        let _ = l.update(two_factor_challenge());
        assert!(l.two_factor.is_some());
        l.set_rules(rules(false, true, None));
        assert!(l.two_factor.is_none());

        let mut l = login_with(vec![]);
        let _ = l.update(Message::User("carol".into()));
        let _ = l.update(Message::Password("pw".into()));
        let _ = l.update(Message::Submit);
        let _ = l.update(two_factor_challenge());
        // The dialog survives a direct rules change only through a SAML
        // request; force the case where it is still open.
        l.rules = rules(false, true, None);
        let _ = l.update(Message::Code("123456".into()));
        let (task, _) = l.update(Message::SubmitCode);
        assert_eq!(task.units(), 0, "a code for a password sign-in");

        let mut l = login_with(vec![LoginServiceConfig {
            service: Some("saml".into()),
            ..Default::default()
        }]);
        let _ = l.update(Message::Service(0));
        let _ = l.update(two_factor_challenge());
        l.set_rules(rules(false, true, None));
        assert!(l.two_factor.is_some(), "a SAML sign-in's code dialog");
    }

    fn two_factor_challenge() -> Message {
        let ch = rocketchat::TwoFactorChallenge::from_details(Some(
            &serde_json::json!({"method": "email", "emailOrUsername": "carol"}),
        ));
        Message::LoginDone(Err(Failure::from(RcError::TwoFactorRequired(ch))))
    }

    /// The 2FA dialog focuses its code field (the modal swallows keys that
    /// reach no input), also after a wrong code or a resend.
    #[test]
    fn two_factor_dialog_focuses_the_code_field() {
        use crate::tree_probe::focus_after;
        let code = Some(iced::advanced::widget::Id::new(CODE_ID));
        let mut l = login_with(vec![]);
        l.user = "carol".into();
        let (task, _) = l.update(two_factor_challenge());
        assert_eq!(
            focus_after(l.overlay().unwrap(), None, task),
            std::slice::from_ref(&code)
        );
        let ch = rocketchat::TwoFactorChallenge::from_details(None);
        let (task, _) = l.update(Message::LoginDone(Err(Failure::from(
            RcError::TwoFactorInvalid(ch),
        ))));
        assert_eq!(
            focus_after(l.overlay().unwrap(), None, task),
            std::slice::from_ref(&code)
        );
        let (task, _) = l.update(Message::CodeResent(Ok(())));
        assert_eq!(focus_after(l.overlay().unwrap(), None, task), [code]);
    }

    /// Enter in the user field moves to an empty password field; with a
    /// password it signs in.
    #[test]
    fn enter_in_user_field_moves_to_password() {
        use crate::tree_probe::focus_after;
        let mut l = login_with(vec![]);
        l.user = "alice".into();
        let (task, _) = l.update(Message::SubmitUser);
        assert_eq!(
            focus_after(l.view(), Some(USER_ID), task),
            [Some(iced::advanced::widget::Id::new(PASSWORD_ID))]
        );
        assert!(!l.busy);
        l.password = "pw".into();
        let _ = l.update(Message::SubmitUser);
        assert!(l.busy, "Enter with a password signs in");
    }

    /// The app answers an unhandled Tab with `focus_cycle`: user -> password,
    /// and Tab from the last field wraps instead of dropping focus.
    #[test]
    fn tab_moves_from_user_to_password() {
        use crate::tree_probe::focus_after;
        use iced::advanced::widget::Id;
        use rustket_ui::widget::focus_cycle;
        let l = login_with(vec![]);
        assert_eq!(
            focus_after(l.view(), Some(USER_ID), focus_cycle(true)),
            [Some(Id::new(PASSWORD_ID))]
        );
        let after_password = focus_after(l.view(), Some(PASSWORD_ID), focus_cycle(true));
        assert_eq!(after_password.len(), 1, "focus kept: {after_password:?}");
        assert_ne!(after_password, [Some(Id::new(PASSWORD_ID))]);
        assert_eq!(
            focus_after(l.view(), Some(USER_ID), focus_cycle(false)).len(),
            1,
            "Shift+Tab from the first field wraps"
        );
    }

    #[test]
    fn debug_redacts_secrets() {
        for m in [
            Message::Password("hunter2".into()),
            Message::PatToken("PAT-SECRET".into()),
            Message::Code("123456".into()),
            Message::Paste("rocketchat://auth?host=x&userId=u&token=PASTED-SECRET".into()),
            Message::PatChecked(
                Credentials::new("u", "PAT-SECRET"),
                Err(Failure::from(RcError::Timeout("x"))),
            ),
        ] {
            let s = format!("{m:?}");
            for secret in ["hunter2", "PAT-SECRET", "123456", "PASTED-SECRET"] {
                assert!(!s.contains(secret), "{s}");
            }
        }
    }

    #[test]
    fn untrusted_quoting() {
        assert_eq!(quote_untrusted("ok", 10), "“ok”");
        assert_eq!(quote_untrusted("abcdef", 3), "“abc…”");
        assert_eq!(quote_untrusted("a\nb", 10), "“a b”");
    }
}
