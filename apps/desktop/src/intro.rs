//! Intros and credits of series episodes, found by listening.
//!
//! Episodes of a series share their intro music and their credits music.
//! For the episode playing and its neighbour in the same folder, a hidden
//! mpv decodes the sound of the first minutes and the last minutes (audio
//! only, as fast as it can, mono at 11 kHz), and Chromaprint (the AcoustID
//! fingerprinter, loaded at runtime) turns it into a fingerprint: one 32-bit
//! number per ~0.12 s. The longest stretch where the two episodes'
//! fingerprints agree, at some shift, is the shared intro; the same at the
//! end gives the credits.
//!
//! Files with chapters named like "Intro", "Opening", "Credits" or "Ending"
//! use those instead, with no listening at all. Results are cached on disk,
//! keyed by the files' size and date.

use std::ffi::c_int;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::channel;
use std::time::Duration;

use player_core::{LoadMode, Player, PlayerConfig, PlayerEvent, Value};

/// Seconds listened to at the start and at the end of each episode.
const HEAD_SECS: f64 = 600.0;
const TAIL_SECS: f64 = 420.0;
/// Shortest and longest believable intro, seconds.
const MIN_INTRO: f64 = 12.0;
const MAX_INTRO: f64 = 180.0;
/// Shortest credits worth a button.
const MIN_CREDITS: f64 = 12.0;
/// Two fingerprint items agree when at most this many of their 32 bits
/// differ, and a run of agreement may have this many disagreeing items in
/// a row (3.5 s). The values Jellyfin's intro skipper settled on; on real
/// recordings looser ones find the same intros but also more false ones.
const MAX_BIT_ERRORS: u32 = 6;
const MAX_GAP: usize = 28;
/// The fingerprint's view of an intro ends a little early; skipping lands
/// this much later.
const END_PAD: f64 = 0.6;
const SAMPLE_RATE: i32 = 11_025;

/// What was found in one episode: times in seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Marks {
    pub intro: Option<(f64, f64)>,
    pub credits: Option<f64>,
}

// ---- Chromaprint, loaded at runtime ----

struct Chromaprint {
    _lib: libloading::Library,
    new: unsafe extern "C" fn(c_int) -> *mut std::ffi::c_void,
    free: unsafe extern "C" fn(*mut std::ffi::c_void),
    start: unsafe extern "C" fn(*mut std::ffi::c_void, c_int, c_int) -> c_int,
    feed: unsafe extern "C" fn(*mut std::ffi::c_void, *const i16, c_int) -> c_int,
    finish: unsafe extern "C" fn(*mut std::ffi::c_void) -> c_int,
    raw: unsafe extern "C" fn(*mut std::ffi::c_void, *mut *mut u32, *mut c_int) -> c_int,
    item_ms: unsafe extern "C" fn(*mut std::ffi::c_void) -> c_int,
    dealloc: unsafe extern "C" fn(*mut std::ffi::c_void),
}

// SAFETY: function pointers into a library that stays loaded; each context
// is used by one thread at a time.
unsafe impl Send for Chromaprint {}
unsafe impl Sync for Chromaprint {}

fn chromaprint() -> Option<&'static Chromaprint> {
    static LIB: OnceLock<Option<Chromaprint>> = OnceLock::new();
    LIB.get_or_init(|| {
        let names = if cfg!(windows) {
            &["chromaprint.dll", "libchromaprint.dll"][..]
        } else if cfg!(target_os = "macos") {
            &["libchromaprint.1.dylib", "libchromaprint.dylib"][..]
        } else {
            &["libchromaprint.so.1", "libchromaprint.so"][..]
        };
        let lib = crate::disc::open_bundled_library(names)?;
        unsafe {
            macro_rules! sym {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).ok()?
                };
            }
            Some(Chromaprint {
                new: sym!("chromaprint_new"),
                free: sym!("chromaprint_free"),
                start: sym!("chromaprint_start"),
                feed: sym!("chromaprint_feed"),
                finish: sym!("chromaprint_finish"),
                raw: sym!("chromaprint_get_raw_fingerprint"),
                item_ms: sym!("chromaprint_get_item_duration_ms"),
                dealloc: sym!("chromaprint_dealloc"),
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether intros can be found by listening on this system.
pub fn available() -> bool {
    chromaprint().is_some()
}

/// Raw fingerprint of mono 16-bit samples at `SAMPLE_RATE`, and the
/// seconds each item stands for.
fn fingerprint(samples: &[i16]) -> Option<(Vec<u32>, f64)> {
    let cp = chromaprint()?;
    unsafe {
        // CHROMAPRINT_ALGORITHM_DEFAULT (TEST2), the one AcoustID uses.
        let ctx = (cp.new)(1);
        if ctx.is_null() {
            return None;
        }
        let mut out = None;
        if (cp.start)(ctx, SAMPLE_RATE, 1) == 1 {
            let mut ok = true;
            for chunk in samples.chunks(1 << 16) {
                ok &= (cp.feed)(ctx, chunk.as_ptr(), chunk.len() as c_int) == 1;
            }
            let mut fp: *mut u32 = std::ptr::null_mut();
            let mut n: c_int = 0;
            if ok && (cp.finish)(ctx) == 1 && (cp.raw)(ctx, &mut fp, &mut n) == 1 && !fp.is_null() {
                let items = std::slice::from_raw_parts(fp, n.max(0) as usize).to_vec();
                (cp.dealloc)(fp.cast());
                let secs = f64::from((cp.item_ms)(ctx)) / 1000.0;
                out = Some((items, if secs > 0.0 { secs } else { 0.1238 }));
            }
        }
        (cp.free)(ctx);
        out
    }
}

// ---- decoding the sound ----

/// Mono 16-bit samples of `path` from `start` for `length` seconds.
fn decode(path: &Path, start: f64, length: f64) -> Option<Vec<i16>> {
    crate::pcm::samples(path, SAMPLE_RATE as u32, start, length)
}

/// Fingerprints of an episode's start and end, cached on disk.
#[derive(Clone, Debug, Default, PartialEq)]
struct Prints {
    head: Vec<u32>,
    tail: Vec<u32>,
    /// Seconds per item.
    step: f64,
    /// Where the tail starts, seconds.
    tail_start: f64,
}

fn cache_file(path: &Path) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let mut h = Sha256::new();
    h.update(path.to_string_lossy().as_bytes());
    h.update(meta.len().to_le_bytes());
    h.update(mtime.to_le_bytes());
    let name: String = h.finalize()[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(
        crate::settings::cache_dir()?
            .join("intros")
            .join(format!("{name}.fp")),
    )
}

impl Prints {
    fn to_bytes(&self) -> Vec<u8> {
        let mut b = b"SMPFP1".to_vec();
        b.extend(self.step.to_le_bytes());
        b.extend(self.tail_start.to_le_bytes());
        b.extend((self.head.len() as u32).to_le_bytes());
        b.extend((self.tail.len() as u32).to_le_bytes());
        for x in self.head.iter().chain(&self.tail) {
            b.extend(x.to_le_bytes());
        }
        b
    }

    fn from_bytes(b: &[u8]) -> Option<Self> {
        let b = b.strip_prefix(b"SMPFP1")?;
        let f = |i: usize| Some(f64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?));
        let u = |i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
        let (step, tail_start) = (f(0)?, f(8)?);
        let (nh, nt) = (u(16)? as usize, u(20)? as usize);
        if b.len() != 24 + 4 * (nh + nt) {
            return None;
        }
        let items: Vec<u32> = (0..nh + nt).map(|i| u(24 + 4 * i)).collect::<Option<_>>()?;
        Some(Self {
            head: items[..nh].to_vec(),
            tail: items[nh..].to_vec(),
            step,
            tail_start,
        })
    }
}

fn prints(path: &Path, duration: f64) -> Option<Prints> {
    let cache = cache_file(path);
    if let Some(p) = cache
        .as_ref()
        .and_then(|c| std::fs::read(c).ok())
        .and_then(|b| Prints::from_bytes(&b))
    {
        return Some(p);
    }
    let head_len = HEAD_SECS.min(duration * 0.4);
    let tail_len = TAIL_SECS.min(duration * 0.3);
    let tail_start = (duration - tail_len).max(head_len);
    let (head, step) = fingerprint(&decode(path, 0.0, head_len)?)?;
    let (tail, _) = fingerprint(&decode(path, tail_start, tail_len)?)?;
    let p = Prints {
        head,
        tail,
        step,
        tail_start,
    };
    if let Some(c) = cache {
        let _ = std::fs::write(c, p.to_bytes());
    }
    Some(p)
}

// ---- comparing ----

/// The longest stretch where `a` and `b` agree at one shift: (start in a,
/// start in b, length), in items.
fn longest_match(a: &[u32], b: &[u32]) -> Option<(usize, usize, usize)> {
    longest_match_with(a, b, MAX_BIT_ERRORS, MAX_GAP)
}

fn longest_match_with(
    a: &[u32],
    b: &[u32],
    max_bits: u32,
    max_gap: usize,
) -> Option<(usize, usize, usize)> {
    let mut best: Option<(usize, usize, usize)> = None;
    let (na, nb) = (a.len() as isize, b.len() as isize);
    // shift = index in a - index in b
    for shift in -(nb - 1)..na {
        let a0 = shift.max(0) as usize;
        let b0 = (-shift).max(0) as usize;
        let len = (a.len() - a0).min(b.len() - b0);
        if best.is_some_and(|(_, _, l)| l >= len) {
            continue;
        }
        let mut run_start = None;
        let mut last_hit = 0;
        let mut gap = 0;
        for i in 0..len {
            if (a[a0 + i] ^ b[b0 + i]).count_ones() <= max_bits {
                if run_start.is_none() {
                    run_start = Some(i);
                }
                last_hit = i;
                gap = 0;
            } else if let Some(s) = run_start {
                gap += 1;
                if gap > max_gap {
                    let l = last_hit + 1 - s;
                    if best.is_none_or(|(_, _, bl)| l > bl) {
                        best = Some((a0 + s, b0 + s, l));
                    }
                    run_start = None;
                }
            }
        }
        if let Some(s) = run_start {
            let l = last_hit + 1 - s;
            if best.is_none_or(|(_, _, bl)| l > bl) {
                best = Some((a0 + s, b0 + s, l));
            }
        }
    }
    best
}

/// Intro and credits of `a`, found by comparing with episode `b`.
fn compare(a: &Prints, b: &Prints) -> Marks {
    let step = a.step;
    let intro = longest_match(&a.head, &b.head).and_then(|(sa, _, l)| {
        let (start, len) = (sa as f64 * step, l as f64 * step);
        (MIN_INTRO..=MAX_INTRO)
            .contains(&len)
            .then_some((start, start + len + END_PAD))
    });
    let credits = longest_match(&a.tail, &b.tail).and_then(|(sa, _, l)| {
        (l as f64 * step >= MIN_CREDITS).then_some(a.tail_start + sa as f64 * step)
    });
    Marks { intro, credits }
}

// ---- chapters ----

/// Marks from chapter names, when the file has them.
pub fn from_chapters(chapters: &Value, duration: f64) -> Marks {
    let list: Vec<(f64, String)> = chapters
        .as_array()
        .iter()
        .filter_map(|c| {
            Some((
                c.get("time")?.as_f64()?,
                c.str_field("title").unwrap_or_default().to_lowercase(),
            ))
        })
        .collect();
    let is = |t: &str, words: &[&str]| {
        let words_in: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).collect();
        words.iter().any(|w| words_in.contains(w))
    };
    let mut marks = Marks::default();
    for (i, (time, title)) in list.iter().enumerate() {
        let end = list.get(i + 1).map_or(duration, |n| n.0);
        if marks.intro.is_none()
            && is(title, &["intro", "opening", "op", "recap"])
            && end - time <= MAX_INTRO * 1.5
        {
            marks.intro = Some((*time, end));
        }
        if marks.credits.is_none()
            && is(title, &["credits", "ending", "ed", "outro", "end"])
            && *time > duration * 0.5
        {
            marks.credits = Some(*time);
        }
    }
    marks
}

// ---- episodes ----

const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "webm", "wmv", "ts", "m2ts", "mpg", "ogv",
];

/// The other episodes in `path`'s folder: same extension, in natural order.
pub fn siblings(path: &Path) -> Vec<PathBuf> {
    let Some(ext) = path.extension().map(|e| e.to_ascii_lowercase()) else {
        return Vec::new();
    };
    if !VIDEO_EXTS.iter().any(|v| ext == *v) {
        return Vec::new();
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file() && p.extension().map(|e| e.to_ascii_lowercase()).as_ref() == Some(&ext)
        })
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    files.sort_by(|a, b| crate::natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    files
}

/// Whether two file names look like episodes of one series: they differ
/// only in their numbers.
pub fn same_series(a: &Path, b: &Path) -> bool {
    let shape = |p: &Path| -> String {
        let name = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let mut out = String::new();
        for c in name.chars() {
            if c.is_ascii_digit() {
                if !out.ends_with('#') {
                    out.push('#');
                }
            } else {
                out.push(c);
            }
        }
        out
    };
    let (sa, sb) = (shape(a), shape(b));
    // Episode titles in the names ("S01E02 The Return") differ too; the
    // part before the episode number must agree.
    let prefix = sa
        .chars()
        .zip(sb.chars())
        .take_while(|(x, y)| x == y)
        .count();
    sa.contains('#') && (sa == sb || prefix >= 6)
}

/// The episode after `path`, if its folder holds one.
pub fn next_episode(path: &Path) -> Option<PathBuf> {
    let all = siblings(path);
    let i = all.iter().position(|p| p == path)?;
    all.get(i + 1).filter(|n| same_series(path, n)).cloned()
}

/// Find the intro and credits of `path` (duration in seconds) by listening
/// to it and a neighbouring episode. Blocks for a few seconds the first
/// time (cached after that). None when there is no neighbour or no
/// Chromaprint.
pub fn detect(path: &Path, duration: f64) -> Option<Marks> {
    if !available() || duration < 300.0 {
        return None;
    }
    let all = siblings(path);
    let i = all.iter().position(|p| p == path)?;
    // The neighbours, nearest first; one that fails (unreadable) is skipped.
    let others = [i + 1, i.wrapping_sub(1), i + 2]
        .into_iter()
        .filter_map(|j| all.get(j))
        .filter(|o| same_series(path, o));
    let mine = prints(path, duration)?;
    for other in others {
        let Some(d) = probe_duration(other) else {
            continue;
        };
        let Some(theirs) = prints(other, d) else {
            continue;
        };
        let marks = compare(&mine, &theirs);
        log::info!("intro detection {}: {marks:?}", path.display());
        return Some(marks);
    }
    None
}

/// A file's duration, from a hidden mpv.
fn probe_duration(path: &Path) -> Option<f64> {
    let (tx, rx) = channel();
    let mut cfg = PlayerConfig::headless();
    cfg.set("pause", "yes")
        .set("vid", "no")
        .set("aid", "no")
        .set("sid", "no")
        .set("load-scripts", "no");
    let player = Player::new(&cfg, move |e| {
        let _ = tx.send(e);
    })
    .ok()?;
    player
        .load(&path.to_string_lossy(), LoadMode::Replace)
        .ok()?;
    loop {
        match rx.recv_timeout(Duration::from_secs(10)).ok()? {
            PlayerEvent::FileLoaded => break,
            PlayerEvent::EndFile { .. } | PlayerEvent::Shutdown => return None,
            _ => {}
        }
    }
    player.get_double("duration").filter(|d| *d > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_shared_stretch() {
        // Two "episodes": different noise around the same 200-item intro,
        // placed at different times.
        let mut seed = 7u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        let intro: Vec<u32> = (0..200).map(|_| noise()).collect();
        let mut a: Vec<u32> = (0..100).map(|_| noise()).collect();
        a.extend(&intro);
        a.extend((0..300).map(|_| noise()));
        let mut b: Vec<u32> = (0..40).map(|_| noise()).collect();
        // A few bits flipped, and one item lost in the noise.
        b.extend(
            intro
                .iter()
                .enumerate()
                .map(|(i, x)| if i == 50 { noise() } else { x ^ 0b101 }),
        );
        b.extend((0..300).map(|_| noise()));
        let (sa, sb, len) = longest_match(&a, &b).unwrap();
        assert_eq!((sa, sb), (100, 40));
        assert_eq!(len, 200);
        let pa = Prints {
            head: a.clone(),
            tail: vec![],
            step: 0.1238,
            tail_start: 0.0,
        };
        let pb = Prints {
            head: b,
            tail: vec![],
            step: 0.1238,
            tail_start: 0.0,
        };
        let m = compare(&pa, &pb);
        let (s, e) = m.intro.unwrap();
        assert!(
            (s - 12.38).abs() < 0.01 && (e - s - 24.76 - END_PAD).abs() < 0.01,
            "{m:?}"
        );
        assert_eq!(Prints::from_bytes(&pa.to_bytes()), Some(pa));
    }

    #[test]
    fn episode_names() {
        let p = |s: &str| PathBuf::from(s);
        assert!(same_series(
            &p("/tv/Show S01E01.mkv"),
            &p("/tv/Show S01E02.mkv")
        ));
        assert!(same_series(
            &p("/tv/Show - 01 - Pilot.mkv"),
            &p("/tv/Show - 02 - The Return.mkv")
        ));
        assert!(!same_series(&p("/films/Alien.mkv"), &p("/films/Heat.mkv")));
    }

    #[test]
    fn chapter_names() {
        let ch = |t: f64, title: &str| {
            Value::Map(vec![
                ("time".into(), Value::Double(t)),
                ("title".into(), Value::Str(title.into())),
            ])
        };
        let list = Value::Array(vec![
            ch(0.0, "Prologue"),
            ch(90.0, "Opening"),
            ch(180.0, "Part A"),
            ch(1300.0, "Ending"),
            ch(1390.0, "Preview"),
        ]);
        let m = from_chapters(&list, 1420.0);
        assert_eq!(m.intro, Some((90.0, 180.0)));
        assert_eq!(m.credits, Some(1300.0));
        // "Opening credits" near the start is not the end credits.
        let m = from_chapters(
            &Value::Array(vec![ch(0.0, "Opening credits"), ch(100.0, "x")]),
            1400.0,
        );
        assert_eq!(m.credits, None);
    }

    /// End to end on real recordings: `SMP_INTRO_TEST=dir` holds
    /// "Show S01E01.ogg" with its intro at 30 s and "Show S01E02.ogg" with
    /// the same 25 s intro at 75 s (pure test tones don't work: Chromaprint
    /// hears pitch classes, and simple tones share them). Skipped without
    /// the files, libmpv or Chromaprint.
    #[test]
    fn listens_for_the_intro() {
        let Some(dir) = std::env::var_os("SMP_INTRO_TEST").map(PathBuf::from) else {
            eprintln!("SMP_INTRO_TEST not set; skipping");
            return;
        };
        if !available() {
            eprintln!("chromaprint not available; skipping");
            return;
        }
        let a = dir.join("Show S01E01.ogg");
        let b = dir.join("Show S01E02.ogg");
        let fa = fingerprint(&decode(&a, 0.0, 200.0).expect("decode a")).unwrap();
        let fb = fingerprint(&decode(&b, 0.0, 200.0).expect("decode b")).unwrap();
        let step = fa.1;
        for (bits, gap) in [(10, 4), (6, 28), (7, 20), (8, 12), (6, 16)] {
            let (x, y, l) = longest_match_with(&fa.0, &fb.0, bits, gap).unwrap();
            eprintln!(
                "bits {bits} gap {gap}: a={:.1} b={:.1} len={:.1}",
                x as f64 * step,
                y as f64 * step,
                l as f64 * step
            );
        }
        let (sa, sb, len) = longest_match(&fa.0, &fb.0).unwrap();
        let t = |i: usize| i as f64 * step;
        assert!((t(sa) - 30.0).abs() < 2.0, "start in a {}", t(sa));
        assert!((t(sb) - 75.0).abs() < 2.0, "start in b {}", t(sb));
        assert!((t(len) - 25.0).abs() < 3.0, "length {}", t(len));
    }
}
