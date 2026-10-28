//! Locations the app shell opens for the engine where no file-manager program can be started
//! (iOS: the Files app, through a `shareddocuments://` URL). The shell registers a channel with
//! [`connect`] and opens what arrives; without one, those platforms report Reveal as unavailable.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, PoisonError};

static SHELL: Mutex<Option<Sender<String>>> = Mutex::new(None);

/// The shell's end: URLs to open, in order. Replaces an earlier connection.
pub fn connect() -> Receiver<String> {
    let (tx, rx) = channel();
    *SHELL.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
    rx
}

/// Hand `url` to the shell. `Err` when no shell is listening.
pub fn open(url: String) -> Result<(), String> {
    let guard = SHELL.lock().unwrap_or_else(PoisonError::into_inner);
    let tx = guard.as_ref().ok_or("this platform has no file manager to show files in")?;
    tx.send(url).map_err(|_| "the file manager connection closed".to_string())
}

/// The Files app's URL for the folder `dir` (`shareddocuments:///path/to/dir`), percent-encoding
/// everything but unreserved characters and `/`.
pub fn files_app_url(dir: &str) -> String {
    let mut out = String::from("shareddocuments://");
    for b in dir.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_folder_path() {
        assert_eq!(files_app_url("/var/mobile/My Files/é"), "shareddocuments:///var/mobile/My%20Files/%C3%A9");
    }

    #[test]
    fn delivers_to_the_connected_shell() {
        let rx = connect();
        assert!(open("shareddocuments:///a".into()).is_ok());
        assert_eq!(rx.try_recv().ok().as_deref(), Some("shareddocuments:///a"));
    }
}
