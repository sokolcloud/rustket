//! Custom sounds (docs/admin.md §8).

use serde::Deserialize;
use serde_json::{Value, json};

use super::{AdminUpload, Page};
use crate::error::Result;
use crate::models::Paginated;
use crate::models::admin::CustomSound;
use crate::rest::{Query, RestClient, field, paginated};

/// Maximum size of a custom sound (5 MiB).
pub const MAX_SOUND_BYTES: u64 = 5_242_880;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Created {
    sound: Option<IdOnly>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct IdOnly {
    #[serde(rename = "_id")]
    id: String,
}

impl RestClient {
    pub async fn custom_sounds_list(
        &self,
        name: &str,
        page: &Page,
    ) -> Result<Paginated<CustomSound>> {
        let mut q = page.apply(Query::new());
        if !name.is_empty() {
            q = q.push("name", name);
        }
        let v: Value = self.get("custom-sounds.list", &q).await?;
        paginated(v, "sounds")
    }

    pub async fn custom_sound_get(&self, id: &str) -> Result<CustomSound> {
        let v: Value = self
            .get("custom-sounds.getOne", &Query::new().push("_id", id))
            .await?;
        field(v, "sound")
    }

    /// `custom-sounds.create` (multipart). The MIME type is derived from the file
    /// name's extension (mp3/wav). Returns the new id.
    pub async fn custom_sound_create(&self, file: AdminUpload, name: &str) -> Result<String> {
        let v = self
            .upload_form(
                "custom-sounds.create",
                vec![("name", name.to_owned())],
                Some(("sound", file)),
                None,
            )
            .await?;
        let c: Created = serde_json::from_value(v)?;
        Ok(c.sound.map(|s| s.id).unwrap_or_default())
    }

    /// `custom-sounds.update` (multipart); `file: None` keeps the sound.
    pub async fn custom_sound_update(
        &self,
        id: &str,
        name: &str,
        file: Option<AdminUpload>,
    ) -> Result<()> {
        self.upload_form(
            "custom-sounds.update",
            vec![("_id", id.to_owned()), ("name", name.to_owned())],
            file.map(|f| ("sound", f)),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn custom_sound_delete(&self, id: &str) -> Result<()> {
        self.post_ok("custom-sounds.delete", &json!({ "_id": id }))
            .await
    }
}
