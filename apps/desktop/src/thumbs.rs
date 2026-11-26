//! Preview pictures over the seek bar.
//!
//! A second, hidden mpv instance on its own thread opens the same file with
//! no sound, no picture output, software decoding and two decoder threads,
//! paused. Hovering the seek bar asks it for one time: it jumps to the
//! nearest keyframe (no decoding up to the exact frame), takes the frame
//! (`screenshot-raw`) scaled down to a small size, and hands it to the UI.
//!
//! What this costs the film that is playing: nothing until the pointer is
//! over the seek bar. Then one small keyframe decode per new position, at
//! most one at a time; a request that was overtaken by a newer one is
//! skipped. Pictures are kept per file, so hovering the same place again
//! costs nothing. The hidden player is shut down after a minute without use.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use player_core::{LoadMode, Player, PlayerConfig, PlayerEvent};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Width of a preview picture in pixels (its height follows the video).
pub const WIDTH: u32 = 256;
/// The hidden player is closed after this long without a request.
const IDLE_CLOSE: Duration = Duration::from_secs(60);
/// Most pictures kept for one file.
const CACHE_MAX: usize = 400;

enum Request {
    /// The file to preview (None: nothing, close it).
    Open(Option<String>),
    /// A picture of the open file at this time (seconds).
    At(f64),
}

/// A picture for the time asked for.
pub struct Thumb {
    pub time: f64,
    pub pixels: SharedPixelBuffer<Rgba8Pixel>,
}

/// The UI side: send requests, get pictures back through `deliver`.
pub struct Thumbnailer {
    tx: Sender<Request>,
    current: Option<String>,
}

impl Thumbnailer {
    /// Start the worker thread (cheap: the hidden player starts on the
    /// first request). `deliver` runs on the worker thread.
    pub fn new(deliver: impl Fn(Thumb) + Send + 'static) -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || worker(rx, deliver))
            .expect("spawn thumbnail thread");
        Self { tx, current: None }
    }

    /// Preview this file from now on (only local files: a stream would
    /// be downloaded twice).
    pub fn open(&mut self, path: Option<String>) {
        if path != self.current {
            self.current.clone_from(&path);
            let _ = self.tx.send(Request::Open(path));
        }
    }

    pub fn request(&self, time: f64) {
        if self.current.is_some() && time.is_finite() && time >= 0.0 {
            let _ = self.tx.send(Request::At(time));
        }
    }
}

/// Files the previews can come from: a local video file.
///
/// Asked several times a second by the UI's timers, so the answer for the
/// last path is remembered: on a network share (gvfs, NFS) every check is a
/// round trip, and while the server stalls the window would freeze.
pub fn previewable(path: &str) -> bool {
    thread_local! {
        static LAST: std::cell::RefCell<Option<(String, bool)>> = const { std::cell::RefCell::new(None) };
    }
    if path.contains("://") || path.is_empty() {
        return false;
    }
    if let Some(known) =
        LAST.with_borrow(|l| l.as_ref().filter(|(p, _)| p == path).map(|(_, k)| *k))
    {
        return known;
    }
    let file = std::path::Path::new(path).is_file();
    LAST.set(Some((path.to_owned(), file)));
    file
}

/// The time a picture is kept under: pictures closer than this are the same
/// keyframe anyway, nearly always.
fn bucket(t: f64) -> i64 {
    (t / 2.0).round() as i64
}

pub(crate) struct Hidden {
    pub(crate) player: Player,
    events: Receiver<PlayerEvent>,
    /// The file is open and its first frame decoded.
    ready: bool,
}

impl Hidden {
    /// Open `path`; its pictures will be `width` pixels wide.
    pub(crate) fn start(path: &str, width: u32) -> Option<Self> {
        Self::open(path, &format!("scale=w={width}:h=-2"))
    }

    /// Open `path`, its frames going through the filters `vf`.
    pub(crate) fn open(path: &str, vf: &str) -> Option<Self> {
        let (tx, events) = channel();
        let mut cfg = PlayerConfig::headless();
        for (k, v) in [
            ("pause", "yes"),
            ("aid", "no"),
            ("sid", "no"),
            ("hwdec", "no"),
            ("vd-lavc-threads", "2"),
            ("vd-lavc-skiploopfilter", "all"),
            ("vd-lavc-fast", "yes"),
            ("hr-seek", "no"),
            ("cache", "no"),
            ("demuxer-readahead-secs", "0"),
            ("keep-open", "always"),
            ("load-scripts", "no"),
            ("sub-auto", "no"),
            ("audio-file-auto", "no"),
            ("resume-playback", "no"),
            ("vf", vf),
        ] {
            cfg.set(k, v);
        }
        let player = Player::new(&cfg, move |e| {
            let _ = tx.send(e);
        })
        .map_err(|e| log::debug!("thumbnails: {e}"))
        .ok()?;
        player.load(path, LoadMode::Replace).ok()?;
        let mut h = Self {
            player,
            events,
            ready: false,
        };
        h.ready = h.wait_restart(Duration::from_secs(5));
        h.ready.then_some(h)
    }

    /// Wait until mpv has a frame after opening or seeking.
    fn wait_restart(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(PlayerEvent::PlaybackRestart) => return true,
                Ok(PlayerEvent::EndFile { .. }) | Ok(PlayerEvent::Shutdown) => return false,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    /// Go to a time, and wait for its frame.
    pub(crate) fn seek(&self, time: f64, mode: &str) -> bool {
        // Events of earlier seeks still queued would answer this one.
        while self.events.try_recv().is_ok() {}
        self.player
            .command(&["seek", &format!("{time:.2}"), mode])
            .is_ok()
            && self.wait_restart(Duration::from_secs(3))
    }

    pub(crate) fn grab(&self, time: f64) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        if !self.seek(time, "absolute+keyframes") {
            return None;
        }
        let shot = self.player.command_ret(&["screenshot-raw", "video"]).ok()?;
        let w = shot.get("w")?.as_i64()? as usize;
        let h = shot.get("h")?.as_i64()? as usize;
        let stride = shot.get("stride")?.as_i64()? as usize;
        let data = shot.get("data")?.as_bytes()?;
        if shot.str_field("format").as_deref() != Some("bgr0") || w == 0 || h == 0 {
            return None;
        }
        to_rgba(data, w, h, stride)
    }
}

/// mpv's `bgr0` rows (with `stride` bytes each) to RGBA.
fn to_rgba(
    data: &[u8],
    w: usize,
    h: usize,
    stride: usize,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    if stride < w * 4 || data.len() < stride * (h - 1) + w * 4 {
        return None;
    }
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
    let out = buf.make_mut_slice();
    for y in 0..h {
        let row = &data[y * stride..y * stride + w * 4];
        for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
            out[y * w + x] = Rgba8Pixel {
                r: px[2],
                g: px[1],
                b: px[0],
                a: 255,
            };
        }
    }
    Some(buf)
}

fn worker(rx: Receiver<Request>, deliver: impl Fn(Thumb)) {
    let mut path: Option<String> = None;
    let mut hidden: Option<Hidden> = None;
    let mut failed = false;
    let mut cache: HashMap<i64, SharedPixelBuffer<Rgba8Pixel>> = HashMap::new();
    loop {
        let first = match rx.recv_timeout(IDLE_CLOSE) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => {
                // Unused for a while: free the hidden player's memory.
                hidden = None;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Only the newest time matters; a file change applies in order.
        let mut want = None;
        for r in std::iter::once(first).chain(rx.try_iter()) {
            match r {
                Request::Open(p) => {
                    if p != path {
                        path = p;
                        hidden = None;
                        failed = false;
                        cache.clear();
                    }
                    want = None;
                }
                Request::At(t) => want = Some(t),
            }
        }
        let (Some(t), Some(p)) = (want, path.as_deref()) else {
            continue;
        };
        if let Some(pixels) = cache.get(&bucket(t)) {
            deliver(Thumb {
                time: t,
                pixels: pixels.clone(),
            });
            continue;
        }
        if hidden.is_none() && !failed {
            hidden = Hidden::start(p, WIDTH);
            // A file the hidden player can't open won't open next time either.
            failed = hidden.is_none();
        }
        let Some(h) = &hidden else { continue };
        if let Some(pixels) = h.grab(t) {
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
            cache.insert(bucket(t), pixels.clone());
            deliver(Thumb { time: t, pixels });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bgr0_rows() {
        // 2x1 picture with 4 bytes of row padding.
        let data = [1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9];
        let buf = to_rgba(&data, 2, 1, 12).unwrap();
        let px = buf.as_slice();
        assert_eq!((px[0].r, px[0].g, px[0].b), (3, 2, 1));
        assert_eq!((px[1].r, px[1].g, px[1].b), (6, 5, 4));
        assert!(to_rgba(&data, 4, 1, 12).is_none());
        assert_eq!(bucket(10.2), bucket(10.9));
        assert_ne!(bucket(10.0), bucket(13.0));
        assert!(!previewable("https://example.com/a.mp4"));
    }
}
