//! What the music view knows about the song beyond mpv's metadata: the
//! album cover, lyrics, and (when the user allows lookups online) a short
//! artist summary.
//!
//! Sources, first found wins:
//! - Cover: the picture embedded in the file (lofty reads the tags), a
//!   cover.jpg/folder.jpg in the song's folder, then Deezer (online).
//! - Lyrics: a `.lrc` file next to the song, lyrics in the tags, then
//!   LRCLIB (online, separate switch). Timed (LRC) lyrics scroll with the
//!   song; plain ones show in the info panel.
//! - About the artist: the Wikipedia summary of the artist's page (online).
//!
//! Everything here blocks: run it on a worker thread. Online answers are
//! cached on disk, including "nothing found" (an empty file).

use std::path::{Path, PathBuf};

use crate::artist::{
    begin_lookup, fresh_miss, http_get, json_string_field, lookup_failed, url_encode,
};

const COVER_NAMES: &[&str] = &["cover", "folder", "front", "albumart", "album"];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// The song, as far as the lookup needs it (from mpv's metadata).
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    /// Allowed to ask Deezer and Wikipedia (cover, artist summary).
    pub online_info: bool,
    /// Allowed to ask LRCLIB for lyrics.
    pub online_lyrics: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub cover: Option<PathBuf>,
    pub year: Option<u32>,
    pub genre: String,
    pub track: Option<u32>,
    pub lyrics: Option<Lyrics>,
    /// A few sentences about the artist, and where they come from.
    pub about: String,
    pub about_source: String,
}

/// Lyrics lines; with times (seconds) when the source was timed LRC.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<(Option<f64>, String)>,
}

impl Lyrics {
    pub fn synced(&self) -> bool {
        self.lines.first().is_some_and(|(t, _)| t.is_some())
    }

    /// The whole text, one line per line.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Index of the line being sung at `t` seconds (timed lyrics only).
    pub fn line_at(&self, t: f64) -> Option<usize> {
        if !self.synced() {
            return None;
        }
        let n = self
            .lines
            .partition_point(|(time, _)| time.is_some_and(|x| x <= t));
        n.checked_sub(1)
    }

    /// Parse lyrics text: LRC (`[mm:ss.xx] words`, several stamps per line
    /// allowed, tags like `[ar:...]` skipped) or plain lines.
    pub fn parse(text: &str) -> Option<Lyrics> {
        let mut timed = Vec::new();
        let mut plain = Vec::new();
        for raw in text.lines() {
            let mut rest = raw.trim();
            let mut stamps = Vec::new();
            while let Some(after) = rest.strip_prefix('[') {
                let Some(end) = after.find(']') else { break };
                match parse_stamp(&after[..end]) {
                    Some(t) => stamps.push(t),
                    // An ID tag such as [ar:Artist] or [offset:...].
                    None if after[..end].contains(':') && stamps.is_empty() => {}
                    None => break,
                }
                rest = after[end + 1..].trim_start();
            }
            let words = rest.trim().to_owned();
            if stamps.is_empty() {
                if raw.trim_start().starts_with('[') && words.is_empty() {
                    continue; // a tag line
                }
                plain.push(words);
            } else {
                timed.extend(stamps.into_iter().map(|t| (Some(t), words.clone())));
            }
        }
        if !timed.is_empty() {
            timed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            return Some(Lyrics { lines: timed });
        }
        // Trim blank lines at both ends of plain lyrics.
        while plain.last().is_some_and(|l| l.is_empty()) {
            plain.pop();
        }
        let start = plain.iter().position(|l| !l.is_empty())?;
        Some(Lyrics {
            lines: plain[start..].iter().map(|l| (None, l.clone())).collect(),
        })
    }
}

/// "mm:ss", "mm:ss.xx" or "mm:ss:xx" → seconds.
fn parse_stamp(s: &str) -> Option<f64> {
    let (m, rest) = s.split_once(':')?;
    let m: u32 = m.trim().parse().ok()?;
    let rest = rest.replacen(':', ".", 1);
    let secs: f64 = rest.trim().parse().ok()?;
    (secs < 60.0).then(|| f64::from(m) * 60.0 + secs)
}

/// Gather what there is for the song. Blocking.
pub fn lookup(q: &Query) -> Info {
    let mut info = Info::default();
    let local = !q.path.is_empty() && !q.path.contains("://");
    let mut embedded_lyrics = None;
    if local {
        if let Ok(tagged) = lofty::read_from_path(&q.path) {
            use lofty::prelude::*;
            if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
                info.year = tag.year();
                info.track = tag.track();
                info.genre = tag.genre().map(|g| g.into_owned()).unwrap_or_default();
                embedded_lyrics = tag.get_string(&ItemKey::Lyrics).map(str::to_owned);
                let pics = tag.pictures();
                let pic = pics
                    .iter()
                    .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
                    .or_else(|| pics.first());
                if let Some(p) = pic {
                    info.cover = cache_bytes("covers", p.data());
                }
            }
        }
        if info.cover.is_none() {
            info.cover = find_local_cover(&q.path);
        }
    }
    if info.cover.is_none() && q.online_info && !q.artist.is_empty() && !q.album.is_empty() {
        info.cover = deezer_album_cover(&q.artist, &q.album);
    }

    info.lyrics = local
        .then(|| std::fs::read_to_string(Path::new(&q.path).with_extension("lrc")).ok())
        .flatten()
        .and_then(|t| Lyrics::parse(&t))
        .or_else(|| embedded_lyrics.and_then(|t| Lyrics::parse(&t)))
        .or_else(|| {
            (q.online_lyrics && !q.artist.is_empty() && !q.title.is_empty())
                .then(|| lrclib(q))
                .flatten()
        });

    if q.online_info
        && !q.artist.is_empty()
        && let Some(about) = wikipedia_summary(&q.artist)
    {
        info.about = about;
        info.about_source = "Wikipedia".into();
    }
    info
}

/// An album cover file in the song's own folder (not its parent: that is
/// the artist's folder, and its pictures are the artist's).
pub fn find_local_cover(media_path: &str) -> Option<PathBuf> {
    let dir = Path::new(media_path).parent()?;
    let files: Vec<(PathBuf, String)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let stem = p.file_stem()?.to_string_lossy().to_lowercase();
            let ext = p.extension()?.to_string_lossy().to_lowercase();
            (IMAGE_EXTS.contains(&ext.as_str()) && p.is_file()).then_some((p, stem))
        })
        .collect();
    COVER_NAMES.iter().find_map(|want| {
        files
            .iter()
            .find(|(_, stem)| stem == want || stem.starts_with(&format!("{want} ")))
            .map(|(p, _)| p.clone())
    })
}

/// Store `bytes` in the cache under a name made from their hash (so the
/// same cover is written once), and return the file.
fn cache_bytes(kind: &str, bytes: &[u8]) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    if bytes.is_empty() {
        return None;
    }
    let hash = Sha256::digest(bytes);
    let name: String = hash[..12].iter().map(|b| format!("{b:02x}")).collect();
    let ext = if bytes.starts_with(b"\x89PNG") {
        "png"
    } else {
        "jpg"
    };
    let path = crate::settings::cache_dir()?
        .join(kind)
        .join(format!("{name}.{ext}"));
    if !path.exists() {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path)
}

/// A cache file for a lookup by text key; `Some(None)` = looked up before,
/// nothing found.
fn cached(kind: &str, key: &str, ext: &str) -> (Option<PathBuf>, Option<Option<Vec<u8>>>) {
    let safe: String = key
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .take(120)
        .collect();
    let Some(path) =
        crate::settings::cache_dir().map(|d| d.join(kind).join(format!("{safe}.{ext}")))
    else {
        return (None, None);
    };
    begin_lookup();
    let hit = match std::fs::metadata(&path) {
        Ok(m) if m.len() > 0 => std::fs::read(&path).ok().map(Some),
        Ok(m) if fresh_miss(&m) => Some(None),
        _ => None, // never looked up, or "nothing found" long ago
    };
    (Some(path), hit)
}

/// Cache a lookup's result. Nothing found because the network failed
/// isn't cached, so it's tried again next time.
fn store(path: Option<&Path>, bytes: Option<&[u8]>) {
    if bytes.is_none() && lookup_failed() {
        return;
    }
    if let Some(path) = path {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, bytes.unwrap_or_default());
    }
}

fn get_text(url: &str) -> Option<String> {
    String::from_utf8(http_get(url, 2 << 20)?).ok()
}

fn deezer_album_cover(artist: &str, album: &str) -> Option<PathBuf> {
    let (path, hit) = cached("albums", &format!("{artist} - {album}"), "jpg");
    if let Some(hit) = hit {
        return hit.and(path);
    }
    let q = format!("artist:\"{artist}\" album:\"{album}\"");
    let found = (|| {
        let json = get_text(&format!(
            "https://api.deezer.com/search/album?limit=1&q={}",
            url_encode(&q)
        ))?;
        let pic = json_string_field(&json, "cover_big")?;
        let host = pic.strip_prefix("https://")?.split('/').next()?;
        if !(host == "dzcdn.net" || host.ends_with(".dzcdn.net") || host.ends_with(".deezer.com")) {
            return None;
        }
        let img = http_get(&pic, 8 << 20)?;
        (img.starts_with(&[0xFF, 0xD8, 0xFF]) || img.starts_with(b"\x89PNG")).then_some(img)
    })();
    store(path.as_deref(), found.as_deref());
    found.and(path)
}

fn lrclib(q: &Query) -> Option<Lyrics> {
    let key = format!("{} - {}", q.artist, q.title);
    let (path, hit) = cached("lyrics", &key, "lrc");
    if let Some(hit) = hit {
        return hit.and_then(|b| Lyrics::parse(&String::from_utf8_lossy(&b)));
    }
    let mut url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}",
        url_encode(&q.artist),
        url_encode(&q.title)
    );
    if !q.album.is_empty() {
        url.push_str(&format!("&album_name={}", url_encode(&q.album)));
    }
    if q.duration > 0.0 {
        url.push_str(&format!("&duration={}", q.duration.round()));
    }
    let text = get_text(&url).and_then(|json| {
        json_string_field(&json, "syncedLyrics")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| json_string_field(&json, "plainLyrics"))
    });
    store(path.as_deref(), text.as_deref().map(str::as_bytes));
    text.and_then(|t| Lyrics::parse(&t))
}

/// Words that show a Wikipedia page is about a musician or a band, not
/// something else of the same name.
const MUSIC_WORDS: &[&str] = &[
    "band",
    "singer",
    "musician",
    "rapper",
    "group",
    "duo",
    "dj",
    "composer",
    "songwriter",
    "producer",
    "orchestra",
    "guitarist",
    "pianist",
    "vocalist",
    "music",
    "trio",
    "quartet",
];

fn wikipedia_summary(artist: &str) -> Option<String> {
    let (path, hit) = cached("about", artist, "txt");
    if let Some(hit) = hit {
        return hit.map(|b| String::from_utf8_lossy(&b).into_owned());
    }
    let found = (|| {
        let title = artist.trim().replace(' ', "_");
        let json = get_text(&format!(
            "https://en.wikipedia.org/api/rest_v1/page/summary/{}",
            url_encode(&title)
        ))?;
        if json_string_field(&json, "type").as_deref() != Some("standard") {
            return None; // a disambiguation page, or missing
        }
        let desc = json_string_field(&json, "description")
            .unwrap_or_default()
            .to_lowercase();
        let extract = json_string_field(&json, "extract")?;
        let first = extract.to_lowercase();
        let musical = MUSIC_WORDS.iter().any(|w| {
            desc.split(|c: char| !c.is_alphanumeric()).any(|x| x == *w)
                || first.contains(&format!(" {w}"))
        });
        musical.then_some(extract)
    })();
    store(path.as_deref(), found.as_deref().map(str::as_bytes));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let text = "[ar:Someone]\n[ti:Song]\n\n[00:12.50]First line\n[00:10.00][01:00.00]Chorus\n[00:15:20]Third\n";
        let l = Lyrics::parse(text).unwrap();
        assert!(l.synced());
        let times: Vec<f64> = l.lines.iter().map(|(t, _)| t.unwrap()).collect();
        assert_eq!(times, vec![10.0, 12.5, 15.2, 60.0]);
        assert_eq!(l.lines[0].1, "Chorus");
        assert_eq!(l.line_at(5.0), None);
        assert_eq!(l.line_at(12.6), Some(1));
        assert_eq!(l.line_at(100.0), Some(3));
    }

    #[test]
    fn parses_plain_lyrics() {
        let l = Lyrics::parse("\n\nHello\n\nWorld\n\n").unwrap();
        assert!(!l.synced());
        assert_eq!(l.text(), "Hello\n\nWorld");
        assert_eq!(l.line_at(3.0), None);
        assert_eq!(Lyrics::parse("\n  \n"), None);
    }

    #[test]
    fn finds_cover_in_song_folder_only() {
        let root = std::env::temp_dir().join(format!("smp-cover-{}", std::process::id()));
        let album = root.join("Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(root.join("folder.jpg"), b"x").unwrap();
        let song = album.join("01.flac");
        std::fs::write(&song, b"").unwrap();
        assert_eq!(find_local_cover(&song.to_string_lossy()), None);
        std::fs::write(album.join("Front.PNG"), b"x").unwrap();
        assert_eq!(
            find_local_cover(&song.to_string_lossy()),
            Some(album.join("Front.PNG"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
