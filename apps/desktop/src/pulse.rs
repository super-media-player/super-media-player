//! Beat tracking and the "pulse line" for the music view.
//!
//! Fed the meter's band levels (meter.rs: 0..1, bass first, lined up with
//! what is heard and not smoothed) about 30 times a second, it finds the
//! beats in two steps:
//!
//! 1. Onsets: how much the bass bands rose since the last tick (spectral
//!    flux), against a threshold that follows the song's recent flux, so it
//!    works in quiet and in loud, compressed music alike.
//! 2. Tempo: the onset signal of the last ~8 s is autocorrelated to find the
//!    beat period (60..180 BPM, leaning towards ~120). Once it is clear, a
//!    beat lands on every period: on the onset when the kick comes close to
//!    when it is expected (which also keeps the phase in step), or on time by
//!    itself when one is soft or missing, so the line keeps the rhythm.
//!
//! It keeps:
//! - `beat`: 1.0 on a beat, decaying to 0, for visuals that throb;
//! - `energy`: overall loudness, 0..1;
//! - ~25 s of a heart-monitor trace: a small wobble that follows the bass,
//!   and an ECG spike whose peak sits exactly on each beat. The UI draws it
//!   scrolling right to left, fading out at the old end.

use std::collections::VecDeque;

/// Samples of trace history: 25 s at the meter's ~30 ticks a second.
pub const HISTORY: usize = 750;
/// Ticks a second (the meter's timer runs every 33 ms).
const RATE: f32 = 30.0;
/// The ECG shape after the peak (R): the S dip, then the T wave.
const AFTER_PEAK: &[f32] = &[-0.55, 0.1, 0.0, 0.1, 0.16, 0.1, 0.04];
/// Dip just before the peak (Q), as a share of the peak.
const BEFORE_PEAK: f32 = -0.15;
/// At least this many ticks between onsets (~0.2 s).
const MIN_GAP: u32 = 6;
/// Onset signal kept for the tempo estimate (~8.5 s).
const ONSET_WINDOW: usize = 256;
/// Beat periods considered, in ticks: 180 BPM .. 60 BPM.
const MIN_PERIOD: usize = 10;
const MAX_PERIOD: usize = 30;
/// Flux values kept for the onset threshold (~1 s).
const FLUX_WINDOW: usize = 30;
/// Without a real onset for this long the tempo is dropped (a quiet part).
const LOST_AFTER: u32 = (4.0 * RATE) as u32;

#[derive(Default)]
pub struct Pulse {
    history: VecDeque<f32>,
    /// Last tick's band levels, for the flux.
    last: Vec<f32>,
    /// Recent flux, for the onset threshold.
    flux: VecDeque<f32>,
    /// Onset strength per tick (flux above its threshold), for the tempo.
    onsets: VecDeque<f32>,
    /// Slow running average of the bass: the trace's baseline.
    avg_bass: f32,
    energy: f32,
    beat: f32,
    /// Ticks since the last beat and since the last real onset.
    since_beat: u32,
    since_onset: u32,
    /// Beat period in ticks, once the tempo is clear.
    period: Option<f32>,
    /// Ticks until the next tempo estimate.
    until_estimate: u32,
    /// Strength of the last real beat, for the ones put in on time.
    last_strength: f32,
    /// Position in `AFTER_PEAK` (None: no spike playing) and its strength.
    spike: Option<(usize, f32)>,
}

impl Pulse {
    /// One meter tick. `levels`: band heights 0..1, bass first (`None` when
    /// nothing is heard, e.g. paused: the trace holds still).
    pub fn tick(&mut self, levels: Option<&[f32]>) {
        self.beat *= 0.86;
        let Some(levels) = levels.filter(|l| !l.is_empty()) else {
            return;
        };
        let n = levels.len();
        let bass_bands = (n / 4).max(1);
        let bass = levels[..bass_bands].iter().sum::<f32>() / bass_bands as f32;
        let energy = levels.iter().sum::<f32>() / n as f32;
        self.energy = 0.7 * self.energy + 0.3 * energy;
        self.avg_bass = if self.last.is_empty() {
            bass
        } else {
            0.95 * self.avg_bass + 0.05 * bass
        };

        // 1. Onset: the rise of the bass (most of it) and of the rest.
        let flux = if self.last.len() == n {
            let rise = |range: std::ops::Range<usize>| {
                let len = range.len().max(1) as f32;
                range
                    .map(|i| (levels[i] - self.last[i]).max(0.0))
                    .sum::<f32>()
                    / len
            };
            rise(0..bass_bands) + 0.25 * rise(bass_bands..n)
        } else {
            0.0
        };
        self.last.clear();
        self.last.extend_from_slice(levels);
        let threshold = self.threshold();
        push(&mut self.flux, flux, FLUX_WINDOW);
        self.since_beat = self.since_beat.saturating_add(1);
        self.since_onset = self.since_onset.saturating_add(1);
        let onset = flux > threshold && self.since_onset >= MIN_GAP;
        push(
            &mut self.onsets,
            if onset { flux - threshold } else { 0.0 },
            ONSET_WINDOW,
        );
        let strength = (0.55 + 0.45 * (flux / threshold.max(1e-3) - 1.0)).clamp(0.45, 1.0);
        if onset {
            self.since_onset = 0;
        }
        if self.since_onset > LOST_AFTER {
            self.period = None;
        }

        // 2. Tempo, re-estimated a few times a second.
        if self.until_estimate == 0 {
            self.period = self
                .estimate_period()
                .or(self.period.filter(|_| self.since_onset < LOST_AFTER));
            self.until_estimate = 8;
        }
        self.until_estimate -= 1;

        let beat = match self.period {
            Some(p) => {
                let since = self.since_beat as f32;
                if onset && since >= 0.6 * p {
                    // The kick, about when expected.
                    Some(strength)
                } else if onset && since <= 0.25 * p {
                    // A kick just after a beat put in on time: that one was
                    // early. Move the grid onto the kick, without a second spike.
                    self.since_beat = 0;
                    None
                } else if since >= p + 1.0 {
                    // None came (a soft or missing kick): keep the rhythm.
                    Some(0.55 * self.last_strength.max(0.6))
                } else {
                    None
                }
            }
            None => onset.then_some(strength),
        };
        if let Some(s) = beat {
            if onset {
                self.last_strength = s;
            }
            self.since_beat = 0;
            self.beat = self.beat.max(s);
        }

        // The trace: a wobble following the bass around its average, and on
        // a beat the peak right here, with the dip before it on the last sample.
        let mut v = (bass - self.avg_bass) * 0.35;
        if let Some((i, s)) = self.spike {
            v += AFTER_PEAK[i] * s * 0.8;
            self.spike = (i + 1 < AFTER_PEAK.len()).then_some((i + 1, s));
        }
        if let Some(s) = beat {
            if let Some(prev) = self.history.back_mut() {
                *prev = (*prev + BEFORE_PEAK * s * 0.8).clamp(-1.0, 1.0);
            }
            v = s * 0.8;
            self.spike = Some((0, s));
        }
        push(&mut self.history, v.clamp(-1.0, 1.0), HISTORY);
    }

    /// Flux an onset must beat: well above the recent typical flux.
    fn threshold(&self) -> f32 {
        if self.flux.is_empty() {
            return 0.05;
        }
        let n = self.flux.len() as f32;
        let mean = self.flux.iter().sum::<f32>() / n;
        let var = self.flux.iter().map(|f| (f - mean).powi(2)).sum::<f32>() / n;
        (mean + 1.4 * var.sqrt()).max(0.03)
    }

    /// The beat period from the autocorrelation of the onset signal, in
    /// (fractional) ticks, or `None` while there is no clear one.
    fn estimate_period(&self) -> Option<f32> {
        if self.onsets.len() < ONSET_WINDOW / 2 {
            return None;
        }
        let o: Vec<f32> = self.onsets.iter().copied().collect();
        let acf = |lag: usize| -> f32 { o.iter().zip(&o[lag..]).map(|(a, b)| a * b).sum() };
        let zero = acf(0);
        if zero <= 0.0 {
            return None;
        }
        // Lean towards ~120 BPM (15 ticks), as people tap along.
        let weight = |lag: usize| {
            let x = (lag as f32 / 15.0).log2();
            (-x * x / (2.0 * 0.6 * 0.6)).exp()
        };
        let scores: Vec<f32> = (MIN_PERIOD - 1..=MAX_PERIOD + 1).map(acf).collect();
        let score = |lag: usize| scores[lag + 1 - MIN_PERIOD];
        let best = (MIN_PERIOD..=MAX_PERIOD)
            .max_by(|&a, &b| (score(a) * weight(a)).total_cmp(&(score(b) * weight(b))))?;
        if score(best) / zero < 0.15 {
            return None;
        }
        // Between ticks: fit a parabola through the peak and its neighbours.
        let (l, c, r) = (score(best - 1), score(best), score(best + 1));
        let d = l - 2.0 * c + r;
        let shift = if d < 0.0 {
            (0.5 * (l - r) / d).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        Some(best as f32 + shift)
    }

    pub fn beat(&self) -> f32 {
        self.beat
    }

    pub fn energy(&self) -> f32 {
        self.energy
    }

    /// The tempo found, beats a minute.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn bpm(&self) -> Option<f32> {
        self.period.map(|p| 60.0 * RATE / p)
    }

    /// Drop the trace and the tempo (a new song starts with a flat line).
    pub fn clear(&mut self) {
        *self = Self {
            energy: self.energy,
            ..Self::default()
        };
    }

    /// The newest trace value, -1..1 (where the UI puts the pen).
    pub fn head(&self) -> f32 {
        self.history.back().copied().unwrap_or(0.0)
    }

    /// SVG path commands for the trace in a `HISTORY` × `view_height`
    /// viewbox (the UI's line has that shape, so nothing is stretched),
    /// newest sample at the right edge. Empty with nothing to draw.
    pub fn path(&self, view_height: f32) -> String {
        use std::fmt::Write;
        if self.history.len() < 2 {
            return String::new();
        }
        let start = HISTORY - self.history.len();
        let mid = view_height / 2.0;
        let mut out = String::with_capacity(self.history.len() * 12);
        for (i, v) in self.history.iter().enumerate() {
            let cmd = if i == 0 { 'M' } else { 'L' };
            let _ = write!(out, "{cmd}{} {:.1} ", start + i, mid - v * mid * 0.92);
        }
        out
    }
}

fn push(q: &mut VecDeque<f32>, v: f32, cap: usize) {
    q.push_back(v);
    while q.len() > cap {
        q.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Levels for one tick of a song with a kick every `period` ticks
    /// (`skip`: a kick left out), a little noise on top.
    fn song(t: usize, period: usize, skip: Option<usize>) -> [f32; 20] {
        let noise = ((t * 7919) % 13) as f32 / 13.0 * 0.04;
        let mut l = [0.3 + noise; 20];
        let since = t % period;
        if Some(t / period) != skip {
            let kick = match since {
                0 => 0.95,
                1 => 0.75,
                2 => 0.55,
                _ => 0.3,
            };
            l[..5].fill(kick + noise);
        }
        l
    }

    #[test]
    fn beats_land_on_the_kicks_and_the_tempo_is_found() {
        let mut p = Pulse::default();
        let mut beats = Vec::new();
        for t in 0..900 {
            p.tick(Some(&song(t, 15, None)));
            if p.since_beat == 0 {
                beats.push(t);
            }
        }
        let bpm = p.bpm().expect("tempo");
        assert!((bpm - 120.0).abs() < 3.0, "bpm {bpm}");
        // After the first seconds every beat is on a kick, and no kick is missed.
        let late: Vec<usize> = beats.iter().copied().filter(|t| *t > 300).collect();
        assert!(late.iter().all(|t| t % 15 == 0), "{late:?}");
        assert_eq!(
            late.len(),
            (300..900).filter(|t| t % 15 == 0 && *t > 300).count()
        );
        // The trace peaks on the beat itself.
        assert!(
            p.history
                .iter()
                .rev()
                .nth(900 - 1 - late[late.len() - 1])
                .unwrap()
                > &0.4
        );
    }

    #[test]
    fn keeps_the_rhythm_through_a_missing_kick() {
        let mut p = Pulse::default();
        let mut beats = Vec::new();
        for t in 0..600 {
            p.tick(Some(&song(t, 18, Some(30))));
            if p.since_beat == 0 && t > 500 {
                beats.push(t);
            }
        }
        // Kick 30 (tick 540) is missing: a beat comes within a tick or two.
        assert!(beats.iter().any(|t| (540..=542).contains(t)), "{beats:?}");
        assert!(beats.contains(&558), "{beats:?}");
    }

    #[test]
    fn steady_sound_has_no_beats() {
        let mut p = Pulse::default();
        for _ in 0..400 {
            p.tick(Some(&[0.5; 20]));
        }
        assert!(p.beat() < 0.01);
        assert!(p.bpm().is_none());
        assert!(p.history.iter().all(|v| v.abs() < 0.1));
    }

    #[test]
    fn history_capped_paused_holds_still() {
        let mut p = Pulse::default();
        for t in 0..HISTORY * 2 {
            p.tick(Some(&song(t, 15, None)));
        }
        assert_eq!(p.history.len(), HISTORY);
        let path = p.path(200.0);
        assert!(path.starts_with("M0 "));
        assert_eq!(path.matches('L').count(), HISTORY - 1);
        p.tick(None);
        assert_eq!(p.path(200.0), path);

        p.clear();
        assert_eq!(p.path(200.0), "");
        p.tick(Some(&[0.5; 20]));
        p.tick(Some(&[0.5; 20]));
        // Short history is drawn at the right edge.
        assert!(p.path(200.0).starts_with(&format!("M{} ", HISTORY - 2)));
    }
}
