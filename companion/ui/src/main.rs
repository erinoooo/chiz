//! Chiz companion: boot and wiring only.
//!
//! - `server`: TLS control connections, one task each (spec 3, 4)
//! - `hub`: the single owner of session, pairing and auth state
//! - `pen`: encrypted UDP pen stream + stuck-pen watchdog (spec 5, 8)
//! - `app`: shared state and the actions the GUI/hotkey/watchers perform
//! - `wire`: framing and typed protocol messages
//! - `gui`/`screens`/`theme`: the egui window
#![cfg_attr(test, allow(clippy::needless_range_loop))]
// Release builds on Windows are GUI-subsystem: no stray console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

/// Shadows `std::eprintln!` for the whole crate: stderr + `chiz.log`.
macro_rules! eprintln {
    ($($arg:tt)*) => { $crate::logging::line(format_args!($($arg)*)) };
}

mod app;
mod config;
mod gui;
mod hub;
mod identity;
mod inject;
mod logging;
mod mdns;
mod pairing;
mod pen;
mod profiles;
mod screens;
mod server;
mod store;
mod theme;
mod util;
mod wire;

use std::sync::Arc;

use store::{PairedStore, PairedTablet};
use util::LockExt;

/// Startup failures are user-facing: say what is wrong and exit non-zero.
fn fatal(msg: &str) -> ! {
    eprintln!("CHIZ: fatal: {msg}");
    std::process::exit(1);
}

fn main() {
    logging::init();
    eprintln!("Chiz {} starting", env!("CARGO_PKG_VERSION"));
    #[cfg(windows)]
    chiz_platform_windows::init_dpi_awareness();

    // Manual runtime: the GUI runs on the main thread outside it, and never
    // needs to block on it (every GUI action is a synchronous App call).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| fatal(&format!("cannot start the async runtime: {e}")));

    let cfg = config::Config::load(&config::config_path());
    let hostname = util::hostname();
    let id = identity::load_or_generate(&hostname)
        .unwrap_or_else(|e| fatal(&format!("cannot load or create the TLS identity: {e}")));
    eprintln!("data dir: {}", identity::data_dir().display());
    eprintln!("certificate id: {}", identity::fp_hex(&id.fingerprint));

    let paired = PairedStore::open(identity::data_dir().join("paired.json"));
    let app = app::App::new(
        cfg.clone(),
        id.fingerprint,
        hostname,
        paired,
        profiles::load(),
    );
    apply_test_hooks(&app);

    let (udp, listener) = rt.block_on(async {
        let udp = tokio::net::UdpSocket::bind(("0.0.0.0", cfg.pen_port))
            .await
            .unwrap_or_else(|e| {
                fatal(&format!(
                    "cannot bind pen UDP port {} ({e}). Is Chiz already running?",
                    cfg.pen_port
                ))
            });
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", cfg.control_port))
            .await
            .unwrap_or_else(|e| {
                fatal(&format!(
                    "cannot bind control TCP port {} ({e}). Is Chiz already running?",
                    cfg.control_port
                ))
            });
        (udp, listener)
    });
    // TLS 1.3 only, no plaintext fallback (spec 3, 12.5).
    let acceptor = tokio_rustls::TlsAcceptor::from(
        identity::server_config(&id)
            .unwrap_or_else(|e| fatal(&format!("cannot build the TLS config: {e}"))),
    );
    eprintln!(
        "listening: control TCP {}, pen UDP {}",
        cfg.control_port, cfg.pen_port
    );

    // mDNS is a convenience; manual IP entry and the QR always work.
    let _mdns = mdns::start(&app.hostname, cfg.control_port, &app.fp);
    rt.spawn(server::serve(Arc::clone(&app), listener, acceptor));
    rt.spawn(pen::udp_task(Arc::clone(&app), udp));
    rt.spawn(pen::watchdog_task(Arc::clone(&app)));
    app::spawn_foreground_watcher(Arc::clone(&app));
    app::install_panic_hotkey(&app);

    if gui::headless() {
        eprintln!("headless mode: no window (unset CHIZ_HEADLESS / omit --headless for the GUI)");
        rt.block_on(std::future::pending::<()>());
        return;
    }
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 720.0]),
        ..Default::default()
    };
    let gui_app = gui::GuiApp::new(Arc::clone(&app));
    if let Err(e) = eframe::run_native(
        "Chiz",
        opts,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(gui_app))
        }),
    ) {
        eprintln!("GUI failed to open ({e}); continuing headless");
        rt.block_on(std::future::pending::<()>());
    }
}

/// Environment hooks for automated tests (never set in normal use):
/// `CHIZ_TEST_PAIR=<device_id>:<base64 32-byte secret>[,...]` pre-pairs a
/// tablet in memory; `CHIZ_PAIRING=open` opens the pairing window at start.
fn apply_test_hooks(app: &app::App) {
    if let Ok(hook) = std::env::var("CHIZ_TEST_PAIR") {
        for entry in hook.split(',') {
            let Some((id, secret)) = entry.split_once(':') else {
                continue;
            };
            let secret = util::b64_decode(secret.trim())
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
            if let Some(secret) = secret {
                app.hub.lock_ok().paired.insert_ephemeral(
                    id.trim(),
                    PairedTablet {
                        secret,
                        name: "test tablet".into(),
                    },
                );
                eprintln!("test hook: pre-paired {}", id.trim());
            }
        }
    }
    if std::env::var("CHIZ_PAIRING").as_deref() == Ok("open") {
        app.open_pairing();
    }
}
