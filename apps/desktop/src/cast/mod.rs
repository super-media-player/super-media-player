//! Casting to a Chromecast or a DLNA TV / speaker.
//!
//! While casting, the player here is paused and only keeps the playlist and
//! what is known about the file; its controls, the media keys and the phone
//! remote work the device instead. A file opened here goes to the device,
//! and the next one follows when it ends.
//!
//! [`tick`] compares the two sides four times a second:
//! - another file here → send it;
//! - a seek, or another sound / subtitle track here → do the same there
//!   (a converted stream is restarted at the new place);
//! - what the device says → the window's position, pause and volume.

mod chromecast;
pub mod convert;
pub mod discovery;
mod dlna;
mod server;
mod session;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use player_core::{Player, Seek, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::settings::{self, Settings};
use crate::{AppWindow, CastDevice};
use convert::{AudioTrack, Job, Plan, Quality, Source, SubTrack, Target, VideoOut, VideoTrack};
use discovery::{Device, Kind};
use server::{Content, Item, Lazy, Server};
use session::{Cmd, Media, Phase, Session};

/// A search lasts this long.
const SEARCH: Duration = Duration::from_secs(5);
/// The device should have asked for the file by now.
const NO_FETCH: Duration = Duration::from_secs(12);
/// A seek here counts once it has rested this long (dragging the seek bar).
const SETTLE: Duration = Duration::from_millis(350);
/// After a change of volume or pause here, the device's own report of it is
/// believed again after this.
const ECHO: Duration = Duration::from_millis(1800);

/// The file on the device.
struct Loaded {
    path: String,
    /// Its playlist entry (the same file can be in there twice).
    entry: i64,
    /// The tracks chosen, as mpv's ids.
    tracks: (String, String),
    source: Source,
    plan: Plan,
    attempt: u8,
    id: u64,
    /// Where the stream sent starts in the file.
    base: f64,
    at: Instant,
    /// Sent to the device (a worker may still be preparing it).
    sent: bool,
    hits_before: u64,
    warned: bool,
    failed: bool,
}

struct Active {
    device: Device,
    session: Session,
    server: Server,
    loaded: Option<Loaded>,
    /// The subtitles last read, for the next start of the same film.
    cues: Option<(String, Arc<std::sync::OnceLock<Vec<convert::Cue>>>)>,
    /// Where the next file sent starts, when not where the player here is.
    start_at: Option<f64>,
    paused: bool,
    pause_set: Option<Instant>,
    volume_set: Option<Instant>,
    /// Seeks of the paused player here (see [`player_sought`]), and until
    /// when they are its own (changing a track makes it seek).
    seeking: bool,
    sought: bool,
    not_ours: Option<Instant>,
    /// Going there, and since when.
    moved: Option<(f64, Instant)>,
    /// Told to go there, when: until it has, it still reports the old place.
    hold: Option<(f64, Instant)>,
    /// Where the device last said it was.
    playing_at: f64,
    /// The position shown.
    position: f64,
    /// Keeps the computer awake.
    #[cfg(target_os = "linux")]
    _awake: Option<zbus::blocking::Connection>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    static DEVICES: RefCell<Vec<Device>> = const { RefCell::new(Vec::new()) };
    static SEARCHING: Cell<bool> = const { Cell::new(false) };
    /// Loads count up over all sessions.
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

/// Casting now: the player's own position, pause and volume aren't what the
/// window should show.
pub fn active() -> bool {
    ACTIVE.with_borrow(Option::is_some)
}

/// Where playback is, here or on the device.
pub fn position(p: &Player) -> Option<f64> {
    ACTIVE
        .with_borrow(|a| a.as_ref().map(|a| a.position))
        .or_else(|| p.get_double("time-pos"))
}

fn with<R>(f: impl FnOnce(&mut Active) -> R) -> Option<R> {
    ACTIVE.with_borrow_mut(|a| a.as_mut().map(f))
}

// ---- the player's controls while casting (true: taken care of) ----

pub fn set_pause(ui: &AppWindow, paused: bool) -> bool {
    with(|a| {
        a.paused = paused;
        a.pause_set = Some(Instant::now());
        if !paused && a.loaded.as_ref().is_some_and(|l| l.failed) {
            // Try again.
            a.loaded = None;
            a.start_at = Some(a.position);
        }
        a.session.send(if paused { Cmd::Pause } else { Cmd::Play });
        ui.set_paused(paused);
    })
    .is_some()
}

pub fn toggle_pause(ui: &AppWindow) -> bool {
    active() && set_pause(ui, !ui.get_paused())
}

pub fn set_volume(ui: &AppWindow, percent: f32) -> bool {
    with(|a| {
        let v = percent.clamp(0.0, 100.0);
        a.volume_set = Some(Instant::now());
        a.session.send(Cmd::Volume(f64::from(v) / 100.0));
        if ui.get_muted() {
            a.session.send(Cmd::Mute(false));
            ui.set_muted(false);
        }
        ui.set_volume(v);
    })
    .is_some()
}

pub fn toggle_mute(ui: &AppWindow) -> bool {
    with(|a| {
        let m = !ui.get_muted();
        a.volume_set = Some(Instant::now());
        a.session.send(Cmd::Mute(m));
        ui.set_muted(m);
    })
    .is_some()
}

/// Seek there, once the position has rested (see [`SETTLE`]).
pub fn seek_to(ui: &AppWindow, t: f64) -> bool {
    with(|a| {
        let end = f64::from(ui.get_duration());
        // Without a length (radio) there is nowhere to go.
        if end > 0.0 {
            let t = t.clamp(0.0, (end - 1.0).max(0.0));
            a.moved = Some((t, Instant::now()));
            a.position = t;
            ui.set_position(t as f32);
        }
    })
    .is_some()
}

/// A relative seek counts from the device's position.
pub fn seek_relative(ui: &AppWindow, by: f64) -> bool {
    with(|a| a.position).is_some_and(|from| seek_to(ui, from + by))
}

/// The paused player here was sought (a chapter, a bookmark, a resumed
/// film): the device follows. `landed`: it has arrived.
pub fn player_sought(landed: bool) {
    with(|a| {
        if !landed {
            a.seeking = true;
        } else if std::mem::take(&mut a.seeking) {
            a.sought = true;
        }
    });
}

/// Speed and frame steps only work here.
pub fn refuses(ui: &AppWindow, what: &str) -> bool {
    let on = active();
    if on {
        ui.set_toast(format!("{what} doesn't work while casting").into());
    }
    on
}

// ---- setting up ----

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    let weak = ui.as_weak();
    ui.on_cast_search(move || {
        if let Some(ui) = weak.upgrade() {
            search(&ui);
        }
    });
    let (weak, p) = (ui.as_weak(), player.clone());
    ui.on_cast_connect(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let device = DEVICES.with_borrow(|d| d.get(i.max(0) as usize).cloned());
        if let Some(d) = device {
            start(&ui, &p, d);
        }
    });
    let (weak, st) = (ui.as_weak(), settings.clone());
    ui.on_cast_add(move |text| {
        let Some(ui) = weak.upgrade() else { return };
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        st.borrow_mut().cast_address = text.clone();
        st.borrow().save();
        ui.set_cast_status("Connecting…".into());
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let found = discovery::at_address(&text);
            let _ = weak.upgrade_in_event_loop(move |ui| match found {
                Ok(d) => {
                    ui.set_cast_status(SharedString::new());
                    add_device(&ui, d);
                }
                Err(e) => ui.set_cast_status(e.into()),
            });
        });
    });
    let (weak, p) = (ui.as_weak(), player.clone());
    ui.on_cast_stop(move || {
        if let Some(ui) = weak.upgrade() {
            stop(&ui, &p, "Stopped casting. Press play to go on here.");
        }
    });
    // Quality or subtitles changed: send the file again that way.
    let (weak, st) = (ui.as_weak(), settings.clone());
    ui.on_cast_options_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        {
            let mut s = st.borrow_mut();
            s.cast_quality =
                settings::CAST_QUALITY[(ui.get_cast_quality().max(0) as usize).min(2)].to_owned();
            s.cast_burn_subs = ui.get_cast_burn_subs();
            s.save();
        }
        with(|a| {
            if let Some(l) = &mut a.loaded {
                l.tracks = Default::default();
                l.attempt = 0;
            }
        });
    });
    {
        let s = settings.borrow();
        ui.set_cast_quality(
            settings::CAST_QUALITY
                .iter()
                .position(|q| *q == s.cast_quality)
                .unwrap_or(0) as i32,
        );
        ui.set_cast_burn_subs(s.cast_burn_subs);
        ui.set_cast_address(s.cast_address.as_str().into());
    }
    let (weak, p) = (ui.as_weak(), player.clone());
    crate::features::every(ui, Duration::from_millis(250), move || {
        if active()
            && let Some(ui) = weak.upgrade()
        {
            tick(&ui, &p, &settings);
        }
    });
}

fn search(ui: &AppWindow) {
    if SEARCHING.replace(true) {
        return;
    }
    DEVICES.with_borrow_mut(Vec::clear);
    show_devices(ui);
    ui.set_cast_status(SharedString::new());
    ui.set_cast_searching(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        discovery::search(SEARCH, |d| {
            let _ = weak.upgrade_in_event_loop(move |ui| add_device(&ui, d));
        });
        let _ = weak.upgrade_in_event_loop(|ui| {
            SEARCHING.set(false);
            ui.set_cast_searching(false);
        });
    });
}

fn add_device(ui: &AppWindow, d: Device) {
    DEVICES.with_borrow_mut(|list| {
        // The same Chromecast found by search and by address.
        if !list
            .iter()
            .any(|x| x.id == d.id || (x.kind == d.kind && x.addr == d.addr))
        {
            list.push(d);
            list.sort_by_key(|d| d.name.to_lowercase());
        }
    });
    show_devices(ui);
}

fn show_devices(ui: &AppWindow) {
    let current = with(|a| a.device.id.clone());
    let rows: Vec<CastDevice> = DEVICES.with_borrow(|list| {
        list.iter()
            .map(|d| CastDevice {
                name: d.name.as_str().into(),
                detail: d.detail().into(),
                sound_only: !d.video,
                current: current.as_ref() == Some(&d.id),
            })
            .collect()
    });
    ui.set_cast_devices(ModelRc::new(VecModel::from(rows)));
}

fn start(ui: &AppWindow, p: &Rc<Player>, device: Device) {
    // From one device to another: go on where that one was.
    let resume_at = with(|a| a.position);
    if active() {
        end();
    }
    let server = match Server::start(device.addr) {
        Ok(s) => s,
        Err(e) => {
            ui.set_cast_status(format!("Couldn't cast: {e}").into());
            return;
        }
    };
    let here = resume_at
        .or_else(|| p.get_double("time-pos"))
        .unwrap_or(0.0);
    let _ = p.set_pause(true);
    let session = Session::start(&device);
    ui.set_cast_name(device.name.as_str().into());
    ui.set_cast_state("Connecting…".into());
    ui.set_cast_status(SharedString::new());
    ui.set_cast_active(true);
    ui.set_paused(false);
    ACTIVE.with_borrow_mut(|a| {
        *a = Some(Active {
            device,
            session,
            server,
            loaded: None,
            cues: None,
            start_at: Some(here),
            paused: false,
            pause_set: None,
            volume_set: None,
            seeking: false,
            sought: false,
            not_ours: None,
            moved: None,
            hold: None,
            playing_at: 0.0,
            position: here,
            #[cfg(target_os = "linux")]
            _awake: stay_awake(),
        });
    });
    show_devices(ui);
}

/// End the session; the device stops.
fn end() {
    if let Some(a) = ACTIVE.with_borrow_mut(Option::take) {
        a.server.clear();
        a.session.send(Cmd::Quit);
    }
}

/// Stop casting: the player here goes to where the device was, paused.
fn stop(ui: &AppWindow, p: &Player, message: &str) {
    let Some(at) = with(|a| (a.position, a.loaded.is_some())) else {
        return;
    };
    end();
    if at.1 && !ui.get_idle() {
        let _ = p.seek(Seek::Absolute(at.0));
    }
    ui.set_cast_active(false);
    ui.set_cast_name(SharedString::new());
    ui.set_cast_state(SharedString::new());
    ui.set_paused(p.get_string("pause").as_deref() == Some("yes"));
    ui.set_volume(p.get_double("volume").unwrap_or(100.0) as f32);
    ui.set_muted(p.get_string("mute").as_deref() == Some("yes"));
    show_devices(ui);
    ui.set_toast(message.into());
}

/// The player is closing: tell the device, and put the window's own volume
/// back (it is saved).
pub fn shutdown(ui: &AppWindow, p: Option<&Player>) {
    if let Some(a) = ACTIVE.with_borrow_mut(Option::take) {
        a.server.clear();
        a.session.finish();
        if let Some(v) = p.and_then(|p| p.get_double("volume")) {
            ui.set_volume(v as f32);
        }
    }
}

// ---- keeping the two in step ----

fn tick(ui: &AppWindow, p: &Rc<Player>, settings: &RefCell<Settings>) {
    let status = match with(|a| a.session.status()) {
        Some(s) => s,
        None => return,
    };
    if let Phase::Closed(why) = &status.phase {
        let message = if why.is_empty() {
            "Casting ended".into()
        } else {
            format!("Casting ended: {why}")
        };
        stop(ui, p, &message);
        return;
    }
    if let Some(note) = with(|a| a.session.take_note()).flatten() {
        ui.set_toast(note.into());
    }
    // Opening a file un-pauses some players; here it stays paused.
    if p.get_string("pause").as_deref() == Some("no") {
        let _ = p.set_pause(true);
    }
    show_volume(ui, &status);

    let path = p
        .get_string("path")
        .filter(|s| !s.is_empty() && !ui.get_idle());
    let here = p.get_double("time-pos");
    let Some(path) = path else {
        // Nothing open here: nothing plays there either.
        let had = with(|a| {
            let had = a.loaded.take().is_some();
            if had {
                a.server.clear();
                a.session.send(Cmd::Pause);
            }
            a.moved = None;
            had
        });
        if had == Some(true) || ui.get_cast_state().is_empty() || status.phase == Phase::Ready {
            ui.set_cast_state("Connected. Open a file to play it there.".into());
        }
        return;
    };
    // Still opening.
    if here.is_none() || status.phase == Phase::Connecting {
        return;
    }
    let here = here.unwrap_or(0.0);
    let tracks = (
        p.get_string("aid").unwrap_or_default(),
        p.get_string("sid").unwrap_or_default(),
    );

    let pos = p.get_double("playlist-pos").unwrap_or(0.0).max(0.0);
    let entry = p.get_double(&format!("playlist/{pos}/id")).unwrap_or(pos) as i64;

    let same = with(|a| {
        a.loaded
            .as_ref()
            .map(|l| (l.path == path && l.entry == entry, l.tracks == tracks))
    })
    .flatten();
    match same {
        Some((true, true)) => {}
        Some((true, false)) => {
            let (at, paused, attempt) = with(|a| {
                a.not_ours = Some(Instant::now());
                (
                    a.position,
                    a.paused,
                    a.loaded.as_ref().map_or(0, |l| l.attempt),
                )
            })
            .unwrap_or_default();
            send(ui, p, settings, (path, entry), tracks, at, paused, attempt);
            return;
        }
        // Another file: it starts where the player here did (resumed or not).
        Some((false, _)) | None => {
            let start = with(|a| {
                a.seeking = false;
                a.sought = false;
                a.moved = None;
                a.hold = None;
                a.start_at.take()
            })
            .flatten()
            .unwrap_or(here);
            send(ui, p, settings, (path, entry), tracks, start, false, 0);
            return;
        }
    }

    // A seek.
    let seek = with(|a| {
        if std::mem::take(&mut a.sought)
            && a.not_ours
                .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            a.moved = Some((here, Instant::now()));
        }
        match a.moved {
            Some((t, at)) if at.elapsed() >= SETTLE => {
                a.moved = None;
                a.position = t;
                Some((
                    t,
                    a.loaded.as_ref().is_some_and(|l| l.plan.direct),
                    a.paused,
                ))
            }
            Some((t, _)) => {
                a.position = t;
                ui.set_position(t as f32);
                None
            }
            None => None,
        }
    })
    .flatten();
    if let Some((t, direct, paused)) = seek {
        ui.set_position(t as f32);
        if direct {
            with(|a| {
                a.session.send(Cmd::Seek(t));
                a.hold = Some((t, Instant::now()));
                a.pause_set = Some(Instant::now());
            });
        } else {
            let attempt = with(|a| a.loaded.as_ref().map_or(0, |l| l.attempt)).unwrap_or(0);
            send(ui, p, settings, (path, entry), tracks, t, paused, attempt);
        }
        return;
    }
    follow(ui, p, settings, &status);
}

/// Show what the device does, and act on it.
fn follow(ui: &AppWindow, p: &Rc<Player>, settings: &RefCell<Settings>, status: &session::Status) {
    let duration = p.get_double("duration").unwrap_or(0.0);
    let Some((sent, id, failed, base)) =
        with(|a| a.loaded.as_ref().map(|l| (l.sent, l.id, l.failed, l.base))).flatten()
    else {
        return;
    };
    if !sent || failed || status.id != id {
        return;
    }
    match &status.phase {
        Phase::Playing | Phase::Paused | Phase::Buffering => {
            let mut t = base + status.position_now();
            if duration > 0.0 {
                t = t.min(duration);
            }
            let moving = with(|a| {
                let old = a
                    .hold
                    .is_some_and(|(to, at)| at.elapsed() < ECHO && (to - t).abs() > 4.0);
                if !old {
                    a.hold = None;
                }
                let wait = old || a.moved.is_some();
                if !wait {
                    a.position = t;
                    a.playing_at = t;
                }
                let believe = a.pause_set.is_none_or(|s| s.elapsed() > ECHO);
                if believe && status.phase != Phase::Buffering {
                    a.paused = status.phase == Phase::Paused;
                }
                (wait, a.paused)
            });
            let Some((moving, paused)) = moving else {
                return;
            };
            if !moving {
                ui.set_position(t as f32);
                crate::state::show_lyric_line(ui, t);
            }
            if ui.get_paused() != paused {
                ui.set_paused(paused);
            }
            show_state(
                ui,
                match status.phase {
                    Phase::Paused => "Paused",
                    Phase::Buffering => "Loading",
                    _ => "Playing",
                },
            );
        }
        Phase::Loading => {
            show_state(ui, "Loading");
            let stuck = with(|a| {
                let hits = a.server.hits();
                let port = a.server.port;
                let l = a.loaded.as_mut()?;
                let stuck = !l.warned && l.at.elapsed() > NO_FETCH && hits == l.hits_before;
                l.warned |= stuck;
                stuck.then_some(port)
            });
            if let Some(Some(port)) = stuck {
                ui.set_toast(
                    format!("The device hasn't asked this computer for the file. If a firewall is on, allow TCP port {port} from the local network.").into(),
                );
            }
        }
        Phase::Ended => ended(ui, p),
        Phase::Stopped => {
            // A DLNA renderer doesn't say why it stopped: at the end it ended.
            let (at, dlna) =
                with(|a| (a.position, a.device.kind == Kind::Dlna)).unwrap_or_default();
            if dlna && duration > 0.0 && at > (duration * 0.97).min(duration - 10.0) {
                ended(ui, p);
            } else {
                stop(
                    ui,
                    p,
                    "Playback was stopped on the device. Press play to go on here.",
                );
            }
        }
        Phase::Failed(why) => {
            let again = with(|a| {
                let (at, paused) = (a.position, a.paused);
                let l = a.loaded.as_mut()?;
                Some((
                    l.attempt,
                    (l.path.clone(), l.entry),
                    l.tracks.clone(),
                    at,
                    paused,
                ))
            })
            .flatten();
            let Some((attempt, path, tracks, at, paused)) = again else {
                return;
            };
            // Some report a failure for the last moments of a stream.
            if duration > 0.0 && at > duration - 3.0 {
                ended(ui, p);
                return;
            }
            log::info!("cast: attempt {attempt}: {why}");
            let auto = settings.borrow().cast_quality == settings::CAST_QUALITY[0];
            if auto && attempt < 2 {
                send(ui, p, settings, path, tracks, at, paused, attempt + 1);
                return;
            }
            fail(ui, format!("{}: {why}", ui.get_cast_name()));
        }
        Phase::Connecting | Phase::Ready | Phase::Closed(_) => {}
    }
}

/// This file can't be cast; the session goes on (the next may).
fn fail(ui: &AppWindow, why: String) {
    with(|a| {
        if let Some(l) = &mut a.loaded {
            l.failed = true;
        }
        a.paused = true;
    });
    ui.set_paused(true);
    ui.set_cast_state(why.as_str().into());
    ui.set_toast(why.into());
}

fn show_state(ui: &AppWindow, what: &str) {
    let how = with(|a| a.loaded.as_ref().map(|l| l.plan.summary()))
        .flatten()
        .unwrap_or_default();
    let text = format!("{what}, {how}");
    if ui.get_cast_state() != text.as_str() {
        ui.set_cast_state(text.into());
    }
}

/// The device played it to the end: repeat it, or go to the next file.
fn ended(ui: &AppWindow, p: &Player) {
    // The sleep timer's "at the end of this file".
    if ui.get_sleep_choice() == 6 {
        ui.invoke_basic("sleep".into(), 0.0);
        stop(ui, p, "Sleep timer: stopped");
        return;
    }
    let again = p.get_string("loop-file").is_some_and(|v| v != "no");
    let count = p.get_double("playlist-count").unwrap_or(0.0);
    let pos = p.get_double("playlist-pos").unwrap_or(0.0);
    let looping = p.get_string("loop-playlist").is_some_and(|v| v != "no");
    if again || (looping && count <= 1.0) {
        // Sent again by the next tick.
        with(|a| {
            a.loaded = None;
            a.start_at = Some(0.0);
        });
    } else {
        // Done with; the next file is seen when the player here opens it.
        with(|a| {
            if let Some(l) = &mut a.loaded {
                l.failed = true;
            }
        });
        let last = pos + 1.0 >= count && !looping;
        // Past the last one the player here stops, like after playing it.
        let _ = p.command(&["playlist-next", if last { "force" } else { "weak" }]);
    }
}

fn show_volume(ui: &AppWindow, status: &session::Status) {
    if with(|a| a.volume_set.is_none_or(|t| t.elapsed() > ECHO)) != Some(true) {
        return;
    }
    if let Some(v) = status.volume {
        let v = (v * 100.0).round() as f32;
        if (ui.get_volume() - v).abs() > 0.5 {
            ui.set_volume(v);
        }
    }
    if let Some(m) = status.muted
        && ui.get_muted() != m
    {
        ui.set_muted(m);
    }
}

// ---- sending a file ----

#[allow(clippy::too_many_arguments)]
fn send(
    ui: &AppWindow,
    p: &Rc<Player>,
    settings: &RefCell<Settings>,
    (path, entry): (String, i64),
    tracks: (String, String),
    start: f64,
    paused: bool,
    attempt: u8,
) {
    let source = gather(ui, p, &settings.borrow(), &path);
    let (quality, burn) = {
        let s = settings.borrow();
        let q = match s.cast_quality.as_str() {
            "original" => Quality::Original,
            "convert" => Quality::Convert,
            _ => Quality::Auto,
        };
        (q, s.cast_burn_subs)
    };
    let ffmpeg = convert::program("ffmpeg").is_some() && convert::program("ffprobe").is_some();
    let id = NEXT_ID.get();
    NEXT_ID.set(id + 1);
    let planned = with(|a| {
        let sink = a.session.status().sink.unwrap_or_default();
        // A network speaker: it lists no video.
        if a.device.kind == Kind::Dlna && !sink.is_empty() {
            a.device.video = sink.iter().any(|s| s.starts_with("video/") || s == "*");
        }
        let target = Target {
            kind: a.device.kind,
            video: a.device.video,
            sink: &sink,
        };
        // The same plan as the one refused: try the one after.
        let before = a
            .loaded
            .as_ref()
            .filter(|l| l.path == path && l.attempt < attempt)
            .map(|l| l.plan.clone());
        let mut attempt = attempt;
        loop {
            let plan = convert::plan(&source, &target, quality, burn, attempt, ffmpeg);
            match &plan {
                Ok(x) if Some(x) == before.as_ref() && attempt < 2 => attempt += 1,
                _ => return plan.map(|x| (x, attempt)),
            }
        }
    });
    let Some(planned) = planned else { return };
    let from = with(|a| a.playing_at).unwrap_or(0.0);
    let (plan, attempt) = match planned {
        Ok(x) => x,
        Err(e) => {
            with(|a| {
                a.session.send(Cmd::Pause);
                a.loaded = Some(Loaded {
                    path,
                    entry,
                    tracks,
                    source,
                    plan: Plan {
                        direct: true,
                        mime: String::new(),
                        video: VideoOut::None,
                        audio: convert::AudioOut::None,
                        container: convert::Container::Mp4,
                        burn: false,
                        small: false,
                        mpv: false,
                    },
                    attempt: 2,
                    id,
                    base: 0.0,
                    at: Instant::now(),
                    sent: false,
                    hits_before: 0,
                    warned: true,
                    failed: true,
                });
            });
            fail(ui, e);
            return;
        }
    };
    let duration = source.duration;
    // A stream without an end can't be started in the middle.
    let start = if duration > 0.0 {
        start.clamp(0.0, (duration - 1.0).max(0.0))
    } else {
        0.0
    };
    let start = if start < 0.5 { 0.0 } else { start };
    log::info!(
        "cast: {} (attempt {attempt}, from {start:.1} s)",
        plan.summary()
    );
    with(|a| {
        a.server.clear();
        a.paused = paused;
        a.position = start;
        a.pause_set = Some(Instant::now());
        a.loaded = Some(Loaded {
            path: path.clone(),
            entry,
            tracks,
            source: source.clone(),
            plan: plan.clone(),
            attempt,
            id,
            base: 0.0,
            at: Instant::now(),
            sent: false,
            hits_before: a.server.hits(),
            warned: false,
            failed: false,
        });
    });
    ui.set_paused(paused);
    ui.set_position(start as f32);
    show_state(ui, "Loading");
    // A copied picture can only start at a keyframe: find it, off this thread.
    if !plan.direct && plan.video == VideoOut::Copy && start > 0.0 {
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let base = convert::keyframe_near(&source, start, from).unwrap_or(start);
            let _ = weak.upgrade_in_event_loop(move |_| hand_over(id, base, base, paused));
        });
    } else {
        hand_over(id, if plan.direct { 0.0 } else { start }, start, paused);
    }
}

/// Offer the file planned as load `id` and tell the device to fetch it.
/// `base`: where the stream begins in the file; `start`: where to play from.
fn hand_over(id: u64, base: f64, start: f64, paused: bool) {
    with(|a| {
        let Some(l) = a.loaded.as_mut().filter(|l| l.id == id && !l.sent) else {
            return;
        };
        l.sent = true;
        l.base = base;
        l.at = Instant::now();
        let (src, plan) = (l.source.clone(), l.plan.clone());
        let content = if plan.direct {
            if src.path.contains("://") {
                Content::Remote(src.path.clone())
            } else {
                Content::File(src.path.clone().into())
            }
        } else {
            Content::Live(Arc::new(Job {
                source: src.clone(),
                plan: plan.clone(),
                start: base,
            }))
        };
        // Text subtitles go along; the device draws them.
        let has_text = plan.video != VideoOut::None
            && a.device.video
            && src.video.is_some()
            && !plan.burn
            && src.sub.as_ref().is_some_and(|s| !s.is_picture());
        let (mut vtt, mut srt) = (None, None);
        if has_text {
            let key = format!("{}\n{:?}", src.path, src.sub);
            let cues = match &a.cues {
                Some((k, c)) if *k == key => c.clone(),
                _ => {
                    let c: Arc<std::sync::OnceLock<Vec<convert::Cue>>> = Arc::default();
                    a.cues = Some((key, c.clone()));
                    c
                }
            };
            let (v, s) = (Lazy::default(), Lazy::default());
            vtt = Some(a.server.add(
                &format!("{id}/subtitles.vtt"),
                Item::new(Content::Text(v.clone()), "text/vtt; charset=utf-8"),
            ));
            srt = Some(a.server.add(
                &format!("{id}/subtitles.srt"),
                Item::new(Content::Text(s.clone()), "text/srt; charset=utf-8"),
            ));
            let (src, shift) = (src.clone(), src.sub_delay - base);
            std::thread::spawn(move || {
                let list = cues.get_or_init(|| convert::read_cues(&src));
                let _ = v.set(convert::write_cues(list, shift, true).into_bytes());
                let _ = s.set(convert::write_cues(list, shift, false).into_bytes());
            });
        }
        let mut item = Item::new(content, &plan.mime);
        item.dlna = convert::dlna_features(plan.direct);
        item.srt = srt.clone();
        let features = item.dlna.clone();
        // Some TVs go by the ending of the link.
        let ext = ending(&src, &plan);
        let url = a.server.add(&format!("{id}/media.{ext}"), item);
        let art = src.art.as_ref().map(|f| {
            let png = f.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
            let (name, mime) = if png {
                ("cover.png", "image/png")
            } else {
                ("cover.jpg", "image/jpeg")
            };
            a.server.add(
                &format!("{id}/{name}"),
                Item::new(Content::File(f.clone()), mime),
            )
        });
        a.session.send(Cmd::Load(Box::new(Media {
            id,
            url,
            mime: plan.mime.clone(),
            title: src.title.clone(),
            artist: src.artist.clone(),
            album: src.album.clone(),
            art,
            vtt,
            srt,
            start: (start - base).max(0.0),
            duration: (src.duration - base).max(0.0),
            video: src.video.is_some() && a.device.video,
            paused,
            dlna: features,
        })));
    });
}

fn ending(src: &Source, plan: &Plan) -> String {
    if !plan.direct {
        return match plan.container {
            convert::Container::Mp4 => "mp4",
            convert::Container::Ts => "ts",
            convert::Container::Mp3 => "mp3",
            convert::Container::Flac => "flac",
        }
        .into();
    }
    std::path::Path::new(src.path.split('?').next().unwrap_or(""))
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".into())
}

/// What is playing, as the planner needs it.
fn gather(ui: &AppWindow, p: &Player, s: &Settings, path: &str) -> Source {
    let list = p.get_value("track-list").unwrap_or_default();
    let tracks = list.as_array();
    let text = |t: &Value, k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let int = |t: &Value, k: &str| t.get(k).and_then(Value::as_i64);
    let flag = |t: &Value, k: &str| t.get(k).and_then(Value::as_bool).unwrap_or(false);
    let of = |kind: &'static str| {
        tracks
            .iter()
            .filter(move |t| t.get("type").and_then(Value::as_str) == Some(kind))
    };
    let embedded = |kind| of(kind).filter(|t| !flag(t, "external")).count() as i64;
    let outside = |t: &Value| {
        flag(t, "external")
            .then(|| text(t, "external-filename"))
            .filter(|f| !f.is_empty())
    };
    let gamma = p.get_string("video-params/gamma").unwrap_or_default();
    let format = [
        "video-params/hw-pixelformat",
        "video-params/pixelformat",
        "video-dec-params/pixelformat",
    ]
    .iter()
    .filter_map(|n| p.get_string(n))
    .collect::<String>();
    let video = of("video")
        .find(|t| flag(t, "selected") && !flag(t, "albumart"))
        .map(|t| VideoTrack {
            id: int(t, "id").unwrap_or(1),
            index: int(t, "ff-index"),
            codec: text(t, "codec"),
            width: int(t, "demux-w").unwrap_or(0),
            height: int(t, "demux-h").unwrap_or(0),
            ten_bit: ["p010", "p10", "p012", "p12", "p016"]
                .iter()
                .any(|f| format.contains(f)),
            hdr: gamma == "pq" || gamma == "hlg",
        });
    let first = of("audio").next().and_then(|t| int(t, "id"));
    let audio = of("audio")
        .find(|t| flag(t, "selected"))
        .map(|t| AudioTrack {
            id: int(t, "id").unwrap_or(1),
            index: int(t, "ff-index"),
            codec: text(t, "codec"),
            channels: int(t, "demux-channel-count").unwrap_or(2),
            rate: int(t, "demux-samplerate").unwrap_or(0),
            first: int(t, "id") == first,
            file: outside(t),
            embedded: embedded("audio"),
        });
    let sub = of("sub")
        .find(|t| flag(t, "selected") && int(t, "main-selection").unwrap_or(0) == 0)
        .map(|t| SubTrack {
            id: int(t, "id").unwrap_or(1),
            index: int(t, "ff-index"),
            codec: text(t, "codec"),
            file: outside(t),
            embedded: embedded("sub"),
        });
    let mut site = Vec::new();
    if path.starts_with("http") {
        site.push(("ytdl".to_owned(), "yes".to_owned()));
        site.push(("ytdl-format".to_owned(), s.ytdl_format()));
        if let Some(o) = p.get_string("script-opts").filter(|o| !o.is_empty()) {
            site.push(("script-opts".to_owned(), o));
        }
    }
    let art = (!path.contains("://"))
        .then(|| crate::musicinfo::find_local_cover(path))
        .flatten()
        .filter(|_| video.is_none());
    Source {
        path: path.to_owned(),
        stream: p.get_string("stream-open-filename").unwrap_or_default(),
        format: p.get_string("file-format").unwrap_or_default(),
        duration: p.get_double("duration").unwrap_or(0.0),
        title: ui.get_media_title().into(),
        artist: ui.get_media_artist().into(),
        album: ui.get_media_album().into(),
        art,
        video,
        audio,
        sub,
        sub_delay: p.get_double("sub-delay").unwrap_or(0.0),
        site,
    }
}

/// Keep the computer from going to sleep while the device plays from it:
/// GNOME's (and others') session manager, as long as the connection lives.
#[cfg(target_os = "linux")]
fn stay_awake() -> Option<zbus::blocking::Connection> {
    // No session bus address: nothing to ask (and nothing to wait for).
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS")?;
    let c = zbus::blocking::Connection::session().ok()?;
    const SUSPEND: u32 = 4;
    c.call_method(
        Some("org.gnome.SessionManager"),
        "/org/gnome/SessionManager",
        Some("org.gnome.SessionManager"),
        "Inhibit",
        &("super-media-player", 0u32, "Casting", SUSPEND),
    )
    .map_err(|e| log::info!("cast: the desktop can't keep the computer awake: {e}"))
    .ok()?;
    Some(c)
}
