//! What both kinds of device have in common: the commands they take, and
//! what they say about themselves. Each session runs on its own thread.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::discovery::{Device, Kind};

/// What to play, in the device's own terms.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    /// Counts up with every load; the status says which one it is about.
    pub id: u64,
    pub url: String,
    pub mime: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art: Option<String>,
    /// WebVTT and SRT links of the subtitles.
    pub vtt: Option<String>,
    pub srt: Option<String>,
    /// Where to start, on the device's clock.
    pub start: f64,
    pub duration: f64,
    pub video: bool,
    pub paused: bool,
    /// DLNA's fourth protocolInfo field.
    pub dlna: String,
}

#[derive(Clone, Debug)]
pub enum Cmd {
    Load(Box<Media>),
    Play,
    Pause,
    Seek(f64),
    /// 0..1
    Volume(f64),
    Mute(bool),
    /// Stop playing there and end the session.
    Quit,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Connecting,
    /// Connected, nothing loaded.
    Ready,
    Loading,
    Buffering,
    Playing,
    Paused,
    /// Played to its end.
    Ended,
    /// Stopped from elsewhere (the TV's own remote, another app).
    Stopped,
    /// This file couldn't be played there; the session goes on.
    Failed(String),
    /// The session is over.
    Closed(String),
}

#[derive(Clone, Debug)]
pub struct Status {
    pub phase: Phase,
    /// The load this is about.
    pub id: u64,
    /// Where it was at `at`, on the device's clock.
    pub position: f64,
    pub at: Instant,
    /// 0..1
    pub volume: Option<f64>,
    pub muted: Option<bool>,
    /// Content types a DLNA renderer plays, once asked.
    pub sink: Option<Vec<String>>,
    /// A command that failed, to be shown once.
    pub note: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            phase: Phase::Connecting,
            id: 0,
            position: 0.0,
            at: Instant::now(),
            volume: None,
            muted: None,
            sink: None,
            note: None,
        }
    }
}

impl Status {
    /// Where it is now.
    pub fn position_now(&self) -> f64 {
        self.position
            + if self.phase == Phase::Playing {
                self.at.elapsed().as_secs_f64()
            } else {
                0.0
            }
    }
}

pub type Shared = Arc<Mutex<Status>>;

pub fn update(s: &Shared, f: impl FnOnce(&mut Status)) {
    if let Ok(mut s) = s.lock() {
        f(&mut s);
    }
}

/// A connection to a device. Dropping it stops playback there.
pub struct Session {
    tx: Sender<Cmd>,
    status: Shared,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    pub fn start(device: &Device) -> Self {
        let (tx, rx): (_, Receiver<Cmd>) = channel();
        let status = Shared::default();
        let (st, d) = (status.clone(), device.clone());
        let thread = std::thread::Builder::new()
            .name("cast".into())
            .spawn(move || {
                let result = match d.kind {
                    Kind::Chromecast => super::chromecast::run(&d, &rx, &st),
                    Kind::Dlna => super::dlna::run(&d, &rx, &st),
                };
                let why = result.err().unwrap_or_default();
                if !why.is_empty() {
                    log::warn!("cast to {}: {why}", d.name);
                }
                update(&st, |s| s.phase = Phase::Closed(why));
            })
            .ok();
        Self { tx, status, thread }
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// The note, taken out.
    pub fn take_note(&self) -> Option<String> {
        self.status.lock().ok()?.note.take()
    }

    /// End it and wait for the device to be told (a moment at most).
    pub fn finish(mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let end = Instant::now() + std::time::Duration::from_millis(1500);
            while !t.is_finished() && Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
    }
}
