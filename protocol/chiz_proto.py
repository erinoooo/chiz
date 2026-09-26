"""Chiz v1 protocol reference implementation (M0).

Pure-Python version of the logic that the Rust core and Kotlin tablet app
must reproduce byte-for-byte. Covers spec sections 4, 5, 7, 8, 11:
framing, HMAC auth, PIN proof, HKDF pen key, AES-GCM pen datagrams,
mapping, pressure curve, key-string parser, profile validation,
pen state repair, button touch state machine.

Crypto uses only the stdlib except AES-GCM, which needs `cryptography`.
Install with: pip install cryptography
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import json
import math
import re
import struct

PROTO_VERSION = 1
MAGIC = 0xC1
MAX_FRAME = 65536

# ---------------------------------------------------------------- crypto

def hkdf_sha256(ikm: bytes, salt: bytes, info: bytes, length: int = 32) -> bytes:
    """RFC 5869 HKDF-SHA256 (extract + expand)."""
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    okm = b""
    t = b""
    for i in range(1, (length + 255) // 32 + 1):
        t = hmac.new(prk, t + info + bytes([i]), hashlib.sha256).digest()
        okm += t
    return okm[:length]


def derive_pen_key(device_secret: bytes, pen_key_salt: bytes) -> bytes:
    assert len(device_secret) == 32 and len(pen_key_salt) == 16
    return hkdf_sha256(device_secret, pen_key_salt, b"chiz-pen-v1", 32)


def compute_auth(device_secret: bytes, nonce: bytes, device_id: str) -> str:
    msg = b"chiz-auth-v1" + nonce + device_id.encode("utf-8")
    return base64.b64encode(hmac.new(device_secret, msg, hashlib.sha256).digest()).decode()


def compute_pin_proof(pin: str, fp_seen: bytes, nonce: bytes, device_id: str) -> str:
    msg = b"chiz-pair-v1" + fp_seen + nonce + device_id.encode("utf-8")
    return base64.b64encode(hmac.new(pin.encode("ascii"), msg, hashlib.sha256).digest()).decode()


def const_time_equal(a: bytes, b: bytes) -> bool:
    return hmac.compare_digest(a, b)

# ---------------------------------------------------------------- framing

def encode_frame(obj: dict) -> bytes:
    payload = json.dumps(obj, separators=(",", ":")).encode("utf-8")
    if len(payload) > MAX_FRAME:
        raise ValueError("frame too large")
    return struct.pack("<I", len(payload)) + payload


def decode_frame(buf: bytes) -> tuple[dict, int]:
    """Returns (message, bytes_consumed). Raises on oversize / incomplete."""
    if len(buf) < 4:
        raise ValueError("incomplete length prefix")
    (n,) = struct.unpack_from("<I", buf, 0)
    if n > MAX_FRAME:
        raise ValueError("oversized frame")
    if len(buf) < 4 + n:
        raise ValueError("incomplete frame")
    obj = json.loads(buf[4:4 + n].decode("utf-8"))
    return obj, 4 + n

# ---------------------------------------------------------------- pen records

PHASES = {"hover": 0, "down": 1, "move": 2, "up": 3, "leave": 4, "cancel": 5}
PHASES_INV = {v: k for k, v in PHASES.items()}


def encode_record(rec: dict) -> bytes:
    flags = PHASES[rec["phase"]] & 0x07
    if rec.get("eraser"):
        flags |= 0x08
    if rec.get("barrel1"):
        flags |= 0x10
    if rec.get("barrel2"):
        flags |= 0x20
    return struct.pack("<BBHHHbbHI",
                       flags, 0,
                       rec["x"] & 0xFFFF, rec["y"] & 0xFFFF,
                       rec["pressure"] & 0xFFFF,
                       rec["tilt_x"], rec["tilt_y"],
                       rec["distance"] & 0xFFFF,
                       rec["t_ms"] & 0xFFFFFFFF)


def decode_record(buf: bytes) -> dict:
    flags, _, x, y, pressure, tx, ty, dist, t_ms = struct.unpack("<BBHHHbbHI", buf[:16])
    return {"phase": PHASES_INV[flags & 0x07], "eraser": bool(flags & 0x08),
            "barrel1": bool(flags & 0x10), "barrel2": bool(flags & 0x20),
            "x": x, "y": y, "pressure": pressure,
            "tilt_x": tx, "tilt_y": ty, "distance": dist, "t_ms": t_ms}


def encode_plaintext(records: list[dict]) -> bytes:
    assert 0 <= len(records) <= 16
    return bytes([len(records)]) + b"".join(encode_record(r) for r in records)


def decode_plaintext(data: bytes) -> list[dict]:
    count = data[0]
    assert 0 <= count <= 16 and len(data) == 1 + 16 * count
    return [decode_record(data[1 + 16 * i:1 + 16 * (i + 1)]) for i in range(count)]


def pen_nonce(seq: int) -> bytes:
    return struct.pack("<I", seq) + b"\x00" * 8


def encrypt_datagram(pen_key: bytes, session_id: int, seq: int, records: list[dict]) -> bytes:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    header = struct.pack("<BBII", MAGIC, 0, session_id, seq)
    ct_with_tag = AESGCM(pen_key).encrypt(pen_nonce(seq), encode_plaintext(records), header)
    return header + ct_with_tag


def decrypt_datagram(pen_key: bytes, datagram: bytes) -> tuple[int, int, list[dict]]:
    from cryptography.exceptions import InvalidTag
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    if len(datagram) < 10 + 1 + 16:
        raise ValueError("datagram too short")
    magic, _, session_id, seq = struct.unpack_from("<BBII", datagram, 0)
    if magic != MAGIC:
        raise ValueError("bad magic")
    header, blob = datagram[:10], datagram[10:]
    try:
        pt = AESGCM(pen_key).decrypt(pen_nonce(seq), blob, header)
    except InvalidTag as e:
        raise ValueError("bad tag") from e
    return session_id, seq, decode_plaintext(pt)


def android_tilt_to_degrees(tilt_rad: float, orient_rad: float) -> tuple[int, int]:
    """Spec section 5: AXIS_TILT (rad from vertical, clamp 89 deg) +
    AXIS_ORIENTATION (rad, 0 = pointing up, clockwise positive).
    Verified against developer.android.com docs: AXIS_TILT 0 = perpendicular,
    PI/2 = flat; AXIS_ORIENTATION 0..PI for stylus azimuth.
    Kept in one function so a single sign flip fixes device variance (spec).
    """
    tilt = min(max(tilt_rad, 0.0), math.radians(89))
    tx = round(math.degrees(math.atan(math.tan(tilt) * math.sin(orient_rad))))
    ty = round(math.degrees(-math.atan(math.tan(tilt) * math.cos(orient_rad))))
    return (max(-90, min(90, tx)), max(-90, min(90, ty)))

# ---------------------------------------------------------------- mapping (spec 8)

def rotate_uv(x: float, y: float, rotation: int) -> tuple[float, float]:
    if rotation == 0:
        return x, y
    if rotation == 90:
        return 1 - y, x
    if rotation == 180:
        return 1 - x, 1 - y
    if rotation == 270:
        return y, 1 - x
    raise ValueError("rotation must be 0/90/180/270")


def map_pen_to_px(x: float, y: float, pen_w: int, pen_h: int,
                  mon_x: int, mon_y: int, mon_w: int, mon_h: int,
                  area: dict | None = None, keep_aspect: bool = True,
                  rotation: int = 0) -> tuple[int, int]:
    """Full mapping pipeline. area is {x,y,w,h} 0..1 within monitor."""
    u, v = rotate_uv(x, y, rotation)
    rp = pen_w / pen_h
    if rotation in (90, 270):
        rp = pen_h / pen_w
    ax = area or {"x": 0, "y": 0, "w": 1, "h": 1}
    area_w = mon_w * ax["w"]
    area_h = mon_h * ax["h"]
    ra = area_w / area_h
    if keep_aspect:
        if rp > ra:
            fw = ra / rp
            u = min(1.0, max(0.0, (u - (1 - fw) / 2) / fw))
        elif rp < ra:
            fh = rp / ra
            v = min(1.0, max(0.0, (v - (1 - fh) / 2) / fh))
    left = mon_x + ax["x"] * mon_w
    top = mon_y + ax["y"] * mon_h
    return (round(left + u * (area_w - 1)), round(top + v * (area_h - 1)))

# ---------------------------------------------------------------- pressure curve (spec 8)

def _bezier(c: list[float], t: float) -> tuple[float, float]:
    x1, y1, x2, y2 = c
    mt = 1 - t
    x = 3 * mt * mt * t * x1 + 3 * mt * t * t * x2 + t ** 3
    y = 3 * mt * mt * t * y1 + 3 * mt * t * t * y2 + t ** 3
    return x, y


def apply_pressure_curve(curve: list[float], p: float) -> float:
    lo, hi = 0.0, 1.0
    for _ in range(12):  # spec: bisection, 12 iterations
        mid = (lo + hi) / 2
        if _bezier(curve, mid)[0] < p:
            lo = mid
        else:
            hi = mid
    return _bezier(curve, (lo + hi) / 2)[1]

# ---------------------------------------------------------------- key strings (spec 11)

EVDEV_LETTERS = {"a": 30, "b": 48, "c": 46, "d": 32, "e": 18, "f": 33, "g": 34,
                 "h": 35, "i": 23, "j": 36, "k": 37, "l": 38, "m": 50, "n": 49,
                 "o": 24, "p": 25, "q": 16, "r": 19, "s": 31, "t": 20, "u": 22,
                 "v": 47, "w": 17, "x": 45, "y": 21, "z": 44}
MODIFIERS = {"ctrl", "shift", "alt", "meta"}
KEY_TABLE: dict[str, tuple[int, int]] = {
    "ctrl": (0xA2, 29), "shift": (0xA0, 42), "alt": (0xA4, 56), "meta": (0x5B, 125),
    "space": (0x20, 57), "enter": (0x0D, 28), "tab": (0x09, 15), "esc": (0x1B, 1),
    "backspace": (0x08, 14), "delete": (0x2E, 111), "insert": (0x2D, 110),
    "home": (0x24, 102), "end": (0x23, 107), "pageup": (0x21, 104),
    "pagedown": (0x22, 109), "left": (0x25, 105), "up": (0x26, 103),
    "right": (0x27, 106), "down": (0x28, 108), "minus": (0xBD, 12),
    "equal": (0xBB, 13), "bracketleft": (0xDB, 26), "bracketright": (0xDD, 27),
    "comma": (0xBC, 51), "period": (0xBE, 52), "slash": (0xBF, 53),
    "backslash": (0xDC, 43), "semicolon": (0xBA, 39), "quote": (0xDE, 40),
    "grave": (0xC0, 41),
}
for i in range(1, 11):
    KEY_TABLE[f"f{i}"] = (0x6F + i, 58 + i)  # f1..f10: VK 0x70.., evdev 59..
KEY_TABLE["f11"] = (0x7A, 87)
KEY_TABLE["f12"] = (0x7B, 88)
for i in range(13, 25):
    KEY_TABLE[f"f{i}"] = (0x7C + (i - 13), 183 + (i - 13))
for d in "0123456789":
    vk = 0x30 + int(d)
    ev = 11 if d == "0" else 2 + (int(d) - 1)
    KEY_TABLE[d] = (vk, ev)
KEY_TABLE.update({k: (0x41 + ord(k.upper()) - 65, v) for k, v in EVDEV_LETTERS.items()})
EXTENDED = {"insert", "delete", "home", "end", "pageup", "pagedown",
            "left", "up", "right", "down", "meta"}


def parse_keys(s: str) -> list[str]:
    toks = [t.lower() for t in s.split("+")]
    if not toks or any(not t for t in toks):
        raise ValueError("empty key token")
    if any(t not in KEY_TABLE for t in toks):
        raise ValueError(f"unknown key in {s!r}")
    if len(toks) > 1 and any(t not in MODIFIERS for t in toks[:-1]):
        raise ValueError("only trailing key may be non-modifier")
    return toks

# ---------------------------------------------------------------- profiles (spec 11)

ID_RE = re.compile(r"^[a-z0-9_-]{1,32}$")
ACTION_TYPES = {"key", "macro", "profile", "eraser_toggle", "mouse", "none"}


def validate_profile(p: dict, all_profiles: list[dict] | None = None) -> list[str]:
    errs = []
    if not ID_RE.match(p.get("id", "")):
        errs.append("bad profile id")
    if len(p.get("name", "")) > 24 or not p.get("name"):
        errs.append("bad profile name")
    grid = p.get("grid", {})
    if not (1 <= grid.get("cols", 0) <= 3 and 1 <= grid.get("rows", 0) <= 12):
        errs.append("bad grid")
    if all_profiles is not None and sum(1 for q in all_profiles if q.get("default")) > 1:
        errs.append("more than one default profile")
    seen: set[str] = set()
    cells: set[tuple[int, int]] = set()
    for b in p.get("buttons", []):
        if not ID_RE.match(b.get("id", "")):
            errs.append(f"bad button id {b.get('id')}")
        if b.get("id") in seen:
            errs.append(f"duplicate button id {b.get('id')}")
        seen.add(b.get("id"))
        if len(b.get("label", "")) > 16:
            errs.append(f"label too long {b.get('id')}")
        if len(b.get("speak", b.get("label", ""))) > 24:
            errs.append(f"speak too long {b.get('id')}")
        if b.get("kind") not in ("tap", "hold", "toggle"):
            errs.append(f"bad kind {b.get('id')}")
        c, r, cs, rs = b.get("col", -1), b.get("row", -1), b.get("colspan", 0), b.get("rowspan", 0)
        if not (0 <= c < grid.get("cols", 0) and 0 <= r < grid.get("rows", 0)
                and cs >= 1 and rs >= 1 and c + cs <= grid.get("cols", 0) and r + rs <= grid.get("rows", 0)):
            errs.append(f"button out of grid {b.get('id')}")
            continue
        for cc in range(c, c + cs):
            for rr in range(r, r + rs):
                if (cc, rr) in cells:
                    errs.append(f"overlapping button {b.get('id')}")
                cells.add((cc, rr))
        a = b.get("action", {})
        if a.get("type") not in ACTION_TYPES:
            errs.append(f"bad action {b.get('id')}")
        elif a["type"] == "key":
            try:
                parse_keys(a["keys"])
                if "keys_off" in a:
                    parse_keys(a["keys_off"])
            except ValueError as e:
                errs.append(f"bad keys {b.get('id')}: {e}")
            if b.get("kind") == "hold" and "+" in a.get("keys", "") and False:
                pass
        elif a["type"] == "macro":
            if b.get("kind") == "hold":
                errs.append("macro not allowed on hold")
            if len(a.get("steps", [])) > 20:
                errs.append("macro too long")
            for s in a.get("steps", []):
                if "wait_ms" in s:
                    if not (0 <= s["wait_ms"] <= 2000):
                        errs.append("bad wait_ms")
                elif "keys" in s:
                    try:
                        parse_keys(s["keys"])
                    except ValueError as e:
                        errs.append(f"bad macro keys: {e}")
                else:
                    errs.append("bad macro step")
        elif a["type"] == "eraser_toggle" and b.get("kind") != "toggle":
            errs.append("eraser_toggle requires toggle")
    return errs

# ---------------------------------------------------------------- state repair (spec 8)

class PenState:
    """Tracks in_range / in_contact, repairs sequences before inject."""

    def __init__(self):
        self.in_range = False
        self.in_contact = False
        self.eraser_in_range: bool | None = None

    def repair(self, phase: str, eraser: bool) -> list[str]:
        """Return list of phases to inject (may prepend hover/down/up/leave)."""
        out: list[str] = []
        if phase in ("down", "move"):
            if not self.in_range:
                out.append("hover")
                self.in_range = True
                self.eraser_in_range = eraser
            elif eraser != self.eraser_in_range and self.in_contact:
                return ["up", "leave", "hover", phase]  # tool switch mid-contact
            if eraser != self.eraser_in_range and not self.in_contact:
                out += ["leave", "hover"]  # tool switch out of contact
                self.eraser_in_range = eraser
            if phase == "move" and not self.in_contact:
                out.append("down")
                self.in_contact = True
            if phase == "down":
                self.in_contact = True
        elif phase == "hover":
            if self.in_contact:
                out.append("up")
                self.in_contact = False
            if not self.in_range:
                self.in_range = True
                self.eraser_in_range = eraser
        elif phase == "up":
            if not self.in_contact:
                return []  # ignore duplicate up
            self.in_contact = False
        elif phase == "leave":
            if not self.in_range:
                return []  # ignore duplicate leave
            if self.in_contact:
                out.append("up")
                self.in_contact = False
            self.in_range = False
        elif phase == "cancel":
            if self.in_contact:
                out.append("up")
                self.in_contact = False
            if self.in_range:
                out.append("leave")
                self.in_range = False
            return out
        out.append(phase)
        return out

# ---------------------------------------------------------------- touch state machine (spec 7)

class ButtonTouchMachine:
    """Models one finger on one button for both announce modes.

    Emits events: 'speak(label)', 'fire', 'hold_on', 'hold_off', 'blocked', None.
    Fake-clock driven: advance(ms) fires rest-delay announcements.
    """

    def __init__(self, kind: str, label: str, rest_delay_ms: int = 250,
                 announce_mode: str = "rest", button_guard_pen_down: bool = False):
        assert kind in ("tap", "hold", "toggle") and announce_mode in ("rest", "touch")
        self.kind = kind
        self.label = label
        self.rest_delay = rest_delay_ms
        self.mode = announce_mode
        self.guard = button_guard_pen_down
        self._down_at: int | None = None
        self._announced = False
        self._now = 0
        self._held = False

    def finger_down(self, now: int) -> str | None:
        self._now = now
        self._down_at = now
        self._announced = False
        if self.guard:
            return "blocked"
        if self.mode == "touch":
            self._announced = True
            return f"speak({self.label})"
        return None

    def advance(self, now: int) -> str | None:
        self._now = now
        if self._down_at is None or self._announced:
            return None
        if now - self._down_at >= self.rest_delay:
            self._announced = True
            if self.kind == "hold":
                self._held = True
                return "hold_on+speak"  # spec: hold activates at rest delay
            if self.mode == "rest":
                return f"speak({self.label})"
        return None

    def finger_up(self, now: int) -> str | None:
        self._now = now
        if self._down_at is None:
            return None
        elapsed = now - self._down_at
        announced = self._announced or elapsed >= self.rest_delay
        self._down_at = None
        if self.guard:
            return None
        if self.kind == "hold":
            if self._held or announced:
                self._held = False
                return "hold_off"
            return None  # quick tap on hold never activates (spec 7)
        if self.mode == "rest":
            if not announced:
                return "fire"  # quick tap, no speech
            return None  # lift after announce: no activation
        return "fire"  # touch mode: lift always activates

# ---------------------------------------------------------------- M1/M2: sender, receiver, control, pairing (spec 3,4,5,8)

import time as _time
import urllib.parse as _urlparse


def batch_motion_samples(samples: list[dict]) -> list[list[dict]]:
    """Sender rule 1: one datagram per MotionEvent, >16 records split in order."""
    return [samples[i:i + 16] for i in range(0, len(samples), 1)] if len(samples) <= 16 else \
        [samples[i:i + 16] for i in range(0, len(samples), 16)]


def needs_repeat(phase: str) -> bool:
    """Sender rule 4: down/up/leave/cancel repeat twice (+10/+20ms)."""
    return phase in ("down", "up", "leave", "cancel")


class PenReceiver:
    """Companion UDP receiver rules (spec 5). Tracks per-session seq, t_ms,
    identical-record dedup, source-IP binding, drop counters, watchdogs."""

    def __init__(self, session_id: int, control_peer_ip: str):
        self.session_id = session_id
        self.control_peer_ip = control_peer_ip
        self.highest_seq = 0
        self.last_t_ms: int | None = None
        self.last_record_bytes: bytes | None = None
        self.dropped_bad_magic = 0
        self.dropped_bad_session = 0
        self.dropped_bad_tag = 0
        self.dropped_wrong_ip = 0
        self.dropped_old_seq = 0
        self.dropped_old_tms = 0
        self.dropped_identical = 0
        self.last_datagram_at: float | None = None
        self.pen_is_down = False
        self.pen_in_range = False

    def accept_datagram(self, datagram: bytes, src_ip: str, pen_key: bytes,
                        now: float | None = None) -> list[dict]:
        """Decrypt + filter a datagram. Returns accepted records in order.
        Drops (with counters) on: short header, bad magic, wrong session,
        wrong IP, non-increasing seq, bad tag."""
        now = now if now is not None else _time.monotonic()
        if len(datagram) < 10 + 1 + 16:
            self.dropped_bad_magic += 1
            return []
        magic, _, session_id, seq = struct.unpack_from("<BBII", datagram, 0)
        if magic != MAGIC:
            self.dropped_bad_magic += 1
            return []
        if session_id != self.session_id:
            self.dropped_bad_session += 1
            return []
        if src_ip != self.control_peer_ip:
            self.dropped_wrong_ip += 1
            return []
        if seq <= self.highest_seq:
            self.dropped_old_seq += 1
            return []
        try:
            _, _, records = decrypt_datagram(pen_key, datagram)
        except ValueError:
            self.dropped_bad_tag += 1
            return []
        self.highest_seq = seq
        self.last_datagram_at = now
        out = []
        for r in records:
            raw = encode_record(r)
            if self.last_t_ms is not None and r["t_ms"] < self.last_t_ms:
                self.dropped_old_tms += 1
                continue
            if self.last_record_bytes is not None and raw == self.last_record_bytes:
                self.dropped_identical += 1
                continue  # removes sender-rule-4 repeats
            self.last_t_ms = r["t_ms"]
            self.last_record_bytes = raw
            out.append(r)
        for r in out:
            if r["phase"] in ("down", "move"):
                self.pen_is_down = True
                self.pen_in_range = True
            elif r["phase"] == "hover":
                self.pen_in_range = True
            elif r["phase"] == "up":
                self.pen_is_down = False
            elif r["phase"] in ("leave", "cancel"):
                self.pen_is_down = False
                self.pen_in_range = False
        return out

    def watchdog_action(self, now: float) -> str | None:
        """Returns 'up+leave' if down-timed-out (750ms), 'leave' if
        hover-timed-out (1000ms), else None. Caller injects then updates flags."""
        if self.last_datagram_at is None:
            return None
        idle_ms = (now - self.last_datagram_at) * 1000
        if self.pen_is_down and idle_ms >= 750:
            self.pen_is_down = False
            self.pen_in_range = False
            return "up+leave"
        if self.pen_in_range and idle_ms >= 1000:
            self.pen_in_range = False
            return "leave"
        return None


KNOWN_CONTROL_TYPES = {"challenge", "hello", "welcome", "error", "bye",
                       "pair_request", "pair_ok", "pair_fail",
                       "ping", "pong", "surface", "button", "button_state",
                       "profile", "speak"}
ERROR_CODES = {"bad_proto", "not_paired", "auth_failed", "busy",
               "bad_message", "internal"}
PAIR_FAIL_CODES = {"bad_token", "bad_pin", "closed", "locked"}


def validate_control_message(obj: dict) -> list[str]:
    """Check a decoded control message. Unknown types/fields are IGNORED
    per spec (not errors); returns error list for malformed known types."""
    if not isinstance(obj, dict) or "t" not in obj:
        return ["missing t"]
    t = obj["t"]
    if t not in KNOWN_CONTROL_TYPES:
        return []  # MUST ignore unknown types
    errs = []
    if t == "hello":
        if obj.get("proto") != 1:
            errs.append("bad proto")
        for f in ("device_id", "device_name", "app_version", "auth", "caps", "pen_area"):
            if f not in obj:
                errs.append(f"missing {f}")
    elif t == "button":
        if obj.get("phase") not in ("down", "up"):
            errs.append("bad button phase")
        if not obj.get("id"):
            errs.append("missing button id")
    elif t == "pair_request":
        if obj.get("mode") not in ("qr", "pin"):
            errs.append("bad pair mode")
    return errs


def build_qr_uri(hosts: list[str], port: int, fingerprint: bytes, token: bytes) -> str:
    """chiz://pair?v=1&h=<hosts>&p=<port>&fp=<b64url>&t=<b64url> (spec 3)."""
    import base64 as _b64
    fp = _b64.urlsafe_b64encode(fingerprint).decode().rstrip("=")
    tk = _b64.urlsafe_b64encode(token).decode().rstrip("=")
    q = _urlparse.urlencode({"v": 1, "h": ",".join(hosts), "p": port, "fp": fp, "t": tk})
    return f"chiz://pair?{q}"


def parse_qr_uri(uri: str) -> dict:
    """Parse + validate a QR URI. Restores base64url padding."""
    import base64 as _b64
    u = _urlparse.urlparse(uri)
    assert u.scheme == "chiz" and u.netloc == "pair", "bad QR scheme"
    q = _urlparse.parse_qs(u.query)
    def _pad(s: str) -> str:
        return s + "=" * (-len(s) % 4)
    fp = _b64.urlsafe_b64decode(_pad(q["fp"][0]))
    tk = _b64.urlsafe_b64decode(_pad(q["t"][0]))
    assert len(fp) == 32 and len(tk) == 16, "bad QR fp/token length"
    return {"v": int(q["v"][0]), "hosts": q["h"][0].split(","),
            "port": int(q["p"][0]), "fp": fp, "token": tk}


class PairingWindow:
    """PC pairing window: 120 s, one at a time, 5 failed attempts (spec 3)."""

    def __init__(self, now: float = 0.0):
        self.open_at: float | None = None
        self.now = now
        self.failures = 0

    def open(self):
        assert self.open_at is None, "only one window at a time"
        self.open_at = self.now
        self.failures = 0

    def is_open(self) -> bool:
        return self.open_at is not None and (self.now - self.open_at) < 120 \
            and self.failures < 5

    def record_failure(self) -> str:
        """Returns 'retry' or 'closed' (window closes after 5 failures)."""
        self.failures += 1
        if self.failures >= 5 or not self.is_open():
            self.open_at = None
            return "closed"
        return "retry"

    def record_success(self):
        self.open_at = None


def select_profile(profiles: list[dict], foreground_app: str | None,
                   locked_id: str | None) -> dict | None:
    """Profile selection (spec 8): manual lock wins; else first app match
    (case-insensitive), else the default profile."""
    by_id = {p["id"]: p for p in profiles}
    if locked_id is not None:
        return by_id.get(locked_id)
    if foreground_app:
        fa = foreground_app.lower()
        for p in profiles:
            if any(a.lower() == fa for a in p.get("apps", [])):
                return p
    for p in profiles:
        if p.get("default"):
            return p
    return None


def plan_key_press(keys: list[str], hold: bool, release: bool = False) -> list[tuple[str, bool]]:
    """Key injection order (spec 8): modifiers down in order, key down/up,
    modifiers up in reverse (~2 ms apart). Hold buttons keep keys down until up."""
    mods = [k for k in keys[:-1] if k in MODIFIERS]
    main = keys[-1]
    if release:  # releasing a hold: modifiers up in reverse
        return [(m, False) for m in reversed(mods)] + ([(main, False)] if main not in mods else [])
    seq = [(m, True) for m in mods]
    if main not in mods:
        seq.append((main, True))
        if not hold:
            seq.append((main, False))
    elif hold and not mods:
        seq = [(main, True)]  # modifier-only hold, e.g. Space-pan
        if not hold:
            seq.append((main, False))
    if not hold:
        seq += [(m, False) for m in reversed(mods)]
    return seq


RECONNECT_SCHEDULE = [0.5, 1, 2, 4]


def reconnect_delay(attempt: int) -> float:
    """0.5, 1, 2, 4 s then every 5 s (spec 3)."""
    if attempt < len(RECONNECT_SCHEDULE):
        return RECONNECT_SCHEDULE[attempt]
    return 5.0

# ---------------------------------------------------------------- failsafe (emergency off switch)

STUCK_TRIPS_TO_AUTOPAUSE = 3
DEFAULT_PANIC_HOTKEY = "ctrl+alt+shift+f12"


class Failsafe:
    """Mirrors companion/core/src/failsafe.rs. Pause latch + stuck-trip
    auto-pause + idempotent panic entry. While paused, pen drops, buttons pass."""

    def __init__(self):
        self.paused: str | None = None  # 'manual' | 'panic' | 'auto_stuck'
        self.stuck_trips = 0
        self.leave_pending = False
        self.panic_hotkey = parse_keys(DEFAULT_PANIC_HOTKEY)

    def pause(self, reason: str) -> bool:
        if self.paused is not None:
            return False
        self.paused = reason
        self.leave_pending = True
        return True

    def resume(self):
        self.paused = None
        self.stuck_trips = 0
        self.leave_pending = False

    def emergency_release(self) -> bool:
        self.stuck_trips = 0
        return self.pause("panic")

    def note_stuck_trip(self) -> str | None:
        self.stuck_trips += 1
        if self.stuck_trips >= STUCK_TRIPS_TO_AUTOPAUSE:
            self.stuck_trips = 0
            self.pause("auto_stuck")
            return "auto_stuck"
        return None

    def note_clean_input(self):
        self.stuck_trips = 0

    def drop_pen(self) -> bool:
        return self.paused is not None

    def set_hotkey(self, s: str):
        self.panic_hotkey = parse_keys(s)
