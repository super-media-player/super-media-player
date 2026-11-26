//! Subtitles written by speech recognition, on this computer, and search
//! in what is said.
//!
//! whisper.cpp (`whisper-cli`) does the recognising: the player decodes the
//! file's sound to 16 kHz mono WAV with a hidden mpv, runs whisper-cli on
//! it (on the GPU when whisper.cpp was built with Vulkan, CUDA or Metal),
//! and loads the SRT it writes. The models come from whisper.cpp's page on
//! Hugging Face, checked against SHA-256 sums pinned here.
//!
//! Any SRT or WebVTT subtitles can be searched, generated or not.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

/// Model: settings name, file, SHA-256, size in bytes.
pub const MODELS: &[(&str, &str, &str, u64)] = &[
    (
        "tiny",
        "ggml-tiny.bin",
        "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        77_691_713,
    ),
    (
        "base",
        "ggml-base.bin",
        "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        147_951_465,
    ),
    (
        "small",
        "ggml-small.bin",
        "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        487_601_967,
    ),
    (
        "medium",
        "ggml-medium.bin",
        "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        1_533_763_059,
    ),
    (
        "large-v3-turbo",
        "ggml-large-v3-turbo.bin",
        "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        1_624_555_275,
    ),
];
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

fn exe_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["whisper-cli.exe"]
    } else {
        &["whisper-cli", "whisper-cpp"]
    }
}

/// The whisper-cli to use: `$SMP_WHISPER`, the package's copy, the one
/// `scripts/fetch-whisper.sh` builds into the data folder, then PATH.
pub fn whisper_cli() -> Option<PathBuf> {
    let mut c = Vec::new();
    if let Some(p) = std::env::var_os("SMP_WHISPER").filter(|p| !p.is_empty()) {
        c.push(PathBuf::from(p));
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf));
    for name in exe_names() {
        if let Some(d) = &exe_dir {
            c.push(d.join("whisper").join(name));
            c.push(d.join(name));
        }
        if let Some(d) = crate::settings::data_dir() {
            c.push(d.join("whisper").join("bin").join(name));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in exe_names() {
                c.push(dir.join(name));
            }
        }
    }
    c.into_iter().find(|p| p.is_file())
}

fn model_dir() -> Option<PathBuf> {
    crate::settings::data_dir().map(|d| d.join("whisper"))
}

pub fn model_info(name: &str) -> Option<&'static (&'static str, &'static str, &'static str, u64)> {
    MODELS.iter().find(|m| m.0 == name)
}

/// The model file, if downloaded.
pub fn model_path(name: &str) -> Option<PathBuf> {
    let (_, file, _, size) = model_info(name)?;
    let p = model_dir()?.join(file);
    std::fs::metadata(&p)
        .is_ok_and(|m| m.len() == *size)
        .then_some(p)
}

/// Download model `name`, checking its SHA-256. `progress` gets 0..1.
pub fn download_model(
    name: &str,
    cancel: &AtomicBool,
    progress: impl Fn(f32),
) -> Result<PathBuf, String> {
    let (_, file, want, size) = model_info(name).ok_or("unknown model")?;
    let dir = model_dir().ok_or("no data folder (HOME is not set)")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(file);
    let part = dest.with_extension("part");
    let resp = ureq::get(&format!("{MODEL_URL}/{file}"))
        .config()
        .timeout_connect(Some(std::time::Duration::from_secs(20)))
        .build()
        .header(
            "User-Agent",
            concat!("SuperMediaPlayer/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut reader = resp.into_body().into_with_config().limit(size + 1).reader();
    let mut out = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut hash = Sha256::new();
    let mut got = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("Cancelled".to_owned());
        }
        match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                hash.update(&buf[..n]);
                if let Err(e) = out.write_all(&buf[..n]) {
                    break Err(format!("{}: {e}", part.display()));
                }
                got += n as u64;
                progress((got as f64 / *size as f64) as f32);
            }
            Err(e) => break Err(format!("download failed: {e}")),
        }
    };
    drop(out);
    let result = result.and_then(|()| {
        let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if hex == *want && got == *size {
            Ok(())
        } else {
            Err("the model download is damaged (wrong checksum); try again".to_owned())
        }
    });
    match result {
        Ok(()) => {
            std::fs::rename(&part, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            Ok(dest)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Where the subtitles for `source` (with this model, translated or not)
/// are kept.
pub fn subtitle_path(source: &str, model: &str, translate: bool) -> Option<PathBuf> {
    let dir = crate::settings::cache_dir()?.join("subtitles");
    Some(dir.join(format!(
        "{}-{model}{}.srt",
        key(source),
        if translate { "-en" } else { "" }
    )))
}

/// Subtitles made earlier for `source`, by any model, the newest first.
pub fn existing_subtitles(source: &str) -> Option<PathBuf> {
    let dir = crate::settings::cache_dir()?.join("subtitles");
    let k = key(source);
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{k}-"))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// The file's identity: path, size and date.
fn key(source: &str) -> String {
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    if let Ok(m) = std::fs::metadata(source) {
        h.update(m.len().to_le_bytes());
        if let Ok(t) = m.modified().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_err(std::io::Error::other)
        }) {
            h.update(t.as_secs().to_le_bytes());
        }
    }
    h.finalize()[..10]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What to transcribe.
pub struct Job {
    pub source: String,
    pub aid: Option<i64>,
    pub duration: f64,
    pub model: String,
    pub translate: bool,
}

/// Progress: a stage and a fraction (0..1).
pub enum Step {
    Download(f32),
    Decode(f32),
    Recognise(f32),
}

/// Make the subtitles: download the model if needed, decode the sound,
/// recognise. Blocks for as long as that takes. Returns the SRT.
pub fn run(job: &Job, cancel: &AtomicBool, progress: impl Fn(Step)) -> Result<PathBuf, String> {
    let cli = whisper_cli().ok_or(
        "whisper.cpp is not installed. Build it with scripts/fetch-whisper.sh, or install whisper-cli.",
    )?;
    let model = match model_path(&job.model) {
        Some(m) => m,
        None => download_model(&job.model, cancel, |f| progress(Step::Download(f)))?,
    };
    let out = subtitle_path(&job.source, &job.model, job.translate).ok_or("no cache folder")?;
    let dir = out.parent().ok_or("no cache folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // 16 kHz mono is what whisper listens to.
    let wav = dir.join(format!("{}.wav", std::process::id()));
    let d = crate::pcm::Decode {
        source: &job.source,
        out: &wav,
        rate: 16_000,
        wav: true,
        start: 0.0,
        length: None,
        aid: job.aid,
    };
    let duration = job.duration.max(1.0);
    crate::pcm::render(&d, cancel, |t| {
        progress(Step::Decode((t / duration).clamp(0.0, 1.0) as f32))
    })?;

    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(1, 8));
    let stem = out.with_extension("");
    let mut cmd = Command::new(&cli);
    cmd.arg("-m")
        .arg(&model)
        .arg("-f")
        .arg(&wav)
        .args(["-osrt", "-pp", "-l", "auto", "-t", &threads.to_string()])
        .arg("-of")
        .arg(&stem);
    if job.translate {
        cmd.arg("-tr");
    }
    // Next to a packaged whisper-cli are its own libraries.
    if let Some(dir) = cli.parent() {
        let mut paths = vec![dir.to_path_buf(), dir.join("lib")];
        if let Some(old) = std::env::var_os("LD_LIBRARY_PATH") {
            paths.extend(std::env::split_paths(&old));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("LD_LIBRARY_PATH", joined);
        }
    }
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", cli.display()));
    let result = match &mut child {
        Ok(child) => {
            let stderr = child.stderr.take();
            let mut last_lines = Vec::new();
            if let Some(err) = stderr {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if cancel.load(Ordering::Relaxed) {
                        let _ = child.kill();
                        break;
                    }
                    if let Some(pc) = parse_progress(&line) {
                        progress(Step::Recognise(pc));
                    } else if !line.trim().is_empty() {
                        last_lines.push(line);
                        if last_lines.len() > 6 {
                            last_lines.remove(0);
                        }
                    }
                }
            }
            let status = child.wait();
            if cancel.load(Ordering::Relaxed) {
                Err("Cancelled".to_owned())
            } else {
                match status {
                    Ok(s) if s.success() && out.is_file() => Ok(out.clone()),
                    Ok(_) | Err(_) => Err(format!(
                        "whisper.cpp failed: {}",
                        last_lines.last().map(String::as_str).unwrap_or("no output")
                    )),
                }
            }
        }
        Err(e) => Err(e.clone()),
    };
    let _ = std::fs::remove_file(&wav);
    if result.is_err() {
        let _ = std::fs::remove_file(&out);
    }
    result
}

/// "whisper_print_progress_callback: progress =  42%" → 0.42.
fn parse_progress(line: &str) -> Option<f32> {
    let rest = line.split("progress =").nth(1)?;
    let n: f32 = rest.trim().trim_end_matches('%').trim().parse().ok()?;
    Some((n / 100.0).clamp(0.0, 1.0))
}

// ---- reading subtitles and searching them ----

/// One spoken line: when (seconds) and what.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub time: f64,
    pub text: String,
}

/// "00:01:02,500" or "01:02.500" → seconds.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (
            h.parse::<f64>().ok()?,
            m.parse::<f64>().ok()?,
            s.parse::<f64>().ok()?,
        ),
        [m, s] => (0.0, m.parse::<f64>().ok()?, s.parse::<f64>().ok()?),
        _ => return None,
    };
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Lines of an SRT or WebVTT file. Markup (`<i>`, `{\an8}`) is removed.
pub fn parse_subtitles(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let text = text.replace("\r\n", "\n");
    for block in text.split("\n\n") {
        let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
        let Some(timing) = lines.by_ref().find(|l| l.contains("-->")) else {
            continue;
        };
        let Some(time) = timing.split("-->").next().and_then(parse_time) else {
            continue;
        };
        let words: Vec<String> = lines.map(strip_markup).filter(|l| !l.is_empty()).collect();
        if !words.is_empty() {
            out.push(Line {
                time,
                text: words.join(" "),
            });
        }
    }
    out
}

pub fn strip_markup(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' | '{' => depth += 1,
            '>' | '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_owned()
}

/// Lines containing every word of `query` (any case), at most `max`.
pub fn search<'a>(lines: &'a [Line], query: &str, max: usize) -> Vec<&'a Line> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return lines.iter().take(max).collect();
    }
    lines
        .iter()
        .filter(|l| {
            let t = l.text.to_lowercase();
            words.iter().all(|w| t.contains(w.as_str()))
        })
        .take(max)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_srt_and_vtt() {
        let srt = "1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i> there\r\n\r\n2\r\n00:01:02,250 --> 00:01:04,000\r\n{\\an8}The password is\r\nswordfish\r\n";
        let l = parse_subtitles(srt);
        assert_eq!(
            l,
            vec![
                Line {
                    time: 1.0,
                    text: "Hello there".into()
                },
                Line {
                    time: 62.25,
                    text: "The password is swordfish".into()
                },
            ]
        );
        let vtt = "WEBVTT\n\n00:05.000 --> 00:06.000\nShort time\n";
        assert_eq!(parse_subtitles(vtt)[0].time, 5.0);
        assert_eq!(search(&l, "PASSWORD sword", 10).len(), 1);
        assert_eq!(search(&l, "nothing", 10).len(), 0);
        assert_eq!(
            parse_progress("whisper_print_progress_callback: progress =  42%"),
            Some(0.42)
        );
        assert_eq!(parse_progress("whisper_init: loading"), None);
    }

    #[test]
    fn models_are_pinned() {
        for (name, file, sha, size) in MODELS {
            assert!(crate::settings::WHISPER_MODELS.contains(name), "{name}");
            assert!(file.starts_with("ggml-") && sha.len() == 64 && *size > 1 << 20);
        }
        assert_eq!(MODELS.len(), crate::settings::WHISPER_MODELS.len());
    }
}

/// End to end: `SMP_WHISPER_TEST=<file with speech>` is transcribed with the
/// tiny model (downloaded when missing). Skipped without it.
#[cfg(test)]
mod live {
    #[test]
    fn transcribes_speech() {
        let Some(src) = std::env::var_os("SMP_WHISPER_TEST") else {
            eprintln!("SMP_WHISPER_TEST not set; skipping");
            return;
        };
        let job = super::Job {
            source: src.to_string_lossy().into_owned(),
            aid: None,
            duration: 60.0,
            model: "tiny".into(),
            translate: false,
        };
        let srt = super::run(&job, &Default::default(), |_| {}).expect("transcribe");
        let lines = super::parse_subtitles(&std::fs::read_to_string(&srt).unwrap());
        eprintln!("{} lines; first: {:?}", lines.len(), lines.first());
        assert!(lines.len() > 3);
        let _ = std::fs::remove_file(srt);
    }
}
