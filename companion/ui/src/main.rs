//! Chiz companion runtime (spec 8, 12).
//!
//! What is wired here (M6-M7 start):
//! - TCP control listener (default 47800) + UDP pen listener (47801).
//! - Private-source gate, 4-unauth cap, 10 s handshake deadline,
//!   5 s liveness deadline, per-IP auth backoff, single-tablet session
//!   table with same-device replace, `release_all` hook on session end.
//!
//! TLS splice point: wrap `TcpStream` in a rustls server stream (rustls +
//! rcgen ECDSA P-256 self-signed cert, TLS 1.3 only, pinned-fingerprint
//! client auth via `hello`/pair proofs) BEFORE the frame loop below. The
//! frame loop itself is transport-agnostic, which is why control.rs tests
//! exercise it without sockets. mDNS (`_chiz._tcp`, TXT v/id/name),
//! tray + egui screens, and QR rendering land in later slices.

mod control;
mod identity;
mod mdns;
mod pairing;
mod pen;
mod screens;
mod tray;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chiz_core::action::{ActionRunner, Effect};
use chiz_core::failsafe::{Failsafe, PauseReason, DEFAULT_PANIC_HOTKEY};
use chiz_core::profile::{self, Profile};
use chiz_core::{derive_pen_key, PenReceiver};
use control::{error_msg, ConnStage, SessionTable};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
struct Config {
    control_port: u16,
    pen_port: u16,
    allow_public: bool, // default false (spec 12.7)
    panic_hotkey: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            control_port: 47800,
            pen_port: 47801,
            allow_public: false,
            panic_hotkey: DEFAULT_PANIC_HOTKEY.into(),
        }
    }
}

impl Config {
    /// Minimal loader: reads ports + public-source override from a JSON
    /// file shaped like companion/config.example.json. Unknown fields
    /// ignored; missing file -> defaults.
    fn load(path: &str) -> Self {
        let mut cfg = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return cfg;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return cfg;
        };
        if let Some(p) = v.get("ports").and_then(|p| p.get("control")).and_then(|p| p.as_u64()) {
            cfg.control_port = p as u16;
        }
        if let Some(p) = v.get("ports").and_then(|p| p.get("pen")).and_then(|p| p.as_u64()) {
            cfg.pen_port = p as u16;
        }
        if let Some(a) = v.get("allow_public_source").and_then(|a| a.as_bool()) {
            cfg.allow_public = a;
        }
        if let Some(h) = v.get("panic_hotkey").and_then(|h| h.as_str()) {
            cfg.panic_hotkey = h.to_string();
        }
        cfg
    }
}

/// Paired tablets: device_id -> 32-byte secret. Backed by paired.json
/// (DPAPI on Windows / 0600 file on Linux per spec 9-10); in-memory here
/// with a loader hook.
#[derive(Default)]
struct PairedStore {
    secrets: HashMap<String, [u8; 32]>,
}

/// Constant-time equality (spec 12.2). No early exit on mismatch.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Crypto-random bytes from the OS (spec 12.1). Linux: /dev/urandom;
/// portability hook: prefer `getrandom`/`rand` crate on Windows builds.
fn os_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(buf))
            .expect("OS CSPRNG unavailable");
    }
    #[cfg(not(unix))]
    {
        // Windows: replace with BCryptGenRandom / `rand` crate before release.
        panic!("OS CSPRNG hook not wired for this platform");
    }
}

struct Shared {
    sessions: Mutex<SessionTable>,
    auth: Mutex<control::AuthTracker>,
    unauth_count: Mutex<usize>,
    pairing: Mutex<pairing::PairingState>,
    paired: Mutex<PairedStore>,
    fp: [u8; 32], // own cert fingerprint: binds PIN proofs, feeds QR/mDNS
    pen_key: Mutex<HashMap<u32, ([u8; 32], String)>>, // session_id -> (key, peer ip)
    pen_rx: Mutex<HashMap<u32, PenReceiver>>,         // session_id -> receiver
    profiles: Mutex<ProfileSet>,
    runner: Mutex<ActionRunner>,
    /// std mutex (not tokio): the OS panic-hook thread has no runtime and
    /// must lock this synchronously. Only ever held for flag flips, never
    /// across await.
    failsafe: std::sync::Mutex<Failsafe>,
}

/// Status banner hook: tray notification + status screen in the full UI;
/// stderr until then so it is visible in logs too.
fn banner(msg: &str) {
    eprintln!("CHIZ: {msg}");
}

/// THE emergency off switch. Callable from ANY thread (OS hotkey hook, tray
/// callback, watchdog): synchronously pauses pen input, releases every held
/// key/button, and raises the banner. Never touches the network, disk, or
/// the async runtime, so it works even if the session tasks are wedged.
pub(crate) fn emergency_release(shared: &Arc<Shared>) {
    let newly = shared.failsafe.lock().expect("failsafe").emergency_release();
    // Release held keys even if pen was already paused: a stuck modifier
    // must never survive the panic switch.
    let rel = shared.runner.blocking_lock().release_all();
    apply_effects(&rel);
    eprintln!("inject_pen up+leave (panic)");
    release_all("panic");
    if newly {
        banner("Emergency release: pen paused, everything released. Resume from the tray.");
    } else {
        banner("Emergency release: everything released (pen already paused).");
    }
}

/// Tray menu wiring point (tray-icon crate, M6). Items:
/// "Emergency release", "Pause pen input" (toggle), "Resume".
fn tray_action(shared: &Arc<Shared>, item: &str) {
    match item {
        "panic" => emergency_release(shared),
        "pause" => {
            if shared.failsafe.lock().expect("failsafe").pause(PauseReason::Manual) {
                let rel = shared.runner.blocking_lock().release_all();
                apply_effects(&rel);
                release_all("pause");
                banner("Pen input paused. Buttons keep working.");
            }
        }
        "resume" => {
            shared.failsafe.lock().expect("failsafe").resume();
            banner("Pen input resumed.");
        }
        _ => {}
    }
}

/// Loaded profiles + manual lock (spec 8). Auto selection polls
/// `foreground_app()` every 500 ms and switches after 300 ms stable; the
/// poll loop lands with the tray UI (M6) and the stub below returns None
/// until the platform crates wire it.
struct ProfileSet {
    profiles: Vec<Profile>,
    locked: Option<String>,
}

impl ProfileSet {
    fn current(&self) -> Option<&Profile> {
        profile::select_profile(&self.profiles, foreground_app().as_deref(), self.locked.as_deref())
    }
    fn switch(&mut self, target: &str) -> Option<(String, String)> {
        // Returns (profile_id, reason). Releases are the caller's job.
        if target == "auto" {
            self.locked = None;
            let p = self.current()?;
            return Some((p.id.clone(), "app".into()));
        }
        let ids: Vec<String> = self.profiles.iter().map(|p| p.id.clone()).collect();
        let cur = self.current().map(|p| p.id.clone()).unwrap_or_default();
        let id = match target {
            "next" | "prev" => {
                let i = ids.iter().position(|x| *x == cur).unwrap_or(0);
                let j = if target == "next" {
                    (i + 1) % ids.len().max(1)
                } else {
                    (i + ids.len().max(1) - 1) % ids.len().max(1)
                };
                ids.get(j)?.clone()
            }
            other => {
                if !ids.iter().any(|x| x == other) {
                    return None;
                }
                other.to_string()
            }
        };
        self.locked = Some(id.clone());
        Some((id, "manual".into()))
    }
}

/// No portable Wayland API exists: returns None, auto-switch stays off,
/// and the UI must say so (spec 10). X11/Wayland platform crates override.
fn foreground_app() -> Option<String> {
    None
}

/// Load `*.json` profiles. Search order: directory next to the executable
/// (drop custom profiles beside the binary), then CWD-relative dev
/// candidates. Falls back to the profiles embedded at compile time so a
/// bare binary with no data files still works (the reported
/// "profiles: 0 loaded" case).
fn load_profiles() -> ProfileSet {
    const EMBEDDED: &[&str] = &[
        include_str!("../../profiles/default.json"),
        include_str!("../../profiles/krita.json"),
    ];
    let mut dirs = vec![];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("profiles").to_string_lossy().into_owned());
        }
    }
    dirs.extend(
        ["companion/profiles", "profiles", "./profiles", "../profiles"]
            .iter()
            .map(|s| s.to_string()),
    );
    let mut profiles = vec![];
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().map(|x| x != "json").unwrap_or(true) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(prof) = serde_json::from_str::<Profile>(&text) else {
                eprintln!("profile skipped (parse): {}", p.display());
                continue;
            };
            if !profile::validate_profile(&prof).is_empty() {
                eprintln!("profile skipped (invalid): {}", p.display());
                continue;
            }
            if p.file_stem().map(|s| s != prof.id.as_str()).unwrap_or(true) {
                eprintln!("profile skipped (file name != id): {}", p.display());
                continue;
            }
            profiles.push(prof);
        }
        if !profiles.is_empty() {
            eprintln!("profiles loaded from {dir}");
            break;
        }
    }
    if profiles.is_empty() {
        for text in EMBEDDED {
            match serde_json::from_str::<Profile>(text) {
                Ok(prof) if profile::validate_profile(&prof).is_empty() => profiles.push(prof),
                _ => eprintln!("built-in profile invalid (build bug)"),
            }
        }
        if !profiles.is_empty() {
            eprintln!("profiles: using built-in defaults (no profiles dir found)");
        }
    }
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    for e in profile::validate_set(&profiles) {
        eprintln!("profile set: {e}");
    }
    ProfileSet {
        profiles,
        locked: None,
    }
}

/// Wire-format `profile` message. The tablet ignores unknown fields.
fn profile_wire(p: &Profile, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "t": "profile",
        "profile": {
            "version": 1, "id": p.id, "name": p.name, "apps": p.apps,
            "default": p.default,
            "grid": {"cols": p.grid.cols, "rows": p.grid.rows},
            "buttons": p.buttons.iter().map(|b| serde_json::json!({
                "id": b.id, "label": b.label, "speak": b.speak_text(),
                "kind": b.kind, "col": b.col, "row": b.row,
                "colspan": b.colspan, "rowspan": b.rowspan,
                "action": action_wire(&b.action),
            })).collect::<Vec<_>>(),
        },
        "reason": reason,
        "app": foreground_app(),
    })
}

fn action_wire(a: &profile::Action) -> serde_json::Value {
    match a {
        profile::Action::Key(k) => {
            let mut v = serde_json::json!({"type": "key", "keys": k.keys});
            if let Some(off) = &k.keys_off {
                v["keys_off"] = serde_json::Value::String(off.clone());
            }
            v
        }
        profile::Action::Macro(m) => serde_json::json!({"type": "macro", "steps": m.steps.iter().map(|s| match s {
            profile::MacroStep::Keys { keys } => serde_json::json!({"keys": keys}),
            profile::MacroStep::Wait { wait_ms } => serde_json::json!({"wait_ms": wait_ms}),
        }).collect::<Vec<_>>()}),
        profile::Action::Profile(p) => serde_json::json!({"type": "profile", "target": p.target}),
        profile::Action::EraserToggle => serde_json::json!({"type": "eraser_toggle"}),
        profile::Action::Mouse(m) => serde_json::json!({"type": "mouse", "button": m.button}),
        profile::Action::None => serde_json::json!({"type": "none"}),
    }
}

/// Apply non-toggle effects to the platform. Key/mouse go to
/// `SendInput`/uinput via the platform crates (M3); until then they are
/// logged so button dispatch is demonstrable end to end.
fn apply_effects(effects: &[Effect]) {
    for fx in effects {
        match fx {
            Effect::Key { key, down } => {
                eprintln!("inject_key {key} {}", if *down { "down" } else { "up" });
            }
            Effect::Mouse { button, down } => {
                eprintln!("inject_mouse {button} {}", if *down { "down" } else { "up" });
            }
            Effect::WaitMs(_) => {}
            _ => {}
        }
    }
}

/// One UDP datagram from the tablet: session lookup -> pause gate ->
/// decrypt/filter -> pen pipeline. Drops while paused (with the single
/// transitional `leave`); records never wait on control/button work.
async fn handle_pen_datagram(shared: &Arc<Shared>, dg: &[u8], src: SocketAddr) {
    if dg.len() < 10 {
        return;
    }
    let sid = u32::from_le_bytes(dg[2..6].try_into().unwrap());
    let keys = shared.pen_key.lock().await;
    let Some((key, _ip)) = keys.get(&sid) else {
        return; // unknown/dead session_id: drop (spec 4/5)
    };
    let key = *key;
    drop(keys);
    // Pause gate: drop decoded pen, inject `leave` once, buttons unaffected.
    if shared.failsafe.lock().expect("failsafe").drop_pen() {
        let mut fs = shared.failsafe.lock().expect("failsafe");
        if fs.leave_pending {
            fs.leave_pending = false;
            eprintln!("inject_pen leave (paused)");
        }
        return;
    }
    let mut rxmap = shared.pen_rx.lock().await;
    let Some(rx) = rxmap.get_mut(&sid) else {
        return;
    };
    let (recs, _stats) = pen::handle_datagram(rx, dg, &src.ip().to_string(), &key, now_secs());
    let eraser_forced = shared.runner.lock().await.eraser_on;
    for r in recs {
        // Pipeline order (spec 8): rotate+map -> pressure curve ->
        // eraser-toggle -> inject_pen. Monitor mapping lands with the
        // platform layers (M2/M3); coordinates logged until then.
        let eraser = r.eraser || eraser_forced;
        eprintln!(
            "inject_pen phase={} x={} y={} p={} tilt={}/{} eraser={}",
            r.phase, r.x, r.y, r.pressure, r.tilt_x, r.tilt_y, eraser
        );
        shared.failsafe.lock().expect("failsafe").note_clean_input();
    }
}

/// 100 ms ticker: stuck-pen watchdogs. A trip injects the repair and feeds
/// the failsafe counter; 3 consecutive trips auto-pause with a banner.
async fn watchdog_tick(shared: &Arc<Shared>) {
    let mut trips = 0;
    {
        let mut rxmap = shared.pen_rx.lock().await;
        for rx in rxmap.values_mut() {
            if let Some(action) = rx.watchdog(now_secs()) {
                eprintln!("inject_pen {action} (watchdog)");
                trips += 1;
            }
        }
    }
    for _ in 0..trips {
        if shared.failsafe.lock().expect("failsafe").note_stuck_trip().is_some() {
            let rel = shared.runner.lock().await.release_all();
            apply_effects(&rel);
            release_all("auto-pause");
            banner("Pen auto-paused after repeated stuck input. Resume from the tray.");
        }
    }
}

/// Platform hook: MUST lift pen, leave range, release every key/button
/// when a session ends, the profile changes, or the app quits (spec 4, 8).
fn release_all(reason: &str) {
    // Wired to platform-windows / platform-linux injectors in M3.
    eprintln!("release_all({reason}) [stub: no OS injector wired yet]");
}

async fn send_msg(stream: &mut (impl AsyncWriteExt + Unpin), v: &serde_json::Value) -> std::io::Result<()> {
    let bytes = control::encode_frame(v).expect("frame encode");
    stream.write_all(&bytes).await
}

async fn read_msg(
    stream: &mut (impl AsyncReadExt + Unpin),
    buf: &mut Vec<u8>,
) -> Result<Option<serde_json::Value>, &'static str> {
    // Drain already-buffered frames FIRST: several small control messages
    // often arrive in one TCP segment, and waiting for new socket data
    // before decoding them stalls the loop until the liveness deadline.
    match control::decode_frame(buf) {
        Ok((v, used)) => {
            buf.drain(..used);
            return Ok(Some(v));
        }
        Err(control::FrameError::Oversize) => return Err("oversize"),
        Err(control::FrameError::Incomplete) => {}
        Err(_) => return Err("bad frame"),
    }
    let mut tmp = [0u8; 4096];
    let n = stream.read(tmp.as_mut()).await.map_err(|_| "read")?;
    if n == 0 {
        return Ok(None); // clean EOF
    }
    buf.extend_from_slice(&tmp[..n]);
    match control::decode_frame(buf) {
        Ok((v, used)) => {
            buf.drain(..used);
            Ok(Some(v))
        }
        Err(control::FrameError::Incomplete) => Ok(Some(serde_json::json!({"t":"_more"}))),
        Err(control::FrameError::Oversize) => Err("oversize"),
        Err(_) => Err("bad frame"),
    }
}

async fn handle_conn<S>(
    mut stream: S,
    peer: SocketAddr,
    cfg: Config,
    shared: Arc<Shared>,
) where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let peer_ip = peer.ip();
    if !cfg.allow_public && !control::is_private_source(&peer_ip) {
        let _ = send_msg(&mut stream, &error_msg("not_paired", "public source refused by default")).await;
        return;
    }
    if shared.auth.lock().await.is_blocked(&peer_ip, now_secs()) {
        return; // ignored for 60 s after 5 failures
    }
    {
        let mut n = shared.unauth_count.lock().await;
        if *n >= control::MAX_UNAUTH {
            return;
        }
        *n += 1;
    }
    let dec_unauth = || async {
        let mut n = shared.unauth_count.lock().await;
        *n = n.saturating_sub(1);
    };

    // Challenge first (fresh 16-byte nonce per connection).
    let mut nonce = [0u8; 16];
    os_random(&mut nonce);
    let challenge_for = |nonce: &[u8; 16], pairing_open: bool| serde_json::json!({
        "t": "challenge", "proto": 1, "nonce": base64_b64(nonce),
        "pc_name": hostname(), "os": std::env::consts::OS,
        "pairing_open": pairing_open,
    });
    let pairing_now_open = shared.pairing.lock().await.is_open(now_secs());
    if send_msg(&mut stream, &challenge_for(&nonce, pairing_now_open)).await.is_err() {
        dec_unauth().await;
        return;
    }

    let accept_at = Instant::now();
    let mut stage = ConnStage::Unauth { since: 0.0 };
    let mut buf: Vec<u8> = Vec::new();
    let mut last_rx = Instant::now();
    let conn_id: u64 = peer.port() as u64 | ((now_secs() * 1000.0) as u64) << 32;

    loop {
        if matches!(stage, ConnStage::Unauth { .. })
            && accept_at.elapsed() > Duration::from_secs_f64(control::HANDSHAKE_DEADLINE_SECS)
        {
            break; // 10 s handshake limit
        }
        if last_rx.elapsed() > Duration::from_secs_f64(control::DEAD_AFTER_SECS) {
            break; // dead connection
        }
        let msg = match tokio::time::timeout(Duration::from_millis(200), read_msg(&mut stream, &mut buf)).await {
            Ok(Ok(Some(v))) => v,
            Ok(Ok(None)) => break, // EOF
            Ok(Err(_)) => break,
            Err(_) => continue, // timeout tick: re-check deadlines
        };
        if msg.get("t") == Some(&serde_json::Value::from("_more")) {
            continue;
        }
        last_rx = Instant::now();
        let Some(t) = control::msg_type(&msg).map(str::to_owned) else {
            continue; // unknown type: ignore
        };
        match t.as_str() {
            "ping" => {
                let id = msg.get("id").cloned().unwrap_or(serde_json::Value::from(0));
                let _ = send_msg(&mut stream, &serde_json::json!({"t":"pong","id":id})).await;
            }
            "pair_request" => {
                // Pairing: only inside an open window, else not_paired.
                // Sent instead of `hello` while unpaired.
                if matches!(stage, ConnStage::Authed { .. }) {
                    continue;
                }
                let device_id = msg.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_owned();
                let mode = msg.get("mode").and_then(|v| v.as_str()).unwrap_or("");
                let mut ps = shared.pairing.lock().await;
                if !ps.is_open(now_secs()) {
                    drop(ps);
                    let _ = send_msg(&mut stream, &error_msg("not_paired", "no pairing window open")).await;
                    break;
                }
                // Own cert fingerprint binds the PIN proof (MITM fails).
                let fp = shared.fp;
                let ok = match mode {
                    "qr" => ps.verify_qr(msg.get("token").and_then(|v| v.as_str()).unwrap_or("")),
                    "pin" => ps.verify_pin(
                        msg.get("proof").and_then(|v| v.as_str()).unwrap_or(""),
                        &fp,
                        &nonce,
                        &device_id,
                    ),
                    _ => false,
                };
                if ok {
                    let mut secret = [0u8; 32];
                    os_random(&mut secret);
                    let device_name = msg.get("device_name").and_then(|v| v.as_str()).unwrap_or("tablet").to_owned();
                    shared.paired.lock().await.secrets.insert(device_id, secret);
                    drop(ps);
                    shared.pairing.lock().await.window.record_success();
                    banner(&format!("Tablet paired: {device_name}"));
                    let reply = serde_json::json!({"t": "pair_ok", "device_secret": base64_b64(&secret)});
                    if send_msg(&mut stream, &reply).await.is_err() {
                        break;
                    }
                    // Fresh challenge: the tablet now answers with `hello`.
                    os_random(&mut nonce);
                    let open = shared.pairing.lock().await.is_open(now_secs());
                    if send_msg(&mut stream, &challenge_for(&nonce, open)).await.is_err() {
                        break;
                    }
                } else {
                    let code = match mode {
                        "qr" => "bad_token",
                        "pin" => "bad_pin",
                        _ => "bad_pin",
                    };
                    let left = ps.attempts_left().saturating_sub(1);
                    let closed = ps.window.record_failure() == "closed";
                    drop(ps);
                    if closed {
                        let _ = send_msg(
                            &mut stream,
                            &serde_json::json!({"t": "pair_fail", "code": "closed", "attempts_left": 0}),
                        )
                        .await;
                    } else {
                        let _ = send_msg(
                            &mut stream,
                            &serde_json::json!({"t": "pair_fail", "code": code, "attempts_left": left}),
                        )
                        .await;
                    }
                }
            }
            "hello" => {
                let device_id = msg.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_owned();
                let auth_b64 = msg.get("auth").and_then(|v| v.as_str()).unwrap_or("");
                let Some(secret) = shared.paired.lock().await.secrets.get(&device_id).cloned() else {
                    let _ = send_msg(&mut stream, &error_msg("not_paired", "unknown tablet")).await;
                    shared.auth.lock().await.record_failure(peer_ip, now_secs());
                    break;
                };
                let expect = chiz_core::compute_auth(&secret, &nonce, &device_id);
                if !ct_eq(expect.as_bytes(), auth_b64.as_bytes()) {
                    let _ = send_msg(&mut stream, &error_msg("auth_failed", "bad auth")).await;
                    shared.auth.lock().await.record_failure(peer_ip, now_secs());
                    break;
                }
                shared.auth.lock().await.record_success(&peer_ip);
                match shared.sessions.lock().await.auth(conn_id, &device_id) {
                    Ok(sid) => {
                        let mut salt = [0u8; 16];
                        os_random(&mut salt);
                        let key = derive_pen_key(&secret, &salt);
                        shared.pen_key.lock().await.insert(sid, (key, peer_ip.to_string()));
                        shared
                            .pen_rx
                            .lock()
                            .await
                            .insert(sid, PenReceiver::new(sid, &peer_ip.to_string()));
                        let welcome = serde_json::json!({
                            "t": "welcome", "session_id": sid,
                            "pen_port": cfg.pen_port,
                            "pen_key_salt": base64_b64(&salt),
                            "features": {"video": false},
                        });
                        if send_msg(&mut stream, &welcome).await.is_err() {
                            break;
                        }
                        // The companion sends `profile` right after `welcome`.
                        if let Some(p) = shared.profiles.lock().await.current() {
                            let pw = profile_wire(p, "connect");
                            if send_msg(&mut stream, &pw).await.is_err() {
                                break;
                            }
                        }
                        stage = ConnStage::Authed { session_id: sid };
                        dec_unauth().await;
                    }
                    Err(_holder) => {
                        let _ = send_msg(&mut stream, &error_msg("busy", "another tablet is connected")).await;
                        break;
                    }
                }
            }
            "bye" => break,
            "button" => {
                // Actions run on `down`; `up` completes holds. Unknown ids
                // are ignored. Buttons work even while pen input is paused.
                if !matches!(stage, ConnStage::Authed { .. }) {
                    continue;
                }
                let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let phase = msg.get("phase").and_then(|v| v.as_str()).unwrap_or("");
                if phase != "down" && phase != "up" {
                    continue;
                }
                let maybe_btn = {
                    let ps = shared.profiles.lock().await;
                    ps.current().and_then(|p| {
                        p.buttons.iter().find(|b| b.id == id).map(|b| b.clone())
                    })
                };
                let Some(button) = maybe_btn else {
                    continue; // unknown button id: ignore
                };
                let effects = shared.runner.lock().await.on_button(&button, phase);
                // Macros run on a worker task (never the pen path, never
                // blocking this control loop); everything else applies now.
                if effects.iter().any(|e| matches!(e, Effect::WaitMs(_))) {
                    let fx_owned = effects.clone();
                    tokio::spawn(async move {
                        for fx in &fx_owned {
                            match fx {
                                Effect::WaitMs(ms) => {
                                    tokio::time::sleep(Duration::from_millis(*ms as u64)).await;
                                }
                                f => apply_effects(std::slice::from_ref(f)),
                            }
                        }
                    });
                } else {
                    apply_effects(&effects);
                }
                for fx in &effects {
                    match fx {
                        Effect::ToggleState { id, on } => {
                            let speak = if *on { " on" } else { " off" };
                            eprintln!("toggle {id}{speak}");
                            let _ = send_msg(
                                &mut stream,
                                &serde_json::json!({"t": "button_state", "id": id, "on": on}),
                            )
                            .await;
                        }
                        Effect::Eraser(on) => {
                            eprintln!("eraser mode {on}");
                        }
                        Effect::SwitchProfile { target } => {
                            // A hold still held is released first.
                            let rel = shared.runner.lock().await.release_all();
                            apply_effects(&rel);
                            let next = shared.profiles.lock().await.switch(target);
                            if let Some((id, reason)) = next {
                                let ps = shared.profiles.lock().await;
                                if let Some(p) = ps.profiles.iter().find(|p| p.id == id) {
                                    let pw = profile_wire(p, &reason);
                                    let _ = send_msg(&mut stream, &pw).await;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "surface" => {
                // Pen area size changed (rotation, strip resize). The
                // companion uses only its ratio for mapping.
                if matches!(stage, ConnStage::Authed { .. }) {
                    eprintln!(
                        "surface {}x{}",
                        msg.get("w").and_then(|v| v.as_i64()).unwrap_or(0),
                        msg.get("h").and_then(|v| v.as_i64()).unwrap_or(0)
                    );
                }
            }
            _ => {} // unknown / not-yet-wired types ignored per spec
        }
    }

    // Session teardown: release everything, drop pen keys (spec 4).
    if let Some(sid) = shared.sessions.lock().await.session_of(conn_id) {
        shared.pen_key.lock().await.remove(&sid);
        shared.pen_rx.lock().await.remove(&sid);
        let rel = shared.runner.lock().await.release_all();
        apply_effects(&rel);
        release_all("session end");
    }
    shared.sessions.lock().await.remove_conn(conn_id);
}

fn base64_b64(bytes: &[u8]) -> String {
    const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = c.iter().fold(0u32, |a, b| (a << 8) | *b as u32) << (8 * (3 - c.len()));
        for i in 0..4 {
            if i <= c.len() {
                out.push(ALPH[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_b64_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    let mut vals = Vec::with_capacity(s.len());
    for c in s.chars() {
        if c == '=' {
            vals.push(0u8);
            continue;
        }
        let v = match c {
            'A'..='Z' => c as u8 - b'A',
            'a'..='z' => c as u8 - b'a' + 26,
            '0'..='9' => c as u8 - b'0' + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err("bad b64"),
        };
        vals.push(v);
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in vals.chunks(4) {
        if c.len() < 4 {
            break;
        }
        let n = ((c[0] as u32) << 18) | ((c[1] as u32) << 12) | ((c[2] as u32) << 6) | c[3] as u32;
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
    }
    let pad = s.chars().rev().take_while(|&c| c == '=').count();
    out.truncate(out.len().saturating_sub(pad));
    Ok(out)
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().chars().take(32).collect())
        .unwrap_or_else(|_| "chiz-pc".into())
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Local IPv4 addresses for the QR `h` list (default-route address first).
fn local_ipv4_hosts() -> Vec<String> {
    let mut hosts = vec![];
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        // No packets sent: connect() only selects the default route.
        if sock.connect("192.0.2.1:80").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                if let std::net::IpAddr::V4(v4) = addr.ip() {
                    if !v4.is_loopback() {
                        hosts.push(v4.to_string());
                    }
                }
            }
        }
    }
    hosts.push("127.0.0.1".into());
    hosts
}

/// Open a pairing window: token + PIN, QR URI on stdout/log, PBM beside the
/// identity, PIN for manual entry. Tray calls this on "Pair new tablet".
async fn open_pairing(shared: &Arc<Shared>, id: &identity::Identity) {
    let (token, pin) = shared.pairing.lock().await.open(now_secs());
    let hosts = local_ipv4_hosts();
    let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let uri = shared.pairing.lock().await.qr_uri(&host_refs, 47800, &id.fingerprint);
    eprintln!("pairing open for 120 s — PIN: {pin}");
    eprintln!("pairing QR: {uri}");
    match pairing::qr_pbm(&uri) {
        Ok((pbm, ascii)) => {
            eprintln!("{ascii}");
            let path = identity::data_dir().join("pair_qr.pbm");
            if std::fs::write(&path, &pbm).is_ok() {
                eprintln!("QR written to {}", path.display());
            }
        }
        Err(e) => eprintln!("QR render failed: {e}"),
    }
    let _ = token;
}

#[allow(dead_code)]
fn peer_ip_of(_a: &SocketAddr) -> IpAddr {
    _a.ip()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: several control messages in one TCP segment must all
    /// decode without waiting for more socket data (old code read the
    /// socket before draining the buffer and stalled to the 5 s deadline).
    #[tokio::test]
    async fn pipelined_frames_all_decode() {
        let (mut client, mut server) = tokio::io::duplex(65536);
        for i in 0..3 {
            let bytes = control::encode_frame(&serde_json::json!({"t": "ping", "id": i})).unwrap();
            client.write_all(&bytes).await.unwrap();
        }
        drop(client); // EOF after the three frames
        let mut buf = Vec::new();
        let mut ids = vec![];
        loop {
            match read_msg(&mut server, &mut buf).await {
                Ok(Some(v)) => {
                    if v.get("t") == Some(&serde_json::Value::from("_more")) {
                        continue;
                    }
                    ids.push(v["id"].as_i64().unwrap());
                }
                Ok(None) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(ids, vec![0, 1, 2]);
    }

    fn test_shared() -> Arc<Shared> {
        let secret = [7u8; 32];
        let salt = [9u8; 16];
        let key = chiz_core::derive_pen_key(&secret, &salt);
        let mut pen_key = HashMap::new();
        pen_key.insert(9u32, (key, "127.0.0.1".to_string()));
        let mut pen_rx = HashMap::new();
        pen_rx.insert(9u32, PenReceiver::new(9, "127.0.0.1"));
        Arc::new(Shared {
            sessions: Mutex::new(SessionTable::default()),
            auth: Mutex::new(control::AuthTracker::default()),
            unauth_count: Mutex::new(0),
            pairing: Mutex::new(pairing::PairingState::closed()),
            paired: Mutex::new(PairedStore::default()),
            fp: [0xabu8; 32],
            pen_key: Mutex::new(pen_key),
            pen_rx: Mutex::new(pen_rx),
            profiles: Mutex::new(ProfileSet { profiles: vec![], locked: None }),
            runner: Mutex::new(ActionRunner::default()),
            failsafe: std::sync::Mutex::new(Failsafe::default()),
        })
    }

    fn test_datagram() -> Vec<u8> {
        let secret = [7u8; 32];
        let salt = [9u8; 16];
        let key = chiz_core::derive_pen_key(&secret, &salt);
        let r = chiz_core::PenRecord {
            phase: 2, eraser: false, barrel1: false, barrel2: false,
            x: 1000, y: 2000, pressure: 30000, tilt_x: 0, tilt_y: 0,
            distance: 0, t_ms: 500,
        };
        chiz_core::encrypt_datagram(&key, 9, 1, &[r])
    }

    /// Paused pen is dropped before decrypt accounting, and exactly one
    /// transitional `leave` is emitted (spec 8 Pause).
    #[tokio::test]
    async fn paused_pen_drops() {
        let sh = test_shared();
        sh.failsafe.lock().expect("failsafe").pause(PauseReason::Manual);
        let src: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let dg = test_datagram();
        handle_pen_datagram(&sh, &dg, src).await;
        assert!(!sh.failsafe.lock().expect("failsafe").leave_pending);
        handle_pen_datagram(&sh, &dg, src).await; // second: no leave again
        assert_eq!(sh.pen_rx.lock().await[&9].highest_seq, 0); // never accepted
    }

    #[tokio::test]
    async fn unpaused_pen_accepts() {
        let sh = test_shared();
        let src: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        handle_pen_datagram(&sh, &test_datagram(), src).await;
        assert_eq!(sh.pen_rx.lock().await[&9].highest_seq, 1);
    }

    /// The panic switch runs on a foreign OS thread with no runtime and
    /// still pauses + releases (would deadlock if it awaited anything).
    #[test]
    fn panic_from_foreign_thread() {
        let sh = test_shared();
        let moved = Arc::clone(&sh);
        std::thread::spawn(move || emergency_release(&moved)).join().unwrap();
        assert_eq!(sh.failsafe.lock().expect("failsafe").paused(), Some(PauseReason::Panic));
    }
}

#[tokio::main]
async fn main() {
    let cfg = Config::load("companion/config.example.json");
    let profiles = load_profiles();
    eprintln!(
        "profiles: {} loaded{}",
        profiles.profiles.len(),
        profiles
            .profiles
            .iter()
            .find(|p| p.default)
            .map(|p| format!(" (default: {})", p.id))
            .unwrap_or_default()
    );
    let me = hostname();
    let id = identity::load_or_generate(&me).expect("TLS identity");
    eprintln!("cert fingerprint sha256: {}", identity::fp_hex(&id.fingerprint));
    let shared = Arc::new(Shared {
        sessions: Mutex::new(SessionTable::default()),
        auth: Mutex::new(control::AuthTracker::default()),
        unauth_count: Mutex::new(0),
        pairing: Mutex::new(pairing::PairingState::closed()),
        paired: Mutex::new(PairedStore::default()),
        fp: id.fingerprint,
        pen_key: Mutex::new(HashMap::new()),
        pen_rx: Mutex::new(HashMap::new()),
        profiles: Mutex::new(profiles),
        runner: Mutex::new(ActionRunner::default()),
        failsafe: std::sync::Mutex::new({
            let mut f = Failsafe::default();
            if let Err(e) = f.set_hotkey(&cfg.panic_hotkey) {
                eprintln!("bad panic_hotkey {e:?}, keeping default");
            }
            f
        }),
    });

    // Test hook (not a backdoor: only from process environment, used by
    // smoke tests to preload one pairing without the QR/PIN UI).
    // Format: CHIZ_TEST_PAIR=<device_id>:<base64 32-byte secret>
    if let Ok(hook) = std::env::var("CHIZ_TEST_PAIR") {
        for entry in hook.split(',') {
            if let Some((id, sec_b64)) = entry.split_once(':') {
                if let Ok(sec) = base64_b64_decode(sec_b64.trim()) {
                    if sec.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&sec);
                        shared.paired.lock().await.secrets.insert(id.trim().into(), arr);
                        eprintln!("test hook: pre-paired {}", id.trim());
                    }
                }
            }
        }
    }

    // Pairing window (deliberate action only: tray "Pair new tablet";
    // CHIZ_PAIRING=open is the headless/test equivalent).
    if std::env::var("CHIZ_PAIRING").as_deref() == Ok("open") {
        open_pairing(&shared, &id).await;
    }

    let udp = Arc::new(
        UdpSocket::bind(("0.0.0.0", cfg.pen_port))
            .await
            .expect("bind pen UDP"),
    );
    eprintln!("chiz companion listening: control TCP {}, pen UDP {}", cfg.control_port, cfg.pen_port);
    eprintln!(
        "panic hotkey: {} (+ tray Emergency release)",
        shared.failsafe.lock().expect("failsafe").panic_hotkey.join("+")
    );

    // Pen receive task (spec 2: never touches disk, DNS, UI, or control).
    {
        let sh = Arc::clone(&shared);
        let sock = Arc::clone(&udp);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let Ok((n, src)) = sock.recv_from(&mut buf).await else {
                    continue;
                };
                // Copy out: handle_pen_datagram awaits on locks.
                let dg = buf[..n].to_vec();
                handle_pen_datagram(&sh, &dg, src).await;
            }
        });
    }
    // Watchdog ticker: 100 ms stuck-pen checks + stuck-trip auto-pause.
    {
        let sh = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            loop {
                tick.tick().await;
                watchdog_tick(&sh).await;
            }
        });
    }
    // NOTE: the OS global-hotkey thread (RegisterHotKey / XGrabKey, see
    // platform crates) calls `emergency_release(&shared)` from outside the
    // runtime. Tray "Emergency release" calls it too via `tray_action`.

    // mDNS: convenience only, never required (manual IP entry always works).
    let _mdns = mdns::start(&me, cfg.control_port, &id.fingerprint);

    let listener = TcpListener::bind(("0.0.0.0", cfg.control_port))
        .await
        .expect("bind control TCP");
    // TLS 1.3 identity (spec 3): every control byte travels inside this
    // acceptor. No plaintext fallback (spec 12.5).
    let acceptor = tokio_rustls::TlsAcceptor::from(identity::server_config(&id).expect("tls config"));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        let Ok(tls) = acceptor.accept(tcp).await else {
            continue; // handshake failure (wrong version, junk): just close
        };
        let (cfg_c, sh_c) = (cfg.clone(), Arc::clone(&shared));
        tokio::spawn(async move { handle_conn(tls, peer, cfg_c, sh_c).await });
    }
}
