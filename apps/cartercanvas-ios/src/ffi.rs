//! The C ABI the Swift host calls (`include/cartercanvas.h`). Every function runs on the main
//! thread, takes the handle `cc_create` returned, and catches panics so none unwinds into Swift.
//! The pointer contracts (valid handle, live buffers of the stated length) are the header's.
#![allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use eframe::wgpu;
use egui::{Event, Modifiers};

use crate::callbacks::{Callbacks, CcHostCallbacks, DialogKind, ImageSink};
use crate::host::{Host, Viewport};
use crate::input::CcTouch;
use crate::services::Dirs;
use crate::{keys, logging};

/// The app behind the host's handle.
pub struct CcApp {
    host: Host,
}

/// `cc_frame` result bits.
pub const CC_FRAME_DREW: u32 = 1;
pub const CC_FRAME_WANTS_KEYBOARD: u32 = 2;

fn guard<R>(name: &str, fallback: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload.downcast_ref::<&str>().map(|s| (*s).to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default();
            log::error!("{name} panicked: {msg}");
            fallback
        }
    }
}

/// A host string argument; null or invalid UTF-8 is `None`.
fn string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the host passes a NUL-terminated string that lives for the call.
    unsafe { CStr::from_ptr(p) }.to_str().ok().map(str::to_owned)
}

fn with_app<R>(app: *mut CcApp, name: &str, fallback: R, f: impl FnOnce(&mut Host) -> R) -> R {
    if app.is_null() {
        return fallback;
    }
    // SAFETY: `app` came from `cc_create` and hasn't been destroyed; calls come from one thread.
    let app = unsafe { &mut *app };
    guard(name, fallback, || f(&mut app.host))
}

/// A wgpu surface on the host's `CAMetalLayer`.
pub(crate) fn surface_from_layer(instance: &wgpu::Instance, layer: *mut c_void) -> Result<wgpu::Surface<'static>, String> {
    if layer.is_null() {
        return Err("no Metal layer".into());
    }
    // SAFETY: `layer` is a live `CAMetalLayer` owned by the host's view, which outlives the host
    // (`cc_destroy` runs before the view goes away).
    unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer)) }.map_err(|e| e.to_string())
}

/// The control server's port and token from the launch environment (`CARTERCANVAS_CONTROL_PORT`,
/// `CARTERCANVAS_CONTROL_TOKEN`; `simctl launch` passes them as `SIMCTL_CHILD_…`).
fn control_from_env() -> Option<(u16, String)> {
    let port = std::env::var("CARTERCANVAS_CONTROL_PORT").ok()?.parse::<u16>().ok().filter(|p| *p != 0)?;
    let Some(token) = std::env::var("CARTERCANVAS_CONTROL_TOKEN").ok().filter(|t| !t.is_empty()) else {
        log::error!("control server off: CARTERCANVAS_CONTROL_PORT is set but CARTERCANVAS_CONTROL_TOKEN isn't");
        return None;
    };
    if let Err(e) = photocraft_automation::security::validate_token(&token) {
        log::error!("control server off: {e}");
        return None;
    }
    Some((port, token.to_ascii_lowercase()))
}

/// Start the app on `layer` (a `CAMetalLayer`, unretained). Returns null on failure (the reason
/// is in the log).
#[unsafe(no_mangle)]
pub extern "C" fn cc_create(
    layer: *mut c_void,
    width_px: u32,
    height_px: u32,
    scale: f32,
    config_dir: *const c_char,
    documents_dir: *const c_char,
    callbacks: *const CcHostCallbacks,
) -> *mut CcApp {
    let (Some(config), Some(documents)) = (string(config_dir), string(documents_dir)) else { return std::ptr::null_mut() };
    if callbacks.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host passes a valid table; it's copied here.
    let table = unsafe { *callbacks };
    guard("cc_create", std::ptr::null_mut(), || {
        let dirs = Dirs { config: PathBuf::from(config), documents: PathBuf::from(documents) };
        logging::install(&dirs.config);
        log::info!("CarterCanvas {} starting: {width_px}×{height_px} px @{scale}x", env!("CARGO_PKG_VERSION"));
        let viewport = Viewport { width_px, height_px, scale, insets: [0.0; 4] };
        match Host::new(layer, viewport, dirs, Callbacks::new(table), control_from_env()) {
            Ok(host) => Box::into_raw(Box::new(CcApp { host })),
            Err(e) => {
                log::error!("CarterCanvas couldn't start: {e}");
                std::ptr::null_mut()
            }
        }
    })
}

/// Save the tool memory and free the app. `app` is invalid afterwards.
#[unsafe(no_mangle)]
pub extern "C" fn cc_destroy(app: *mut CcApp) {
    if app.is_null() {
        return;
    }
    // SAFETY: `app` came from `cc_create` and is destroyed once.
    let mut app = unsafe { Box::from_raw(app) };
    guard("cc_destroy", (), || app.host.shutdown());
    drop(app);
}

/// The view changed size, scale or safe area (insets in points: top, left, bottom, right).
#[unsafe(no_mangle)]
pub extern "C" fn cc_resize(app: *mut CcApp, width_px: u32, height_px: u32, scale: f32, top: f32, left: f32, bottom: f32, right: f32) {
    with_app(app, "cc_resize", (), |h| h.resize(Viewport { width_px, height_px, scale, insets: [top, left, bottom, right] }));
}

/// A display-link tick (`CACurrentMediaTime()` seconds). Returns `CC_FRAME_*` bits.
#[unsafe(no_mangle)]
pub extern "C" fn cc_frame(app: *mut CcApp, timestamp: f64) -> u32 {
    with_app(app, "cc_frame", 0, |h| {
        let r = h.frame(timestamp);
        (u32::from(r.drew) * CC_FRAME_DREW) | (u32::from(r.wants_keyboard) * CC_FRAME_WANTS_KEYBOARD)
    })
}

/// Touch and Apple Pencil samples (coalesced samples in order), in points.
#[unsafe(no_mangle)]
pub extern "C" fn cc_touches(app: *mut CcApp, touches: *const CcTouch, count: usize) {
    if touches.is_null() || count == 0 {
        return;
    }
    // SAFETY: the host passes `count` samples that live for the call.
    let touches = unsafe { std::slice::from_raw_parts(touches, count) };
    with_app(app, "cc_touches", (), |h| h.input().touches(touches));
}

/// A trackpad/mouse pointer or a hovering Pencil at (x, y) points; `active` false when it left.
#[unsafe(no_mangle)]
pub extern "C" fn cc_hover(app: *mut CcApp, x: f32, y: f32, active: bool) {
    with_app(app, "cc_hover", (), |h| h.input().hover(x, y, active));
}

/// A trackpad/mouse button (0 primary, 1 secondary, 2 middle).
#[unsafe(no_mangle)]
pub extern "C" fn cc_pointer_button(app: *mut CcApp, x: f32, y: f32, button: u32, pressed: bool) {
    with_app(app, "cc_pointer_button", (), |h| h.input().pointer_button(x, y, button, pressed));
}

/// Trackpad or mouse-wheel scrolling, in points.
#[unsafe(no_mangle)]
pub extern "C" fn cc_scroll(app: *mut CcApp, dx: f32, dy: f32) {
    with_app(app, "cc_scroll", (), |h| h.input().scroll(dx, dy));
}

/// A trackpad pinch around (x, y): `factor` is this step's scale change (1 = none).
#[unsafe(no_mangle)]
pub extern "C" fn cc_pinch(app: *mut CcApp, x: f32, y: f32, factor: f32) {
    with_app(app, "cc_pinch", (), |h| h.input().pinch(x, y, factor));
}

/// The held modifiers changed (`UIKeyModifierFlags`).
#[unsafe(no_mangle)]
pub extern "C" fn cc_modifiers(app: *mut CcApp, flags: u32) {
    with_app(app, "cc_modifiers", (), |h| h.input().set_modifiers(keys::modifiers(flags)));
}

/// A hardware key (`UIKeyboardHIDUsage`) went down or up; `text` is what it types (may be null).
/// Returns whether the app used it, so the host passes unknown keys on to UIKit.
#[unsafe(no_mangle)]
pub extern "C" fn cc_key(app: *mut CcApp, usage: u32, pressed: bool, flags: u32, repeat: bool, text: *const c_char) -> bool {
    let text = string(text);
    with_app(app, "cc_key", false, |h| {
        let modifiers = keys::modifiers(flags);
        let input = h.input();
        input.set_modifiers(modifiers);
        let key = keys::key(usage);
        if let Some(key) = key {
            input.push(Event::Key { key, physical_key: Some(key), pressed, repeat, modifiers });
        }
        let typed = pressed && text.as_deref().is_some_and(|t| keys::types_text(flags, t));
        if typed && let Some(t) = text {
            input.push(Event::Text(t));
        }
        key.is_some() || typed
    })
}

/// Text from the on-screen keyboard, dictation or a hardware keyboard's composed input.
#[unsafe(no_mangle)]
pub extern "C" fn cc_text(app: *mut CcApp, text: *const c_char) {
    let Some(text) = string(text) else { return };
    with_app(app, "cc_text", (), |h| {
        let input = h.input();
        // Return on the on-screen keyboard is a key, not a newline character.
        if text == "\n" {
            for pressed in [true, false] {
                input.push(Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed, repeat: false, modifiers: Modifiers::NONE });
            }
        } else if !text.is_empty() {
            input.push(Event::Text(text));
        }
    });
}

/// The on-screen keyboard's delete key.
#[unsafe(no_mangle)]
pub extern "C" fn cc_delete_backward(app: *mut CcApp) {
    with_app(app, "cc_delete_backward", (), |h| {
        let input = h.input();
        for pressed in [true, false] {
            input.push(Event::Key { key: egui::Key::Backspace, physical_key: Some(egui::Key::Backspace), pressed, repeat: false, modifiers: Modifiers::NONE });
        }
    });
}

/// The pasteboard's text, answering `request_paste` (or the Edit menu's Paste in a text field).
#[unsafe(no_mangle)]
pub extern "C" fn cc_paste(app: *mut CcApp, text: *const c_char) {
    let Some(text) = string(text) else { return };
    with_app(app, "cc_paste", (), |h| h.input().push(Event::Paste(text)));
}

/// The host's answer to file dialog `id`: a JSON array of paths, or null for Cancel.
#[unsafe(no_mangle)]
pub extern "C" fn cc_file_dialog_answer(app: *mut CcApp, id: u64, kind: u32, paths_json: *const c_char) {
    let kind = match kind {
        0 => DialogKind::OpenOne,
        1 => DialogKind::OpenMany,
        2 => DialogKind::Folder,
        _ => DialogKind::Save,
    };
    let json = string(paths_json);
    with_app(app, "cc_file_dialog_answer", (), |h| match json.as_deref().map(serde_json::from_str::<Vec<String>>) {
        None => h.answer_dialog(id, kind, None),
        Some(Ok(paths)) => h.answer_dialog(id, kind, Some(paths)),
        Some(Err(e)) => h.dialog_failed(id, format!("the file picker's answer couldn't be read: {e}")),
    });
}

/// A file dialog the host couldn't show (`message` says why).
#[unsafe(no_mangle)]
pub extern "C" fn cc_file_dialog_failed(app: *mut CcApp, id: u64, message: *const c_char) {
    let message = string(message).unwrap_or_else(|| "the file picker isn't available".into());
    with_app(app, "cc_file_dialog_failed", (), |h| h.dialog_failed(id, message));
}

/// Open files the OS handed over (a JSON array of paths).
#[unsafe(no_mangle)]
pub extern "C" fn cc_open_paths(app: *mut CcApp, paths_json: *const c_char) {
    let Some(json) = string(paths_json) else { return };
    with_app(app, "cc_open_paths", (), |h| match serde_json::from_str::<Vec<String>>(&json) {
        Ok(paths) => h.open_paths(paths),
        Err(e) => log::warn!("cc_open_paths: {e}"),
    });
}

/// The host's pasteboard image for a `clipboard_get_image` call: RGBA8, `len` = w × h × 4.
#[unsafe(no_mangle)]
pub extern "C" fn cc_image_sink_put(sink: *mut c_void, width: u32, height: u32, rgba: *const u8, len: usize) {
    if sink.is_null() || rgba.is_null() || width == 0 || height == 0 {
        return;
    }
    if (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4)) != Some(len) {
        log::warn!("cc_image_sink_put: {len} bytes for {width}×{height}");
        return;
    }
    // SAFETY: `sink` is the `ImageSink` passed to `clipboard_get_image`, still on its stack;
    // `rgba` holds `len` bytes for the call.
    let (sink, bytes) = unsafe { (&mut *sink.cast::<ImageSink>(), std::slice::from_raw_parts(rgba, len)) };
    sink.image = Some((width, height, bytes.to_vec()));
}

/// The app became active (true) or resigned active (false).
#[unsafe(no_mangle)]
pub extern "C" fn cc_set_focused(app: *mut CcApp, focused: bool) {
    with_app(app, "cc_set_focused", (), |h| h.set_focused(focused));
}

/// The app is entering the background: flushes autosaves.
#[unsafe(no_mangle)]
pub extern "C" fn cc_background(app: *mut CcApp) {
    with_app(app, "cc_background", (), Host::will_background);
}

/// The system is low on memory.
#[unsafe(no_mangle)]
pub extern "C" fn cc_memory_warning(app: *mut CcApp) {
    with_app(app, "cc_memory_warning", (), Host::memory_warning);
}

/// Whether any open document has unsaved changes.
#[unsafe(no_mangle)]
pub extern "C" fn cc_has_unsaved(app: *mut CcApp) -> bool {
    with_app(app, "cc_has_unsaved", false, |h| h.has_unsaved_work())
}
