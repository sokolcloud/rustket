//! Video conferences (docs/teams-videoconf.md §7.3). `leave`, `heartbeat` and
//! `share-chat` belong to the in-product call window and are not offered.

use serde_json::{Value, json};

use super::teams::Page;
use super::{Query, RestClient, paginated};
use crate::error::{Error, Result};
use crate::models::{
    JoinResult, JoinableCall, Paginated, ProviderCapabilities, StartInstructions,
    VideoConfProvider, VideoConference,
};

/// Provider errors of `capabilities` / `start` (§7.4), as `error` (no
/// `errorType`) of a 400 response.
pub const NO_PROVIDER_APP: &str = "no-videoconf-provider-app";
pub const NO_ACTIVE_PROVIDER: &str = "no-active-video-conf-provider";
pub const PROVIDER_NOT_CONFIGURED: &str = "video-conf-provider-not-configured";
pub const PROVIDER_UNAVAILABLE: &str = "video-conf-provider-unavailable";

/// The provider error code of `e` (`no-videoconf-provider-app`, …), if it is one.
pub fn provider_error(e: &Error) -> Option<&str> {
    match e.api_error_code()? {
        c @ (NO_PROVIDER_APP
        | NO_ACTIVE_PROVIDER
        | PROVIDER_NOT_CONFIGURED
        | PROVIDER_UNAVAILABLE) => Some(c),
        _ => None,
    }
}

impl RestClient {
    /// `video-conference.providers`.
    pub async fn videoconf_providers(&self) -> Result<Vec<VideoConfProvider>> {
        let v: Value = self
            .get("video-conference.providers", &Query::new())
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get("data").cloned().unwrap_or_default(),
        ))
    }

    /// `video-conference.capabilities` (fails with a provider error code when
    /// no provider is usable; no side effect, unlike `start`).
    pub async fn videoconf_capabilities(&self) -> Result<ProviderCapabilities> {
        self.get("video-conference.capabilities", &Query::new())
            .await
    }

    /// `video-conference.start`.
    pub async fn videoconf_start(
        &self,
        rid: &str,
        title: Option<&str>,
        allow_ringing: bool,
    ) -> Result<StartInstructions> {
        let mut body = json!({ "roomId": rid, "allowRinging": allow_ringing });
        if let Some(t) = title.filter(|t| !t.is_empty()) {
            body["title"] = json!(t);
        }
        let v: Value = self.post("video-conference.start", &body).await?;
        v.get("data")
            .and_then(StartInstructions::from_value)
            .ok_or_else(|| Error::Protocol("video-conference.start: no call instructions".into()))
    }

    /// `video-conference.join`: the per-user URL to open in the browser.
    pub async fn videoconf_join(&self, call_id: &str, mic: bool, cam: bool) -> Result<JoinResult> {
        self.post(
            "video-conference.join",
            &json!({ "callId": call_id, "state": { "mic": mic, "cam": cam } }),
        )
        .await
    }

    /// `video-conference.cancel`: the caller gives up a ringing direct call.
    pub async fn videoconf_cancel(&self, call_id: &str) -> Result<()> {
        self.post_ok("video-conference.cancel", &json!({ "callId": call_id }))
            .await
    }

    /// `video-conference.decline`.
    pub async fn videoconf_decline(&self, call_id: &str) -> Result<()> {
        self.post_ok("video-conference.decline", &json!({ "callId": call_id }))
            .await
    }

    /// `video-conference.info` (with `capabilities` and `chatAccess`).
    pub async fn videoconf_info(&self, call_id: &str) -> Result<VideoConference> {
        self.get(
            "video-conference.info",
            &Query::new().push("callId", call_id),
        )
        .await
    }

    /// `video-conference.list`: call history of a room, newest first.
    pub async fn videoconf_list(
        &self,
        rid: &str,
        page: Page,
    ) -> Result<Paginated<VideoConference>> {
        let q = Query::new()
            .push("roomId", rid)
            .push("offset", page.offset)
            .opt("count", (page.count > 0).then_some(page.count));
        let v: Value = self.get("video-conference.list", &q).await?;
        paginated(v, "data")
    }

    /// `video-conference.joinable`.
    pub async fn videoconf_joinable(&self) -> Result<Vec<JoinableCall>> {
        let v: Value = self.get("video-conference.joinable", &Query::new()).await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get("calls").cloned().unwrap_or_default(),
        ))
    }

    /// `video-conference.ring`: re-rings one member; `true` if it rang.
    pub async fn videoconf_ring(&self, call_id: &str, uid: &str) -> Result<bool> {
        let v: Value = self
            .post(
                "video-conference.ring",
                &json!({ "callId": call_id, "userId": uid }),
            )
            .await?;
        Ok(v.get("rang").and_then(Value::as_bool).unwrap_or(false))
    }

    /// `video-conference.add-participants` (1..10 usernames); the added ids.
    pub async fn videoconf_add_participants(
        &self,
        call_id: &str,
        usernames: &[&str],
        ring: bool,
    ) -> Result<Vec<String>> {
        let v: Value = self
            .post(
                "video-conference.add-participants",
                &json!({ "callId": call_id, "users": usernames, "ring": ring }),
            )
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get("added").cloned().unwrap_or_default(),
        ))
    }

    /// `video-conference.rename` (creator of a running group call).
    pub async fn videoconf_rename(&self, call_id: &str, title: &str) -> Result<()> {
        self.post_ok(
            "video-conference.rename",
            &json!({ "callId": call_id, "title": title }),
        )
        .await
    }
}
