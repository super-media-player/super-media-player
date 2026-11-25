//! DVD menus. The disc's program runs in [`vm`] (libdvdnav); its menus reach
//! mpv as a stream of our own (`smpdvd://`), with the highlighted button laid
//! over the picture ([`spu`]). Titles play through mpv's own DVD support.
//!
//! The program is a little ahead of the picture (mpv reads about a second
//! ahead), so buttons are kept with their times. Everything that may wait
//! for the drive runs on a worker thread.

mod spu;
mod vm;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use player_core::{EndReason, Player, TrackKind, Value};
use slint::ComponentHandle;

use crate::{AppWindow, disc};
use vm::{Action, Halt, Machine, Menu, Moment, Outcome, Reader, Shared, TitleStart};

pub use vm::available;

/// The protocol mpv reads menus through.
pub const PROTOCOL: &str = "smpdvd";
/// mpv's overlay slot for the highlight.
const OVERLAY: &str = "61";
const TICK: Duration = Duration::from_millis(60);

/// Options of the menu stream: start at once, stay close to the disc's
/// program, and keep the last picture up when the stream stops (a still).
const MENU_OPTIONS: &[(&str, &str)] = &[
    ("cache", "no"),
    ("demuxer-readahead-secs", "0.4"),
    ("demuxer-lavf-format", "mpeg"),
    ("demuxer-lavf-probesize", "800000"),
    ("demuxer-lavf-analyzeduration", "0.6"),
    ("stream-buffer-size", "65536"),
    ("keep-open", "always"),
    ("keep-open-pause", "no"),
    ("loop-file", "no"),
    ("sid", "no"),
    ("start", "none"),
];

enum Command {
    Act(Action, Option<Moment>),
    Quit,
}

/// What mpv is playing for this disc.
#[derive(Clone, PartialEq, Debug)]
enum Playing {
    /// The disc is being opened.
    Nothing,
    Menu,
    Title(TitleStart),
}

struct Session {
    device: String,
    name: String,
    machine: Option<Shared>,
    commands: Sender<Command>,
    playing: Playing,
    /// Where the film was left for the menu.
    left_at: Option<(TitleStart, f64)>,
    /// A position to go to once the title has loaded.
    seek_to: Option<f64>,
    /// The stream halted and its last picture is up, since then.
    still_since: Option<Instant>,
    /// The overlay now shown was made for this.
    drawn: Option<DrawKey>,
    /// The buttons on screen.
    moment: Option<Moment>,
    place: Option<spu::Place>,
    /// Chosen by the mouse or a key before the machine says so.
    asked: i32,
    /// A button was pressed just now: it looks so until this passes.
    pressed_until: Option<Instant>,
    /// A first title was found at once: no menu has been shown yet.
    loading: bool,
}

#[derive(Clone, PartialEq)]
struct DrawKey {
    button: i32,
    moment: i64,
    picture: Option<(i64, u16, u16)>,
    place: spu::Place,
    pressed: bool,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static TIMER: slint::Timer = slint::Timer::default();
    static WINDOW: RefCell<Option<(slint::Weak<AppWindow>, Rc<Player>)>> = const { RefCell::new(None) };
    /// Sessions are numbered, so that late answers of an old one are ignored.
    static NUMBER: Cell<u64> = const { Cell::new(0) };
}

/// The machine behind `smpdvd://` links, for mpv's threads.
static CURRENT: Mutex<Option<Shared>> = Mutex::new(None);

pub fn install(ui: &AppWindow, player: &Rc<Player>) {
    WINDOW.with_borrow_mut(|w| *w = Some((ui.as_weak(), player.clone())));
    let notify: Arc<dyn Fn() + Send + Sync> = {
        let weak = ui.as_weak();
        Arc::new(move || {
            let _ = weak.upgrade_in_event_loop(|ui| tick(&ui));
        })
    };
    let added = player.add_protocol(PROTOCOL, move |_| {
        let machine = CURRENT.lock().ok()?.clone()?;
        let generation = machine.lock().ok()?.generation;
        Some(Box::new(Reader::new(machine, generation, notify.clone())))
    });
    if let Err(e) = added {
        log::warn!("DVD menus: {e}");
    }
    let weak = ui.as_weak();
    ui.on_dvd(move |what| {
        if let Some(ui) = weak.upgrade() {
            command(&ui, &what);
        }
    });
    let weak = ui.as_weak();
    ui.on_dvd_mouse(move |x, y, press| {
        if let Some(ui) = weak.upgrade() {
            mouse(&ui, x, y, press);
        }
    });
}

fn with_player<R>(f: impl FnOnce(&Player) -> R) -> Option<R> {
    let player = WINDOW.with_borrow(|w| w.as_ref().map(|(_, p)| p.clone()))?;
    Some(f(&player))
}

/// The language of the desktop, for the disc's menus ("en").
fn language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .map(|v| v.chars().take(2).collect::<String>().to_lowercase())
        .filter(|v| v.len() == 2 && v.chars().all(|c| c.is_ascii_lowercase()))
        .unwrap_or_else(|| "en".into())
}

/// Whether a path belongs to this module.
pub fn is_menu_path(path: &str) -> bool {
    path.starts_with(PROTOCOL)
}

/// A disc's menu (or one of its short clips) is on screen.
pub fn menu_active() -> bool {
    SESSION.with_borrow(|s| s.as_ref().is_some_and(|s| s.playing == Playing::Menu))
}

/// Start a DVD with its own program: usually logos, then the main menu.
/// `at_menu`: straight to the menu, from title `title`.
pub fn start(ui: &AppWindow, device: &str, at_menu: Option<i32>) {
    end(ui);
    let number = NUMBER.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    let (tx, rx) = channel();
    SESSION.with_borrow_mut(|s| {
        *s = Some(Session {
            device: device.to_owned(),
            name: String::new(),
            machine: None,
            commands: tx,
            playing: Playing::Nothing,
            left_at: None,
            seek_to: None,
            still_since: None,
            drawn: None,
            moment: None,
            place: None,
            asked: 0,
            pressed_until: None,
            loading: true,
        });
    });
    ui.set_dvd_disc(true);
    ui.set_toast("Opening the DVD's menu…".into());
    let device = device.to_owned();
    let weak = ui.as_weak();
    let spawned = std::thread::Builder::new().name("dvd-menu".into()).spawn(move || {
        let first = Machine::open(&device, &language()).map(|mut m| {
            let mut outcome = m.advance();
            if let Some(title) = at_menu {
                if title > 0 {
                    m.enter_title(title);
                    m.advance();
                }
                outcome = m.act(Action::Menu(if title > 0 { Menu::Root } else { Menu::Title }), None);
            }
            (m, outcome)
        });
        match first {
            Ok((m, outcome)) => {
                let name = m.name();
                let machine: Shared = Arc::new(Mutex::new(m));
                let m = machine.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| opened(&ui, number, m, name, outcome));
                work(&machine, &rx, &weak, number);
            }
            Err(e) => {
                log::warn!("DVD menu: {e}");
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    if NUMBER.get() == number {
                        no_menu(&ui, "Couldn't open the DVD's menu: playing the main film");
                    }
                });
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("DVD menu: {e}");
        no_menu(ui, "Couldn't open the DVD's menu: playing the main film");
    }
}

/// The worker: what the user does, run against the disc.
fn work(machine: &Shared, rx: &Receiver<Command>, weak: &slint::Weak<AppWindow>, number: u64) {
    while let Ok(Command::Act(action, moment)) = rx.recv() {
        let Ok(mut m) = machine.lock() else { return };
        let outcome = m.act(action, moment.as_ref());
        drop(m);
        log::debug!("dvd: {action:?} → {outcome:?}");
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if NUMBER.get() == number {
                after(&ui, outcome);
            }
        });
    }
}

fn opened(ui: &AppWindow, number: u64, machine: Shared, name: String, outcome: Outcome) {
    if NUMBER.get() != number {
        return;
    }
    log::info!("DVD menus: {name:?}, starts with {outcome:?}");
    if let Ok(mut c) = CURRENT.lock() {
        *c = Some(machine.clone());
    }
    SESSION.with_borrow_mut(|s| {
        if let Some(s) = s.as_mut() {
            s.machine = Some(machine);
            s.name = name;
        }
    });
    match outcome {
        // The first block is waiting, or there is a still with nothing in it.
        Outcome::Same => load_menu(ui),
        o => after(ui, o),
    }
}

fn after(ui: &AppWindow, outcome: Outcome) {
    match outcome {
        Outcome::Same => tick(ui),
        Outcome::Reload => load_menu(ui),
        Outcome::Title(t) => play_title(ui, t, None),
        Outcome::End => {
            if SESSION.with_borrow(|s| s.as_ref().is_some_and(|s| s.loading)) {
                no_menu(ui, "This DVD has no menu: playing the main film");
            } else {
                end(ui);
                with_player(|p| p.stop());
            }
        }
    }
}

/// No menu to be had: the main film, as without menus.
fn no_menu(ui: &AppWindow, why: &str) {
    let device = SESSION.with_borrow(|s| s.as_ref().map(|s| s.device.clone()));
    end(ui);
    ui.set_toast(why.into());
    if let Some(d) = device {
        main_film(&d);
    }
}

fn main_film(device: &str) {
    let plan = disc::DiscPlan::dvd(device.to_owned());
    with_player(|p| {
        if let Err(e) = plan.apply(p) {
            log::warn!("{e}");
        }
    });
}

/// Runs only while a menu is up.
fn start_timer(ui: &AppWindow) {
    TIMER.with(|t| {
        let weak = ui.as_weak();
        t.start(slint::TimerMode::Repeated, TICK, move || {
            if let Some(ui) = weak.upgrade() {
                tick(&ui);
            }
        });
    });
}

fn load_menu(ui: &AppWindow) {
    let name = SESSION.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        if let Playing::Title(t) = &s.playing
            && let Some(pos) = with_player(|p| p.get_double("time-pos")).flatten()
            && pos > 5.0
        {
            s.left_at = Some((t.clone(), pos));
        }
        s.playing = Playing::Menu;
        s.still_since = None;
        s.moment = None;
        s.drawn = None;
        s.pressed_until = None;
        s.loading = false;
        Some(if s.name.is_empty() { "DVD menu".to_owned() } else { format!("{} (menu)", s.name) })
    });
    let Some(name) = name else { return };
    ui.set_dvd_menu(true);
    ui.set_dvd_can_return(SESSION.with_borrow(|s| s.as_ref().is_some_and(|s| s.left_at.is_some())));
    let mut options = MENU_OPTIONS.to_vec();
    options.push(("force-media-title", &name));
    start_timer(ui);
    with_player(|p| {
        clear_overlay(p);
        let _ = p.set_pause(false);
        if let Err(e) = p.load_with(&format!("{PROTOCOL}://menu"), &options) {
            log::warn!("DVD menu: {e}");
        }
    });
}

/// mpv's link for a title (it counts from 0).
fn title_url(title: i32) -> String {
    format!("dvd://{}", title - 1)
}

fn play_title(ui: &AppWindow, t: TitleStart, seek_to: Option<f64>) {
    let found = SESSION.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        s.playing = Playing::Title(t.clone());
        s.seek_to = seek_to;
        s.still_since = None;
        s.moment = None;
        s.drawn = None;
        s.loading = false;
        Some((s.device.clone(), s.name.clone()))
    });
    let Some((device, name)) = found else { return };
    log::info!("DVD: title {}, chapter {}", t.title, t.chapter);
    ui.set_dvd_menu(false);
    TIMER.with(slint::Timer::stop);
    let title = if name.is_empty() { format!("DVD title {}", t.title) } else { format!("{name} · title {}", t.title) };
    let start = format!("#{}", t.chapter);
    let mut options = vec![("force-media-title", title.as_str())];
    if t.chapter > 1 && seek_to.is_none() {
        options.push(("start", start.as_str()));
    }
    let mut plan = disc::DiscPlan::dvd(device);
    plan.url = title_url(t.title);
    with_player(|p| {
        clear_overlay(p);
        if let Err(e) = plan.apply_with(p, &options) {
            log::warn!("{e}");
        }
    });
}

fn clear_overlay(p: &Player) {
    let _ = p.command(&["overlay-remove", OVERLAY]);
}

/// Something else plays now, or nothing.
pub fn end(ui: &AppWindow) {
    NUMBER.with(|n| n.set(n.get() + 1));
    TIMER.with(slint::Timer::stop);
    let Some(s) = SESSION.take() else { return };
    let _ = s.commands.send(Command::Quit);
    if let Ok(mut c) = CURRENT.lock() {
        *c = None;
    }
    with_player(clear_overlay);
    ui.set_dvd_menu(false);
    ui.set_dvd_disc(false);
    ui.set_dvd_can_return(false);
}

/// A file started: is it still ours?
pub fn file_started(ui: &AppWindow, path: Option<&str>) {
    let Some(path) = path.filter(|p| !p.is_empty()) else { return };
    let ours = SESSION.with_borrow(|s| match s.as_ref().map(|s| &s.playing) {
        None => true,
        Some(Playing::Nothing) => true,
        Some(Playing::Menu) => is_menu_path(path),
        Some(Playing::Title(t)) => path == title_url(t.title),
    });
    if !ours {
        end(ui);
    }
}

/// A title was loaded: the sound and subtitles chosen in the menu, and the
/// place it was left at.
pub fn file_loaded(player: &Player) {
    if player.get_string("path").is_some_and(|p| p.starts_with("dvd://"))
        && let Some(d) = player.get_double("duration").filter(|d| *d > 10.0)
    {
        // At a title's end mpv's DVD stream runs on into whatever the disc
        // has next (a menu without buttons): stop there instead.
        let _ = player.set_string("file-local-options/end", &format!("{:.3}", d - 0.1));
    }
    let tracks = player.tracks();
    let found = SESSION.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        match s.playing.clone() {
            Playing::Title(t) => Some((t, s.seek_to.take())),
            _ => None,
        }
    });
    let Some((t, seek_to)) = found else { return };
    let find = |kind: TrackKind, number: i32, mask: i64| {
        tracks.iter().filter(|x| x.kind == kind).find(|x| x.src_id.is_some_and(|i| i & mask == i64::from(number))).map(|x| x.id)
    };
    with_player(|p| {
        // Sound: 0x80 + n (AC-3), 0x88 + n (DTS), 0xA0 + n (PCM), 0xC0 + n.
        if let Some(id) = t.audio.and_then(|n| find(TrackKind::Audio, n, 7)) {
            let _ = p.select_track(TrackKind::Audio, Some(id));
        }
        let sub = t.subtitle.and_then(|n| find(TrackKind::Subtitle, n, 0x1f));
        let _ = p.select_track(TrackKind::Subtitle, sub);
        if t.angle > 1 {
            let _ = p.set_string("dvd-angle", &t.angle.to_string());
        }
        if let Some(pos) = seek_to {
            let _ = p.command(&["seek", &format!("{pos:.3}"), "absolute"]);
        }
    });
}

/// A file ended. True: the disc goes on by itself.
pub fn file_ended(ui: &AppWindow, reason: EndReason) -> bool {
    let playing = SESSION.with_borrow(|s| s.as_ref().map(|s| s.playing.clone()));
    match (playing, reason) {
        (Some(Playing::Title(_)), EndReason::Eof) => {
            SESSION.with_borrow_mut(|s| {
                if let Some(s) = s.as_mut() {
                    s.playing = Playing::Nothing;
                    s.left_at = None;
                }
            });
            send(Action::TitleDone);
            true
        }
        (Some(Playing::Menu), EndReason::Error) => {
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    no_menu(&ui, "Couldn't play the DVD's menu: playing the main film");
                }
            });
            true
        }
        _ => false,
    }
}

/// Nothing plays any more (Stop was pressed).
pub fn went_idle(ui: &AppWindow) {
    if SESSION.with_borrow(|s| s.as_ref().is_some_and(|s| s.playing != Playing::Nothing)) {
        end(ui);
    }
}

fn send(action: Action) {
    SESSION.with_borrow(|s| {
        if let Some(s) = s {
            let _ = s.commands.send(Command::Act(action, s.moment.clone()));
        }
    });
}

/// A key, button or menu item.
fn command(ui: &AppWindow, what: &str) {
    match what {
        "menu" => return open_menu(ui),
        "main-film" => {
            let device = SESSION.with_borrow(|s| s.as_ref().map(|s| s.device.clone()));
            if let Some(d) = device {
                end(ui);
                main_film(&d);
            }
            return;
        }
        "return" if menu_active() => {
            let left = SESSION.with_borrow_mut(|s| s.as_mut().and_then(|s| s.left_at.take()));
            if let Some((t, pos)) = left {
                ui.set_dvd_can_return(false);
                play_title(ui, t, Some(pos));
            }
            return;
        }
        _ => {}
    }
    if !menu_active() {
        return;
    }
    match what {
        "up" => send(Action::Up),
        "down" => send(Action::Down),
        "left" => send(Action::Left),
        "right" => send(Action::Right),
        "press" => press(ui, None),
        "skip" => send(Action::Skip),
        "back" => send(Action::Back),
        _ => {}
    }
}

fn press(ui: &AppWindow, button: Option<i32>) {
    with_player(|p| p.set_pause(false));
    SESSION.with_borrow_mut(|s| {
        if let Some(s) = s.as_mut() {
            s.asked = button.unwrap_or(s.asked);
            s.pressed_until = Some(Instant::now() + Duration::from_millis(400));
        }
    });
    send(Action::Press(button));
    draw(ui);
}

/// Playback → Disc menu.
fn open_menu(ui: &AppWindow) {
    let state = SESSION.with_borrow(|s| s.as_ref().map(|s| s.playing.clone()));
    match state {
        // In the menus already: the disc's main one.
        Some(Playing::Menu) => send(Action::Menu(Menu::Title)),
        Some(Playing::Title(_)) => {
            ui.set_toast("Opening the DVD's menu…".into());
            send(Action::Menu(Menu::Root));
        }
        Some(Playing::Nothing) => {}
        None => {
            // A DVD that was started without its menu.
            let found = with_player(|p| {
                let path = p.get_string("path").filter(|p| p.starts_with("dvd://"))?;
                let device = p.get_string("dvd-device").filter(|d| !d.is_empty())?;
                let title = p.get_double("disc-title").map_or(0, |t| t as i32 + 1);
                Some((path, device, title, p.get_double("time-pos").unwrap_or(0.0)))
            });
            match found.flatten() {
                Some((_, device, title, pos)) if available() => {
                    start(ui, &device, Some(title));
                    SESSION.with_borrow_mut(|s| {
                        if let Some(s) = s.as_mut()
                            && title > 0
                            && pos > 5.0
                        {
                            let t = TitleStart { title, chapter: 1, audio: None, subtitle: None, angle: 1 };
                            s.left_at = Some((t, pos));
                        }
                    });
                }
                Some(_) => ui.set_toast("DVD menus need libdvdnav, which is not installed".into()),
                None => ui.set_toast("No DVD is playing".into()),
            }
        }
    }
}

/// The mouse moved to, or clicked at, a point of the window's content.
fn mouse(ui: &AppWindow, x: f32, y: f32, pressed: bool) {
    let window = ui.window();
    let scale = f64::from(window.scale_factor());
    // The menu bar sits above the content.
    let bar = f64::from(window.size().height) - f64::from(ui.get_content_height()) * scale;
    let (px, py) = (f64::from(x) * scale, f64::from(y) * scale + bar.max(0.0));
    let hit = SESSION.with_borrow_mut(|s| {
        let s = s.as_mut().filter(|s| s.playing == Playing::Menu)?;
        let (vx, vy) = s.place?.to_video(px, py);
        let i = s.moment.as_ref()?.buttons.iter().position(|b| b.rect.contains(vx, vy))?;
        let n = i as i32 + 1;
        let new = n != s.asked;
        s.asked = n;
        Some((n, new))
    });
    ui.set_dvd_over_button(hit.is_some());
    match hit {
        Some((n, _)) if pressed => press(ui, Some(n)),
        Some((n, true)) => send(Action::Select(n)),
        _ => {}
    }
}

/// Where mpv draws the picture.
fn place(p: &Player) -> Option<spu::Place> {
    let Some(Value::Map(d)) = p.get_value("osd-dimensions") else { return None };
    let n = |k: &str| {
        d.iter().find(|(key, _)| key == k).and_then(|(_, v)| match v {
            Value::Int(i) => Some(*i as f64),
            Value::Double(f) => Some(*f),
            _ => None,
        })
    };
    let (w, h) = (n("w")?, n("h")?);
    let (left, right, top, bottom) = (n("ml")?, n("mr")?, n("mt")?, n("mb")?);
    let video = (p.get_double("video-params/w")? as u32, p.get_double("video-params/h")? as u32);
    let place = spu::Place { left, top, width: w - left - right, height: h - top - bottom, video };
    (place.width > 1.0 && place.height > 1.0 && video.0 > 0 && video.1 > 0).then_some(place)
}

/// Keep up with the stream: what it halted for, and the highlight.
fn tick(ui: &AppWindow) {
    if !menu_active() {
        return;
    }
    let machine = SESSION.with_borrow(|s| s.as_ref().and_then(|s| s.machine.clone()));
    let Some(machine) = machine else { return };
    // The reader may hold it while the drive seeks: next time, then.
    let Ok(mut m) = machine.try_lock() else { return };
    if m.hopped && m.halt.is_none() {
        // Only the reader saw the jump (a button pressed itself).
        let reload = m.advance();
        drop(m);
        return after(ui, reload);
    }
    let halt = m.halt.clone();
    drop(m);
    let shown = with_player(|p| p.get_string("eof-reached").as_deref() == Some("yes")).unwrap_or(false);
    if let Some(halt) = halt.filter(|_| shown) {
        let since = SESSION.with_borrow_mut(|s| s.as_mut().map(|s| *s.still_since.get_or_insert_with(Instant::now)));
        match halt {
            Halt::Title(t) => return play_title(ui, t, None),
            Halt::End => return after(ui, Outcome::End),
            Halt::Still(Some(secs)) => {
                if since.is_some_and(|s| s.elapsed() >= Duration::from_secs(u64::from(secs))) {
                    // Once: the next stream resets it.
                    SESSION.with_borrow_mut(|s| {
                        if let Some(s) = s.as_mut() {
                            s.still_since = Some(Instant::now() + Duration::from_secs(3600));
                        }
                    });
                    send(Action::StillOver);
                }
            }
            Halt::Still(None) => {}
        }
    }
    draw(ui);
}

/// Lay the chosen button over the picture, if that changed.
fn draw(ui: &AppWindow) {
    let found = SESSION.with_borrow(|s| {
        let s = s.as_ref().filter(|s| s.playing == Playing::Menu)?;
        let pressed = s.pressed_until.is_some_and(|t| Instant::now() < t);
        Some((s.machine.clone()?, s.drawn.clone(), pressed))
    });
    let Some((machine, drawn, pressed)) = found else { return };
    with_player(|p| {
        let place = place(p);
        let Ok(m) = machine.try_lock() else { return };
        let still = m.halt.is_some() && p.get_string("eof-reached").as_deref() == Some("yes");
        let time = p.get_double("time-pos").unwrap_or(0.0);
        let pts = if still { i64::MAX } else { 90_000 + (time * 90_000.0) as i64 + 9_000 };
        let (moment, picture) = m.at(pts);
        let (button, clut, in_menu) = (m.button, m.clut, m.in_menu());
        drop(m);
        let has_buttons = moment.as_ref().is_some_and(|m| !m.buttons.is_empty());
        SESSION.with_borrow_mut(|s| {
            if let Some(s) = s.as_mut() {
                s.moment.clone_from(&moment);
                s.place = place;
                s.asked = button;
            }
        });
        ui.set_dvd_buttons(has_buttons);
        let Some(place) = place else { return };
        let key = DrawKey {
            button,
            moment: moment.as_ref().map_or(-1, |m| m.pts),
            picture: picture.as_ref().map(|p| (p.pts, p.x, p.y)),
            place,
            pressed,
        };
        if drawn.as_ref() == Some(&key) {
            return;
        }
        let lit = moment
            .as_ref()
            .and_then(|m| m.buttons.get(usize::try_from(button - 1).ok()?))
            .map(|b| (&b.rect, if pressed { &b.pressed } else { &b.selected }));
        // Subtitles of a clip are not the menu's business.
        let overlay = picture.filter(|_| in_menu || has_buttons).and_then(|sp| spu::draw(&sp, lit, &clut, &place));
        match overlay {
            Some(o) => {
                let args = [
                    "overlay-add".to_owned(),
                    OVERLAY.to_owned(),
                    o.x.to_string(),
                    o.y.to_string(),
                    format!("&{}", o.bgra.as_ptr() as usize),
                    "0".to_owned(),
                    "bgra".to_owned(),
                    o.width.to_string(),
                    o.height.to_string(),
                    (o.width * 4).to_string(),
                ];
                // mpv copies the picture before the command returns.
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                if let Err(e) = p.command(&refs) {
                    log::debug!("{e}");
                }
            }
            None => clear_overlay(p),
        }
        SESSION.with_borrow_mut(|s| {
            if let Some(s) = s.as_mut() {
                s.drawn = Some(key);
            }
        });
    });
}
