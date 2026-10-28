//! The Swift host's callbacks, and safe wrappers around them.
//!
//! Every callback is called on the main thread (the thread the host calls `cc_*` on).

use std::ffi::{CString, c_char, c_void};
use std::rc::Rc;

/// What a file dialog request asks the host for.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    /// One file to read.
    OpenOne = 0,
    /// One or more files to read.
    OpenMany = 1,
    /// A folder.
    Folder = 2,
    /// Where to save a file.
    Save = 3,
}

/// The function table the host passes to `cc_create`. A null entry means the host has no such
/// service; the matching feature then reports itself unavailable instead of failing silently.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CcHostCallbacks {
    /// Passed back as the first argument of every callback.
    pub user_data: *mut c_void,
    /// Show a file dialog: `(user_data, request_id, kind, request_json)`. The JSON object has
    /// `suggested` (save), `extensions` (array or null) and `initialDir`. The host answers later
    /// with `cc_file_dialog_answer` and the same `request_id`.
    pub file_dialog: Option<extern "C" fn(*mut c_void, u64, u32, *const c_char)>,
    /// Open a URL in the system browser; returns whether it was handed off.
    pub open_url: Option<extern "C" fn(*mut c_void, *const c_char) -> bool>,
    /// Put an RGBA8 image on the pasteboard: `(user_data, width, height, pixels, len)`.
    pub clipboard_set_image: Option<extern "C" fn(*mut c_void, u32, u32, *const u8, usize) -> bool>,
    /// Read the pasteboard's image: the host calls `cc_image_sink_put(sink, …)` before returning
    /// when there is one.
    pub clipboard_get_image: Option<extern "C" fn(*mut c_void, *mut c_void)>,
    /// Put text on the pasteboard.
    pub clipboard_set_text: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// Show or hide the on-screen keyboard (a text field has focus).
    pub set_keyboard_visible: Option<extern "C" fn(*mut c_void, bool)>,
    /// Play a haptic: 0 = light tick, 1 = success, 2 = warning.
    pub haptic: Option<extern "C" fn(*mut c_void, u32)>,
    /// Offer a file through the share sheet (Export › Share).
    pub share_file: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// A text field wants the pasteboard's text: the host answers with `cc_paste`.
    pub request_paste: Option<extern "C" fn(*mut c_void)>,
}

/// The host's callbacks, shared by the services that use them.
#[derive(Clone)]
pub struct Callbacks(Rc<CcHostCallbacks>);

/// Collects the image the host hands to `cc_image_sink_put` during a clipboard read.
#[derive(Default)]
pub struct ImageSink {
    pub image: Option<(u32, u32, Vec<u8>)>,
}

fn c_string(s: &str) -> CString {
    // Interior NULs can't cross the C boundary; they never occur in paths or JSON we produce.
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl Callbacks {
    pub fn new(table: CcHostCallbacks) -> Self {
        Self(Rc::new(table))
    }

    pub fn file_dialog(&self, id: u64, kind: DialogKind, request_json: &str) -> bool {
        let Some(f) = self.0.file_dialog else { return false };
        let json = c_string(request_json);
        f(self.0.user_data, id, kind as u32, json.as_ptr());
        true
    }

    pub fn open_url(&self, url: &str) -> Result<(), String> {
        let f = self.0.open_url.ok_or("opening links isn't available")?;
        let url = c_string(url);
        if f(self.0.user_data, url.as_ptr()) { Ok(()) } else { Err("the link couldn't be opened".into()) }
    }

    pub fn clipboard_set_image(&self, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
        let f = self.0.clipboard_set_image.ok_or("the pasteboard isn't available")?;
        if f(self.0.user_data, w, h, rgba.as_ptr(), rgba.len()) { Ok(()) } else { Err("the image couldn't be copied".into()) }
    }

    pub fn clipboard_get_image(&self) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
        let f = self.0.clipboard_get_image.ok_or("the pasteboard isn't available")?;
        let mut sink = ImageSink::default();
        f(self.0.user_data, std::ptr::from_mut(&mut sink).cast());
        Ok(sink.image)
    }

    pub fn clipboard_set_text(&self, text: &str) {
        if let Some(f) = self.0.clipboard_set_text {
            let text = c_string(text);
            f(self.0.user_data, text.as_ptr());
        }
    }

    pub fn set_keyboard_visible(&self, visible: bool) {
        if let Some(f) = self.0.set_keyboard_visible {
            f(self.0.user_data, visible);
        }
    }

    pub fn haptic(&self, kind: u32) {
        if let Some(f) = self.0.haptic {
            f(self.0.user_data, kind);
        }
    }

    pub fn request_paste(&self) {
        if let Some(f) = self.0.request_paste {
            f(self.0.user_data);
        }
    }

    pub fn share_file(&self, path: &str) -> bool {
        let Some(f) = self.0.share_file else { return false };
        let path = c_string(path);
        f(self.0.user_data, path.as_ptr());
        true
    }
}
