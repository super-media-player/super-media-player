//! Line subtitles up with the speech.
//!
//! Subtitles are on screen while somebody talks, so the loudness of the voice
//! band, over time, rises and falls with them. The two are slid along each
//! other (a cross-correlation) at every usual frame-rate ratio; the right
//! place stands out clearly, in any language. Subtitles of another film or
//! episode fit nowhere, and are refused. If parts of the film sit at another
//! distance (advertisements cut out, a longer version), each part is moved by
//! its own amount.
//!
//! Measured on four TV episodes with known subtitles: the place found was
//! within 0.1 s, the peak 0.33–0.41 high (14–16 standard deviations); subtitles
//! of another episode gave 0.04–0.06 (3–5).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Sound is looked at in frames of 20 ms, at 8 kHz.
const RATE: u32 = 8000;
const FRAME: usize = 160;
const FPS: f64 = 50.0;

/// The furthest subtitles are moved, in seconds.
const MAX_SHIFT: f64 = 180.0;
/// How clear the best place must be: its height, and the same in standard
/// deviations of all places. (Not how far it leads the second best: with a
/// cut in the middle, two places are right.)
const MIN_SCORE: f64 = 0.12;
const MIN_SIGMA: f64 = 7.0;
/// Parts this long are checked one by one for cuts (seconds).
const PART: f64 = 180.0;
/// How far a part may sit from the rest, and from when it counts.
const PART_SHIFT: f64 = 60.0;
const SAME: f64 = 0.4;
/// Subtitles longer on screen than this say little about when it is said.
const MAX_CUE: f64 = 8.0;

/// When a subtitle is on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
}

/// How to move the subtitles: new = old × `ratio` + the offset of the last
/// piece starting (in new time, before its offset) at or before it.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    pub ratio: f64,
    pub pieces: Vec<(f64, f64)>,
    pub score: f64,
}

impl Fit {
    fn offset(&self, scaled: f64) -> f64 {
        self.pieces.iter().rev().find(|p| p.0 <= scaled).or(self.pieces.first()).map_or(0.0, |p| p.1)
    }

    /// The new time of `t`, for a subtitle starting at `start` (so one that
    /// crosses a cut stays in one piece).
    fn map(&self, t: f64, start: f64) -> f64 {
        t * self.ratio + self.offset(start * self.ratio)
    }

    /// Whether anybody would notice.
    pub fn changes(&self) -> bool {
        (self.ratio - 1.0).abs() > 1e-6 || self.pieces.iter().any(|p| p.1.abs() >= 0.15)
    }

    /// "moved 2.3 s later", for the user.
    pub fn describe(&self) -> String {
        if !self.changes() {
            return "they were already in time".into();
        }
        let by = |o: f64| format!("{:.1} s {}", o.abs(), if o < 0.0 { "earlier" } else { "later" });
        let mut text = match self.pieces.as_slice() {
            [p] => format!("moved {}", by(p.1)),
            p => format!("moved in {} parts, the first {}", p.len(), by(p[0].1)),
        };
        if (self.ratio - 1.0).abs() > 1e-6 {
            text += &format!(", and fitted to the frame rate ({:+.1} %)", (self.ratio - 1.0) * 100.0);
        }
        text
    }
}

// ---- the sound ----

/// One second-order filter (the usual "cookbook" low and high pass).
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(freq: f64, high: bool) -> Self {
        let w = std::f64::consts::TAU * freq / f64::from(RATE);
        let alpha = w.sin() / std::f64::consts::SQRT_2;
        let (c, a0) = (w.cos(), 1.0 + alpha);
        let b = if high { [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0] } else { [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0] };
        Self { b: b.map(|x| x / a0), a: [-2.0 * c / a0, (1.0 - alpha) / a0], z: [0.0; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z = [self.b[1] * x - self.a[0] * y + self.z[1], self.b[2] * x - self.a[1] * y];
        y
    }
}

/// The mean of `x` over `half` values to each side.
fn smooth(x: &[f64], half: usize) -> Vec<f64> {
    let mut sum = Vec::with_capacity(x.len() + 1);
    sum.push(0.0);
    for v in x {
        sum.push(sum.last().unwrap_or(&0.0) + v);
    }
    (0..x.len())
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(x.len()));
            (sum[hi] - sum[lo]) / (hi - lo) as f64
        })
        .collect()
}

/// How much louder the voice band is than it usually is thereabouts.
fn loudness(samples: &[i16]) -> Vec<f64> {
    let (mut hp, mut lp) = (Biquad::new(250.0, true), Biquad::new(3000.0, false));
    let energy: Vec<f64> = samples
        .chunks(FRAME)
        .map(|c| {
            let e: f64 = c.iter().map(|s| lp.run(hp.run(f64::from(*s))).powi(2)).sum();
            (e / FRAME as f64 + 1.0).ln()
        })
        .collect();
    let near = smooth(&energy, 5);
    let around = smooth(&energy, (10.0 * FPS) as usize);
    let x: Vec<f64> = near.iter().zip(around).map(|(a, b)| (a - b).clamp(-4.0, 4.0)).collect();
    let slow = smooth(&x, (60.0 * FPS) as usize);
    x.iter().zip(slow).map(|(a, b)| a - b).collect()
}

/// The same for subtitles: 1 while one is on screen, less what is usual
/// thereabouts.
fn presence(cues: &[Cue], ratio: f64, n: usize) -> Vec<f64> {
    let mut x = vec![0.0; n];
    for c in cues {
        let end = c.end.min(c.start + MAX_CUE);
        let at = |t: f64| ((t * ratio * FPS).max(0.0) as usize).min(n);
        x[at(c.start)..at(end).max(at(c.start))].fill(1.0);
    }
    let slow = smooth(&x, (60.0 * FPS) as usize);
    x.iter().zip(slow).map(|(a, b)| a - b).collect()
}

// ---- sliding ----

/// In-place radix-2 FFT of `re` + i·`im`, whose length is a power of two.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    // One table of roots, so long transforms don't drift.
    let table: Vec<(f64, f64)> =
        (0..n / 2).map(|k| (sign * std::f64::consts::TAU * k as f64 / n as f64).sin_cos()).collect();
    let mut len = 2;
    while len <= n {
        let (half, step) = (len / 2, n / len);
        for block in (0..n).step_by(len) {
            for k in 0..half {
                let (s, c) = table[k * step];
                let (a, b) = (block + k, block + k + half);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len *= 2;
    }
}

/// The best place for `b` along `a`.
struct Peak {
    /// `b` moved by this many frames fits best.
    lag: i64,
    /// How well (a correlation, at most 1).
    height: f64,
    /// The same, in standard deviations of all places.
    sigma: f64,
    /// How much higher than anywhere else.
    lead: f64,
}

/// Slide `b` along `a`, up to `max` frames each way.
fn slide(a: &[f64], b: &[f64], max: usize) -> Option<Peak> {
    let norm = (a.iter().map(|x| x * x).sum::<f64>() * b.iter().map(|x| x * x).sum::<f64>()).sqrt();
    if norm < 1e-9 {
        return None;
    }
    let n = (a.len().max(b.len()) + max + 1).next_power_of_two();
    let pad = |x: &[f64]| {
        let mut v = vec![0.0; n];
        v[..x.len()].copy_from_slice(x);
        v
    };
    // Two real signals in one transform.
    let (mut re, mut im) = (pad(a), pad(b));
    fft(&mut re, &mut im, false);
    let (mut xr, mut xi) = (vec![0.0; n], vec![0.0; n]);
    for k in 0..n {
        let m = (n - k) % n;
        let (ar, ai) = ((re[k] + re[m]) / 2.0, (im[k] - im[m]) / 2.0);
        let (br, bi) = ((im[k] + im[m]) / 2.0, (re[m] - re[k]) / 2.0);
        // a · conj(b)
        xr[k] = ar * br + ai * bi;
        xi[k] = ai * br - ar * bi;
    }
    fft(&mut xr, &mut xi, true);
    let max = max.min(n / 2 - 1) as i64;
    let at = |lag: i64| xr[lag.rem_euclid(n as i64) as usize] / n as f64 / norm;
    let lag = (-max..=max).max_by(|x, y| at(*x).total_cmp(&at(*y)))?;
    let height = at(lag);
    let next = (-max..=max).filter(|l| (l - lag).abs() > FPS as i64).map(at).fold(0.0, f64::max);
    let count = (2 * max + 1) as f64;
    let mean = (-max..=max).map(at).sum::<f64>() / count;
    let deviation = ((-max..=max).map(|l| (at(l) - mean).powi(2)).sum::<f64>() / count).sqrt();
    let sigma = if deviation > 0.0 { (height - mean) / deviation } else { 0.0 };
    Some(Peak { lag, height, sigma, lead: if next > 0.0 { height / next } else { f64::INFINITY } })
}

/// Where `cues` belong along `sound` (from [`loudness`]); None when they
/// don't clearly fit anywhere.
pub fn fit(sound: &[f64], cues: &[Cue], cancel: &AtomicBool) -> Option<Fit> {
    if cues.len() < 10 || sound.len() < (60.0 * FPS) as usize {
        return None;
    }
    let same = 1.0;
    let (film, pal, ntsc) = (24.0, 25.0, 24000.0 / 1001.0);
    let ratios = [same, film / ntsc, pal / ntsc, pal / film, ntsc / film, ntsc / pal, film / pal];
    let last = cues.iter().map(|c| c.end).fold(0.0, f64::max);
    let length = |ratio: f64| sound.len().max((last * ratio * FPS) as usize + 1);
    let mut best: Option<(Peak, f64)> = None;
    for ratio in ratios {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let p = slide(sound, &presence(cues, ratio, length(ratio)), (MAX_SHIFT * FPS) as usize)?;
        // Another speed has to fit clearly better than the one it has.
        if best.as_ref().is_none_or(|(b, _)| p.height > b.height * 1.05) {
            best = Some((p, ratio));
        }
    }
    let (peak, ratio) = best?;
    log::info!("subtitles: ratio {ratio:.4}, {:+.2} s, height {:.3}, {:.1} sigma", peak.lag as f64 / FPS, peak.height, peak.sigma);
    if peak.height < MIN_SCORE || peak.sigma < MIN_SIGMA {
        return None;
    }
    let base = peak.lag as f64 / FPS;
    let pieces = cuts(sound, cues, ratio, base);
    Some(Fit { ratio, pieces, score: peak.height })
}

/// The offsets of the film's parts: one piece, unless some parts clearly sit
/// elsewhere than `base`.
fn cuts(sound: &[f64], cues: &[Cue], ratio: f64, base: f64) -> Vec<(f64, f64)> {
    let scaled: Vec<Cue> = cues.iter().map(|c| Cue { start: c.start * ratio, end: c.end * ratio }).collect();
    let last = scaled.iter().map(|c| c.start).fold(0.0, f64::max);
    // Each part's own best place, where it is clear.
    let mut found: Vec<(f64, f64)> = Vec::new();
    let margin = PART_SHIFT * FPS;
    for i in 0..(last / PART) as usize + 1 {
        let from = i as f64 * PART;
        let part: Vec<Cue> = scaled.iter().copied().filter(|c| c.start >= from && c.start < from + PART).collect();
        if part.len() < 8 {
            continue;
        }
        // The stretch of sound around the part, where it should be.
        let start = ((from + base) * FPS - margin).max(0.0) as usize;
        let end = (((from + PART + base) * FPS + margin).max(0.0) as usize).min(sound.len());
        if start >= end {
            continue;
        }
        let moved: Vec<Cue> =
            part.iter().map(|c| Cue { start: c.start + base - start as f64 / FPS, end: c.end + base - start as f64 / FPS }).collect();
        let Some(p) = slide(&sound[start..end], &presence(&moved, 1.0, end - start), margin as usize) else { continue };
        if p.height >= 0.15 && p.lead >= 1.3 {
            let o = p.lag as f64 / FPS;
            found.push((from, if o.abs() < SAME { 0.0 } else { o }));
        }
    }
    // A part alone in its opinion is mistaken.
    let agree = |a: f64, b: f64| (a - b).abs() < SAME;
    let sure: Vec<(f64, f64)> = (0..found.len())
        .filter(|&i| {
            let o = found[i].1;
            o == 0.0 || (i > 0 && agree(found[i - 1].1, o)) || found.get(i + 1).is_some_and(|f| agree(f.1, o))
        })
        .map(|i| found[i])
        .collect();
    let mut runs: Vec<(f64, f64, f64)> = Vec::new(); // first part, last part, offset
    for (from, o) in sure {
        match runs.last_mut() {
            Some(r) if agree(r.2, o) => r.1 = from,
            _ => runs.push((from, from, o)),
        }
    }
    let mut pieces = vec![(0.0, base + runs.first().map_or(0.0, |r| r.2))];
    for w in runs.windows(2) {
        // The cut is somewhere between: after the cue where the one offset
        // stops fitting and the other starts to.
        let fits = |c: &Cue, o: f64| -> f64 {
            let at = |t: f64| (((t + base + o) * FPS).max(0.0) as usize).min(sound.len());
            let s = &sound[at(c.start)..at(c.end).max(at(c.start))];
            s.iter().sum::<f64>()
        };
        let between: Vec<&Cue> = scaled.iter().filter(|c| c.start >= w[0].1 && c.start < w[1].0 + PART).collect();
        let gain: Vec<f64> = between.iter().map(|c| fits(c, w[0].2) - fits(c, w[1].2)).collect();
        let (mut sum, mut top, mut cut) = (0.0, f64::MIN, 0);
        for (i, g) in gain.iter().enumerate() {
            if sum > top {
                (top, cut) = (sum, i);
            }
            sum += g;
        }
        if let Some(c) = between.get(cut) {
            pieces.push((c.start, base + w[1].2));
        }
    }
    pieces
}

// ---- subtitle files ----

/// The kinds of files that can be read and rewritten.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// SRT and WebVTT.
    Arrow,
    /// ASS and SSA.
    Dialogue,
}

fn kind(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_lowercase().as_str() {
        "srt" | "vtt" => Some(Kind::Arrow),
        "ass" | "ssa" => Some(Kind::Dialogue),
        _ => None,
    }
}

/// Whether [`run`] can work on this subtitle file.
pub fn can_sync(path: &Path) -> bool {
    kind(path).is_some() && path.is_file()
}

/// The text of a subtitle file. Only the times are looked at and rewritten;
/// each byte of anything else stays what it is, whatever the encoding, by
/// reading every byte as one character. (UTF-16 becomes UTF-8.)
fn read(bytes: &[u8]) -> (String, bool) {
    let wide = |le: bool| {
        let u: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| if le { u16::from_le_bytes(*b) } else { u16::from_be_bytes(*b) })
            .collect();
        String::from_utf16_lossy(&u)
    };
    match bytes {
        [0xff, 0xfe, ..] => (wide(true), true),
        [0xfe, 0xff, ..] => (wide(false), true),
        _ => (bytes.iter().map(|b| *b as char).collect(), false),
    }
}

fn write(text: &str, utf8: bool) -> Vec<u8> {
    if utf8 { text.as_bytes().to_vec() } else { text.chars().map(|c| c as u8).collect() }
}

/// A line's times: what is before, the start, what is between, the end, the rest.
fn times(line: &str, kind: Kind) -> Option<(&str, Cue, &str, &str)> {
    let time = crate::transcribe::parse_time;
    match kind {
        Kind::Arrow => {
            let (a, b) = line.split_once("-->")?;
            let b = b.trim_start();
            let (end, rest) = b.split_at(b.find(char::is_whitespace).unwrap_or(b.len()));
            Some(("", Cue { start: time(a)?, end: time(end)? }, " --> ", rest))
        }
        Kind::Dialogue => {
            let rest = line.strip_prefix("Dialogue:")?;
            let (layer, rest) = rest.split_once(',')?;
            let (a, rest) = rest.split_once(',')?;
            let (b, _) = rest.split_once(',')?;
            let before = &line[..line.len() - rest.len() - a.len() - 1];
            debug_assert!(before.ends_with(',') && before.contains(layer));
            Some((before, Cue { start: time(a)?, end: time(b)? }, ",", &rest[b.len()..]))
        }
    }
}

/// The cues somebody speaks: without sounds ("[door slams]") and songs.
fn cues(text: &str, kind: Kind) -> Vec<Cue> {
    let lines: Vec<&str> = text.lines().collect();
    let mut all = Vec::new();
    let mut spoken = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((_, cue, _, rest)) = times(line, kind) else { continue };
        if cue.end <= cue.start {
            continue;
        }
        all.push(cue);
        let words = match kind {
            Kind::Arrow => lines[i + 1..].iter().take_while(|l| !l.trim().is_empty()).copied().collect::<Vec<_>>().join(" "),
            Kind::Dialogue => rest.splitn(8, ',').last().unwrap_or_default().to_owned(),
        };
        let words = crate::transcribe::strip_markup(&words);
        let w = words.trim();
        let sound = (w.starts_with('[') && w.ends_with(']')) || (w.starts_with('(') && w.ends_with(')'));
        // "♪" in UTF-8, read byte by byte, or as it is.
        if !w.is_empty() && !sound && !w.contains('♪') && !w.contains("\u{e2}\u{99}\u{aa}") {
            spoken.push(cue);
        }
    }
    if spoken.len() >= 10 { spoken } else { all }
}

fn clock(t: f64, comma: bool) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    let (h, m, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    format!("{h:02}:{m:02}:{s:02}{}{:03}", if comma { ',' } else { '.' }, ms % 1000)
}

fn ass_clock(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

/// The file with its times moved.
fn retime(text: &str, kind: Kind, fit: &Fit) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let Some((before, cue, between, rest)) = times(body, kind) else {
            out += line;
            continue;
        };
        let (a, b) = (fit.map(cue.start, cue.start), fit.map(cue.end, cue.start));
        let show = |t: f64| match kind {
            Kind::Arrow => clock(t, body.contains(',') && !text.starts_with("WEBVTT")),
            Kind::Dialogue => ass_clock(t),
        };
        out += &format!("{before}{}{between}{}{rest}{}", show(a), show(b), &line[body.len()..]);
    }
    out
}

// ---- doing it ----

/// What to line up with what.
pub struct Job {
    pub video: String,
    pub aid: Option<i64>,
    pub duration: f64,
    pub subtitles: PathBuf,
}

/// Why it wasn't done.
pub const UNSURE: &str = "The subtitles don't clearly match the speech, so they were left as they are. They may belong to another version or episode.";

/// Listen to the film and work out how the subtitles must move. `progress`
/// gets 0..1. Returns that, and the corrected file. Blocks for some seconds.
pub fn run(job: &Job, cancel: &AtomicBool, progress: impl Fn(f32)) -> Result<(Fit, Vec<u8>), String> {
    let kind = kind(&job.subtitles).ok_or("Only SRT, WebVTT and ASS subtitles can be lined up.")?;
    let bytes = std::fs::read(&job.subtitles).map_err(|e| format!("{}: {e}", job.subtitles.display()))?;
    let (text, utf8) = read(&bytes);
    let cues = cues(&text, kind);
    if cues.len() < 10 {
        return Err("There are too few lines in these subtitles to go by.".into());
    }
    let dir = crate::settings::cache_dir().ok_or("no cache folder")?.join("pcm");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let raw = dir.join(format!("sync-{}-{:?}.raw", std::process::id(), std::thread::current().id()));
    let d = crate::pcm::Decode {
        source: &job.video,
        out: &raw,
        rate: RATE,
        wav: false,
        start: 0.0,
        length: None,
        aid: job.aid,
    };
    let duration = job.duration.max(1.0);
    crate::pcm::render(&d, cancel, |t| progress((t / duration).clamp(0.0, 1.0) as f32))?;
    let sound = std::fs::read(&raw);
    let _ = std::fs::remove_file(&raw);
    let sound = sound.map_err(|e| format!("{}: {e}", raw.display()))?;
    let samples: Vec<i16> = sound.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
    let found = fit(&loudness(&samples), &cues, cancel);
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    let fit = found.ok_or(UNSURE)?;
    let new = write(&retime(&text, kind, &fit), utf8);
    Ok((fit, new))
}

/// Put the corrected subtitles in place of `file`, keeping the original next
/// to it as "….orig" (the first one: syncing again doesn't lose it).
pub fn replace(file: &Path, new: &[u8], backup: bool) -> std::io::Result<()> {
    if backup {
        let mut orig = file.as_os_str().to_owned();
        orig.push(".orig");
        let orig = PathBuf::from(orig);
        if !orig.exists() {
            std::fs::copy(file, &orig)?;
        }
    }
    // All or nothing: a full disk must not leave half a file.
    let part = file.with_extension("sync-part");
    std::fs::write(&part, new).and_then(|()| std::fs::rename(&part, file)).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numbers that look random, the same every time.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as f64 / (1u64 << 31) as f64
        }
    }

    /// Lines of speech: 1–4 s, with pauses.
    fn speech(seed: u64, length: f64) -> Vec<Cue> {
        let mut r = Rng(seed);
        let (mut t, mut v) = (5.0, Vec::new());
        while t < length - 10.0 {
            let end = t + 1.0 + 3.0 * r.next();
            v.push(Cue { start: t, end });
            t = end + 0.3 + 6.0 * r.next() * r.next();
        }
        v
    }

    /// Sound in which somebody speaks at `cues`, over noise.
    fn sound(cues: &[Cue], length: f64) -> Vec<f64> {
        let mut r = Rng(9);
        let n = (length * f64::from(RATE)) as usize;
        let mut x: Vec<i16> = (0..n).map(|_| ((r.next() - 0.5) * 600.0) as i16).collect();
        for c in cues {
            let (from, to) = ((c.start * 8000.0) as usize, ((c.end * 8000.0) as usize).min(n));
            for (i, v) in x[from..to].iter_mut().enumerate() {
                let t = (from + i) as f64 / 8000.0;
                let syllable = (t * 25.0).sin().max(0.0);
                *v += ((t * 5000.0).sin() * 4000.0 * syllable) as i16;
            }
        }
        loudness(&x)
    }

    fn moved(cues: &[Cue], ratio: f64, by: f64) -> Vec<Cue> {
        cues.iter().map(|c| Cue { start: (c.start + by) * ratio, end: (c.end + by) * ratio }).collect()
    }

    static NEVER: AtomicBool = AtomicBool::new(false);

    #[test]
    fn finds_shift_and_frame_rate() {
        let spoken = speech(1, 900.0);
        let s = sound(&spoken, 900.0);
        for (ratio, by) in [(1.0, 0.0), (1.0, 37.3), (1.0, -4.4), (25.0 / 24.0, 5.0), (24000.0 / 1001.0 / 25.0, -3.0)] {
            let f = fit(&s, &moved(&spoken, ratio, by), &NEVER).expect("fits");
            assert_eq!(f.pieces.len(), 1, "{f:?}");
            assert!((f.ratio * ratio - 1.0).abs() < 1e-4, "{f:?} for {ratio}");
            assert!((f.pieces[0].1 + by).abs() < 0.12, "{f:?} for {by}");
            assert_eq!(f.changes(), by != 0.0);
        }
    }

    #[test]
    fn refuses_other_subtitles() {
        let s = sound(&speech(1, 900.0), 900.0);
        for seed in 2..6 {
            assert_eq!(fit(&s, &speech(seed, 900.0), &NEVER), None);
        }
        assert_eq!(fit(&s, &[], &NEVER), None);
    }

    #[test]
    fn moves_each_side_of_a_cut_by_its_own() {
        let spoken = speech(1, 1800.0);
        let s = sound(&spoken, 1800.0);
        // The subtitles are for a version with 20 s more from 1000 s on.
        let cues: Vec<Cue> = spoken
            .iter()
            .map(|c| {
                let by = if c.start >= 1000.0 { 22.0 } else { 2.0 };
                Cue { start: c.start + by, end: c.end + by }
            })
            .collect();
        let f = fit(&s, &cues, &NEVER).expect("fits");
        assert_eq!(f.pieces.len(), 2, "{f:?}");
        let off = cues.iter().zip(&spoken).filter(|(c, s)| (f.map(c.start, c.start) - s.start).abs() > 0.2).count();
        assert!(off <= 2, "{off} lines off: {f:?}");
    }

    #[test]
    fn rewrites_only_the_times() {
        let f = Fit { ratio: 1.0, pieces: vec![(0.0, 2.5)], score: 0.4 };
        let srt = "1\r\n00:00:01,000 --> 00:00:02,500 X1:40\r\n\u{ce}i, ce faci?\r\n\r\n2\r\n01:59:58,000 --> 01:59:59,999\r\n--> no\r\n";
        // Latin-2 bytes, not UTF-8, come through untouched.
        let bytes: Vec<u8> = srt.chars().map(|c| c as u8).collect();
        let (text, utf8) = read(&bytes);
        let new = write(&retime(&text, Kind::Arrow, &f), utf8);
        let expect = srt.replace("00:00:01,000", "00:00:03,500").replace("00:00:02,500", "00:00:05,000");
        let expect = expect.replace("01:59:58,000", "02:00:00,500").replace("01:59:59,999", "02:00:02,499");
        assert_eq!(new, expect.chars().map(|c| c as u8).collect::<Vec<u8>>());

        let vtt = "WEBVTT\n\n00:05.000 --> 00:06.000 line:1, x\nHi\n";
        assert_eq!(retime(vtt, Kind::Arrow, &f), "WEBVTT\n\n00:00:07.500 --> 00:00:08.500 line:1, x\nHi\n");

        let ass = "[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hi, you\nComment: 0,0:00:01.00,0:00:02.00,x\n";
        assert_eq!(
            retime(ass, Kind::Dialogue, &f),
            "[Events]\nDialogue: 0,0:00:03.50,0:00:04.50,Default,,0,0,0,,Hi, you\nComment: 0,0:00:01.00,0:00:02.00,x\n"
        );
        // Nothing goes below zero.
        let back = Fit { ratio: 1.0, pieces: vec![(0.0, -5.0)], score: 0.4 };
        assert!(retime(vtt, Kind::Arrow, &back).contains("00:00:00.000 --> 00:00:01.000"));
    }

    #[test]
    fn utf16_becomes_utf8() {
        let mut b = vec![0xff, 0xfe];
        b.extend("1\n00:00:01,000 --> 00:00:02,000\nПривет\n".encode_utf16().flat_map(u16::to_le_bytes));
        let (text, utf8) = read(&b);
        assert!(utf8 && text.contains("Привет"));
        assert!(String::from_utf8(write(&text, utf8)).unwrap().contains("Привет"));
    }

    #[test]
    fn sounds_and_songs_do_not_count() {
        let mut srt = String::new();
        for i in 0..20 {
            let what = ["Hello.", "[door slams]", "(sighs)", "♪ La la ♪"][i % 4];
            let t = i * 4;
            srt += &format!("{i}\n00:{:02}:{:02},000 --> 00:{:02}:{:02},000\n{what}\n\n", t / 60, t % 60, (t + 2) / 60, (t + 2) % 60);
        }
        // Too few are left: all of them are used.
        assert_eq!(cues(&srt, Kind::Arrow).len(), 20);
        let long = srt.repeat(3);
        assert_eq!(cues(&long, Kind::Arrow).len(), 15);
        let bytes = read(long.as_bytes()).0;
        assert_eq!(cues(&bytes, Kind::Arrow).len(), 15);
    }

    #[test]
    fn says_what_it_did() {
        let f = |ratio, pieces: &[(f64, f64)]| Fit { ratio, pieces: pieces.to_vec(), score: 0.4 }.describe();
        assert_eq!(f(1.0, &[(0.0, 0.04)]), "they were already in time");
        assert_eq!(f(1.0, &[(0.0, -2.34)]), "moved 2.3 s earlier");
        assert_eq!(f(25.0 / 24.0, &[(0.0, 1.0)]), "moved 1.0 s later, and fitted to the frame rate (+4.2 %)");
        assert_eq!(f(1.0, &[(0.0, 1.0), (9.0, 2.0)]), "moved in 2 parts, the first 1.0 s later");
    }

    #[test]
    fn keeps_the_first_original() {
        let dir = std::env::temp_dir().join(format!("smp-subsync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("film.en.srt");
        std::fs::write(&f, "one").unwrap();
        replace(&f, b"two", true).unwrap();
        replace(&f, b"three", true).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"three");
        assert_eq!(std::fs::read(dir.join("film.en.srt.orig")).unwrap(), b"one");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `SMP_SYNC_VIDEO=… SMP_SYNC_SUBS=… cargo test subsync_this -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a film and its subtitles"]
    fn subsync_this() {
        let job = Job {
            video: std::env::var("SMP_SYNC_VIDEO").unwrap(),
            aid: None,
            duration: 0.0,
            subtitles: std::env::var("SMP_SYNC_SUBS").unwrap().into(),
        };
        let start = std::time::Instant::now();
        match run(&job, &NEVER, |_| {}) {
            Ok((fit, _)) => println!("{} {fit:?} in {:?}", fit.describe(), start.elapsed()),
            Err(e) => println!("{e} in {:?}", start.elapsed()),
        }
    }
}
