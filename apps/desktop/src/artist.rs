//! Artist pictures for the music view: a picture next to the music files
//! (artist.jpg, folder.jpg, cover.jpg, ...), or, when the user has turned it
//! on, one looked up online by artist name (Deezer's public API, no account).
//! Online results are cached on disk.

use std::path::{Path, PathBuf};

const LOCAL_NAMES: &[&str] = &[
    "artist", "folder", "cover", "front", "albumart", "album", "thumb",
];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// A picture in the song's folder, or in its parent (the artist folder of an
/// Artist/Album/song layout). `artist.*` wins over album covers.
pub fn find_local(media_path: &str) -> Option<PathBuf> {
    if media_path.contains("://") {
        return None;
    }
    let dir = Path::new(media_path).parent()?;
    let files: Vec<(PathBuf, String, String)> = [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter_map(|e| {
            let p = e.path();
            let stem = p.file_stem()?.to_string_lossy().to_lowercase();
            let ext = p.extension()?.to_string_lossy().to_lowercase();
            (IMAGE_EXTS.contains(&ext.as_str()) && p.is_file()).then_some((p, stem, ext))
        })
        .collect();
    LOCAL_NAMES.iter().find_map(|want| {
        files
            .iter()
            .find(|(_, stem, _)| stem == want || stem.starts_with(&format!("{want} ")))
            .map(|(p, _, _)| p.clone())
    })
}

const AGENT: &str = concat!("SuperMediaPlayer/", env!("CARGO_PKG_VERSION"));

thread_local! {
    /// Set when a request in the current lookup failed for a reason that may
    /// pass (offline, timeout, server error), as opposed to "not found".
    static TRANSIENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// GET `url` (at most `limit` bytes). A failure that may pass is noted, so
/// it isn't cached as "nothing found" (see [`lookup_failed`]).
pub(crate) fn http_get(url: &str, limit: u64) -> Option<Vec<u8>> {
    let resp = ureq::get(url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .header("User-Agent", AGENT)
        .call();
    let mut resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) if code != 429 && code < 500 => return None,
        Err(e) => {
            log::debug!("{url}: {e}");
            TRANSIENT.with(|t| t.set(true));
            return None;
        }
    };
    match resp.body_mut().with_config().limit(limit).read_to_vec() {
        Ok(b) => Some(b),
        Err(_) => {
            TRANSIENT.with(|t| t.set(true));
            None
        }
    }
}

/// Start a lookup: forget earlier transient failures on this thread.
pub(crate) fn begin_lookup() {
    TRANSIENT.with(|t| t.set(false));
}

/// Whether a request since [`begin_lookup`] failed in a way that may pass.
pub(crate) fn lookup_failed() -> bool {
    TRANSIENT.with(|t| t.get())
}

/// How long "nothing found" is believed before looking again (new pictures
/// and lyrics get added).
const MISS_TTL: std::time::Duration = std::time::Duration::from_secs(14 * 86_400);

/// A cached "nothing found" (an empty file) that is still fresh.
pub(crate) fn fresh_miss(meta: &std::fs::Metadata) -> bool {
    meta.len() == 0
        && meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < MISS_TTL)
}

fn cache_path(artist: &str) -> Option<PathBuf> {
    let safe: String = artist
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .take(80)
        .collect();
    (!safe.trim_matches('_').is_empty())
        .then(|| {
            crate::settings::cache_dir().map(|d| d.join("artists").join(format!("{safe}.jpg")))
        })
        .flatten()
}

/// Look the artist up online. Blocking; run it off the UI thread. Returns a
/// cached file when there is one (an empty file marks "nothing found").
pub fn fetch_online(artist: &str) -> Option<PathBuf> {
    let artist = primary_artist(artist);
    let cache = cache_path(&artist)?;
    if let Ok(meta) = std::fs::metadata(&cache) {
        if meta.len() > 0 {
            return Some(cache);
        }
        if fresh_miss(&meta) {
            return None;
        }
    }
    begin_lookup();
    let result = lookup_deezer(&artist);
    if result.is_none() && lookup_failed() {
        return None; // offline or the like: try again next time
    }
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&cache, result.as_deref().unwrap_or_default());
    result.map(|_| cache)
}

/// "Artist feat. Other" and "A; B" → the first artist. Not "&" or ",":
/// those are part of many band names.
fn primary_artist(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut end = s.len();
    for sep in [" feat.", " feat ", " ft.", " featuring ", ";", " / "] {
        if let Some(i) = lower.find(sep) {
            end = end.min(i);
        }
    }
    s[..end].trim().to_owned()
}

fn lookup_deezer(artist: &str) -> Option<Vec<u8>> {
    let url = format!(
        "https://api.deezer.com/search/artist?limit=1&q={}",
        url_encode(artist)
    );
    let json = String::from_utf8(http_get(&url, 1 << 20)?).ok()?;
    let pic = json_string_field(&json, "picture_big")?;
    // Only fetch pictures from Deezer's own image servers.
    let host = pic.strip_prefix("https://")?.split('/').next()?;
    if !(host == "dzcdn.net" || host.ends_with(".dzcdn.net") || host.ends_with(".deezer.com")) {
        return None;
    }
    // Deezer's placeholder for artists without a picture has an empty hash.
    if pic.contains("/artist//") {
        return None;
    }
    let img = http_get(&pic, 8 << 20)?;
    let is_image = img.starts_with(&[0xFF, 0xD8, 0xFF]) || img.starts_with(b"\x89PNG");
    is_image.then_some(img)
}

pub(crate) fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The first `"field":"value"` string in a JSON text, unescaped. Enough for
/// the one URL we need, without a JSON library.
pub(crate) fn json_string_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let rest = &json[json.find(&key)? + key.len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'u' => {
                    let mut hex4 = || u32::from_str_radix(&chars.by_ref().take(4).collect::<String>(), 16).ok();
                    let hi = hex4()?;
                    // Characters outside the BMP (emoji) come as a
                    // surrogate pair: \ud83c\udfb5.
                    let code = if (0xD800..0xDC00).contains(&hi) {
                        let lo = (chars.next() == Some('\\') && chars.next() == Some('u'))
                            .then(|| u32::from_str_radix(&chars.by_ref().take(4).collect::<String>(), 16).ok())
                            .flatten()
                            .filter(|lo| (0xDC00..0xE000).contains(lo));
                        lo.map_or(0xFFFD, |lo| 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
                    } else {
                        hi
                    };
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' | 'f' => {}
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_picture_url() {
        let json = r#"{"data":[{"id":27,"name":"Daft Punk","picture_big":"https:\/\/e-cdns-images.dzcdn.net\/images\/artist\/abc\/500x500.jpg"}]}"#;
        assert_eq!(
            json_string_field(json, "picture_big").as_deref(),
            Some("https://e-cdns-images.dzcdn.net/images/artist/abc/500x500.jpg")
        );
        assert_eq!(json_string_field(json, "missing"), None);
    }

    #[test]
    fn first_artist_and_encoding() {
        assert_eq!(
            primary_artist("Daft Punk feat. Pharrell Williams"),
            "Daft Punk"
        );
        assert_eq!(primary_artist("Simon & Garfunkel"), "Simon & Garfunkel");
        assert_eq!(primary_artist("Queen; David Bowie"), "Queen");
        assert_eq!(url_encode("AC/DC é"), "AC%2FDC%20%C3%A9");
        assert_eq!(
            json_string_field(r#"{"a": "x\ud83c\udfb5y\u00e9\/"}"#, "a").as_deref(),
            Some("x\u{1F3B5}yé/")
        );
    }

    #[test]
    fn local_artist_picture_preferred() {
        let root = std::env::temp_dir().join(format!("smp-artist-{}", std::process::id()));
        let album = root.join("Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01.flac"), b"").unwrap();
        std::fs::write(album.join("Cover.JPG"), b"x").unwrap();
        assert_eq!(
            find_local(&album.join("01.flac").to_string_lossy()),
            Some(album.join("Cover.JPG"))
        );
        std::fs::write(root.join("artist.png"), b"x").unwrap();
        assert_eq!(
            find_local(&album.join("01.flac").to_string_lossy()),
            Some(root.join("artist.png"))
        );
        assert_eq!(find_local("https://example.com/a.mp3"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
