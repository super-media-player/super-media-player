//! Subtitles from OpenSubtitles.com: search by the file's hash (exact
//! matches for that very release) and by its name, download the chosen one
//! next to the video and load it.
//!
//! OpenSubtitles serves programs that send an API key ("consumer"), made
//! for free at opensubtitles.com/consumers. Signing in is optional and
//! raises the daily download limit. Key and login live in
//! `opensubtitles.conf` (mode 600).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::settings;

const API: &str = "https://api.opensubtitles.com/api/v1";
// OpenSubtitles asks for "AppName vVersion".
const AGENT: &str = concat!("SuperMediaPlayer v", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub key: String,
    pub user: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub file_id: i64,
    pub language: String,
    pub release: String,
    pub downloads: i64,
    /// Made for this very file (its hash matched).
    pub exact: bool,
    pub hearing_impaired: bool,
    pub title: String,
}

fn file() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("opensubtitles.conf"))
}

pub fn load() -> Account {
    let text = file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    let mut a = Account::default();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().to_owned();
            match k.trim() {
                "api_key" => a.key = v,
                "user" => a.user = v,
                "password" => a.password = v,
                _ => {}
            }
        }
    }
    a
}

pub fn save(a: &Account) -> std::io::Result<()> {
    let f = file().ok_or_else(|| std::io::Error::other("no config folder"))?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    let clean = |s: &str| s.replace(['\n', '\r'], "");
    std::fs::write(
        &f,
        format!(
            "api_key = {}\nuser = {}\npassword = {}\n",
            clean(&a.key),
            clean(&a.user),
            clean(&a.password)
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The OpenSubtitles hash: the file size plus the 64-bit little-endian
/// words of its first and last 64 KiB, wrapping; 16 hex digits.
pub fn hash(path: &Path) -> std::io::Result<String> {
    const CHUNK: u64 = 65536;
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    if size < CHUNK * 2 {
        return Err(std::io::Error::other("file too small to hash"));
    }
    let mut h = size;
    let mut buf = vec![0u8; CHUNK as usize];
    for start in [0, size - CHUNK] {
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf)?;
        for w in buf.as_chunks::<8>().0 {
            h = h.wrapping_add(u64::from_le_bytes(*w));
        }
    }
    Ok(format!("{h:016x}"))
}

/// A cleaner search text from a file name: "The.Show.S01E02.1080p.WEB" →
/// "The Show S01E02".
pub fn query_from_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map_or(name.to_owned(), |s| s.to_string_lossy().into_owned());
    let mut words = Vec::new();
    for w in stem.split(['.', '_', ' ', '-']).filter(|w| !w.is_empty()) {
        let lw = w.to_lowercase();
        let junk = [
            "1080p", "720p", "2160p", "480p", "x264", "x265", "h264", "h265", "hevc", "web",
            "webrip", "web-dl", "bluray", "brrip", "hdtv", "dvdrip", "aac", "dts", "hdr", "remux",
            "proper", "repack",
        ];
        if junk.contains(&lw.as_str()) || lw.starts_with('[') {
            break;
        }
        words.push(w.to_owned());
        // Stop after a year or an episode code.
        if (w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) && words.len() > 1)
            || (lw.starts_with('s')
                && lw.contains('e')
                && lw[1..].chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            break;
        }
    }
    words.join(" ")
}

fn agent(timeout: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn read_json(r: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, String> {
    let mut resp = r.map_err(|e| format!("OpenSubtitles couldn't be reached ({e})"))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if status >= 400 {
        let msg = v["message"]
            .as_str()
            .or(v["errors"][0].as_str())
            .unwrap_or("")
            .to_owned();
        return Err(match status {
            401 | 403 => format!(
                "OpenSubtitles didn't accept the API key or login{}. Check them in Settings → Subtitles.",
                if msg.is_empty() {
                    String::new()
                } else {
                    format!(" (\"{msg}\")")
                }
            ),
            406 | 429 if msg.is_empty() => {
                "OpenSubtitles' download limit for today is reached".into()
            }
            _ if msg.is_empty() => format!("OpenSubtitles answered {status}"),
            _ => msg,
        });
    }
    Ok(v)
}

/// Search by hash and / or text, in `languages` ("en,ro").
pub fn search(
    a: &Account,
    hash: Option<&str>,
    query: &str,
    languages: &str,
) -> Result<Vec<Hit>, String> {
    if a.key.is_empty() {
        return Err("Add your OpenSubtitles API key in Settings → Subtitles first".into());
    }
    let mut params: Vec<(&str, String)> = Vec::new();
    // Parameters in alphabetical order, as the API prefers.
    let langs = languages.to_lowercase().replace(' ', "");
    if !langs.is_empty() {
        params.push(("languages", langs));
    }
    if let Some(h) = hash {
        params.push(("moviehash", h.to_owned()));
    }
    if !query.trim().is_empty() {
        params.push(("query", query.trim().to_lowercase()));
    }
    let r = agent(20)
        .get(&format!("{API}/subtitles"))
        .header("Api-Key", &a.key)
        .header("User-Agent", AGENT)
        .query_pairs(params.iter().map(|(k, v)| (*k, v.as_str())))
        .call();
    let v = read_json(r)?;
    let mut hits: Vec<Hit> = v["data"]
        .as_array()
        .map(|d| d.iter().filter_map(hit).collect())
        .unwrap_or_default();
    // Exact matches first, then the most downloaded.
    hits.sort_by(|x, y| y.exact.cmp(&x.exact).then(y.downloads.cmp(&x.downloads)));
    Ok(hits)
}

fn hit(d: &Value) -> Option<Hit> {
    let a = &d["attributes"];
    let file_id = a["files"][0]["file_id"].as_i64()?;
    let fd = &a["feature_details"];
    let title = match (fd["title"].as_str(), fd["year"].as_i64()) {
        (Some(t), Some(y)) if y > 0 => format!("{t} ({y})"),
        (Some(t), _) => t.to_owned(),
        _ => String::new(),
    };
    Some(Hit {
        file_id,
        language: a["language"].as_str().unwrap_or("?").to_owned(),
        release: a["release"]
            .as_str()
            .or(a["files"][0]["file_name"].as_str())
            .unwrap_or("")
            .to_owned(),
        downloads: a["download_count"].as_i64().unwrap_or(0),
        exact: a["moviehash_match"].as_bool().unwrap_or(false),
        hearing_impaired: a["hearing_impaired"].as_bool().unwrap_or(false),
        title,
    })
}

fn login(a: &Account) -> Result<Option<String>, String> {
    if a.user.is_empty() || a.password.is_empty() {
        return Ok(None);
    }
    let body = serde_json::json!({ "username": a.user, "password": a.password }).to_string();
    let r = agent(20)
        .post(&format!("{API}/login"))
        .header("Api-Key", &a.key)
        .header("User-Agent", AGENT)
        .header("Content-Type", "application/json")
        .send(body.as_bytes());
    let v = read_json(r)?;
    Ok(v["token"].as_str().map(String::from))
}

/// Download `file_id` as `dest` (an .srt). Returns how many downloads are
/// left today, when OpenSubtitles says.
pub fn download(a: &Account, file_id: i64, dest: &Path) -> Result<Option<i64>, String> {
    let token = login(a)?;
    let body = serde_json::json!({ "file_id": file_id, "sub_format": "srt" }).to_string();
    let mut req = agent(20)
        .post(&format!("{API}/download"))
        .header("Api-Key", &a.key)
        .header("User-Agent", AGENT)
        .header("Content-Type", "application/json");
    if let Some(t) = &token {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    let v = read_json(req.send(body.as_bytes()))?;
    let link = v["link"]
        .as_str()
        .ok_or("OpenSubtitles sent no download link")?;
    let mut resp = agent(60)
        .get(link)
        .header("User-Agent", AGENT)
        .call()
        .map_err(|e| format!("Couldn't download the subtitles ({e})"))?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(20 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    std::fs::write(dest, bytes).map_err(|e| format!("Couldn't save {}: {e}", dest.display()))?;
    Ok(v["remaining"].as_i64())
}

/// Where to save: next to the video ("Film.en.srt"), or in the cache when
/// that folder can't be written to (a disc, a read-only share).
pub fn destination(video: &Path, lang: &str) -> PathBuf {
    let stem = video
        .file_stem()
        .map_or("subtitles".into(), |s| s.to_string_lossy().into_owned());
    let name = format!("{stem}.{lang}.srt");
    let beside = video.with_file_name(&name);
    let probe = video.with_file_name(".smp-write-test");
    let writable = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    if writable {
        return beside;
    }
    let dir = settings::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("subtitles");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(name)
}

// ---- UI ----

use std::cell::RefCell;
use std::rc::Rc;

use player_core::Player;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, SubHit};

thread_local! {
    static HITS: RefCell<Vec<Hit>> = const { RefCell::new(Vec::new()) };
}

fn language_name(code: &str) -> String {
    let names = [
        ("en", "English"),
        ("ro", "Romanian"),
        ("ru", "Russian"),
        ("es", "Spanish"),
        ("fr", "French"),
        ("de", "German"),
        ("it", "Italian"),
        ("pt-pt", "Portuguese"),
        ("pt-br", "Portuguese (Brazil)"),
        ("nl", "Dutch"),
        ("pl", "Polish"),
        ("uk", "Ukrainian"),
        ("tr", "Turkish"),
        ("el", "Greek"),
        ("hu", "Hungarian"),
        ("cs", "Czech"),
        ("sv", "Swedish"),
        ("fi", "Finnish"),
        ("da", "Danish"),
        ("no", "Norwegian"),
        ("ar", "Arabic"),
        ("he", "Hebrew"),
        ("zh-cn", "Chinese"),
        ("ja", "Japanese"),
        ("ko", "Korean"),
        ("bg", "Bulgarian"),
        ("sr", "Serbian"),
        ("hr", "Croatian"),
    ];
    names
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(code))
        .map_or(code.to_owned(), |(_, n)| (*n).to_owned())
}

fn status_text(a: &Account) -> String {
    match (a.key.is_empty(), a.user.is_empty()) {
        (true, _) => "Make a free API consumer at opensubtitles.com/consumers (sign up first), paste its key here and press Save.".into(),
        (false, true) => "API key saved. Optional: your OpenSubtitles login raises the daily download limit.".into(),
        (false, false) => format!("API key and login ({}) saved. They stay on this computer.", a.user),
    }
}

pub fn install(
    ui: &AppWindow,
    player: &Rc<Player>,
    settings: Rc<RefCell<crate::settings::Settings>>,
) {
    ui.set_opensubs_status(status_text(&load()).into());
    let weak = ui.as_weak();
    ui.on_save_opensubs(move |key, user, password| {
        let Some(ui) = weak.upgrade() else { return };
        let mut a = load();
        if !key.trim().is_empty() {
            a.key = key.trim().to_owned();
        }
        // Empty fields keep what was saved; a new user name needs its password.
        if !user.trim().is_empty() {
            a.user = user.trim().to_owned();
            a.password = password.to_string();
        }
        ui.set_opensubs_status(
            match save(&a) {
                Ok(()) => status_text(&a),
                Err(e) => format!("Couldn't save: {e}"),
            }
            .into(),
        );
    });

    // Opening the panel: suggest a search from the file name.
    let (p, weak) = (player.clone(), ui.as_weak());
    let s2 = settings.clone();
    ui.on_show_sub_search(move || {
        let Some(ui) = weak.upgrade() else { return };
        let path = p.get_string("path").unwrap_or_default();
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.set_subsearch_query(query_from_name(&name).into());
        ui.set_subsearch_languages(s2.borrow().sub_languages.clone().into());
        ui.set_subsearch_open(true);
        if !path.contains("://") && !path.is_empty() && !load().key.is_empty() {
            ui.invoke_subsearch_search(ui.get_subsearch_query(), ui.get_subsearch_languages());
        } else if load().key.is_empty() {
            ui.set_subsearch_status(
                "Add your OpenSubtitles API key in Settings → Subtitles first.".into(),
            );
        }
    });

    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_subsearch_search(move |query, langs| {
        let Some(ui) = weak.upgrade() else { return };
        {
            let mut s = settings.borrow_mut();
            if s.sub_languages != langs.as_str() && !langs.trim().is_empty() {
                s.sub_languages = langs.trim().to_owned();
                s.save();
            }
        }
        let path = p.get_string("path").unwrap_or_default();
        let local = (!path.contains("://") && Path::new(&path).is_file()).then(|| PathBuf::from(&path));
        ui.set_subsearch_working(true);
        ui.set_subsearch_status("Searching…".into());
        let weak = ui.as_weak();
        let (query, langs) = (query.to_string(), langs.to_string());
        std::thread::spawn(move || {
            let a = load();
            let h = local.as_deref().and_then(|f| hash(f).ok());
            let mut result = search(&a, h.as_deref(), &query, &langs);
            // Nothing for this exact file and name together: try the name alone.
            if matches!(&result, Ok(v) if v.is_empty()) && h.is_some() && !query.is_empty() {
                result = search(&a, None, &query, &langs);
            }
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_subsearch_working(false);
                match result {
                    Ok(hits) => {
                        let exact = hits.iter().filter(|x| x.exact).count();
                        ui.set_subsearch_status(
                            match (hits.len(), exact) {
                                (0, _) => "Nothing found. Try another search, or make subtitles from the speech.".to_string(),
                                (n, 0) => format!("{n} found by name; check the release matches your file."),
                                (n, e) => format!("{n} found; {e} made for this very file (marked ✓)."),
                            }
                            .into(),
                        );
                        let rows: Vec<SubHit> = hits
                            .iter()
                            .map(|x| SubHit {
                                title: if x.release.is_empty() { x.title.clone() } else { x.release.clone() }.into(),
                                detail: {
                                    let mut d = format!("{} · {} downloads", language_name(&x.language), x.downloads);
                                    if x.hearing_impaired {
                                        d += " · for the hard of hearing";
                                    }
                                    if !x.title.is_empty() && !x.release.is_empty() {
                                        d += &format!(" · {}", x.title);
                                    }
                                    d.into()
                                },
                                exact: x.exact,
                            })
                            .collect();
                        ui.set_subsearch_hits(ModelRc::new(VecModel::from(rows)));
                        HITS.with_borrow_mut(|h| *h = hits);
                    }
                    Err(e) => ui.set_subsearch_status(e.into()),
                }
            });
        });
    });

    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_subsearch_download(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(h) = HITS.with_borrow(|v| v.get(i as usize).cloned()) else {
            return;
        };
        let path = p.get_string("path").unwrap_or_default();
        let dest = if path.contains("://") || path.is_empty() {
            let dir = crate::settings::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("subtitles");
            let _ = std::fs::create_dir_all(&dir);
            dir.join(format!("{}.{}.srt", h.file_id, h.language))
        } else {
            destination(Path::new(&path), &h.language)
        };
        ui.set_subsearch_working(true);
        ui.set_subsearch_status("Downloading…".into());
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let r = download(&load(), h.file_id, &dest);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_subsearch_working(false);
                match r {
                    Ok(left) => {
                        ui.invoke_load_subtitle_path(dest.to_string_lossy().to_string().into());
                        ui.set_subsearch_open(false);
                        // Not made for this very file: the times may be off.
                        if !h.exact {
                            ui.invoke_sub_sync_auto(dest.to_string_lossy().to_string().into());
                        }
                        ui.set_toast(
                            match left {
                                Some(n) => format!(
                                    "Subtitles loaded ({}). {n} downloads left today.",
                                    language_name(&h.language)
                                ),
                                None => {
                                    format!("Subtitles loaded ({})", language_name(&h.language))
                                }
                            }
                            .into(),
                        );
                    }
                    Err(e) => ui.set_subsearch_status(e.into()),
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_of_known_pattern() {
        // 128 KiB + 8 bytes of zeros: the hash is just the size.
        let dir = std::env::temp_dir().join(format!("smp-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("z.bin");
        std::fs::write(&f, vec![0u8; 131_080]).unwrap();
        assert_eq!(hash(&f).unwrap(), format!("{:016x}", 131_080u64));
        // A 1 in the first word adds 1.
        let mut v = vec![0u8; 131_080];
        v[0] = 1;
        std::fs::write(&f, v).unwrap();
        assert_eq!(hash(&f).unwrap(), format!("{:016x}", 131_081u64));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn queries_from_names() {
        assert_eq!(
            query_from_name("The.Show.S01E02.1080p.WEB-DL.mkv"),
            "The Show S01E02"
        );
        assert_eq!(
            query_from_name("Some_Film_2019_720p_x264.mp4"),
            "Some Film 2019"
        );
        assert_eq!(query_from_name("Movie Name.mkv"), "Movie Name");
    }

    #[test]
    fn parses_a_hit() {
        let v: Value = serde_json::from_str(
            r#"{"attributes":{"language":"en","release":"Film.2020.WEB","download_count":42,
               "moviehash_match":true,"hearing_impaired":false,
               "feature_details":{"title":"Film","year":2020},
               "files":[{"file_id":123,"file_name":"x.srt"}]}}"#,
        )
        .unwrap();
        let h = hit(&v).unwrap();
        assert_eq!((h.file_id, h.exact, h.downloads), (123, true, 42));
        assert_eq!(h.title, "Film (2020)");
    }
}
