//! The music overlays besides the pulse line (pulse.rs), drawn by the UI
//! over any music visual: ribbons, melody trail, heat map, ring and part
//! lamps.
//!
//! Fed the meter's band levels (meter.rs: 20 bands, bass first, unsmoothed,
//! lined up with what is heard), the beats from pulse.rs and, for the
//! melody trail, the notes (pitch.rs), about 30 times a second. It keeps the
//! last `SPAN` ticks and turns them into SVG path commands (in a viewbox
//! `SPAN` wide and as tall as the overlay's shape, so nothing is stretched)
//! or, for the heat map, a small picture the UI scales up.
//!
//! Songs differ a lot in level (a quiet acoustic track barely reaches half
//! of a loud pop one), so everything is scaled to the song's own recent
//! range (`Range`), which adapts over a few seconds: one range for all bands
//! (heat map, ring: the shape of the sound stays), one per group (ribbons,
//! lamps: each part's own ups and downs show).
//!
//! The band groups: bass 50–165 Hz (bands 0–4), the voice's range
//! 170 Hz–2.4 kHz (6–13), bright sounds 3–14 kHz (14–19: snare, hi-hats,
//! cymbals). Instruments sharing a range share a lane: this measures where
//! the sound is, it doesn't take the mix apart.

use std::collections::VecDeque;
use std::fmt::Write;

use crate::meter::BANDS;
use crate::pitch::{Note, Notes};

/// Ticks kept and shown: 10 s at the meter's ~30 a second.
pub const SPAN: usize = 300;
/// The melody trail's note range, MIDI (A1 .. F♯6), bottom to top.
pub const MIDI_LO: f32 = 33.0;
pub const MIDI_HI: f32 = 90.0;
/// Room left above and below that range, as a share of the height.
pub const NOTE_PAD: f32 = 0.08;
/// Number of part lamps: kick, bass, voice, snare, cymbals.
pub const LAMPS: usize = 5;

const BASS: std::ops::RangeInclusive<usize> = 0..=4;
const VOICE: std::ops::RangeInclusive<usize> = 6..=13;
const BRIGHT: std::ops::RangeInclusive<usize> = 14..=19;
const KICK: std::ops::RangeInclusive<usize> = 0..=2;
const SNARE: std::ops::RangeInclusive<usize> = 9..=15;

/// A level's recent range, to scale it to 0..1: the edges follow a new
/// high or low at once and creep back towards the level over a few seconds.
#[derive(Clone, Copy, Default)]
struct Range {
    lo: f32,
    hi: f32,
    started: bool,
}

impl Range {
    /// Creep per tick (~0.1 a second).
    const CREEP: f32 = 0.0035;
    /// Smallest range: steady sound isn't blown up into big swings.
    const MIN: f32 = 0.2;

    fn update(&mut self, lo: f32, hi: f32) {
        if !self.started {
            *self = Self { lo, hi: hi.max(lo + Self::MIN), started: true };
            return;
        }
        self.hi = (self.hi - Self::CREEP).max(hi);
        self.lo = (self.lo + Self::CREEP).min(lo);
    }

    /// `v` within the range, 0..1 (a steady level ends up at the top).
    fn scale(&self, v: f32) -> f32 {
        let span = (self.hi - self.lo).max(Self::MIN);
        ((v - self.hi + span) / span).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy, Default)]
struct Frame {
    /// Loudness of each group, 0..1 (scaled to its range).
    bass: f32,
    voice: f32,
    bright: f32,
    /// The beat that landed on this tick (pulse.rs `hit`), 0 for none.
    beat: f32,
    melody: Option<Note>,
    bass_note: Option<Note>,
}

#[derive(Default)]
pub struct Overlay {
    frames: VecDeque<Frame>,
    heat: VecDeque<[f32; BANDS]>,
    last: Option<[f32; BANDS]>,
    /// The ring's band levels, smoothed a little.
    ring: [f32; BANDS],
    lamps: [f32; LAMPS],
    notes: Notes,
    /// All bands' range, and each group's (bass, voice, bright).
    all: Range,
    groups: [Range; 3],
}

fn mean(l: &[f32], r: std::ops::RangeInclusive<usize>) -> f32 {
    let n = r.clone().count() as f32;
    l[r].iter().sum::<f32>() / n
}

/// How much a group rose since the last tick (a hit).
fn rise(now: &[f32], last: &[f32], r: std::ops::RangeInclusive<usize>) -> f32 {
    let n = r.clone().count() as f32;
    r.map(|i| (now[i] - last[i]).max(0.0)).sum::<f32>() / n
}

impl Overlay {
    /// One meter tick. `levels`: band heights (None when nothing is heard,
    /// e.g. paused: everything holds still); `notes`: the note meter's
    /// energies while it runs; `beat`: pulse.rs `hit`.
    pub fn tick(&mut self, levels: Option<&[f32]>, notes: Option<&[f32]>, beat: f32) {
        let Some(l) = levels.filter(|l| l.len() == BANDS) else {
            return;
        };
        let (lo, hi) = l.iter().fold((1f32, 0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        self.all.update(lo, hi);
        let mut now = [0.0; BANDS];
        for (n, &v) in now.iter_mut().zip(l) {
            *n = self.all.scale(v);
        }
        let (melody, bass_note) = self.notes.tick(notes);
        let mut g = [mean(l, BASS), mean(l, VOICE), mean(l, BRIGHT)];
        for (v, r) in g.iter_mut().zip(&mut self.groups) {
            // Its own ups and downs, weighed by how loud it is against the
            // rest (a steady quiet part stays thin).
            r.update(*v, *v);
            *v = r.scale(*v) * (0.3 + 0.7 * self.all.scale(*v));
        }
        let frame = Frame {
            bass: g[0],
            voice: g[1],
            bright: g[2],
            beat,
            melody,
            bass_note,
        };
        let last = self.last.unwrap_or(now);
        let raw = [
            (rise(&now, &last, KICK) * 2.5 + beat * 0.7).min(1.0),
            frame.bass,
            frame.voice,
            (rise(&now, &last, SNARE) * 4.0).min(1.0),
            frame.bright,
        ];
        for (s, r) in self.lamps.iter_mut().zip(raw) {
            // Light at once, fade in about a quarter of a second.
            *s = if r > *s { r } else { *s * 0.8 };
        }
        for (s, v) in self.ring.iter_mut().zip(now) {
            *s += (v - *s) * 0.45;
        }
        self.last = Some(now);
        push(&mut self.frames, frame);
        push(&mut self.heat, now);
    }

    /// Forget the song (a new one starts empty).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// x of frame `i` of the kept ones: the newest at the right edge.
    fn x(&self, i: usize) -> f32 {
        (SPAN - self.frames.len() + i) as f32
    }

    /// Ribbons: the filled shapes of the bass, voice and bright lanes (top
    /// to bottom, each a quarter of `view_h` tall), then the beat lane's
    /// ticks as strokes.
    pub fn ribbons(&self, view_h: f32) -> [String; 4] {
        let lane = view_h / 4.0;
        let shape = |k: usize, get: fn(&Frame) -> f32| -> String {
            let cy = lane * (k as f32 + 0.5);
            let half = |f: &Frame| get(f).powf(1.2) * lane * 0.46 + 0.6;
            let mut out = String::with_capacity(self.frames.len() * 24);
            if self.frames.len() < 2 {
                return out;
            }
            for (i, f) in self.frames.iter().enumerate() {
                let c = if i == 0 { 'M' } else { 'L' };
                let _ = write!(out, "{c}{} {:.1} ", self.x(i), cy - half(f));
            }
            for (i, f) in self.frames.iter().enumerate().rev() {
                let _ = write!(out, "L{} {:.1} ", self.x(i), cy + half(f));
            }
            out.push('Z');
            out
        };
        let cy = lane * 3.5;
        let mut beats = String::new();
        for (i, f) in self.frames.iter().enumerate() {
            if f.beat > 0.0 {
                let a = f.beat * lane * 0.42;
                let _ = write!(beats, "M{} {:.1} L{} {:.1} ", self.x(i), cy - a, self.x(i), cy + a);
            }
        }
        [
            shape(0, |f| f.bass),
            shape(1, |f| f.voice),
            shape(2, |f| f.bright),
            beats,
        ]
    }

    /// Melody trail: the filled trails of the melody and of the bass note
    /// (thicker when louder), and a thin line from top to bottom on each beat.
    pub fn melody(&self, view_h: f32) -> [String; 3] {
        let trail = |get: fn(&Frame) -> Option<Note>, base: f32, gain: f32| -> String {
            let mut out = String::new();
            let mut run: Vec<(f32, f32, f32)> = Vec::new();
            let mut flush = |run: &mut Vec<(f32, f32, f32)>| {
                if run.len() >= 2 {
                    for (j, (x, y, w)) in run.iter().enumerate() {
                        let c = if j == 0 { 'M' } else { 'L' };
                        let _ = write!(out, "{c}{x} {:.1} ", y - w);
                    }
                    for (x, y, w) in run.iter().rev() {
                        let _ = write!(out, "L{x} {:.1} ", y + w);
                    }
                    out.push_str("Z ");
                }
                run.clear();
            };
            for (i, f) in self.frames.iter().enumerate() {
                match get(f) {
                    Some(n) => run.push((self.x(i), note_y(n.midi, view_h), base + gain * n.level)),
                    None => flush(&mut run),
                }
            }
            flush(&mut run);
            out
        };
        let mut beats = String::new();
        for (i, f) in self.frames.iter().enumerate() {
            if f.beat > 0.0 {
                let _ = write!(beats, "M{} 0 L{} {view_h:.1} ", self.x(i), self.x(i));
            }
        }
        [
            trail(|f| f.melody, 0.35, 1.3),
            trail(|f| f.bass_note, 0.25, 0.8),
            beats,
        ]
    }

    /// The melody and bass notes now, for the trail's pens.
    pub fn notes_now(&self) -> (Option<Note>, Option<Note>) {
        self.frames
            .back()
            .map_or((None, None), |f| (f.melody, f.bass_note))
    }

    /// Heat map: `SPAN` × `BANDS` pixels, treble at the top, the newest
    /// column at the right; quiet parts are see-through and the old end
    /// fades out.
    pub fn heat_map(&self) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
        let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(SPAN as u32, BANDS as u32);
        let start = SPAN - self.heat.len();
        let px = buf.make_mut_slice();
        for (j, col) in self.heat.iter().enumerate() {
            let x = start + j;
            let fade = (x as f32 / (SPAN as f32 * 0.25)).min(1.0);
            for (b, &v) in col.iter().enumerate() {
                let [r, g, bl] = heat_colour(v);
                let a = ((0.2 + 0.8 * v.powf(0.7)) * fade * 255.0) as u8;
                px[(BANDS - 1 - b) * SPAN + x] = slint::Rgba8Pixel { r, g, b: bl, a };
            }
        }
        buf
    }

    /// The beats as short ticks along the bottom of a `view_h` tall strip
    /// (under the heat map, which leaves the lowest 8% free).
    pub fn beat_ticks(&self, view_h: f32) -> String {
        let mut out = String::new();
        for (i, f) in self.frames.iter().enumerate() {
            if f.beat > 0.0 {
                let top = view_h * (1.0 - 0.07 * f.beat);
                let _ = write!(out, "M{} {top:.1} L{} {view_h:.1} ", self.x(i), self.x(i));
            }
        }
        out
    }

    /// Ring: a closed shape in a 200 × 200 viewbox around its middle, the
    /// bass pushing out the bottom and the treble the top (mirrored left and
    /// right); `beat` (0..1) swells it. The picture covers up to radius ~52.
    pub fn ring(&self, beat: f32) -> String {
        const N: usize = BANDS * 2;
        let pts: Vec<(f32, f32)> = (0..N)
            .map(|k| {
                let band = if k < BANDS { k } else { N - 1 - k };
                let a = std::f32::consts::FRAC_PI_2
                    - (k as f32 + 0.5) / N as f32 * std::f32::consts::TAU;
                let r = 56.0 + 4.0 * beat + self.ring[band].powf(1.2) * 40.0;
                (100.0 + a.cos() * r, 100.0 + a.sin() * r)
            })
            .collect();
        // Smooth: a curve through the midpoints, each point its control.
        let mid = |p: (f32, f32), q: (f32, f32)| ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
        let (sx, sy) = mid(pts[0], pts[1]);
        let mut out = format!("M{sx:.1} {sy:.1} ");
        for k in 1..=N {
            let p = pts[k % N];
            let (mx, my) = mid(p, pts[(k + 1) % N]);
            let _ = write!(out, "Q{:.1} {:.1} {mx:.1} {my:.1} ", p.0, p.1);
        }
        out.push('Z');
        out
    }

    /// How bright the sound is now, 0 (dark, bass-heavy) .. 1 (bright): the
    /// ring's colour.
    pub fn tone(&self) -> f32 {
        let total: f32 = self.ring.iter().sum();
        if total <= 1e-3 {
            return 0.4;
        }
        let centre: f32 = self.ring.iter().enumerate().map(|(i, v)| i as f32 * v).sum::<f32>() / total;
        ((centre - 7.5) / 5.0).clamp(0.0, 1.0)
    }

    /// The part lamps now, 0..1: kick, bass, voice, snare, cymbals.
    pub fn lamps(&self) -> [f32; LAMPS] {
        self.lamps
    }
}

/// y of note `midi` in a melody trail `view_h` tall (the UI places its note
/// labels with the same sum).
pub fn note_y(midi: f32, view_h: f32) -> f32 {
    let f = ((midi - MIDI_LO) / (MIDI_HI - MIDI_LO)).clamp(0.0, 1.0);
    view_h * (NOTE_PAD + (1.0 - f) * (1.0 - 2.0 * NOTE_PAD))
}

/// The heat map's colours: dark violet, violet, cyan, near white (the
/// default theme's accents).
fn heat_colour(v: f32) -> [u8; 3] {
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.0, [30.0, 18.0, 60.0]),
        (0.3, [70.0, 36.0, 140.0]),
        (0.55, [164.0, 107.0, 255.0]),
        (0.8, [57.0, 208.0, 255.0]),
        (1.0, [236.0, 252.0, 255.0]),
    ];
    let v = v.clamp(0.0, 1.0);
    for w in STOPS.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if v <= b {
            let t = (v - a) / (b - a);
            return [0, 1, 2].map(|k| (ca[k] + (cb[k] - ca[k]) * t) as u8);
        }
    }
    [236, 252, 255]
}

fn push<T>(q: &mut VecDeque<T>, v: T) {
    q.push_back(v);
    while q.len() > SPAN {
        q.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loud(bands: std::ops::Range<usize>, v: f32) -> [f32; BANDS] {
        let mut l = [0.25; BANDS];
        l[bands].fill(v);
        l
    }

    #[test]
    fn lanes_follow_their_range() {
        let mut o = Overlay::default();
        // A few seconds: the ranges settle.
        for _ in 0..80 {
            o.tick(Some(&loud(0..5, 0.9)), None, 0.0);
        }
        let f = *o.frames.back().unwrap();
        assert!(f.bass > 0.8 && f.voice < 0.35 && f.bright < 0.35, "{} {} {}", f.bass, f.voice, f.bright);
        let [bass, voice, bright, beats] = o.ribbons(75.0);
        assert!(bass.starts_with("M220 ") && bass.ends_with('Z'), "{bass}");
        assert!(!voice.is_empty() && !bright.is_empty());
        assert!(beats.is_empty());
        o.tick(Some(&loud(0..5, 0.9)), None, 1.0);
        assert!(o.ribbons(75.0)[3].starts_with("M299 "));
        // The ring swells at the bottom (bass), not at the top.
        assert!(o.tone() < 0.2);
    }

    #[test]
    fn paused_holds_still_and_heat_map_has_its_size() {
        let mut o = Overlay::default();
        o.tick(Some(&loud(14..20, 0.9)), None, 0.0);
        o.tick(None, None, 0.0);
        assert_eq!(o.frames.len(), 1);
        let img = o.heat_map();
        assert_eq!((img.width(), img.height()), (SPAN as u32, BANDS as u32));
        // The newest column: bright at the top, faint at the bottom.
        let px = img.as_slice();
        let top = px[SPAN - 1];
        let bottom = px[(BANDS - 1) * SPAN + SPAN - 1];
        assert!(top.a > bottom.a && top.g > bottom.g);
        // Columns not filled yet stay see-through.
        assert_eq!(px[0].a, 0);
    }

    #[test]
    fn ranges_follow_the_song() {
        let mut r = Range::default();
        r.update(0.1, 0.5);
        assert_eq!(r.scale(0.5), 1.0);
        assert_eq!(r.scale(0.1), 0.0);
        // A quiet song: the range creeps down to it.
        for _ in 0..200 {
            r.update(0.0, 0.2);
        }
        assert!(r.scale(0.2) > 0.95 && r.scale(0.0) < 0.05, "{} {}", r.lo, r.hi);
    }

    #[test]
    fn note_rows_run_bottom_to_top() {
        assert!(note_y(MIDI_HI, 100.0) < note_y(60.0, 100.0));
        assert!((note_y(MIDI_LO, 100.0) - 92.0).abs() < 0.01);
        let ring = Overlay::default().ring(0.0);
        assert!(ring.starts_with('M') && ring.ends_with('Z') && ring.matches('Q').count() == 40);
    }
}
