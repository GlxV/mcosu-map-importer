use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::app_state::StabilityConfig;
use anyhow::Result;
use notify::{Event, RecursiveMode, Watcher};

pub fn start_watcher<F: Fn(PathBuf) + Send + 'static>(dir: PathBuf, callback: F) -> Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            if let Some(path) = event.paths.first() {
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().eq_ignore_ascii_case("osz") {
                        let _ = event_tx.send(path.to_path_buf());
                    }
                }
            }
        }
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    thread::spawn(move || {
        let _keep = watcher;
        while let Ok(path) = event_rx.recv() {
            callback(path);
        }
        drop(_keep);
    });
    Ok(())
}

pub fn is_file_stable(path: &PathBuf, cfg: &StabilityConfig) -> bool {
    let mut last_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut last_mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| std::time::SystemTime::now());
    let start = Instant::now();
    let interval = Duration::from_millis(cfg.interval_ms);
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let mut stable_count = 0u32;

    while start.elapsed() < timeout {
        std::thread::sleep(interval);
        if let Ok(mut file) = std::fs::File::open(path) {
            let mut buf = [0u8; 64];
            if file.read(&mut buf).is_err() {
                stable_count = 0;
                continue;
            }
        } else {
            stable_count = 0;
            continue;
        }

        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => {
                stable_count = 0;
                continue;
            }
        };
        let size = meta.len();
        let mtime = meta
            .modified()
            .unwrap_or_else(|_| std::time::SystemTime::now());

        if size == last_size && mtime == last_mtime {
            stable_count += 1;
            if stable_count >= cfg.consecutive_checks {
                return true;
            }
        } else {
            stable_count = 0;
            last_size = size;
            last_mtime = mtime;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn file_stability_changes() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.osz");
        {
            let mut f = std::fs::File::create(&file).unwrap();
            write!(f, "12345").unwrap();
        }
        let cfg = crate::app_state::StabilityConfig {
            consecutive_checks: 2,
            interval_ms: 50,
            timeout_secs: 5,
        };
        assert!(is_file_stable(&file, &cfg));
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap();
            write!(f, "6789").unwrap();
        }
        // After new write it should eventually stabilize again
        assert!(is_file_stable(&file, &cfg));
    }
}
