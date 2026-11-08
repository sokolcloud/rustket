//! Mailer (docs/admin.md §12).

use crate::error::Result;
use crate::models::admin::MailerRequest;
use crate::rest::RestClient;

impl RestClient {
    /// `POST mailer`. A dry run sends one mail to the user whose address equals
    /// `from`.
    pub async fn mailer_send(&self, req: &MailerRequest) -> Result<()> {
        self.post_ok("mailer", req).await
    }
}
