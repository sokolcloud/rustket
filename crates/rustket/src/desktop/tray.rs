//! System tray (StatusNotifierItem via ksni) with the unread badge and a menu.
//! STUB: keeps the counters only.

use iced::{Subscription, Task};

use super::Request;

#[derive(Debug, Clone)]
pub enum Message {
    Show,
    Quit,
}

#[derive(Debug, Default)]
pub struct Tray {
    pub unread: u64,
    pub mentions: u64,
}

impl Tray {
    pub fn set_unread(&mut self, unread: u64, mentions: u64) {
        self.unread = unread;
        self.mentions = mentions;
    }

    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>) {
        match msg {
            Message::Show => (Task::none(), vec![Request::ShowWindow]),
            Message::Quit => (Task::none(), vec![Request::Quit]),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }
}
