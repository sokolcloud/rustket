//! Launch at login through `SMAppService.mainApp` (ServiceManagement, macOS
//! 13+), the one way a sandboxed app can register itself. There are no
//! Rust bindings in the dependency tree, so the four messages are sent with
//! `msg_send!`; this module is one of the few allowed to use `unsafe`.
#![allow(unsafe_code)]

use std::io;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{available, msg_send};
use objc2_foundation::NSError;

use super::{State, login_item_state};

// SAFETY: declares no items; it only links the framework that defines
// `SMAppService` (looked up at runtime with `AnyClass::get`).
#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

/// Whether `SMAppService` exists (macOS 13 and later).
pub fn supported() -> bool {
    available!(macos = 13.0) && AnyClass::get(c"SMAppService").is_some()
}

/// `[SMAppService mainAppService]`.
fn main_app() -> Option<Retained<AnyObject>> {
    if !available!(macos = 13.0) {
        return None;
    }
    let class = AnyClass::get(c"SMAppService")?;
    // SAFETY: `+mainAppService` takes no arguments and returns an
    // autoreleased `SMAppService *` (never nil per the SDK headers; an
    // `Option` covers it anyway); `msg_send!` retains it.
    unsafe { msg_send![class, mainAppService] }
}

pub fn state() -> State {
    let Some(service) = main_app() else {
        return State::Off;
    };
    // SAFETY: `-status` takes no arguments and returns `SMAppServiceStatus`,
    // an `NSInteger` (`isize`).
    let status: isize = unsafe { msg_send![&*service, status] };
    login_item_state(status)
}

pub fn set_enabled(enabled: bool) -> io::Result<()> {
    let service = main_app().ok_or_else(|| io::Error::other("SMAppService is unavailable"))?;
    let current = state();
    if enabled == current.is_on() {
        return Ok(());
    }
    let result: Result<(), Retained<NSError>> = if enabled {
        // SAFETY: `-registerAndReturnError:` returns `BOOL` and takes an
        // `NSError **` out-parameter, which `_` makes `msg_send!` handle.
        unsafe { msg_send![&*service, registerAndReturnError: _] }
    } else {
        // SAFETY: as above, for `-unregisterAndReturnError:`.
        unsafe { msg_send![&*service, unregisterAndReturnError: _] }
    };
    result.map_err(|e| io::Error::other(format!("{} ({})", e.localizedDescription(), e.code())))
}
