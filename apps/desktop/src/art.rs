//! Pictures for the media library: album covers, artists, video frames.
//!
//! Only what is on screen is asked for. Two low-priority threads (covers
//! are quick, a video frame needs a hidden decoder) answer the newest
//! request first, so scrolling fast doesn't wait for what scrolled past.
//! Small copies are kept on disk; in memory only the last `KEEP` pictures,
//! and none while the library is closed.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::medialib::Db;
use crate::thumbs::Hidden;

/// Pictures held in memory (about 50 MB at most).
const KEEP: usize = 220;
const COVER: u32 = 240;
const FRAME: u32 = 320;
/// Largest picture decoded (a hostile file could claim any size).
const MAX_SIDE: u32 = 12_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// The cover of the album this song is on.
    Cover(String),
    /// An artist, with one of their songs; may look online.
    Artist {
        name: String,
        path: String,
        online: bool,
    },
    /// A frame of a video.
    Video(String),
}

type Arrived = std::rc::Rc<dyn Fn(&Key)>;
type Pixels = SharedPixelBuffer<Rgba8Pixel>;

struct Queue {
    list: Mutex<Vec<Key>>,
    wake: Condvar,
}

impl Queue {
    const fn new() -> Self {
        Self {
            list: Mutex::new(Vec::new()),
            wake: Condvar::new(),
        }
    }

    fn push(&self, key: Key) {
        self.list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(key);
        self.wake.notify_one();
    }

    /// The newest request; waits for one.
    fn pop(&self) -> Key {
        let mut list = self.list.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(k) = list.pop() {
                return k;
            }
            list = self.wake.wait(list).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn clear(&self) {
        self.list.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

static COVERS: Queue = Queue::new();
static FRAMES: Queue = Queue::new();

#[derive(Default)]
struct Store {
    /// None: there is no picture.
    have: HashMap<Key, Option<Image>>,
    order: VecDeque<Key>,
    asked: HashSet<Key>,
    /// Told (on the UI thread) when a picture arrives.
    arrived: Option<Arrived>,
    started: bool,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

/// `arrived` runs on the UI thread for each picture that was asked for.
pub fn on_arrival(arrived: impl Fn(&Key) + 'static) {
    STORE.with_borrow_mut(|s| s.arrived = Some(std::rc::Rc::new(arrived)));
}

/// The picture, if it is in memory; otherwise it is asked for.
pub fn get(key: &Key) -> Option<Image> {
    STORE.with_borrow_mut(|s| {
        if let Some(found) = s.have.get(key) {
            return found.clone();
        }
        if s.asked.insert(key.clone()) {
            if !s.started {
                s.started = true;
                start("library-covers", &COVERS);
                start("library-frames", &FRAMES);
            }
            if matches!(key, Key::Video(_)) {
                &FRAMES
            } else {
                &COVERS
            }
            .push(key.clone());
        }
        None
    })
}

/// The library was closed: free the pictures, drop what is waiting.
pub fn clear() {
    COVERS.clear();
    FRAMES.clear();
    STORE.with_borrow_mut(|s| {
        s.have.clear();
        s.order.clear();
        s.asked.clear();
    });
}

fn start(name: &str, queue: &'static Queue) {
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            low_priority();
            loop {
                let key = queue.pop();
                // A broken picture mustn't end the thread.
                let k = key.clone();
                let pixels = std::panic::catch_unwind(move || load(&k)).ok().flatten();
                let _ = slint::invoke_from_event_loop(move || deliver(key, pixels));
            }
        });
    if let Err(e) = spawned {
        log::warn!("{name}: {e}");
    }
}

fn deliver(key: Key, pixels: Option<Pixels>) {
    let arrived = STORE.with_borrow_mut(|s| {
        if !s.asked.remove(&key) {
            return None; // the library was closed meanwhile
        }
        s.have.insert(key.clone(), pixels.map(Image::from_rgba8));
        s.order.push_back(key.clone());
        while s.order.len() > KEEP {
            if let Some(old) = s.order.pop_front() {
                s.have.remove(&old);
            }
        }
        s.arrived.clone()
    });
    if let Some(f) = arrived {
        f(&key);
    }
}

/// Let playback go first (Linux: the nice value of this thread alone).
pub fn low_priority() {
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn setpriority(which: u32, who: u32, prio: i32) -> i32;
        }
        // SAFETY: plain system call; 0, 0 is PRIO_PROCESS of the caller,
        // which on Linux is the calling thread.
        unsafe { setpriority(0, 0, 15) };
    }
}

// ---- on the threads ----

fn stamp(path: &str) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The small copy's file, named after what it shows and its time, so a
/// changed file gets a new picture.
fn cache_file(key: &Key) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let text = match key {
        Key::Cover(p) => format!("cover\n{p}\n{}", stamp(p)),
        Key::Artist { name, path, online } => format!("artist\n{name}\n{path}\n{online}"),
        Key::Video(p) => format!("video\n{p}\n{}", stamp(p)),
    };
    let hash = Sha256::digest(text.as_bytes());
    let name: String = hash[..12].iter().map(|b| format!("{b:02x}")).collect();
    Some(
        crate::settings::cache_dir()?
            .join("library")
            .join(format!("{name}.jpg")),
    )
}

fn load(key: &Key) -> Option<Pixels> {
    let cache = cache_file(key);
    if let Some(meta) = cache.as_deref().and_then(|c| std::fs::metadata(c).ok()) {
        if crate::artist::fresh_miss(&meta) {
            return None;
        }
        if meta.len() > 0
            && let Some(img) = cache
                .as_deref()
                .and_then(|c| std::fs::read(c).ok())
                .and_then(|b| decode(&b))
        {
            return Some(pixels(&img.to_rgba8()));
        }
    }
    // A file that isn't there now (an unplugged drive) says nothing lasting.
    let there = match key {
        Key::Cover(p) | Key::Video(p) | Key::Artist { path: p, .. } => Path::new(p).exists(),
    };
    if !there {
        return None;
    }
    let img = match key {
        Key::Cover(p) => cover(p).map(|i| i.thumbnail(COVER, COVER)),
        Key::Artist { name, path, online } => artist(name, path, *online)
            .map(|i| i.resize_to_fill(COVER, COVER, image::imageops::FilterType::Triangle)),
        Key::Video(p) => frame(p),
    };
    if let Some(file) = cache {
        let bytes = img.as_ref().and_then(encode).unwrap_or_default();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = file.with_extension("tmp");
        let _ = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &file));
    }
    img.map(|i| pixels(&i.to_rgba8()))
}

fn decode(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    reader.decode().ok()
}

fn decode_file(path: &Path) -> Option<image::DynamicImage> {
    let size = std::fs::metadata(path).ok()?.len();
    (size < 40 * 1024 * 1024)
        .then(|| std::fs::read(path).ok())
        .flatten()
        .and_then(|b| decode(&b))
}

fn encode(img: &image::DynamicImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 86);
    img.to_rgb8().write_with_encoder(enc).ok()?;
    Some(out)
}

fn pixels(img: &image::RgbaImage) -> Pixels {
    Pixels::clone_from_slice(img.as_raw(), img.width(), img.height())
}

/// The cover in the song's tags, else a cover.jpg next to it.
fn cover(path: &str) -> Option<image::DynamicImage> {
    use lofty::prelude::*;
    if crate::medialib::is_audio(Path::new(path))
        && let Ok(tagged) = lofty::read_from_path(path)
        && let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag())
    {
        let pics = tag.pictures();
        let front = pics
            .iter()
            .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
            .or_else(|| pics.first());
        if let Some(img) = front.and_then(|p| decode(p.data())) {
            return Some(img);
        }
    }
    decode_file(&crate::musicinfo::find_local_cover(path)?)
}

fn artist(name: &str, path: &str, online: bool) -> Option<image::DynamicImage> {
    let local = crate::artist::find_local(path).filter(|p| {
        p.file_stem()
            .is_some_and(|s| s.to_string_lossy().to_lowercase().starts_with("artist"))
    });
    local
        .and_then(|p| decode_file(&p))
        .or_else(|| {
            online
                .then(|| crate::artist::fetch_online(name))
                .flatten()
                .and_then(|p| decode_file(&p))
        })
        .or_else(|| cover(path))
}

/// A frame some way into the video, past logos and fades. Its length is
/// kept too: it shows on the card and in "Continue watching".
fn frame(path: &str) -> Option<image::DynamicImage> {
    let hidden = Hidden::start(path, FRAME)?;
    let duration = hidden.player.get_double("duration").unwrap_or(0.0);
    let height = hidden.player.get_double("height").unwrap_or(0.0) as i64;
    if duration > 0.0
        && let Err(e) = Db::open().and_then(|db| db.set_probe(path, duration, height))
    {
        log::debug!("{path}: {e}");
    }
    let shot = hidden.grab(duration * 0.12)?;
    let raw = shot.as_bytes().to_vec();
    image::RgbaImage::from_raw(shot.width(), shot.height(), raw).map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_round_trip_and_bad_ones_are_refused() {
        let img: image::DynamicImage =
            image::RgbaImage::from_pixel(600, 300, image::Rgba([200, 10, 10, 255])).into();
        let small = img.thumbnail(COVER, COVER);
        assert_eq!((small.width(), small.height()), (240, 120));
        let back = decode(&encode(&small).unwrap()).unwrap();
        let px = pixels(&back.to_rgba8());
        assert_eq!((px.width(), px.height()), (240, 120));
        assert!(px.as_slice()[0].r > 180 && px.as_slice()[0].g < 40);
        assert!(decode(b"not a picture").is_none());
        // A PNG that claims to be 60000 pixels wide.
        let mut png = Vec::from(*b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        png.extend(60_000u32.to_be_bytes());
        png.extend(10u32.to_be_bytes());
        png.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        assert!(decode(&png).is_none());
    }

    #[test]
    fn newest_request_first() {
        let q = Queue::new();
        q.push(Key::Cover("a".into()));
        q.push(Key::Cover("b".into()));
        assert_eq!(q.pop(), Key::Cover("b".into()));
        q.clear();
        q.push(Key::Video("c".into()));
        assert_eq!(q.pop(), Key::Video("c".into()));
    }
}
