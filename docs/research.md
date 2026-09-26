# Web-research concretions (vague spec parts → chosen APIs)

All verified via web search during M0; keep links in git history, not here.

1. Tilt/orientation (spec 5 table): `AXIS_TILT` = radians from vertical
   (0 perpendicular, PI/2 flat), `AXIS_ORIENTATION` = azimuth 0..PI, 0 = up.
   Formula `tilt_x = deg(atan(tan(tilt)*sin(orient)))`,
   `tilt_y = deg(-atan(tan(tilt)*cos(orient)))` implemented once in
   `android/PenAndTouch.kt` + `protocol/chiz_proto.py`; AT-5 decides sign.
2. Windows pen (spec 9): `CreateSyntheticPointerDevice(PT_PEN, 1,
   POINTER_FEEDBACK_NONE)` + `InjectSyntheticPointerInput` with one
   `POINTER_TYPE_INFO`; `POINTER_PEN_INFO.pressure` 0..1024,
   `penMask = PRESSURE|TILT_X|TILT_Y`, `penFlags` BARREL/INVERTED/ERASER;
   barrel2 ignored, hover distance ignored. See `platform-windows`.
3. Linux tablet (spec 10): uinput "Chiz Pen" (BTN_TOOL_PEN/RUBBER, TOUCH,
   STYLUS/STYLUS2; ABS ranges X/Y=union-1, PRESSURE 8191, DISTANCE 255,
   TILT ±90) + "Chiz Keys" (all spec-11 keys, REL_X/Y declared-unsent).
   python-evdev patterns confirm SYN_REPORT grouping. See `platform-linux`.
4. TLS identity (spec 3): companion self-signed ECDSA P-256 via `rcgen` +
   `rustls` (TLS 1.3 only); tablet pins SHA-256 DER fingerprint, never the
   system store; QR carries fp+token, PIN proof binds observed fp.
5. Discovery (spec 3): companion advertises `_chiz._tcp` with TXT
   `v/id/name` via `mdns-sd`; tablet browses with `NsdManager`
   (`discoverServices` → `resolveService`), plus manual IP entry fallback.
6. Audio latency (spec 7): `SoundPool` (max 4, USAGE_GAME/SONIFICATION) for
   earcons; single `TextToSpeech` + `synthesizeToFile` cache keyed by
   text/engine/locale/rate (LRU 20 MB) for <40 ms touch-to-sound.
7. Foreground service (spec 6): type `connectedDevice` + Wi-Fi low-latency
   lock + partial wake lock; A14 requires `FOREGROUND_SERVICE_CONNECTED_DEVICE`
   plus a network/multicast permission. `requestUnbufferedDispatch` for pen.

Open spec decisions (14): license, launch languages (EN+TR suggested),
Windows code-signing budget — left for the owner.
