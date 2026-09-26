#!/usr/bin/env python3
"""Synthesize the 8 spec-7 earcons as 16-bit 44.1 kHz mono WAVs.

Recipes from the Chiz spec (peak -6 dBFS, none longer than 200 ms):
  fire       dry click near 1.2 kHz, 40 ms
  hold_on    click near 1.6 kHz, 50 ms
  hold_off   click near 0.9 kHz, 50 ms
  on         two notes rising 600 -> 900 Hz, 90 ms
  off        two notes falling 900 -> 600 Hz, 90 ms
  blocked    low thud near 200 Hz, 60 ms
  connect    three notes rising, 200 ms
  disconnect three notes falling, 200 ms
Output: android/app/src/main/res/raw/<name>.wav
"""
import math
import struct
import wave
from pathlib import Path

SR = 44100
PEAK = 10 ** (-6 / 20)  # -6 dBFS


def burst(freq: float, ms: float, decay_ms: float | None = None) -> list[float]:
    n = int(SR * ms / 1000)
    d = int(SR * (decay_ms if decay_ms is not None else ms) / 1000)
    out = []
    for i in range(n):
        t = i / SR
        env = math.exp(-i / max(d, 1)) * min(1.0, i / max(SR * 0.002, 1))
        out.append(math.sin(2 * math.pi * freq * t) * env)
    return out


def note(freq: float, ms: float) -> list[float]:
    n = int(SR * ms / 1000)
    out = []
    for i in range(n):
        t = i / SR
        env = min(1.0, i / (SR * 0.005), (n - i) / (SR * 0.008))
        out.append(math.sin(2 * math.pi * freq * t) * env)
    return out


def seq(parts: list[list[float]]) -> list[float]:
    out: list[float] = []
    for p in parts:
        out += p
    return out


def write_wav(path: Path, samples: list[float]) -> None:
    peak = max(1e-9, max(abs(s) for s in samples))
    gain = PEAK / peak
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(b"".join(struct.pack("<h", int(max(-1.0, min(1.0, s * gain)) * 32767)) for s in samples))


def main() -> None:
    out = Path(__file__).resolve().parent / "app" / "src" / "main" / "res" / "raw"
    out.mkdir(parents=True, exist_ok=True)
    sounds = {
        "fire": burst(1200, 40),
        "hold_on": burst(1600, 50),
        "hold_off": burst(900, 50),
        "on": seq([note(600, 45), note(900, 45)]),
        "off": seq([note(900, 45), note(600, 45)]),
        "blocked": burst(200, 60),
        "connect": seq([note(523, 66), note(659, 66), note(784, 68)]),
        "disconnect": seq([note(784, 66), note(659, 66), note(523, 68)]),
    }
    for name, samples in sounds.items():
        write_wav(out / f"{name}.wav", samples)
        ms = len(samples) * 1000 / SR
        assert ms <= 200, name
        print(f"{name}.wav ({ms:.0f} ms)")


if __name__ == "__main__":
    main()
