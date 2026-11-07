//! "Add a workspace": URL entry, server check, certificate prompt.
//! Strings: `locales/add_server.yml`.

use iced::widget::{column, container, text};
use iced::{Element, Length, Task};
use rocketchat::tls::Fingerprint;
use rocketchat::{CertificateInfo, ServerUrl};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{MONOSPACE_FONT, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION};
use rustket_ui::widget::{
    BannerKind, banner, dialog, ghost_button, primary_button, secondary_button, text_field,
};

use super::cert::CertView;
use super::login::{ProbeError, ServerProbe, probe};

pub const URL_INPUT_ID: &str = "add-server-url";

#[derive(Debug, Clone)]
pub enum Message {
    UrlChanged(String),
    Submit,
    Checked(u64, ServerUrl, Result<Box<ServerProbe>, ProbeError>),
    TrustCertificate,
    RejectCertificate,
    Cancel,
}

/// What the parent should do after an update.
#[derive(Debug, Clone)]
pub enum Out {
    None,
    Added {
        probe: Box<ServerProbe>,
        pins: Vec<(String, Fingerprint)>,
    },
    Cancel,
}

#[derive(Debug, Clone)]
pub struct CertPrompt {
    pub url: ServerUrl,
    pub host: String,
    pub chain: Vec<CertificateInfo>,
    pub views: Vec<CertView>,
}

#[derive(Debug, Default)]
pub struct AddServer {
    pub url: String,
    pub busy: bool,
    pub error: Option<String>,
    /// Pins the user approved during this flow (host-bound).
    pub pins: Vec<(String, Fingerprint)>,
    pub cert_prompt: Option<CertPrompt>,
    /// A cancel button is shown when other servers exist.
    pub can_cancel: bool,
    /// Identifies the current check; results of older checks are ignored.
    attempt: u64,
}

impl AddServer {
    pub fn new(can_cancel: bool) -> Self {
        Self {
            can_cancel,
            ..Self::default()
        }
    }

    pub fn with_url(mut self, url: &str) -> Self {
        self.url = url.to_owned();
        self
    }

    /// `existing`: URLs of configured servers (duplicates are rejected).
    pub fn update(&mut self, msg: Message, existing: &[ServerUrl]) -> (Task<Message>, Out) {
        match msg {
            Message::UrlChanged(s) => {
                self.url = s;
                self.error = None;
                // Editing the address abandons a running check.
                self.busy = false;
                self.attempt += 1;
            }
            Message::Submit => {
                if self.busy {
                    return (Task::none(), Out::None);
                }
                let url = match ServerUrl::parse(&self.url) {
                    Ok(u) => u,
                    Err(e) => {
                        self.error = Some(t!("add_server.invalid_url", reason = e).into_owned());
                        return (Task::none(), Out::None);
                    }
                };
                if existing.iter().any(|u| u.as_str() == url.as_str()) {
                    self.error = Some(t!("add_server.duplicate").into_owned());
                    return (Task::none(), Out::None);
                }
                return (self.check(url), Out::None);
            }
            Message::Checked(attempt, url, result) => {
                if attempt != self.attempt || !self.busy {
                    return (Task::none(), Out::None);
                }
                self.busy = false;
                match result {
                    Ok(probe) => {
                        return (
                            Task::none(),
                            Out::Added {
                                probe,
                                pins: std::mem::take(&mut self.pins),
                            },
                        );
                    }
                    Err(ProbeError::Certificate { host, chain }) if !chain.is_empty() => {
                        self.cert_prompt = Some(CertPrompt {
                            url,
                            host,
                            views: chain.iter().map(CertView::new).collect(),
                            chain,
                        });
                    }
                    Err(e) => self.error = Some(e.user_text()),
                }
            }
            Message::TrustCertificate => {
                if let Some(p) = self.cert_prompt.take()
                    && let Some(leaf) = p.chain.first()
                {
                    let pin = (p.host.clone(), leaf.sha256);
                    if !self.pins.contains(&pin) {
                        self.pins.push(pin);
                    }
                    return (self.check(p.url), Out::None);
                }
            }
            Message::RejectCertificate => {
                self.cert_prompt = None;
                self.error = Some(t!("add_server.certificate_rejected").into_owned());
            }
            Message::Cancel => return (Task::none(), Out::Cancel),
        }
        (Task::none(), Out::None)
    }

    fn check(&mut self, url: ServerUrl) -> Task<Message> {
        self.busy = true;
        self.error = None;
        self.attempt += 1;
        let attempt = self.attempt;
        let pins = self.pins.clone();
        Task::perform(
            async move {
                let r = probe(url.clone(), pins).await.map(Box::new);
                (url, r)
            },
            move |(url, r)| Message::Checked(attempt, url, r),
        )
    }

    pub fn view(&self) -> Element<'_, Message> {
        let mut body = column![
            text_field(t!("add_server.url_label"), &self.url, Message::UrlChanged)
                .placeholder(t!("add_server.url_placeholder"))
                .help(t!("add_server.url_help"))
                .error(self.error.clone())
                .id(URL_INPUT_ID)
                .on_submit(Message::Submit),
            primary_button(
                t!("add_server.connect"),
                (!self.url.trim().is_empty()).then_some(Message::Submit)
            )
            .loading(self.busy)
            .width(Length::Fill),
        ]
        .spacing(SPACE_4);
        if self.can_cancel {
            body = body.push(
                ghost_button(t!("add_server.cancel"), Some(Message::Cancel)).width(Length::Fill),
            );
        }
        super::card(
            t!("add_server.title").into_owned(),
            Some(t!("add_server.subtitle").into_owned()),
            body.into(),
        )
    }

    /// The certificate dialog, when a server presented an untrusted certificate.
    pub fn overlay(&self) -> Option<Element<'_, Message>> {
        let p = self.cert_prompt.as_ref()?;
        let leaf = p.views.first()?;
        let mut body = column![
            banner(
                BannerKind::Warning,
                t!("add_server.cert_warning", host = p.host),
                None
            ),
            text(t!("add_server.cert_subject"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
            text(
                leaf.subject
                    .clone()
                    .unwrap_or_else(|| t!("add_server.cert_unknown").into_owned())
            ),
        ]
        .spacing(SPACE_2);
        if let Some(issuer) = &leaf.issuer {
            body = body
                .push(
                    text(t!("add_server.cert_issuer"))
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                )
                .push(text(issuer.clone()));
        }
        body = body
            .push(
                text(t!("add_server.cert_fingerprint"))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            )
            .push(
                container(
                    text(leaf.fingerprint.clone())
                        .font(MONOSPACE_FONT)
                        .size(TEXT_CAPTION),
                )
                .padding(SPACE_3)
                .width(Length::Fill)
                .style(style::container_code_block),
            )
            .push(
                text(t!("add_server.cert_hint"))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            );
        Some(dialog(
            t!("add_server.cert_title"),
            body,
            vec![
                secondary_button(
                    t!("add_server.cert_reject"),
                    Some(Message::RejectCertificate),
                )
                .into(),
                primary_button(t!("add_server.cert_trust"), Some(Message::TrustCertificate))
                    .icon(Icon::ShieldCheck)
                    .into(),
            ],
            Some(Message::RejectCertificate),
        ))
    }
}
