//! Safe playback engine built on libmpv.
//!
//! [`Player`] owns one mpv instance. Commands are plain method calls; state
//! changes arrive as [`PlayerEvent`]s on a background thread through the
//! handler passed to [`Player::new`]. Video is drawn by a [`RenderContext`]
//! into an OpenGL framebuffer that the UI owns.

mod node;
mod render;
mod stream;
mod types;

use std::ffi::{CString, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use mpv_sys::*;

pub use node::Value;
pub use render::{NativeDisplay, RenderContext};
pub use stream::StreamSource;
pub use types::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Load(String),
    #[error("mpv_create failed")]
    Create,
    #[error("{context}: {message} ({code})")]
    Mpv {
        code: i32,
        message: String,
        context: String,
    },
    #[error("string contains a NUL byte: {0:?}")]
    Nul(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Options applied before mpv is initialized.
#[derive(Debug, Clone)]
pub struct PlayerConfig {
    pub options: Vec<(String, String)>,
}

impl Default for PlayerConfig {
    /// Defaults for an embedded GUI player: video through the render API,
    /// safe hardware decoding, no mpv terminal/OSC/key bindings, no user mpv config.
    fn default() -> Self {
        let opts = [
            ("vo", "libmpv"),
            ("hwdec", "auto-safe"),
            ("terminal", "no"),
            ("config", "no"),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("keep-open", "no"),
            ("idle", "yes"),
            ("ytdl", "yes"),
            ("gapless-audio", "weak"),
            ("volume-max", "150"),
            ("audio-display", "embedded-first"),
            ("screenshot-directory", "~~desktop/"),
            ("cache", "auto"),
        ];
        Self {
            options: opts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl PlayerConfig {
    /// No video or audio output. For tests and probing.
    pub fn headless() -> Self {
        let mut c = Self::default();
        c.set("vo", "null").set("ao", "null").set("ytdl", "no");
        c
    }

    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.options.retain(|(k, _)| k != key);
        self.options.push((key.to_owned(), value.to_owned()));
        self
    }
}

pub(crate) struct Inner {
    pub(crate) api: &'static Mpv,
    pub(crate) handle: *mut mpv_handle,
    quitting: AtomicBool,
}

// SAFETY: the mpv client API is fully thread-safe for a single handle.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        unsafe { (self.api.mpv_terminate_destroy)(self.handle) };
    }
}

impl Inner {
    fn check(&self, code: i32, context: impl FnOnce() -> String) -> Result<()> {
        if code >= 0 {
            return Ok(());
        }
        let message = unsafe { node::cstr_to_string((self.api.mpv_error_string)(code)) };
        Err(Error::Mpv {
            code,
            message,
            context: context(),
        })
    }
}

fn cstring(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::Nul(s.to_owned()))
}

/// How [`Player::load`] adds the file to the playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// Replace the playlist and play the file now.
    Replace,
    /// Add to the end of the playlist.
    Append,
    /// Add to the end and start playing if nothing is playing.
    AppendPlay,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seek {
    /// Seconds from the start.
    Absolute(f64),
    /// Seconds relative to the current position.
    Relative(f64),
    /// Percent of the duration.
    Percent(f64),
}

/// Property ids for `mpv_observe_property`. The order matches [`OBSERVED`].
const OBSERVED: &[(&str, mpv_format)] = &[
    ("time-pos", MPV_FORMAT_DOUBLE),
    ("duration", MPV_FORMAT_DOUBLE),
    ("pause", MPV_FORMAT_FLAG),
    ("volume", MPV_FORMAT_DOUBLE),
    ("mute", MPV_FORMAT_FLAG),
    ("media-title", MPV_FORMAT_STRING),
    ("idle-active", MPV_FORMAT_FLAG),
    ("track-list", MPV_FORMAT_NODE),
    ("playlist", MPV_FORMAT_NODE),
    ("playlist-pos", MPV_FORMAT_INT64),
    ("speed", MPV_FORMAT_DOUBLE),
    ("cache-buffering-state", MPV_FORMAT_INT64),
    ("paused-for-cache", MPV_FORMAT_FLAG),
    ("hwdec-current", MPV_FORMAT_STRING),
    ("metadata", MPV_FORMAT_NODE),
    ("loop-file", MPV_FORMAT_STRING),
    ("loop-playlist", MPV_FORMAT_STRING),
    ("path", MPV_FORMAT_STRING),
    ("audio-bitrate", MPV_FORMAT_DOUBLE),
    ("audio-params/samplerate", MPV_FORMAT_INT64),
    ("fullscreen", MPV_FORMAT_FLAG),
];

pub struct Player {
    inner: Arc<Inner>,
    event_thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Create and start an mpv instance. `on_event` is called on a background
    /// thread for every event; keep it short (forward to the UI thread).
    pub fn new(
        config: &PlayerConfig,
        on_event: impl FnMut(PlayerEvent) + Send + 'static,
    ) -> Result<Self> {
        let api = mpv_sys::load().map_err(|e| Error::Load(e.to_string()))?;
        log::info!("loaded libmpv from {}", api.path.display());

        let handle = unsafe { (api.mpv_create)() };
        if handle.is_null() {
            return Err(Error::Create);
        }
        let inner = Arc::new(Inner {
            api,
            handle,
            quitting: AtomicBool::new(false),
        });

        for (k, v) in &config.options {
            let (ck, cv) = (cstring(k)?, cstring(v)?);
            let rc = unsafe { (api.mpv_set_option_string)(handle, ck.as_ptr(), cv.as_ptr()) };
            if let Err(e) = inner.check(rc, || format!("option {k}={v}")) {
                // An unknown option on an older/newer libmpv should not stop playback.
                log::warn!("{e}");
            }
        }

        let rc = unsafe { (api.mpv_initialize)(handle) };
        inner.check(rc, || "mpv_initialize".into())?;

        for (id, (name, format)) in OBSERVED.iter().enumerate() {
            let cname = cstring(name)?;
            let rc =
                unsafe { (api.mpv_observe_property)(handle, id as u64, cname.as_ptr(), *format) };
            inner.check(rc, || format!("observe {name}"))?;
        }
        let level = cstring("warn")?;
        unsafe { (api.mpv_request_log_messages)(handle, level.as_ptr()) };

        let thread_inner = inner.clone();
        let event_thread = std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || event_loop(&thread_inner, on_event))
            .expect("spawn mpv event thread");

        Ok(Self {
            inner,
            event_thread: Some(event_thread),
        })
    }

    // -- commands ----------------------------------------------------------

    /// Run an mpv command, e.g. `["seek", "10", "relative"]`.
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect::<Result<_>>()?;
        let mut ptrs: Vec<*const std::ffi::c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let rc = unsafe { (self.inner.api.mpv_command)(self.inner.handle, ptrs.as_mut_ptr()) };
        self.inner.check(rc, || args.join(" "))
    }

    /// Run an mpv command and return its result (e.g. `screenshot-raw`).
    pub fn command_ret(&self, args: &[&str]) -> Result<Value> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect::<Result<_>>()?;
        let mut ptrs: Vec<*const std::ffi::c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let mut node = std::mem::MaybeUninit::<mpv_node>::zeroed();
        let rc = unsafe {
            (self.inner.api.mpv_command_ret)(
                self.inner.handle,
                ptrs.as_mut_ptr(),
                node.as_mut_ptr(),
            )
        };
        self.inner.check(rc, || args.join(" "))?;
        let v = unsafe { node::from_node(node.assume_init_ref()) };
        unsafe { (self.inner.api.mpv_free_node_contents)(node.as_mut_ptr()) };
        Ok(v)
    }

    /// Open a file path or URL (http, https, rtsp, dvd://, bd://, cdda://, ...).
    pub fn load(&self, path_or_url: &str, mode: LoadMode) -> Result<()> {
        let flag = match mode {
            LoadMode::Replace => "replace",
            LoadMode::Append => "append",
            LoadMode::AppendPlay => "append-play",
        };
        self.command(&["loadfile", path_or_url, flag])
    }

    /// [`load`](Self::load) with options for this file only
    /// (`("cache", "no")`). Values must not contain commas.
    pub fn load_with(&self, path_or_url: &str, options: &[(&str, &str)]) -> Result<()> {
        let opts: Vec<String> = options.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let opts = opts.join(",");
        // Since client API 2.3 (mpv 0.38) an index comes before the options.
        #[allow(clippy::useless_conversion)]
        let version = u64::from(unsafe { (self.inner.api.mpv_client_api_version)() });
        if version >= (2 << 16) | 3 {
            self.command(&["loadfile", path_or_url, "replace", "-1", &opts])
        } else {
            self.command(&["loadfile", path_or_url, "replace", &opts])
        }
    }

    pub fn set_pause(&self, paused: bool) -> Result<()> {
        self.set_flag("pause", paused)
    }

    pub fn toggle_pause(&self) -> Result<()> {
        self.command(&["cycle", "pause"])
    }

    /// Stop playback and clear the playlist.
    pub fn stop(&self) -> Result<()> {
        self.command(&["stop"])
    }

    pub fn seek(&self, seek: Seek) -> Result<()> {
        match seek {
            Seek::Absolute(s) => self.command(&["seek", &s.to_string(), "absolute"]),
            Seek::Relative(s) => self.command(&["seek", &s.to_string(), "relative"]),
            Seek::Percent(p) => self.command(&["seek", &p.to_string(), "absolute-percent"]),
        }
    }

    pub fn next(&self) -> Result<()> {
        self.command(&["playlist-next", "weak"])
    }

    pub fn prev(&self) -> Result<()> {
        self.command(&["playlist-prev", "weak"])
    }

    /// Jump to playlist entry `index` (0-based) and play it.
    pub fn play_index(&self, index: usize) -> Result<()> {
        self.command(&["playlist-play-index", &index.to_string()])
    }

    pub fn remove_index(&self, index: usize) -> Result<()> {
        self.command(&["playlist-remove", &index.to_string()])
    }

    pub fn clear_playlist(&self) -> Result<()> {
        self.command(&["playlist-clear"])
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.command(&[if on {
            "playlist-shuffle"
        } else {
            "playlist-unshuffle"
        }])
    }

    pub fn set_volume(&self, percent: f64) -> Result<()> {
        self.set_double("volume", percent)
    }

    pub fn set_mute(&self, mute: bool) -> Result<()> {
        self.set_flag("mute", mute)
    }

    pub fn set_speed(&self, speed: f64) -> Result<()> {
        self.set_double("speed", speed)
    }

    /// Select a track by id, or `None` to disable that kind of track.
    pub fn select_track(&self, kind: TrackKind, id: Option<i64>) -> Result<()> {
        let prop = match kind {
            TrackKind::Video => "vid",
            TrackKind::Audio => "aid",
            TrackKind::Subtitle => "sid",
        };
        let value = id.map_or_else(|| "no".to_owned(), |i| i.to_string());
        self.set_string(prop, &value)
    }

    /// Load an external subtitle file and select it.
    pub fn add_subtitle(&self, path: &str) -> Result<()> {
        self.command(&["sub-add", path, "select"])
    }

    pub fn set_loop_file(&self, on: bool) -> Result<()> {
        self.set_string("loop-file", if on { "inf" } else { "no" })
    }

    pub fn set_loop_playlist(&self, on: bool) -> Result<()> {
        self.set_string("loop-playlist", if on { "inf" } else { "no" })
    }

    /// Save a screenshot (with subtitles) to mpv's screenshot directory.
    pub fn screenshot(&self) -> Result<()> {
        self.command(&["screenshot", "subtitles"])
    }

    pub fn frame_step(&self, forward: bool) -> Result<()> {
        self.command(&[if forward {
            "frame-step"
        } else {
            "frame-back-step"
        }])
    }

    // -- raw properties ----------------------------------------------------

    pub fn set_string(&self, name: &str, value: &str) -> Result<()> {
        let (n, v) = (cstring(name)?, cstring(value)?);
        let rc = unsafe {
            (self.inner.api.mpv_set_property_string)(self.inner.handle, n.as_ptr(), v.as_ptr())
        };
        self.inner.check(rc, || format!("set {name}={value}"))
    }

    pub fn set_flag(&self, name: &str, value: bool) -> Result<()> {
        let n = cstring(name)?;
        let mut v: i32 = value.into();
        let rc = unsafe {
            (self.inner.api.mpv_set_property)(
                self.inner.handle,
                n.as_ptr(),
                MPV_FORMAT_FLAG,
                (&raw mut v).cast(),
            )
        };
        self.inner.check(rc, || format!("set {name}={value}"))
    }

    pub fn set_double(&self, name: &str, value: f64) -> Result<()> {
        let n = cstring(name)?;
        let mut v = value;
        let rc = unsafe {
            (self.inner.api.mpv_set_property)(
                self.inner.handle,
                n.as_ptr(),
                MPV_FORMAT_DOUBLE,
                (&raw mut v).cast(),
            )
        };
        self.inner.check(rc, || format!("set {name}={value}"))
    }

    /// Read a property as a string, e.g. `"mpv-version"`.
    pub fn get_string(&self, name: &str) -> Option<String> {
        let n = cstring(name).ok()?;
        let p = unsafe { (self.inner.api.mpv_get_property_string)(self.inner.handle, n.as_ptr()) };
        if p.is_null() {
            return None;
        }
        let s = unsafe { node::cstr_to_string(p) };
        unsafe { (self.inner.api.mpv_free)(p.cast()) };
        Some(s)
    }

    pub fn get_double(&self, name: &str) -> Option<f64> {
        let n = cstring(name).ok()?;
        let mut v = 0.0f64;
        let rc = unsafe {
            (self.inner.api.mpv_get_property)(
                self.inner.handle,
                n.as_ptr(),
                MPV_FORMAT_DOUBLE,
                (&raw mut v).cast(),
            )
        };
        (rc >= 0).then_some(v)
    }

    /// The current file's tracks.
    pub fn tracks(&self) -> Vec<Track> {
        self.get_value("track-list")
            .unwrap_or_default()
            .as_array()
            .iter()
            .filter_map(Track::from_value)
            .collect()
    }

    pub fn get_value(&self, name: &str) -> Option<Value> {
        let n = cstring(name).ok()?;
        let mut node = std::mem::MaybeUninit::<mpv_node>::zeroed();
        let rc = unsafe {
            (self.inner.api.mpv_get_property)(
                self.inner.handle,
                n.as_ptr(),
                MPV_FORMAT_NODE,
                node.as_mut_ptr().cast(),
            )
        };
        if rc < 0 {
            return None;
        }
        let v = unsafe { node::from_node(node.assume_init_ref()) };
        unsafe { (self.inner.api.mpv_free_node_contents)(node.as_mut_ptr()) };
        Some(v)
    }

    pub(crate) fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.inner.quitting.store(true, Ordering::SeqCst);
        unsafe { (self.inner.api.mpv_wakeup)(self.inner.handle) };
        if let Some(t) = self.event_thread.take() {
            let _ = t.join();
        }
        // `Inner` (and mpv) is destroyed once any RenderContext is dropped too.
    }
}

fn event_loop(inner: &Inner, mut on_event: impl FnMut(PlayerEvent)) {
    loop {
        if inner.quitting.load(Ordering::SeqCst) {
            return;
        }
        let ev = unsafe { &*(inner.api.mpv_wait_event)(inner.handle, -1.0) };
        let event = match ev.event_id {
            MPV_EVENT_NONE => continue,
            MPV_EVENT_SHUTDOWN => {
                on_event(PlayerEvent::Shutdown);
                return;
            }
            MPV_EVENT_START_FILE => PlayerEvent::StartFile,
            MPV_EVENT_FILE_LOADED => PlayerEvent::FileLoaded,
            MPV_EVENT_SEEK => PlayerEvent::Seek,
            MPV_EVENT_PLAYBACK_RESTART => PlayerEvent::PlaybackRestart,
            MPV_EVENT_END_FILE => {
                let data = unsafe { &*(ev.data as *const mpv_event_end_file) };
                let reason = match data.reason {
                    MPV_END_FILE_REASON_EOF => EndReason::Eof,
                    MPV_END_FILE_REASON_STOP => EndReason::Stop,
                    MPV_END_FILE_REASON_QUIT => EndReason::Quit,
                    MPV_END_FILE_REASON_ERROR => EndReason::Error,
                    MPV_END_FILE_REASON_REDIRECT => EndReason::Redirect,
                    _ => EndReason::Other,
                };
                let error = (reason == EndReason::Error).then(|| unsafe {
                    node::cstr_to_string((inner.api.mpv_error_string)(data.error))
                });
                PlayerEvent::EndFile { reason, error }
            }
            MPV_EVENT_LOG_MESSAGE => {
                let m = unsafe { &*(ev.data as *const mpv_event_log_message) };
                unsafe {
                    PlayerEvent::Log {
                        level: node::cstr_to_string(m.level),
                        prefix: node::cstr_to_string(m.prefix),
                        text: node::cstr_to_string(m.text).trim_end().to_owned(),
                    }
                }
            }
            MPV_EVENT_PROPERTY_CHANGE => {
                let p = unsafe { &*(ev.data as *const mpv_event_property) };
                match unsafe { property_from_event(ev.reply_userdata, p) } {
                    Some(prop) => PlayerEvent::Property(prop),
                    None => continue,
                }
            }
            _ => continue,
        };
        on_event(event);
    }
}

/// # Safety
/// `p` must be the property payload of an `MPV_EVENT_PROPERTY_CHANGE`.
unsafe fn property_from_event(id: u64, p: &mpv_event_property) -> Option<Property> {
    let name = OBSERVED.get(id as usize)?.0;
    let has = p.format != MPV_FORMAT_NONE && !p.data.is_null();
    let f64_ = || has.then(|| unsafe { *(p.data as *const f64) });
    let i64_ = || has.then(|| unsafe { *(p.data as *const i64) });
    let flag = || has && unsafe { *(p.data as *const i32) } != 0;
    let string = || {
        has.then(|| unsafe { node::cstr_to_string(*(p.data as *const *const std::ffi::c_char)) })
    };
    let value = || {
        if has {
            unsafe { node::from_node(&*(p.data as *const mpv_node)) }
        } else {
            Value::None
        }
    };

    Some(match name {
        "time-pos" => Property::TimePos(f64_()),
        "duration" => Property::Duration(f64_()),
        "pause" => Property::Pause(flag()),
        "volume" => Property::Volume(f64_().unwrap_or(100.0)),
        "mute" => Property::Mute(flag()),
        "media-title" => Property::MediaTitle(string()),
        "idle-active" => Property::Idle(flag()),
        "track-list" => Property::Tracks(
            value()
                .as_array()
                .iter()
                .filter_map(Track::from_value)
                .collect(),
        ),
        "playlist" => Property::Playlist(
            value()
                .as_array()
                .iter()
                .filter_map(PlaylistEntry::from_value)
                .collect(),
        ),
        "playlist-pos" => Property::PlaylistPos(i64_().filter(|&i| i >= 0)),
        "speed" => Property::Speed(f64_().unwrap_or(1.0)),
        "cache-buffering-state" => Property::Buffering(i64_()),
        "paused-for-cache" => Property::PausedForCache(flag()),
        "hwdec-current" => Property::HwDecoder(string().filter(|s| !s.is_empty() && s != "no")),
        "metadata" => Property::Metadata(match value() {
            Value::Map(entries) => entries
                .into_iter()
                .filter_map(|(k, v)| Some((k.to_lowercase(), v.as_str()?.to_owned())))
                .collect(),
            _ => Vec::new(),
        }),
        "loop-file" => Property::LoopFile(string().is_some_and(|s| s != "no")),
        "loop-playlist" => Property::LoopPlaylist(string().is_some_and(|s| s != "no")),
        "path" => Property::Path(string()),
        "audio-bitrate" => Property::AudioBitrate(f64_()),
        "audio-params/samplerate" => Property::SampleRate(i64_()),
        "fullscreen" => Property::Fullscreen(flag()),
        _ => return None,
    })
}

/// Raw pointer helper used by the render module.
pub(crate) fn as_void<T>(r: &mut T) -> *mut c_void {
    (r as *mut T).cast()
}
