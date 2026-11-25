//! How fast things play, remembered: the speed chosen for a file, and
//! whether its silence is skipped (silence.rs), come back with it.
//!
//! Something not seen before plays at normal speed, except that
//! - an episode of a podcast starts the way its show was last played, and
//! - what comes from the same place (folder, disc, site) as the file before
//!   it goes on the same way, so a speed set for a lecture lasts for the
//!   rest of the course.
//!
//! Kept in `pace.tsv` in the data folder (`speed<TAB>0|1<TAB>path or show`
//! per line, newest first, at most `MAX_ENTRIES`); normal ones aren't.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use player_core::Player;
use slint::ComponentHandle;

use crate::settings::Settings;
use crate::{AppWindow, cast, silence};

const MAX_ENTRIES: usize = 500;
const MAX_KEY: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pace {
    speed: f64,
    skip: bool,
}

const NORMAL: Pace = Pace { speed: 1.0, skip: false };

impl Pace {
    fn normal(self) -> bool {
        (self.speed - 1.0).abs() < 0.001 && !self.skip
    }
}

#[derive(Default)]
struct Store {
    entries: Vec<(Pace, String)>,
    /// What plays, and where it is from.
    current: Option<(String, String)>,
    /// The show (feed address) of episodes played from the Library.
    shows: HashMap<String, String>,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store { entries: load(), ..Default::default() });
}

fn file() -> Option<PathBuf> {
    crate::settings::data_dir().map(|d| d.join("pace.tsv"))
}

fn load() -> Vec<(Pace, String)> {
    file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()
}

fn parse(text: &str) -> Vec<(Pace, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let speed = it.next()?.parse::<f64>().ok().filter(|s| (0.25..=4.0).contains(s))?;
            let skip = it.next()? == "1";
            Some((Pace { speed, skip }, it.next()?.to_owned()))
        })
        .filter(|(_, k)| !k.is_empty())
        .take(MAX_ENTRIES)
        .collect()
}

fn to_text(entries: &[(Pace, String)]) -> String {
    entries.iter().map(|(p, k)| format!("{}\t{}\t{k}\n", p.speed, u8::from(p.skip))).collect()
}

/// Where a file is from: a podcast's show, a folder, a site, a kind of disc.
fn place(path: &str, shows: &HashMap<String, String>) -> String {
    if let Some(feed) = shows.get(path) {
        return format!("show:{feed}");
    }
    match path.split_once("://") {
        Some(("file", p)) => place(p, shows),
        Some((scheme, rest)) if scheme.starts_with("http") => {
            format!("site:{}", rest.split(['/', '?', '#']).next().unwrap_or(rest))
        }
        // Shares, as folders. A disc's titles and menus belong together.
        Some(("dvd" | "dvdnav" | "smpdvd", _)) => "disc:dvd".into(),
        Some((scheme @ ("bd" | "cdda" | "edl"), _)) => format!("disc:{scheme}"),
        Some((scheme, rest)) => format!("{scheme}:{}", rest.rsplit_once('/').map_or("", |(d, _)| d)),
        None => format!("folder:{}", Path::new(path).parent().map(Path::to_string_lossy).unwrap_or_default()),
    }
}

/// Discs have no name of their own to remember.
fn can_remember(path: &str, place: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_KEY && !path.contains(['\t', '\n', '\r']) && !place.starts_with("disc:")
}

impl Store {
    fn get(&self, key: &str) -> Option<Pace> {
        self.entries.iter().find(|(_, k)| k == key).map(|(p, _)| *p)
    }

    /// The pace for `path`, opened while `now` is in use. None: no change.
    fn open(&mut self, path: &str, now: Pace) -> Option<Pace> {
        let place = place(path, &self.shows);
        let before = self.current.replace((path.to_owned(), place.clone()));
        let same_place = before.is_some_and(|(_, p)| p == place);
        let known = self.get(path).or_else(|| self.get(&place));
        let want = known.unwrap_or(if same_place { now } else { NORMAL });
        (want != now).then_some(want)
    }

    /// The user chose `pace` for what plays. True when the list changed.
    fn choose(&mut self, pace: Pace) -> bool {
        let Some((path, place)) = self.current.clone() else { return false };
        if !can_remember(&path, &place) {
            return false;
        }
        let old = self.entries.clone();
        let show = place.starts_with("show:").then_some(place);
        for key in [Some(path), show].into_iter().flatten() {
            self.entries.retain(|(_, k)| *k != key);
            if !pace.normal() {
                self.entries.insert(0, (pace, key));
            }
        }
        self.entries.truncate(MAX_ENTRIES);
        old != self.entries
    }

    fn save(&self) {
        let Some(path) = file() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tsv.tmp");
        if let Err(e) = std::fs::write(&tmp, to_text(&self.entries)).and_then(|()| std::fs::rename(&tmp, &path)) {
            log::warn!("could not save playing speeds: {e}");
        }
    }
}

/// `url` is an episode of the podcast with this feed.
pub fn episode_of(url: &str, feed: &str) {
    if !feed.is_empty() {
        STORE.with_borrow_mut(|s| s.shows.insert(url.to_owned(), feed.to_owned()));
    }
}

fn now(ui: &AppWindow) -> Pace {
    Pace { speed: (f64::from(ui.get_speed()) * 100.0).round() / 100.0, skip: ui.get_skip_silence() }
}

fn describe(p: Pace) -> String {
    match (p.speed, p.skip) {
        (_, true) if (p.speed - 1.0).abs() < 0.001 => "Skipping silence, as before".into(),
        (s, true) => format!("Speed {s}× and skipping silence, as before"),
        (s, false) => format!("Speed {s}×, as before"),
    }
}

/// A file starts: give it its pace.
pub fn opened(ui: &AppWindow, player: &Player) {
    let Some(path) = player.get_string("path").filter(|p| !p.is_empty()) else { return };
    let now = now(ui);
    let want = STORE.with_borrow_mut(|s| s.open(&path, now));
    // Advanced navigation and casting have their own idea of speed.
    let Some(want) = want.filter(|_| ui.get_s_remember_pace() && !ui.get_s_nav_enabled() && !cast::active()) else { return };
    if (want.speed - now.speed).abs() > 0.001 {
        let _ = player.set_speed(want.speed);
    }
    ui.set_skip_silence(want.skip);
    if !want.normal() {
        ui.set_toast(describe(want).into());
    }
}

fn choose(ui: &AppWindow, pace: Pace) {
    if ui.get_s_remember_pace() && STORE.with_borrow_mut(|s| s.choose(pace)) {
        STORE.with_borrow(Store::save);
    }
}

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    let (weak, p) = (ui.as_weak(), player.clone());
    ui.on_set_speed(move |speed| {
        let Some(ui) = weak.upgrade() else { return };
        if cast::refuses(&ui, "Playing at another speed") {
            return;
        }
        match p.set_speed(f64::from(speed)) {
            Ok(()) => choose(&ui, Pace { speed: (f64::from(speed) * 100.0).round() / 100.0, ..now(&ui) }),
            Err(e) => crate::report(&ui, Err(e)),
        }
    });
    let weak = ui.as_weak();
    ui.on_toggle_skip_silence(move || {
        let Some(ui) = weak.upgrade() else { return };
        let on = !ui.get_skip_silence();
        let why_not = silence::obstacle(&ui, settings.borrow().bit_perfect);
        if let (true, Some(why)) = (on, why_not) {
            ui.set_toast(why.into());
            return;
        }
        ui.set_skip_silence(on);
        choose(&ui, now(&ui));
        let saved = ui.get_silence_saved();
        ui.set_toast(match (on, saved.is_empty()) {
            (true, _) => "Skipping silence".into(),
            (false, true) => "Silence is played again".into(),
            (false, false) => format!("Silence is played again ({saved} skipped)").into(),
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: Pace = Pace { speed: 1.5, skip: true };

    #[test]
    fn places() {
        let shows = HashMap::from([("https://cdn.x/ep1.mp3".to_owned(), "https://feed.x/rss".to_owned())]);
        let p = |s| place(s, &shows);
        assert_eq!(p("/v/course/01.mp4"), "folder:/v/course");
        assert_eq!(p("file:///v/course/01.mp4"), "folder:/v/course");
        assert_eq!(p("https://cdn.x/ep1.mp3"), "show:https://feed.x/rss");
        assert_eq!(p("https://www.youtube.com/watch?v=1"), "site:www.youtube.com");
        assert_eq!(p("smpdvd://menu"), p("dvd://1"));
        assert_ne!(p("bd://1"), p("dvd://1"));
        assert!(!can_remember("dvd://1", "disc:dvd"));
        assert!(!can_remember("/a\tb", "folder:/"));
        assert!(can_remember("/a b.mp3", "folder:/"));
    }

    #[test]
    fn remembers_the_file() {
        let mut s = Store::default();
        assert_eq!(s.open("/v/a.mp4", NORMAL), None);
        assert!(s.choose(FAST));
        // The next of the same folder goes on; elsewhere it is normal again.
        assert_eq!(s.open("/v/b.mp4", FAST), None);
        assert_eq!(s.open("/music/song.mp3", FAST), Some(NORMAL));
        assert_eq!(s.open("/v/b.mp4", NORMAL), None);
        assert_eq!(s.open("/v/a.mp4", NORMAL), Some(FAST));
        // Back to normal: forgotten.
        assert!(s.choose(NORMAL));
        assert!(s.entries.is_empty());
        assert!(!s.choose(NORMAL));
    }

    #[test]
    fn remembers_the_show() {
        let mut s = Store::default();
        s.shows.insert("http://x/1.mp3".into(), "feed".into());
        s.shows.insert("http://x/2.mp3".into(), "feed".into());
        s.open("http://x/1.mp3", NORMAL);
        s.choose(FAST);
        s.open("/v/film.mkv", FAST);
        assert_eq!(s.open("http://x/2.mp3", NORMAL), Some(FAST));
        assert_eq!(parse(&to_text(&s.entries)), s.entries);
    }

    #[test]
    fn bad_lines() {
        assert!(parse("x\t1\t/a\n9\t0\t/b\nnan\t0\t/c\n1.5\t1\n1.5\t1\t\n\n").is_empty());
        assert_eq!(parse("1.5\t1\t/a\tb"), vec![(FAST, "/a\tb".to_owned())]);
    }
}
