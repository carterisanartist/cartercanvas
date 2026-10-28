//! Log records go to standard error, which Xcode's console and `devicectl --console` show, and
//! to `<config>/Logs/cartercanvas.log` so a tester can attach the log of a session that went wrong.

use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

struct Logger {
    file: Mutex<Option<std::fs::File>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Largest log file kept; a bigger one is moved to `cartercanvas.1.log` at the next launch.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let ours = metadata.target().starts_with("cartercanvas") || metadata.target().starts_with("photocraft");
        metadata.level() <= if ours { log::Level::Info } else { log::Level::Warn }
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("[{}] {}: {}\n", record.level(), record.target(), record.args());
        let _ = std::io::stderr().write_all(line.as_bytes());
        if let Some(f) = self.file.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Some(f) = self.file.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            let _ = f.flush();
        }
    }
}

/// Install the logger (once; later calls are ignored) and a panic hook that logs the panic.
pub fn install(config_dir: &Path) {
    let logs = config_dir.join("Logs");
    let file = std::fs::create_dir_all(&logs).ok().and_then(|()| {
        let path = logs.join("cartercanvas.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
            let _ = std::fs::rename(&path, logs.join("cartercanvas.1.log"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
    });
    if LOGGER.set(Logger { file: Mutex::new(file) }).is_ok()
        && let Some(logger) = LOGGER.get()
        && log::set_logger(logger).is_ok()
    {
        log::set_max_level(log::LevelFilter::Info);
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::error!("panic: {info}");
            log::logger().flush();
            default_hook(info);
        }));
    }
}
