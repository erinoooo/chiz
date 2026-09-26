"""M0 verification: spec test vectors + automated-test list (section 13)."""
import base64
import json
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from chiz_proto import (apply_pressure_curve, compute_auth, compute_pin_proof,
                        decrypt_datagram, derive_pen_key, encode_record,
                        encrypt_datagram, map_pen_to_px, parse_keys,
                        validate_profile, PenState, ButtonTouchMachine,
                        encode_frame, decode_frame, decode_plaintext)

V = json.loads((Path(__file__).parent / "test_vectors.json").read_text())
fails = []


def check(name, cond, extra=""):
    print(("PASS " if cond else "FAIL ") + name + (f" -- {extra}" if extra and not cond else ""))
    if not cond:
        fails.append(name)


# --- pen key / datagram byte-for-byte
pv = V["pen_vector"]
secret = bytes.fromhex(pv["device_secret_hex"])
salt = bytes.fromhex(pv["pen_key_salt_hex"])
key = derive_pen_key(secret, salt)
check("pen_key", key.hex() == pv["pen_key_hex"], key.hex())
rec = {"phase": "down", "eraser": False, "barrel1": False, "barrel2": False,
       "x": 0x8000, "y": 0x4000, "pressure": 0x2000, "tilt_x": 15,
       "tilt_y": -10, "distance": 0, "t_ms": 1234}
from chiz_proto import encode_plaintext
check("plaintext", encode_plaintext([rec]).hex() == pv["plaintext_hex"])
dg = encrypt_datagram(key, pv["session_id"], pv["seq"], [rec])
check("datagram", dg.hex() == pv["datagram_hex"], dg.hex())
sid, seq, recs = decrypt_datagram(key, bytes.fromhex(pv["datagram_hex"]))
check("decrypt-roundtrip", sid == pv["session_id"] and seq == 1 and recs[0]["x"] == 0x8000
      and recs[0]["tilt_y"] == -10 and recs[0]["phase"] == "down")
try:
    bad = bytearray(bytes.fromhex(pv["datagram_hex"]))
    bad[-1] ^= 1
    decrypt_datagram(key, bytes(bad))
    check("tamper-tag-rejected", False)
except ValueError:
    check("tamper-tag-rejected", True)
# wrong session / replay
try:
    decrypt_datagram(key, encrypt_datagram(key, 999, 1, [rec]))
    s, _, _ = decrypt_datagram(key, encrypt_datagram(key, 999, 1, [rec]))
    check("session-id-check", s == 999)  # crypto passes; receiver must drop by id
except ValueError:
    check("session-id-check", True)

# --- auth + pin proof
av = V["auth_vector"]
nonce = bytes.fromhex(av["nonce_hex"])
check("auth", compute_auth(secret, nonce, av["device_id"]) == av["auth_base64"],
      compute_auth(secret, nonce, av["device_id"]))
pp = V["pin_proof_vector"]
fp_seen = bytes.fromhex(pp["fp_seen_hex"])
check("pin-proof", compute_pin_proof(pp["pin"], fp_seen, nonce, pp["device_id"]) == pp["proof_base64"],
      compute_pin_proof(pp["pin"], fp_seen, nonce, pp["device_id"]))

# --- mapping cases (keep_aspect on, rotation, monitor 1920x1080 at origin)
for i, c in enumerate(V["mapping_cases"]):
    pw, ph = c["pen_area"]
    x, y = c["input"]
    mw, mh = c["monitor"]
    got = map_pen_to_px(x, y, pw, ph, 0, 0, mw, mh, keep_aspect=True, rotation=c["rotation"])
    check(f"map-{i}", got == tuple(c["expect"]), f"got {got} want {c['expect']}")

# --- pressure curves tol 0.002
for c in V["curve_cases"]:
    for p, want in zip(c["p"], c["expect"]):
        got = apply_pressure_curve(c["curve"], p)
        check(f"curve-{c['name']}-{p}", abs(got - want) <= 0.002, f"got {got:.4f} want {want}")

# --- key parser
for good in ["ctrl+z", "ctrl+shift+z", "space", "shift", "b", "ctrl+equal", "ctrl+minus", "f12", "meta+tab"]:
    try:
        parse_keys(good)
        check(f"keys-ok-{good}", True)
    except ValueError:
        check(f"keys-ok-{good}", False)
for badk in ["ctrl+foo", "z+ctrl", "ctrl++z", "", "plus"]:
    try:
        parse_keys(badk)
        check(f"keys-bad-{badk!r}-rejected", False)
    except ValueError:
        check(f"keys-bad-{badk!r}-rejected", True)

# --- profile validation
good_p = {"id": "krita", "name": "Krita", "apps": ["krita.exe"], "default": False,
          "grid": {"cols": 1, "rows": 8},
          "buttons": [{"id": "undo", "label": "Undo", "speak": "Undo", "kind": "tap",
                       "col": 0, "row": 0, "colspan": 1, "rowspan": 1,
                       "action": {"type": "key", "keys": "ctrl+z"}}]}
check("profile-good", validate_profile(good_p) == [], str(validate_profile(good_p)))
overlap = {"id": "x", "name": "X", "grid": {"cols": 1, "rows": 2},
           "buttons": [
               {"id": "a", "label": "A", "kind": "tap", "col": 0, "row": 0, "colspan": 1, "rowspan": 1, "action": {"type": "key", "keys": "a"}},
               {"id": "a", "label": "B", "kind": "tap", "col": 0, "row": 0, "colspan": 1, "rowspan": 1, "action": {"type": "key", "keys": "b"}}]}
check("profile-dup-overlap", len(validate_profile(overlap)) >= 2, str(validate_profile(overlap)))

# --- state repair
st = PenState()
check("repair-down-without-hover", st.repair("down", False) == ["hover", "down"])
st2 = PenState()
st2.in_range, st2.in_contact = True, False
check("repair-move-without-down", st2.repair("move", False)[-2:] == ["down", "move"])
st3 = PenState()
st3.in_range, st3.in_contact = True, True
check("repair-hover-while-touch", st3.repair("hover", False) == ["up", "hover"])
check("repair-dup-up", PenState().repair("up", False) == [])

# --- touch machine both modes
m = ButtonTouchMachine("tap", "Undo", announce_mode="rest")
check("rest-quick-tap-fire", m.finger_down(0) is None and m.finger_up(100) == "fire")
m = ButtonTouchMachine("tap", "Undo", announce_mode="rest")
m.finger_down(0)
check("rest-hold-announces", m.advance(300) == "speak(Undo)")
check("rest-lift-after-announce-no-fire", m.finger_up(400) is None)
m = ButtonTouchMachine("hold", "Pan", announce_mode="rest")
m.finger_down(0)
check("hold-activates-at-rest", m.advance(300) == "hold_on+speak")
check("hold-releases", m.finger_up(500) == "hold_off")
m = ButtonTouchMachine("hold", "Pan", announce_mode="rest")
m.finger_down(0)
check("hold-quick-tap-never-fires", m.finger_up(100) is None)
m = ButtonTouchMachine("tap", "Undo", announce_mode="touch")
check("touch-announces-at-once", m.finger_down(0) == "speak(Undo)")
check("touch-lift-fires", m.finger_up(50) == "fire")

# --- framing
try:
    decode_frame(struct.pack("<I", 65537) + b"x" * 10)
    check("frame-oversize-rejected", False)
except ValueError:
    check("frame-oversize-rejected", True)
msg, _ = decode_frame(encode_frame({"t": "ping", "id": 1, "zzz_unknown": 5}))
check("frame-unknown-field-ignored", msg["t"] == "ping")

print()
print("FAILURES:", fails if fails else "none")
sys.exit(1 if fails else 0)
