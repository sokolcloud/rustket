//! macOS backend: `NSUserNotification` through `mac-notification-sys`.
//!
//! Posted as [`crate::BUNDLE_ID`] when running from the app bundle, as
//! Terminal otherwise (development builds). The library waits for the
//! click or dismissal on the calling thread, so every notification gets a
//! short-lived thread of its own that reports the response through
//! [`super::native`]; at most [`MAX_WAITING`] wait at a time, later ones are
//! posted without waiting (not clickable).
//!
//! Limits: delivered notifications can be neither replaced nor removed. A
//! notification replacing another reuses its id (a click on any of a room's
//! notifications opens the room) and `close` only forgets the id, so a later
//! dismissal is not reported.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};

use futures::FutureExt;
use futures::future::BoxFuture;
use mac_notification_sys::{MainButton, Notification, NotificationResponse};

use super::{ACTION_DEFAULT, Event, Notifier, Outgoing, Signal, native, text};

/// Notifications waiting for a response at the same time.
const MAX_WAITING: usize = 16;

fn ensure_application() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        let app = std::env::current_exe()
            .map(|exe| text::mac_application(&exe))
            .unwrap_or("com.apple.Terminal");
        if let Err(e) = mac_notification_sys::set_application(app) {
            tracing::debug!(error = %e, "cannot set the notification application");
        }
    });
}

#[derive(Default)]
struct Inner {
    next_id: AtomicU32,
    generation: AtomicU64,
    /// Latest showing of every id that is not closed.
    current: Mutex<HashMap<u32, u64>>,
    waiting: AtomicUsize,
}

impl Inner {
    fn is_current(&self, id: u32, generation: u64) -> bool {
        self.current
            .lock()
            .is_ok_and(|c| c.get(&id) == Some(&generation))
    }

    fn forget(&self, id: u32) {
        if let Ok(mut c) = self.current.lock() {
            c.remove(&id);
        }
    }

    /// Posts `n` and reports the response (runs on its own thread).
    fn post(&self, id: u32, generation: u64, n: &Outgoing, wait: bool) {
        ensure_application();
        let image = n
            .image
            .as_ref()
            .and_then(|(w, h, px)| native::image_file(*w, *h, px))
            .map(|p| p.to_string_lossy().into_owned());
        let buttons: Vec<&str> = n
            .actions
            .iter()
            .filter(|(k, _)| k != ACTION_DEFAULT)
            .map(|(_, l)| l.as_str())
            .collect();
        let mut note = Notification::new();
        note.title(&n.title).message(&n.body);
        if n.sound {
            note.default_sound();
        }
        if let Some(img) = &image {
            note.content_image(img);
        }
        match buttons.as_slice() {
            [] => {}
            [one] => {
                note.main_button(MainButton::SingleAction(one));
            }
            [first, ..] => {
                note.main_button(MainButton::DropdownActions(first, &buttons));
            }
        }
        if wait {
            note.wait_for_click(true);
        } else {
            note.asynchronous(true);
        }
        let response = note.send();
        let signal = match response {
            Ok(NotificationResponse::Click | NotificationResponse::Reply(_)) => {
                Some(Signal::Action(ACTION_DEFAULT.to_owned()))
            }
            Ok(NotificationResponse::ActionButton(label)) => {
                let key = n
                    .actions
                    .iter()
                    .find(|(_, l)| *l == label)
                    .map_or(ACTION_DEFAULT, |(k, _)| k.as_str());
                Some(Signal::Action(key.to_owned()))
            }
            Ok(NotificationResponse::CloseButton(_)) => Some(Signal::Closed),
            Ok(NotificationResponse::None) => wait.then_some(Signal::Closed),
            Err(e) => {
                tracing::debug!(error = %e, "notification not shown");
                Some(Signal::Closed)
            }
        };
        match signal {
            Some(Signal::Closed) => {
                if self.is_current(id, generation) {
                    self.forget(id);
                    native::emit(id, Signal::Closed);
                }
            }
            Some(action) => {
                if self.is_current(id, generation) {
                    self.forget(id);
                }
                native::emit(id, action);
            }
            None => {}
        }
    }
}

/// `NSUserNotification`s.
pub struct MacNotifier {
    inner: Arc<Inner>,
}

impl MacNotifier {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner::default()),
        }
    }
}

impl Notifier for MacNotifier {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event> {
        let inner = Arc::clone(&self.inner);
        let id = n
            .replaces
            .unwrap_or_else(|| inner.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let generation = inner.generation.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = inner.current.lock() {
            c.insert(id, generation);
        }
        let wait = inner.waiting.fetch_add(1, Ordering::Relaxed) < MAX_WAITING;
        if !wait {
            inner.waiting.fetch_sub(1, Ordering::Relaxed);
        }
        let server_sound = n.sound;
        let thread_inner = Arc::clone(&inner);
        let spawned = std::thread::Builder::new()
            .name("notification".into())
            .spawn(move || {
                thread_inner.post(id, generation, &n, wait);
                if wait {
                    thread_inner.waiting.fetch_sub(1, Ordering::Relaxed);
                }
            });
        let event = match spawned {
            Ok(_) => Event::Shown { id, server_sound },
            Err(e) => {
                if wait {
                    inner.waiting.fetch_sub(1, Ordering::Relaxed);
                }
                inner.forget(id);
                Event::Failed(e.to_string())
            }
        };
        async move { event }.boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        self.inner.forget(id);
        async {}.boxed()
    }

    fn listens(&self) -> bool {
        true
    }
}
