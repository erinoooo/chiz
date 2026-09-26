# Chiz v1 — build status

## Verified (run these)

```sh
python3 protocol/verify_m0.py    # spec §5 + §13 vectors: crypto, datagram, mapping, curves
python3 protocol/verify_m1m2.py  # sender/receiver, control, QR, pairing, profiles, keys
cargo test --workspace           # Rust: 26 core + 5 linux + 4 windows + 22 ui (run from companion/)
cargo build --workspace          # whole companion workspace compiles
# Windows syscalls compile-check (needs the MSVC target std, no linker):
#   rustup target add x86_64-pc-windows-msvc
#   cargo check -p chiz-platform-windows --target x86_64-pc-windows-msvc
```
Socket smoke test (proves TLS 1.3 + pinned fp, challenge→hello→welcome→
profile, live UDP pen, button tap/hold/toggle→button_state, unknown-id
ignore, not_paired, busy — needs the test-pair hook):

```sh
cd companion && cargo build -p chiz-ui
S1=$(python3 -c "import base64; print(base64.b64encode(bytes(range(32))).decode())")
S2=$(python3 -c "import base64; print(base64.b64encode(bytes([255-i for i in range(32)])).decode())")
export CHIZ_DATA_DIR=/tmp/chiz-id CHIZ_TEST_PAIR="<dev-id>:$S1,dev-B:$S2" CHIZ_BUSY_CHECK=1
export CHIZ_PAIR_TEST=1 CHIZ_PAIRING=open CHIZ_SERVER_LOG=/tmp/chiz.log
./target/debug/chiz-ui > /tmp/chiz.log 2>&1 &
python3 ../protocol/smoke_auth.py   # PAIR-E2E-OK + AUTH-E2E-OK
```

## What exists (spec §14 layout)

```text
protocol/       test_vectors.json, chiz_proto.py (reference impl),
                verify_m0/m1m2.py, smoke_auth.py (socket smoke)
companion/      Rust workspace: core (crypto/pen/mapping/curve/receiver/
                  control/pairing), platform-windows (SyntheticPointer spec),
                  platform-linux (uinput spec),
                ui (control.rs framing/sessions/rate-limits,
                    pen.rs UDP loop, main.rs listeners + HMAC auth +
                    profiles + button dispatch + failsafe,
                    identity.rs TLS 1.3 ECDSA P-256 via rustls/rcgen),
                config.example.json, profiles/default.json + krita.json,
                packaging/ (udev rule, modules-load, chiz-setup.sh)
android/        Gradle app (minSdk 29): proto/ (pure logic, JVM-verified),
                net/ (TLS control, UDP pen, mDNS, Keystore store, audio,
                foreground service), ui/ (Compose screens, pen/strip views,
                DataStore settings). Run: kotlinc proto/*.kt jvmtest/*.kt
docs/           research.md (web-verified API choices)
```

## Milestone mapping

- M0 done: framing/TLS-pinning design, pairing proofs, hello/welcome,
  pen encode/decode — vectors pass in Python and Rust.
- M1/M2 logic done and tested: sender batching/split/repeat rules,
  receiver drop counters (magic/session/IP/seq/tag/t_ms/identical),
  watchdogs (750 ms down, 1000 ms hover), mapping + rotation + aspect,
  pressure curves. OS injection itself needs Windows/Linux hardware.
- M4–M6 logic done: touch state machine both announce modes, key press
  plans, profile validation + selection, QR build/parse, pairing window,
  reconnect schedule.
- M4 runtime wired: profiles load at startup, `profile` follows `welcome`,
  `button` down/up dispatches tap/hold/toggle/mouse/macro/profile actions,
  toggles report `button_state`, macros run on a worker task, held keys
  release on session end / profile switch. Key/mouse effects log until the
  platform injectors land (M3).
- Emergency off switch: `Failsafe` latch (manual/panic/auto) in core, live
  UDP receive loop with pause gate + 100 ms watchdog ticker (3 stuck trips
  auto-pause), sync `emergency_release` callable from any OS thread, PC
  hotkey `ctrl+alt+shift+f12` (configurable) + tray items, tablet 3-finger
  panic gesture. See `docs/failsafe.md`.
- TLS identity: first-run ECDSA P-256 self-signed cert (CN `chiz-<host>`,
  10 y, `cert.der`+`key.bin` 0600), TLS 1.3-only server config, SHA-256
  fingerprint pinning, QR codec in core. `CHIZ_DATA_DIR` overrides storage.
- Pairing flow: 120 s window with token + 8-digit PIN, QR URI + PBM/ASCII
  render, `pair_request` verify (QR token / PIN HMAC bound to observed fp),
  `pair_ok` + fresh challenge, `pair_fail` with attempts left.
- Discovery: `_chiz._tcp` mDNS advertise (TXT v/id/name), Avahi fallback
  note, manual IP entry always available.
- Linux uinput: "Chiz Pen" + "Chiz Keys" creation via raw ioctls, exact
  event sequences (tool switch, barrel changes, SYN batching) unit-tested
  on a recording backend — no `/dev/uinput` needed for tests.
- Windows syscalls: synthetic pen (`PT_PEN`, per-phase flags, 0–1024
  pressure, barrel/eraser flags, leave-repeat hook), single-call `SendInput`
  combos with scancodes + extended flags, mouse, monitor enum, foreground
  exe + once-per-app UIPI warning, `RegisterHotKey` panic thread. Pure
  builders tested on Linux; syscalls check-clean on the MSVC target.
- Screens + tray: egui Status/Mapping/Pressure/Profiles/Tablets/Settings
  (`ui/screens.rs`, `eframe` runner left for a display host), tray menu
  model + dispatch (`ui/tray.rs`, icon loop left for a display host).
- Still needs hardware/SDKs: actual `SendInput`/uinput calls, `NsdManager`
  + `SSLSocket` + `SoundPool`/`TTS` on Android, mDNS/TLS wiring, egui
  screens, installers. Stubs point at the exact APIs (docs/research.md).
