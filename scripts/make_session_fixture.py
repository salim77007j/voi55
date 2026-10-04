#!/usr/bin/env python3
"""Generates the Phase 5 RSS fixture: a 3-minute 48 kHz stereo WAV.

Content is a sung-vowel-like signal (harmonics with vibrato + breath noise),
PCM 16-bit — small file, loads through the app's normal import path where it
is stored internally as f32. Only used for memory measurement, not for
quality claims.

Usage: make_session_fixture.py OUT.wav [SECONDS]
"""
import math
import random
import struct
import sys
import wave


def main() -> None:
    out = sys.argv[1]
    secs = float(sys.argv[2]) if len(sys.argv) > 2 else 180.0
    sr = 48_000
    n = int(secs * sr)
    rng = random.Random(2026)

    # Slow melody over an octave, ~1 note per 1.5 s (A3–A4), sung "ah".
    notes = [220.0, 246.9, 261.6, 293.7, 329.6, 349.2, 392.0, 440.0]
    frames = bytearray()
    phase = 0.0
    vibrato_phase = 0.0
    for i in range(n):
        t = i / sr
        note_idx = int(t / 1.5) % len(notes)
        f0 = notes[note_idx]
        # 5.5 Hz vibrato ±50 cents, gentle note-entry glide.
        cents = 50.0 * math.sin(2 * math.pi * 5.5 * t)
        freq = f0 * 2 ** (cents / 1200.0)
        phase += 2 * math.pi * freq / sr
        vibrato_phase += 2 * math.pi * 5.5 / sr

        # 1/k harmonic roll-off with a mild formant boost near 700/1200 Hz.
        sample = 0.0
        for k in range(1, 25):
            f = freq * k
            if f >= sr / 2:
                break
            amp = (1.0 / k) * (1.0 + 0.8 / (1 + ((f - 700) / 150) ** 2)
                               + 0.5 / (1 + ((f - 1200) / 200) ** 2))
            sample += amp * math.sin(phase * k)

        # Breath bed (lowpassed noise) + gentle envelope per note.
        breath = 0.02 * (rng.random() * 2 - 1)
        pos_in_note = t % 1.5
        env = min(1.0, pos_in_note / 0.08, (1.5 - pos_in_note) / 0.15)
        value = env * (0.25 * sample + breath)

        s = max(-1.0, min(1.0, value))
        v = int(s * 32767)
        frames += struct.pack("<hh", v, v)

    with wave.open(out, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(bytes(frames))
    print(f"wrote {out}: {secs:.0f} s @ {sr} Hz stereo PCM16 ({len(frames)/1e6:.1f} MB)")


if __name__ == "__main__":
    main()
