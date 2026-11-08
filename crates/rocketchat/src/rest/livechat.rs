//! Omnichannel (livechat) endpoints for agents (`docs/omnichannel.md`). REST only:
//! the agent-side `livechat:*` DDP methods were removed in Rocket.Chat 8.0.

use serde_json::{Value, json};

use super::{Query, RestClient, field, paginated};
use crate::error::{Error, Result};
use crate::models::{
    AgentDepartment, AgentStatus, CannedResponse, CannedScope, CloseChat, Contact, ContactChannel,
    CustomFieldDef, Department, DepartmentRef, ForwardChat, Inquiry, LicenseInfo, LivechatAgent,
    Message, Paginated, Priority, Room, RoutingConfig, SaveCannedResponse, SaveRoomInfo, Sla, Tag,
    TransferData, Visitor,
};
use crate::server_url::encode_segment;

/// `count` / `offset` of a paginated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Page {
    pub count: Option<u64>,
    pub offset: Option<u64>,
}

impl Page {
    pub fn new(offset: u64, count: u64) -> Self {
        Self {
            count: Some(count),
            offset: Some(offset),
        }
    }

    fn apply(&self, q: Query) -> Query {
        q.opt("count", self.count).opt("offset", self.offset)
    }
}

/// Filter of [`RestClient::livechat_departments`] (`GET livechat/department`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepartmentQuery {
    pub text: Option<String>,
    pub only_my_departments: bool,
    /// Only enabled departments (`false` = no filter).
    pub enabled: bool,
    pub show_archived: bool,
    /// e.g. the room's current department when forwarding.
    pub exclude_department_id: Option<String>,
    pub page: Page,
}

impl DepartmentQuery {
    /// The forward picker: enabled departments except the room's current one.
    pub fn for_forward(text: Option<String>, current: Option<&str>) -> Self {
        Self {
            text,
            enabled: true,
            exclude_department_id: current.map(str::to_owned),
            page: Page::new(0, 50),
            ..Default::default()
        }
    }
}

/// Filter of [`RestClient::livechat_agents`] (`GET livechat/users/agent`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentQuery {
    pub text: Option<String>,
    pub only_available: bool,
    /// Usually the room's serving agent.
    pub exclude_id: Option<String>,
    /// `false` hides agents whose status is offline.
    pub show_idle_agents: bool,
    pub page: Page,
}

impl AgentQuery {
    /// The forward picker: available, reachable agents except `serving`.
    pub fn for_forward(text: Option<String>, serving: Option<&str>) -> Self {
        Self {
            text,
            only_available: true,
            exclude_id: serving.map(str::to_owned),
            show_idle_agents: false,
            page: Page::new(0, 25),
        }
    }
}

/// Filter of [`RestClient::livechat_rooms`] (`GET livechat/rooms`). Agents
/// without `view-livechat-rooms` must set `agents` to exactly their own id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LivechatRoomsQuery {
    pub agents: Vec<String>,
    pub room_name: Option<String>,
    pub department_id: Option<String>,
    pub open: Option<bool>,
    pub on_hold: Option<bool>,
    pub queued: Option<bool>,
    pub tags: Vec<String>,
    pub page: Page,
}

/// Filter of [`RestClient::canned_responses`] (EE).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CannedResponseQuery {
    /// Matches shortcut or text.
    pub text: Option<String>,
    pub scope: Option<CannedScope>,
    pub department_id: Option<String>,
    pub page: Page,
}

fn seg(s: &str) -> String {
    encode_segment(s)
}

impl Error {
    /// The server feature needs an Enterprise license (`error-action-not-allowed`,
    /// "This is an enterprise feature"): hide it instead of showing an error.
    pub fn is_enterprise_only(&self) -> bool {
        self.livechat_error_code() == Some("error-action-not-allowed")
    }

    /// The error code of an omnichannel endpoint: `errorType`, else a trailing
    /// `[code]` of `error`, else `error` itself (several endpoints only send
    /// `{"error": "error-comment-is-required"}` or a plain text like
    /// `"Agent not found"`).
    pub fn livechat_error_code(&self) -> Option<&str> {
        let Error::Api {
            error, error_type, ..
        } = self
        else {
            return None;
        };
        if let Some(t) = error_type.as_deref().filter(|t| !t.is_empty()) {
            return Some(t);
        }
        let e = error.as_deref()?.trim();
        if let Some(open) = e.rfind('[')
            && e.ends_with(']')
        {
            return Some(&e[open + 1..e.len() - 1]);
        }
        Some(e)
    }
}

impl RestClient {
    // --- availability, routing, license ------------------------------------------

    /// `POST livechat/agent.status`: sets the current agent's availability, or
    /// toggles it when `status` is `None`. Returns the resulting status (compare it
    /// with the requested one: business hours may keep it unchanged).
    pub async fn livechat_agent_status(&self, status: Option<AgentStatus>) -> Result<AgentStatus> {
        self.livechat_agent_status_for(None, status).await
    }

    /// Like [`Self::livechat_agent_status`] for another agent (needs
    /// `manage-livechat-agents`).
    pub async fn livechat_agent_status_for(
        &self,
        agent_id: Option<&str>,
        status: Option<AgentStatus>,
    ) -> Result<AgentStatus> {
        let mut body = json!({});
        if let Some(s) = status.filter(|s| *s != AgentStatus::Unknown) {
            body["status"] = json!(s.as_str());
        }
        if let Some(a) = agent_id {
            body["agentId"] = json!(a);
        }
        let v: Value = self.post("livechat/agent.status", &body).await?;
        field(v, "status")
    }

    /// `GET livechat/config/routing`.
    pub async fn livechat_routing_config(&self) -> Result<RoutingConfig> {
        let v: Value = self.get("livechat/config/routing", &Query::new()).await?;
        field(v, "config")
    }

    /// `GET licenses.info` → `license`.
    pub async fn licenses_info(&self) -> Result<LicenseInfo> {
        let v: Value = self.get("licenses.info", &Query::new()).await?;
        field(v, "license")
    }

    /// `GET livechat/agents/{uid}/departments`.
    pub async fn livechat_agent_departments(
        &self,
        uid: &str,
        enabled_only: bool,
    ) -> Result<Vec<AgentDepartment>> {
        let mut v: Value = self
            .get(
                &format!("livechat/agents/{}/departments", seg(uid)),
                &Query::new().push("enabledDepartmentsOnly", enabled_only),
            )
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get_mut("departments")
                .map(Value::take)
                .unwrap_or_default(),
        ))
    }

    // --- queue --------------------------------------------------------------------

    /// `GET livechat/inquiries.queuedForUser`. Pass the pool size setting as
    /// `count` (`None` = server default; `0` only means "all" while the private
    /// `API_Allow_Infinite_Count` is on). No `sort` is sent on purpose.
    pub async fn livechat_inquiries_queued(
        &self,
        count: Option<u64>,
        department: Option<&str>,
    ) -> Result<Paginated<Inquiry>> {
        let v: Value = self
            .get(
                "livechat/inquiries.queuedForUser",
                &Query::new()
                    .opt("count", count)
                    .opt("department", department),
            )
            .await?;
        paginated(v, "inquiries")
    }

    /// `GET livechat/inquiries.getOne?roomId=` (`None` when the room has no inquiry).
    pub async fn livechat_inquiry_for_room(&self, rid: &str) -> Result<Option<Inquiry>> {
        let mut v: Value = self
            .get(
                "livechat/inquiries.getOne",
                &Query::new().push("roomId", rid),
            )
            .await?;
        Ok(v.get_mut("inquiry")
            .map(Value::take)
            .filter(|i| !i.is_null())
            .and_then(crate::models::lenient::try_convert))
    }

    /// `POST livechat/inquiries.take` with `clientAction: true` (otherwise routing
    /// failures are swallowed). The response carries no inquiry.
    pub async fn livechat_take_inquiry(&self, inquiry_id: &str) -> Result<()> {
        self.post_ok(
            "livechat/inquiries.take",
            &json!({ "inquiryId": inquiry_id, "options": { "clientAction": true } }),
        )
        .await
    }

    /// `POST livechat/inquiries.returnAsInquiry`. Only offer it when the routing
    /// config has `returnQueue` (under auto assignment the inquiry gets stuck).
    pub async fn livechat_return_to_queue(
        &self,
        rid: &str,
        department_id: Option<&str>,
    ) -> Result<bool> {
        let mut body = json!({ "roomId": rid });
        if let Some(d) = department_id {
            body["departmentId"] = json!(d);
        }
        let v: Value = self
            .post("livechat/inquiries.returnAsInquiry", &body)
            .await?;
        Ok(v.get("result").and_then(Value::as_bool).unwrap_or(false))
    }

    // --- room actions ---------------------------------------------------------------

    /// `GET livechat/room.join?roomId=` (GET only; `POST` is a 404).
    pub async fn livechat_room_join(&self, rid: &str) -> Result<()> {
        let _: Value = self
            .get("livechat/room.join", &Query::new().push("roomId", rid))
            .await?;
        Ok(())
    }

    /// `POST livechat/room.closeByUser`.
    pub async fn livechat_close_chat(&self, close: &CloseChat) -> Result<()> {
        self.post_ok("livechat/room.closeByUser", close).await
    }

    /// `POST livechat/room.forward` (needs `transfer-livechat-guest`).
    pub async fn livechat_forward_chat(&self, forward: &ForwardChat) -> Result<()> {
        self.post_ok("livechat/room.forward", &forward.to_body())
            .await
    }

    /// `POST livechat/room.saveInfo`. `room.tags` must be the full list.
    pub async fn livechat_save_room_info(&self, info: &SaveRoomInfo) -> Result<()> {
        self.post_ok("livechat/room.saveInfo", info).await
    }

    /// `POST livechat/room.onHold` (EE).
    pub async fn livechat_place_on_hold(&self, rid: &str) -> Result<()> {
        self.post_ok("livechat/room.onHold", &json!({ "roomId": rid }))
            .await
    }

    /// `POST livechat/room.resumeOnHold` (EE).
    pub async fn livechat_resume_on_hold(&self, rid: &str) -> Result<()> {
        self.post_ok("livechat/room.resumeOnHold", &json!({ "roomId": rid }))
            .await
    }

    /// `POST livechat/room/{rid}/priority` (EE).
    pub async fn livechat_set_room_priority(&self, rid: &str, priority_id: &str) -> Result<()> {
        self.post_ok(
            &format!("livechat/room/{}/priority", seg(rid)),
            &json!({ "priorityId": priority_id }),
        )
        .await
    }

    /// `DELETE livechat/room/{rid}/priority` (EE).
    pub async fn livechat_clear_room_priority(&self, rid: &str) -> Result<()> {
        let _: Value = self
            .delete(
                &format!("livechat/room/{}/priority", seg(rid)),
                &Query::new(),
            )
            .await?;
        Ok(())
    }

    /// `GET livechat/transfer.history/{rid}` (needs `view-livechat-rooms`).
    pub async fn livechat_transfer_history(
        &self,
        rid: &str,
        page: Page,
    ) -> Result<Paginated<TransferData>> {
        let v: Value = self
            .get(
                &format!("livechat/transfer.history/{}", seg(rid)),
                &page.apply(Query::new()),
            )
            .await?;
        paginated(v, "history")
    }

    // --- transcripts ---------------------------------------------------------------

    /// `POST livechat/transcript/{rid}`: e-mail the transcript when the open room closes.
    pub async fn livechat_request_email_transcript(
        &self,
        rid: &str,
        email: &str,
        subject: &str,
    ) -> Result<()> {
        self.post_ok(
            &format!("livechat/transcript/{}", seg(rid)),
            &json!({ "email": email, "subject": subject }),
        )
        .await
    }

    /// `DELETE livechat/transcript/{rid}`.
    pub async fn livechat_cancel_email_transcript(&self, rid: &str) -> Result<()> {
        let _: Value = self
            .delete(&format!("livechat/transcript/{}", seg(rid)), &Query::new())
            .await?;
        Ok(())
    }

    /// `POST livechat/transcript` (visitor endpoint, for closed rooms): `token` is
    /// the room's visitor token (`room.v.token`).
    pub async fn livechat_send_transcript(
        &self,
        token: &str,
        rid: &str,
        email: &str,
    ) -> Result<()> {
        self.post_ok(
            "livechat/transcript",
            &json!({ "token": token, "rid": rid, "email": email }),
        )
        .await
    }

    /// `POST omnichannel/{rid}/request-transcript` (EE PDF; closed rooms only).
    pub async fn livechat_request_pdf_transcript(&self, rid: &str) -> Result<()> {
        self.post_ok(
            &format!("omnichannel/{}/request-transcript", seg(rid)),
            &json!({}),
        )
        .await
    }

    // --- visitors and contacts -----------------------------------------------------

    /// `GET livechat/visitors.info?visitorId=`.
    pub async fn livechat_visitor_info(&self, visitor_id: &str) -> Result<Visitor> {
        let v: Value = self
            .get(
                "livechat/visitors.info",
                &Query::new().push("visitorId", visitor_id),
            )
            .await?;
        field(v, "visitor")
    }

    /// `GET omnichannel/contacts.get?contactId=` (`None` when missing).
    pub async fn omnichannel_contact(&self, contact_id: &str) -> Result<Option<Contact>> {
        let r: Result<Value> = self
            .get(
                "omnichannel/contacts.get",
                &Query::new().push("contactId", contact_id),
            )
            .await;
        match r {
            Ok(mut v) => Ok(v
                .get_mut("contact")
                .map(Value::take)
                .filter(|c| !c.is_null())
                .and_then(crate::models::lenient::try_convert)),
            Err(Error::Api { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `GET omnichannel/contacts.channels?contactId=`.
    pub async fn omnichannel_contact_channels(
        &self,
        contact_id: &str,
    ) -> Result<Vec<ContactChannel>> {
        let mut v: Value = self
            .get(
                "omnichannel/contacts.channels",
                &Query::new().push("contactId", contact_id),
            )
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get_mut("channels").map(Value::take).unwrap_or_default(),
        ))
    }

    /// `GET omnichannel/contacts.search` (`count` echoes the request, not the
    /// result size).
    pub async fn omnichannel_contacts_search(
        &self,
        text: Option<&str>,
        unknown: Option<bool>,
        page: Page,
    ) -> Result<Paginated<Contact>> {
        let v: Value = self
            .get(
                "omnichannel/contacts.search",
                &page.apply(Query::new().opt("searchText", text).opt("unknown", unknown)),
            )
            .await?;
        paginated(v, "contacts")
    }

    /// `GET omnichannel/contacts.history`: closed chats of a contact, newest first.
    pub async fn omnichannel_contact_history(
        &self,
        contact_id: &str,
        source: Option<&str>,
        page: Page,
    ) -> Result<Paginated<Room>> {
        let v: Value = self
            .get(
                "omnichannel/contacts.history",
                &page.apply(
                    Query::new()
                        .push("contactId", contact_id)
                        .opt("source", source),
                ),
            )
            .await?;
        paginated(v, "history")
    }

    /// `GET livechat/visitors.chatHistory/room/{rid}/visitor/{vid}` (open and closed).
    pub async fn livechat_visitor_chat_history(
        &self,
        rid: &str,
        visitor_id: &str,
        page: Page,
    ) -> Result<Paginated<Room>> {
        let v: Value = self
            .get(
                &format!(
                    "livechat/visitors.chatHistory/room/{}/visitor/{}",
                    seg(rid),
                    seg(visitor_id)
                ),
                &page.apply(Query::new()),
            )
            .await?;
        paginated(v, "history")
    }

    /// `GET livechat/visitors.searchChats/room/{rid}/visitor/{vid}`.
    pub async fn livechat_visitor_search_chats(
        &self,
        rid: &str,
        visitor_id: &str,
        search: Option<&str>,
        closed_only: bool,
        served_only: bool,
        page: Page,
    ) -> Result<Paginated<Room>> {
        let v: Value = self
            .get(
                &format!(
                    "livechat/visitors.searchChats/room/{}/visitor/{}",
                    seg(rid),
                    seg(visitor_id)
                ),
                &page.apply(
                    Query::new()
                        .opt("searchText", search)
                        .push("closedChatsOnly", closed_only)
                        .push("servedChatsOnly", served_only),
                ),
            )
            .await?;
        paginated(v, "history")
    }

    /// `GET livechat/{rid}/messages`: normal messages and `livechat-close` of a
    /// (past) chat. `count` in the result echoes the page size.
    pub async fn livechat_room_messages(
        &self,
        rid: &str,
        search: Option<&str>,
        page: Page,
        oldest_first: bool,
    ) -> Result<Paginated<Message>> {
        let v: Value = self
            .get(
                &format!("livechat/{}/messages", seg(rid)),
                &page.apply(Query::new().opt("searchTerm", search).push(
                    "sort",
                    if oldest_first {
                        r#"{"ts":1}"#
                    } else {
                        r#"{"ts":-1}"#
                    },
                )),
            )
            .await?;
        paginated(v, "messages")
    }

    /// `GET livechat/rooms` (Contact Center chats).
    pub async fn livechat_rooms(&self, q: &LivechatRoomsQuery) -> Result<Paginated<Room>> {
        let mut query = Query::new();
        for a in &q.agents {
            query = query.push("agents[]", a);
        }
        for t in &q.tags {
            query = query.push("tags[]", t);
        }
        query = query
            .opt("roomName", q.room_name.as_deref())
            .opt("departmentId", q.department_id.as_deref())
            .opt("open", q.open)
            .opt("onhold", q.on_hold)
            .opt("queued", q.queued);
        let v: Value = self.get("livechat/rooms", &q.page.apply(query)).await?;
        paginated(v, "rooms")
    }

    // --- departments, agents, metadata --------------------------------------------

    /// `GET livechat/department`.
    pub async fn livechat_departments(&self, q: &DepartmentQuery) -> Result<Paginated<Department>> {
        let query = Query::new()
            .opt("text", q.text.as_deref())
            .push("onlyMyDepartments", q.only_my_departments)
            .push("enabled", q.enabled)
            .push("showArchived", q.show_archived)
            .opt("excludeDepartmentId", q.exclude_department_id.as_deref())
            .push("sort", r#"{"name":1}"#);
        let v: Value = self
            .get("livechat/department", &q.page.apply(query))
            .await?;
        paginated(v, "departments")
    }

    /// `GET livechat/department/{id}?includeAgents=false` (`None` when missing:
    /// the server answers `{"department": null}`).
    pub async fn livechat_department(&self, id: &str) -> Result<Option<Department>> {
        let mut v: Value = self
            .get(
                &format!("livechat/department/{}", seg(id)),
                &Query::new().push("includeAgents", false),
            )
            .await?;
        Ok(v.get_mut("department")
            .map(Value::take)
            .filter(|d| !d.is_null())
            .and_then(crate::models::lenient::try_convert))
    }

    /// `GET livechat/department.autocomplete`.
    pub async fn livechat_department_autocomplete(
        &self,
        term: &str,
        only_my_departments: bool,
    ) -> Result<Vec<DepartmentRef>> {
        let mut v: Value = self
            .get(
                "livechat/department.autocomplete",
                &Query::new()
                    .push("selector", json!({ "term": term }))
                    .push("onlyMyDepartments", only_my_departments),
            )
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get_mut("items").map(Value::take).unwrap_or_default(),
        ))
    }

    /// `GET livechat/users/agent` (allowed with `transfer-livechat-guest` or
    /// `edit-omnichannel-contact`).
    pub async fn livechat_agents(&self, q: &AgentQuery) -> Result<Paginated<LivechatAgent>> {
        let query = Query::new()
            .opt("text", q.text.as_deref())
            .push("onlyAvailable", q.only_available)
            .opt("excludeId", q.exclude_id.as_deref())
            .push("showIdleAgents", q.show_idle_agents)
            .push("sort", r#"{"name":1}"#);
        let v: Value = self
            .get("livechat/users/agent", &q.page.apply(query))
            .await?;
        paginated(v, "users")
    }

    /// `GET livechat/custom-fields`.
    pub async fn livechat_custom_fields(&self, page: Page) -> Result<Paginated<CustomFieldDef>> {
        let v: Value = self
            .get("livechat/custom-fields", &page.apply(Query::new()))
            .await?;
        paginated(v, "customFields")
    }

    /// `GET livechat/tags` (EE). The web client passes `view_all = department.is_none()`.
    pub async fn livechat_tags(
        &self,
        text: Option<&str>,
        department: Option<&str>,
        view_all: bool,
    ) -> Result<Paginated<Tag>> {
        let v: Value = self
            .get(
                "livechat/tags",
                &Query::new()
                    .opt("text", text)
                    .opt("department", department)
                    .push("viewAll", view_all),
            )
            .await?;
        paginated(v, "tags")
    }

    /// `GET livechat/priorities` (EE), sorted by `sortItem`.
    pub async fn livechat_priorities(&self) -> Result<Paginated<Priority>> {
        let v: Value = self
            .get(
                "livechat/priorities",
                &Query::new().push("sort", r#"{"sortItem":1}"#),
            )
            .await?;
        paginated(v, "priorities")
    }

    /// `GET livechat/sla` (EE).
    pub async fn livechat_sla_policies(&self) -> Result<Paginated<Sla>> {
        let v: Value = self.get("livechat/sla", &Query::new()).await?;
        paginated(v, "sla")
    }

    // --- canned responses (EE) -----------------------------------------------------

    /// `GET canned-responses.get`: every response the user may use.
    pub async fn canned_responses_all(&self) -> Result<Vec<CannedResponse>> {
        let mut v: Value = self.get("canned-responses.get", &Query::new()).await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get_mut("responses").map(Value::take).unwrap_or_default(),
        ))
    }

    /// `GET canned-responses` (paginated, filtered).
    pub async fn canned_responses(
        &self,
        q: &CannedResponseQuery,
    ) -> Result<Paginated<CannedResponse>> {
        let query = Query::new()
            .opt("text", q.text.as_deref())
            .opt("scope", q.scope.map(|s| s.as_str()))
            .opt("departmentId", q.department_id.as_deref());
        let v: Value = self.get("canned-responses", &q.page.apply(query)).await?;
        paginated(v, "cannedResponses")
    }

    /// `GET canned-responses/{id}`.
    pub async fn canned_response(&self, id: &str) -> Result<CannedResponse> {
        let v: Value = self
            .get(&format!("canned-responses/{}", seg(id)), &Query::new())
            .await?;
        field(v, "cannedResponse")
    }

    /// `POST canned-responses` (create or update).
    pub async fn save_canned_response(&self, response: &SaveCannedResponse) -> Result<()> {
        self.post_ok("canned-responses", response).await
    }

    /// `DELETE canned-responses/{id}`.
    pub async fn delete_canned_response(&self, id: &str) -> Result<()> {
        let _: Value = self
            .delete(&format!("canned-responses/{}", seg(id)), &Query::new())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn api(error: Option<&str>, error_type: Option<&str>) -> Error {
        Error::Api {
            status: 400,
            error: error.map(str::to_owned),
            error_type: error_type.map(str::to_owned),
            message: None,
            details: None,
        }
    }

    #[test]
    fn error_codes() {
        let e = api(
            Some("This is an enterprise feature [error-action-not-allowed]"),
            Some("error-action-not-allowed"),
        );
        assert!(e.is_enterprise_only());
        let e = api(Some("error-comment-is-required"), None);
        assert_eq!(e.livechat_error_code(), Some("error-comment-is-required"));
        assert!(!e.is_enterprise_only());
        let e = api(Some("Inquiry already taken [error-inquiry-taken]"), None);
        assert_eq!(e.livechat_error_code(), Some("error-inquiry-taken"));
        let e = api(Some("Agent not found"), None);
        assert_eq!(e.livechat_error_code(), Some("Agent not found"));
        assert_eq!(Error::Protocol("x".into()).livechat_error_code(), None);
    }

    #[test]
    fn forward_queries() {
        let d = DepartmentQuery::for_forward(None, Some("cur"));
        assert!(d.enabled);
        assert_eq!(d.exclude_department_id.as_deref(), Some("cur"));
        let a = AgentQuery::for_forward(Some("x".into()), Some("me"));
        assert!(a.only_available && !a.show_idle_agents);
    }
}
