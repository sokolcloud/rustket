//! Sending into encrypted rooms (§7.6, §7.8): when to encrypt, when to block,
//! and the encrypted forms of messages, edits and uploads.

use rocketchat::RestClient;
use rocketchat::models::{Message, UploadedFile};
use rocketchat::rest::EncryptedMediaConfirm;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::e2e::{E2eManager, E2eSettings, OutgoingEncrypted};

/// How a message to a room goes out.
#[derive(Debug, Clone)]
pub enum Mode {
    /// Not an encrypted room (or E2E is off on the server): plaintext.
    Plain,
    /// Encrypt with the room key.
    Encrypt(E2eManager),
    /// The room key is missing and unencrypted messages are not allowed.
    Blocked,
    /// The room key is missing but the server allows unencrypted messages:
    /// plaintext, with the composer hint shown.
    Unencrypted,
}

impl Mode {
    pub fn manager(&self) -> Option<&E2eManager> {
        match self {
            Mode::Encrypt(m) => Some(m),
            _ => None,
        }
    }
}

/// Whether `rid` is encrypted; `None` while only its subscription is known
/// (the room object was dropped as unparsable, or a subscription insert
/// arrived before the room). A subscription holding a room key means an
/// encrypted room.
fn encrypted_state(ws: &Workspace, rid: &str) -> Option<bool> {
    let Some(e) = ws.room(rid) else {
        return Some(false);
    };
    match &e.room {
        Some(_) => Some(e.is_encrypted()),
        None if e.sub.as_ref().is_some_and(|s| s.e2e_key.is_some()) => Some(true),
        None => None,
    }
}

fn room_encrypted(ws: &Workspace, rid: &str) -> bool {
    encrypted_state(ws, rid) == Some(true)
}

/// Whether the room object of `rid` is loaded (see [`encrypted_state`]).
pub fn room_known(ws: &Workspace, rid: &str) -> bool {
    encrypted_state(ws, rid).is_some()
}

/// E2E settings as published by the server.
pub fn settings(ws: &Workspace) -> E2eSettings {
    E2eSettings::from_settings(ws.settings())
}

/// Whether the server's E2E settings are known (`settings.public` may have
/// failed during the load; until they are, encrypted rooms fail closed).
pub fn settings_loaded(ws: &Workspace) -> bool {
    E2eSettings::loaded(ws.settings())
}

/// The send mode of `rid`.
pub fn mode(ws: &Workspace, rid: &str) -> Mode {
    let state = encrypted_state(ws, rid);
    if state == Some(false) {
        return Mode::Plain;
    }
    if !settings_loaded(ws) {
        return Mode::Blocked;
    }
    let s = settings(ws);
    if !s.enabled {
        return Mode::Plain;
    }
    if state.is_none() {
        // Whether the room is encrypted is not known yet: fail closed.
        return Mode::Blocked;
    }
    if let Some(m) = ws.e2e().filter(|m| m.can_encrypt(rid)) {
        return Mode::Encrypt(m.clone());
    }
    if s.allow_unencrypted {
        Mode::Unencrypted
    } else {
        Mode::Blocked
    }
}

/// Slash commands are not run in rooms that must stay encrypted (§2.1:
/// `isSlashCommandAllowed`).
pub fn slash_commands_blocked(ws: &Workspace, rid: &str) -> bool {
    let s = settings(ws);
    encrypted_state(ws, rid) != Some(false)
        && (!settings_loaded(ws) || (s.enabled && !s.allow_unencrypted))
}

/// Why sending to `rid` is refused ([`Mode::Blocked`]), for a toast.
pub fn blocked_text(ws: &Workspace, rid: &str) -> String {
    if !settings_loaded(ws) {
        t!("e2e.composer_settings_unknown").into_owned()
    } else if !room_known(ws, rid) {
        t!("e2e.composer_room_unknown").into_owned()
    } else {
        t!("e2e.send_blocked").into_owned()
    }
}

/// A local failure carried like a server error (shown with its text).
pub fn local_error(text: String) -> rocketchat::Error {
    rocketchat::Error::Api {
        status: 0,
        error: None,
        error_type: Some("e2e".into()),
        message: Some(text),
        details: None,
    }
}

/// The encrypted form of an outbox message: `None` to send it as is,
/// `Some(Err)` when it must not go out (never downgraded silently).
pub fn prepare_send(
    ws: &Workspace,
    m: &Message,
) -> Option<Result<OutgoingEncrypted, rocketchat::Error>> {
    match mode(ws, &m.rid) {
        Mode::Plain | Mode::Unencrypted => None,
        Mode::Blocked => Some(Err(local_error(blocked_text(ws, &m.rid)))),
        Mode::Encrypt(mgr) => Some(encrypt_text(&mgr, m).map_err(local_error)),
    }
}

/// Sends an outbox message: encrypted when `enc` is given.
pub async fn send_outgoing(
    rest: RestClient,
    out: rocketchat::models::OutgoingMessage,
    enc: Option<Result<OutgoingEncrypted, rocketchat::Error>>,
) -> rocketchat::Result<Message> {
    match enc {
        None => rest.send_message(&out).await,
        Some(Err(e)) => Err(e),
        Some(Ok(enc)) => {
            let id = out.id.unwrap_or_else(rocketchat::ids::random_id);
            rest.e2e_send_message(
                &out.rid,
                &id,
                out.tmid.as_deref(),
                out.tshow == Some(true),
                &enc.content,
                enc.mentions.as_ref(),
            )
            .await
        }
    }
}

/// The encrypted form of an edit of `original` (the message as shown when
/// the edit started): `None` for a plain edit, `Some(Err)` when the edit must
/// not be sent (an encrypted message is never edited with plaintext, §8).
pub fn prepare_edit(
    ws: &Workspace,
    rid: &str,
    original: &Message,
    text: &str,
) -> Option<Result<OutgoingEncrypted, String>> {
    let encrypted = original.t.as_deref() == Some("e2e") || original.content.is_some();
    if !encrypted {
        // A plaintext message cannot become encrypted (the server refuses
        // `content` unless the message is `t: 'e2e'`); a plaintext edit goes
        // out only where unencrypted messages may (§7.6).
        return plain_edit_refusal(ws, rid).map(Err);
    }
    if let Err(e) = super::room::check_edit(ws, original) {
        return Some(Err(e));
    }
    // A copy from another room never decides how this room is written to.
    if original.rid != rid {
        return Some(Err(t!("e2e.edit_unavailable").into_owned()));
    }
    Some(match mode(ws, rid) {
        Mode::Encrypt(mgr) => encrypt_edit(&mgr, original, text),
        _ => Err(t!("e2e.edit_unavailable").into_owned()),
    })
}

/// Why a plaintext message of `rid` cannot be edited (with plaintext), if it
/// cannot: the room must stay encrypted, or its state is unknown.
pub fn plain_edit_refusal(ws: &Workspace, rid: &str) -> Option<String> {
    match mode(ws, rid) {
        Mode::Plain | Mode::Unencrypted => None,
        Mode::Blocked => Some(blocked_text(ws, rid)),
        Mode::Encrypt(_) if settings(ws).allow_unencrypted => None,
        Mode::Encrypt(_) => Some(t!("e2e.edit_plaintext_refused").into_owned()),
    }
}

/// Sends an edit: `chat.update` with `content` when encrypted.
pub async fn update_message(
    rest: RestClient,
    rid: String,
    mid: String,
    text: String,
    enc: Option<OutgoingEncrypted>,
) -> rocketchat::Result<Message> {
    match enc {
        None => rest.update_message(&rid, &mid, &text).await,
        Some(e) => {
            rest.e2e_update_message(&rid, &mid, &e.content, e.mentions.as_ref())
                .await
        }
    }
}

/// Encrypts an outbox message (text, thread fields stay plaintext).
pub fn encrypt_text(mgr: &E2eManager, m: &Message) -> Result<OutgoingEncrypted, String> {
    mgr.encrypt_outgoing(&m.rid, &m.msg)
        .map_err(|e| super::error_text(&e))
}

/// Sends an encrypted outbox message (`chat.sendMessage` with `t: 'e2e'`).
pub async fn send(
    rest: RestClient,
    m: Message,
    out: OutgoingEncrypted,
) -> rocketchat::Result<Message> {
    rest.e2e_send_message(
        &m.rid,
        &m.id,
        m.tmid.as_deref(),
        m.tshow == Some(true),
        &out.content,
        out.mentions.as_ref(),
    )
    .await
}

/// An edit of `original` (an encrypted message as shown): re-encrypts
/// `{msg, attachments, files, file}` of the original decrypted payload with
/// the current key (never quote attachments added locally, never `text`).
pub fn encrypt_edit(
    mgr: &E2eManager,
    original: &Message,
    text: &str,
) -> Result<OutgoingEncrypted, String> {
    let raw = super::shown::raw_view(original);
    let payload = match mgr.decrypt_message(&raw) {
        rustket_core::e2e::DecryptResult::Decrypted(d) => Some(d.message.clone()),
        _ => None,
    };
    if payload.is_none() && original.content.is_some() {
        // Never drop the file keys of a message that could not be read.
        return Err(t!("e2e.edit_unreadable").into_owned());
    }
    mgr.encrypt_edit(&original.rid, text, payload.as_ref())
        .map_err(|e| super::error_text(&e))
}

/// What the upload path does in `rid` (§2.1 `E2E_Enable_Encrypt_Files`).
#[derive(Debug, Clone)]
pub enum UploadMode {
    Plain,
    Encrypt(E2eManager),
    /// Refused, with the reason.
    Refused(String),
}

pub fn upload_mode(ws: &Workspace, rid: &str) -> UploadMode {
    let s = settings(ws);
    match mode(ws, rid) {
        Mode::Plain | Mode::Unencrypted => {
            // Files are refused even with E2E off when the room is encrypted
            // and files must not be sent unencrypted (web behaviour).
            if room_encrypted(ws, rid) && !s.encrypt_files && !s.allow_unencrypted {
                UploadMode::Refused(t!("e2e.files_unencrypted_refused").into_owned())
            } else {
                UploadMode::Plain
            }
        }
        Mode::Encrypt(m) if s.encrypt_files => UploadMode::Encrypt(m),
        Mode::Encrypt(_) if s.allow_unencrypted => UploadMode::Plain,
        Mode::Encrypt(_) => UploadMode::Refused(t!("e2e.files_unencrypted_refused").into_owned()),
        Mode::Blocked => UploadMode::Refused(blocked_text(ws, rid)),
    }
}

/// Pixel size of an image (for the encrypted attachment's dimensions).
fn image_dimensions(mime: &str, data: &[u8]) -> Option<(u32, u32)> {
    if !mime.starts_with("image/") {
        return None;
    }
    ::image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Encrypts and uploads a file, then confirms it as an encrypted message
/// (§7.8). The caption goes into the encrypted payload; `description` (alt
/// text) and the file name never reach the server in plaintext (§11.27).
#[allow(clippy::too_many_arguments)]
pub async fn upload(
    mgr: E2eManager,
    rest: RestClient,
    rid: String,
    tmid: Option<String>,
    name: String,
    mime: String,
    data: Vec<u8>,
    caption: String,
    alt: Option<String>,
    progress: Option<rocketchat::rest::ProgressFn>,
) -> Result<Message, String> {
    let dims = image_dimensions(&mime, &data);
    let mut prepared = mgr
        .prepare_upload_async(&rid, name, mime, data)
        .await
        .map_err(|e| super::error_text(&e))?;
    if let Some(alt) = alt.filter(|a| !a.trim().is_empty()) {
        prepared.metadata.alt_text = Some(alt);
        let meta = serde_json::to_value(&prepared.metadata)
            .map_err(|_| t!("e2e.err_internal").into_owned())?;
        prepared.file_content = mgr
            .encrypt_value(&rid, &meta)
            .map_err(|e| super::error_text(&e))?;
    }
    let uploaded: UploadedFile = rest
        .e2e_upload_media(
            &rid,
            prepared.hashed_name.clone(),
            rocketchat::rest::UploadSource::Bytes(prepared.encrypted.clone()),
            &prepared.file_content,
            progress,
        )
        .await
        .map_err(|e| crate::error::user_text(&e))?;
    let mut payload = rustket_core::e2e::upload_payload(&prepared, &uploaded, &caption, dims);
    if let (Some(alt), Some(a)) = (
        prepared.metadata.alt_text.clone(),
        payload.attachments.as_mut().and_then(|a| a.first_mut()),
    ) && a.image_url.is_some()
    {
        a.image_alt = Some(alt);
    }
    let content = mgr
        .encrypt_payload(&rid, &payload)
        .map_err(|e| super::error_text(&e))?;
    rest.e2e_confirm_media(
        &rid,
        &uploaded.id,
        &EncryptedMediaConfirm {
            tmid,
            content,
            file_content: prepared.file_content,
        },
    )
    .await
    .map_err(|e| crate::error::user_text(&e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{Room, RoomType, Setting, Subscription};
    use rustket_core::workspace::Change;

    fn ws(enabled: bool, allow: bool, encrypted: bool) -> Workspace {
        let mut w = Workspace::new();
        for (id, v) in [
            ("E2E_Enable", enabled),
            ("E2E_Allow_Unencrypted_Messages", allow),
        ] {
            w.apply_setting(
                Change::Updated,
                Setting {
                    id: id.into(),
                    value: serde_json::json!(v),
                    ..Default::default()
                },
            );
        }
        w.apply_room(
            Change::Inserted,
            Room {
                id: "g".into(),
                t: RoomType::Private,
                encrypted: Some(encrypted),
                ..Default::default()
            },
        );
        w.apply_subscription(
            Change::Inserted,
            Subscription {
                rid: "g".into(),
                t: RoomType::Private,
                ..Default::default()
            },
        );
        w
    }

    #[test]
    fn modes() {
        assert!(matches!(mode(&ws(false, false, true), "g"), Mode::Plain));
        assert!(matches!(mode(&ws(true, false, false), "g"), Mode::Plain));
        assert!(matches!(mode(&ws(true, false, true), "g"), Mode::Blocked));
        assert!(matches!(
            mode(&ws(true, true, true), "g"),
            Mode::Unencrypted
        ));
        assert!(slash_commands_blocked(&ws(true, false, true), "g"));
        assert!(!slash_commands_blocked(&ws(true, true, true), "g"));
        assert!(!slash_commands_blocked(&ws(false, false, true), "g"));
        assert!(matches!(
            upload_mode(&ws(true, false, true), "g"),
            UploadMode::Refused(_)
        ));
        assert!(matches!(
            upload_mode(&ws(false, false, false), "g"),
            UploadMode::Plain
        ));
    }

    /// `settings.public` failed during the load: no E2E settings at all.
    fn ws_without_settings() -> Workspace {
        let mut w = Workspace::new();
        for (id, encrypted) in [("g", true), ("p", false)] {
            w.apply_room(
                Change::Inserted,
                Room {
                    id: id.into(),
                    t: RoomType::Private,
                    encrypted: Some(encrypted),
                    ..Default::default()
                },
            );
            w.apply_subscription(
                Change::Inserted,
                Subscription {
                    rid: id.into(),
                    t: RoomType::Private,
                    ..Default::default()
                },
            );
        }
        w
    }

    #[test]
    fn unknown_settings_fail_closed_in_encrypted_rooms() {
        let w = ws_without_settings();
        assert!(!settings_loaded(&w));
        let text = |rid: &str| Message {
            id: format!("m-{rid}"),
            rid: rid.into(),
            msg: "secret".into(),
            ..Default::default()
        };
        // Text, slash commands, uploads and edits never go out in plaintext.
        assert!(matches!(mode(&w, "g"), Mode::Blocked));
        assert!(matches!(prepare_send(&w, &text("g")), Some(Err(_))));
        assert!(slash_commands_blocked(&w, "g"));
        assert!(matches!(upload_mode(&w, "g"), UploadMode::Refused(_)));
        assert!(matches!(
            prepare_edit(&w, "g", &text("g"), "edited"),
            Some(Err(_))
        ));
        assert!(super::super::room::check_edit(&w, &text("g")).is_err());
        assert!(super::super::room::composer_block::<()>(&w, "g").is_some());
        // Rooms that are not encrypted are not affected.
        assert!(matches!(mode(&w, "p"), Mode::Plain));
        assert!(prepare_send(&w, &text("p")).is_none());
        assert!(!slash_commands_blocked(&w, "p"));
        assert!(matches!(upload_mode(&w, "p"), UploadMode::Plain));
        assert!(prepare_edit(&w, "p", &text("p"), "edited").is_none());
        // Once the settings arrive (E2E off), the encrypted room is writable.
        let mut w = w;
        w.apply_setting(
            Change::Updated,
            Setting {
                id: "E2E_Enable".into(),
                value: serde_json::json!(false),
                ..Default::default()
            },
        );
        assert!(settings_loaded(&w));
        assert!(matches!(mode(&w, "g"), Mode::Plain));
    }

    #[test]
    fn unknown_settings_refusals_say_why() {
        let w = ws_without_settings();
        let text = Message {
            id: "m".into(),
            rid: "g".into(),
            msg: "plain".into(),
            ..Default::default()
        };
        let reason = t!("e2e.composer_settings_unknown").into_owned();
        assert!(matches!(upload_mode(&w, "g"), UploadMode::Refused(r) if r == reason));
        assert_eq!(plain_edit_refusal(&w, "g"), Some(reason.clone()));
        assert_eq!(
            prepare_edit(&w, "g", &text, "x").and_then(Result::err),
            Some(reason)
        );
    }

    /// A room entry with only a subscription (no room object).
    fn ws_sub_only(enabled: bool, e2e_key: Option<&str>) -> Workspace {
        let mut w = ws(enabled, false, false);
        w.apply_subscription(
            Change::Inserted,
            Subscription {
                rid: "s".into(),
                t: RoomType::Private,
                e2e_key: e2e_key.map(str::to_owned),
                ..Default::default()
            },
        );
        w
    }

    #[test]
    fn rooms_without_a_room_object_fail_closed() {
        let w = ws_sub_only(true, None);
        assert!(w.room("s").is_some_and(|e| e.room.is_none()));
        let text = Message {
            id: "m".into(),
            rid: "s".into(),
            msg: "secret".into(),
            ..Default::default()
        };
        assert!(matches!(mode(&w, "s"), Mode::Blocked));
        assert!(matches!(prepare_send(&w, &text), Some(Err(_))));
        let reason = t!("e2e.composer_room_unknown").into_owned();
        assert!(matches!(upload_mode(&w, "s"), UploadMode::Refused(r) if r == reason));
        assert!(matches!(
            prepare_edit(&w, "s", &text, "edited"),
            Some(Err(_))
        ));
        assert!(slash_commands_blocked(&w, "s"));
        assert!(super::super::room::composer_block::<()>(&w, "s").is_some());
        // A subscription with a room key is an encrypted room.
        let w = ws_sub_only(true, Some("key"));
        assert!(room_encrypted(&w, "s"));
        assert!(matches!(mode(&w, "s"), Mode::Blocked));
        assert!(matches!(upload_mode(&w, "s"), UploadMode::Refused(_)));
        // E2E off on the server: nothing to protect.
        let w = ws_sub_only(false, None);
        assert!(matches!(mode(&w, "s"), Mode::Plain));
        assert!(matches!(upload_mode(&w, "s"), UploadMode::Plain));
    }

    #[tokio::test]
    async fn plaintext_messages_are_not_edited_into_encrypted_ones() {
        use crate::room::timeline::Act;
        let (mgr, _, _) = super::super::shown::tests::ready_manager_with_key("g").await;
        let plain = Message {
            id: "p".into(),
            rid: "g".into(),
            msg: "sent before encryption".into(),
            ..Default::default()
        };
        // Unencrypted messages not allowed: refused before the composer
        // drops the edit, and the action is not offered.
        let mut w = ws(true, false, true);
        w.set_e2e(Some(mgr.clone()));
        assert!(matches!(mode(&w, "g"), Mode::Encrypt(_)));
        assert_eq!(
            prepare_edit(&w, "g", &plain, "x").and_then(Result::err),
            Some(t!("e2e.edit_plaintext_refused").into_owned())
        );
        assert!(super::super::room::check_edit(&w, &plain).is_err());
        let mut acts = vec![Act::Edit, Act::CopyText];
        super::super::room::filter_actions(&w, &plain, &mut acts);
        assert!(!acts.contains(&Act::Edit));
        assert!(acts.contains(&Act::CopyText));
        // Allowed: a plaintext message stays plaintext (`chat.update {text}`).
        let mut w = ws(true, true, true);
        w.set_e2e(Some(mgr));
        assert!(matches!(mode(&w, "g"), Mode::Encrypt(_)));
        assert!(prepare_edit(&w, "g", &plain, "x").is_none());
        assert!(super::super::room::check_edit(&w, &plain).is_ok());
    }

    #[test]
    fn edits_never_go_out_as_plaintext() {
        let e2e = Message {
            id: "m".into(),
            rid: "g".into(),
            t: Some("e2e".into()),
            content: Some(serde_json::json!({"algorithm": "rc.v2.aes-sha2"})),
            ..Default::default()
        };
        let plain = Message {
            id: "p".into(),
            rid: "g".into(),
            msg: "hello".into(),
            ..Default::default()
        };
        // Key missing, unencrypted messages not allowed: nothing goes out.
        let blocked = ws(true, false, true);
        assert!(matches!(
            prepare_edit(&blocked, "g", &e2e, "x"),
            Some(Err(_))
        ));
        assert!(matches!(
            prepare_edit(&blocked, "g", &plain, "x"),
            Some(Err(_))
        ));
        // Unencrypted messages allowed: a plaintext message may be edited as
        // plaintext, an encrypted one (not decrypted here) never.
        let allowed = ws(true, true, true);
        assert!(matches!(
            prepare_edit(&allowed, "g", &e2e, "x"),
            Some(Err(_))
        ));
        assert!(prepare_edit(&allowed, "g", &plain, "x").is_none());
        // E2E off on the server: encrypted messages stay read-only.
        let off = ws(false, false, true);
        assert!(matches!(prepare_edit(&off, "g", &e2e, "x"), Some(Err(_))));
        assert!(prepare_edit(&off, "g", &plain, "x").is_none());
        assert!(super::super::room::check_edit(&off, &e2e).is_err());
        assert!(super::super::room::check_edit(&off, &plain).is_ok());
    }
}
