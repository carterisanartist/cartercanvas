//! iOS platform services for `PhotocraftApp`: files through the host's document picker,
//! crash-safe writes inside the app container (and folders the user granted), the pasteboard,
//! preferences, brush presets, and crash-recovery autosaves.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use photocraft_codecs::{ChannelLayout, EncodeOptions, Image};
use photocraft_doc::Document;
use photocraft_format::RecoveryStore;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogReply, FileDialogRequest, OsEvent, Recoverable, Services};
use serde_json::json;

use crate::callbacks::{Callbacks, DialogKind};

/// Where the app keeps its files, from the host (`cc_create`).
#[derive(Clone, Debug)]
pub struct Dirs {
    /// Application Support/CarterCanvas: preferences, presets, recovery.
    pub config: PathBuf,
    /// The app's Documents folder (shown in the Files app): the default save location.
    pub documents: PathBuf,
}

impl Dirs {
    pub fn prefs_file(&self) -> PathBuf {
        self.config.join("preferences.json")
    }
    pub fn presets(&self) -> PathBuf {
        self.config.join("Presets")
    }
    pub fn recovery(&self) -> PathBuf {
        self.config.join("Recovery")
    }
}

/// File dialogs waiting for the host's answer, by request id.
#[derive(Clone, Default)]
pub struct PendingDialogs {
    next: Rc<RefCell<u64>>,
    replies: Rc<RefCell<HashMap<u64, FileDialogReply>>>,
}

impl PendingDialogs {
    fn insert(&self, reply: FileDialogReply) -> u64 {
        let mut next = self.next.borrow_mut();
        *next += 1;
        self.replies.borrow_mut().insert(*next, reply);
        *next
    }

    /// Answer request `id`; `None` is Cancel. An unknown id (answered twice) is ignored.
    pub fn answer(&self, id: u64, answer: Option<FileDialogAnswer>) {
        let reply = self.replies.borrow_mut().remove(&id);
        if let Some(reply) = reply {
            reply.send(answer);
        }
    }
}

/// Files the OS asked us to open (Files app, share sheet, "Open in"), drained by the app.
#[derive(Clone, Default)]
pub struct OsInbox(Rc<RefCell<Vec<OsEvent>>>);

impl OsInbox {
    pub fn open(&self, paths: Vec<String>) {
        if !paths.is_empty() {
            self.0.borrow_mut().push(OsEvent::Open(paths));
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    photocraft_format::atomic_write(path, bytes).map_err(|e| e.to_string())
}

fn write_atomic_progress(path: &Path, bytes: &[u8], progress: &dyn Fn(f32)) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    photocraft_format::atomic_write_progress(path, bytes, progress).map_err(|e| e.to_string())
}

/// The share of a save's progress bar that encoding fills; writing the file fills the rest.
const ENCODE_SHARE: f32 = 0.85;

/// A save on a worker thread: encode, then write crash-safely. Cancelling while encoding leaves
/// the target untouched; once writing starts the save can't be cancelled.
fn save_file(doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings, ctl: &photocraft_ui_egui::jobs_ui::SaveCtl) -> Result<Vec<String>, String> {
    ctl.progress(0.0, "Encoding");
    let opts = settings.io_options();
    let cancelled = || ctl.check().is_err();
    let progress = |f: f32| ctl.progress(f * ENCODE_SHARE, "");
    let interrupt = photocraft_raster::Interrupt::new(&cancelled, &progress);
    let r = photocraft_io::export_with(doc, path, &opts, &interrupt).map_err(|e| e.to_string())?;
    ctl.commit()?;
    ctl.progress(ENCODE_SHARE, "Writing");
    write_atomic_progress(Path::new(path), &r.bytes, &|f| ctl.progress(ENCODE_SHARE + (0.99 - ENCODE_SHARE) * f, ""))?;
    ctl.progress(1.0, "");
    Ok(r.warnings)
}

/// Whether `a` and `b` reach the same file (same device and inode).
fn same_file(a: &str, b: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// The JSON the host receives with a file dialog request, and the dialog kind.
pub fn dialog_request(request: &FileDialogRequest, documents: &Path) -> (DialogKind, String) {
    match request {
        FileDialogRequest::Open { multiple, initial_dir, extensions } => (
            if *multiple { DialogKind::OpenMany } else { DialogKind::OpenOne },
            json!({ "extensions": extensions, "initialDir": initial_dir }).to_string(),
        ),
        FileDialogRequest::Folder { initial_dir } => (DialogKind::Folder, json!({ "initialDir": initial_dir }).to_string()),
        FileDialogRequest::Save { suggested } => {
            let path = Path::new(suggested);
            let name = path.file_name().map_or_else(|| suggested.clone(), |n| n.to_string_lossy().into_owned());
            // A document's own folder when it has one we can write to, else Documents.
            let folder = path.parent().filter(|p| p.is_absolute() && p.is_dir()).map_or_else(|| documents.to_path_buf(), Path::to_path_buf);
            (DialogKind::Save, json!({ "suggested": name, "folder": folder.to_string_lossy(), "documents": documents.to_string_lossy() }).to_string())
        }
    }
}

/// The app's answer for what the host picked: `paths` (`None` = cancelled).
pub fn dialog_answer(kind: DialogKind, paths: Option<Vec<String>>) -> Option<FileDialogAnswer> {
    let mut paths = paths.filter(|p| !p.is_empty())?;
    Some(match kind {
        DialogKind::OpenOne | DialogKind::OpenMany => FileDialogAnswer::Paths(paths),
        DialogKind::Folder => FileDialogAnswer::FolderTo(paths.swap_remove(0)),
        DialogKind::Save => FileDialogAnswer::SaveTo(paths.swap_remove(0)),
    })
}

type SharedRecovery = Rc<RefCell<Option<RecoveryStore>>>;

fn with_store<R>(store: &SharedRecovery, f: impl FnOnce(&mut RecoveryStore) -> R) -> Result<R, String> {
    let mut slot = store.try_borrow_mut().map_err(|_| "crash recovery is busy".to_string())?;
    Ok(f(slot.as_mut().ok_or("no recovery folder")?))
}

/// Background incremental autosaves into `dir`, listed again after a crash or a kill.
fn recovery_services(dir: PathBuf) -> Services {
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("recovery folder {}: {e}", dir.display());
    }
    let recover_dir = dir.clone();
    let store: SharedRecovery = Rc::new(RefCell::new(Some(RecoveryStore::new(dir))));
    let (s1, s2, s4, s5) = (store.clone(), store.clone(), store.clone(), store.clone());
    Services {
        autosave: Some(Box::new(move |doc: &Arc<Document>, revision: u64, path: Option<&str>| {
            with_store(&s1, |s| s.autosave_checked(doc, revision, path.map(str::to_string)))?
        })),
        autosave_results: Some(Box::new(move || with_store(&s4, |s| s.take_completed()).unwrap_or_default())),
        flush_autosaves: Some(Box::new(move || with_store(&s5, RecoveryStore::flush).unwrap_or_default())),
        discard_autosave: Some(Box::new(move |id: u64| {
            let _ = with_store(&s2, |s| s.discard(id));
        })),
        recover: Some(Box::new(move || {
            photocraft_format::list_recovery(&recover_dir)
                .into_iter()
                .map(|e| Recoverable {
                    key: e.info.key.clone(),
                    name: e.info.document_name.clone(),
                    path: e.info.original_path.clone(),
                    load: Box::new(move || photocraft_format::recover(&e).map_err(|e| e.to_string())),
                })
                .collect()
        })),
        adopt_autosave: Some(Box::new(move |id: u64, key: &str| {
            let _ = with_store(&store, |s| s.adopt(id, key));
        })),
        ..Default::default()
    }
}

thread_local! {
    /// The host callbacks for [`haptic_beep`], which `Services::beep` takes as a plain `fn()`.
    static BEEP: RefCell<Option<Callbacks>> = const { RefCell::new(None) };
}

/// Preferences › General › Beep When Done: a success haptic (iOS apps have no alert beep).
fn haptic_beep() {
    BEEP.with(|b| {
        if let Some(cb) = b.borrow().as_ref() {
            cb.haptic(1);
        }
    });
}

/// Everything `PhotocraftApp` needs from the platform.
pub fn ios(callbacks: &Callbacks, dirs: &Dirs, dialogs: &PendingDialogs, inbox: &OsInbox) -> Services {
    BEEP.with(|b| *b.borrow_mut() = Some(callbacks.clone()));
    for dir in [&dirs.config, &dirs.documents, &dirs.presets()] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::error!("{}: {e}", dir.display());
        }
    }
    let documents = dirs.documents.clone();
    let prefs_read = dirs.prefs_file();
    let prefs_write = dirs.prefs_file();
    let (dialog_cb, url_cb, set_cb, get_cb) = (callbacks.clone(), callbacks.clone(), callbacks.clone(), callbacks.clone());
    let dialogs = dialogs.clone();
    let inbox = inbox.clone();
    Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], opts: &photocraft_io::ImportOptions| {
            photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        })),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            photocraft_io::export(doc, path, &settings.io_options()).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        save_file: Some(Arc::new(save_file)),
        file_dialog: Some(Box::new(move |request: FileDialogRequest, _parent: Option<&eframe::Frame>, reply: FileDialogReply| {
            let (kind, json) = dialog_request(&request, &documents);
            let id = dialogs.insert(reply);
            if !dialog_cb.file_dialog(id, kind, &json) {
                dialogs.answer(id, Some(FileDialogAnswer::Unavailable("This build has no file picker.".into())));
            }
        })),
        write: Some(Box::new(|path: &str, bytes: &[u8]| write_atomic(Path::new(path), bytes))),
        file_stamp: Some(Box::new(|path: &str| {
            let meta = std::fs::metadata(path).ok()?;
            Some(photocraft_ui_egui::FileStamp { len: meta.len(), modified: meta.modified().ok() })
        })),
        same_file: Some(Box::new(same_file)),
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        open_url: Some(Box::new(move |url: &str| url_cb.open_url(url))),
        beep: Some(haptic_beep),
        clipboard_set_image: Some(Box::new(move |w: u32, h: u32, px: &[u8]| set_cb.clipboard_set_image(w, h, px))),
        clipboard_get_image: Some(Box::new(move || get_cb.clipboard_get_image())),
        clipboard_unreadable: Some(Box::new(|| false)),
        load_prefs: Some(Box::new(move || std::fs::read_to_string(&prefs_read).ok())),
        save_prefs: Some(Box::new(move |text: &str| write_atomic(&prefs_write, text.as_bytes()))),
        append_text: Some(Box::new(|path: &str, text: &str| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())
        })),
        os_events: Some(Box::new(move || std::mem::take(&mut *inbox.0.borrow_mut()))),
        preset_store: Some(photocraft_engine::preset_store::open_dir_async(dirs.presets())),
        ..recovery_services(dirs.recovery())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_request_suggests_documents_for_untitled() {
        let docs = std::env::temp_dir();
        let (kind, json) = dialog_request(&FileDialogRequest::Save { suggested: "Untitled-1.psd".into() }, &docs);
        assert_eq!(kind, DialogKind::Save);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(v["suggested"], "Untitled-1.psd");
        assert_eq!(v["folder"], docs.to_string_lossy().as_ref());
    }

    #[test]
    fn answers_map_to_the_dialog_kind() {
        assert_eq!(dialog_answer(DialogKind::Save, Some(vec!["/a/b.psd".into()])), Some(FileDialogAnswer::SaveTo("/a/b.psd".into())));
        assert_eq!(dialog_answer(DialogKind::Folder, Some(vec!["/a".into()])), Some(FileDialogAnswer::FolderTo("/a".into())));
        assert_eq!(dialog_answer(DialogKind::OpenMany, Some(vec!["/x.png".into(), "/y.png".into()])), Some(FileDialogAnswer::Paths(vec!["/x.png".into(), "/y.png".into()])));
        assert_eq!(dialog_answer(DialogKind::OpenOne, Some(Vec::new())), None);
        assert_eq!(dialog_answer(DialogKind::OpenOne, None), None);
    }
}
