//! Freedesktop notifications (notify-rust over zbus) honouring server and
//! local preferences (`rustket_core::notify`). STUB: nothing is shown.

use iced::Task;
use rocketchat::session::DesktopNotification;
use rustket_core::config::UiPrefs;
use rustket_core::{ServerId, Workspace};

use super::Request;

#[derive(Debug, Clone)]
pub enum Message {
    /// A notification action was clicked.
    Activated { server_id: ServerId, rid: String },
}

#[derive(Debug, Default)]
pub struct Notifications;

impl Notifications {
    pub fn notify(
        &mut self,
        server_id: &ServerId,
        server_name: &str,
        notification: &DesktopNotification,
        workspace: &Workspace,
        prefs: &UiPrefs,
        focused: bool,
    ) -> Task<super::Message> {
        let _ = (
            server_id,
            server_name,
            notification,
            workspace,
            prefs,
            focused,
        );
        Task::none()
    }

    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>) {
        match msg {
            Message::Activated { server_id, rid } => {
                (Task::none(), vec![Request::OpenRoom { server_id, rid }])
            }
        }
    }
}
