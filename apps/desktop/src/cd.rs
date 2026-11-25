//! Audio CDs: song names and ripping.
//!
//! Names come from MusicBrainz, looked up by the disc ID worked out from the
//! table of contents (no audio is read or sent), and are cached on disk.
//! Offline, or for a CD MusicBrainz doesn't know, the CD-Text some discs
//! carry is used instead (read in `disc.rs`).
//!
//! Ripping writes one FLAC file per song through a hidden mpv in encoding
//! mode (as `clip.rs` does), reading with cdparanoia's full error
//! correction, then tags it and adds the cover with lofty.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::disc::CdTrack;

const AGENT: &str = concat!("SuperMediaPlayer/", env!("CARGO_PKG_VERSION"));

/// One song's name.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TrackName {
    pub title: String,
    pub artist: String,
}

/// What is known about a CD.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Album {
    pub title: String,
    pub artist: String,
    /// "2019" or "2019-05-03"; may be empty.
    pub date: String,
    /// In disc order, one per audio track.
    pub tracks: Vec<TrackName>,
    /// MusicBrainz release, for the cover.
    pub release_id: Option<String>,
    /// "MusicBrainz" or "CD-Text".
    pub source: String,
}

impl Album {
    /// The name of song `i` (0-based), or "Track N".
    pub fn track_title(&self, i: usize, number: u8) -> String {
        self.tracks
            .get(i)
            .map(|t| t.title.trim())
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Track {number}"))
    }

    /// The artist of song `i`, else the album's.
    pub fn track_artist(&self, i: usize) -> String {
        self.tracks
            .get(i)
            .map(|t| t.artist.trim())
            .filter(|a| !a.is_empty())
            .unwrap_or(self.artist.trim())
            .to_owned()
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "title": self.title,
            "artist": self.artist,
            "date": self.date,
            "release_id": self.release_id,
            "source": self.source,
            "tracks": self.tracks.iter().map(|t| serde_json::json!([t.title, t.artist])).collect::<Vec<_>>(),
        })
    }

    fn from_json(v: &serde_json::Value) -> Option<Album> {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
        Some(Album {
            title: s("title"),
            artist: s("artist"),
            date: s("date"),
            release_id: v["release_id"].as_str().map(str::to_owned),
            source: s("source"),
            tracks: v["tracks"]
                .as_array()?
                .iter()
                .map(|t| TrackName {
                    title: t[0].as_str().unwrap_or_default().to_owned(),
                    artist: t[1].as_str().unwrap_or_default().to_owned(),
                })
                .collect(),
        })
    }
}

// ---- disc ID ----

/// The MusicBrainz disc ID of a CD from its TOC: (track number, start LBA,
/// is data track) entries, the lead-out last (as `disc::toc::read` gives).
/// A data track after the songs (Enhanced CD) is left out, as MusicBrainz
/// does: the songs then end 11400 frames before it starts.
pub fn disc_id(toc: &[(u8, u32, bool)]) -> Option<String> {
    let (leadout, entries) = toc.split_last()?;
    let audio: Vec<_> = entries.iter().filter(|e| !e.2).collect();
    let (first, last) = (audio.first()?, audio.last()?);
    // The end of the songs: the lead-out, or the gap before a data track
    // that follows the last song.
    let end = match entries.iter().find(|e| e.2 && e.0 > last.0) {
        Some(data) => data.1.checked_sub(11_400)?,
        None => leadout.1,
    };
    let mut offsets = [0u32; 100];
    offsets[0] = end + 150;
    for e in &audio {
        offsets[usize::from(e.0)] = e.1 + 150;
    }
    let mut text = format!("{:02X}{:02X}", first.0, last.0);
    for o in offsets {
        text += &format!("{o:08X}");
    }
    use sha1::Digest;
    let hash = sha1::Sha1::digest(text.as_bytes());
    Some(base64_mb(&hash))
}

/// Base64 with MusicBrainz's URL-safe alphabet ('.', '_', and '-' padding).
fn base64_mb(data: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('-');
            }
        }
    }
    out
}

// ---- names ----

fn cache_file(id: &str) -> Option<PathBuf> {
    crate::settings::cache_dir().map(|d| d.join("cd").join(format!("{id}.json")))
}

/// The names of the songs on the CD with this TOC: from the cache, then
/// MusicBrainz (when `online`), then the disc's CD-Text. Blocks for up to a
/// few seconds; run it off the UI thread.
pub fn names(toc: &[(u8, u32, bool)], cd_text: impl FnOnce() -> Option<Album>, online: bool) -> Option<Album> {
    let songs = toc.iter().filter(|e| !e.2).count().saturating_sub(1);
    let id = disc_id(toc);
    let cached = id.as_deref().and_then(cache_file);
    if let Some(album) = cached
        .as_ref()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .and_then(|v| Album::from_json(&v))
    {
        return Some(album);
    }
    if online && let Some(id) = &id {
        match musicbrainz(id) {
            Ok(Some(album)) => {
                if let Some(f) = &cached {
                    let _ = std::fs::create_dir_all(f.parent().unwrap_or(Path::new(".")));
                    let _ = std::fs::write(f, album.to_json().to_string());
                }
                return Some(album);
            }
            Ok(None) => log::info!("MusicBrainz doesn't know the CD {id}"),
            Err(e) => log::info!("MusicBrainz lookup of {id} failed: {e}"),
        }
    }
    // An Enhanced CD's TOC counts the data track too; CD-Text names songs only.
    cd_text().map(|mut a| {
        a.tracks.truncate(songs.max(a.tracks.len().min(99)));
        a
    })
}

fn agent(timeout: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout)))
        .build()
        .into()
}

/// Look the disc ID up. Ok(None): MusicBrainz has no such CD.
fn musicbrainz(id: &str) -> Result<Option<Album>, String> {
    let url = format!(
        "https://musicbrainz.org/ws/2/discid/{id}?inc=recordings+artist-credits&cdstubs=no&fmt=json"
    );
    let resp = agent(6).get(&url).header("User-Agent", AGENT).header("Accept", "application/json").call();
    let mut resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let body = resp.body_mut().with_config().limit(4 << 20).read_to_vec().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    Ok(parse_musicbrainz(&json, id))
}

/// "Artist A & Artist B" from an artist-credit list.
fn credit(v: &serde_json::Value) -> String {
    v.as_array()
        .map(|parts| {
            parts
                .iter()
                .map(|p| {
                    let name = p["name"].as_str().or(p["artist"]["name"].as_str()).unwrap_or_default();
                    format!("{name}{}", p["joinphrase"].as_str().unwrap_or_default())
                })
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// The release and medium holding the disc `id`. With several releases
/// (reissues, other countries), the first one MusicBrainz lists.
fn parse_musicbrainz(json: &serde_json::Value, id: &str) -> Option<Album> {
    for release in json["releases"].as_array()? {
        let Some(media) = release["media"].as_array() else { continue };
        let Some(medium) = media.iter().find(|m| {
            m["discs"].as_array().is_some_and(|d| d.iter().any(|d| d["id"].as_str() == Some(id)))
        }) else {
            continue;
        };
        let artist = credit(&release["artist-credit"]);
        let tracks = medium["tracks"]
            .as_array()
            .map(|ts| {
                ts.iter()
                    .map(|t| {
                        let title = t["title"].as_str().or(t["recording"]["title"].as_str()).unwrap_or_default();
                        let mut a = credit(&t["artist-credit"]);
                        if a.is_empty() {
                            a = credit(&t["recording"]["artist-credit"]);
                        }
                        TrackName { title: title.to_owned(), artist: a }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut title = release["title"].as_str().unwrap_or_default().to_owned();
        // "Album (disc 2)" for sets of several CDs.
        if media.len() > 1
            && let Some(pos) = medium["position"].as_u64()
        {
            match medium["title"].as_str().filter(|t| !t.is_empty()) {
                Some(t) => title += &format!(" (disc {pos}: {t})"),
                None => title += &format!(" (disc {pos})"),
            }
        }
        return Some(Album {
            title,
            artist,
            date: release["date"].as_str().unwrap_or_default().to_owned(),
            tracks,
            release_id: release["id"].as_str().map(str::to_owned),
            source: "MusicBrainz".into(),
        });
    }
    None
}

/// The album's front cover from the Cover Art Archive (cached), if it has one.
pub fn cover(album: &Album, online: bool) -> Option<PathBuf> {
    let id = album.release_id.as_deref()?;
    if !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return None;
    }
    let file = crate::settings::cache_dir()?.join("cd").join(format!("{id}.jpg"));
    if file.is_file() {
        return Some(file);
    }
    if !online {
        return None;
    }
    let url = format!("https://coverartarchive.org/release/{id}/front-500");
    let mut resp = agent(10).get(&url).header("User-Agent", AGENT).call().ok()?;
    let data = resp.body_mut().with_config().limit(8 << 20).read_to_vec().ok()?;
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    std::fs::create_dir_all(file.parent()?).ok()?;
    std::fs::write(&file, data).ok()?;
    Some(file)
}

// ---- CD-Text ----

/// Album, artist and song names from raw CD-Text packs (the data of READ
/// TOC format 5 after its 4-byte header): 18-byte packs of pack type,
/// track number, sequence, block / character position, 12 bytes of text
/// and a CRC. Only the first block (usually English, ISO 8859-1) is read.
pub fn parse_cd_text(packs: &[u8]) -> Option<Album> {
    // Text per pack type (0x80 title, 0x81 performer), split into strings
    // by NULs: string i belongs to track first_track + i (0 = the album).
    let mut titles: Vec<String> = Vec::new();
    let mut performers: Vec<String> = Vec::new();
    for kind in [0x80u8, 0x81] {
        let mut text: Vec<u8> = Vec::new();
        let mut start_track = None;
        for p in packs.as_chunks::<18>().0 {
            // Block 0, single-byte characters.
            if p[0] != kind || p[3] & 0x70 != 0 || p[3] & 0x80 != 0 {
                continue;
            }
            start_track.get_or_insert(p[1] & 0x7F);
            text.extend_from_slice(&p[4..16]);
        }
        let Some(first) = start_track else { continue };
        let mut strings: Vec<String> = vec![String::new(); usize::from(first)];
        let mut prev = String::new();
        for s in text.split(|&b| b == 0) {
            let s: String = s.iter().map(|&b| b as char).collect();
            // A lone TAB repeats the previous string.
            let s = if s == "\t" { prev.clone() } else { s };
            prev = s.clone();
            strings.push(s);
        }
        // The text ends with a NUL, which leaves an empty string at the end.
        while strings.last().is_some_and(|s| s.is_empty()) {
            strings.pop();
        }
        if kind == 0x80 {
            titles = strings;
        } else {
            performers = strings;
        }
    }
    if titles.is_empty() && performers.is_empty() {
        return None;
    }
    let get = |v: &[String], i: usize| v.get(i).map(|s| s.trim().to_owned()).unwrap_or_default();
    let songs = titles.len().max(performers.len()).saturating_sub(1);
    let album = Album {
        title: get(&titles, 0),
        artist: get(&performers, 0),
        date: String::new(),
        tracks: (1..=songs).map(|i| TrackName { title: get(&titles, i), artist: get(&performers, i) }).collect(),
        release_id: None,
        source: "CD-Text".into(),
    };
    (album.tracks.iter().any(|t| !t.title.is_empty()) || !album.title.is_empty()).then_some(album)
}

// ---- playback ----

/// Escape a value for an EDL parameter (`%length%value`, length in bytes).
fn edl_escape(s: &str) -> String {
    format!("%{}%{s}", s.len())
}

/// The EDL of one song on the CD: the cdda stream from `start` for
/// `length` seconds, with the song's tags, so the music view, the media
/// controls see its title, artist and album.
pub fn track_edl(track: &CdTrack, index: usize, album: Option<&Album>) -> String {
    let start = f64::from(track.start) / 75.0;
    let length = f64::from(track.frames) / 75.0;
    let mut edl = String::from("edl://");
    if let Some(a) = album {
        let clean = |s: String| s.replace(['\n', '\r'], " ");
        let mut tags = vec![
            ("title", clean(a.track_title(index, track.number))),
            ("artist", clean(a.track_artist(index))),
            ("album", clean(a.title.clone())),
            ("track", track.number.to_string()),
        ];
        if !a.date.is_empty() {
            tags.push(("date", a.date.clone()));
        }
        edl += "!global_tags";
        for (k, v) in tags.into_iter().filter(|(_, v)| !v.is_empty()) {
            edl += &format!(",{k}={}", edl_escape(&v));
        }
        edl += ";";
    }
    edl + &format!("%7%cdda://,start={start:.3},length={length:.3}")
}

// ---- ripping ----

/// A file or folder name: no slashes or control characters, no leading dot.
fn file_name(s: &str) -> String {
    let clean: String = s
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() || ":*?\"<>|".contains(c) { '_' } else { c })
        .take(120)
        .collect();
    let clean = clean.trim().trim_start_matches('.').trim().to_owned();
    if clean.is_empty() { "Unknown".into() } else { clean }
}

/// The user's Music folder (XDG), else ~/Music.
pub fn music_dir() -> PathBuf {
    crate::clip::user_dir("XDG_MUSIC_DIR", "Music")
}

/// Where a rip of `album` goes: Music/Artist/Album.
pub fn rip_folder(root: &Path, album: Option<&Album>, disc_id: Option<&str>) -> PathBuf {
    match album {
        Some(a) if !a.title.trim().is_empty() => {
            let artist = if a.artist.trim().is_empty() { "Unknown artist" } else { a.artist.as_str() };
            root.join(file_name(artist)).join(file_name(&a.title))
        }
        _ => root.join(format!("Audio CD {}", disc_id.map(|i| &i[..8.min(i.len())]).unwrap_or("(no name)"))),
    }
}

/// Rip each of `tracks` from `device` to FLAC in `dir`, tagged from `album`
/// with the `cover` added. `progress(track index, fraction of that track)`.
/// Leaves the songs already done when cancelled or when one fails (the one
/// in progress is removed). Blocks; run it on a worker thread.
pub fn rip(
    device: &str,
    tracks: &[CdTrack],
    album: Option<&Album>,
    cover: Option<&Path>,
    dir: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(usize, f32),
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    let picture = cover.and_then(|c| std::fs::read(c).ok());
    let total = tracks.len();
    let mut done = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        let title = album.map(|a| a.track_title(i, t.number)).unwrap_or_else(|| format!("Track {}", t.number));
        let out = dir.join(format!("{:02} - {}.flac", t.number, file_name(&title)));
        let length = f64::from(t.frames) / 75.0;
        let job = crate::clip::Job {
            source: track_edl(t, i, None),
            start: 0.0,
            end: length,
            format: crate::clip::Format::Flac,
            aid: None,
            vid: None,
            out: out.clone(),
            extra: vec![
                ("cdda-device".into(), device.to_owned()),
                // Full cdparanoia checking: slower, but scratches don't
                // end up as clicks in the file.
                ("cdda-paranoia".into(), "2".into()),
            ],
        };
        crate::clip::run(&job, cancel, |f| progress(i, f))?;
        if let Err(e) = tag(&out, album, i, t.number, total, picture.as_deref()) {
            log::warn!("couldn't tag {}: {e}", out.display());
        }
        done.push(out);
    }
    if let Some(c) = cover {
        let _ = std::fs::copy(c, dir.join("cover.jpg"));
    }
    Ok(done)
}

/// Write the song's tags (and the cover) into the FLAC file.
fn tag(path: &Path, album: Option<&Album>, i: usize, number: u8, total: usize, cover: Option<&[u8]>) -> Result<(), String> {
    use lofty::config::WriteOptions;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::prelude::*;
    use lofty::tag::Tag;
    let mut file = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    let kind = file.primary_tag_type();
    if file.primary_tag_mut().is_none() {
        file.insert_tag(Tag::new(kind));
    }
    let tag = file.primary_tag_mut().ok_or("no tag")?;
    tag.set_track(u32::from(number));
    tag.set_track_total(total as u32);
    if let Some(a) = album {
        tag.set_title(a.track_title(i, number));
        let artist = a.track_artist(i);
        if !artist.is_empty() {
            tag.set_artist(artist);
        }
        if !a.title.is_empty() {
            tag.set_album(a.title.clone());
        }
        if !a.artist.is_empty() {
            tag.insert_text(ItemKey::AlbumArtist, a.artist.clone());
        }
        if !a.date.is_empty() {
            tag.insert_text(ItemKey::RecordingDate, a.date.clone());
        }
        if let Some(id) = &a.release_id {
            tag.insert_text(ItemKey::MusicBrainzReleaseId, id.clone());
        }
    } else {
        tag.set_title(format!("Track {number}"));
    }
    if let Some(data) = cover {
        tag.push_picture(Picture::new_unchecked(PictureType::CoverFront, Some(MimeType::Jpeg), None, data.to_vec()));
    }
    file.save_to_path(path, WriteOptions::default()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example disc from python-discid's documentation and tests.
    #[test]
    fn disc_id_matches_musicbrainz() {
        let starts = [150u32, 15363, 32314, 46592, 63414, 80489];
        let mut toc: Vec<(u8, u32, bool)> =
            starts.iter().enumerate().map(|(i, &s)| (i as u8 + 1, s - 150, false)).collect();
        toc.push((0xAA, 95462 - 150, false));
        assert_eq!(disc_id(&toc).as_deref(), Some("49HHV7Eb8UKF3aQiNmu1GR8vKTY-"));
    }

    /// An Enhanced CD: the data track and its gap are not part of the ID.
    #[test]
    fn enhanced_cd_leaves_the_data_track_out() {
        let plain = vec![(1, 0, false), (2, 20_000, false), (0xAA, 40_000, false)];
        let enhanced = vec![(1, 0, false), (2, 20_000, false), (3, 51_400, true), (0xAA, 90_000, false)];
        assert_eq!(disc_id(&plain), disc_id(&enhanced));
    }

    #[test]
    fn base64_alphabet() {
        assert_eq!(base64_mb(b"\xfb\xef\xbe"), "....");
        assert_eq!(base64_mb(b"\xff\xff\xff"), "____");
        assert_eq!(base64_mb(b"ab"), "YWI-");
        assert_eq!(base64_mb(b"a"), "YQ--");
    }

    #[test]
    fn musicbrainz_answer() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"releases":[{"id":"r1","title":"Album","date":"2001-02-03",
              "artist-credit":[{"name":"A","joinphrase":" & "},{"name":"B"}],
              "media":[{"position":1,"discs":[{"id":"other"}],"tracks":[]},
                       {"position":2,"title":"","discs":[{"id":"X"}],"tracks":[
                 {"title":"One","artist-credit":[{"name":"A"}]},
                 {"recording":{"title":"Two","artist-credit":[{"name":"C"}]}}]}]}]}"#,
        )
        .unwrap();
        let a = parse_musicbrainz(&json, "X").unwrap();
        assert_eq!(a.title, "Album (disc 2)");
        assert_eq!(a.artist, "A & B");
        assert_eq!(a.date, "2001-02-03");
        assert_eq!(a.tracks, vec![
            TrackName { title: "One".into(), artist: "A".into() },
            TrackName { title: "Two".into(), artist: "C".into() },
        ]);
        assert_eq!(a.release_id.as_deref(), Some("r1"));
        assert!(parse_musicbrainz(&json, "nope").is_none());
        // Round trip through the cache format.
        assert_eq!(Album::from_json(&a.to_json()).as_ref(), Some(&a));
    }

    /// Build CD-Text packs of one type from a list of NUL-ended strings.
    fn packs(kind: u8, strings: &[&str]) -> Vec<u8> {
        let text: Vec<u8> = strings.iter().flat_map(|s| s.bytes().chain([0])).collect();
        let mut out = Vec::new();
        for (seq, chunk) in text.chunks(12).enumerate() {
            let mut p = vec![kind, 0, seq as u8, 0];
            let mut t = chunk.to_vec();
            t.resize(12, 0);
            p.extend(t);
            p.extend([0, 0]);
            out.extend(p);
        }
        out
    }

    #[test]
    fn cd_text() {
        let mut data = packs(0x80, &["The Album", "First song", "\t", "Third"]);
        data.extend(packs(0x81, &["The Band", "The Band", "Guest", "The Band"]));
        let a = parse_cd_text(&data).unwrap();
        assert_eq!(a.title, "The Album");
        assert_eq!(a.artist, "The Band");
        assert_eq!(a.tracks.len(), 3);
        assert_eq!(a.tracks[1], TrackName { title: "First song".into(), artist: "Guest".into() });
        assert_eq!(a.tracks[2].title, "Third");
        assert!(parse_cd_text(&[]).is_none());
    }

    #[test]
    fn rip_folder_names() {
        let a = Album { title: "A/B: C".into(), artist: ".Hidden".into(), ..Default::default() };
        assert_eq!(rip_folder(Path::new("/m"), Some(&a), None), Path::new("/m/Hidden/A_B_ C"));
        assert_eq!(rip_folder(Path::new("/m"), None, Some("49HHV7Eb8UKF")), Path::new("/m/Audio CD 49HHV7Eb"));
    }

    /// mpv takes the tags from the EDL header, escapes and all.
    #[test]
    fn edl_tags_reach_mpv() {
        let Ok(player) = player_core::Player::new(&player_core::PlayerConfig::headless(), |_| {}) else {
            eprintln!("no libmpv, skipped");
            return;
        };
        let album = Album {
            title: "Greatest; Hits, Vol. 1".into(),
            artist: "Ünïcode = Band".into(),
            tracks: vec![TrackName { title: "Song, with %percent%".into(), artist: String::new() }],
            ..Default::default()
        };
        let t = CdTrack { number: 1, start: 0, frames: 75 * 3 };
        let edl = track_edl(&t, 0, Some(&album))
            .replace("%7%cdda://", &edl_escape("av://lavfi:sine=d=3:f=440"));
        let _ = player.set_string("ao", "null");
        player.load(&edl, player_core::LoadMode::Replace).unwrap();
        let mut tags = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if let Some(t) = player.get_string("metadata/by-key/title") {
                tags = Some((t, player.get_string("metadata/by-key/artist"), player.get_string("metadata/by-key/album")));
                break;
            }
        }
        let (title, artist, alb) = tags.expect("no metadata from the EDL");
        assert_eq!(title, "Song, with %percent%");
        assert_eq!(artist.as_deref(), Some("Ünïcode = Band"));
        assert_eq!(alb.as_deref(), Some("Greatest; Hits, Vol. 1"));
    }

    /// A ripped song gets its tags and cover (encoded from a test tone).
    #[test]
    fn ripped_file_is_tagged() {
        let dir = std::env::temp_dir().join(format!("smp-cd-tag-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("01 - Song.flac");
        let job = crate::clip::Job {
            source: "av://lavfi:sine=d=2:f=440".into(),
            start: 0.0,
            end: 1.0,
            format: crate::clip::Format::Flac,
            aid: None,
            vid: None,
            out: out.clone(),
            extra: Vec::new(),
        };
        if crate::clip::run(&job, &AtomicBool::new(false), |_| {}).is_err() {
            eprintln!("no libmpv encoder, skipped");
            return;
        }
        let album = Album {
            title: "Album".into(),
            artist: "Band".into(),
            date: "1999".into(),
            tracks: vec![TrackName { title: "Song".into(), artist: "Guest".into() }],
            release_id: Some("abc".into()),
            source: "MusicBrainz".into(),
        };
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0xFF, 0xD9];
        tag(&out, Some(&album), 0, 1, 12, Some(&jpeg)).unwrap();
        use lofty::prelude::*;
        let file = lofty::read_from_path(&out).unwrap();
        let t = file.primary_tag().unwrap();
        assert_eq!(t.title().as_deref(), Some("Song"));
        assert_eq!(t.artist().as_deref(), Some("Guest"));
        assert_eq!(t.album().as_deref(), Some("Album"));
        assert_eq!(t.track(), Some(1));
        assert_eq!(t.track_total(), Some(12));
        assert_eq!(t.get_string(&ItemKey::AlbumArtist), Some("Band"));
        assert_eq!(t.pictures().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Names the CD in a drive as the app does and rips some of it:
    /// `SMP_TEST_CD=/dev/sr0 SMP_TEST_CD_SONGS=2 cargo test cd_this_machine -- --ignored --nocapture`
    /// (files in SMP_TEST_OUT, else the temporary folder).
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs an audio CD: SMP_TEST_CD"]
    fn cd_this_machine() {
        let Ok(device) = std::env::var("SMP_TEST_CD") else { return };
        let songs: usize = std::env::var("SMP_TEST_CD_SONGS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let root = std::env::var_os("SMP_TEST_OUT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let toc = crate::disc::toc::read(&device).expect("no table of contents");
        let tracks = crate::disc::audio_tracks(&toc);
        let id = disc_id(&toc);
        println!("{} songs, disc ID {id:?}", tracks.len());
        let text = crate::disc::toc::cd_text(&device);
        println!("CD-Text: {:?}", text.as_deref().and_then(parse_cd_text));
        println!("MusicBrainz: {:?}", musicbrainz(id.as_deref().unwrap()));
        let album = names(&toc, || text.as_deref().and_then(parse_cd_text), true);
        for (i, t) in tracks.iter().enumerate() {
            println!("{:02} {:?} {}", t.number, album.as_ref().map(|a| a.track_title(i, t.number)), t.frames / 75);
        }
        let art = album.as_ref().and_then(|a| cover(a, true));
        println!("cover: {art:?}");
        let dir = rip_folder(&root, album.as_ref(), id.as_deref());
        let start = std::time::Instant::now();
        let files = rip(&device, &tracks[..songs.min(tracks.len())], album.as_ref(), art.as_deref(), &dir, &AtomicBool::new(false), |_, _| {}).unwrap();
        println!("{files:?} in {:?}", start.elapsed());
    }
}
