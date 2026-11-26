//! Advanced navigation (Tools → Advanced navigation, `nav.slint`): steps of
//! a chosen size (one frame, a millisecond … five minutes), a jog wheel that
//! turns with the media and scrubs it, a spring-loaded shuttle with J / K / L,
//! fine speed and pitch, an exact clock, go-to-time and A/B marks.
//!
//! The panel sits docked under the media player's control bar or in a
//! window of its own (beside the player). Turning the tool off gives the
//! arrow keys back their usual 5-second seeks and restores the speed and
//! pitch the player had before.
//!
//! Exact steps: mpv reports the time of the frame on screen, so a step
//! smaller than a frame, measured from there, would land on the same frame
//! for ever. Steps are counted from the time last asked for instead
//! (`target`), as long as the picture is still on that frame.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use player_core::Player;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::settings::Settings;
use crate::{AppWindow, NavState, NavWindow, Theme};

/// Step sizes offered, in milliseconds; -1 is one frame.
pub const STEPS_MS: &[f64] = &[
    -1.0, 1.0, 5.0, 10.0, 20.0, 40.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0,
    30_000.0, 60_000.0, 300_000.0,
];
/// Seconds of media in one turn of the jog wheel (kept in settings.rs,
/// which checks saved values against it).
use crate::settings::NAV_TURNS as TURNS;
/// Longest custom step: an hour.
const MAX_STEP_MS: f64 = 3_600_000.0;
const MIN_SPEED: f64 = 0.05;
const MAX_SPEED: f64 = 8.0;
/// Shuttle speeds: from this (just off centre) to `MAX_SPEED` (at the end).
const SHUTTLE_SLOWEST: f64 = 0.1;
/// Below this far off centre the shuttle stands still.
const SHUTTLE_DEAD: f64 = 0.06;
/// The shuttle plays backwards by seeking this often.
const REVERSE_TICK: Duration = Duration::from_millis(50);
/// A jog wheel that stops turning (button still held) pauses after this.
const JOG_IDLE: Duration = Duration::from_millis(140);
/// How much is heard of each step, with "sound on each step".
const SNIPPET: Duration = Duration::from_millis(140);

/// "1 frame", "250 ms", "2 s", "1 min".
pub fn step_label(ms: f64) -> String {
    if ms < 0.0 {
        "1 frame".into()
    } else if ms < 1000.0 {
        format!("{} ms", trim_num(ms))
    } else if ms < 60_000.0 {
        format!("{} s", trim_num(ms / 1000.0))
    } else {
        format!("{} min", trim_num(ms / 60_000.0))
    }
}

fn turn_label(s: f64) -> String {
    if s < 60.0 {
        format!("{} s", trim_num(s))
    } else {
        format!("{} min", trim_num(s / 60.0))
    }
}

/// 2.0 → "2", 0.5 → "0.5", 41.708 → "41.71".
fn trim_num(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// "1:02:03.250" or "2:03.250".
pub fn fmt_precise(t: f64) -> String {
    let ms_total = (t.max(0.0) * 1000.0).round() as u64;
    let (ms, s) = (ms_total % 1000, ms_total / 1000);
    if s >= 3600 {
        format!("{}:{:02}:{:02}.{ms:03}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}.{ms:03}", s / 60, s % 60)
    }
}

/// A typed time: absolute, or relative with a leading + or -.
#[derive(Debug, PartialEq)]
pub enum TimeInput {
    At(f64),
    By(f64),
}

/// "1:23.5", "83.5", "1:02:03,250", "f240" / "240f" (a frame), "+2.5",
/// "-1:00". Frames need the frame rate.
pub fn parse_time(text: &str, fps: Option<f64>) -> Option<TimeInput> {
    let t = text.trim().to_lowercase().replace(',', ".");
    let (sign, body) = match t.strip_prefix('+') {
        Some(rest) => (1.0, rest.trim()),
        None => match t.strip_prefix('-') {
            Some(rest) => (-1.0, rest.trim()),
            None => (0.0, t.as_str()),
        },
    };
    let frames = body.strip_prefix('f').or_else(|| body.strip_suffix('f'));
    let secs = if let Some(n) = frames {
        let n: f64 = n.trim().parse().ok()?;
        n / fps.filter(|f| *f > 0.0)?
    } else {
        let parts: Vec<&str> = body.split(':').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let mut total = 0.0;
        for (i, p) in parts.iter().enumerate() {
            let v: f64 = p.trim().parse().ok()?;
            // Only the seconds may have a fraction.
            if v < 0.0 || (i + 1 < parts.len() && v.fract() != 0.0) {
                return None;
            }
            total = total * 60.0 + v;
        }
        total
    };
    if !secs.is_finite() {
        return None;
    }
    Some(if sign == 0.0 {
        TimeInput::At(secs)
    } else {
        TimeInput::By(sign * secs)
    })
}

/// Shuttle position (-1..1) to a speed (negative: backwards); 0 near the middle.
pub fn shuttle_rate(v: f64) -> f64 {
    let a = v.abs();
    if a < SHUTTLE_DEAD {
        return 0.0;
    }
    let u = ((a - SHUTTLE_DEAD) / (1.0 - SHUTTLE_DEAD)).clamp(0.0, 1.0);
    let rate = SHUTTLE_SLOWEST * (MAX_SPEED / SHUTTLE_SLOWEST).powf(u);
    v.signum() * (rate * 100.0).round() / 100.0
}

/// The shuttle position that gives `rate` (for J / K / L).
fn shuttle_position(rate: f64) -> f64 {
    if rate == 0.0 {
        return 0.0;
    }
    let u = (rate.abs() / SHUTTLE_SLOWEST).ln() / (MAX_SPEED / SHUTTLE_SLOWEST).ln();
    rate.signum() * (SHUTTLE_DEAD + u.clamp(0.0, 1.0) * (1.0 - SHUTTLE_DEAD))
}

fn shuttle_text(rate: f64) -> String {
    if rate > 0.0 {
        format!("Forward {}×", trim_num(rate))
    } else if rate < 0.0 {
        format!("Backward {}×", trim_num(-rate))
    } else {
        "Shuttle: hold off centre · J / K / L".into()
    }
}

fn step_index(ms: f64) -> usize {
    STEPS_MS
        .iter()
        .position(|s| (s - ms).abs() < 1e-6)
        .unwrap_or(STEPS_MS.len())
}

fn step_labels(ms: f64) -> ModelRc<SharedString> {
    let mut labels: Vec<SharedString> = STEPS_MS.iter().map(|s| step_label(*s).into()).collect();
    if step_index(ms) == STEPS_MS.len() {
        labels.push(format!("Custom: {}", step_label(ms)).into());
    }
    ModelRc::new(VecModel::from(labels))
}

struct Nav {
    player: Rc<Player>,
    settings: Rc<RefCell<Settings>>,
    ui: slint::Weak<AppWindow>,
    window: Option<NavWindow>,
    enabled: Cell<bool>,
    /// The time last stepped to (see the module comment).
    target: Cell<Option<f64>>,
    /// Paused, and the speed, from before the wheel or the shuttle took over.
    held: Cell<Option<(bool, f64)>>,
    /// The speed and pitch correction from before the tool was turned on.
    before: Cell<(f64, bool)>,
    shuttle: Cell<f64>,
    reverse: slint::Timer,
    jog_last: Cell<Option<Instant>>,
    jog_speed: Cell<f64>,
    jog_playing: Cell<bool>,
    jog_idle: slint::Timer,
    snippet: Cell<u64>,
    /// The step labels were made for this step size.
    labels_for: Cell<f64>,
}

impl Nav {
    fn paused(&self) -> bool {
        self.player.get_string("pause").as_deref() != Some("no")
    }

    fn speed(&self) -> f64 {
        self.player.get_double("speed").unwrap_or(1.0)
    }

    fn time(&self) -> Option<f64> {
        self.player.get_double("time-pos")
    }

    fn fps(&self) -> Option<f64> {
        ["container-fps", "estimated-vf-fps"]
            .iter()
            .find_map(|p| self.player.get_double(p))
            .filter(|f| *f > 0.0 && f.is_finite())
    }

    fn toast(&self, text: &str) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_toast(text.into());
        }
    }

    /// Where steps count from: the last step's time while that frame is
    /// still shown, else mpv's clock.
    fn base(&self) -> Option<f64> {
        let actual = self.time()?;
        let tolerance = self.fps().map_or(0.05, |f| 1.5 / f).max(0.02);
        Some(match self.target.get() {
            Some(t) if self.paused() && (t - actual).abs() < tolerance => t,
            _ => actual,
        })
    }

    fn seek_exact(&self, to: f64) {
        let to = match self.player.get_double("duration") {
            Some(d) if d > 0.0 => to.clamp(0.0, d),
            _ => to.max(0.0),
        };
        if let Err(e) = self
            .player
            .command(&["seek", &format!("{to:.6}"), "absolute+exact"])
        {
            log::debug!("seek: {e}");
        }
        self.target.set(Some(to));
    }

    /// Move by `secs` exactly. `sound`: let a moment of it be heard.
    fn step_by(self: &Rc<Self>, secs: f64, sound: bool) {
        let Some(base) = self.base() else { return };
        self.seek_exact(base + secs);
        if sound && self.paused() && self.settings.borrow().nav_step_sound {
            self.play_snippet();
        }
    }

    /// Play a moment from the step's time, then go back there.
    fn play_snippet(self: &Rc<Self>) {
        let n = self.snippet.get() + 1;
        self.snippet.set(n);
        let _ = self.player.set_pause(false);
        let me = Rc::downgrade(self);
        slint::Timer::single_shot(SNIPPET, move || {
            let Some(me) = me.upgrade() else { return };
            if me.snippet.get() == n {
                let _ = me.player.set_pause(true);
                if let Some(t) = me.target.get() {
                    me.seek_exact(t);
                }
            }
        });
    }

    /// One step of the chosen size, `times` times (negative: back).
    fn step(self: &Rc<Self>, times: f64) {
        let ms = self.settings.borrow().nav_step_ms;
        self.stop_motion();
        if ms < 0.0 {
            // Frames: single ones with mpv's own frame stepping; ten at once by time.
            match self.fps() {
                Some(fps) if times.abs() > 1.0 => self.step_by(times / fps, true),
                _ => self.frame(times > 0.0),
            }
        } else {
            self.step_by(ms / 1000.0 * times, true);
        }
    }

    fn frame(&self, forward: bool) {
        self.stop_motion();
        self.target.set(None);
        let cmd = if forward {
            "frame-step"
        } else {
            "frame-back-step"
        };
        if let Err(e) = self.player.command(&[cmd]) {
            log::debug!("{cmd}: {e}");
        }
    }

    /// Remember the play state before the wheel or shuttle changes it.
    fn hold(&self) {
        if self.held.get().is_none() {
            self.held.set(Some((self.paused(), self.speed())));
        }
    }

    /// Back to the play state from before the wheel or shuttle.
    fn release(&self) {
        if let Some((paused, speed)) = self.held.take() {
            let _ = self.player.set_speed(speed);
            let _ = self.player.set_pause(paused);
        }
    }

    // ---- jog wheel ----

    fn jog(self: &Rc<Self>, degrees: f64) {
        let turn = self.settings.borrow().nav_turn;
        let secs = degrees / 360.0 * turn;
        let now = Instant::now();
        let dt = self
            .jog_last
            .replace(Some(now))
            .map_or(0.016, |t| (now - t).as_secs_f64())
            .clamp(0.005, 0.25);
        self.stop_shuttle(false);
        self.hold();
        if secs > 0.0 && self.settings.borrow().nav_jog_sound {
            // Forwards with sound: play at the speed the wheel turns, like
            // a record under a hand.
            let speed = (0.5 * self.jog_speed.get() + 0.5 * secs / dt).clamp(MIN_SPEED, MAX_SPEED);
            self.jog_speed.set(speed);
            let _ = self.player.set_speed(speed);
            if !self.jog_playing.replace(true) {
                let _ = self.player.set_pause(false);
            }
            self.target.set(None);
            let me = Rc::downgrade(self);
            self.jog_idle
                .start(slint::TimerMode::SingleShot, JOG_IDLE, move || {
                    if let Some(me) = me.upgrade() {
                        me.jog_still();
                    }
                });
        } else {
            self.jog_still();
            self.step_by(secs, false);
        }
    }

    /// The wheel is held but not turning (or turning back): stand still.
    fn jog_still(&self) {
        self.jog_speed.set(0.0);
        if self.jog_playing.replace(false) || !self.paused() {
            let _ = self.player.set_pause(true);
        }
    }

    fn jog_end(&self) {
        self.jog_idle.stop();
        self.jog_last.set(None);
        self.jog_speed.set(0.0);
        self.jog_playing.set(false);
        self.release();
    }

    // ---- shuttle ----

    fn set_shuttle(self: &Rc<Self>, rate: f64) {
        self.hold();
        let was = self.shuttle.replace(rate);
        if rate > 0.0 {
            self.reverse.stop();
            let _ = self.player.set_speed(rate);
            let _ = self.player.set_pause(false);
        } else {
            let _ = self.player.set_pause(true);
            if rate < 0.0 && was >= 0.0 {
                self.target.set(None);
                let me = Rc::downgrade(self);
                self.reverse
                    .start(slint::TimerMode::Repeated, REVERSE_TICK, move || {
                        let Some(me) = me.upgrade() else { return };
                        let r = me.shuttle.get();
                        let Some(base) = me.target.get().or_else(|| me.time()) else {
                            return;
                        };
                        if base <= 0.0 {
                            me.stop_shuttle(false);
                            return;
                        }
                        let to = (base + r * REVERSE_TICK.as_secs_f64()).max(0.0);
                        // Fast: whole pictures only (keyframes), which keeps up.
                        let flags = if r < -2.0 {
                            "absolute+keyframes"
                        } else {
                            "absolute+exact"
                        };
                        let _ = me.player.command(&["seek", &format!("{to:.6}"), flags]);
                        me.target.set(Some(to));
                    });
            } else if rate == 0.0 {
                self.reverse.stop();
            }
        }
    }

    /// `restore`: back to how it was (the shuttle let go); else pause
    /// there (K), keeping the old speed.
    fn stop_shuttle(&self, restore: bool) {
        self.reverse.stop();
        if self.shuttle.replace(0.0) == 0.0 && !restore {
            return;
        }
        if restore {
            self.release();
        } else if let Some((_, speed)) = self.held.take() {
            let _ = self.player.set_speed(speed);
            let _ = self.player.set_pause(true);
        }
    }

    /// J / K / L: each press of J or L goes faster that way; K stops.
    fn shuttle_key(self: &Rc<Self>, dir: f64) {
        let r = self.shuttle.get();
        if dir == 0.0 {
            if r != 0.0 {
                self.stop_shuttle(false);
            } else {
                let _ = self.player.toggle_pause();
            }
            return;
        }
        let next = if r * dir <= 0.0 {
            dir
        } else {
            (r * 2.0).clamp(-MAX_SPEED, MAX_SPEED)
        };
        self.set_shuttle(next);
    }

    /// Stop the wheel and the shuttle (before a step, say).
    fn stop_motion(&self) {
        if self.shuttle.get() != 0.0 {
            self.stop_shuttle(false);
        }
        if self.jog_playing.get() {
            self.jog_end();
        }
    }

    fn set_speed(&self, s: f64) {
        let s = (s.clamp(MIN_SPEED, MAX_SPEED) * 100.0).round() / 100.0;
        if let Err(e) = self.player.set_speed(s) {
            log::debug!("speed: {e}");
        }
    }

    // ---- turning the tool on and off ----

    fn turn_on(&self) {
        self.before.set((
            self.speed(),
            self.player.get_string("audio-pitch-correction").as_deref() != Some("no"),
        ));
        let pitch = self.settings.borrow().nav_pitch;
        let _ = self.player.set_flag("audio-pitch-correction", pitch);
        self.target.set(None);
    }

    fn turn_off(&self) {
        self.stop_motion();
        self.jog_end();
        let (speed, pitch) = self.before.get();
        let _ = self.player.set_speed(speed);
        let _ = self.player.set_flag("audio-pitch-correction", pitch);
        self.target.set(None);
    }

    fn mark(&self, which: &str) {
        let name = if which == "a" {
            "ab-loop-a"
        } else {
            "ab-loop-b"
        };
        let Some(t) = self.base() else { return };
        if let Err(e) = self.player.set_double(name, t) {
            log::debug!("{name}: {e}");
        }
        match (
            self.player.get_double("ab-loop-a"),
            self.player.get_double("ab-loop-b"),
        ) {
            (Some(a), Some(b)) if b > a => self.toast(&format!(
                "Repeating {} – {}",
                fmt_precise(a),
                fmt_precise(b)
            )),
            (Some(a), Some(b)) => self.toast(&format!(
                "B ({}) is before A ({}): set B later than A to repeat",
                fmt_precise(b),
                fmt_precise(a)
            )),
            _ => {}
        }
    }

    fn act(self: &Rc<Self>, id: &str, v: f64) {
        let Some(ui) = self.ui.upgrade() else { return };
        match id {
            "pause" => {
                if self.shuttle.get() != 0.0 {
                    self.stop_shuttle(false);
                } else {
                    ui.invoke_toggle_pause();
                }
            }
            "step" => self.step(v),
            "frame" => self.frame(v > 0.0),
            "home" => {
                self.stop_motion();
                self.seek_exact(0.0);
            }
            "jog" => self.jog(v),
            "jog-end" => self.jog_end(),
            // The mouse wheel over the jog wheel: a silent step.
            "nudge" => {
                self.stop_motion();
                let turn = self.settings.borrow().nav_turn;
                self.step_by(v / 360.0 * turn, false);
            }
            "shuttle" => self.set_shuttle(shuttle_rate(v)),
            "shuttle-end" => self.stop_shuttle(true),
            "shuttle-key" => self.shuttle_key(v),
            "speed" => self.set_speed(v),
            "speed-nudge" => self.set_speed(self.speed() + 0.05 * v),
            "pitch" => {
                let on = v > 0.0;
                let _ = self.player.set_flag("audio-pitch-correction", on);
                self.save(|s| s.nav_pitch = on);
            }
            "jog-sound" => self.save(|s| s.nav_jog_sound = v > 0.0),
            "step-sound" => self.save(|s| s.nav_step_sound = v > 0.0),
            "step-index" => {
                if let Some(ms) = STEPS_MS.get(v.max(0.0) as usize) {
                    self.save(|s| s.nav_step_ms = *ms);
                }
            }
            "turn-index" => {
                if let Some(t) = TURNS.get(v.max(0.0) as usize) {
                    self.save(|s| s.nav_turn = *t);
                }
            }
            "mark-a" => self.mark("a"),
            "mark-b" => self.mark("b"),
            "mark-clear" => {
                let _ = self.player.set_string("ab-loop-a", "no");
                let _ = self.player.set_string("ab-loop-b", "no");
            }
            "dock" => {
                ui.set_s_nav_docked(v > 0.0);
                ui.invoke_settings_changed();
            }
            "close" => {
                ui.set_s_nav_enabled(false);
                ui.invoke_settings_changed();
            }
            other => log::warn!("unknown navigation action {other}"),
        }
    }

    fn text(&self, id: &str, text: &str) {
        match id {
            "goto" => match parse_time(text, self.fps()) {
                Some(TimeInput::At(t)) => {
                    self.stop_motion();
                    self.seek_exact(t);
                }
                Some(TimeInput::By(d)) => {
                    self.stop_motion();
                    if let Some(b) = self.base() {
                        self.seek_exact(b + d);
                    }
                }
                None if text.trim().is_empty() => {}
                None => self.toast(&format!(
                    "\"{}\" isn't a time: try 1:23.5, 83.5, +2, or f240 for a frame",
                    text.trim()
                )),
            },
            "custom-step" => match text.trim().replace(',', ".").parse::<f64>() {
                Ok(ms) if ms > 0.0 && ms <= MAX_STEP_MS => {
                    self.save(|s| s.nav_step_ms = (ms * 1000.0).round() / 1000.0);
                    self.toast(&format!("Each step is now {}", step_label(ms)));
                }
                _ => self.toast("A custom step is a number of milliseconds, up to an hour"),
            },
            other => log::warn!("unknown navigation input {other}"),
        }
    }

    fn save(&self, change: impl FnOnce(&mut Settings)) {
        let mut s = self.settings.borrow_mut();
        change(&mut s);
        s.save();
    }

    /// What the panel shows now.
    fn state(&self, ui: &AppWindow) -> NavState {
        let s = self.settings.borrow();
        let p = &self.player;
        let idle = ui.get_idle();
        let actual = self.time().unwrap_or(0.0);
        let paused = self.paused();
        let shown = match self.target.get() {
            Some(t) if paused && self.base() == Some(t) => t,
            _ => actual,
        };
        let duration = p.get_double("duration").unwrap_or(0.0);
        let fps = self.fps().filter(|_| ui.get_has_video());
        let frame = match (fps, p.get_double("estimated-frame-number")) {
            (Some(fps), Some(n)) => {
                let of = p
                    .get_double("estimated-frame-count")
                    .map(|c| format!(" of {}", c as u64))
                    .unwrap_or_default();
                format!("Frame {}{of} · {} fps", n as u64, trim_num(fps))
            }
            _ => String::new(),
        };
        let chapter = match (p.get_double("chapter"), p.get_double("chapters")) {
            (Some(c), Some(n)) if c >= 0.0 && n > 0.0 => {
                let title = p
                    .get_string(&format!("chapter-list/{}/title", c as i64))
                    .filter(|t| !t.is_empty())
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default();
                format!("Chapter {} of {}{title}", c as i64 + 1, n as i64)
            }
            _ => String::new(),
        };
        let mark = |name: &str| p.get_double(name).map(fmt_precise).unwrap_or_default();
        let rate = self.shuttle.get();
        NavState {
            position: shown as f32,
            duration: duration as f32,
            paused,
            idle,
            has_video: ui.get_has_video(),
            clock: if idle {
                "0:00.000".into()
            } else {
                fmt_precise(shown).into()
            },
            total: fmt_precise(duration).into(),
            frame: frame.into(),
            chapter: chapter.into(),
            speed: self.speed() as f32,
            pitch: s.nav_pitch,
            jog_sound: s.nav_jog_sound,
            step_sound: s.nav_step_sound,
            step_index: step_index(s.nav_step_ms) as i32,
            step_text: step_label(s.nav_step_ms).into(),
            custom_step: if step_index(s.nav_step_ms) == STEPS_MS.len() {
                trim_num(s.nav_step_ms).into()
            } else {
                SharedString::default()
            },
            turn_index: TURNS
                .iter()
                .position(|t| (t - s.nav_turn).abs() < 1e-6)
                .unwrap_or(2) as i32,
            turn_seconds: s.nav_turn as f32,
            shuttle: shuttle_position(rate) as f32,
            shuttle_label: shuttle_text(rate).into(),
            mark_a: mark("ab-loop-a").into(),
            mark_b: mark("ab-loop-b").into(),
        }
    }
}

/// The skin's colours in the navigation window too.
fn copy_theme(ui: &AppWindow, w: &NavWindow) {
    let (a, b) = (ui.global::<Theme>(), w.global::<Theme>());
    b.set_bg(a.get_bg());
    b.set_bg_2(a.get_bg_2());
    b.set_surface(a.get_surface());
    b.set_surface_hi(a.get_surface_hi());
    b.set_border(a.get_border());
    b.set_text(a.get_text());
    b.set_text_dim(a.get_text_dim());
    b.set_text_faint(a.get_text_faint());
    b.set_accent(a.get_accent());
    b.set_accent_2(a.get_accent_2());
    b.set_danger(a.get_danger());
    b.set_accent_gradient(a.get_accent_gradient());
}

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    let window = match NavWindow::new() {
        Ok(w) => Some(w),
        Err(e) => {
            log::warn!("navigation window: {e}");
            None
        }
    };
    let turn_labels: Vec<SharedString> = TURNS.iter().map(|t| turn_label(*t).into()).collect();
    let turn_labels = ModelRc::new(VecModel::from(turn_labels));
    ui.set_nav_turn_labels(turn_labels.clone());
    let step_ms = settings.borrow().nav_step_ms;
    ui.set_nav_step_labels(step_labels(step_ms));
    if let Some(w) = &window {
        w.set_turn_labels(turn_labels);
        w.set_step_labels(step_labels(step_ms));
    }
    let nav = Rc::new(Nav {
        player: player.clone(),
        settings,
        ui: ui.as_weak(),
        window,
        enabled: Cell::new(false),
        target: Cell::new(None),
        held: Cell::new(None),
        before: Cell::new((1.0, true)),
        shuttle: Cell::new(0.0),
        reverse: slint::Timer::default(),
        jog_last: Cell::new(None),
        jog_speed: Cell::new(0.0),
        jog_playing: Cell::new(false),
        jog_idle: slint::Timer::default(),
        snippet: Cell::new(0),
        labels_for: Cell::new(step_ms),
    });

    let n = nav.clone();
    ui.on_nav_act(move |id, v| n.act(&id, f64::from(v)));
    let n = nav.clone();
    ui.on_nav_text(move |id, t| n.text(&id, &t));
    if let Some(w) = &nav.window {
        let n = Rc::downgrade(&nav);
        w.on_act(move |id, v| {
            if let Some(n) = n.upgrade() {
                n.act(&id, f64::from(v));
            }
        });
        let n = Rc::downgrade(&nav);
        w.on_text(move |id, t| {
            if let Some(n) = n.upgrade() {
                n.text(&id, &t);
            }
        });
        // Closing the window turns the tool off.
        let weak = ui.as_weak();
        w.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_s_nav_enabled(false);
                ui.invoke_settings_changed();
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    // Follow the tool's checkbox, and keep the panel current (~30 times a
    // second while it is on, so the wheel turns smoothly).
    let weak = ui.as_weak();
    let nav_tick: Weak<Nav> = Rc::downgrade(&nav);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            let (Some(ui), Some(nav)) = (weak.upgrade(), nav_tick.upgrade()) else {
                return;
            };
            let on = ui.get_s_nav_enabled();
            if on != nav.enabled.replace(on) {
                if on {
                    nav.turn_on();
                } else {
                    nav.turn_off();
                }
            }
            let player_view = nav.settings.borrow().view_mode == "player";
            let in_window = on && (!ui.get_s_nav_docked() || !player_view);
            if let Some(w) = &nav.window {
                if in_window && !w.window().is_visible() {
                    copy_theme(&ui, w);
                    // Beside the player, where the window system lets us place it.
                    let scale = ui.window().scale_factor();
                    let pos = ui.window().position().to_logical(scale);
                    let size = ui.window().size().to_logical(scale);
                    w.window()
                        .set_position(slint::LogicalPosition::new(pos.x + size.width + 8.0, pos.y));
                    let _ = w.show();
                } else if !in_window && w.window().is_visible() {
                    let _ = w.hide();
                }
                w.set_can_dock(player_view);
            }
            if !on {
                return;
            }
            let step_ms = nav.settings.borrow().nav_step_ms;
            if nav.labels_for.replace(step_ms) != step_ms {
                ui.set_nav_step_labels(step_labels(step_ms));
                if let Some(w) = &nav.window {
                    w.set_step_labels(step_labels(step_ms));
                }
            }
            let state = nav.state(&ui);
            if let Some(w) = nav.window.as_ref().filter(|w| w.window().is_visible()) {
                w.set_state(state.clone());
            }
            ui.set_nav_state(state);
        },
    );
    std::mem::forget(timer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(step_label(-1.0), "1 frame");
        assert_eq!(step_label(40.0), "40 ms");
        assert_eq!(step_label(41.708), "41.71 ms");
        assert_eq!(step_label(2500.0), "2.5 s");
        assert_eq!(step_label(300_000.0), "5 min");
        assert_eq!(turn_label(0.5), "0.5 s");
        assert_eq!(fmt_precise(83.5), "1:23.500");
        assert_eq!(fmt_precise(3723.25), "1:02:03.250");
        assert_eq!(fmt_precise(-1.0), "0:00.000");
    }

    #[test]
    fn typed_times() {
        assert_eq!(parse_time("1:23.5", None), Some(TimeInput::At(83.5)));
        assert_eq!(parse_time(" 83,5 ", None), Some(TimeInput::At(83.5)));
        assert_eq!(
            parse_time("1:02:03.250", None),
            Some(TimeInput::At(3723.25))
        );
        assert_eq!(parse_time("+2", None), Some(TimeInput::By(2.0)));
        assert_eq!(parse_time("-1:00", None), Some(TimeInput::By(-60.0)));
        assert_eq!(parse_time("f240", Some(24.0)), Some(TimeInput::At(10.0)));
        assert_eq!(parse_time("48F", Some(24.0)), Some(TimeInput::At(2.0)));
        assert_eq!(parse_time("f240", None), None);
        assert_eq!(parse_time("1.5:00", None), None);
        assert_eq!(parse_time("1:2:3:4", None), None);
        assert_eq!(parse_time("soon", None), None);
    }

    #[test]
    fn shuttle_scale() {
        assert_eq!(shuttle_rate(0.0), 0.0);
        assert_eq!(shuttle_rate(0.05), 0.0);
        assert_eq!(shuttle_rate(1.0), MAX_SPEED);
        assert_eq!(shuttle_rate(-1.0), -MAX_SPEED);
        assert!(shuttle_rate(0.07) > 0.0 && shuttle_rate(0.07) < 0.2);
        for r in [1.0, 2.0, -4.0, 8.0] {
            assert!((shuttle_rate(shuttle_position(r)) - r).abs() < 0.02, "{r}");
        }
    }

    #[test]
    fn custom_steps_get_their_own_label() {
        assert_eq!(step_index(40.0), 5);
        assert_eq!(step_index(41.7), STEPS_MS.len());
    }
}
