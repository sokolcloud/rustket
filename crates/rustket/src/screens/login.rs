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
    FONT_MEDIUM, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, tokens,
};
use rustket_ui::widget::{
    BannerKind, above, banner, dialog, ghost_button, primary_button, secondary_button, spinner,
    text_field,
};

use crate::deeplink::{PendingKind, PendingLogin};
use crate::error::{Failure, Res};
use crate::session::tls_for;
use rustket_core::config::TokenKind;

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
        modern_oauth: get("Accounts_OAuth_Use_Modern_Flow")
            .and_then(|s| s.as_bool())
            .unwrap_or(false),
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
    last_request: Option<LoginRequest>,
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
                last_request: None,
            },
            task,
        ))
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

    /// The live pending login that accepted exactly `credentials`.
    pub fn token_claim_matches(&self, credentials: &Credentials, now: Instant) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            p.login.kind.expects_token()
                && !p.login.is_expired(now)
                && p.login.token_claim.as_ref() == Some(credentials)
        })
    }

    fn run_login(&mut self, request: LoginRequest, code: Option<TwoFactorCode>) -> Task<Message> {
        self.busy = true;
        self.error = None;
        let rest = self.rest.clone();
        if let Some(tf) = self.two_factor.as_mut() {
            tf.busy = true;
            tf.error = None;
        }
        // Remember the request for a possible 2FA round.
        self.last_request = Some(request.clone());
        Task::perform(
            async move {
                rest.login(&request, code.as_ref())
                    .await
                    .map_err(Failure::from)
            },
            Message::LoginDone,
        )
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
            Message::Password(s) => self.password = s,
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
            Message::LoginDone(result) => {
                self.busy = false;
                match result {
                    Ok(r) => {
                        self.cancel_pending();
                        self.two_factor = None;
                        self.password.clear();
                        return (Task::none(), Out::LoggedIn(Box::new(r), TokenKind::Login));
                    }
                    Err(f) => match f.error() {
                        RcError::TwoFactorRequired(ch) => {
                            self.cancel_pending();
                            let request = self
                                .last_request
                                .clone()
                                .unwrap_or_else(|| self.form_request());
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
                            });
                            return (iced::widget::operation::focus(CODE_ID), Out::None);
                        }
                        RcError::TwoFactorInvalid(_) if self.two_factor.is_some() => {
                            if let Some(tf) = self.two_factor.as_mut() {
                                tf.busy = false;
                                tf.error = Some(f.user_text());
                            }
                            return (iced::widget::operation::focus(CODE_ID), Out::None);
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
                        }
                    },
                }
            }
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
                let req = tf.request.clone();
                return (self.run_login(req, Some(code)), Out::None);
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
        } else if self.token_mode {
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
        if meta.show_form {
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
            if meta.show_form {
                col = col.push(or_divider());
            }
            for (i, s) in meta.services.iter().enumerate() {
                col = col.push(service_button(s, Message::Service(i)));
            }
        }
        if meta.show_form && meta.services.is_empty() {
            col = col.push(or_divider());
        }
        col = col
            .push(
                secondary_button(t!("login.browser"), Some(Message::BrowserLogin))
                    .icon(Icon::Globe)
                    .width(Length::Fill),
            )
            .push(
                ghost_button(t!("login.use_token"), Some(Message::ShowToken(true)))
                    .icon(Icon::Key)
                    .width(Length::Fill),
            );
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
                    .font(FONT_MEDIUM)
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
                ghost_button(t!("login.cancel"), Some(Message::CancelPending)).width(Length::Fill),
            ]
            .spacing(SPACE_2),
        );
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
        container(text(label).size(TEXT_BODY).font(FONT_MEDIUM))
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
                radius: RADIUS_MD.into(),
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
