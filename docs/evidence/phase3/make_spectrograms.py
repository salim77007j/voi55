#!/usr/bin/env python3
"""Spectrogram pairs for the Phase 3 evidence corpus.

Run from docs/evidence/phase3/ (or the repo root):
    python3 make_spectrograms.py [wav_dir] [out_dir]
Requires numpy + matplotlib.
"""
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
import wave


def read_wav(path):
    with wave.open(str(path), "rb") as w:
        n = w.getnframes()
        data = np.frombuffer(w.readframes(n), dtype=np.int16).astype(np.float64) / 32768.0
    return data


def spectrogram(ax, x, sr, title):
    ax.specgram(x, NFFT=2048, Fs=sr, noverlap=1536, cmap="magma",
                vmin=-110, vmax=-20)
    ax.set_title(title, fontsize=9)
    ax.set_ylabel("Hz")
    ax.set_ylim(0, 5000)


PAIRS = [
    ("vocal_a.wav", "vocal_a_pitch_plus4.wav", "Pitch +4 st (160 -> 201.6 Hz)"),
    ("vocal_a.wav", "vocal_a_formant_130mm.wav", "Formant 175 -> 130 mm (F1 500 -> 673 Hz)"),
    ("vocal_a.wav", "vocal_a_air_plus6.wav", "Air +6 dB (residual boosted)"),
    ("vocal_b_breathy.wav", "vocal_b_air_minus18.wav", "Air -18 dB (breath bursts removed)"),
    ("vocal_a.wav", "vocal_a_combined.wav", "Combined +4 st / 130 mm / +6 dB"),
]

def main():
    wav_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
    out_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else wav_dir
    for before, after, title in PAIRS:
        xb = read_wav(wav_dir / before)
        xa = read_wav(wav_dir / after)
        fig, axes = plt.subplots(2, 1, figsize=(8, 5), sharex=True, constrained_layout=True)
        spectrogram(axes[0], xb, 48000, f"before: {before}")
        spectrogram(axes[1], xa, 48000, f"after: {after}")
        axes[1].set_xlabel("s")
        fig.suptitle(title, fontsize=11)
        name = f"spec_{Path(after).stem}.png"
        fig.savefig(out_dir / name, dpi=110)
        plt.close(fig)
        print(f"wrote {out_dir / name}")

if __name__ == "__main__":
    main()
