//! Window operations on the (single) main window: show/raise, hide to the
//! tray, minimize.

use iced::Task;
use iced::window::{self, Mode};

/// Shows, un-minimizes and focuses the window.
pub fn show<T: Send + 'static>() -> Task<T> {
    window::latest().then(|id| match id {
        Some(id) => window::set_mode(id, Mode::Windowed)
            .chain(window::minimize(id, false))
            .chain(window::gain_focus(id)),
        None => Task::none(),
    })
}

/// Hides the window (it lives on in the tray).
pub fn hide<T: Send + 'static>() -> Task<T> {
    window::latest().then(|id| match id {
        Some(id) => window::set_mode(id, Mode::Hidden),
        None => Task::none(),
    })
}

/// Minimizes the window (start minimized without a tray).
pub fn minimize<T: Send + 'static>() -> Task<T> {
    window::latest().then(|id| match id {
        Some(id) => window::minimize(id, true),
        None => Task::none(),
    })
}

/// Hides the window when it was just minimized (minimize to tray). The
/// output says whether it was hidden.
pub fn hide_if_minimized() -> Task<bool> {
    window::latest().then(|id| match id {
        Some(id) => window::is_minimized(id).then(move |m| {
            if m == Some(true) {
                window::set_mode::<bool>(id, Mode::Hidden).chain(Task::done(true))
            } else {
                Task::done(false)
            }
        }),
        None => Task::done(false),
    })
}
