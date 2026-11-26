//! The audio signal path: every stage between the file and the sound card,
//! and whether the card gets the file's samples untouched (bit-perfect).
//!
//! The stages come from mpv (decoder, filters, volume, output) and, on
//! Linux, from the system: PipeWire's clock settings and the rate the sound
//! card really runs at (`/proc/asound/.../hw_params`), which is where a
//! sound server's resampling shows up.

use std::path::PathBuf;

use player_core::Player;

/// A stage: title, detail, level (0 untouched, 1 changed, 2 processed).
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub title: String,
    pub detail: String,
    pub level: i32,
}

fn stage(title: &str, detail: impl Into<String>, level: i32) -> Stage {
    Stage {
        title: title.into(),
        detail: detail.into(),
        level,
    }
}

/// What the path adds up to: the text and its level.
pub struct Report {
    pub stages: Vec<Stage>,
    pub verdict: String,
    pub level: i32,
    /// PipeWire converts the file's rate and could follow it instead.
    pub can_fix_rates: bool,
}

const LOSSLESS: &[&str] = &[
    "flac", "alac", "wavpack", "ape", "tta", "truehd", "mlp", "dsd", "pcm", "wav", "aiff",
    "shorten",
];

/// "s16" → "16-bit integer", "floatp" → "32-bit float"…
fn describe_format(f: &str) -> String {
    let base = f.trim_end_matches('p');
    match base {
        "u8" => "8-bit".into(),
        "s16" => "16-bit integer".into(),
        "s24" => "24-bit integer".into(),
        "s32" => "32-bit integer".into(),
        "float" => "32-bit float".into(),
        "double" => "64-bit float".into(),
        "s64" => "64-bit integer".into(),
        other => other.to_owned(),
    }
}

/// The parts of mpv's `audio-params` / `audio-out-params` used here.
#[derive(Clone, Debug, Default, PartialEq)]
struct Params {
    format: String,
    rate: i64,
    channels: String,
}

fn params(p: &Player, name: &str) -> Option<Params> {
    let v = p.get_value(name)?;
    Some(Params {
        format: v.str_field("format")?,
        rate: v.get("samplerate")?.as_i64()?,
        channels: v
            .str_field("hr-channels")
            .or_else(|| v.str_field("channels"))
            .unwrap_or_default(),
    })
}

/// The rate a running ALSA playback device really works at, and its name.
#[derive(Clone, Debug, PartialEq)]
pub struct Hardware {
    pub card: String,
    pub format: String,
    pub rate: i64,
}

/// Parse an ALSA `hw_params` file (None when the device is closed).
fn parse_hw_params(text: &str) -> Option<(String, i64)> {
    let mut format = None;
    let mut rate = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("format:") {
            format = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("rate:") {
            rate = v.split_whitespace().next().and_then(|r| r.parse().ok());
        }
    }
    Some((format?, rate?))
}

/// Playback devices open right now, from `/proc/asound`.
pub fn running_hardware() -> Vec<Hardware> {
    let mut out = Vec::new();
    let Ok(cards) = std::fs::read_dir("/proc/asound") else {
        return out;
    };
    for card in cards.flatten() {
        let name = card.file_name().to_string_lossy().into_owned();
        if !name.starts_with("card") {
            continue;
        }
        let id = std::fs::read_to_string(card.path().join("id"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| name.clone());
        let Ok(pcms) = std::fs::read_dir(card.path()) else {
            continue;
        };
        for pcm in pcms.flatten() {
            let pn = pcm.file_name().to_string_lossy().into_owned();
            if !(pn.starts_with("pcm") && pn.ends_with('p')) {
                continue;
            }
            let Ok(subs) = std::fs::read_dir(pcm.path()) else {
                continue;
            };
            for sub in subs.flatten() {
                let hw = sub.path().join("hw_params");
                if let Some((format, rate)) = std::fs::read_to_string(&hw)
                    .ok()
                    .as_deref()
                    .and_then(parse_hw_params)
                {
                    out.push(Hardware {
                        card: id.clone(),
                        format,
                        rate,
                    });
                }
            }
        }
    }
    out
}

/// PipeWire's clock: the rate it runs at and the rates it may switch to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PwClock {
    pub rate: i64,
    pub allowed: Vec<i64>,
}

fn parse_pw_metadata(text: &str) -> Option<PwClock> {
    let value = |key: &str| {
        text.lines()
            .find(|l| l.contains(&format!("key:'{key}'")))
            .and_then(|l| l.split("value:'").nth(1))
            .and_then(|v| v.split('\'').next())
            .map(str::to_owned)
    };
    let rate = value("clock.rate")?.parse().ok()?;
    let allowed = value("clock.allowed-rates")
        .map(|v| {
            v.trim_matches(|c| c == '[' || c == ']')
                .split([' ', ','])
                .filter_map(|x| x.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default();
    Some(PwClock { rate, allowed })
}

fn pipewire_clock() -> Option<PwClock> {
    let out = std::process::Command::new("pw-metadata")
        .args(["-n", "settings"])
        .output()
        .ok()?;
    parse_pw_metadata(&String::from_utf8_lossy(&out.stdout))
}

/// Common rates a sound card may follow.
const RATES: &[i64] = &[44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// Let PipeWire follow each file's rate: now (`pw-metadata`) and from the
/// next login (a drop-in config file). Returns what was done.
pub fn allow_rates() -> Result<String, String> {
    let list = RATES
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let live = std::process::Command::new("pw-metadata")
        .args([
            "-n",
            "settings",
            "0",
            "clock.allowed-rates",
            &format!("[ {list} ]"),
        ])
        .output()
        .map_err(|e| format!("pw-metadata: {e}"))?;
    if !live.status.success() {
        return Err(String::from_utf8_lossy(&live.stderr).trim().to_owned());
    }
    let dir = config_home().join("pipewire/pipewire.conf.d");
    let file = dir.join("50-super-media-player-rates.conf");
    let text = format!(
        "# Written by Super Media Player (Audio & Subtitles > Signal path):\n\
         # let the sound card run at the rate of what is playing instead of\n\
         # converting everything to one rate. Delete this file to undo.\n\
         context.properties = {{\n    default.clock.allowed-rates = [ {list} ]\n}}\n"
    );
    std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&file, text))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(format!(
        "The sound card now follows each file's sample rate (kept in {}). It applies from the next song.",
        file.display()
    ))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
}

/// Everything mpv and the system say about the path now. Blocking (runs
/// `pw-metadata`): call from a worker thread or rarely.
pub struct Facts {
    codec: String,
    input: Option<Params>,
    output: Option<Params>,
    filters: Vec<String>,
    volume: f64,
    mute: bool,
    speed: f64,
    replaygain: String,
    ao: String,
    device: String,
    exclusive: bool,
    bit_perfect_setting: bool,
    hardware: Vec<Hardware>,
    pipewire: Option<PwClock>,
}

pub fn gather(p: &Player, bit_perfect_setting: bool) -> Facts {
    let filters = p
        .get_value("af")
        .map(|v| {
            v.as_array()
                .iter()
                .filter(|f| f.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true))
                .filter_map(|f| f.str_field("label").or_else(|| f.str_field("name")))
                .collect()
        })
        .unwrap_or_default();
    let ao = p.get_string("current-ao").unwrap_or_default();
    Facts {
        codec: p
            .get_string("audio-codec-name")
            .or_else(|| p.get_string("audio-codec"))
            .unwrap_or_default(),
        input: params(p, "audio-params"),
        output: params(p, "audio-out-params"),
        filters,
        volume: p.get_double("volume").unwrap_or(100.0),
        mute: p.get_string("mute").as_deref() == Some("yes"),
        speed: p.get_double("speed").unwrap_or(1.0),
        replaygain: p.get_string("replaygain").unwrap_or_else(|| "no".into()),
        pipewire: if ao == "pipewire" || ao == "pulse" {
            pipewire_clock()
        } else {
            None
        },
        ao,
        device: p.get_string("audio-device").unwrap_or_default(),
        exclusive: p.get_string("audio-exclusive").as_deref() == Some("yes"),
        bit_perfect_setting,
        hardware: running_hardware(),
    }
}

/// Turn the facts into stages and a verdict.
pub fn report(f: &Facts) -> Report {
    let mut stages = Vec::new();
    let Some(input) = &f.input else {
        return Report {
            stages,
            verdict: "Nothing is playing".into(),
            level: 2,
            can_fix_rates: false,
        };
    };
    let lossless = LOSSLESS
        .iter()
        .any(|c| f.codec.to_lowercase().starts_with(c));
    stages.push(stage(
        "Source",
        format!(
            "{} · {} · {} Hz · {}{}",
            f.codec.to_uppercase(),
            describe_format(&input.format),
            input.rate,
            input.channels,
            if lossless {
                " · lossless"
            } else {
                " · lossy (decoded to the format shown)"
            }
        ),
        0,
    ));

    let ours: Vec<&str> = f.filters.iter().map(String::as_str).collect();
    if ours.is_empty() {
        stages.push(stage(
            "Filters",
            "None: the decoded samples pass straight on",
            0,
        ));
    } else {
        let names: Vec<&str> = ours
            .iter()
            .map(|l| match *l {
                "tone" => "tone, equalizer and effects (works in floating point)",
                "quiet" => "silence left out",
                "vis" => "level meter for the visuals (listens only)",
                other => other,
            })
            .collect();
        stages.push(stage(
            "Filters",
            format!(
                "{}. Turn on Settings → Bit-perfect output to take them out.",
                names.join("; ")
            ),
            2,
        ));
    }

    let mut gain = Vec::new();
    if (f.volume - 100.0).abs() > 0.01 {
        gain.push(format!(
            "player volume at {:.0}% (scales every sample)",
            f.volume
        ));
    }
    if f.mute {
        gain.push("muted".into());
    }
    if f.replaygain != "no" {
        gain.push(format!("ReplayGain ({}) adjusts the level", f.replaygain));
    }
    if (f.speed - 1.0).abs() > 0.001 {
        gain.push(format!("speed {:.2}× (time-stretched)", f.speed));
    }
    if gain.is_empty() {
        stages.push(stage(
            "Volume",
            "100%: untouched (use the sound card's or amplifier's volume)",
            0,
        ));
    } else {
        stages.push(stage("Volume", gain.join("; "), 2));
    }

    let mut can_fix_rates = false;
    match &f.output {
        Some(out) => {
            let mut changes = Vec::new();
            if out.rate != input.rate {
                changes.push(format!("resampled {} → {} Hz", input.rate, out.rate));
            }
            if describe_format(&out.format) != describe_format(&input.format) {
                changes.push(format!(
                    "{} → {}",
                    describe_format(&input.format),
                    describe_format(&out.format)
                ));
            }
            let detail = format!(
                "{} · {} · {} Hz · {}{}",
                f.ao,
                describe_format(&out.format),
                out.rate,
                out.channels,
                if f.device.is_empty() || f.device == "auto" {
                    String::new()
                } else {
                    format!(" · {}", f.device)
                }
            );
            let level = if changes.is_empty() { 0 } else { 1 };
            let detail = if changes.is_empty() {
                detail
            } else {
                format!("{detail} ({})", changes.join(", "))
            };
            stages.push(stage("Player output", detail, level));
        }
        None => stages.push(stage("Player output", "not open yet", 1)),
    }

    if let Some(pw) = &f.pipewire {
        let follows = pw.allowed.contains(&input.rate);
        let detail = format!(
            "PipeWire clock {} Hz; may switch to: {}",
            pw.rate,
            if pw.allowed.is_empty() {
                "—".to_owned()
            } else {
                pw.allowed
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        if pw.rate == input.rate {
            stages.push(stage("Sound server", detail, 0));
        } else {
            can_fix_rates = !follows;
            stages.push(stage(
                "Sound server",
                format!("{detail}. It converts {} Hz to {} Hz.", input.rate, pw.rate),
                1,
            ));
        }
    } else if f.exclusive {
        stages.push(stage(
            "Sound server",
            "Bypassed: the player has the sound card to itself",
            0,
        ));
    }

    for hw in &f.hardware {
        let level = if hw.rate == input.rate { 0 } else { 1 };
        stages.push(stage(
            "Sound card",
            format!("{} · {} · {} Hz", hw.card, hw.format, hw.rate),
            level,
        ));
    }

    let level = stages.iter().map(|s| s.level).max().unwrap_or(0);
    let verdict = match level {
        0 => "Bit-perfect: the sound card gets the file's samples untouched".to_owned(),
        1 => "Clean, but converted on the way (see the yellow stages)".to_owned(),
        _ if !f.bit_perfect_setting => "Processed: effects or volume change the samples".to_owned(),
        _ => "Processed: the volume or speed changes the samples".to_owned(),
    };
    Report {
        stages,
        verdict,
        level,
        can_fix_rates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_alsa_and_pipewire() {
        let hw = "access: MMAP_INTERLEAVED\nformat: S16_LE\nsubformat: STD\nchannels: 2\nrate: 48000 (48000/1)\n";
        assert_eq!(parse_hw_params(hw), Some(("S16_LE".into(), 48_000)));
        assert_eq!(parse_hw_params("closed\n"), None);
        let pw = "Found \"settings\" metadata 31\nupdate: id:0 key:'clock.rate' value:'48000' type:''\n\
                  update: id:0 key:'clock.allowed-rates' value:'[ 44100 48000 ]' type:''\n";
        assert_eq!(
            parse_pw_metadata(pw),
            Some(PwClock {
                rate: 48_000,
                allowed: vec![44_100, 48_000]
            })
        );
    }

    fn facts() -> Facts {
        let p = |f: &str, r| Params {
            format: f.into(),
            rate: r,
            channels: "stereo".into(),
        };
        Facts {
            codec: "flac".into(),
            input: Some(p("s16", 44_100)),
            output: Some(p("s16", 44_100)),
            filters: vec![],
            volume: 100.0,
            mute: false,
            speed: 1.0,
            replaygain: "no".into(),
            ao: "pipewire".into(),
            device: "auto".into(),
            exclusive: false,
            bit_perfect_setting: true,
            hardware: vec![Hardware {
                card: "DAC".into(),
                format: "S16_LE".into(),
                rate: 44_100,
            }],
            pipewire: Some(PwClock {
                rate: 44_100,
                allowed: vec![44_100, 48_000],
            }),
        }
    }

    #[test]
    fn verdicts() {
        assert_eq!(report(&facts()).level, 0);
        // The server converting 44.1 kHz to 48 kHz: clean but converted, and fixable.
        let mut f = facts();
        f.pipewire = Some(PwClock {
            rate: 48_000,
            allowed: vec![48_000],
        });
        f.hardware[0].rate = 48_000;
        let r = report(&f);
        assert_eq!(r.level, 1);
        assert!(r.can_fix_rates);
        // Volume below 100% changes every sample.
        let mut f = facts();
        f.volume = 80.0;
        assert_eq!(report(&f).level, 2);
        let mut f = facts();
        f.filters = vec!["tone".into()];
        assert_eq!(report(&f).level, 2);
    }
}
