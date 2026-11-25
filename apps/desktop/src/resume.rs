//! Pick up where you left off: long videos, and long audio (podcasts,
//! audiobooks, DJ mixes), start again at the position they were left at.
//! Songs and short clips always start from the beginning.
//!
//! Positions are kept in `resume.tsv` in the data folder (`seconds<TAB>path`
//! per line, newest first, at most `MAX_ENTRIES`). A file played to (nearly)
//! the end is forgotten.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use player_core::Player;
use slint::ComponentHandle;

use crate::AppWindow;
use crate::settings::Settings;

const MAX_ENTRIES: usize = 500;
/// Shortest files remembered: videos, and audio (music albums are songs of
/// a few minutes; anything this long is a podcast, a book or a mix).
const MIN_VIDEO_SECS: f64 = 5.0 * 60.0;
const MIN_AUDIO_SECS: f64 = 20.0 * 60.0;
/// Not worth resuming this close to either end.
const HEAD_SECS: f64 = 30.0;
const TAIL_SECS: f64 = 60.0;

#[derive(Default)]
struct Positions {
    entries: Vec<(f64, String)>,
    dirty: bool,
    last_save: Option<Instant>,
}

impl Positions {
    fn load() -> Self {
        let entries = file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| parse(&t))
            .unwrap_or_default();
        Self { entries, ..Default::default() }
    }

    fn get(&self, path: &str) -> Option<f64> {
        self.entries.iter().find(|(_, p)| p == path).map(|(t, _)| *t)
    }

    fn set(&mut self, path: &str, pos: Option<f64>) {
        let before = self.entries.len();
        let old = self.get(path);
        self.entries.retain(|(_, p)| p != path);
        if let Some(t) = pos {
            self.entries.insert(0, (t, path.to_owned()));
            self.entries.truncate(MAX_ENTRIES);
        }
        if old.map(|o| (o - pos.unwrap_or(-1.0)).abs() > 0.5).unwrap_or(pos.is_some())
            || before != self.entries.len()
        {
            self.dirty = true;
        }
    }

    /// Write when changed, at most every 10 s unless `now`.
    fn save(&mut self, now: bool) {
        let due = self.last_save.is_none_or(|t| t.elapsed() > Duration::from_secs(10));
        if !self.dirty || !(now || due) {
            return;
        }
        self.dirty = false;
        self.last_save = Some(Instant::now());
        let Some(path) = file() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = self
            .entries
            .iter()
            .map(|(t, p)| format!("{t:.1}\t{p}\n"))
            .collect();
        let tmp = path.with_extension("tsv.tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path)) {
            log::warn!("could not save resume positions: {e}");
        }
    }
}

thread_local! {
    /// The positions, shared with the media library's "Continue watching".
    static POSITIONS: Rc<RefCell<Positions>> = Rc::new(RefCell::new(Positions::load()));
}

/// Where files were left (seconds, path), the latest first.
pub fn list() -> Vec<(f64, String)> {
    POSITIONS.with(|p| p.borrow().entries.clone())
}

/// Forget where a file was left.
pub fn forget(path: &str) {
    POSITIONS.with(|p| {
        let mut p = p.borrow_mut();
        p.set(path, None);
        p.save(true);
    });
}

fn file() -> Option<PathBuf> {
    crate::settings::data_dir().map(|d| d.join("resume.tsv"))
}

fn parse(text: &str) -> Vec<(f64, String)> {
    text.lines()
        .filter_map(|l| {
            let (t, p) = l.split_once('\t')?;
            Some((t.parse::<f64>().ok().filter(|t| t.is_finite())?, p.to_owned()))
        })
        .filter(|(_, p)| !p.is_empty())
        .take(MAX_ENTRIES)
        .collect()
}

/// Worth remembering: long enough, and not a disc (whose titles jump in
/// time) or an audio CD.
fn eligible(path: &str, duration: f64, video: bool) -> bool {
    let disc = ["dvd://", "dvdnav://", "bd://", "cdda://"]
        .iter()
        .any(|p| path.starts_with(p));
    !disc && duration >= if video { MIN_VIDEO_SECS } else { MIN_AUDIO_SECS }
}

/// The position to keep for `pos` in a file of `duration`: `None` (forget
/// it) near the start or the end.
fn keep(pos: f64, duration: f64) -> Option<f64> {
    (pos > HEAD_SECS && pos < duration - TAIL_SECS).then_some(pos)
}

/// Watch playback once a second: seek a newly opened file to its saved
/// position, and keep the position of the one playing.
pub fn start(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    let positions = POSITIONS.with(Rc::clone);
    // The file whose position was looked at (resumed or not) since it opened.
    let mut handled: Option<String> = None;
    let weak = ui.as_weak();
    let player = player.clone();
    {
        // Home: start over (undo a resume).
        let (player, positions) = (player.clone(), positions.clone());
        ui.on_start_over(move || {
            if let Some(path) = player.get_string("path") {
                positions.borrow_mut().set(&path, None);
            }
            let _ = player.command(&["seek", "0", "absolute"]);
        });
    }
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut pos_store = positions.borrow_mut();
        let path = player.get_string("path").unwrap_or_default();
        if ui.get_media_playing() != path.as_str() {
            ui.set_media_playing(path.as_str().into());
        }
        let duration = player.get_double("duration").unwrap_or(0.0);
        let pos = player.get_double("time-pos");
        let video = ui.get_has_video();
        if path.is_empty() || duration <= 0.0 || ui.get_idle() {
            if path.is_empty() {
                handled = None;
            }
            pos_store.save(true);
            return;
        }
        if !settings.borrow().resume || !eligible(&path, duration, video) {
            handled = Some(path);
            return;
        }
        if handled.as_deref() != Some(path.as_str()) {
            handled = Some(path.clone());
            if let Some(t) = pos_store.get(&path).and_then(|t| keep(t, duration)) {
                // Only when it really just opened (not after a manual seek).
                if pos.unwrap_or(0.0) < 10.0 {
                    let _ = player.command(&["seek", &format!("{t:.1}"), "absolute"]);
                    ui.set_toast(
                        format!("Resumed at {}. Press Home to start over.", clock(t)).into(),
                    );
                }
            }
            return;
        }
        if let Some(p) = crate::cast::position(&player) {
            pos_store.set(&path, keep(p, duration));
            pos_store.save(false);
        }
    });
    std::mem::forget(timer);
}

fn clock(t: f64) -> String {
    let t = t as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert!(eligible("/v/film.mkv", 5400.0, true));
        assert!(!eligible("/v/clip.mp4", 120.0, true));
        assert!(!eligible("/m/song.flac", 300.0, false));
        assert!(eligible("/p/podcast.mp3", 3600.0, false));
        assert!(!eligible("bd://1", 7200.0, true));
        assert_eq!(keep(10.0, 3600.0), None);
        assert_eq!(keep(1800.0, 3600.0), Some(1800.0));
        assert_eq!(keep(3580.0, 3600.0), None);
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(65.0), "1:05");
    }

    #[test]
    fn store() {
        let mut p = Positions::default();
        p.set("/a", Some(100.0));
        p.set("/b", Some(200.0));
        p.set("/a", Some(150.0));
        assert_eq!(p.entries[0], (150.0, "/a".to_owned()));
        assert_eq!(p.entries.len(), 2);
        p.set("/b", None);
        assert_eq!(p.get("/b"), None);
        let text: String = p.entries.iter().map(|(t, f)| format!("{t:.1}\t{f}\n")).collect();
        assert_eq!(parse(&text), p.entries);
        assert_eq!(parse("junk\nnan\t/x\n12.5\t/ok\n"), vec![(12.5, "/ok".to_owned())]);
    }
}
