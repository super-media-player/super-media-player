//! Skip silence: pauses in podcasts, lectures and talks are cut short while
//! they play.
//!
//! An audio filter looks a little ahead of and behind each moment; where
//! all of that is quiet, it drops the sound. The timestamps of what remains
//! are left as they were, so mpv knows the true position: the seek bar,
//! subtitles and chapters stay right, and the picture catches up at once
//! (`mc`). Nothing is analysed beforehand, so it works on streams too.
//!
//! The graph: a mono copy of the voice band is made louder or quieter by
//! `volume@ref`, then seen through several taps shifted in time; the
//! loudest of them is the "gate" channel, merged behind the sound. `astats`
//! measures it in short frames, `ametadata` drops the frames below `GATE`,
//! and `pan` takes the extra channels off again. How loud the speech and
//! the background are is learned from the frames that are kept
//! ([`Listener`]) and only moves `volume@ref`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use player_core::{Player, Value};
use slint::ComponentHandle;

use crate::{AppWindow, meter};

/// mpv filter label; also the `af-metadata/<label>` key.
pub const LABEL: &str = "quiet";
/// The gate channel's level below which a frame is dropped, dBFS.
const GATE: f64 = -36.0;
/// Below this is silence, until the speech has been heard: careful.
const START: f64 = -52.0;
/// Seconds between taps, and the length of the frames kept or dropped.
const HOP: f64 = 0.05;
/// How much of each pause stays, in taps: after a word, and before the next.
const BEHIND: usize = 3;
const AHEAD: usize = 5;
/// Levels looked at (one per poll, four a second).
const HISTORY: usize = 120;
const ENOUGH: usize = 12;
const POLL: Duration = Duration::from_millis(250);

/// The lavfi graph for audio with mpv channel layout `layout`. `None` when
/// the layout isn't one that can be restored.
pub fn graph(layout: &str, channels: usize, rate: u32) -> Option<String> {
    if channels == 0 || rate < 4000 {
        return None;
    }
    let hop = (HOP * f64::from(rate)).round() as usize;
    let mix = match channels {
        1 => "c0=c0",
        2 => "c0=0.5*c0+0.5*c1",
        _ => "c0=0.5*c0+0.5*c1+0.5*c2",
    };
    let top = 6000.0_f64.min(f64::from(rate) * 0.45);
    let taps = BEHIND + 1 + AHEAD;
    let mut g = format!(
        "aformat=sample_fmts=fltp|flt,asplit[m][s];[s]pan=mono|{mix},highpass=f=120,lowpass=f={top:.0},\
         asplit[r][v];[v]volume@ref=volume={:.2}dB,asplit={taps}",
        GATE - START
    );
    for i in 0..taps {
        g += &format!("[a{i}]");
    }
    for i in 0..taps {
        // apad: near the end of the file the taps ahead are silent, rather
        // than ending the sound early.
        g += &match i.cmp(&BEHIND) {
            std::cmp::Ordering::Less => format!(";[a{i}]adelay=delays={}S:all=1,apad[b{i}]", (BEHIND - i) * hop),
            std::cmp::Ordering::Equal => format!(";[a{i}]apad[b{i}]"),
            std::cmp::Ordering::Greater => {
                let n = (i - BEHIND) * hop;
                format!(";[a{i}]atrim=start_sample={n},asetpts=PTS-{n}/SR/TB,apad[b{i}]")
            }
        };
    }
    g += ";";
    for i in 0..taps {
        g += &format!("[b{i}]");
    }
    let loudest = (1..taps).fold("abs(val(0))".to_owned(), |e, i| format!("max({e}\\,abs(val({i})))"));
    g += &format!(
        "amerge=inputs={taps},aeval=exprs='{loudest}':c=mono[e];[m][r][e]amerge=inputs=3,asetnsamples=n={hop}:p=0,\
         astats=metadata=1:reset=1:measure_perchannel=Peak_level:measure_overall=none,\
         ametadata=mode=select:key={}:function=greater:value={GATE},pan={layout}",
        peak_key(channels + 2)
    );
    for c in 0..channels {
        g += &format!("|c{c}=c{c}");
    }
    Some(g)
}

/// astats numbers channels from 1.
fn peak_key(channel: usize) -> String {
    format!("lavfi.astats.{channel}.Peak_level")
}

/// Learns how loud the speech is, and how loud its background.
#[derive(Default)]
struct Listener {
    /// The loudest around each moment, and each moment alone, dBFS.
    around: VecDeque<f64>,
    alone: VecDeque<f64>,
    /// What counts as silence now, dBFS.
    threshold: Option<f64>,
}

fn percentile(v: &VecDeque<f64>, p: f64) -> f64 {
    let mut s: Vec<f64> = v.iter().copied().collect();
    s.sort_by(f64::total_cmp);
    s[((s.len() - 1) as f64 * p).round() as usize]
}

impl Listener {
    fn threshold(&self) -> f64 {
        self.threshold.unwrap_or(START)
    }

    /// `gate` is the level measured behind `volume@ref`.
    fn hear(&mut self, alone: f64, gate: f64, gain: f64) {
        for (list, v) in [(&mut self.alone, alone), (&mut self.around, gate - gain)] {
            list.push_back(v.max(-90.0));
            if list.len() > HISTORY {
                list.pop_front();
            }
        }
        if self.around.len() < ENOUGH {
            return;
        }
        // Just over the background, which is heard alone in the kept ends
        // of pauses; but well below the speech, and no pickier than needed
        // in a clean recording.
        let speech = percentile(&self.around, 0.6);
        let background = percentile(&self.alone, 0.05);
        self.threshold = Some((background + 6.0).clamp(speech - 30.0, speech - 18.0).clamp(-70.0, -12.0));
    }
}

#[derive(Default)]
struct State {
    path: String,
    listener: Listener,
    /// The gain `volume@ref` has.
    gain: f64,
    /// The last look: when, where.
    last: Option<(Instant, f64)>,
    /// Seconds of silence left out of this file.
    saved: f64,
    /// The picture is set to catch up at once.
    catching_up: bool,
    /// The last seek, by anyone.
    sought: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { gain: GATE - START, ..Default::default() });
}

/// mpv seeks: the position jumps, but not over silence.
pub fn note_seek() {
    STATE.with_borrow_mut(|s| {
        s.sought = Some(Instant::now());
        s.last = None;
    });
}

/// Whether a jump ahead in the position now comes from silence left out
/// rather than from a seek.
pub fn jumps(player: &Player) -> bool {
    STATE.with_borrow(|s| s.sought.is_none_or(|t| t.elapsed() > Duration::from_secs(1))) && installed(player)
}

fn installed(player: &Player) -> bool {
    player.get_string("af").is_some_and(|af| af.contains(&format!("@{LABEL}:")))
}

fn level(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Str(s) => Some(s.parse::<f64>().ok().filter(|x| !x.is_nan()).unwrap_or(f64::NEG_INFINITY)),
        Value::Double(d) => Some(*d),
        _ => None,
    }
}

/// Seconds of `now - then` at `speed` that playing can't explain. Small
/// differences are just when the position was looked at.
fn left_out(moved: f64, elapsed: f64, speed: f64) -> f64 {
    let d = moved - elapsed * speed;
    if d > 0.15 * speed.max(1.0) { d } else { 0.0 }
}

/// Four times a second: learn the levels, count what was left out, and keep
/// the picture in step.
fn look(ui: &AppWindow, player: &Player, smooth_motion: bool) {
    let path = player.get_string("path").unwrap_or_default();
    let on = installed(player);
    STATE.with_borrow_mut(|s| {
        let mut same_file = true;
        if s.path != path {
            same_file = false;
            s.path = path;
            s.listener = Listener::default();
            s.gain = GATE - START;
            s.last = None;
            s.saved = 0.0;
        }
        let video = on && ui.get_has_video();
        if s.catching_up != video || (video && !same_file) {
            s.catching_up = video;
            // For this file only, since mpv doesn't take its default (-1)
            // back; turned off midway, a tenth of a frame is the same.
            if video {
                let _ = player.set_string("file-local-options/mc", "1");
            } else if same_file {
                let fps = player.get_double("container-fps").filter(|f| *f > 1.0).unwrap_or(25.0);
                let _ = player.set_string("file-local-options/mc", &format!("{:.5}", 0.1 / fps));
            }
            if smooth_motion {
                // Frames timed by the screen would take seconds to catch up.
                let _ = player.set_string("video-sync", if video { "audio" } else { "display-resample" });
            }
        }
        if !on || ui.get_paused() {
            s.last = None;
        } else if let Some(channels) = player
            .get_string("audio-params/channels")
            .and_then(|l| meter::channels(&l))
        {
            let meta = player.get_value(&format!("af-metadata/{LABEL}"));
            let peak = |c| level(meta.as_ref()?.get(&peak_key(c)));
            if let (Some(alone), Some(gate)) = (peak(channels + 1), peak(channels + 2)) {
                let gain = s.gain;
                s.listener.hear(alone, gate, gain);
            }
            // Sent every time: a seek builds the graph anew.
            s.gain = GATE - s.listener.threshold();
            let gain = format!("{:.2}dB", s.gain);
            let _ = player.command(&["af-command", LABEL, "volume", &gain, "volume@ref"]);

            let now = Instant::now();
            if let Some(pos) = player.get_double("time-pos") {
                if let Some((t, p)) = s.last {
                    let speed = player.get_double("speed").unwrap_or(1.0);
                    s.saved += left_out(pos - p, now.duration_since(t).as_secs_f64(), speed);
                }
                s.last = Some((now, pos));
            }
        }
        let saved = if s.saved >= 1.0 { crate::fmt_clock(s.saved) } else { String::new() };
        if ui.get_silence_saved() != saved.as_str() {
            ui.set_silence_saved(saved.as_str().into());
        }
    });
}

/// Whether silence can be left out of what plays now; else why not.
pub fn obstacle(ui: &AppWindow, bit_perfect: bool) -> Option<&'static str> {
    if bit_perfect {
        Some("Bit-perfect output plays everything: turn it off to skip silence")
    } else if ui.get_party_mode() != 0 {
        Some("Silence isn't skipped while watching together")
    } else if crate::cast::active() {
        Some("Silence isn't skipped while casting")
    } else if !ui.get_idle() && ui.get_duration() <= 0.0 {
        Some("Silence can't be skipped in a live stream")
    } else {
        None
    }
}

pub fn install(ui: &AppWindow, player: &std::rc::Rc<Player>, settings: std::rc::Rc<RefCell<crate::settings::Settings>>) {
    let (weak, player) = (ui.as_weak(), player.clone());
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, POLL, move || {
        let Some(ui) = weak.upgrade() else { return };
        let busy = STATE.with_borrow(|s| s.catching_up || s.saved > 0.0);
        if ui.get_skip_silence() || busy {
            look(&ui, &player, settings.borrow().smooth_motion);
        }
    });
    std::mem::forget(timer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_shape() {
        let g = graph("stereo", 2, 44_100).unwrap();
        assert!(g.contains("adelay=delays=2205S"), "{g}");
        assert!(g.contains("[a3]apad[b3];[a4]atrim=start_sample=2205"), "{g}");
        assert!(g.contains("key=lavfi.astats.4.Peak_level"), "{g}");
        assert!(g.ends_with("pan=stereo|c0=c0|c1=c1"), "{g}");
        assert!(graph("mono", 1, 8000).unwrap().contains("lowpass=f=3600"));
        assert!(graph("odd", 0, 44_100).is_none());
    }

    #[test]
    fn learns_the_levels() {
        // Clean: speech at -10, pauses at -70: 30 dB below the speech.
        let mut l = Listener::default();
        assert_eq!(l.threshold(), START);
        for i in 0..60 {
            l.hear(if i % 5 == 0 { -70.0 } else { -12.0 }, -10.0, 0.0);
        }
        assert_eq!(l.threshold(), -40.0);
        // A noisy room: just over the noise.
        let mut l = Listener::default();
        for i in 0..60 {
            l.hear(if i % 5 == 0 { -38.0 } else { -12.0 }, -10.0 + 7.0, 7.0);
        }
        assert_eq!(l.threshold(), -32.0);
        // No pauses at all: nothing that could be speech is cut.
        let mut l = Listener::default();
        for _ in 0..60 {
            l.hear(-12.0, -10.0, 0.0);
        }
        assert_eq!(l.threshold(), -28.0);
        // Digital silence.
        let mut l = Listener::default();
        for _ in 0..60 {
            l.hear(f64::NEG_INFINITY, f64::NEG_INFINITY, 0.0);
        }
        assert!(l.threshold().is_finite());
    }

    #[test]
    fn counts_only_what_was_left_out() {
        assert_eq!(left_out(0.26, 0.25, 1.0), 0.0);
        assert!((left_out(1.75, 0.25, 1.0) - 1.5).abs() < 1e-9);
        assert_eq!(left_out(0.6, 0.25, 2.0), 0.0);
        assert_eq!(left_out(0.0, 0.25, 1.0), 0.0);
    }

    fn ffmpeg(args: &[&str]) -> Option<String> {
        let out = std::process::Command::new("ffmpeg").args(args).output().ok()?;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// 1 s of tone, 2 s of silence, 1 s of tone: the middle is cut, the rest
    /// keeps its times and layout. Runs the `ffmpeg` CLI when present.
    #[test]
    fn cuts_the_pause_in_every_layout() {
        for layout in ["mono", "stereo", "2.1", "5.1", "5.1(side)", "7.1"] {
            for rate in [44_100, 48_000, 22_050] {
                let n = meter::channels(layout).unwrap();
                let src = format!(
                    "aevalsrc=exprs='if(between(t\\,1\\,3)\\,0\\,0.3*sin(2*PI*440*t))':s={rate}:d=4,aformat=channel_layouts={layout}"
                );
                let af = format!("{},ametadata=mode=print:file=-", graph(layout, n, rate).unwrap());
                let Some(out) = ffmpeg(&["-v", "error", "-f", "lavfi", "-i", &src, "-af", &af, "-f", "null", "-"]) else {
                    eprintln!("ffmpeg not found; skipping");
                    return;
                };
                let times: Vec<f64> =
                    out.lines().filter_map(|l| l.split("pts_time:").nth(1)?.trim().parse().ok()).collect();
                let gap = times.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
                assert!((1.3..1.9).contains(&gap), "{layout} {rate}: {gap}");
                let before = times.iter().copied().filter(|t| *t < 2.0).fold(0.0, f64::max);
                let after = times.iter().copied().filter(|t| *t > before).fold(9.0, f64::min);
                assert!(before > 1.02 && after < 2.8, "{layout} {rate}: {before} {after}");
                assert!(times.last().is_some_and(|t| *t > 3.9), "{layout} {rate}: the end is cut off");
            }
        }
    }

    /// End to end through libmpv: 1 s of tone, 4 s of silence, 2 s of tone
    /// with a seek after it: the position jumps the pause, a seek back does
    /// it again, and the levels can be read and set. Skipped without libmpv
    /// or ffmpeg.
    #[test]
    fn jumps_the_pause_while_playing() {
        let dir = std::env::temp_dir().join(format!("smp-silence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("talk.wav");
        let src = "aevalsrc=exprs='if(between(t\\,1\\,5)\\,0\\,0.3*sin(2*PI*440*t))':s=44100:d=8,aformat=channel_layouts=stereo";
        let name = file.to_string_lossy().into_owned();
        if ffmpeg(&["-v", "error", "-y", "-f", "lavfi", "-i", src, &name]).is_none() {
            eprintln!("ffmpeg not found; skipping");
            return;
        }
        let Ok(player) = Player::new(&player_core::PlayerConfig::headless(), |_| {}) else {
            eprintln!("libmpv not available; skipping");
            return;
        };
        let g = graph("stereo", 2, 44_100).unwrap();
        player.set_string("af", &meter::lavfi(LABEL, &g)).unwrap();
        player.load(&name, player_core::LoadMode::Replace).unwrap();
        let wait = |at: f64| {
            let start = Instant::now();
            while player.get_double("time-pos").is_none_or(|t| t < at) {
                assert!(start.elapsed() < Duration::from_secs(8), "stuck before {at}");
                std::thread::sleep(Duration::from_millis(10));
            }
            start.elapsed().as_secs_f64()
        };
        wait(0.5);
        let took = wait(5.2);
        assert!((0.7..2.8).contains(&took), "{took}");
        assert!(installed(&player));
        let meta = player.get_value("af-metadata/quiet").unwrap();
        assert!(level(meta.get(&peak_key(3))).is_some_and(|l| (-14.0..-8.0).contains(&l)), "{meta:?}");
        assert!(level(meta.get(&peak_key(4))).is_some());
        assert!(player.command(&["af-command", LABEL, "volume", "-3.0dB", "volume@ref"]).is_ok());
        player.seek(player_core::Seek::Absolute(0.5)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(player.get_double("time-pos").is_some_and(|t| t < 2.0));
        let took = wait(5.2);
        assert!((0.3..2.8).contains(&took), "after the seek: {took}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
