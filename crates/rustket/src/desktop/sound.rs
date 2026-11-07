//! Notification sounds (`pw-play`/`paplay`). STUB.

#[derive(Debug, Default)]
pub struct Sound;

impl Sound {
    /// Plays the notification sound named by the server (`audioNotificationValue`).
    pub fn play(&self, name: Option<&str>) {
        let _ = name;
    }
}
