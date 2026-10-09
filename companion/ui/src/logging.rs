//! File logging. Release builds on Windows have no console, so without this
//! every diagnostic vanishes. All `eprintln!` calls in this crate are routed
//! here (see the macro at the top of main.rs): stderr when there is one, plus
//! `chiz.log` in the data dir, rotated at 1 MiB.

use std::fmt::Arguments;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
const MAX_BYTES: u64 = 1 << 20;

/// Open the log file. Safe to skip (tests): `line` then only writes stderr.
pub fn init() {
    let dir = super::identity::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("chiz.log");
    if std::fs::metadata(&path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(&path, dir.join("chiz.log.1"));
    }
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = FILE.set(Mutex::new(f));
    }
    // A panic in a background task would otherwise vanish silently.
    std::panic::set_hook(Box::new(|info| {
        line(format_args!("PANIC: {info}"));
    }));
}

pub fn line(args: Arguments<'_>) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let text = format!("{h:02}:{m:02}:{s:02} {args}\n");
    let _ = std::io::stderr().write_all(text.as_bytes());
    if let Some(f) = FILE.get() {
        if let Ok(mut f) = f.lock() {
            let _ = f.write_all(text.as_bytes());
        }
    }
}
