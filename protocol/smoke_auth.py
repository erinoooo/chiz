"""Smoke: TLS 1.3 handshake + auth + buttons + pen over real TCP/UDP.

Needs a CHIZ_TEST_PAIR server. Mirrors PIN mode: the client captures the
fingerprint from the handshake (trust-on-first-sight) instead of the
system store, asserts TLS 1.3, then runs the full session.
"""
import base64
import hashlib
import hmac
import json
import socket
import ssl
import struct

DEV = "5b0f6c1e-7d0a-4a55-9d3e-0c6f1a2b3c4d"
SECRET = bytes(range(32))


def read_n(s, n):
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("eof")
        buf += chunk
    return buf


def read_frame(s):
    (n,) = struct.unpack("<I", read_n(s, 4))
    assert n <= 65536, n
    return json.loads(read_n(s, n))


def send(s, obj):
    p = json.dumps(obj, separators=(",", ":")).encode()
    s.sendall(struct.pack("<I", len(p)) + p)


def connect_tls():
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_3
    ctx.maximum_version = ssl.TLSVersion.TLSv1_3
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", 47800), timeout=5)
    tls = ctx.wrap_socket(raw, server_hostname="127.0.0.1")
    tls.settimeout(5)
    return tls


s = connect_tls()
print("tls:", s.version())
assert s.version() == "TLSv1.3", s.version()
der = s.getpeercert(binary_form=True)
print("server fp sha256:", hashlib.sha256(der).hexdigest())

# Full pairing run (PIN mode) against an open window. The PIN comes from
# the server log (CHIZ_SERVER_LOG), like reading it off the PC screen.
import os
import re
if os.environ.get("CHIZ_PAIR_TEST") == "1":
    DEV2 = "11111111-2222-4333-8444-555555555555"
    p = connect_tls()
    chp = read_frame(p)
    assert chp.get("pairing_open") is True, chp
    nonce2 = base64.b64decode(chp["nonce"])
    fp2 = hashlib.sha256(p.getpeercert(binary_form=True)).digest()
    logtext = open(os.environ["CHIZ_SERVER_LOG"]).read()
    m = re.search(r"pairing open for 120 s.+PIN: (\d{8})", logtext)
    assert m, "PIN not found in server log"
    pin = m.group(1)
    # One wrong PIN first: pair_fail with attempts left.
    bad = base64.b64encode(b"\x00" * 32).decode()
    send(p, {"t": "pair_request", "proto": 1, "device_id": DEV2,
             "device_name": "pair-tab", "mode": "pin", "proof": bad})
    f1 = read_frame(p)
    print("pair_fail:", f1)
    assert f1["t"] == "pair_fail" and f1["code"] == "bad_pin" and f1["attempts_left"] == 4, f1
    proof = base64.b64encode(hmac.new(pin.encode(), b"chiz-pair-v1" + fp2 + nonce2 + DEV2.encode(),
                                      hashlib.sha256).digest()).decode()
    send(p, {"t": "pair_request", "proto": 1, "device_id": DEV2,
             "device_name": "pair-tab", "mode": "pin", "proof": proof})
    ok = read_frame(p)
    assert ok["t"] == "pair_ok", ok
    secret2 = base64.b64decode(ok["device_secret"])
    assert len(secret2) == 32
    ch2 = read_frame(p)  # fresh challenge after pair_ok
    assert ch2["t"] == "challenge"
    auth2 = base64.b64encode(hmac.new(secret2, b"chiz-auth-v1" + base64.b64decode(ch2["nonce"]) + DEV2.encode(),
                                      hashlib.sha256).digest()).decode()
    send(p, {"t": "hello", "proto": 1, "device_id": DEV2, "device_name": "pair-tab",
             "app_version": "0.0", "auth": auth2, "caps": {}, "pen_area": {"w": 1, "h": 1}})
    w2 = read_frame(p)
    assert w2["t"] == "welcome", w2
    read_frame(p)  # profile(connect)
    p.close()
    print("PAIR-E2E-OK")
ch = read_frame(s)
assert ch["t"] == "challenge" and ch["proto"] == 1
nonce = base64.b64decode(ch["nonce"])
auth = base64.b64encode(hmac.new(SECRET, b"chiz-auth-v1" + nonce + DEV.encode(),
                                 hashlib.sha256).digest()).decode()
send(s, {"t": "hello", "proto": 1, "device_id": DEV, "device_name": "test-tab",
         "app_version": "0.0", "auth": auth,
         "caps": {"pressure": True, "tilt": True, "hover": True,
                  "eraser": True, "barrel": 2, "video": False},
         "pen_area": {"w": 2100, "h": 1600}})
w = read_frame(s)
print("welcome:", w["t"], "session:", w["session_id"], "pen_port:", w["pen_port"])
assert w["t"] == "welcome" and w["features"] == {"video": False}
assert len(base64.b64decode(w["pen_key_salt"])) == 16

# Live UDP pen path: encrypt a hover record under the session key and fire
# it at the pen port. The server log must show `inject_pen phase=0`.
from chiz_proto import derive_pen_key, encrypt_datagram
pen_key = derive_pen_key(SECRET, base64.b64decode(w["pen_key_salt"]))
hover = {"phase": "hover", "eraser": False, "barrel1": False, "barrel2": False,
         "x": 32768, "y": 32768, "pressure": 0, "tilt_x": 0, "tilt_y": 0,
         "distance": 10000, "t_ms": 777}
dg = encrypt_datagram(pen_key, w["session_id"], 1, [hover])
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.sendto(dg, ("127.0.0.1", w["pen_port"]))
u.close()
print("pen datagram sent, session", w["session_id"])

s2 = connect_tls()
read_frame(s2)  # challenge
send(s2, {"t": "hello", "proto": 1, "device_id": "other-device",
          "device_name": "x", "app_version": "0", "auth": "y",
          "caps": {}, "pen_area": {"w": 1, "h": 1}})
r2 = read_frame(s2)
print("second-tablet-reply:", r2)
assert r2["code"] in ("not_paired", "busy"), r2
s2.close()

# M4: profile arrives right after welcome; buttons dispatch.
prof = read_frame(s)
print("profile:", prof["profile"]["id"], "reason:", prof["reason"],
      "buttons:", len(prof["profile"]["buttons"]))
assert prof["t"] == "profile" and prof["reason"] == "connect"
assert any(b["id"] == "undo" for b in prof["profile"]["buttons"])

send(s, {"t": "button", "id": "undo", "phase": "down"})
send(s, {"t": "button", "id": "undo", "phase": "up"})
send(s, {"t": "button", "id": "pan", "phase": "down"})
send(s, {"t": "button", "id": "pan", "phase": "up"})
send(s, {"t": "button", "id": "eraser", "phase": "down"})
bs = read_frame(s)
print("button_state:", bs)
assert bs == {"t": "button_state", "id": "eraser", "on": True}, bs
send(s, {"t": "button", "id": "eraser", "phase": "down"})
bs2 = read_frame(s)
assert bs2 == {"t": "button_state", "id": "eraser", "on": False}, bs2
print("toggle off ok")
send(s, {"t": "button", "id": "nope-unknown", "phase": "down"})
send(s, {"t": "surface", "w": 1600, "h": 2100})
s.settimeout(0.6)
try:
    stray = read_frame(s)
    raise SystemExit(f"unexpected reply to unknown button/surface: {stray}")
except socket.timeout:
    print("unknown-button/surface ignored ok")
s.settimeout(5)
s.close()

# Busy path: two PAIRED tablets, second authenticated one must get `busy`.
import os
if os.environ.get("CHIZ_BUSY_CHECK") == "1":
    a = connect_tls()
    na = base64.b64decode(read_frame(a)["nonce"])
    aa = base64.b64encode(hmac.new(SECRET, b"chiz-auth-v1" + na + DEV.encode(),
                                   hashlib.sha256).digest()).decode()
    send(a, {"t": "hello", "proto": 1, "device_id": DEV, "device_name": "a",
             "app_version": "0", "auth": aa, "caps": {}, "pen_area": {"w": 1, "h": 1}})
    wa = read_frame(a)
    assert wa["t"] == "welcome", wa
    read_frame(a)  # profile(connect)
    b = connect_tls()
    nb = base64.b64decode(read_frame(b)["nonce"])
    sec2 = bytes([255 - i for i in range(32)])
    ab = base64.b64encode(hmac.new(sec2, b"chiz-auth-v1" + nb + b"dev-B",
                                   hashlib.sha256).digest()).decode()
    send(b, {"t": "hello", "proto": 1, "device_id": "dev-B", "device_name": "b",
             "app_version": "0", "auth": ab, "caps": {}, "pen_area": {"w": 1, "h": 1}})
    rb = read_frame(b)
    print("busy-check:", rb)
    assert rb["code"] == "busy", rb
    a.close()
    b.close()

print("AUTH-E2E-OK")
