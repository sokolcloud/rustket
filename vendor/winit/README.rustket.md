# Vendored winit 0.30.13 (Rustket patch)

This is winit 0.30.13 exactly as published on crates.io (`Cargo.toml`, `LICENSE`
and sources as shipped), with one patch. The root `Cargo.toml` wires it in for
every build and keeps it out of the workspace:

```toml
[workspace]
exclude = ["vendor/winit"]

[patch.crates-io]
winit = { path = "vendor/winit" }
```

## Why

winit's macOS backend links two private CoreGraphics Services functions,
`CGSMainConnectionID` and `CGSSetWindowBackgroundBlurRadius`, for
`WindowAttributes::with_blur`. The Mac App Store rejects binaries that reference
private symbols (guideline 2.5.1, ITMS-90338); iced/winit apps were rejected
for exactly these two in October 2026. Rustket never enables blur, so the
patch removes the declarations and makes `set_blur` a no-op. The CI gate in
`packaging/macos/build.sh` (`nm -u`, both slices) fails any MAS build that
references a `_CGS*`/`_SLS*` symbol other than the public
`_CGShieldingWindowLevel`.

## The diff

```diff
--- winit-0.30.13/src/platform_impl/macos/ffi.rs
+++ vendor/winit/src/platform_impl/macos/ffi.rs
@@ -10,8 +10,6 @@
 use core_foundation::uuid::CFUUIDRef;
 use core_graphics::base::CGError;
 use core_graphics::display::{CGDirectDisplayID, CGDisplayConfigRef};
-use objc2::ffi::NSInteger;
-use objc2::runtime::AnyObject;
 
 pub type CGDisplayFadeInterval = f32;
 pub type CGDisplayReservationInterval = f32;
@@ -116,14 +114,6 @@
     pub fn CGDisplayModeCopyPixelEncoding(mode: CGDisplayModeRef) -> CFStringRef;
     pub fn CGDisplayModeRetain(mode: CGDisplayModeRef);
     pub fn CGDisplayModeRelease(mode: CGDisplayModeRef);
-
-    // Wildly used private APIs; Apple uses them for their Terminal.app.
-    pub fn CGSMainConnectionID() -> *mut AnyObject;
-    pub fn CGSSetWindowBackgroundBlurRadius(
-        connection_id: *mut AnyObject,
-        window_id: NSInteger,
-        radius: i64,
-    ) -> i32;
 }
 
 mod core_video {
--- winit-0.30.13/src/platform_impl/macos/window_delegate.rs
+++ vendor/winit/src/platform_impl/macos/window_delegate.rs
@@ -887,19 +887,10 @@
         self.window().setBackgroundColor(Some(&color));
     }
 
-    pub fn set_blur(&self, blur: bool) {
-        // NOTE: in general we want to specify the blur radius, but the choice of 80
-        // should be a reasonable default.
-        let radius = if blur { 80 } else { 0 };
-        let window_number = unsafe { self.window().windowNumber() };
-        unsafe {
-            ffi::CGSSetWindowBackgroundBlurRadius(
-                ffi::CGSMainConnectionID(),
-                window_number,
-                radius,
-            );
-        }
-    }
+    // Rustket patch: the blur used the private CGSSetWindowBackgroundBlurRadius,
+    // which the Mac App Store rejects (guideline 2.5.1). Rustket never enables
+    // blur, so this is a no-op (vendor/winit/README.rustket.md).
+    pub fn set_blur(&self, _blur: bool) {}
 
     pub fn set_visible(&self, visible: bool) {
         match visible {
```

Only `.cargo-ok` (a registry extraction marker) was left out of the copy.

## Known undocumented API left in place

These are not linked symbols, so `nm` does not see them, and no known rejection
names them. If App Store processing flags one, map the cursors to documented
`NSCursor` class methods, and re-test keyboard handling before dropping the
override:

- `src/platform_impl/macos/cursor.rs`: undocumented `NSCursor` class selectors
  `_helpCursor`, `_zoomInCursor`, `_zoomOutCursor`,
  `_windowResize{NorthEast,NorthWest,SouthEast,SouthWest,NorthEastSouthWest,NorthWestSouthEast}Cursor`
  and `busyButClickableCursor` (sent through `performSelector:` after a
  `respondsToSelector:` check), plus `load_webkit_cursor` reading cursor PDFs
  from `HIServices.framework/Versions/A/Resources/cursors` (`move`, `cell`).
- `src/platform_impl/macos/view.rs`: the `_wantsKeyDownForEvent:` override on
  winit's `NSView` subclass.

## Updating or dropping it

- Drop the vendor directory and the `[patch.crates-io]` entry as soon as iced
  depends on a winit release without these symbols (grep that release's macOS
  backend for `CGS`). Then confirm with the `nm -u` gate on a MAS build.
- When iced moves to another winit 0.30.x, re-copy that release from
  `~/.cargo/registry/src/*/winit-<version>`, re-apply the diff above, update the
  version here and run `cargo tree -i winit` (it must print the
  `vendor/winit` path). `cargo update` keeps the path source as long as the
  versions match; a version iced requires that the vendored copy does not
  satisfy makes Cargo ignore the patch with a warning, and the `nm` gate then
  fails the MAS build.
- The vendored crate keeps its own `unsafe` code; it is not a workspace member,
  so `cargo fmt --all` and `cargo clippy --workspace` skip it. Rustket's crates
  get no new `unsafe` from it.
