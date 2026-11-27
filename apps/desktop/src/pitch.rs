//! The melody's note and the bass note, for the melody trail (overlay.rs).
//!
//! Fed the note meter's energies (meter.rs: one band a semitone wide per
//! note from `NOTE_LOW` up) about 30 times a second. A note is scored by its
//! own band plus the bands of its overtones (an octave, an octave and a
//! fifth, two octaves up), so a voice's strong second harmonic doesn't pass
//! for the note itself. The best note in the melody range and in the bass
//! range counts when it stands well clear of the rest; a short run of
//! picks is voted on, so one stray tick doesn't make the line jump.

use crate::meter::{NOTE_LOW, NOTES};

/// Overtones counted for a note: semitones up and weight.
const OVERTONES: &[(usize, f32)] = &[(0, 1.0), (12, 0.5), (19, 0.33), (24, 0.25)];
/// Ranges searched, MIDI: bass C2..E3, melody C3..E6.
const BASS: (u8, u8) = (36, 52);
const MELODY: (u8, u8) = (48, 88);
/// A pick must score this many times the range's median...
const CLEAR: f32 = 4.0;
/// ...and its band be at least this loud (power; -60 dBFS).
const FLOOR: f32 = 1e-6;

/// A found note: MIDI number (fractional, between bands) and loudness 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub midi: f32,
    pub level: f32,
}

/// One range followed over time.
#[derive(Default)]
struct Track {
    /// The last three raw picks, newest last.
    recent: [Option<Note>; 3],
    shown: Option<Note>,
}

impl Track {
    fn push(&mut self, pick: Option<Note>) -> Option<Note> {
        self.recent.rotate_left(1);
        self.recent[2] = pick;
        // Two of the last three agree (within a semitone): that note.
        let agree = |a: Note, b: Note| (a.midi - b.midi).abs() <= 1.0;
        let voted = pick
            .filter(|p| {
                self.recent[..2]
                    .iter()
                    .flatten()
                    .any(|q| agree(*p, *q))
            })
            .or_else(|| match self.recent {
                [Some(a), Some(b), _] if agree(a, b) => Some(b),
                _ => None,
            });
        self.shown = match (voted, self.shown) {
            // Glide a little within a note (vibrato, slides); jump to a new one.
            (Some(v), Some(s)) if (v.midi - s.midi).abs() < 0.75 => Some(Note {
                midi: s.midi + 0.5 * (v.midi - s.midi),
                level: v.level,
            }),
            (v, _) => v,
        };
        self.shown
    }
}

#[derive(Default)]
pub struct Notes {
    melody: Track,
    bass: Track,
}

impl Notes {
    /// One tick: `energies` from the note meter (None when nothing is
    /// measured). Returns the melody note and the bass note now.
    pub fn tick(&mut self, energies: Option<&[f32]>) -> (Option<Note>, Option<Note>) {
        let (m, b) = match energies.filter(|e| e.len() == NOTES) {
            Some(e) => (pick(e, MELODY), pick(e, BASS)),
            None => (None, None),
        };
        (self.melody.push(m), self.bass.push(b))
    }
}

/// Score of note index `i` (0 = `NOTE_LOW`): its band and its overtones'.
fn score(e: &[f32], i: usize) -> f32 {
    OVERTONES
        .iter()
        .filter_map(|&(up, w)| e.get(i + up).map(|v| v * w))
        .sum()
}

/// The clear best note in MIDI range `range`, if there is one.
fn pick(e: &[f32], range: (u8, u8)) -> Option<Note> {
    let lo = usize::from(range.0 - NOTE_LOW);
    let hi = usize::from(range.1 - NOTE_LOW).min(e.len() - 1);
    let scores: Vec<f32> = (lo..=hi).map(|i| score(e, i)).collect();
    let (best, &top) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))?;
    let mut sorted = scores.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    let i = lo + best;
    if e[i] < FLOOR || top < CLEAR * median.max(1e-12) {
        return None;
    }
    // Between bands: a parabola through the peak and its neighbours (in dB).
    let db = |j: usize| 10.0 * e[j].max(1e-12).log10();
    let shift = if i > 0 && i + 1 < e.len() {
        let (l, c, r) = (db(i - 1), db(i), db(i + 1));
        let d = l - 2.0 * c + r;
        if d < 0.0 {
            (0.5 * (l - r) / d).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    } else {
        0.0
    };
    Some(Note {
        midi: f32::from(NOTE_LOW) + i as f32 + shift,
        // -50 dBFS .. -10 dBFS.
        level: ((db(i) + 50.0) / 40.0).clamp(0.0, 1.0),
    })
}

/// Name of the note nearest to `midi`, like "A4".
pub fn name(midi: f32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
    ];
    let m = midi.round() as i32;
    format!("{}{}", NAMES[m.rem_euclid(12) as usize], m.div_euclid(12) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Energies for a voice singing `midi`, with overtones, over a little noise.
    fn voice(midi: u8, bass: Option<u8>) -> Vec<f32> {
        let mut e = vec![1e-7f32; NOTES];
        let mut put = |m: u8, v: f32| {
            if let Some(x) = e.get_mut(usize::from(m - NOTE_LOW)) {
                *x += v;
            }
        };
        // The second harmonic louder than the note itself, as in many voices.
        for (up, v) in [(0, 2e-3), (12, 3e-3), (19, 1e-3), (24, 5e-4)] {
            put(midi + up, v);
        }
        if let Some(b) = bass {
            put(b, 4e-3);
            put(b + 12, 1e-3);
        }
        e
    }

    #[test]
    fn finds_the_note_not_its_overtone() {
        let mut n = Notes::default();
        let mut last = (None, None);
        for _ in 0..3 {
            last = n.tick(Some(&voice(64, Some(40))));
        }
        let (m, b) = last;
        assert_eq!(m.map(|m| m.midi.round() as u8), Some(64));
        assert_eq!(b.map(|b| b.midi.round() as u8), Some(40));
        assert_eq!(name(64.2), "E4");
        assert_eq!(name(40.0), "E2");
    }

    #[test]
    fn noise_has_no_note_and_one_stray_pick_is_ignored() {
        let mut n = Notes::default();
        let flat = vec![1e-4f32; NOTES];
        assert_eq!(n.tick(Some(&flat)), (None, None));
        n.tick(Some(&flat));
        // One tick of a note between noise: not shown.
        assert_eq!(n.tick(Some(&voice(67, None))).0, None);
        // Held: shown from the second tick.
        assert!(n.tick(Some(&voice(67, None))).0.is_some());
    }
}
