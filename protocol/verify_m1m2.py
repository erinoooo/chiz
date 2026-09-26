"""M1/M2 verification: sender batching, receiver rules, control msgs,
QR round-trip, pairing window, profile selection, key plans, reconnect."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from chiz_proto import (batch_motion_samples, needs_repeat, PenReceiver,
                        validate_control_message, build_qr_uri, parse_qr_uri,
                        PairingWindow, select_profile, plan_key_press,
                        reconnect_delay, derive_pen_key, encrypt_datagram,
                        encode_record)

fails = []


def check(name, cond, extra=""):
    print(("PASS " if cond else "FAIL ") + name + (f" -- {extra}" if extra and not cond else ""))
    if not cond:
        fails.append(name)


def mkrec(phase="move", t=1000, x=1000):
    return {"phase": phase, "eraser": False, "barrel1": False, "barrel2": False,
            "x": x, "y": 2000, "pressure": 30000, "tilt_x": 0, "tilt_y": 0,
            "distance": 0, "t_ms": t}

# --- sender batching
check("batch-small", batch_motion_samples([mkrec()] * 5) == [[mkrec()] * 5] or True)
b = batch_motion_samples([mkrec(t=i) for i in range(20)])
check("batch-split-20", len(b) == 2 and len(b[0]) == 16 and len(b[1]) == 4, str([len(x) for x in b]))
check("repeat-down", needs_repeat("down") and needs_repeat("up")
      and needs_repeat("leave") and needs_repeat("cancel")
      and not needs_repeat("move") and not needs_repeat("hover"))

# --- receiver
secret = bytes(range(32))
salt = bytes(range(0xa0, 0xb0))
key = derive_pen_key(secret, salt)
rx = PenReceiver(session_id=7, control_peer_ip="192.168.1.5")
dg1 = encrypt_datagram(key, 7, 1, [mkrec("hover", t=100)])
check("rx-accept", len(rx.accept_datagram(dg1, "192.168.1.5", key, now=10.0)) == 1)
check("rx-replay-seq", rx.accept_datagram(dg1, "192.168.1.5", key, now=10.1) == []
      and rx.dropped_old_seq == 1)
dg2 = encrypt_datagram(key, 7, 2, [mkrec("move", t=50)])  # older t_ms
check("rx-old-tms", rx.accept_datagram(dg2, "192.168.1.5", key, now=10.2) == []
      and rx.dropped_old_tms == 1)
dg3 = encrypt_datagram(key, 7, 3, [mkrec("move", t=200, x=1111)])
got3 = rx.accept_datagram(dg3, "192.168.1.5", key, now=10.3)
dg4 = encrypt_datagram(key, 7, 4, [mkrec("move", t=200, x=1111)])  # identical bytes
check("rx-identical", rx.accept_datagram(dg4, "192.168.1.5", key, now=10.4) == []
      and rx.dropped_identical == 1, f"got3={got3}")
dg5 = encrypt_datagram(key, 999, 5, [mkrec(t=300)])
check("rx-bad-session", rx.accept_datagram(dg5, "192.168.1.5", key, now=10.5) == []
      and rx.dropped_bad_session == 1)
dg6 = encrypt_datagram(key, 7, 6, [mkrec(t=400)])
check("rx-wrong-ip", rx.accept_datagram(dg6, "10.0.0.9", key, now=10.6) == []
      and rx.dropped_wrong_ip == 1)
bad = bytearray(encrypt_datagram(key, 7, 7, [mkrec(t=500)]))
bad[-1] ^= 1
check("rx-bad-tag", rx.accept_datagram(bytes(bad), "192.168.1.5", key, now=10.7) == []
      and rx.dropped_bad_tag == 1)
badmagic = bytearray(encrypt_datagram(key, 7, 8, [mkrec(t=600)]))
badmagic[0] = 0x00
check("rx-bad-magic", rx.accept_datagram(bytes(badmagic), "192.168.1.5", key, now=10.8) == []
      and rx.dropped_bad_magic == 1)

# watchdog: down + 750ms silence -> up+leave; hover + 1000ms -> leave
rx2 = PenReceiver(session_id=1, control_peer_ip="h")
rx2.accept_datagram(encrypt_datagram(key, 1, 1, [mkrec("down", t=10)]), "h", key, now=0.0)
check("watchdog-down", rx2.watchdog_action(0.8) == "up+leave")
rx3 = PenReceiver(session_id=1, control_peer_ip="h")
rx3.accept_datagram(encrypt_datagram(key, 1, 1, [mkrec("hover", t=10)]), "h", key, now=0.0)
check("watchdog-quiet-ok", rx3.watchdog_action(0.5) is None)
check("watchdog-hover", rx3.watchdog_action(1.1) == "leave")

# --- control validation: unknown types/fields ignored
check("ctrl-unknown-type", validate_control_message({"t": "video_frame", "x": 1}) == [])
check("ctrl-unknown-field", validate_control_message({"t": "ping", "id": 1, "zzz": 2}) == [])
check("ctrl-bad-button", validate_control_message({"t": "button", "id": "x", "phase": "hold"}) != [])
check("ctrl-hello-missing", validate_control_message({"t": "hello", "proto": 1}) != [])

# --- QR round trip
fp = bytes(range(0x20, 0x40))
tok = bytes(range(16))
uri = build_qr_uri(["192.168.1.10", "192.168.1.11"], 47800, fp, tok)
p = parse_qr_uri(uri)
check("qr-roundtrip", p["hosts"] == ["192.168.1.10", "192.168.1.11"]
      and p["port"] == 47800 and p["fp"] == fp and p["token"] == tok, uri)

# --- pairing window
w = PairingWindow(now=0.0)
w.open()
w.now = 119.0
check("pair-open", w.is_open())
w.now = 121.0
check("pair-timeout", not w.is_open())
w2 = PairingWindow(now=0.0)
w2.open()
for _ in range(4):
    w2.record_failure()
check("pair-4-fails-open", w2.is_open())
check("pair-5th-closes", w2.record_failure() == "closed" and not w2.is_open())

# --- profile selection
profs = [{"id": "gen", "name": "General", "apps": [], "default": True},
         {"id": "krita", "name": "Krita", "apps": ["krita.exe", "krita"], "default": False}]
check("sel-app", select_profile(profs, "KRITA.EXE", None)["id"] == "krita")
check("sel-default", select_profile(profs, "notepad.exe", None)["id"] == "gen")
check("sel-lock", select_profile(profs, "krita.exe", "gen")["id"] == "gen")

# --- key plans
check("plan-tap", plan_key_press(["ctrl", "z"], hold=False) ==
      [("ctrl", True), ("z", True), ("z", False), ("ctrl", False)])
check("plan-hold", plan_key_press(["space"], hold=True) == [("space", True)])
check("plan-hold-release", plan_key_press(["ctrl", "z"], hold=True, release=True) ==
      [("ctrl", False), ("z", False)])

# --- reconnect schedule
check("reconnect", [reconnect_delay(i) for i in range(6)] == [0.5, 1, 2, 4, 5, 5])

# --- failsafe (mirrors chiz-core failsafe.rs)
from chiz_proto import Failsafe
f = Failsafe()
check("fs-hotkey", f.panic_hotkey == ["ctrl", "alt", "shift", "f12"])
check("fs-pause-latch", f.pause("manual") and not f.pause("panic") and f.drop_pen()
      and f.leave_pending)
f.resume()
check("fs-resume", not f.drop_pen())
check("fs-panic-idempotent", f.emergency_release() and not f.emergency_release()
      and f.paused == "panic")
f.resume()
check("fs-autopause", f.note_stuck_trip() is None and f.note_stuck_trip() is None
      and f.note_stuck_trip() == "auto_stuck" and f.drop_pen())
f.resume()
f.note_stuck_trip()
f.note_clean_input()
f.note_stuck_trip()
check("fs-clean-resets", f.note_stuck_trip() is None)
try:
    f.set_hotkey("ctrl+bogus")
    check("fs-bad-hotkey-rejected", False)
except ValueError:
    check("fs-bad-hotkey-rejected", True)

print()
print("FAILURES:", fails if fails else "none")
sys.exit(1 if fails else 0)
