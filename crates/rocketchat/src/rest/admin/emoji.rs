//! Custom emoji administration (docs/admin.md §7).

use serde_json::{Value, json};

use super::{AdminUpload, Page, paged};
use crate::error::Result;
use crate::models::{CustomEmoji, Paginated};
use crate::rest::{Query, RestClient};

impl RestClient {
    /// `emoji-custom.all` (`name`: case-insensitive substring).
    pub async fn emoji_custom_all(
        &self,
        name: &str,
        page: &Page,
    ) -> Result<Paginated<CustomEmoji>> {
        let mut q = page.apply(Query::new());
        if !name.is_empty() {
            q = q.push("name", name);
        }
        let v: Value = self.get("emoji-custom.all", &q).await?;
        paged(v, "emojis", page)
    }

    /// `emoji-custom.create` (multipart). `aliases` is comma-separated. The stored
    /// extension comes from the image's MIME type.
    pub async fn emoji_custom_create(
        &self,
        image: AdminUpload,
        name: &str,
        aliases: &str,
    ) -> Result<()> {
        self.upload_form(
            "emoji-custom.create",
            vec![("name", name.to_owned()), ("aliases", aliases.to_owned())],
            Some(("emoji", image)),
            None,
        )
        .await?;
        Ok(())
    }

    /// `emoji-custom.update` (multipart); `image: None` keeps the picture.
    pub async fn emoji_custom_update(
        &self,
        id: &str,
        name: &str,
        aliases: &str,
        image: Option<AdminUpload>,
    ) -> Result<()> {
        self.upload_form(
            "emoji-custom.update",
            vec![
                ("_id", id.to_owned()),
                ("name", name.to_owned()),
                ("aliases", aliases.to_owned()),
            ],
            image.map(|i| ("emoji", i)),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn emoji_custom_delete(&self, id: &str) -> Result<()> {
        self.post_ok("emoji-custom.delete", &json!({ "emojiId": id }))
            .await
    }
}
