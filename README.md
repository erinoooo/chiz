<div align="center">

<img width="160" alt="Chiz logo: a pen nib drawing between two offset frames" src="docs/logo.svg" />

# Chiz

[![build-and-release](https://github.com/erinoooo/chiz/actions/workflows/release.yml/badge.svg)](https://github.com/erinoooo/chiz/actions/workflows/release.yml)

</div>

Chiz turns an Android tablet into a **screenless pen tablet** for a Windows or
Linux PC. You draw on the tablet with a stylus; the pen drives the PC cursor
with pressure, tilt and hover — while your eyes stay on the monitor. A strip
of touch buttons along the tablet edge fires keyboard shortcuts (undo, brush,
pan, …) and *tells you* what each button does, so you never have to look down.

Free, ad-free, payment-free. The name comes from the Turkish word
*“Çiz”* (“draw”); *Chiz* is the ASCII spelling used for packages and domains.

## How it works

```
┌──────────────┐  UDP pen packets   ┌──────────────┐  OS pen events  ┌────────────┐
│    Tablet    │ ─────────────────▶ │  Companion   │ ──────────────▶ │ Drawing    │
│  pen area    │  (own socket+thread│  (PC: Win/Linux)│  (Windows Ink │ app (Krita │
│  + button    │   so nothing ever  │  mapping,      │   / uinput)   │  GIMP, …)  │
│  strip       │   blocks the pen)  │  pressure curve│               └────────────┘
└──────────────┘  TCP/TLS button events + profiles + speech
```

- **Pen path** — position, pressure, tilt, hover, eraser end and barrel
  buttons stream over encrypted UDP and are injected as a *real* pen device
  (Windows Ink on Windows, a virtual tablet on Linux). Your drawing app just
  works — pick the Windows Ink / tablet API in its settings.
- **Button strip** — virtual buttons live on the tablet but everything about
  them (layouts, shortcuts, spoken names) lives on the PC as *profiles* that
  can switch automatically per app (Krita vs. Blender, …).
- **Audio-first feedback** — rest a finger on a button to hear its name, tap
  to fire it with a click. No haptics needed, no looking at the tablet.
- **Local only** — one TCP control connection + one UDP pen stream over your
  LAN. No accounts, no cloud, no telemetry.

See [`Chizv1Specification.md`](Chizv1Specification.md) for the full protocol
and behavior spec, and [`docs/`](docs/) for research notes and guides.

## Requirements

| Side | Needs |
| --- | --- |
| Tablet | Android 10+, a stylus, 5 GHz Wi-Fi recommended |
| PC | Windows 10 (1809+) / 11, or current Ubuntu LTS / Fedora |
| Network | Same LAN, no client isolation; PC on Ethernet is ideal |
| Apps | Krita, GIMP, Blender, … (anything with pen support) |

## Quick start

1. **Install the companion** on the PC — grab `chiz-ui-*` from
   [Releases](https://github.com/erinoooo/chiz/releases) (or build below).
2. **Pair once.** On the PC open *Pair new tablet* (120-second window), then
   on the tablet either **scan the QR code** (preferred) or pick the PC and
   type the 8-digit **PIN** — only on a network you trust. Wrong PIN 5 times
   closes the window.
3. **Draw.** Open Krita, put the pen down on the tablet's pen area, and draw.
   Hover moves the cursor without inking; flip the pen for the eraser.
4. **Buttons.** Rest a finger to hear a button, tap to fire, hold *Pan* to
   pan while another finger taps. The layout follows the active app.

Stuck cursor or a key that won't release? Hit **`Ctrl+Alt+Shift+F12`** on the
PC, or 3-finger-tap the tablet strip — see [`docs/failsafe.md`](docs/failsafe.md).

## Button strip & profiles

The default profile gives you an 8-button strip: Undo, Redo, Brush, Eraser
(toggle), Pan (hold), Zoom in/out, Save. Profiles are JSON files in
`companion/profiles/` — copy one to share it. Buttons can tap, hold
(modifiers), toggle, click the mouse, run short macros, or switch profiles.

## Security model

- Pairing is explicit and short-lived; every tablet gets its own secret.
- Control channel: TLS 1.3 with certificate pinning (no system trust store).
- Pen stream: AES-256-GCM with a fresh key per session; forged, replayed
  and out-of-order packets are dropped and counted.
- By default the PC refuses non-local connections. Nothing phones home.

## This repository

```text
protocol/    wire-format reference + test vectors (Python)
companion/   PC program (Rust): core protocol, Windows + Linux injectors,
             networking/pairing/profiles runtime, egui screens, tray model
android/     tablet app (Kotlin): pen capture, button strip, audio feedback,
             TLS + pairing, Compose screens
docs/        research notes, failsafe guide, user notes
```

## Developing

Prerequisites per part: Rust stable, Python 3.12 + `cryptography`, JDK 17 +
Kotlin 2.1.20 (protocol tests only — no Android SDK needed for those).

```sh
python3 protocol/verify_m0.py && python3 protocol/verify_m1m2.py  # vectors
cargo test --workspace                                          # from companion/
kotlinc android/app/src/main/java/xyz/chiz/tablet/proto/*.kt android/jvmtest/TestMain.kt -d /tmp/kt-out
java -cp "/tmp/kt-out:<kotlinc>/lib/kotlin-stdlib.jar" xyz.chiz.tablet.jvmtest.TestMainKt
```

Full Android APK: open `android/` in Android Studio (or `gradle :app:assembleDebug`).
Windows installer notes: `companion/packaging/`.

Cutting a release: push a tag — `git tag v1.0.0 && git push origin v1.0.0` —
and CI attaches the Linux + Windows companions and the debug APK.

## Status & roadmap

Working now: pairing (QR + PIN), encrypted pen with pressure/tilt/hover/
eraser/barrel, per-app profiles, button strip with speech + earcons, panic
switches, mapping + pressure curves, Linux + Windows injection, mDNS.

Still to wire on a display host: the `eframe` runner and tray icon loop
(screens and menu model are done), then device testing per the acceptance
suite in the spec. After v1: screen streaming, USB transport, macOS, relay.
