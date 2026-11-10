use crate::config::app_data_dir;
use std::fs::OpenOptions;
use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Installs a global panic hook that captures panic details and writes
/// a formatted report with backtrace to `panic.log` inside the application data directory.
///
/// Also chains into the previously registered panic hook (or default hook) so
/// standard console output is preserved.
pub fn setup_panic_hook() {
    let next_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let log_dir = app_data_dir()
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let _ = log_panic_to_dir(info, &log_dir);
        next_hook(info);
    }));
}

/// Logs panic information to a `panic.log` file in the specified directory.
pub fn log_panic_to_dir(info: &PanicHookInfo<'_>, dir: &Path) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let log_path = dir.join("panic.log");

    let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<Any> (unknown panic payload)".to_string()
    };

    let location = info
        .location()
        .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
        .unwrap_or_else(|| "unknown location".to_string());

    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("<unnamed>");

    let timestamp = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(dur) => dur.as_secs(),
        Err(_) => 0,
    };

    let backtrace = std::backtrace::Backtrace::capture();

    let report = format!(
        "\n==================== PANIC REPORT ====================\n\
         Timestamp (unix): {}\n\
         Thread: {}\n\
         Location: {}\n\
         Message: {}\n\
         Backtrace:\n{}\n\
         ======================================================\n",
        timestamp, thread_name, location, payload, backtrace
    );

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    file.write_all(report.as_bytes())?;
    file.flush()?;

    Ok(log_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::PanicHookInfo;

    #[test]
    fn test_panic_logging_writes_file() {
        let temp_dir = std::env::temp_dir().join(format!("test_panic_hook_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);

        // We can test that log_panic_to_dir writes correctly
        // We'll simulate a panic hook info using catch_unwind
        let result = std::panic::catch_unwind(|| {
            // Panic with a string message
            panic!("test panic for hook logging");
        });
        assert!(result.is_err());

        // In catch_unwind we get the payload, but let's test with a real panic hook capture
        let recorded_path = std::sync::Arc::new(std::sync::Mutex::new(None));
        let rec_clone = std::sync::Arc::clone(&recorded_path);
        let test_dir = temp_dir.clone();

        let old_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
            if let Ok(path) = log_panic_to_dir(info, &test_dir) {
                *rec_clone.lock().unwrap() = Some(path);
            }
        }));

        let _ = std::panic::catch_unwind(|| {
            panic!("controlled test panic message");
        });

        panic::set_hook(old_hook);

        let written_path = recorded_path.lock().unwrap().take();
        assert!(
            written_path.is_some(),
            "Panic log path should have been recorded"
        );
        let path = written_path.unwrap();
        assert!(path.exists(), "panic.log file must exist");

        let contents = std::fs::read_to_string(&path).expect("read panic log");
        assert!(contents.contains("PANIC REPORT"));
        assert!(contents.contains("controlled test panic message"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
