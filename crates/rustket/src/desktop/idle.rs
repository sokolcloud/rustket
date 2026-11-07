//! Idle detection (set "away" after inactivity). STUB.

use iced::{Subscription, Task};

use super::Request;

#[derive(Debug, Clone)]
pub enum Message {
    Idle(bool),
}

#[derive(Debug, Default)]
pub struct Idle;

impl Idle {
    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>) {
        match msg {
            Message::Idle(idle) => (Task::none(), vec![Request::Idle(idle)]),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }
}
