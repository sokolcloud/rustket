//! E-mail inboxes (docs/admin.md §16). SMTP/IMAP passwords are secrets.

use serde::Deserialize;
use serde_json::Value;

use super::Page;
use crate::error::Result;
use crate::models::Paginated;
use crate::models::admin::EmailInbox;
use crate::rest::{Query, RestClient, paginated};

#[derive(Deserialize, Default)]
#[serde(default)]
struct IdResult {
    #[serde(rename = "_id")]
    id: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct SearchResult {
    email_inbox: Option<EmailInbox>,
}

impl RestClient {
    pub async fn email_inbox_list(&self, page: &Page) -> Result<Paginated<EmailInbox>> {
        let v: Value = self
            .get("email-inbox.list", &page.apply(Query::new()))
            .await?;
        paginated(v, "emailInboxes")
    }

    /// Inbox fields at the top level; a missing id answers HTTP 404.
    pub async fn email_inbox_get(&self, id: &str) -> Result<EmailInbox> {
        self.get(
            &format!("email-inbox/{}", crate::server_url::encode_segment(id)),
            &Query::new(),
        )
        .await
    }

    /// Creates (`inbox.id == None`) or updates an inbox; returns its id.
    pub async fn email_inbox_save(&self, inbox: &EmailInbox) -> Result<String> {
        let r: IdResult = self.post("email-inbox", inbox).await?;
        Ok(r.id)
    }

    pub async fn email_inbox_delete(&self, id: &str) -> Result<()> {
        let _: Value = self
            .delete(
                &format!("email-inbox/{}", crate::server_url::encode_segment(id)),
                &Query::new(),
            )
            .await?;
        Ok(())
    }

    /// The inbox using `email`, if any (duplicate check).
    pub async fn email_inbox_search(&self, email: &str) -> Result<Option<EmailInbox>> {
        let r: SearchResult = self
            .get("email-inbox.search", &Query::new().push("email", email))
            .await?;
        Ok(r.email_inbox)
    }

    /// Sends a test mail through the inbox's SMTP.
    pub async fn email_inbox_send_test(&self, id: &str) -> Result<()> {
        let _: Value = self
            .post(
                &format!(
                    "email-inbox.send-test/{}",
                    crate::server_url::encode_segment(id)
                ),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }
}
