#!/usr/bin/env python3
"""
slatefree — Python version.

Same algorithm as the Rust program in ../src_rust/main.rs (byte-identical XML output);
handy for experimenting without a compiler. Console messages are in Russian.

    pip install -r requirements.txt
    python slatefree.py <shoot folder> [--cams ACAM BCAM] [--audio AUDIO] [--exclude "mix*.wav"]

Output: <shoot>/SYNC/<shoot>_sync.xml (FCP7 XML for DaVinci Resolve) + sync_result.csv
"""
import argparse
import csv
import fnmatch
import math
import os
import re
import struct
import subprocess
import sys
import time
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from xml.sax.saxutils import escape

import numpy as np
from scipy import signal
from scipy.ndimage import uniform_filter1d

try:
    import imageio_ffmpeg
    FFMPEG = imageio_ffmpeg.get_ffmpeg_exe()
except ImportError:
    FFMPEG = "ffmpeg"

VIDEO_EXT = (".mov", ".mp4", ".mxf", ".mts", ".m4v", ".avi")
SR = 8000            # analysis sample rate
HOP = 80             # 10 ms feature hop
ESR = SR // HOP      # 100 Hz
NFFT = 512
NB = 16

# --------------------------------------------------------------------------- probe

STD_FPS = [24000 / 1001, 24, 25, 30000 / 1001, 30, 48, 50, 60000 / 1001, 60, 120000 / 1001, 120]

def snap_fps(f):
    """ffmpeg prints rounded rates (23.98) -> exact standard rate (24000/1001)"""
    best = min(STD_FPS, key=lambda s: abs(s - f))
    return best if abs(best - f) < 0.02 else f

def probe(path):
    r = subprocess.run([FFMPEG, "-hide_banner", "-i", path], capture_output=True,
                       text=True, errors="replace", check=False).stderr
    d = {"path": path, "file": os.path.basename(path), "size": os.path.getsize(path)}
    m = re.search(r"Duration: (\d+):(\d+):([\d.]+)", r)
    d["dur"] = int(m[1]) * 3600 + int(m[2]) * 60 + float(m[3]) if m else 0.0
    m = re.search(r"timecode\s*:\s*(\d\d[:;]\d\d[:;]\d\d[:;.]\d\d)", r)
    d["tc"] = m[1] if m else None
    m = re.search(r"creation_time\s*:\s*(\S+)", r)
    d["ctime"] = m[1] if m else None
    m = re.search(r"time_reference\s*:\s*(\d+)", r)
    d["tref"] = int(m[1]) if m else None
    m = re.search(r"([\d.]+) fps", r)
    d["fps"] = snap_fps(float(m[1])) if m else None
    m = re.search(r"encoded_by\s*:\s*(.+)", r)
    d["encoder"] = m[1].strip() if m else ""
    d["has_audio"] = bool(re.search(r"Stream #\S+: Audio", r))
    m = re.search(r"Audio: .*?(\d+) Hz", r)
    d["audio_sr"] = int(m[1]) if m else None
    return d

def wav_frames(path):
    """number of sample frames and sample rate from the WAV header (works for >2 GB)"""
    with open(path, "rb") as f:
        head = f.read(1 << 16)
    fmt = head.find(b"fmt ")
    ch, sr = struct.unpack("<HI", head[fmt + 10:fmt + 16])
    bits = struct.unpack("<H", head[fmt + 22:fmt + 24])[0]
    i = head.find(b"data")
    size = struct.unpack("<I", head[i + 4:i + 8])[0]
    size = min(size, os.path.getsize(path) - (i + 8))
    return size // (ch * bits // 8), sr

# --------------------------------------------------------------------------- audio

def extract(path, cache):
    dst = os.path.join(cache, os.path.basename(path) + ".npy")
    if os.path.exists(dst):
        return dst
    r = subprocess.run([FFMPEG, "-v", "error", "-i", path, "-vn", "-ac", "1", "-ar", str(SR),
                        "-f", "f32le", "-"], capture_output=True, check=False)
    a = np.frombuffer(r.stdout, dtype=np.float32)
    np.save(dst, a)
    return dst

_edges = np.geomspace(150, 3800, NB + 1)
_freqs = np.fft.rfftfreq(NFFT, 1 / SR)
_band = np.zeros((NB, len(_freqs)), np.float32)
for _i in range(NB):
    _band[_i, (_freqs >= _edges[_i]) & (_freqs < _edges[_i + 1])] = 1
_win = np.hanning(NFFT).astype(np.float32)

def logbands(x, chunk_frames=12000):
    """log band energies; frame i covers samples [i*HOP, i*HOP+NFFT) — one global grid"""
    x = np.asarray(x, dtype=np.float32)
    n = (len(x) - NFFT) // HOP + 1
    if n <= 0:
        return np.zeros((0, NB), np.float32)
    out = []
    for f0 in range(0, n, chunk_frames):
        f1 = min(n, f0 + chunk_frames)
        seg = x[f0 * HOP:(f1 - 1) * HOP + NFFT]
        idx = np.arange(NFFT)[None, :] + HOP * np.arange(f1 - f0)[:, None]
        out.append(np.log((np.abs(np.fft.rfft(seg[idx] * _win, axis=1)) ** 2) @ _band.T + 1e-9))
    return np.concatenate(out).astype(np.float32)

def _locnorm(d, k):
    m = uniform_filter1d(d, k, axis=0)
    v = np.sqrt(np.maximum(uniform_filter1d(d * d, k, axis=0) - m * m, 1e-12))
    return (d - m) / (v + 1e-3)

def flux(x):
    """summed spectral flux, 100 Hz — robust onset feature for the global search"""
    B = logbands(x)
    if len(B) < 2:
        return np.zeros(len(B), np.float32)
    k = 3 * ESR
    d = np.maximum(np.diff(B, axis=0, prepend=B[:1]), 0).sum(1)
    f = _locnorm(d, k)
    f[uniform_filter1d(B.max(1), k) < np.log(1e-6)] = 0
    return np.clip(f, -3, 6).astype(np.float32)

def bandfeat(x):
    """per-band detrended log energy (timbre/melody) — resolves beat ambiguity in music"""
    B = logbands(x)
    if len(B) < 2:
        return np.zeros((len(B), NB), np.float32)
    f = _locnorm(B, ESR)
    f[uniform_filter1d(B.max(1), 3 * ESR) < np.log(1e-6)] = 0
    return np.clip(f, -3, 3).astype(np.float32)

# --------------------------------------------------------------------------- recorder master

def variant(fname):
    """channel-set suffix of a recorder file: H4n 4CH '..001I.wav' -> 'I', '..001M.wav' -> 'M', else ''"""
    stem = os.path.splitext(fname)[0]
    return stem[-1].upper() if stem[-1].isalpha() and stem[-2:-1].isdigit() else ""

class Master:
    """recorder takes placed on the recorder clock (seconds of day from BWF TimeReference)"""

    def __init__(self, wavs, cache, ref_suffix):
        takes = defaultdict(list)
        for w in wavs:
            n, sr = wav_frames(w["path"])
            w["nsamp"], w["sr"] = n, sr
            if n / sr < 5:                       # test blips
                continue
            takes[(w["tref"], n)].append(w)
        self.takes = []
        for (tref, n), files in sorted(takes.items(), key=lambda kv: kv[0][0]):
            files.sort(key=lambda w: w["file"])
            ref = next((w for w in files if variant(w["file"]) == ref_suffix), None)
            ref = ref or max(files, key=lambda w: os.path.getsize(w["path"]))
            sr = files[0]["sr"]
            self.takes.append({"tref": tref, "sr": sr, "nsamp": n, "len": n / sr, "files": files, "ref": ref,
                               "start": tref / sr})
        # continuation of a size-split recording (2 GB / 4 GB): start = prev start + prev length
        for a, b in zip(self.takes, self.takes[1:]):
            prev_end = a["start"] + a["len"]
            if a["ref"]["size"] > 1.9e9 and abs(b["start"] - prev_end) < 6:
                b["start"] = prev_end
                b["continues"] = True
        self.variants = sorted({variant(w["file"]) for t in self.takes for w in t["files"]})
        if not self.takes:
            sys.exit("Нет WAV-записей рекордера с TimeReference")
        self.T0 = self.takes[0]["start"]
        T1 = max(t["start"] + t["len"] for t in self.takes)
        print(f"Рекордер: {len(self.takes)} записей, {(T1 - self.T0) / 3600:.2f} ч, варианты каналов: {self.variants}")
        for t in self.takes:
            t["npy"] = os.path.join(cache, t["ref"]["file"] + ".npy")
        self.flux = np.zeros(int((T1 - self.T0) * ESR) + 1, np.float32)
        for t in self.takes:
            f = flux(np.load(t["npy"], mmap_mode="r"))
            i = round(float((t["start"] - self.T0) * ESR))
            self.flux[i:i + len(f)] = f[:len(self.flux) - i]

    def wave(self, t, n):
        out = np.zeros(n, np.float32)
        for tk in self.takes:
            a = np.load(tk["npy"], mmap_mode="r")
            i0 = round(float((t - tk["start"]) * SR))
            lo, hi = max(0, i0), min(len(a), i0 + n)
            if hi > lo:
                out[lo - i0:hi - i0] = a[lo:hi]
        return out

    def covered(self, t0, t1):
        tot = sum(max(0, min(t1, tk["start"] + tk["len"]) - max(t0, tk["start"])) for tk in self.takes)
        return tot / max(t1 - t0, 1e-9)

# --------------------------------------------------------------------------- correlation

def global_search(M, f):
    """coarse position of clip flux f in the whole master flux; returns (t, z)"""
    n = len(f)
    if n < ESR or n >= len(M.flux):
        return None, 0.0
    r = signal.fftconvolve(M.flux, (f - f.mean())[::-1], mode="valid") / n
    k = int(np.argmax(r))
    med = np.median(r); mad = 1.4826 * np.median(np.abs(r - med)) + 1e-12
    return M.T0 + k / ESR, float((r[k] - med) / mad)

def ncc(cf, mf, nl):
    n = len(cf)
    c0 = cf - cf.mean(0)
    seg = mf[:nl + n - 1]
    num = sum(signal.fftconvolve(seg[:, b], c0[::-1, b], mode="valid") for b in range(cf.shape[1]))
    z = np.zeros((1, cf.shape[1]))
    cs1 = np.concatenate([z, np.cumsum(seg, 0, dtype=np.float64)])
    cs2 = np.concatenate([z, np.cumsum(seg.astype(np.float64) ** 2, 0)])
    s1 = cs1[n:n + nl] - cs1[:nl]; s2 = cs2[n:n + nl] - cs2[:nl]
    var_m = (s2 - s1 ** 2 / n).sum(1)
    return num[:nl] / (np.sqrt((c0 ** 2).sum() * np.maximum(var_m, 1e-9)) + 1e-9)

def locate(M, x, t_guess, W):
    """start time of x on the recorder clock within t_guess +- W: (t, r, r_second, at_edge)"""
    pad = 3.0
    w = M.wave(t_guess - W - pad, len(x) + int((2 * W + 2 * pad) * SR))
    cf = bandfeat(x); mf = bandfeat(w)[int(pad * ESR):]
    nl = int(2 * W * ESR) + 1
    if len(cf) < 20 or len(mf) < nl + len(cf) - 1:
        return None
    r = ncc(cf, mf, nl)
    k = int(np.argmax(r)); d = 0.0
    if 0 < k < nl - 1:
        y0, y1, y2 = r[k - 1], r[k], r[k + 1]
        den = y0 - 2 * y1 + y2
        d = 0.5 * (y0 - y2) / den if den else 0.0
    rr = r.copy(); rr[max(0, k - ESR // 3):k + ESR // 3] = -1
    return t_guess - W + (k + d) / ESR, float(r[k]), float(rr.max()) if nl > ESR else 0.0, k in (0, nl - 1)

# --------------------------------------------------------------------------- timecode

def tc_label_seconds(tc, fps):
    h, m, s, f = map(int, re.split(r"[:;.]", tc))
    return h * 3600 + m * 60 + s + f / round(fps)

def tc_to_frames(tc, fps):
    """timecode label -> frame count (NDF, or drop-frame for 29.97/59.94 with ';')"""
    h, m, s, f = map(int, re.split(r"[:;.]", tc))
    tb = round(fps)
    n = ((h * 60 + m) * 60 + s) * tb + f
    if ";" in tc and tb in (30, 60):
        mins = h * 60 + m
        n -= (tb // 15) * (mins - mins // 10)
    return n

def frames_to_tc(n, tb):
    return f"{n // (tb * 3600):02d}:{n // (tb * 60) % 60:02d}:{n // tb % 60:02d}:{n % tb:02d}"

def ctime_seconds(ct):
    h, m, s = map(int, ct[11:19].split(":"))
    return h * 3600 + m * 60 + s

# --------------------------------------------------------------------------- sync

TC_TOL = 3.0         # s: max spread of (timecode - file time) for a time-of-day clock

def wrap_day(d):
    return (d + 43200.0) % 86400.0 - 43200.0

def densest(v, tol):
    """largest group of values within a 2*tol window: (count, median of the group)"""
    if not v:
        return 0, 0.0
    s = sorted(v)
    bi = bj = j = 0
    for i in range(len(s)):
        j = max(j, i)
        while j + 1 < len(s) and s[j + 1] - s[i] <= 2 * tol:
            j += 1
        if j - i > bj - bi:
            bi, bj = i, j
    return bj - bi + 1, float(np.median(s[bi:bj + 1]))

def check_timecodes(clips):
    """Time-of-day (Free Run) timecode keeps a constant distance to the file creation time; Rec Run or a
    reset timecode does not. Clips outside the main group get tc_state 'bad' and are placed by file time."""
    idx = [c for c in clips if c["tc"] and c["ctime"]]
    def d_of(c, end):
        ct = ctime_seconds(c["ctime"]) - (c["dur"] if end else 0.0)
        return wrap_day(tc_label_seconds(c["tc"], c["fps"]) - ct)
    ds = [d_of(c, False) for c in idx]
    de = [d_of(c, True) for c in idx]
    cs, ms = densest(ds, TC_TOL)
    ce, me = densest(de, TC_TOL)
    ctime_end = ce > cs
    d, center = (de, me) if ctime_end else (ds, ms)
    n = len(idx)
    member = [True] * n if n <= 2 else [abs(x - center) <= TC_TOL for x in d]
    cnt = sum(member)
    valid = n > 0 and (n <= 2 or (cnt >= 3 and cnt * 4 >= n))
    info = {"ctime_end": ctime_end, "n_tc": n,
            "dc": float(np.median([x for x, m in zip(d, member) if m])) if valid else None}
    for c in clips:
        c["tc_state"] = "none" if not c["tc"] else ("unverified" if not c["ctime"] else "ok")
    for c, m in zip(idx, member):
        if not (valid and m):
            c["tc_state"] = "bad"
    info["bad"] = [c["file"] for c in clips if c["tc_state"] == "bad"]
    info["none"] = [c["file"] for c in clips if c["tc_state"] == "none"]
    return info

def report_timecodes(cam, total, info):
    if info["bad"]:
        if len(info["bad"]) == info["n_tc"]:
            print(f"  {cam}: у ВСЕХ клипов неправильный таймкод (похоже на Rec Run или сброс) — используется время "
                  f"создания файлов. На следующей съёмке включите Free Run.")
        else:
            print(f"  {cam}: клипы с неправильным таймкодом ({len(info['bad'])} из {info['n_tc']}) — для них "
                  f"используется время создания файла: {', '.join(info['bad'])}")
    if info["none"]:
        if len(info["none"]) == total:
            print(f"  {cam}: у клипов нет таймкода — используется время создания файлов")
        else:
            print(f"  {cam}: клипы без таймкода — используется время создания файла: {', '.join(info['none'])}")

def sync_camera(M, cam, clips, info):
    """place every clip of one camera on the recorder clock"""
    # camera clock: valid timecode as is; other clips: file creation time on the same scale
    for c in clips:
        c["t_alt"] = None
        if c["tc_state"] in ("ok", "unverified"):
            c["t_cam"] = tc_label_seconds(c["tc"], c["fps"])
        elif c["ctime"]:
            cs = ctime_seconds(c["ctime"])
            if info["dc"] is not None:
                c["t_cam"] = cs - (c["dur"] if info["ctime_end"] else 0.0) + info["dc"]
            else:
                c["t_cam"], c["t_alt"] = cs, cs - c["dur"]   # start vs end decided by the audio below
        else:
            c["t_cam"] = None
    for c in clips:
        if c["has_audio"] and c["t_cam"] is not None:
            c["_x"] = np.load(c["npy"], mmap_mode="r")
            c["g_pos"], c["g_z"] = global_search(M, flux(c["_x"]))
        else:
            c["g_pos"], c["g_z"] = None, 0.0
    # anchors: strong global matches with a consistent camera offset
    cand = [c for c in clips if c["g_pos"] is not None and c["g_z"] >= 20 and c["dur"] >= 15]
    if len(cand) < 3:
        cand = [c for c in clips if c["g_pos"] is not None and c["g_z"] >= 12 and c["dur"] >= 8]
    if not cand:
        print(f"  {cam}: не найдено ни одного надёжного совпадения со звуком рекордера — камера пропущена")
        for c in clips:
            c["status"] = "unplaced"
        return
    # no valid timecode at all: does the file time mark the start or the end of the recording?
    if sum(1 for c in cand if c["t_alt"] is not None) >= 3:
        def mad(o):
            md = np.median(o)
            return float(np.median(np.abs(np.array(o) - md)))
        a_ = mad([c["g_pos"] - c["t_cam"] for c in cand])
        b_ = mad([c["g_pos"] - (c["t_alt"] if c["t_alt"] is not None else c["t_cam"]) for c in cand])
        if b_ < a_:
            for c in clips:
                if c["t_alt"] is not None:
                    c["t_cam"] = c["t_alt"]
            cand = [c for c in clips if c["g_pos"] is not None and c["g_z"] >= 20 and c["dur"] >= 15]
            if len(cand) < 3:
                cand = [c for c in clips if c["g_pos"] is not None and c["g_z"] >= 12 and c["dur"] >= 8]
    offs = np.array([c["g_pos"] - c["t_cam"] for c in cand])
    med = np.median(offs)
    anchors = [(c["t_cam"], o) for c, o in zip(cand, offs) if abs(o - med) < 3]
    at = np.array([a[0] for a in anchors]); ao = np.array([a[1] for a in anchors])
    print(f"  {cam}: смещение часов камеры ≈ {med:+.1f} с, опорных клипов {len(anchors)}")

    for c in clips:
        if c["t_cam"] is None:
            c["status"] = "unplaced"; continue
        j = np.argsort(np.abs(at - c["t_cam"]))[:3]
        c["pred"] = c["t_cam"] + float(np.median(ao[j]))
        if "_x" not in c:
            c["status"] = "no-camera-audio"; continue
        if M.covered(c["pred"], c["pred"] + c["dur"]) < 0.3:
            c["status"] = "outside-recorder"; continue
        x = np.asarray(c["_x"])
        loc = locate(M, x[:min(len(x), 60 * SR)], c["pred"], 4.0)
        if loc is None:
            c["status"] = "not-found"; continue
        p0, c["r"], c["r2"], edge = loc
        c["pos"] = p0
        if len(x) > 45 * SR:                       # long clip: 30 s chunks, line fit
            ts, os_ = [], []
            for s in range(0, len(x) // SR - 30 + 1, 30):
                q = locate(M, x[s * SR:(s + 30) * SR], p0 + s, 0.3)
                if q and q[1] > 0.15 and q[1] > 1.5 * q[2] and not q[3]:
                    ts.append(s); os_.append(q[0] - s)
            if len(ts) >= 3:
                ts, os_ = np.array(ts, float), np.array(os_)
                ok = np.ones(len(ts), bool)
                for _ in range(4):
                    b, a = np.polyfit(ts[ok], os_[ok], 1)
                    res = os_ - (a + b * ts)
                    ok = np.abs(res) < max(0.008, 3 * 1.4826 * np.median(np.abs(res[ok])))
                c["pos"] = float(a)
                if ok.sum() >= 5:
                    c["drift_ms"] = float(b * c["dur"] * 1000)
                    if np.abs(res).max() > 0.04:
                        c["note"] = f"скачок звука внутри клипа ~{np.abs(res).max() * 1000:.0f} мс"
        strong = c["r"] >= 0.3 and c["r"] >= 1.8 * c["r2"] and not edge
        c["status"] = "ok" if strong else "check"

    # neighbour band check + model placement
    ok = [c for c in clips if c.get("status") == "ok"]
    ref = ([(c["t_cam"], c["pos"] - c["t_cam"]) for c in ok] if ok else anchors)
    rt = np.array([a[0] for a in ref]); ro = np.array([a[1] for a in ref])
    for c in clips:
        if c["status"] == "unplaced":
            continue
        band = float(np.median(ro[np.argsort(np.abs(rt - c["t_cam"]))[:4]]))
        if c["status"] == "check" and abs(c["pos"] - c["t_cam"] - band) > 0.9:
            c["status"] = "model"
        c["final"] = c["pos"] if c["status"] in ("ok", "check") else c["t_cam"] + band
        c.pop("_x", None)

class ClipWave:
    """one clip's audio as a 'master' for locate(); t is seconds from the clip start"""

    def __init__(self, c):
        self.a = np.load(c["npy"], mmap_mode="r")

    def wave(self, t, n):
        out = np.zeros(n, np.float32)
        i0 = round(float(t * SR))
        lo, hi = max(0, i0), min(len(self.a), i0 + n)
        if hi > lo:
            out[lo - i0:hi - i0] = self.a[lo:hi]
        return out

CAM_W = 3.0          # search window for camera-to-camera alignment, s

def cross_camera(cams, clips):
    """clips without recorder audio: align by camera audio to overlapping clips of other cameras.
    Partners: clips placed by the recorder (or already aligned) first; then, where none exist,
    the first camera's own clips at their timecode positions (anchor camera)."""
    trusted = {id(c) for c in clips if c.get("status") in ("ok", "check")}
    for allow_ref in (False, True):
        changed = True
        while changed:
            changed = False
            todo = [c for c in clips if c.get("status") in ("outside-recorder", "not-found", "model")
                    and c["has_audio"] and "final" in c]
            for u in todo:
                best = None
                for p in clips:
                    if p["cam"] == u["cam"] or "final" not in p or not p["has_audio"]:
                        continue
                    tier = 0 if id(p) in trusted else (1 if allow_ref and p["cam"] == cams[0] else None)
                    if tier is None:
                        continue
                    # part of u that stays inside p for every lag in +-CAM_W
                    s0 = max(0.0, p["final"] - u["final"] + CAM_W)
                    s1 = min(u["dur"], p["final"] + p["dur"] - u["final"] - CAM_W, s0 + 60)
                    if s1 - s0 < 3:
                        continue
                    key = (tier, -(s1 - s0))
                    if best is None or key < best[0]:
                        best = (key, p, s0, s1)
                if best is None:
                    continue
                _, p, s0, s1 = best
                x = np.asarray(np.load(u["npy"], mmap_mode="r")[int(s0 * SR):int(s1 * SR)])
                q = locate(ClipWave(p), x, u["final"] + s0 - p["final"], CAM_W)
                if q and q[1] >= 0.1 and q[1] >= 2 * q[2] and not q[3]:
                    u["final"] = p["final"] + q[0] - s0
                    u["status"], u["r"], u["r2"] = "camsync", q[1], q[2]
                    u["note"] = f"по звуку камеры: {p['file']}"
                    trusted.add(id(u))
                    changed = True

# --------------------------------------------------------------------------- xmeml

NOTE = {"check": "ПРОВЕРИТЬ: слабая корреляция",
        "outside-recorder": "по таймкоду (нет звука рекордера), ±0.5 с",
        "no-camera-audio": "по таймкоду (нет звука в клипе), ±0.5 с",
        "not-found": "по таймкоду (звук не найден), ±0.5 с",
        "model": "по таймкоду (звук не совпал с соседями), ±0.5 с",
        "camsync": "нет звука рекордера — выровнено по звуку другой камеры"}

def write_xmeml(path, name, M, cams, clips, fps, label_offset):
    tb = round(fps); ntsc = abs(fps - tb) > 0.01
    RATE = f"<rate><timebase>{tb}</timebase><ntsc>{'TRUE' if ntsc else 'FALSE'}</ntsc></rate>"
    url = lambda p: "file://localhost/" + p.replace("\\", "/").replace(" ", "%20").lstrip("/")

    # Resolve links a WAV only if XML extents lie inside the file:
    # start = first whole frame at/after file start, end = last whole frame before file end
    for t in M.takes:
        tref = t["tref"] / t["sr"]
        t["src0"] = math.ceil(tref * fps - 1e-9)
        t["src_end"] = math.floor((tref + t["len"]) * fps + 1e-9)
        t["eff"] = t["start"] + (t["src0"] / fps - tref)
    # labels read like the first camera's clock at recorder start, then count real frames
    F0 = round(float((M.takes[0]["eff"] + label_offset) * tb))
    for t in M.takes:
        t["frame"] = F0 + round((t["eff"] - M.takes[0]["eff"]) * fps)

    def place(tt):
        best = min(M.takes, key=lambda t: 0 if t["start"] <= tt <= t["start"] + t["len"]
                   else min(abs(tt - t["start"]), abs(tt - t["start"] - t["len"])))
        return best["frame"] + round((tt - best["eff"]) * fps)

    placed = [c for c in clips if "final" in c]
    for c in placed:
        c["tl"] = place(c["final"])
        c["nf"] = int(c["dur"] * fps)
        c["src0"] = tc_to_frames(c["tc"], c["fps"]) if c["tc"] else 0
        c["new_tc"] = frames_to_tc(c["tl"], tb)
    seq0 = min([c["tl"] for c in placed] + [t["frame"] for t in M.takes]) // (tb * 60) * (tb * 60)
    seq_end = max([c["tl"] + c["nf"] for c in placed] + [t["frame"] + t["src_end"] - t["src0"] for t in M.takes])

    done = set()
    def file_xml(fid, fname, fpath, dur, src0, video, tcfmt="NDF", depth=16):
        if fid in done:
            return f'<file id="{escape(fid)}"/>'
        done.add(fid)
        v = (f"<video><samplecharacteristics>{RATE}<width>1920</width><height>1080</height>"
             f"</samplecharacteristics></video>") if video else ""
        return (f'<file id="{escape(fid)}"><name>{escape(fname)}</name><pathurl>{escape(url(fpath))}</pathurl>{RATE}'
                f"<duration>{dur}</duration><timecode>{RATE}<string>{frames_to_tc(src0, tb)}</string>"
                f"<frame>{src0}</frame><displayformat>{tcfmt}</displayformat></timecode><media>{v}<audio>"
                f"<samplecharacteristics><depth>{depth}</depth><samplerate>48000</samplerate></samplecharacteristics>"
                f"<channelcount>2</channelcount></audio></media></file>")

    def clipitem(cid, nm, start, nf, fx, kind, extra=""):
        st = ("<sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack>"
              if kind == "audio" else "")
        return (f'<clipitem id="{escape(cid)}"><name>{escape(nm)}</name><enabled>TRUE</enabled>'
                f"<duration>{nf}</duration>{RATE}<start>{start}</start><end>{start + nf}</end>"
                f"<in>0</in><out>{nf}</out>{fx}{st}{extra}</clipitem>")

    vtr, atr = [], []
    for ti, cam in enumerate(cams, 1):
        cc = sorted([c for c in placed if c["cam"] == cam], key=lambda c: c["tl"])
        vv, aa = [], []
        for i, c in enumerate(cc):
            fid = f"f-{cam}-{c['file']}"
            note = NOTE.get(c["status"], "")
            if c.get("note"):
                note = (note + "; " + c["note"]).strip("; ")
            mk = (f"<marker><name>{escape(note)}</name><comment>{escape(note)}</comment>"
                  f"<in>0</in><out>-1</out></marker>") if note else ""
            lk = "".join(f"<link><linkclipref>{escape(k + '-' + cam + '-' + c['file'])}</linkclipref>"
                         f"<mediatype>{m}</mediatype><trackindex>{ti}</trackindex><clipindex>{i + 1}</clipindex></link>"
                         for k, m in (("v", "video"), ("a", "audio")))
            fx = file_xml(fid, c["file"], c["path"], int(c["dur"] * fps), c["src0"], True,
                          "DF" if c["tc"] and ";" in c["tc"] else "NDF")
            vv.append(clipitem(f"v-{cam}-{c['file']}", c["file"], c["tl"] - seq0, c["nf"], fx, "video", mk + lk))
            if c["has_audio"]:
                aa.append(clipitem(f"a-{cam}-{c['file']}", c["file"], c["tl"] - seq0, c["nf"],
                                   f'<file id="{escape(fid)}"/>', "audio", lk))
        vtr.append(vv); atr.append(aa)
    for var in M.variants:
        items = []
        for i, t in enumerate(M.takes):
            w = next((w for w in t["files"] if variant(w["file"]) == var), None)
            if w is None:
                continue
            nf = t["src_end"] - t["src0"]
            if i + 1 < len(M.takes):
                nf = min(nf, M.takes[i + 1]["frame"] - t["frame"])
            fx = file_xml("f-rec-" + w["file"], w["file"], w["path"], t["src_end"] - t["src0"], t["src0"], False,
                          depth=24)
            items.append(clipitem("a-rec-" + w["file"], w["file"], t["frame"] - seq0, nf, fx, "audio"))
        atr.append(items)

    track = lambda it: "<track>" + "".join(it) + "<enabled>TRUE</enabled><locked>FALSE</locked></track>"
    xml = ('<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE xmeml>\n<xmeml version="4">'
           f'<sequence id="seq-1"><name>{escape(name)}</name><duration>{seq_end - seq0}</duration>{RATE}'
           f"<timecode>{RATE}<string>{frames_to_tc(seq0, tb)}</string><frame>{seq0}</frame>"
           f"<displayformat>NDF</displayformat></timecode><media><video><format><samplecharacteristics>{RATE}"
           f"<width>1920</width><height>1080</height><pixelaspectratio>square</pixelaspectratio>"
           f"</samplecharacteristics></format>" + "".join(track(v) for v in vtr) +
           "</video><audio><numOutputChannels>2</numOutputChannels>" + "".join(track(a) for a in atr) +
           "</audio></media></sequence></xmeml>\n")
    with open(path, "w", encoding="utf-8") as f:
        f.write(xml)

# --------------------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser(description="slatefree — sync cameras to a field-recorder track")
    ap.add_argument("project", help="папка съёмки")
    ap.add_argument("--cams", nargs="*", help="папки камер (по умолчанию — все папки с видео, кроме --audio)")
    ap.add_argument("--audio", default="AUDIO", help="папка с WAV рекордера (по умолчанию AUDIO)")
    ap.add_argument("--ref-suffix", default="I",
                    help="по какому варианту каналов искать (H4n 4CH: I = встроенные микрофоны)")
    ap.add_argument("--fps", type=float, help="частота таймлайна (по умолчанию — самая частая у клипов)")
    ap.add_argument("--out", help="папка результата (по умолчанию <project>/SYNC)")
    ap.add_argument("--exclude", nargs="*", default=[], help="маски файлов, которые не трогать (напр. summ*.wav)")
    ap.add_argument("--jobs", type=int, default=6, help="параллельных ffmpeg при извлечении звука")
    a = ap.parse_args()

    proj = os.path.abspath(a.project)
    out = a.out or os.path.join(proj, "SYNC")
    cache = os.path.join(out, ".cache")
    os.makedirs(cache, exist_ok=True)
    adir = os.path.join(proj, a.audio)
    cams = a.cams or sorted(d for d in os.listdir(proj) if os.path.isdir(os.path.join(proj, d))
                            and d != a.audio and any(f.lower().endswith(VIDEO_EXT)
                                                     for f in os.listdir(os.path.join(proj, d))))
    print(f"Камеры: {cams}; рекордер: {a.audio}")

    excl = lambda f: any(fnmatch.fnmatch(f.lower(), p.lower()) for p in a.exclude)
    jobs = [(None, os.path.join(adir, f)) for f in sorted(os.listdir(adir)) if f.lower().endswith(".wav") and not excl(f)]
    for cam in cams:
        jobs += [(cam, os.path.join(proj, cam, f)) for f in sorted(os.listdir(os.path.join(proj, cam)))
                 if f.lower().endswith(VIDEO_EXT) and not excl(f)]
    T = [time.perf_counter()]
    lap = lambda what: (T.append(time.perf_counter()), print(f"   [{what}: {T[-1] - T[-2]:.1f} с]"))
    print(f"Чтение метаданных ({len(jobs)} файлов)…")
    with ThreadPoolExecutor(a.jobs) as ex:
        metas = list(ex.map(lambda j: probe(j[1]), jobs))
    EDITORS = ("resolve", "premiere", "final cut", "audition", "reaper", "pro tools", "fairlight", "ffmpeg", "lavf")
    wavs, ignored = [], []
    for (cam, _), m in zip(jobs, metas):
        if cam is None:
            (ignored if m["tref"] is None or any(e in m["encoder"].lower() for e in EDITORS) else wavs).append(m)
    if ignored:
        print(f"WAV не с рекордера (пропущены): {', '.join(m['file'] for m in ignored)}")
    clips = []
    for (cam, _), m in zip(jobs, metas):
        if cam:
            m["cam"] = cam
            clips.append(m)

    lap("метаданные")
    fps = a.fps or Counter(c["fps"] for c in clips if c["fps"]).most_common(1)[0][0]
    skipped = [c for c in clips if not c["fps"] or abs(c["fps"] - fps) > 0.01]
    clips = [c for c in clips if c not in skipped]
    if skipped:
        print(f"Пропущено {len(skipped)} клипов с другой частотой кадров (таймлайн {fps:.3f}): "
              f"{', '.join(sorted(c['file'] for c in skipped)[:8])}{' …' if len(skipped) > 8 else ''}")

    print("Проверка таймкода…")
    tc_infos = {}
    for cam in cams:
        cc = [c for c in clips if c["cam"] == cam]
        tc_infos[cam] = check_timecodes(cc)
        report_timecodes(cam, len(cc), tc_infos[cam])
    if not any(i["bad"] or i["none"] for i in tc_infos.values()):
        print("  таймкод у всех клипов в порядке")

    need = [w["path"] for w in wavs] + [c["path"] for c in clips if c["has_audio"]]
    print(f"Извлечение звука ({len(need)} файлов, кэш в {cache})…")
    with ThreadPoolExecutor(a.jobs) as ex:
        list(ex.map(lambda p: extract(p, cache), need))
    lap("извлечение звука")
    for c in clips:
        c["npy"] = os.path.join(cache, c["file"] + ".npy")
        if c["has_audio"] and np.load(c["npy"], mmap_mode="r").size < SR:
            c["has_audio"] = False

    M = Master(wavs, cache, a.ref_suffix)
    lap("признаки рекордера")
    print("Синхронизация…")
    for cam in cams:
        sync_camera(M, cam, [c for c in clips if c["cam"] == cam], tc_infos[cam])
        lap(f"синхронизация {cam}")
    cross_camera(cams, clips)
    lap("камера ↔ камера")

    # timeline labels: recorder clock shifted to the first camera's clock
    first = [c for c in clips if c["cam"] == cams[0] and c.get("status") == "ok"]
    label_offset = float(np.median([c["t_cam"] - c["final"] for c in first])) if first else 0.0

    name = os.path.basename(proj.rstrip("\\/")) + "_sync"
    xml_path = os.path.join(out, name + ".xml")
    write_xmeml(xml_path, name, M, cams, clips, fps, label_offset)

    with open(os.path.join(out, "sync_result.csv"), "w", newline="", encoding="utf-8-sig") as f:
        w = csv.writer(f, delimiter=";")
        w.writerow(["camera", "file", "fps", "duration_s", "orig_tc", "tc_check", "timeline_tc", "status", "r",
                    "r_second", "intra_drift_ms", "note"])
        for c in sorted(clips, key=lambda c: c.get("tl", 1e12)):
            w.writerow([c["cam"], c["file"], f"{c['fps']:.3f}", f"{c['dur']:.2f}", c["tc"] or "", c["tc_state"],
                        c.get("new_tc", ""), c.get("status"), f"{c.get('r', 0):.2f}", f"{c.get('r2', 0):.2f}",
                        round(c["drift_ms"]) if "drift_ms" in c else "", c.get("note", "")])
        for c in skipped:
            w.writerow([c["cam"], c["file"], f"{c['fps']:.3f}" if c["fps"] else "", f"{c['dur']:.2f}", c["tc"] or "",
                        "", "", "skipped-fps", "", "", "", ""])

    print("\nИтог:")
    for cam in cams:
        st = Counter(c.get("status") for c in clips if c["cam"] == cam)
        print(f"  {cam}: " + ", ".join(f"{k} {v}" for k, v in st.most_common()))
    chk = [c["file"] for c in clips if c.get("status") == "check"]
    if chk:
        print(f"  проверить вручную: {', '.join(chk)}")
    print(f"\nТаймлайн: {xml_path}\nResolve: File > Import > Timeline (частота проекта {fps})")

if __name__ == "__main__":
    main()
