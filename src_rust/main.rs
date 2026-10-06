//! slatefree — sync multi-camera event footage to one continuous field-recorder track
//! (Zoom H4n etc.) by audio, and build a DaVinci Resolve timeline (FCP7 XML, fixed tracks).
//!
//! Same algorithm as src_python/slatefree.py; heavy work runs in parallel (rayon).

use rayon::prelude::*;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

const SR: usize = 8000; // analysis sample rate
const HOP: usize = 80; // 10 ms feature hop
const ESR: usize = SR / HOP; // 100 Hz
const NFFT: usize = 512;
const NB: usize = 16;
const VIDEO_EXT: [&str; 6] = ["mov", "mp4", "mxf", "mts", "m4v", "avi"];
const STD_FPS: [f64; 11] = [
    24000.0 / 1001.0, 24.0, 25.0, 30000.0 / 1001.0, 30.0, 48.0, 50.0,
    60000.0 / 1001.0, 60.0, 120000.0 / 1001.0, 120.0,
];
const EDITORS: [&str; 9] = [
    "resolve", "premiere", "final cut", "audition", "reaper", "pro tools", "fairlight", "ffmpeg", "lavf",
];

type Row = [f32; NB];

// ------------------------------------------------------------------------- i18n

static LANG_RU: OnceLock<bool> = OnceLock::new();

/// Russian UI when the Windows UI language is Russian (or --lang ru / SLATEFREE_LANG=ru)
fn ru() -> bool {
    *LANG_RU.get_or_init(|| {
        if let Ok(v) = std::env::var("SLATEFREE_LANG") {
            return v.to_lowercase().starts_with("ru");
        }
        #[cfg(windows)]
        {
            let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
            id & 0x3ff == 0x19
        }
        #[cfg(not(windows))]
        {
            std::env::var("LANG").map(|v| v.starts_with("ru")).unwrap_or(false)
        }
    })
}

/// pick the Russian or English text
macro_rules! tr {
    ($ru:literal, $en:literal $(, $a:expr)* $(,)?) => {
        if ru() { format!($ru $(, $a)*) } else { format!($en $(, $a)*) }
    };
}

// ------------------------------------------------------------------------- ffmpeg

fn ffmpeg_path() -> &'static PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        // 1) next to the program, 2) PATH, 3) Homebrew / usual Unix places (Finder-launched apps may lack PATH)
        let mut dirs: Vec<PathBuf> =
            std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).into_iter().collect();
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        if !cfg!(windows) {
            dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
        }
        dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(name))
    })
}

fn ffmpeg_hint() -> String {
    if cfg!(target_os = "macos") {
        tr!("Установите ffmpeg: brew install ffmpeg (https://brew.sh)", "Install ffmpeg: brew install ffmpeg (https://brew.sh)")
    } else if cfg!(windows) {
        tr!("Положите ffmpeg.exe рядом с slatefree.exe", "Put ffmpeg.exe next to slatefree.exe")
    } else {
        tr!("Установите ffmpeg из пакетов системы (например, sudo apt install ffmpeg)",
            "Install ffmpeg from your distribution (e.g. sudo apt install ffmpeg)")
    }
}

fn ffmpeg() -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut c = Command::new(ffmpeg_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

// ------------------------------------------------------------------------- media

#[derive(Clone, Debug, Default)]
struct Media {
    path: PathBuf,
    file: String,
    size: u64,
    dur: f64,
    tc: Option<String>,
    ctime: Option<String>,
    tref: Option<u64>,
    fps: Option<f64>,
    has_audio: bool,
    encoder: String,
    // camera clip state
    cam: String,
    cache: PathBuf,
    t_cam: Option<f64>,
    g_pos: Option<f64>,
    g_z: f64,
    pos: Option<f64>,
    r: f64,
    r2: f64,
    drift_ms: Option<f64>,
    note: String,
    status: String,
    final_t: Option<f64>,
    tl: Option<i64>,
    nf: i64,
    src0: i64,
    new_tc: String,
    tc_state: String,
    t_alt: Option<f64>,
}

fn snap_fps(f: f64) -> f64 {
    let best = STD_FPS
        .iter()
        .copied()
        .min_by(|a, b| (a - f).abs().partial_cmp(&(b - f).abs()).unwrap())
        .unwrap();
    if (best - f).abs() < 0.02 { best } else { f }
}

struct Rx {
    dur: Regex,
    tc: Regex,
    ctime: Regex,
    tref: Regex,
    fps: Regex,
    audio: Regex,
    enc: Regex,
}

fn rx() -> &'static Rx {
    static R: OnceLock<Rx> = OnceLock::new();
    R.get_or_init(|| Rx {
        dur: Regex::new(r"Duration: (\d+):(\d+):([\d.]+)").unwrap(),
        tc: Regex::new(r"timecode\s*:\s*(\d\d[:;]\d\d[:;]\d\d[:;.]\d\d)").unwrap(),
        ctime: Regex::new(r"creation_time\s*:\s*(\S+)").unwrap(),
        tref: Regex::new(r"time_reference\s*:\s*(\d+)").unwrap(),
        fps: Regex::new(r"([\d.]+) fps").unwrap(),
        audio: Regex::new(r"Stream #\S+: Audio").unwrap(),
        enc: Regex::new(r"encoded_by\s*:\s*(.+)").unwrap(),
    })
}

fn probe(path: &Path) -> Media {
    let out = ffmpeg().arg("-hide_banner").arg("-i").arg(path).stdin(Stdio::null()).output();
    let r = match out {
        Ok(o) => String::from_utf8_lossy(&o.stderr).replace('\r', ""),
        Err(e) => {
            eprintln!("{}", tr!("Не удалось запустить ffmpeg ({}): {}", "Cannot run ffmpeg ({}): {}", ffmpeg_path().display(), e));
            eprintln!("{}", ffmpeg_hint());
            pause_if_own_console();
            std::process::exit(2);
        }
    };
    let x = rx();
    let mut d = Media {
        path: path.to_path_buf(),
        file: path.file_name().unwrap().to_string_lossy().into_owned(),
        size: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        ..Default::default()
    };
    if let Some(m) = x.dur.captures(&r) {
        d.dur = m[1].parse::<f64>().unwrap() * 3600.0 + m[2].parse::<f64>().unwrap() * 60.0
            + m[3].parse::<f64>().unwrap();
    }
    d.tc = x.tc.captures(&r).map(|m| m[1].to_string());
    d.ctime = x.ctime.captures(&r).map(|m| m[1].to_string());
    d.tref = x.tref.captures(&r).and_then(|m| m[1].parse().ok());
    d.fps = x.fps.captures(&r).and_then(|m| m[1].parse::<f64>().ok()).map(snap_fps);
    d.encoder = x.enc.captures(&r).map(|m| m[1].trim().to_string()).unwrap_or_default();
    d.has_audio = x.audio.is_match(&r);
    d
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// number of sample frames and sample rate from the WAV header (works for >2 GB)
fn wav_frames(path: &Path) -> (u64, u32) {
    let mut head = vec![0u8; 1 << 16];
    let n = fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    head.truncate(n);
    let fmt = find(&head, b"fmt ").unwrap_or(0);
    let u16at = |i: usize| u16::from_le_bytes([head[i], head[i + 1]]) as u64;
    let u32at = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]) as u64;
    let ch = u16at(fmt + 10);
    let sr = u32at(fmt + 12) as u32;
    let bits = u16at(fmt + 22);
    let i = find(&head, b"data").unwrap_or(0);
    let fsize = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let size = u32at(i + 4).min(fsize.saturating_sub(i as u64 + 8));
    (size / (ch * bits / 8).max(1), sr)
}

// ------------------------------------------------------------------------- audio cache

fn cache_path(cache: &Path, file: &str) -> PathBuf {
    cache.join(format!("{file}.f32"))
}

fn extract(path: &Path, cache: &Path) {
    let dst = cache_path(cache, &path.file_name().unwrap().to_string_lossy());
    if dst.exists() {
        return;
    }
    let out = ffmpeg()
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", &SR.to_string(), "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("ffmpeg");
    let tmp = dst.with_extension("part");
    fs::write(&tmp, &out.stdout).expect("cache write");
    fs::rename(&tmp, &dst).expect("cache rename");
}

fn load_f32(p: &Path) -> Vec<f32> {
    let b = fs::read(p).unwrap_or_default();
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

// ------------------------------------------------------------------------- features

struct FeatTab {
    win: Vec<f32>,
    band_of: Vec<i8>, // band index per rfft bin, -1 = unused
    fft: Arc<dyn RealToComplex<f32>>,
}

fn tab() -> &'static FeatTab {
    static T: OnceLock<FeatTab> = OnceLock::new();
    T.get_or_init(|| {
        let (l0, l1) = (150f64.ln(), 3800f64.ln());
        let mut edges: Vec<f64> = (0..=NB).map(|i| (l0 + (l1 - l0) * i as f64 / NB as f64).exp()).collect();
        edges[0] = 150.0;
        edges[NB] = 3800.0;
        let band_of = (0..=NFFT / 2)
            .map(|k| {
                let f = k as f64 * SR as f64 / NFFT as f64;
                (0..NB).find(|&b| f >= edges[b] && f < edges[b + 1]).map(|b| b as i8).unwrap_or(-1)
            })
            .collect();
        let win = (0..NFFT)
            .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (NFFT - 1) as f64).cos()) as f32)
            .collect();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(NFFT);
        FeatTab { win, band_of, fft }
    })
}

/// log band energies; frame i covers samples [i*HOP, i*HOP+NFFT)
fn logbands(x: &[f32]) -> Vec<Row> {
    if x.len() < NFFT {
        return Vec::new();
    }
    let n = (x.len() - NFFT) / HOP + 1;
    let t = tab();
    let mut out = vec![[0f32; NB]; n];
    out.par_chunks_mut(2048).enumerate().for_each(|(ci, rows)| {
        let mut inp = t.fft.make_input_vec();
        let mut spec = t.fft.make_output_vec();
        let mut scratch = t.fft.make_scratch_vec();
        for (j, row) in rows.iter_mut().enumerate() {
            let s = (ci * 2048 + j) * HOP;
            for i in 0..NFFT {
                inp[i] = x[s + i] * t.win[i];
            }
            t.fft.process_with_scratch(&mut inp, &mut spec, &mut scratch).unwrap();
            let mut acc = [0f64; NB];
            for (k, c) in spec.iter().enumerate() {
                let b = t.band_of[k];
                if b >= 0 {
                    acc[b as usize] += (c.re * c.re + c.im * c.im) as f64;
                }
            }
            for b in 0..NB {
                row[b] = (acc[b] + 1e-9).ln() as f32;
            }
        }
    });
    out
}

/// scipy.ndimage.uniform_filter1d(mode='reflect'), window [i - k/2, i + k - 1 - k/2]
fn uniform_reflect(d: &[f64], k: usize) -> Vec<f64> {
    let n = d.len();
    if n == 0 {
        return Vec::new();
    }
    let left = k / 2;
    let nn = n as isize;
    let get = |mut i: isize| -> f64 {
        loop {
            if i < 0 {
                i = -i - 1;
            } else if i >= nn {
                i = 2 * nn - i - 1;
            } else {
                return d[i as usize];
            }
        }
    };
    let total = n + k - 1;
    let mut cs = vec![0f64; total + 1];
    for j in 0..total {
        cs[j + 1] = cs[j] + get(j as isize - left as isize);
    }
    (0..n).map(|i| (cs[i + k] - cs[i]) / k as f64).collect()
}

fn locnorm(d: &[f64], k: usize) -> Vec<f64> {
    let m = uniform_reflect(d, k);
    let sq: Vec<f64> = d.iter().map(|v| v * v).collect();
    let m2 = uniform_reflect(&sq, k);
    d.iter()
        .zip(m.iter().zip(m2.iter()))
        .map(|(v, (mm, mm2))| (v - mm) / ((mm2 - mm * mm).max(1e-12).sqrt() + 1e-3))
        .collect()
}

fn silent_mask(b: &[Row]) -> Vec<bool> {
    let mx: Vec<f64> = b.iter().map(|r| r.iter().fold(f32::MIN, |a, &v| a.max(v)) as f64).collect();
    let thr = (1e-6f64).ln();
    uniform_reflect(&mx, 3 * ESR).into_iter().map(|v| v < thr).collect()
}

/// summed spectral flux, 100 Hz — robust onset feature for the global search
fn flux(x: &[f32]) -> Vec<f32> {
    let b = logbands(x);
    if b.len() < 2 {
        return vec![0.0; b.len()];
    }
    let d: Vec<f64> = (0..b.len())
        .map(|t| {
            if t == 0 {
                0.0
            } else {
                (0..NB).map(|k| ((b[t][k] - b[t - 1][k]) as f64).max(0.0)).sum()
            }
        })
        .collect();
    let f = locnorm(&d, 3 * ESR);
    let sil = silent_mask(&b);
    f.iter().zip(sil).map(|(&v, s)| if s { 0.0 } else { v.clamp(-3.0, 6.0) as f32 }).collect()
}

/// per-band detrended log energy (timbre/melody) — resolves beat ambiguity in music
fn bandfeat(x: &[f32]) -> Vec<Row> {
    let b = logbands(x);
    let n = b.len();
    if n < 2 {
        return vec![[0.0; NB]; n];
    }
    let mut out = vec![[0f32; NB]; n];
    for k in 0..NB {
        let col: Vec<f64> = b.iter().map(|r| r[k] as f64).collect();
        for (t, v) in locnorm(&col, ESR).into_iter().enumerate() {
            out[t][k] = v.clamp(-3.0, 3.0) as f32;
        }
    }
    for (t, s) in silent_mask(&b).into_iter().enumerate() {
        if s {
            out[t] = [0.0; NB];
        }
    }
    out
}

// ------------------------------------------------------------------------- stats helpers

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { 0.5 * (s[n / 2 - 1] + s[n / 2]) }
}

fn median_f32(v: &[f32]) -> f64 {
    let mut s: Vec<f32> = v.to_vec();
    let n = s.len();
    let cmp = |a: &f32, b: &f32| a.partial_cmp(b).unwrap();
    if n % 2 == 1 {
        *s.select_nth_unstable_by(n / 2, cmp).1 as f64
    } else {
        let hi = *s.select_nth_unstable_by(n / 2, cmp).1 as f64;
        let lo = s[..n / 2].iter().cloned().fold(f32::MIN, f32::max) as f64;
        0.5 * (lo + hi)
    }
}

fn argmax<T: PartialOrd + Copy>(v: &[T]) -> usize {
    let mut k = 0;
    for i in 1..v.len() {
        if v[i] > v[k] {
            k = i;
        }
    }
    k
}

/// indices of the `m` values nearest to `t` (stable)
fn nearest(at: &[f64], t: f64, m: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..at.len()).collect();
    idx.sort_by(|&a, &b| (at[a] - t).abs().partial_cmp(&(at[b] - t).abs()).unwrap());
    idx.truncate(m);
    idx
}

fn polyfit1(x: &[f64], y: &[f64]) -> (f64, f64) {
    let n = x.len() as f64;
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let sxy: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let sxx: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
    let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    (b, my - b * mx)
}

// ------------------------------------------------------------------------- recorder master

fn variant(fname: &str) -> String {
    let stem = Path::new(fname).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let c: Vec<char> = stem.chars().collect();
    if c.len() >= 2 && c[c.len() - 1].is_alphabetic() && c[c.len() - 2].is_ascii_digit() {
        c[c.len() - 1].to_uppercase().to_string()
    } else {
        String::new()
    }
}

struct Take {
    tref: u64,
    sr: u32,
    len: f64,
    files: Vec<Media>,
    ref_i: usize,
    start: f64,
    audio: Vec<f32>,
    src0: i64,
    src_end: i64,
    eff: f64,
    frame: i64,
}

struct Master {
    takes: Vec<Take>,
    variants: Vec<String>,
    t0: f64,
    flux: Vec<f32>,
    fft_len: usize,
    flux_spec: Vec<Complex<f32>>,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
}

impl Master {
    /// group recorder files into takes (same TimeReference + length) — no audio loaded yet
    fn group(wavs: Vec<Media>, ref_suffix: &str) -> Vec<Take> {
        let mut groups: BTreeMap<(u64, u64), Vec<(Media, u32)>> = BTreeMap::new();
        for w in wavs {
            let (n, sr) = wav_frames(&w.path);
            if sr == 0 || (n as f64) / (sr as f64) < 5.0 {
                continue; // test blips
            }
            groups.entry((w.tref.unwrap(), n)).or_default().push((w, sr));
        }
        let mut takes: Vec<Take> = groups
            .into_iter()
            .map(|((tref, n), mut fs)| {
                fs.sort_by(|a, b| a.0.file.cmp(&b.0.file));
                let sr = fs[0].1;
                let files: Vec<Media> = fs.into_iter().map(|x| x.0).collect();
                let ref_i = files
                    .iter()
                    .position(|w| variant(&w.file) == ref_suffix)
                    .unwrap_or_else(|| (0..files.len()).max_by_key(|&i| files[i].size).unwrap());
                Take {
                    tref, sr, len: n as f64 / sr as f64, files, ref_i, start: tref as f64 / sr as f64,
                    audio: Vec::new(), src0: 0, src_end: 0, eff: 0.0, frame: 0,
                }
            })
            .collect();
        takes.sort_by_key(|t| t.tref);
        // continuation of a size-split recording (2 GB / 4 GB): start = prev start + prev length
        for i in 1..takes.len() {
            let prev_end = takes[i - 1].start + takes[i - 1].len;
            if takes[i - 1].files[takes[i - 1].ref_i].size as f64 > 1.9e9 && (takes[i].start - prev_end).abs() < 6.0 {
                takes[i].start = prev_end;
            }
        }
        takes
    }

    fn new(mut takes: Vec<Take>, cache: &Path, max_clip_frames: usize) -> Master {
        if takes.is_empty() {
            eprintln!("{}", tr!("Нет WAV-записей рекордера с TimeReference (BWF)", "No recorder WAV files with a BWF TimeReference found"));
            pause_if_own_console();
            std::process::exit(1);
        }
        let mut variants: Vec<String> = takes.iter().flat_map(|t| t.files.iter().map(|w| variant(&w.file))).collect();
        variants.sort();
        variants.dedup();
        let t0 = takes[0].start;
        let t1 = takes.iter().map(|t| t.start + t.len).fold(f64::MIN, f64::max);
        println!(
            "{}",
            tr!("Рекордер: {} записей, {:.2} ч, варианты каналов: {:?}",
                "Recorder: {} takes, {:.2} h, channel sets: {:?}",
                takes.len(), (t1 - t0) / 3600.0, variants)
        );
        for t in takes.iter_mut() {
            t.audio = load_f32(&cache_path(cache, &t.files[t.ref_i].file));
        }
        let mut fl = vec![0f32; ((t1 - t0) * ESR as f64) as usize + 1];
        for t in &takes {
            let f = flux(&t.audio);
            let i = ((t.start - t0) * ESR as f64).round() as usize;
            let end = (i + f.len()).min(fl.len());
            if end > i {
                fl[i..end].copy_from_slice(&f[..end - i]);
            }
        }
        let fft_len = (fl.len() + max_clip_frames + 1).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(fft_len);
        let c2r = planner.plan_fft_inverse(fft_len);
        let mut inp = r2c.make_input_vec();
        inp[..fl.len()].copy_from_slice(&fl);
        let mut flux_spec = r2c.make_output_vec();
        r2c.process(&mut inp, &mut flux_spec).unwrap();
        Master { takes, variants, t0, flux: fl, fft_len, flux_spec, r2c, c2r }
    }

    fn wave(&self, t: f64, n: usize) -> Vec<f32> {
        let mut out = vec![0f32; n];
        for tk in &self.takes {
            let a = &tk.audio;
            let i0 = ((t - tk.start) * SR as f64).round() as i64;
            let lo = i0.max(0);
            let hi = (a.len() as i64).min(i0 + n as i64);
            if hi > lo {
                out[(lo - i0) as usize..(hi - i0) as usize].copy_from_slice(&a[lo as usize..hi as usize]);
            }
        }
        out
    }

    fn covered(&self, t0: f64, t1: f64) -> f64 {
        let tot: f64 = self.takes.iter().map(|tk| (t1.min(tk.start + tk.len) - t0.max(tk.start)).max(0.0)).sum();
        tot / (t1 - t0).max(1e-9)
    }

    /// coarse position of clip flux f in the whole master flux: (t, z)
    fn global_search(&self, f: &[f32]) -> Option<(f64, f64)> {
        let n = f.len();
        let nm = self.flux.len();
        if n < ESR || n >= nm || n + nm > self.fft_len {
            return None;
        }
        let mean = f.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        let mut inp = self.r2c.make_input_vec();
        for (i, &v) in f.iter().enumerate() {
            inp[i] = (v as f64 - mean) as f32;
        }
        let mut spec = self.r2c.make_output_vec();
        self.r2c.process(&mut inp, &mut spec).unwrap();
        for (s, m) in spec.iter_mut().zip(&self.flux_spec) {
            *s = m * s.conj();
        }
        let last = spec.len() - 1;
        spec[0].im = 0.0;
        spec[last].im = 0.0;
        let mut r = self.c2r.make_output_vec();
        let _ = self.c2r.process(&mut spec, &mut r);
        let scale = 1.0 / (self.fft_len as f32 * n as f32);
        let r: Vec<f32> = r[..=nm - n].iter().map(|v| v * scale).collect();
        let k = argmax(&r);
        let med = median_f32(&r);
        let dev: Vec<f32> = r.iter().map(|v| (*v as f64 - med).abs() as f32).collect();
        let mad = 1.4826 * median_f32(&dev) + 1e-12;
        Some((self.t0 + k as f64 / ESR as f64, (r[k] as f64 - med) / mad))
    }
}

// ------------------------------------------------------------------------- correlation

/// Pearson correlation of n x NB clip features vs master features at lags 0..nl-1
fn ncc(cf: &[Row], mf: &[Row], nl: usize) -> Vec<f64> {
    let n = cf.len();
    let mut mean = [0f64; NB];
    for r in cf {
        for b in 0..NB {
            mean[b] += r[b] as f64;
        }
    }
    for m in mean.iter_mut() {
        *m /= n as f64;
    }
    let c0: Vec<[f64; NB]> = cf.iter().map(|r| std::array::from_fn(|b| r[b] as f64 - mean[b])).collect();
    let c0sq: f64 = c0.iter().flat_map(|r| r.iter()).map(|v| v * v).sum();
    let seg = &mf[..nl + n - 1];
    let mut cs1 = vec![[0f64; NB]; seg.len() + 1];
    let mut cs2 = vec![[0f64; NB]; seg.len() + 1];
    for (i, r) in seg.iter().enumerate() {
        for b in 0..NB {
            cs1[i + 1][b] = cs1[i][b] + r[b] as f64;
            cs2[i + 1][b] = cs2[i][b] + (r[b] as f64) * (r[b] as f64);
        }
    }
    (0..nl)
        .into_par_iter()
        .with_min_len(16)
        .map(|k| {
            let mut num = 0f64;
            for t in 0..n {
                let m = &seg[k + t];
                let c = &c0[t];
                let mut s = 0f64;
                for b in 0..NB {
                    s += c[b] * m[b] as f64;
                }
                num += s;
            }
            let mut var_m = 0f64;
            for b in 0..NB {
                let s1 = cs1[k + n][b] - cs1[k][b];
                let s2 = cs2[k + n][b] - cs2[k][b];
                var_m += s2 - s1 * s1 / n as f64;
            }
            num / ((c0sq * var_m.max(1e-9)).sqrt() + 1e-9)
        })
        .collect()
}

/// anything that can hand out a waveform window: the recorder master or a single clip
trait Wave: Sync {
    fn wave(&self, t: f64, n: usize) -> Vec<f32>;
}

impl Wave for Master {
    fn wave(&self, t: f64, n: usize) -> Vec<f32> {
        Master::wave(self, t, n)
    }
}

/// one clip's audio; t is seconds from the clip start
struct ClipWave(Vec<f32>);

impl Wave for ClipWave {
    fn wave(&self, t: f64, n: usize) -> Vec<f32> {
        let mut out = vec![0f32; n];
        let i0 = (t * SR as f64).round() as i64;
        let lo = i0.max(0);
        let hi = (self.0.len() as i64).min(i0 + n as i64);
        if hi > lo {
            out[(lo - i0) as usize..(hi - i0) as usize].copy_from_slice(&self.0[lo as usize..hi as usize]);
        }
        out
    }
}

/// start time of x on the source's clock within t_guess +- w: (t, r, r_second, at_edge)
fn locate(m: &dyn Wave, x: &[f32], t_guess: f64, w: f64) -> Option<(f64, f64, f64, bool)> {
    let pad = 3.0;
    let win = m.wave(t_guess - w - pad, x.len() + ((2.0 * w + 2.0 * pad) * SR as f64) as usize);
    let cf = bandfeat(x);
    let mf_all = bandfeat(&win);
    let skip = (pad * ESR as f64) as usize;
    let mf = if mf_all.len() > skip { &mf_all[skip..] } else { &mf_all[..0] };
    let nl = (2.0 * w * ESR as f64) as usize + 1;
    if cf.len() < 20 || mf.len() < nl + cf.len() - 1 {
        return None;
    }
    let r = ncc(&cf, mf, nl);
    let k = argmax(&r);
    let mut d = 0.0;
    if k > 0 && k < nl - 1 {
        let (y0, y1, y2) = (r[k - 1], r[k], r[k + 1]);
        let den = y0 - 2.0 * y1 + y2;
        if den != 0.0 {
            d = 0.5 * (y0 - y2) / den;
        }
    }
    let mut rr = r.clone();
    for v in rr[k.saturating_sub(ESR / 3)..(k + ESR / 3).min(nl)].iter_mut() {
        *v = -1.0;
    }
    let second = if nl > ESR { rr.iter().cloned().fold(f64::MIN, f64::max) } else { 0.0 };
    Some((t_guess - w + (k as f64 + d) / ESR as f64, r[k], second, k == 0 || k == nl - 1))
}

// ------------------------------------------------------------------------- timecode

fn tc_parts(tc: &str) -> [i64; 4] {
    let v: Vec<i64> = tc.split([':', ';', '.']).map(|s| s.parse().unwrap_or(0)).collect();
    [v[0], v[1], v[2], v[3]]
}

fn tc_label_seconds(tc: &str, fps: f64) -> f64 {
    let [h, m, s, f] = tc_parts(tc);
    (h * 3600 + m * 60 + s) as f64 + f as f64 / fps.round()
}

/// timecode label -> frame count (NDF, or drop-frame for 29.97/59.94 with ';')
fn tc_to_frames(tc: &str, fps: f64) -> i64 {
    let [h, m, s, f] = tc_parts(tc);
    let tb = fps.round() as i64;
    let mut n = ((h * 60 + m) * 60 + s) * tb + f;
    if tc.contains(';') && (tb == 30 || tb == 60) {
        let mins = h * 60 + m;
        n -= (tb / 15) * (mins - mins / 10);
    }
    n
}

fn frames_to_tc(n: i64, tb: i64) -> String {
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        n.div_euclid(tb * 3600),
        n.div_euclid(tb * 60).rem_euclid(60),
        n.div_euclid(tb).rem_euclid(60),
        n.rem_euclid(tb)
    )
}

fn ctime_seconds(ct: &str) -> f64 {
    let p: Vec<f64> = ct.get(11..19).unwrap_or("00:00:00").split(':').map(|s| s.parse().unwrap_or(0.0)).collect();
    p[0] * 3600.0 + p[1] * 60.0 + p[2]
}

// ------------------------------------------------------------------------- sync

const TC_TOL: f64 = 3.0; // s: max spread of (timecode - file time) for a time-of-day clock

/// per-camera result of the timecode check
#[derive(Default)]
struct TcInfo {
    /// median (timecode - file creation time) of the clips with a valid time-of-day timecode
    dc: Option<f64>,
    /// file creation time marks the end of the recording (not the start)
    ctime_end: bool,
    n_tc: usize,
    bad: Vec<String>,
    none: Vec<String>,
}

fn wrap_day(d: f64) -> f64 {
    (d + 43200.0).rem_euclid(86400.0) - 43200.0
}

/// largest group of values within a 2*tol window: (count, median of the group)
fn densest(v: &[f64], tol: f64) -> (usize, f64) {
    if v.is_empty() {
        return (0, 0.0);
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (mut bi, mut bj, mut j) = (0, 0, 0);
    for i in 0..s.len() {
        j = j.max(i);
        while j + 1 < s.len() && s[j + 1] - s[i] <= 2.0 * tol {
            j += 1;
        }
        if j - i > bj - bi {
            bi = i;
            bj = j;
        }
    }
    (bj - bi + 1, median(&s[bi..=bj]))
}

/// Time-of-day (Free Run) timecode keeps a constant distance to the file creation time; Rec Run or a reset
/// timecode does not. Clips outside the main group get tc_state "bad" and are placed by file time instead.
fn check_timecodes(clips: &mut [Media]) -> TcInfo {
    let idx: Vec<usize> = (0..clips.len()).filter(|&i| clips[i].tc.is_some() && clips[i].ctime.is_some()).collect();
    let d_of = |c: &Media, end: bool| {
        let tc = tc_label_seconds(c.tc.as_ref().unwrap(), c.fps.unwrap());
        let ct = ctime_seconds(c.ctime.as_ref().unwrap()) - if end { c.dur } else { 0.0 };
        wrap_day(tc - ct)
    };
    let ds: Vec<f64> = idx.iter().map(|&i| d_of(&clips[i], false)).collect();
    let de: Vec<f64> = idx.iter().map(|&i| d_of(&clips[i], true)).collect();
    let (cs, ms) = densest(&ds, TC_TOL);
    let (ce, me) = densest(&de, TC_TOL);
    let ctime_end = ce > cs;
    let (d, center) = if ctime_end { (&de, me) } else { (&ds, ms) };
    let n = idx.len();
    let member: Vec<bool> = if n <= 2 { vec![true; n] } else { d.iter().map(|x| (x - center).abs() <= TC_TOL).collect() };
    let cnt = member.iter().filter(|&&b| b).count();
    let valid = n > 0 && (n <= 2 || (cnt >= 3 && cnt * 4 >= n));
    let mut info = TcInfo { ctime_end, n_tc: n, ..Default::default() };
    if valid {
        let mv: Vec<f64> = d.iter().zip(&member).filter(|p| *p.1).map(|p| *p.0).collect();
        info.dc = Some(median(&mv));
    }
    for c in clips.iter_mut() {
        c.tc_state = if c.tc.is_none() { "none" } else if c.ctime.is_none() { "unverified" } else { "ok" }.into();
    }
    for (k, &i) in idx.iter().enumerate() {
        if !(valid && member[k]) {
            clips[i].tc_state = "bad".into();
        }
    }
    info.bad = clips.iter().filter(|c| c.tc_state == "bad").map(|c| c.file.clone()).collect();
    info.none = clips.iter().filter(|c| c.tc_state == "none").map(|c| c.file.clone()).collect();
    info
}

fn report_timecodes(cam: &str, total: usize, info: &TcInfo) {
    if !info.bad.is_empty() {
        if info.bad.len() == info.n_tc {
            println!("{}", tr!(
                "  {}: у ВСЕХ клипов неправильный таймкод (похоже на Rec Run или сброс) — используется время создания файлов. На следующей съёмке включите Free Run.",
                "  {}: ALL clips have an invalid timecode (looks like Rec Run or a reset) — file creation times are used instead. Set the camera to Free Run next time.",
                cam));
        } else {
            println!("{}", tr!(
                "  {}: клипы с неправильным таймкодом ({} из {}) — для них используется время создания файла: {}",
                "  {}: clips with an invalid timecode ({} of {}) — their file creation time is used instead: {}",
                cam, info.bad.len(), info.n_tc, info.bad.join(", ")));
        }
    }
    if !info.none.is_empty() {
        if info.none.len() == total {
            println!("{}", tr!("  {}: у клипов нет таймкода — используется время создания файлов",
                               "  {}: clips have no timecode — file creation times are used", cam));
        } else {
            println!("{}", tr!("  {}: клипы без таймкода — используется время создания файла: {}",
                               "  {}: clips without timecode — file creation time is used: {}", cam, info.none.join(", ")));
        }
    }
}

fn sync_camera(m: &Master, cam: &str, clips: &mut [Media], info: &TcInfo) {
    // camera clock: valid timecode as is; other clips: file creation time on the same scale
    for c in clips.iter_mut() {
        c.t_alt = None;
        c.t_cam = if c.tc_state == "ok" || c.tc_state == "unverified" {
            Some(tc_label_seconds(c.tc.as_ref().unwrap(), c.fps.unwrap()))
        } else if let Some(ct) = &c.ctime {
            let cs = ctime_seconds(ct);
            match info.dc {
                Some(dc) => Some(cs - if info.ctime_end { c.dur } else { 0.0 } + dc),
                None => {
                    c.t_alt = Some(cs - c.dur); // start vs end of recording decided by the audio below
                    Some(cs)
                }
            }
        } else {
            None
        };
    }
    // global search (parallel over clips)
    clips.par_iter_mut().for_each(|c| {
        c.g_pos = None;
        c.g_z = 0.0;
        if c.has_audio && c.t_cam.is_some() {
            if let Some((p, z)) = m.global_search(&flux(&load_f32(&c.cache))) {
                c.g_pos = Some(p);
                c.g_z = z;
            }
        }
    });
    // anchors: strong global matches with a consistent camera offset
    let mut cand: Vec<&Media> = clips.iter().filter(|c| c.g_pos.is_some() && c.g_z >= 20.0 && c.dur >= 15.0).collect();
    if cand.len() < 3 {
        cand = clips.iter().filter(|c| c.g_pos.is_some() && c.g_z >= 12.0 && c.dur >= 8.0).collect();
    }
    if cand.is_empty() {
        println!("{}", tr!("  {}: не найдено ни одного надёжного совпадения со звуком рекордера — камера пропущена",
                            "  {}: no reliable match with the recorder audio — camera skipped", cam));
        for c in clips.iter_mut() {
            c.status = "unplaced".into();
        }
        return;
    }
    // no valid timecode at all: does the file time mark the start or the end of the recording?
    let n_alt = cand.iter().filter(|c| c.t_alt.is_some()).count();
    if n_alt >= 3 {
        let mad = |o: Vec<f64>| {
            let md = median(&o);
            median(&o.iter().map(|v| (v - md).abs()).collect::<Vec<_>>())
        };
        let a = mad(cand.iter().map(|c| c.g_pos.unwrap() - c.t_cam.unwrap()).collect());
        let b = mad(cand.iter().map(|c| c.g_pos.unwrap() - c.t_alt.unwrap_or(c.t_cam.unwrap())).collect());
        if b < a {
            for c in clips.iter_mut() {
                if let Some(t) = c.t_alt {
                    c.t_cam = Some(t);
                }
            }
            cand = clips.iter().filter(|c| c.g_pos.is_some() && c.g_z >= 20.0 && c.dur >= 15.0).collect();
            if cand.len() < 3 {
                cand = clips.iter().filter(|c| c.g_pos.is_some() && c.g_z >= 12.0 && c.dur >= 8.0).collect();
            }
        }
    }
    let offs: Vec<f64> = cand.iter().map(|c| c.g_pos.unwrap() - c.t_cam.unwrap()).collect();
    let med = median(&offs);
    let anchors: Vec<(f64, f64)> = cand
        .iter()
        .zip(&offs)
        .filter(|(_, o)| (*o - med).abs() < 3.0)
        .map(|(c, o)| (c.t_cam.unwrap(), *o))
        .collect();
    let at: Vec<f64> = anchors.iter().map(|a| a.0).collect();
    let ao: Vec<f64> = anchors.iter().map(|a| a.1).collect();
    println!("{}", tr!("  {}: смещение часов камеры ≈ {:+.1} с, опорных клипов {}",
                        "  {}: camera clock offset ≈ {:+.1} s, anchor clips {}", cam, med, anchors.len()));

    // precise placement (parallel over clips)
    clips.par_iter_mut().for_each(|c| {
        let Some(tc) = c.t_cam else {
            c.status = "unplaced".into();
            return;
        };
        let j = nearest(&at, tc, 3);
        let pred = tc + median(&j.iter().map(|&i| ao[i]).collect::<Vec<_>>());
        if !c.has_audio {
            c.status = "no-camera-audio".into();
            return;
        }
        if m.covered(pred, pred + c.dur) < 0.3 {
            c.status = "outside-recorder".into();
            return;
        }
        let x = load_f32(&c.cache);
        let Some((p0, r, r2, edge)) = locate(m, &x[..x.len().min(60 * SR)], pred, 4.0) else {
            c.status = "not-found".into();
            return;
        };
        c.r = r;
        c.r2 = r2;
        c.pos = Some(p0);
        if x.len() > 45 * SR {
            // long clip: 30 s chunks, line fit
            let (mut ts, mut os) = (Vec::new(), Vec::new());
            let mut s = 0usize;
            while (x.len() / SR) >= 30 && s <= x.len() / SR - 30 {
                if let Some(q) = locate(m, &x[s * SR..(s + 30) * SR], p0 + s as f64, 0.3) {
                    if q.1 > 0.15 && q.1 > 1.5 * q.2 && !q.3 {
                        ts.push(s as f64);
                        os.push(q.0 - s as f64);
                    }
                }
                s += 30;
            }
            if ts.len() >= 3 {
                let mut ok = vec![true; ts.len()];
                let (mut a, mut b) = (0.0, 0.0);
                let mut res = vec![0.0; ts.len()];
                for _ in 0..4 {
                    let xs: Vec<f64> = ts.iter().zip(&ok).filter(|p| *p.1).map(|p| *p.0).collect();
                    let ys: Vec<f64> = os.iter().zip(&ok).filter(|p| *p.1).map(|p| *p.0).collect();
                    if xs.len() < 2 {
                        break;
                    }
                    (b, a) = polyfit1(&xs, &ys);
                    res = ts.iter().zip(&os).map(|(t, o)| o - (a + b * t)).collect();
                    let absok: Vec<f64> = res.iter().zip(&ok).filter(|p| *p.1).map(|p| p.0.abs()).collect();
                    let thr = 0.008f64.max(3.0 * 1.4826 * median(&absok));
                    ok = res.iter().map(|v| v.abs() < thr).collect();
                }
                c.pos = Some(a);
                if ok.iter().filter(|v| **v).count() >= 5 {
                    c.drift_ms = Some(b * c.dur * 1000.0);
                    let mx = res.iter().fold(0f64, |acc, v| acc.max(v.abs()));
                    if mx > 0.04 {
                        c.note = tr!("скачок звука внутри клипа ~{:.0} мс", "audio jump inside the clip ~{:.0} ms", mx * 1000.0);
                    }
                }
            }
        }
        let strong = r >= 0.3 && r >= 1.8 * r2 && !edge;
        c.status = if strong { "ok" } else { "check" }.into();
    });

    // neighbour band check + model placement
    let ok: Vec<(f64, f64)> = clips
        .iter()
        .filter(|c| c.status == "ok")
        .map(|c| (c.t_cam.unwrap(), c.pos.unwrap() - c.t_cam.unwrap()))
        .collect();
    let refs = if ok.is_empty() { anchors } else { ok };
    let rt: Vec<f64> = refs.iter().map(|a| a.0).collect();
    let ro: Vec<f64> = refs.iter().map(|a| a.1).collect();
    for c in clips.iter_mut() {
        if c.status == "unplaced" {
            continue;
        }
        let tc = c.t_cam.unwrap();
        let band = median(&nearest(&rt, tc, 4).iter().map(|&i| ro[i]).collect::<Vec<_>>());
        if c.status == "check" && (c.pos.unwrap() - tc - band).abs() > 0.9 {
            c.status = "model".into();
        }
        c.final_t = Some(if c.status == "ok" || c.status == "check" { c.pos.unwrap() } else { tc + band });
    }
}

const CAM_W: f64 = 3.0; // search window for camera-to-camera alignment, s

/// clips without recorder audio: align by camera audio to overlapping clips of other cameras.
/// Partners: clips placed by the recorder (or already aligned) first; then, where none exist,
/// the first camera's own clips at their timecode positions (anchor camera).
fn cross_camera(cams: &[String], per_cam: &mut [Vec<Media>]) {
    let idx: Vec<(usize, usize)> = per_cam.iter().enumerate().flat_map(|(i, v)| (0..v.len()).map(move |j| (i, j))).collect();
    let mut trusted: Vec<Vec<bool>> =
        per_cam.iter().map(|v| v.iter().map(|c| c.status == "ok" || c.status == "check").collect()).collect();
    for allow_ref in [false, true] {
        let mut changed = true;
        while changed {
            changed = false;
            for &(ui, uj) in &idx {
                let u = &per_cam[ui][uj];
                if !matches!(u.status.as_str(), "outside-recorder" | "not-found" | "model") || !u.has_audio || u.final_t.is_none() {
                    continue;
                }
                let (uf, ud) = (u.final_t.unwrap(), u.dur);
                // ((tier, -overlap), camera index, clip index, s0, s1) of the best partner so far
                type Partner = ((u8, f64), usize, usize, f64, f64);
                let mut best: Option<Partner> = None;
                for &(pi, pj) in &idx {
                    let p = &per_cam[pi][pj];
                    if pi == ui || p.final_t.is_none() || !p.has_audio {
                        continue;
                    }
                    let tier = if trusted[pi][pj] {
                        0u8
                    } else if allow_ref && p.cam == cams[0] {
                        1
                    } else {
                        continue;
                    };
                    let pf = p.final_t.unwrap();
                    // part of u that stays inside p for every lag in +-CAM_W
                    let s0 = (pf - uf + CAM_W).max(0.0);
                    let s1 = ud.min(pf + p.dur - uf - CAM_W).min(s0 + 60.0);
                    if s1 - s0 < 3.0 {
                        continue;
                    }
                    let key = (tier, -(s1 - s0));
                    if best.as_ref().is_none_or(|b| key < b.0) {
                        best = Some((key, pi, pj, s0, s1));
                    }
                }
                let Some((_, pi, pj, s0, s1)) = best else { continue };
                let p = &per_cam[pi][pj];
                let pf = p.final_t.unwrap();
                let pfile = p.file.clone();
                let src = ClipWave(load_f32(&p.cache));
                let x = load_f32(&per_cam[ui][uj].cache);
                let a = ((s0 * SR as f64) as usize).min(x.len());
                let b = ((s1 * SR as f64) as usize).min(x.len());
                if let Some(q) = locate(&src, &x[a..b], uf + s0 - pf, CAM_W) {
                    if q.1 >= 0.1 && q.1 >= 2.0 * q.2 && !q.3 {
                        let u = &mut per_cam[ui][uj];
                        u.final_t = Some(pf + q.0 - s0);
                        u.status = "camsync".into();
                        u.r = q.1;
                        u.r2 = q.2;
                        u.note = tr!("по звуку камеры: {}", "aligned to camera audio: {}", pfile);
                        trusted[ui][uj] = true;
                        changed = true;
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------- xmeml

fn note_for(status: &str) -> String {
    match status {
        "camsync" => tr!("нет звука рекордера — выровнено по звуку другой камеры", "no recorder audio — aligned to another camera's audio"),
        "check" => tr!("ПРОВЕРИТЬ: слабая корреляция", "CHECK: weak correlation"),
        "outside-recorder" => tr!("по таймкоду (нет звука рекордера), ±0.5 с", "by timecode (no recorder audio), ±0.5 s"),
        "no-camera-audio" => tr!("по таймкоду (нет звука в клипе), ±0.5 с", "by timecode (clip has no audio), ±0.5 s"),
        "not-found" => tr!("по таймкоду (звук не найден), ±0.5 с", "by timecode (audio not found), ±0.5 s"),
        "model" => tr!("по таймкоду (звук не совпал с соседями), ±0.5 с", "by timecode (audio disagreed with neighbours), ±0.5 s"),
        _ => String::new(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// file URL as FCP7 XML expects it: file://localhost/D:/x.mov (Windows), file://localhost/Users/x.mov (macOS/Linux)
fn url(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/").replace(' ', "%20");
    format!("file://localhost/{}", s.trim_start_matches('/'))
}

fn write_xmeml(path: &Path, name: &str, m: &mut Master, cams: &[String], clips: &mut [Vec<Media>], fps: f64, label_offset: f64) {
    let tb = fps.round() as i64;
    let ntsc = (fps - tb as f64).abs() > 0.01;
    let rate = format!("<rate><timebase>{tb}</timebase><ntsc>{}</ntsc></rate>", if ntsc { "TRUE" } else { "FALSE" });

    // Resolve links a WAV only if XML extents lie inside the file:
    // start = first whole frame at/after file start, end = last whole frame before file end
    for t in m.takes.iter_mut() {
        let tref = t.tref as f64 / t.sr as f64;
        t.src0 = (tref * fps - 1e-9).ceil() as i64;
        t.src_end = ((tref + t.len) * fps + 1e-9).floor() as i64;
        t.eff = t.start + (t.src0 as f64 / fps - tref);
    }
    // labels read like the first camera's clock at recorder start, then count real frames
    let f0 = ((m.takes[0].eff + label_offset) * tb as f64).round() as i64;
    let eff0 = m.takes[0].eff;
    for t in m.takes.iter_mut() {
        t.frame = f0 + ((t.eff - eff0) * fps).round() as i64;
    }
    let takes = &m.takes;
    let place = |tt: f64| -> i64 {
        let key = |t: &Take| {
            if t.start <= tt && tt <= t.start + t.len { 0.0 } else { (tt - t.start).abs().min((tt - t.start - t.len).abs()) }
        };
        let mut best = &takes[0];
        for t in takes.iter() {
            if key(t) < key(best) {
                best = t;
            }
        }
        best.frame + ((tt - best.eff) * fps).round() as i64
    };

    let mut lo = i64::MAX;
    let mut hi = i64::MIN;
    for c in clips.iter_mut().flatten() {
        if let Some(f) = c.final_t {
            let tl = place(f);
            c.tl = Some(tl);
            c.nf = (c.dur * fps) as i64;
            c.src0 = c.tc.as_ref().map(|tc| tc_to_frames(tc, c.fps.unwrap())).unwrap_or(0);
            c.new_tc = frames_to_tc(tl, tb);
            lo = lo.min(tl);
            hi = hi.max(tl + c.nf);
        }
    }
    for t in takes {
        lo = lo.min(t.frame);
        hi = hi.max(t.frame + t.src_end - t.src0);
    }
    let seq0 = lo.div_euclid(tb * 60) * (tb * 60);
    let seq_end = hi;

    let mut done = std::collections::HashSet::new();
    let mut file_xml = |fid: &str, fname: &str, fpath: &Path, dur: i64, src0: i64, video: bool, tcfmt: &str, depth: u32| -> String {
        if !done.insert(fid.to_string()) {
            return format!("<file id=\"{}\"/>", esc(fid));
        }
        let v = if video {
            format!("<video><samplecharacteristics>{rate}<width>1920</width><height>1080</height></samplecharacteristics></video>")
        } else {
            String::new()
        };
        format!(
            "<file id=\"{}\"><name>{}</name><pathurl>{}</pathurl>{rate}<duration>{dur}</duration><timecode>{rate}<string>{}</string>\
             <frame>{src0}</frame><displayformat>{tcfmt}</displayformat></timecode><media>{v}<audio>\
             <samplecharacteristics><depth>{depth}</depth><samplerate>48000</samplerate></samplecharacteristics>\
             <channelcount>2</channelcount></audio></media></file>",
            esc(fid), esc(fname), esc(&url(fpath)), frames_to_tc(src0, tb)
        )
    };
    let clipitem = |cid: &str, nm: &str, start: i64, nf: i64, fx: &str, audio: bool, extra: &str| -> String {
        let st = if audio { "<sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack>" } else { "" };
        format!(
            "<clipitem id=\"{}\"><name>{}</name><enabled>TRUE</enabled><duration>{nf}</duration>{rate}<start>{start}</start>\
             <end>{}</end><in>0</in><out>{nf}</out>{fx}{st}{extra}</clipitem>",
            esc(cid), esc(nm), start + nf
        )
    };

    let mut vtr: Vec<Vec<String>> = Vec::new();
    let mut atr: Vec<Vec<String>> = Vec::new();
    for (ti0, cam) in cams.iter().enumerate() {
        let ti = ti0 + 1;
        let mut cc: Vec<&Media> = clips[ti0].iter().filter(|c| c.tl.is_some()).collect();
        cc.sort_by_key(|c| c.tl.unwrap());
        let (mut vv, mut aa) = (Vec::new(), Vec::new());
        for (i, c) in cc.iter().enumerate() {
            let fid = format!("f-{cam}-{}", c.file);
            let mut note = note_for(&c.status);
            if !c.note.is_empty() {
                note = if note.is_empty() { c.note.clone() } else { format!("{note}; {}", c.note) };
            }
            let mk = if note.is_empty() {
                String::new()
            } else {
                format!("<marker><name>{0}</name><comment>{0}</comment><in>0</in><out>-1</out></marker>", esc(&note))
            };
            let lk: String = [("v", "video"), ("a", "audio")]
                .iter()
                .map(|(k, mt)| {
                    format!(
                        "<link><linkclipref>{}</linkclipref><mediatype>{mt}</mediatype><trackindex>{ti}</trackindex><clipindex>{}</clipindex></link>",
                        esc(&format!("{k}-{cam}-{}", c.file)), i + 1
                    )
                })
                .collect();
            let tcfmt = if c.tc.as_deref().is_some_and(|t| t.contains(';')) { "DF" } else { "NDF" };
            let fx = file_xml(&fid, &c.file, &c.path, (c.dur * fps) as i64, c.src0, true, tcfmt, 16);
            let start = c.tl.unwrap() - seq0;
            vv.push(clipitem(&format!("v-{cam}-{}", c.file), &c.file, start, c.nf, &fx, false, &format!("{mk}{lk}")));
            if c.has_audio {
                aa.push(clipitem(&format!("a-{cam}-{}", c.file), &c.file, start, c.nf, &format!("<file id=\"{}\"/>", esc(&fid)), true, &lk));
            }
        }
        vtr.push(vv);
        atr.push(aa);
    }
    for var in &m.variants {
        let mut items = Vec::new();
        for (i, t) in takes.iter().enumerate() {
            let Some(w) = t.files.iter().find(|w| &variant(&w.file) == var) else { continue };
            let mut nf = t.src_end - t.src0;
            if i + 1 < takes.len() {
                nf = nf.min(takes[i + 1].frame - t.frame);
            }
            let fx = file_xml(&format!("f-rec-{}", w.file), &w.file, &w.path, t.src_end - t.src0, t.src0, false, "NDF", 24);
            items.push(clipitem(&format!("a-rec-{}", w.file), &w.file, t.frame - seq0, nf, &fx, true, ""));
        }
        atr.push(items);
    }
    let track = |it: &Vec<String>| format!("<track>{}<enabled>TRUE</enabled><locked>FALSE</locked></track>", it.concat());
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n<xmeml version=\"4\"><sequence id=\"seq-1\"><name>{}</name>\
         <duration>{}</duration>{rate}<timecode>{rate}<string>{}</string><frame>{seq0}</frame><displayformat>NDF</displayformat>\
         </timecode><media><video><format><samplecharacteristics>{rate}<width>1920</width><height>1080</height>\
         <pixelaspectratio>square</pixelaspectratio></samplecharacteristics></format>{}</video><audio>\
         <numOutputChannels>2</numOutputChannels>{}</audio></media></sequence></xmeml>\n",
        esc(name), seq_end - seq0, frames_to_tc(seq0, tb),
        vtr.iter().map(track).collect::<String>(),
        atr.iter().map(track).collect::<String>()
    );
    fs::write(path, xml).expect("write xml");
}

// ------------------------------------------------------------------------- cli

struct Args {
    project: PathBuf,
    cams: Vec<String>,
    audio: String,
    ref_suffix: String,
    fps: Option<f64>,
    out: Option<PathBuf>,
    exclude: Vec<String>,
    jobs: usize,
    threads: usize,
}

const USAGE_RU: &str = "slatefree — синхронизация камер по записи рекордера → таймлайн для DaVinci Resolve

Использование:
  slatefree.exe <папка съёмки> [ключи]
  (или перетащить папку съёмки на slatefree.exe)

Ключи:
  --cams ACAM BCAM     папки камер (по умолчанию — все папки с видео, кроме --audio)
  --audio AUDIO        папка с WAV рекордера (по умолчанию AUDIO)
  --exclude МАСКА ...  не трогать эти файлы (напр. \"summ*.wav\")
  --ref-suffix I       по какому варианту каналов искать (H4n 4CH: I = встроенные микрофоны)
  --fps 25             частота таймлайна (по умолчанию — самая частая у клипов)
  --out ПАПКА          куда писать результат (по умолчанию <съёмка>\\SYNC)
  --jobs N             одновременных ffmpeg при извлечении звука (по умолчанию 6)
  --threads N          потоков для вычислений (по умолчанию — все ядра)
  --lang ru|en         язык сообщений (по умолчанию — язык Windows)";

const USAGE_EN: &str = "slatefree — sync cameras to a field-recorder track → DaVinci Resolve timeline

Usage:
  slatefree.exe <shoot folder> [options]
  (or drag the shoot folder onto slatefree.exe)

Options:
  --cams ACAM BCAM     camera folders (default: every folder with video except --audio)
  --audio AUDIO        folder with the recorder WAV files (default AUDIO)
  --exclude MASK ...   ignore these files (e.g. \"mix*.wav\")
  --ref-suffix I       which recorder channel set to analyse (H4n 4CH: I = built-in mics)
  --fps 25             timeline frame rate (default: the most common clip rate)
  --out FOLDER         output folder (default <shoot>\\SYNC)
  --jobs N             parallel ffmpeg processes for audio extraction (default 6)
  --threads N          compute threads (default: all cores)
  --lang ru|en         message language (default: Windows UI language)";

fn parse_args() -> Option<Args> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "-h" || a == "--help") {
        return None;
    }
    let mut a = Args {
        project: PathBuf::new(), cams: vec![], audio: "AUDIO".into(), ref_suffix: "I".into(), fps: None,
        out: None, exclude: vec![], jobs: 6, threads: 0,
    };
    let mut i = 0;
    let mut positional = None;
    let take_list = |i: &mut usize| -> Vec<String> {
        let mut v = vec![];
        while *i + 1 < raw.len() && !raw[*i + 1].starts_with("--") {
            *i += 1;
            v.push(raw[*i].clone());
        }
        v
    };
    while i < raw.len() {
        let k = raw[i].as_str();
        match k {
            "--cams" => a.cams = take_list(&mut i),
            "--exclude" => a.exclude = take_list(&mut i),
            "--audio" | "--ref-suffix" | "--fps" | "--out" | "--jobs" | "--threads" | "--lang" => {
                i += 1;
                let v = raw.get(i).cloned().unwrap_or_default();
                match k {
                    "--audio" => a.audio = v,
                    "--ref-suffix" => a.ref_suffix = v,
                    "--fps" => a.fps = v.parse().ok(),
                    "--out" => a.out = Some(PathBuf::from(v)),
                    "--jobs" => a.jobs = v.parse().unwrap_or(6),
                    "--lang" => {
                        let _ = LANG_RU.set(v.to_lowercase().starts_with("ru"));
                    }
                    _ => a.threads = v.parse().unwrap_or(0),
                }
            }
            _ => positional = Some(PathBuf::from(k)),
        }
        i += 1;
    }
    a.project = positional?;
    Some(a)
}

fn wildcard(pat: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pat.to_lowercase().chars().collect(), s.to_lowercase().chars().collect());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// pause before exit when the console window belongs only to us (double-click / drag & drop)
fn pause_if_own_console() {
    #[cfg(windows)]
    {
        use std::io::{self, BufRead, Write};
        let mut buf = [0u32; 4];
        let n = unsafe { windows_sys::Win32::System::Console::GetConsoleProcessList(buf.as_mut_ptr(), 4) };
        if n == 1 {
            print!("\n{}", tr!("Нажмите Enter, чтобы закрыть окно…", "Press Enter to close this window…"));
            let _ = io::stdout().flush();
            let _ = io::stdin().lock().lines().next();
        }
    }
}

/// one field of the ';'-separated report, quoted like Python's csv module when needed
fn csv_field(v: &str) -> String {
    if v.contains([';', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

fn sorted_dir(p: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(p)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn has_ext(f: &str, exts: &[&str]) -> bool {
    Path::new(f).extension().map(|e| exts.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false)
}

fn main() {
    let code = run();
    pause_if_own_console();
    std::process::exit(code);
}

fn run() -> i32 {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = raw.iter().position(|x| x == "--lang") {
        let _ = LANG_RU.set(raw.get(i + 1).is_some_and(|v| v.to_lowercase().starts_with("ru")));
    }
    let Some(a) = parse_args() else {
        println!("{}", if ru() { USAGE_RU } else { USAGE_EN });
        return if raw.iter().any(|x| x == "-h" || x == "--help") { 0 } else { 1 };
    };
    if a.threads > 0 {
        rayon::ThreadPoolBuilder::new().num_threads(a.threads).build_global().ok();
    }
    let proj = fs::canonicalize(&a.project).unwrap_or(a.project.clone());
    let proj = PathBuf::from(proj.to_string_lossy().trim_start_matches(r"\\?\").to_string());
    if !proj.is_dir() {
        eprintln!("{}", tr!("Папка не найдена: {}", "Folder not found: {}", proj.display()));
        return 1;
    }
    let out = a.out.clone().unwrap_or_else(|| proj.join("SYNC"));
    let cache = out.join(".cache");
    fs::create_dir_all(&cache).expect("create output dir");
    let adir = proj.join(&a.audio);
    let cams: Vec<String> = if a.cams.is_empty() {
        sorted_dir(&proj)
            .into_iter()
            .filter(|d| proj.join(d).is_dir() && d != &a.audio && sorted_dir(&proj.join(d)).iter().any(|f| has_ext(f, &VIDEO_EXT)))
            .collect()
    } else {
        a.cams.clone()
    };
    println!("{}", tr!("Камеры: {:?}; рекордер: {}; потоков: {}", "Cameras: {:?}; recorder: {}; threads: {}",
                        cams, a.audio, rayon::current_num_threads()));

    let excl = |f: &str| a.exclude.iter().any(|p| wildcard(p, f));
    let mut jobs: Vec<(Option<String>, PathBuf)> = sorted_dir(&adir)
        .into_iter()
        .filter(|f| has_ext(f, &["wav"]) && !excl(f))
        .map(|f| (None, adir.join(f)))
        .collect();
    for cam in &cams {
        for f in sorted_dir(&proj.join(cam)) {
            if has_ext(&f, &VIDEO_EXT) && !excl(&f) {
                jobs.push((Some(cam.clone()), proj.join(cam).join(f)));
            }
        }
    }
    let io_pool = rayon::ThreadPoolBuilder::new().num_threads(a.jobs.max(1)).build().unwrap();
    let mut tm = Instant::now();
    let mut lap = |what: String| {
        println!("   [{}: {:.1} {}]", what, tm.elapsed().as_secs_f64(), if ru() { "с" } else { "s" });
        tm = Instant::now();
    };

    println!("{}", tr!("Чтение метаданных ({} файлов)…", "Reading metadata ({} files)…", jobs.len()));
    let metas: Vec<Media> = io_pool.install(|| jobs.par_iter().map(|j| probe(&j.1)).collect());
    let mut wavs = Vec::new();
    let mut ignored = Vec::new();
    let mut all_clips = Vec::new();
    for ((cam, _), mut md) in jobs.into_iter().zip(metas) {
        match cam {
            None => {
                let enc = md.encoder.to_lowercase();
                if md.tref.is_none() || EDITORS.iter().any(|e| enc.contains(e)) {
                    ignored.push(md.file);
                } else {
                    wavs.push(md);
                }
            }
            Some(c) => {
                md.cam = c;
                all_clips.push(md);
            }
        }
    }
    if !ignored.is_empty() {
        println!("{}", tr!("WAV не с рекордера (пропущены): {}", "WAV not from a recorder (skipped): {}", ignored.join(", ")));
    }
    lap(tr!("метаданные", "metadata"));

    // timeline fps = most frequent clip fps (first seen wins ties)
    let fps = a.fps.map(snap_fps).unwrap_or_else(|| {
        let mut cnt: Vec<(f64, usize)> = Vec::new();
        for c in all_clips.iter().filter_map(|c| c.fps) {
            match cnt.iter_mut().find(|x| x.0 == c) {
                Some(x) => x.1 += 1,
                None => cnt.push((c, 1)),
            }
        }
        let best = cnt.iter().map(|x| x.1).max().unwrap_or(0);
        cnt.iter().find(|x| x.1 == best).map(|x| x.0).unwrap_or(25.0)
    });
    let (clips_ok, skipped): (Vec<Media>, Vec<Media>) =
        all_clips.into_iter().partition(|c| c.fps.is_some_and(|f| (f - fps).abs() <= 0.01));
    if !skipped.is_empty() {
        let mut names: Vec<&str> = skipped.iter().map(|c| c.file.as_str()).collect();
        names.sort();
        let more = if names.len() > 8 { " …" } else { "" };
        names.truncate(8);
        println!(
            "{}",
            tr!("Пропущено {} клипов с другой частотой кадров (таймлайн {:.3}): {}{}",
                "Skipped {} clips with a different frame rate (timeline {:.3}): {}{}",
                skipped.len(), fps, names.join(", "), more)
        );
    }

    let mut per_cam: Vec<Vec<Media>> = cams.iter().map(|_| Vec::new()).collect();
    for c in clips_ok {
        let i = cams.iter().position(|x| x == &c.cam).unwrap();
        per_cam[i].push(c);
    }
    println!("{}", tr!("Проверка таймкода…", "Checking timecode…"));
    let tc_infos: Vec<TcInfo> = per_cam.iter_mut().map(|cc| check_timecodes(cc)).collect();
    let mut tc_problems = false;
    for ((cam, cc), info) in cams.iter().zip(&per_cam).zip(&tc_infos) {
        tc_problems |= !info.bad.is_empty() || !info.none.is_empty();
        report_timecodes(cam, cc.len(), info);
    }
    if !tc_problems {
        println!("{}", tr!("  таймкод у всех клипов в порядке", "  all clips have a valid timecode"));
    }

    let takes = Master::group(wavs, &a.ref_suffix);
    let mut need: Vec<PathBuf> = takes.iter().map(|t| t.files[t.ref_i].path.clone()).collect();
    need.extend(per_cam.iter().flatten().filter(|c| c.has_audio).map(|c| c.path.clone()));
    println!("{}", tr!("Извлечение звука ({} файлов, кэш в {})…", "Extracting audio ({} files, cache in {})…", need.len(), cache.display()));
    io_pool.install(|| need.par_iter().for_each(|p| extract(p, &cache)));
    for c in per_cam.iter_mut().flatten() {
        c.cache = cache_path(&cache, &c.file);
        if c.has_audio && fs::metadata(&c.cache).map(|m| m.len()).unwrap_or(0) < (SR * 4) as u64 {
            c.has_audio = false;
        }
    }
    lap(tr!("извлечение звука", "audio extraction"));

    let max_clip_frames = per_cam.iter().flatten().map(|c| (c.dur * ESR as f64) as usize + ESR).max().unwrap_or(0);
    let mut m = Master::new(takes, &cache, max_clip_frames);
    lap(tr!("признаки рекордера", "recorder features"));

    println!("{}", tr!("Синхронизация…", "Syncing…"));
    for ((cam, cc), info) in cams.iter().zip(per_cam.iter_mut()).zip(&tc_infos) {
        sync_camera(&m, cam, cc, info);
        lap(tr!("синхронизация {}", "sync {}", cam));
    }
    cross_camera(&cams, &mut per_cam);
    lap(tr!("камера ↔ камера", "camera ↔ camera"));

    // timeline labels: recorder clock shifted to the first camera's clock
    let first: Vec<f64> = per_cam
        .first()
        .map(|cc| cc.iter().filter(|c| c.status == "ok").map(|c| c.t_cam.unwrap() - c.final_t.unwrap()).collect())
        .unwrap_or_default();
    let label_offset = if first.is_empty() { 0.0 } else { median(&first) };

    let pname = proj.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let name = format!("{pname}_sync");
    let xml_path = out.join(format!("{name}.xml"));
    write_xmeml(&xml_path, &name, &mut m, &cams, &mut per_cam, fps, label_offset);

    // csv
    let mut rows: Vec<&Media> = per_cam.iter().flatten().collect();
    rows.sort_by_key(|c| c.tl.unwrap_or(i64::MAX));
    let mut csv = String::from("\u{feff}");
    let mut row = |f: [String; 12]| {
        csv += &f.iter().map(|v| csv_field(v)).collect::<Vec<_>>().join(";");
        csv += "\r\n";
    };
    row([
        "camera", "file", "fps", "duration_s", "orig_tc", "tc_check", "timeline_tc", "status", "r", "r_second",
        "intra_drift_ms", "note",
    ]
    .map(String::from));
    for c in rows {
        row([
            c.cam.clone(), c.file.clone(), format!("{:.3}", c.fps.unwrap_or(0.0)), format!("{:.2}", c.dur),
            c.tc.clone().unwrap_or_default(), c.tc_state.clone(), c.new_tc.clone(), c.status.clone(),
            format!("{:.2}", c.r), format!("{:.2}", c.r2),
            c.drift_ms.map(|d| format!("{:.0}", d)).unwrap_or_default(), c.note.clone(),
        ]);
    }
    for c in &skipped {
        row([
            c.cam.clone(), c.file.clone(), format!("{:.3}", c.fps.unwrap_or(0.0)), format!("{:.2}", c.dur),
            c.tc.clone().unwrap_or_default(), String::new(), String::new(), "skipped-fps".into(),
            String::new(), String::new(), String::new(), String::new(),
        ]);
    }
    fs::write(out.join("sync_result.csv"), csv).expect("write csv");

    println!("\n{}", tr!("Итог:", "Summary:"));
    for (cam, cc) in cams.iter().zip(&per_cam) {
        let mut st: Vec<(String, usize)> = Vec::new();
        for c in cc {
            match st.iter_mut().find(|x| x.0 == c.status) {
                Some(x) => x.1 += 1,
                None => st.push((c.status.clone(), 1)),
            }
        }
        st.sort_by_key(|a| std::cmp::Reverse(a.1));
        println!("  {cam}: {}", st.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", "));
    }
    let chk: Vec<&str> = per_cam.iter().flatten().filter(|c| c.status == "check").map(|c| c.file.as_str()).collect();
    if !chk.is_empty() {
        println!("{}", tr!("  проверить вручную: {}", "  check manually: {}", chk.join(", ")));
    }
    println!(
        "\n{}",
        tr!("Таймлайн: {}\nResolve: File > Import > Timeline (частота проекта {:.3})",
            "Timeline: {}\nResolve: File > Import > Timeline (project frame rate {:.3})",
            xml_path.display(), fps)
    );
    0
}

// ------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_ndf_roundtrip() {
        assert_eq!(tc_to_frames("01:46:19:14", 24000.0 / 1001.0), 153110);
        assert_eq!(frames_to_tc(153110, 24), "01:46:19:14");
        assert_eq!(tc_to_frames("00:00:01:00", 25.0), 25);
    }

    #[test]
    fn timecode_drop_frame() {
        // 29.97 DF: 00:01:00;02 is frame 1800 (frames 0 and 1 of minute 1 are dropped)
        assert_eq!(tc_to_frames("00:01:00;02", 30000.0 / 1001.0), 1800);
        assert_eq!(tc_to_frames("00:10:00;00", 30000.0 / 1001.0), 17982);
    }

    #[test]
    fn fps_snapping() {
        assert_eq!(snap_fps(23.98), 24000.0 / 1001.0);
        assert_eq!(snap_fps(29.97), 30000.0 / 1001.0);
        assert_eq!(snap_fps(25.0), 25.0);
        assert_eq!(snap_fps(12.5), 12.5);
    }

    #[test]
    fn recorder_channel_sets() {
        assert_eq!(variant("1_4CH001I.wav"), "I");
        assert_eq!(variant("4CH002M.WAV"), "M");
        assert_eq!(variant("STE-001.wav"), "");
        assert_eq!(variant("ZOOM0001.WAV"), "");
    }

    #[test]
    fn file_urls_for_both_platforms() {
        assert_eq!(url(Path::new(r"D:\VIDEO\a b\x.MOV")), "file://localhost/D:/VIDEO/a%20b/x.MOV");
        assert_eq!(url(Path::new("/Users/me/Shoot/x.MOV")), "file://localhost/Users/me/Shoot/x.MOV");
    }

    #[test]
    fn csv_fields_are_quoted_like_python() {
        assert_eq!(csv_field("22:19:57;18"), "\"22:19:57;18\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("plain"), "plain");
    }

    #[test]
    fn wildcard_masks() {
        assert!(wildcard("summ*.wav", "SUMM.WAV"));
        assert!(wildcard("*_proxy*", "clip_proxy_01.mov"));
        assert!(!wildcard("summ*.wav", "1_4CH001I.wav"));
        assert!(wildcard("a?c", "abc"));
    }

    #[test]
    fn uniform_filter_matches_scipy_reflect() {
        // scipy.ndimage.uniform_filter1d([1,2,3,4,5], 3) -> [1.333, 2, 3, 4, 4.667]
        let r = uniform_reflect(&[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        let want = [4.0 / 3.0, 2.0, 3.0, 4.0, 14.0 / 3.0];
        for (a, b) in r.iter().zip(want) {
            assert!((a - b).abs() < 1e-12);
        }
        // even size: window [i-2, i+1] -> uniform_filter1d([1,2,3,4,5], 4)[2] == 2.5
        assert!((uniform_reflect(&[1.0, 2.0, 3.0, 4.0, 5.0], 4)[2] - 2.5).abs() < 1e-12);
    }

    /// clip starting at `start` s of day (file time) with timecode label `tc` s, duration `dur`
    fn clip(name: &str, start: u32, tc: Option<u32>, dur: f64) -> Media {
        let hms = |t: u32| format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60);
        Media {
            file: name.into(),
            dur,
            fps: Some(25.0),
            tc: tc.map(|t| format!("{}:00", hms(t))),
            ctime: Some(format!("2026-10-04T{}.000000Z", hms(start))),
            ..Default::default()
        }
    }

    #[test]
    fn timecode_free_run_is_valid() {
        // time-of-day TC 18:47 behind the file clock (like a camera with a wrong TC preset) — still a clock
        let mut v: Vec<Media> = (0..10).map(|i| clip(&format!("c{i}"), 60000 + i * 300, Some(60000 + i * 300 - 1127), 40.0)).collect();
        let info = check_timecodes(&mut v);
        assert!(info.bad.is_empty() && info.none.is_empty());
        assert_eq!(info.dc, Some(-1127.0));
        assert!(!info.ctime_end);
    }

    #[test]
    fn timecode_rec_run_is_detected() {
        // Rec Run: TC counts recorded time only (40 s clips, several minutes apart)
        let mut v: Vec<Media> = (0..10).map(|i| clip(&format!("c{i}"), 60000 + i * 300, Some(i * 40), 40.0)).collect();
        let info = check_timecodes(&mut v);
        assert_eq!(info.bad.len(), 10);
        assert_eq!(info.n_tc, 10);
        assert!(info.dc.is_none());
    }

    #[test]
    fn timecode_partial_problem_lists_clips() {
        let mut v: Vec<Media> = (0..8).map(|i| clip(&format!("c{i}"), 60000 + i * 300, Some(60000 + i * 300), 20.0)).collect();
        v[3].tc = Some("00:00:12:00".into()); // reset timecode
        v.push(clip("notc", 63000, None, 20.0));
        let info = check_timecodes(&mut v);
        assert_eq!(info.bad, vec!["c3".to_string()]);
        assert_eq!(info.none, vec!["notc".to_string()]);
        assert_eq!(info.dc, Some(0.0));
    }

    #[test]
    fn ncc_finds_known_lag() {
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        let mf: Vec<Row> = (0..400).map(|_| std::array::from_fn(|_| rnd())).collect();
        let cf: Vec<Row> = mf[123..223].to_vec();
        let r = ncc(&cf, &mf, 250);
        assert_eq!(argmax(&r), 123);
        assert!((r[123] - 1.0).abs() < 1e-9);
    }
}
