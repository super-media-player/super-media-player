//! Live spectrum levels for the bars the UI draws itself (the skins' mini
//! spectrum and the bars under the spinning disc).
//!
//! An audio filter (`af`) measures the music as mpv plays it: a mono copy of
//! the sound goes through one band-pass filter per bar, the bands ride along
//! as extra channels while `astats` measures every channel's loudness, and a
//! final `pan` drops them again, so what you hear is untouched. mpv exposes
//! the numbers as `af-metadata/vis`; [`Meter::tick`] turns them into 0..1
//! bar heights.
//!
//! The filter sees audio a little before it is heard (mpv's output buffer,
//! `audio-buffer`, 0.2 s by default), so the levels are held back by that
//! much to line the bars up with the sound.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use player_core::{Player, Value};

/// Number of bars (bands), lowest frequency first.
pub const BANDS: usize = 20;
const LOW_HZ: f64 = 50.0;
const HIGH_HZ: f64 = 14_000.0;
/// mpv filter label; also the `af-metadata/<label>` key.
const LABEL: &str = "vis";

/// Channel layouts the final `pan` can restore by name. Anything else (rare
/// in music) is left alone, without bars.
const LAYOUTS: &[(&str, usize)] = &[
    ("mono", 1),
    ("stereo", 2),
    ("2.1", 3),
    ("3.0", 3),
    ("quad", 4),
    ("4.0", 4),
    ("5.0", 5),
    ("5.0(side)", 5),
    ("5.1", 6),
    ("5.1(side)", 6),
    ("6.1", 7),
    ("7.1", 8),
    ("7.1(wide)", 8),
];

/// Number of channels of a layout that `pan` can restore.
pub fn channels(layout: &str) -> Option<usize> {
    LAYOUTS.iter().find(|(n, _)| *n == layout).map(|l| l.1)
}

/// Band centre frequencies, spaced evenly on a log scale and kept below the
/// Nyquist limit of low sample rates.
fn centers(samplerate: u32) -> Vec<f64> {
    let top = HIGH_HZ.min(f64::from(samplerate) * 0.45);
    (0..BANDS)
        .map(|i| LOW_HZ * (top / LOW_HZ).powf(i as f64 / (BANDS - 1) as f64))
        .collect()
}

/// The lavfi graph measuring audio with layout `layout` (mpv's name for it,
/// e.g. "stereo"). `None` when the layout is not one we can restore.
pub fn graph(layout: &str, samplerate: u32) -> Option<String> {
    let channels = LAYOUTS.iter().find(|(n, _)| *n == layout)?.1;
    let cs = centers(samplerate);
    let width = (cs[BANDS - 1] / cs[0]).log2() / (BANDS - 1) as f64;
    let mix = if channels == 1 {
        "c0=c0"
    } else {
        "c0=0.5*c0+0.5*c1"
    };
    let mut g = format!("asplit[m][s];[s]pan=mono|{mix},asplit={BANDS}");
    for i in 0..BANDS {
        g += &format!("[b{i}]");
    }
    for (i, c) in cs.iter().enumerate() {
        g += &format!(";[b{i}]bandpass=f={c:.0}:width_type=o:w={width:.3}[c{i}]");
    }
    g += ";[m]";
    for i in 0..BANDS {
        g += &format!("[c{i}]");
    }
    g += &format!(
        "amerge=inputs={},astats=metadata=1:reset=1:measure_perchannel=RMS_level:\
         measure_overall=none,pan={layout}",
        BANDS + 1
    );
    for c in 0..channels {
        g += &format!("|c{c}=c{c}");
    }
    Some(g)
}

/// One labelled lavfi filter for mpv's `af` option. `%len%` quoting keeps
/// mpv's option parser away from the graph's own `[`, `:` and `,`.
pub fn lavfi(label: &str, graph: &str) -> String {
    format!("@{label}:lavfi=graph=%{}%{graph}", graph.len())
}

/// The `af` option value for the meter graph alone.
#[cfg(test)]
fn af_value(graph: &str) -> String {
    lavfi(LABEL, graph)
}

/// Label of the tone filter (the silver deck's treble and bass knobs).
const TONE_LABEL: &str = "tone";

/// The sound settings the tone filter applies: the silver deck's knobs and
/// the equalizer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    pub bass: f64,
    pub treble: f64,
    pub eq_enabled: bool,
    pub eq: [f64; 10],
    /// Bit-perfect output: no filters at all.
    pub bypass: bool,
    /// Gain of the fade stage now (0..1), so a chain put on mid-fade starts
    /// there.
    pub fade: f64,
    /// Night mode: loud scenes are compressed and quiet ones lifted.
    pub night: bool,
    /// Dialogue boost: voices are lifted over music and effects.
    pub dialogue: bool,
    /// Karaoke: the centre of a stereo mix (usually the singer) is removed.
    pub karaoke: bool,
    /// Silence is left out (silence.rs).
    pub skip_silence: bool,
}

impl Default for Tone {
    fn default() -> Self {
        Self {
            bass: 0.0,
            treble: 0.0,
            eq_enabled: false,
            eq: [0.0; 10],
            bypass: false,
            fade: 1.0,
            night: false,
            dialogue: false,
            karaoke: false,
            skip_silence: false,
        }
    }
}

/// The tone chain: bass and treble shelves (-12..12 dB), the 10-band
/// equalizer when it is on, a volume stage for fades between songs, and a
/// limiter. At 0 dB everything passes the sound through unchanged. The
/// instance names (`bass@lo`, `eq3@eq`, `volume@xf`, ...) let [`set_tone`],
/// [`set_eq_band`] and [`set_fade`] change one while it plays.
///
/// A boost lifts loud peaks over full scale. In 16-bit audio (CDs) the
/// filters would clip them (and FFmpeg logs "clipping ... reduce gain"), so
/// they work in floating point, which has headroom, and a limiter then holds
/// the peaks just under full scale. Below full scale the limiter does
/// nothing; `level=0` stops it from also normalizing the volume.
fn tone_graph(t: &Tone, layout: &str) -> String {
    let mut g = String::from("aformat=sample_fmts=fltp|flt,");
    g += &voice_stages(t, layout);
    g += &format!("bass@lo=g={:.1}:f=100,treble@hi=g={:.1}:f=3000,", t.bass, t.treble);
    if t.eq_enabled {
        for (i, (f, gain)) in crate::settings::EQ_BANDS.iter().zip(t.eq).enumerate() {
            // One octave wide peaking filters, one per band.
            g += &format!("equalizer@eq{i}=f={f:.0}:t=o:w=1:g={gain:.1},");
        }
    }
    if t.night {
        // About -26 dB threshold, 6:1, with make-up gain: explosions come
        // down, whispers come up. The limiter below catches what is left.
        g += "acompressor@night=threshold=0.05:ratio=6:attack=5:release=300:makeup=4:knee=4,";
    }
    g + &format!(
        "volume@xf=volume={:.3},alimiter@lim=limit=1:level=0:attack=5:release=50",
        t.fade.clamp(0.0, 1.0)
    )
}

/// Index of the front-centre channel in mpv layout `layout`, where there is one.
fn centre_channel(layout: &str) -> Option<usize> {
    match layout {
        "3.0" | "5.0" | "5.0(side)" | "5.1" | "5.1(side)" | "6.1" | "7.1" | "7.1(wide)" => Some(2),
        _ => None,
    }
}

/// The stages that work on voices, ahead of the tone controls: karaoke
/// (stereo only: the singer is usually mixed to the middle, so the middle
/// of everything above the bass is turned down) and dialogue boost (a film's
/// centre channel turned up; in stereo, FFmpeg's dialogue filter pulls the
/// voices out and they are mixed back in louder). Each ends with the
/// audio's own layout, so the rest of the chain and the meter are unchanged.
fn voice_stages(t: &Tone, layout: &str) -> String {
    let mut g = String::new();
    if t.karaoke && layout == "stereo" {
        g += "asplit=2[kl][kh];[kl]lowpass=f=160,lowpass=f=160[klo];\
              [kh]highpass=f=160,highpass=f=160,stereotools=mlev=0.04[khi];\
              [klo][khi]amix=inputs=2:normalize=0,";
    }
    if t.dialogue {
        if layout == "stereo" {
            g += "dialoguenhance=original=1:enhance=2:voice=2,pan=stereo|c0=c0+c2|c1=c1+c2,";
        } else if let (Some(c), Some((_, n))) =
            (centre_channel(layout), LAYOUTS.iter().find(|(l, _)| *l == layout))
        {
            g += &format!("pan={layout}");
            for i in 0..*n {
                g += &if i == c { format!("|c{i}=2.0*c{i}") } else { format!("|c{i}=c{i}") };
            }
            g += ",";
        }
    }
    g
}

/// Change bass and treble on the playing file at once (`af-command`, no
/// break in the sound). Fails harmlessly when nothing plays; the next file
/// starts with the values from [`Meter::tone`].
pub fn set_tone(player: &Player, bass_db: f64, treble_db: f64) {
    let bass = format!("{bass_db:.1}");
    let treble = format!("{treble_db:.1}");
    let _ = player.command(&["af-command", TONE_LABEL, "g", &bass, "bass@lo"]);
    let _ = player.command(&["af-command", TONE_LABEL, "g", &treble, "treble@hi"]);
}

/// Change one equalizer band on the playing file.
pub fn set_eq_band(player: &Player, band: usize, gain_db: f64) {
    let g = format!("{gain_db:.1}");
    let inst = format!("eq{band}@eq");
    let _ = player.command(&["af-command", TONE_LABEL, "g", &g, &inst]);
}

/// Set the fade stage's gain (0..1) on the playing file.
pub fn set_fade(player: &Player, gain: f64) {
    let g = format!("{:.3}", gain.clamp(0.0, 1.0));
    let _ = player.command(&["af-command", TONE_LABEL, "volume", &g, "volume@xf"]);
}

/// Gain for a fade between songs `secs` long at `pos` of `duration`
/// seconds: down over the last `secs` (only when another song follows),
/// up over the first half of that. 1.0 when fading is off or the length is
/// unknown.
pub fn fade_gain(secs: f64, pos: f64, duration: f64, next_follows: bool) -> f64 {
    if secs <= 0.0 || duration < secs * 4.0 {
        return 1.0;
    }
    let fade_in = (pos / (secs / 2.0)).clamp(0.0, 1.0);
    let fade_out = if next_follows {
        ((duration - pos) / secs).clamp(0.0, 1.0)
    } else {
        1.0
    };
    // Equal-power shape: sounds even, unlike a straight line.
    (fade_in * fade_out).sqrt()
}

/// Bar height 0..1 for band `i` measured at `db` (RMS, dBFS). Music gets
/// quieter towards the treble (about 1 dB per band here), so higher bands
/// get a matching boost to keep the bars level on average.
fn height(i: usize, db: f64) -> f32 {
    const FLOOR_DB: f64 = -52.0;
    const RANGE_DB: f64 = 44.0;
    if !db.is_finite() {
        return 0.0;
    }
    let tilted = db + 0.8 * i as f64;
    ((tilted - FLOOR_DB) / RANGE_DB).clamp(0.0, 1.0) as f32
}

/// Bar heights from an `af-metadata/vis` reply. The bands come after the
/// audio's own `channels` (astats numbers channels from 1).
fn parse(meta: &Value, channels: usize) -> Option<Vec<f32>> {
    (0..BANDS)
        .map(|i| {
            let key = format!("lavfi.astats.{}.RMS_level", channels + 1 + i);
            let db = match meta.get(&key)? {
                Value::Str(s) => s.parse::<f64>().unwrap_or(f64::NEG_INFINITY),
                Value::Double(d) => *d,
                _ => return None,
            };
            Some(height(i, db))
        })
        .collect()
}

/// A real video track is playing (cover art doesn't count).
fn mpv_has_video(player: &Player) -> bool {
    player.get_string("current-tracks/video/id").is_some()
        && player.get_string("current-tracks/video/albumart").as_deref() != Some("yes")
}

/// What the file-local filter chain was built for: path, channel layout,
/// sample rate, whether it measures (music without video), equalizer on,
/// bypassed (bit-perfect), and night / dialogue / karaoke / skip silence.
type Installed = (String, String, u32, bool, bool, bool, [bool; 4]);

/// Puts the audio filters on each file (tone, and for music the meter) and
/// feeds the UI bars. Owned by the UI thread; call [`Meter::tick`] ~30 times
/// a second.
#[derive(Default)]
pub struct Meter {
    installed: Option<Installed>,
    channels: usize,
    /// The measuring filter for the current file is in place.
    measuring: bool,
    /// Tone and equalizer, shared with the knobs' and the equalizer's
    /// callbacks.
    pub tone: std::rc::Rc<std::cell::Cell<Tone>>,
    /// Measured levels waiting to be shown, oldest first.
    pending: VecDeque<(Instant, Vec<f32>)>,
    /// What the bars show now, falling smoothly like a hardware analyzer.
    shown: Vec<f32>,
    /// What is being heard now, as measured (not smoothed): for beat
    /// tracking (pulse.rs).
    heard: Option<Vec<f32>>,
    last_tick: Option<Instant>,
}

impl Meter {
    /// Keep the filters on the current file (`has_video`: no meter needed)
    /// and return the bar heights to show while `bars` are on screen, or
    /// `None` when nothing changed.
    pub fn tick(
        &mut self,
        player: &Player,
        bars: bool,
        has_video: bool,
        paused: bool,
    ) -> Option<Vec<f32>> {
        let now = Instant::now();
        let dt = self
            .last_tick
            .replace(now)
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32().min(0.2));
        if self.shown.len() != BANDS {
            self.shown = vec![0.0; BANDS];
        }

        // mpv knows whether the file has a real video track before the UI
        // does (the track list arrives as an event); deciding from the UI
        // alone put the meter on a movie for a moment, and every change of
        // filters restarts the sound.
        let video = has_video || mpv_has_video(player);
        self.install(player, !video);
        let mut target = None;
        if bars && self.measuring && !paused {
            if let Some(levels) = player
                .get_value(&format!("af-metadata/{LABEL}"))
                .and_then(|m| parse(&m, self.channels))
            {
                self.pending.push_back((now, levels));
            }
            // Show what is being heard now: measured `audio-buffer` ago.
            let lead = player.get_double("audio-buffer").unwrap_or(0.2) + 0.04;
            let lead = Duration::from_secs_f64(lead.clamp(0.0, 2.0));
            while self.pending.len() > 1 && now.duration_since(self.pending[1].0) >= lead {
                self.pending.pop_front();
            }
            if let Some((t, l)) = self.pending.front()
                && now.duration_since(*t) >= lead
            {
                target = Some(l.clone());
            }
        }
        if paused || !bars {
            self.pending.clear();
        }
        self.heard.clone_from(&target);

        // Rise at once, fall at a steady rate (about 0.4 s from full height).
        let fall = 2.5 * dt;
        let mut changed = false;
        for (i, s) in self.shown.iter_mut().enumerate() {
            let t = target.as_ref().map_or(0.0, |l| l[i]);
            let new = if t >= *s { t } else { (*s - fall).max(t) };
            if (new - *s).abs() > 0.001 {
                changed = true;
            }
            *s = new;
        }
        changed.then(|| self.shown.clone())
    }

    /// The levels being heard now, unsmoothed (None while nothing is
    /// measured, e.g. paused).
    pub fn heard(&self) -> Option<&[f32]> {
        self.heard.as_deref()
    }

    /// Put the filters on the current file once its audio format is known:
    /// the tone filter always, the meter for music (`measure`). They are
    /// file-local, so mpv drops them by itself when the next file starts,
    /// and that file gets filters built for its own layout and the current
    /// tone settings.
    fn install(&mut self, player: &Player, measure: bool) {
        let path = player.get_string("path").unwrap_or_default();
        let layout = player.get_string("audio-params/channels").unwrap_or_default();
        let rate = player
            .get_double("audio-params/samplerate")
            .unwrap_or(0.0) as u32;
        if path.is_empty() || layout.is_empty() || rate == 0 {
            return;
        }
        let tone = self.tone.get();
        let key = (
            path,
            layout,
            rate,
            measure,
            tone.eq_enabled,
            tone.bypass,
            [tone.night, tone.dialogue, tone.karaoke, tone.skip_silence],
        );
        // mpv drops file-local options when a file ends, so the same file
        // played again (repeat one) needs the filters put back.
        let present = tone.bypass
            || player
                .get_string("af")
                .is_some_and(|af| af.contains(&format!("@{TONE_LABEL}:")));
        if self.installed.as_ref() == Some(&key) && present {
            return;
        }
        if tone.bypass {
            // Bit-perfect: nothing between the decoder and the sound card.
            if let Err(e) = player.set_string("file-local-options/af", "") {
                log::warn!("audio filters: {e}");
            }
            self.measuring = false;
            self.pending.clear();
            self.installed = Some(key);
            return;
        }
        let mut af = lavfi(TONE_LABEL, &tone_graph(&tone, &key.1));
        if tone.skip_silence
            && let Some(g) = channels(&key.1).and_then(|n| crate::silence::graph(&key.1, n, rate))
        {
            // First: it judges the sound as it is in the file.
            af = format!("{},{af}", lavfi(crate::silence::LABEL, &g));
        }
        let meter = if measure { graph(&key.1, rate) } else { None };
        if measure && meter.is_none() {
            log::info!("spectrum meter: no bars for channel layout {}", key.1);
        }
        if let Some(g) = &meter {
            af += ",";
            af += &lavfi(LABEL, g);
        }
        let ok = match player.set_string("file-local-options/af", &af) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("audio filters: {e}");
                false
            }
        };
        self.channels = channels(&key.1).unwrap_or(0);
        self.measuring = ok && meter.is_some();
        self.pending.clear();
        // Remembered even after a failure, so it isn't retried 30 times a
        // second.
        self.installed = Some(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_shape() {
        let g = graph("stereo", 44_100).unwrap();
        assert!(g.starts_with("asplit[m][s];[s]pan=mono|c0=0.5*c0+0.5*c1,asplit=20[b0]"));
        assert!(g.contains(&format!("amerge=inputs={}", BANDS + 1)));
        assert!(g.ends_with("pan=stereo|c0=c0|c1=c1"));
        assert!(graph("mono", 44_100).unwrap().ends_with("pan=mono|c0=c0"));
        assert!(graph("5.1(side)", 48_000).unwrap().ends_with("|c5=c5"));
        assert!(graph("weird", 48_000).is_none());
        // Bands stay under Nyquist for low-rate audio.
        assert!(centers(22_050).iter().all(|&c| c < 11_025.0));
        assert!(af_value("a,b").starts_with("@vis:lavfi=graph=%3%a,b"));
    }

    #[test]
    fn tone_filter() {
        let mut t = Tone { bass: 3.0, treble: -2.5, ..Default::default() };
        let g = tone_graph(&t, "stereo");
        assert!(g.starts_with("aformat=sample_fmts=fltp|flt,"), "{g}");
        assert!(
            g.contains("bass@lo=g=3.0:f=100,treble@hi=g=-2.5:f=3000,volume@xf=volume=1.000,alimiter@lim="),
            "{g}"
        );
        t.eq_enabled = true;
        t.eq[3] = -4.0;
        let g = tone_graph(&t, "stereo");
        assert!(g.contains("equalizer@eq0=f=31:t=o:w=1:g=0.0,"), "{g}");
        assert!(g.contains("equalizer@eq3=f=250:t=o:w=1:g=-4.0,"), "{g}");
        assert!(g.contains("equalizer@eq9=f=16000:"), "{g}");
        assert!(lavfi("tone", "x=1").starts_with("@tone:lavfi=graph=%3%x=1"));
    }

    #[test]
    fn fades() {
        assert_eq!(fade_gain(0.0, 1.0, 200.0, true), 1.0);
        assert_eq!(fade_gain(6.0, 0.0, 200.0, true), 0.0);
        assert_eq!(fade_gain(6.0, 100.0, 200.0, true), 1.0);
        assert!(fade_gain(6.0, 197.0, 200.0, true) < 0.75);
        // The last song plays out; short clips are left alone.
        assert_eq!(fade_gain(6.0, 199.0, 200.0, false), 1.0);
        assert_eq!(fade_gain(6.0, 1.0, 20.0, true), 1.0);
    }

    #[test]
    fn heights() {
        assert_eq!(height(0, f64::NEG_INFINITY), 0.0);
        assert_eq!(height(0, -80.0), 0.0);
        assert_eq!(height(0, 0.0), 1.0);
        assert!(height(BANDS - 1, -30.0) > height(0, -30.0));
        let meta = Value::Map(
            (1..=BANDS + 2)
                .map(|c| (format!("lavfi.astats.{c}.RMS_level"), Value::Str("-30.0".into())))
                .collect(),
        );
        let l = parse(&meta, 2).unwrap();
        assert_eq!(l.len(), BANDS);
        assert!(parse(&meta, 3).is_none()); // band 20 would be channel 24
    }

    /// Fully boosted on loud 16-bit audio (a CD), the tone filter must not
    /// clip. Runs the `ffmpeg` CLI when present.
    #[test]
    fn boosted_tone_does_not_clip() {
        let src = "sine=f=60:d=1,volume=8,alimiter=limit=1:level=0,aformat=sample_fmts=s16:channel_layouts=stereo";
        // Everything at full boost, equalizer included, mid-fade.
        let tone = Tone { bass: 12.0, treble: 12.0, eq_enabled: true, eq: [12.0; 10], fade: 0.5, ..Default::default() };
        let Ok(out) = std::process::Command::new("ffmpeg")
            .args(["-v", "warning", "-f", "lavfi", "-i", src, "-af", &tone_graph(&tone, "stereo"), "-f", "null", "-"])
            .output()
        else {
            eprintln!("ffmpeg not found; skipping");
            return;
        };
        let log = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{log}");
        assert!(!log.contains("clipping"), "{log}");
    }

    /// Night mode, dialogue boost and karaoke must be valid FFmpeg on every
    /// layout, and leave the layout as it was. Runs the `ffmpeg` CLI when
    /// present.
    #[test]
    fn voice_stages_run_in_ffmpeg() {
        let t = Tone { night: true, dialogue: true, karaoke: true, ..Default::default() };
        assert!(tone_graph(&t, "stereo").contains("stereotools"));
        assert!(tone_graph(&t, "5.1").contains("|c2=2.0*c2"));
        assert!(!tone_graph(&t, "mono").contains("stereotools"));
        for (layout, _) in LAYOUTS {
            let src = format!("sine=f=440:d=0.3,aformat=channel_layouts={layout}");
            let g = format!("{},aformat=channel_layouts={layout}", tone_graph(&t, layout));
            let Ok(out) = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i", &src, "-af", &g, "-f", "null", "-"])
                .output()
            else {
                eprintln!("ffmpeg not found; skipping");
                return;
            };
            assert!(out.status.success(), "{layout}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    /// The graph must be valid FFmpeg for every layout we claim, or mpv drops
    /// the filter. Runs the `ffmpeg` CLI when present.
    #[test]
    fn graphs_run_in_ffmpeg() {
        for (layout, _) in LAYOUTS {
            let g = graph(layout, 44_100).unwrap();
            let src = format!("sine=f=440:d=0.2,aformat=channel_layouts={layout}");
            let Ok(out) = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i", &src, "-af", &g, "-f", "null", "-"])
                .output()
            else {
                eprintln!("ffmpeg not found; skipping");
                return;
            };
            assert!(
                out.status.success(),
                "{layout}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// End to end through libmpv: a 1 kHz tone lights the bars around 1 kHz
    /// and leaves the bass and treble low. Skipped without libmpv or ffmpeg.
    #[test]
    fn tone_lights_its_band() {
        let dir = std::env::temp_dir().join(format!("smp-meter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tone.wav");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=1000:d=3", "-ac", "2"])
            .arg(&file)
            .status()
            .is_ok_and(|s| s.success());
        let Ok(player) = Player::new(&player_core::PlayerConfig::headless(), |_| {}) else {
            eprintln!("libmpv not available; skipping");
            return;
        };
        if !made {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        player
            .load(&file.to_string_lossy(), player_core::LoadMode::Replace)
            .unwrap();
        let mut meter = Meter::default();
        let mut peak = vec![0.0f32; BANDS];
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(1500) {
            if let Some(l) = meter.tick(&player, true, false, false) {
                for (p, v) in peak.iter_mut().zip(l) {
                    *p = p.max(v);
                }
            }
            std::thread::sleep(Duration::from_millis(33));
        }
        // The tone filter rides along and takes live changes.
        assert!(player.get_string("af").unwrap_or_default().contains("@tone:"));
        assert!(player.command(&["af-command", "tone", "g", "6.0", "bass@lo"]).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
        let loudest = (0..BANDS).max_by(|&a, &b| peak[a].total_cmp(&peak[b])).unwrap();
        let hz = centers(44_100)[loudest];
        assert!((600.0..1700.0).contains(&hz), "loudest band {hz} Hz: {peak:?}");
        assert!(peak[0] < 0.3 && peak[BANDS - 1] < 0.3, "{peak:?}");
    }
}
