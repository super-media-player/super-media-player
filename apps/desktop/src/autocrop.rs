//! Crop black bars: the bars that are part of a video (a wide film on a
//! 16:9 disc, a 4:3 programme with bars at its sides) are found and left
//! out, so the picture itself fits the window.
//!
//! What is kept is every place where a picture was ever seen, so a film
//! that opens up for some scenes keeps those whole. That is learned in one
//! of two ways, both with FFmpeg's `cropdetect`:
//!
//! - A file on disk is looked through by a hidden player, at moments spread
//!   over all of it, in a second or two; the result is kept on disk.
//! - A disc or stream can't be: a filter looks at two frames a second as
//!   they play. The crop comes once the bars have stayed the same for a
//!   while, and opens at once when a picture shows up in them. All frames
//!   pass that filter untouched, on the graphics card if they are there;
//!   only copies of those two come back from it, in a branch of their own
//!   whose findings are written to a small file.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use player_core::{Player, Value};
use slint::ComponentHandle;

use crate::AppWindow;
use crate::thumbs::{self, Hidden};

const LABEL: &str = "bars";
/// A line whose average is below this is black (8-bit video: black is 16).
const LIMIT: &str = "24/255";
/// Moments of a file looked at.
const MOMENTS: usize = 24;
/// Up to this length the exact moment is decoded, not the keyframe before.
const SHORT: f64 = 40.0;
/// Seconds the bars stay the same before they are cropped while playing.
const SETTLE: f64 = 10.0;
const POLL: Duration = Duration::from_millis(200);
/// Seconds between the frames looked at while playing.
const EVERY: f64 = 0.5;

/// Where the picture is in a frame, in pixels as stored in the file: left
/// and top edge, and the first pixel past the right and bottom edge.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
}

/// The size of a frame, and how wide its pixels are shown.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    w: i64,
    h: i64,
    sar: f64,
}

impl Area {
    /// From `cropdetect`'s findings. None: a black frame, or none yet.
    fn read(meta: &Value) -> Option<Self> {
        Self::from(|k| meta.str_field(k)?.parse().ok())
    }

    /// From the last of its findings in a file written by `metadata`.
    fn last_in(text: &str) -> Option<Self> {
        text.rsplit("frame:").find_map(|block| {
            // The last line may not be all there yet.
            let lines = &block[..=block.rfind('\n')?];
            Self::from(|k| lines.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')?.parse().ok()))
        })
    }

    fn from(get: impl Fn(&str) -> Option<i64>) -> Option<Self> {
        let n = |k: &str| get(&format!("lavfi.cropdetect.{k}"));
        let a = Self { x1: n("x1")?, y1: n("y1")?, x2: n("x2")? + 1, y2: n("y2")? + 1 };
        (a.x1 >= 0 && a.y1 >= 0 && a.x2 - a.x1 > 8 && a.y2 - a.y1 > 8).then_some(a)
    }

    fn join(self, o: Self) -> Self {
        Self { x1: self.x1.min(o.x1), y1: self.y1.min(o.y1), x2: self.x2.max(o.x2), y2: self.y2.max(o.y2) }
    }

    fn w(self) -> i64 {
        self.x2 - self.x1
    }

    fn h(self) -> i64 {
        self.y2 - self.y1
    }

    fn fills(self, f: Frame) -> bool {
        (self.w(), self.h()) == (f.w, f.h)
    }

    /// mpv's `video-crop`.
    fn geometry(self) -> String {
        format!("{}x{}+{}+{}", self.w(), self.h(), self.x1, self.y1)
    }
}

/// One direction of a frame `size` long with a picture from `a` to `b`: what
/// to keep of it.
fn keep(a: i64, b: i64, size: i64) -> (i64, i64) {
    let (mut before, mut after) = (a.clamp(0, size), (size - b).clamp(0, size));
    // Bars come in pairs. One much wider than the other: something dark at
    // that side of the picture, or only part of the bars is free.
    if (before - after).abs() * 25 > size {
        before = before.min(after);
        after = before;
    }
    // Not worth it: the odd lines at the edge of old recordings.
    if (before + after) * 200 < size * 3 {
        return (0, size);
    }
    // Whole chroma samples; inwards, the edge lines are often half black.
    (before + (before & 1), size - after - (after & 1))
}

/// What to show of a frame whose pictures are within `area`. None: all.
fn crop(area: Area, f: Frame) -> Option<Area> {
    let (x1, x2) = keep(area.x1, area.x2, f.w);
    let (y1, y2) = keep(area.y1, area.y2, f.h);
    let c = Area { x1, y1, x2, y2 };
    if c.fills(f) || c.w() * 5 < f.w || c.h() * 5 < f.h {
        return None;
    }
    // Bars on all sides: only around a shape that pictures come in. A logo
    // or a title on black isn't the picture.
    if c.w() < f.w && c.h() < f.h {
        let aspect = c.w() as f64 * f.sar / c.h() as f64;
        let known = [4.0 / 3.0, 1.375, 1.5, 1.66, 16.0 / 9.0, 1.85, 2.0, 2.2, 2.35, 2.39, 2.55];
        let near = known.iter().any(|k| (aspect / k).ln().abs() < 0.025);
        if !near || c.w() * 2 < f.w || c.h() * 2 < f.h {
            return None;
        }
    }
    Some(c)
}

/// `cropdetect`, for each frame alone or for all so far.
fn detector(each: bool) -> String {
    format!("cropdetect=limit={LIMIT}:round=2:skip=0:reset={}", u8::from(each))
}

/// The filter for watching; `hw`: the format of frames on the graphics card.
fn watcher(hw: Option<&str>, log: &str) -> String {
    let download = hw.map(|f| format!("hwdownload,format={f},")).unwrap_or_default();
    format!(
        "@{LABEL}:lavfi=[split[m][s];[s]select='isnan(prev_selected_t)+gte(t-prev_selected_t,{EVERY})',{download}{},\
         metadata=mode=print:direct=1:file='{log}',nullsink;[m]null]",
        detector(false)
    )
}

/// What `watcher` writes to, for this player.
fn log_file() -> Option<PathBuf> {
    let f = crate::settings::cache_dir()?.join(format!("bars-{}.log", std::process::id()));
    // It is named inside a filter graph.
    let plain = |c: char| !matches!(c, '\'' | '\\' | ':' | ',' | ';' | '[' | ']' | '%');
    f.to_str().is_some_and(|s| s.chars().all(plain)).then_some(f)
}

/// At the end: this player's file, and any left by one that crashed.
pub fn tidy_up() {
    const OLD: Duration = Duration::from_secs(24 * 3600);
    let Some(own) = log_file() else { return };
    let Some(files) = own.parent().and_then(|d| std::fs::read_dir(d).ok()) else { return };
    for f in files.flatten() {
        let name = f.file_name();
        let name = name.to_string_lossy();
        let old = || f.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|e| e > OLD));
        if name.starts_with("bars-") && name.ends_with(".log") && (f.path() == own || old()) {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

// ---- a whole file ----

/// Look through a file. None: it can't be read, or there's nothing to see.
fn scan(path: &str, cancel: &AtomicBool) -> Option<(Frame, Area)> {
    let h = Hidden::open(path, &format!("@{LABEL}:lavfi=[{}]", detector(true)))?;
    let duration = h.player.get_double("duration").filter(|d| *d > 0.0)?;
    let frame = frame(&h.player)?;
    let mode = if duration <= SHORT { "absolute+exact" } else { "absolute+keyframes" };
    let mut found: Option<Area> = None;
    let mut seen = 0;
    for i in 0..MOMENTS {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // Not the very ends: logos, and black.
        let t = duration * (0.03 + 0.94 * (i as f64 + 0.5) / MOMENTS as f64);
        if !h.seek(t, mode) {
            continue;
        }
        if let Some(a) = metadata(&h.player).as_ref().and_then(Area::read) {
            found = Some(found.map_or(a, |f| f.join(a)));
            seen += 1;
            // Pictures everywhere already: the rest can't change that.
            if seen >= 3 && found.is_some_and(|a| a.fills(frame)) {
                break;
            }
        }
    }
    (seen >= 3).then_some((frame, found?))
}

fn metadata(p: &Player) -> Option<Value> {
    p.get_value(&format!("vf-metadata/{LABEL}"))
}

fn frame(p: &Player) -> Option<Frame> {
    let d = |k: &str| p.get_double(&format!("video-params/{k}")).filter(|v| *v > 0.0);
    let (w, h, dw, dh) = (d("w")?, d("h")?, d("dw")?, d("dh")?);
    Some(Frame { w: w as i64, h: h as i64, sar: (dw / dh) / (w / h) })
}

/// Results for files, newest first: size, date, frame, area, path.
struct Cache(Vec<(String, Frame, Area)>);

const CACHE_MAX: usize = 1000;

impl Cache {
    fn file() -> Option<PathBuf> {
        crate::settings::cache_dir().map(|d| d.join("bars.tsv"))
    }

    fn key(path: &str) -> Option<String> {
        let m = std::fs::metadata(Path::new(path)).ok()?;
        let date = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        (!path.contains(['\t', '\n', '\r'])).then(|| format!("{}\t{date}\t{path}", m.len()))
    }

    fn parse(text: &str) -> Self {
        let line = |l: &str| {
            let mut it = l.splitn(8, '\t');
            let mut n = || it.next()?.parse::<i64>().ok();
            let (w, h) = (n()?, n()?);
            let area = Area { x1: n()?, y1: n()?, x2: n()?, y2: n()? };
            let sar = it.next()?.parse().ok()?;
            Some((it.next()?.to_owned(), Frame { w, h, sar }, area))
        };
        Self(text.lines().filter_map(line).take(CACHE_MAX).collect())
    }

    fn text(&self) -> String {
        let line = |(k, f, a): &(String, Frame, Area)| {
            format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{k}\n", f.w, f.h, a.x1, a.y1, a.x2, a.y2, f.sar)
        };
        self.0.iter().map(line).collect()
    }

    fn get(&self, key: &str) -> Option<(Frame, Area)> {
        self.0.iter().find(|e| e.0 == key).map(|e| (e.1, e.2))
    }

    fn put(&mut self, key: String, frame: Frame, area: Area) {
        self.0.retain(|e| e.0 != key);
        self.0.insert(0, (key, frame, area));
        self.0.truncate(CACHE_MAX);
        let Some(path) = Self::file() else { return };
        let tmp = path.with_extension("tsv.tmp");
        if let Err(e) = std::fs::write(&tmp, self.text()).and_then(|()| std::fs::rename(&tmp, &path)) {
            log::debug!("bars.tsv: {e}");
        }
    }
}

// ---- what plays ----

#[derive(Default)]
enum Source {
    #[default]
    New,
    Scanning(Receiver<Option<(Frame, Area)>>, Arc<AtomicBool>),
    /// Looked at as it plays: seconds without news.
    Watching(f64),
    Done,
}

#[derive(Default)]
struct State {
    path: String,
    source: Source,
    /// What was found, for frames of this size.
    found: Option<(Frame, Area)>,
    /// `video-crop` as set for this file.
    applied: String,
    /// The filter for watching is there, for frames of this format.
    filtering: Option<Option<String>>,
    /// How much it had written.
    written: u64,
    stretching: bool,
    /// Just switched on: say what comes of it.
    tell: bool,
    was_on: Option<bool>,
    cache: Option<Cache>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// mpv starts a file, and has dropped the options of the one before.
pub fn file_started() {
    STATE.with_borrow_mut(|s| s.applied.clear());
}

/// What is shown of the picture, when it is cropped.
pub fn shown() -> Option<String> {
    STATE.with_borrow(|s| shown_of(&s.applied))
}

/// A place across the shown picture (0..1) as one across the whole frame,
/// which is what shaders see.
pub fn across_frame(x: f64) -> f64 {
    STATE.with_borrow(|s| match s.found {
        Some((f, a)) if !s.applied.is_empty() => {
            crop(a, f).map_or(x, |c| (c.x1 as f64 + x * c.w() as f64) / f.w as f64)
        }
        _ => x,
    })
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Self::Scanning(_, cancel) = self {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl State {
    fn set_crop(&mut self, p: &Player, want: String) {
        if self.applied != want {
            match p.set_string("file-local-options/video-crop", &want) {
                Ok(()) => log::info!("black bars: showing {}", if want.is_empty() { "everything" } else { &want }),
                Err(e) => log::warn!("video-crop={want}: {e}"),
            }
            self.applied = want;
        }
    }

    /// Put the filter on (again, when the frames' format has changed).
    fn start_filter(&mut self, p: &Player) {
        let hw = p.get_string("video-params/hw-pixelformat").filter(|f| !f.is_empty());
        if self.filtering.as_ref() == Some(&hw) {
            return;
        }
        self.stop_filter(p);
        if let Some(log) = log_file() {
            self.written = 0;
            if let Err(e) = p.command(&["vf", "add", &watcher(hw.as_deref(), &log.to_string_lossy())]) {
                log::warn!("black bars: {e}");
            }
        }
        self.filtering = Some(hw);
    }

    fn stop_filter(&mut self, p: &Player) {
        if self.filtering.take().is_some() {
            let _ = p.command(&["vf", "remove", &format!("@{LABEL}")]);
            if let Some(log) = log_file() {
                let _ = std::fs::remove_file(log);
            }
        }
    }

    /// What the filter found, when there is news of it.
    fn news(&mut self) -> Option<Area> {
        let log = log_file()?;
        let len = std::fs::metadata(&log).ok()?.len();
        if std::mem::replace(&mut self.written, len) == len {
            return None;
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&log).ok()?;
        f.seek(SeekFrom::Start(len.saturating_sub(1500))).ok()?;
        let mut text = String::new();
        f.take(4096).read_to_string(&mut text).ok()?;
        Area::last_in(&text)
    }

    /// Picture subtitles are drawn for the whole frame, and mpv squeezes
    /// them into what is shown of it. Where the window has about the shape
    /// of the whole frame, they go where they were meant to be.
    fn place_subtitles(&mut self, p: &Player) {
        let want = !self.applied.is_empty()
            && (|| {
                if p.get_double("video-params/rotate").is_some_and(|r| r % 180.0 != 0.0) {
                    return None;
                }
                let (f, a) = self.found?;
                let c = crop(a, f)?;
                let whole = f.w as f64 * f.sar / f.h as f64;
                let shown = c.w() as f64 * f.sar / c.h() as f64;
                let (w, h) = (p.get_double("osd-dimensions/w")?, p.get_double("osd-dimensions/h")?);
                (w > 0.0 && h > 0.0).then(|| (w / h / whole).ln().abs() + 0.02 < (shown / whole).ln().abs())
            })()
            .unwrap_or(false);
        if self.stretching != want {
            self.stretching = want;
            let _ = p.set_flag("stretch-image-subs-to-screen", want);
        }
    }

    fn reset(&mut self, p: &Player) {
        self.set_crop(p, String::new());
        self.stop_filter(p);
        self.place_subtitles(p);
        self.source = Source::New;
    }

    fn look(&mut self, ui: &AppWindow, p: &Player) {
        let on = ui.get_s_auto_crop();
        if self.was_on != Some(on) {
            // Not at startup.
            self.tell = on && self.was_on.is_some();
            self.was_on = Some(on);
            self.path.clear();
            if !on {
                if !self.applied.is_empty() {
                    ui.set_toast("Black bars shown".into());
                }
                self.reset(p);
                self.found = None;
            }
        }
        if !on {
            return;
        }
        // Options of the picture are only touched while one shows: mpv would
        // wait for its window otherwise (see features.rs).
        if !ui.get_has_video() {
            return;
        }
        let path = p.get_string("path").unwrap_or_default();
        if path != self.path {
            // Another file than it was switched on for.
            self.tell &= self.path.is_empty();
            self.path = path;
            self.found = None;
            self.reset(p);
        }
        let menu = crate::dvdmenu::menu_active() || self.path.starts_with("smpdvd://");
        let Some(frame) = (!menu && video_track(p)).then(|| frame(p)).flatten() else {
            if menu {
                self.reset(p);
            }
            return;
        };
        self.learn(p, frame);
        // Found in frames of another size: the stream changed, or another
        // video track was chosen.
        let settled = matches!(self.source, Source::Done) || matches!(self.source, Source::Watching(t) if t >= SETTLE);
        let fits = self.found.filter(|(f, _)| (f.w, f.h) == (frame.w, frame.h));
        let want = fits.and_then(|(f, a)| crop(a, f)).map(Area::geometry).unwrap_or_default();
        // Opening up doesn't wait.
        if settled || !self.applied.is_empty() {
            self.set_crop(p, want);
        }
        self.place_subtitles(p);
        if self.tell && settled {
            self.tell = false;
            ui.set_toast(match shown_of(&self.applied) {
                Some(size) => format!("Black bars cropped: {size}").into(),
                None => "No black bars in this video".into(),
            });
        }
    }

    fn learn(&mut self, p: &Player, frame: Frame) {
        match &mut self.source {
            Source::New => {
                let local = thumbs::previewable(&self.path);
                if local {
                    let cache = self.cache.get_or_insert_with(|| {
                        Cache::parse(&Cache::file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default())
                    });
                    if let Some(hit) = Cache::key(&self.path).and_then(|k| cache.get(&k)) {
                        self.found = Some(hit);
                        self.source = Source::Done;
                        return;
                    }
                    let (tx, rx) = channel();
                    let cancel = Arc::new(AtomicBool::new(false));
                    let (path, c) = (self.path.clone(), cancel.clone());
                    let spawned = std::thread::Builder::new().name("black bars".into()).spawn(move || {
                        let _ = tx.send(scan(&path, &c));
                    });
                    if spawned.is_ok() {
                        self.source = Source::Scanning(rx, cancel);
                        return;
                    }
                }
                self.source = Source::Watching(0.0);
            }
            Source::Scanning(rx, _) => match rx.try_recv() {
                Ok(Some((f, a))) => {
                    self.found = Some((f, a));
                    if let (Some(c), Some(k)) = (&mut self.cache, Cache::key(&self.path)) {
                        c.put(k, f, a);
                    }
                    self.source = Source::Done;
                }
                // Not a file that can be jumped about in.
                Ok(None) | Err(TryRecvError::Disconnected) => self.source = Source::Watching(0.0),
                Err(TryRecvError::Empty) => {}
            },
            Source::Watching(_) => {
                self.start_filter(p);
                let Some(a) = self.news() else { return };
                let mut quiet = match self.source {
                    Source::Watching(q) => q,
                    _ => 0.0,
                };
                let same_size = self.found.filter(|(f, _)| (f.w, f.h) == (frame.w, frame.h));
                let joined = same_size.map_or(a, |(_, old)| old.join(a));
                if same_size.is_none_or(|(_, old)| old != joined) {
                    // A line more or less is noise, not a picture.
                    let grown = same_size.is_none_or(|(f, old)| crop(old, f) != crop(joined, frame));
                    if grown {
                        quiet = 0.0;
                    }
                    self.found = Some((frame, joined));
                } else {
                    quiet += EVERY;
                }
                self.source = Source::Watching(quiet);
                // Pictures everywhere: nothing more to learn.
                if joined.fills(frame) {
                    self.stop_filter(p);
                    self.source = Source::Done;
                }
            }
            Source::Done => {}
        }
    }
}

fn shown_of(geometry: &str) -> Option<String> {
    geometry.split_once('+').map(|(size, _)| size.replace('x', " × "))
}

/// A moving picture, not a cover or a music visualizer.
fn video_track(p: &Player) -> bool {
    let t = p.get_value("current-tracks/video");
    t.is_some_and(|t| t.get("id").is_some() && !t.bool_field("albumart") && !t.bool_field("image"))
}

pub fn install(ui: &AppWindow, player: &Rc<Player>) {
    let (weak, p) = (ui.as_weak(), player.clone());
    crate::features::every(ui, POLL, move || {
        if let Some(ui) = weak.upgrade() {
            STATE.with_borrow_mut(|s| s.look(&ui, &p));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: Frame = Frame { w: 1920, h: 1080, sar: 1.0 };

    fn area(x1: i64, y1: i64, x2: i64, y2: i64) -> Area {
        Area { x1, y1, x2, y2 }
    }

    fn cropped(a: Area, f: Frame) -> Option<String> {
        crop(a, f).map(Area::geometry)
    }

    #[test]
    fn crops_bars() {
        assert_eq!(cropped(area(0, 140, 1920, 940), HD).as_deref(), Some("1920x800+0+140"));
        assert_eq!(cropped(area(240, 0, 1680, 1080), HD).as_deref(), Some("1440x1080+240+0"));
        // Odd edges go inwards.
        assert_eq!(cropped(area(0, 139, 1920, 941), HD).as_deref(), Some("1920x800+0+140"));
        // All sides: 16:9 inside 4:3 inside 16:9.
        assert_eq!(cropped(area(240, 135, 1680, 945), HD).as_deref(), Some("1440x808+240+136"));
        // A phone's video: tall.
        assert_eq!(cropped(area(656, 0, 1264, 1080), HD).as_deref(), Some("608x1080+656+0"));
        // Anamorphic DVD, 4:3 pixels.
        let dvd = Frame { w: 720, h: 576, sar: 64.0 / 45.0 };
        assert_eq!(cropped(area(0, 74, 720, 502), dvd).as_deref(), Some("720x428+0+74"));
    }

    #[test]
    fn leaves_pictures_alone() {
        assert_eq!(cropped(area(0, 0, 1920, 1080), HD), None);
        // The odd line at the edge.
        assert_eq!(cropped(area(4, 2, 1916, 1078), HD), None);
        // Dark at one side.
        assert_eq!(cropped(area(700, 0, 1920, 1080), HD), None);
        // Subtitles burned into the lower bar: the upper bar stays too.
        assert_eq!(cropped(area(0, 140, 1920, 1060), HD).as_deref(), Some("1920x1040+0+20"));
        // A logo, and a line of text.
        assert_eq!(cropped(area(800, 450, 1120, 630), HD), None);
        assert_eq!(cropped(area(300, 500, 1620, 580), HD), None);
        assert_eq!(cropped(area(0, 500, 1920, 580), HD), None);
    }

    #[test]
    fn keeps_every_picture() {
        // A film that opens up for some scenes.
        let a = area(0, 140, 1920, 940).join(area(0, 26, 1920, 1054));
        assert_eq!(cropped(a, HD).as_deref(), Some("1920x1028+0+26"));
    }

    #[test]
    fn reads_what_the_filter_writes() {
        let block = |n: u32, y: i64| {
            format!("frame:{n}    pts:1 pts_time:1\nlavfi.cropdetect.x1=0\nlavfi.cropdetect.x2=1919\nlavfi.cropdetect.y1={y}\nlavfi.cropdetect.y2=939\nlavfi.cropdetect.w=1920\n")
        };
        let two = block(0, 150) + &block(1, 140);
        assert_eq!(Area::last_in(&two), Some(area(0, 140, 1920, 940)));
        // Half written, and cut off at the start.
        let cut = &two[two.find("y2").unwrap()..two.rfind("39\nlavfi.cropdetect.w").unwrap()];
        assert_eq!(Area::last_in(cut), None);
        let cut = &two[..two.rfind("39\nlavfi.cropdetect.w").unwrap()];
        assert_eq!(Area::last_in(cut), Some(area(0, 150, 1920, 940)));
        assert_eq!(Area::last_in(""), None);
        assert!(watcher(Some("p010"), "/x/y.log").contains(",hwdownload,format=p010,cropdetect=limit=24/255:round=2:skip=0:reset=0,metadata"));
        assert!(!watcher(None, "/x/y.log").contains("hwdownload"));
    }

    #[test]
    fn cache_round_trip() {
        let mut c = Cache(vec![("1\t2\t/v/a b.mkv".into(), HD, area(0, 140, 1920, 940))]);
        c.0.push(("3\t4\t/v/c.mkv".into(), Frame { w: 720, h: 576, sar: 64.0 / 45.0 }, area(1, 2, 3, 4)));
        let d = Cache::parse(&c.text());
        assert_eq!(d.0, c.0);
        assert_eq!(d.get("1\t2\t/v/a b.mkv"), Some((HD, area(0, 140, 1920, 940))));
        assert_eq!(Cache::parse("junk\n1\t2\n").0.len(), 0);
    }

    /// The real thing on a made-up film: dark at first, and wider in one part.
    #[test]
    fn scans_a_file() {
        let dir = std::env::temp_dir().join(format!("smp-bars-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, graph: &str| {
            let out = dir.join(name);
            let ok = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", graph, "-c:v", "mpeg4", "-q:v", "4", "-g", "10"])
                .arg(&out)
                .status()
                .is_ok_and(|s| s.success());
            ok.then(|| out.to_string_lossy().into_owned())
        };
        let film = make("film.mkv", "testsrc2=s=640x268:r=10:d=30,pad=640:360:0:46,drawbox=c=black:t=fill:enable='lt(t,6)'");
        let opens = make(
            "opens.mkv",
            "testsrc2=s=640x360:r=10:d=30,drawbox=h=46:c=black:t=fill:enable='lt(t,20)',drawbox=y=314:h=46:c=black:t=fill:enable='lt(t,20)'",
        );
        let usable = Player::new(&player_core::PlayerConfig::headless(), |_| {}).is_ok();
        if let (Some(film), Some(opens), true) = (film, opens, usable) {
            let no = AtomicBool::new(false);
            let (f, a) = scan(&film, &no).expect("film");
            assert_eq!((f.w, f.h), (640, 360));
            assert_eq!(cropped(a, f).as_deref(), Some("640x268+0+46"));
            let (f, a) = scan(&opens, &no).expect("opens");
            assert_eq!(cropped(a, f), None);
        } else {
            eprintln!("no libmpv or ffmpeg, skipped");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
