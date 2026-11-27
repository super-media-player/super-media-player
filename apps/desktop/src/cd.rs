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
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
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
pub fn names(
    toc: &[(u8, u32, bool)],
    cd_text: impl FnOnce() -> Option<Album>,
    online: bool,
) -> Option<Album> {
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
    let resp = agent(6)
        .get(&url)
        .header("User-Agent", AGENT)
        .header("Accept", "application/json")
        .call();
    let mut resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let body = resp
        .body_mut()
        .with_config()
        .limit(4 << 20)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
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
                    let name = p["name"]
                        .as_str()
                        .or(p["artist"]["name"].as_str())
                        .unwrap_or_default();
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
        let Some(media) = release["media"].as_array() else {
            continue;
        };
        let Some(medium) = media.iter().find(|m| {
            m["discs"]
                .as_array()
                .is_some_and(|d| d.iter().any(|d| d["id"].as_str() == Some(id)))
        }) else {
            continue;
        };
        let artist = credit(&release["artist-credit"]);
        let tracks = medium["tracks"]
            .as_array()
            .map(|ts| {
                ts.iter()
                    .map(|t| {
                        let title = t["title"]
                            .as_str()
                            .or(t["recording"]["title"].as_str())
                            .unwrap_or_default();
                        let mut a = credit(&t["artist-credit"]);
                        if a.is_empty() {
                            a = credit(&t["recording"]["artist-credit"]);
                        }
                        TrackName {
                            title: title.to_owned(),
                            artist: a,
                        }
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
    let file = crate::settings::cache_dir()?
        .join("cd")
        .join(format!("{id}.jpg"));
    if file.is_file() {
        return Some(file);
    }
    if !online {
        return None;
    }
    let url = format!("https://coverartarchive.org/release/{id}/front-500");
    let mut resp = agent(10)
        .get(&url)
        .header("User-Agent", AGENT)
        .call()
        .ok()?;
    let data = resp
        .body_mut()
        .with_config()
        .limit(8 << 20)
        .read_to_vec()
        .ok()?;
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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
        tracks: (1..=songs)
            .map(|i| TrackName {
                title: get(&titles, i),
                artist: get(&performers, i),
            })
            .collect(),
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
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() || ":*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let clean = clean.trim().trim_start_matches('.').trim().to_owned();
    if clean.is_empty() {
        "Unknown".into()
    } else {
        clean
    }
}

/// The user's Music folder (XDG), else ~/Music.
pub fn music_dir() -> PathBuf {
    crate::clip::user_dir("XDG_MUSIC_DIR", "Music")
}

/// Where a rip of `album` goes: Music/Artist/Album.
pub fn rip_folder(root: &Path, album: Option<&Album>, disc_id: Option<&str>) -> PathBuf {
    match album {
        Some(a) if !a.title.trim().is_empty() => {
            let artist = if a.artist.trim().is_empty() {
                "Unknown artist"
            } else {
                a.artist.as_str()
            };
            root.join(file_name(artist)).join(file_name(&a.title))
        }
        _ => root.join(format!(
            "Audio CD {}",
            disc_id.map(|i| &i[..8.min(i.len())]).unwrap_or("(no name)")
        )),
    }
}

/// What a rip found while reading: the songs written and how hard the
/// disc was to read.
#[derive(Debug, Default)]
pub struct RipReport {
    pub files: Vec<PathBuf>,
    /// Read errors and dropped or doubled bytes that cdparanoia repaired by
    /// reading again (a worn or dirty disc; the result is still exact).
    pub corrected: u64,
    /// Sectors cdparanoia gave up on (scratches it couldn't read past): the
    /// song has a short glitch there. Songs with any are named in `damaged`.
    pub skipped: u64,
    pub damaged: Vec<String>,
}

/// Rip each of `tracks` from `device` to FLAC in `dir`, tagged from `album`
/// with the `cover` added. `first_lba`: where the first audio track starts
/// on the disc (the tracks' starts count from there).
/// `progress(track index, fraction of that track)`.
///
/// The sectors are read with libcdio-paranoia (full checking: every
/// sector is read until two reads agree, jitter and scratches corrected)
/// and encoded as they come, so each file holds exactly the song's samples:
/// nothing cut, nothing added, the same bits as on the CD.
/// Leaves the songs already done when cancelled or when one fails (the one
/// in progress is removed). Blocks; run it on a worker thread.
#[allow(clippy::too_many_arguments)]
pub fn rip(
    device: &str,
    tracks: &[CdTrack],
    first_lba: u32,
    album: Option<&Album>,
    cover: Option<&Path>,
    dir: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(usize, f32),
) -> Result<RipReport, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    let picture = cover.and_then(|c| std::fs::read(c).ok());
    let mut reader = paranoia::Reader::open(device)?;
    let total = tracks.len();
    let mut report = RipReport::default();
    for (i, t) in tracks.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        let title = album
            .map(|a| a.track_title(i, t.number))
            .unwrap_or_else(|| format!("Track {}", t.number));
        let out = dir.join(format!("{:02} - {}.flac", t.number, file_name(&title)));
        let start = first_lba + t.start;
        let (corrected, skipped) = paranoia::counts();
        let flac = reader.read_flac(start, t.frames, cancel, |f| progress(i, f))?;
        let (c2, s2) = paranoia::counts();
        report.corrected += c2 - corrected;
        if s2 > skipped {
            report.skipped += s2 - skipped;
            report.damaged.push(title.clone());
        }
        let flac = with_tags(&flac, &tags(album, i, t.number, total), picture.as_deref())?;
        let part = out.with_extension("flac.part");
        std::fs::write(&part, flac)
            .map_err(|e| format!("couldn't write {}: {e}", part.display()))?;
        std::fs::rename(&part, &out)
            .map_err(|e| format!("couldn't write {}: {e}", out.display()))?;
        report.files.push(out);
    }
    if let Some(c) = cover {
        let _ = std::fs::copy(c, dir.join("cover.jpg"));
    }
    Ok(report)
}

/// libcdio-paranoia (cdparanoia's library), loaded when a rip starts: it is
/// bundled with the player, and only ripping needs it.
mod paranoia {
    use std::ffi::{CString, c_char, c_int, c_long, c_void};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    use flacenc::source::{Fill, Source};

    /// Samples per sector and channel (2352 bytes of 16-bit stereo).
    const SECTOR_SAMPLES: usize = 588;
    /// PARANOIA_MODE_FULL ^ PARANOIA_MODE_NEVERSKIP: check everything, but
    /// give up on a sector after the retries instead of hanging forever (what
    /// `cdparanoia` does by default).
    const MODE: c_int = 0xff ^ 0x20;
    const RETRIES: c_int = 20;
    // paranoia_cb_mode_t values (cdio/paranoia/paranoia.h). Edge and atom
    // fix-ups (2, 3) are the drive's ordinary jitter, corrected on clean
    // discs too, so they don't count as trouble.
    const CB_SKIP: c_int = 6;
    const CB_FIXUP_DROPPED: c_int = 10;
    const CB_FIXUP_DUPED: c_int = 11;
    const CB_READERR: c_int = 12;

    static CORRECTED: AtomicU64 = AtomicU64::new(0);
    static SKIPPED: AtomicU64 = AtomicU64::new(0);

    /// (sectors corrected, sectors skipped) so far, over all rips.
    pub fn counts() -> (u64, u64) {
        (
            CORRECTED.load(Ordering::Relaxed),
            SKIPPED.load(Ordering::Relaxed),
        )
    }

    extern "C" fn callback(_pos: c_long, mode: c_int) {
        match mode {
            CB_FIXUP_DROPPED | CB_FIXUP_DUPED | CB_READERR => {
                CORRECTED.fetch_add(1, Ordering::Relaxed);
            }
            CB_SKIP => {
                SKIPPED.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    type Identify = unsafe extern "C" fn(*const c_char, c_int, *mut *mut c_char) -> *mut c_void;
    type Open = unsafe extern "C" fn(*mut c_void) -> c_int;
    type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
    type Init = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
    type Free = unsafe extern "C" fn(*mut c_void);
    type ModeSet = unsafe extern "C" fn(*mut c_void, c_int);
    type Seek = unsafe extern "C" fn(*mut c_void, i32, c_int) -> i32;
    type Read =
        unsafe extern "C" fn(*mut c_void, extern "C" fn(c_long, c_int), c_int) -> *const i16;

    pub struct Reader {
        // Kept loaded while the handles live.
        _cdda: libloading::Library,
        _paranoia: libloading::Library,
        drive: *mut c_void,
        p: *mut c_void,
        close: Close,
        free: Free,
        seek: Seek,
        read: Read,
    }

    impl Reader {
        pub fn open(device: &str) -> Result<Reader, String> {
            let missing = || {
                "the CD reading library (libcdio-paranoia) is missing: on Ubuntu, sudo apt install libcdio-paranoia2t64".to_owned()
            };
            let cdda = crate::disc::open_bundled_library(&["libcdio_cdda.so.2", "libcdio_cdda.so"])
                .ok_or_else(missing)?;
            let paranoia = crate::disc::open_bundled_library(&[
                "libcdio_paranoia.so.2",
                "libcdio_paranoia.so",
            ])
            .ok_or_else(missing)?;
            // SAFETY: the signatures match libcdio-paranoia 10.2+2.x's headers
            // (cdio/paranoia/cdda.h and paranoia.h).
            unsafe {
                let sym = |e: libloading::Error| format!("libcdio-paranoia is too old: {e}");
                let identify = *cdda
                    .get::<Identify>(b"cdio_cddap_identify\0")
                    .map_err(sym)?;
                let open = *cdda.get::<Open>(b"cdio_cddap_open\0").map_err(sym)?;
                let close = *cdda.get::<Close>(b"cdio_cddap_close\0").map_err(sym)?;
                let init = *paranoia.get::<Init>(b"cdio_paranoia_init\0").map_err(sym)?;
                let free = *paranoia.get::<Free>(b"cdio_paranoia_free\0").map_err(sym)?;
                let modeset = *paranoia
                    .get::<ModeSet>(b"cdio_paranoia_modeset\0")
                    .map_err(sym)?;
                let seek = *paranoia.get::<Seek>(b"cdio_paranoia_seek\0").map_err(sym)?;
                let read = *paranoia
                    .get::<Read>(b"cdio_paranoia_read_limited\0")
                    .map_err(sym)?;
                let dev = CString::new(device).map_err(|_| "bad device name".to_owned())?;
                let drive = identify(dev.as_ptr(), 0, std::ptr::null_mut());
                if drive.is_null() {
                    return Err(format!("couldn't open the CD in {device}"));
                }
                if open(drive) != 0 {
                    close(drive);
                    return Err(format!("couldn't read the CD in {device}"));
                }
                let p = init(drive);
                if p.is_null() {
                    close(drive);
                    return Err("couldn't start cdparanoia".into());
                }
                modeset(p, MODE);
                Ok(Reader {
                    _cdda: cdda,
                    _paranoia: paranoia,
                    drive,
                    p,
                    close,
                    free,
                    seek,
                    read,
                })
            }
        }

        /// Read `sectors` sectors from `lba` on and encode them as FLAC.
        pub fn read_flac(
            &mut self,
            lba: u32,
            sectors: u32,
            cancel: &AtomicBool,
            progress: impl Fn(f32),
        ) -> Result<Vec<u8>, String> {
            // SAFETY: p is a live paranoia handle; SEEK_SET = 0.
            if unsafe { (self.seek)(self.p, lba as i32, 0) } < 0 {
                return Err(format!("couldn't seek to sector {lba}"));
            }
            let mut src = Sectors {
                reader: self,
                left: sectors,
                total: sectors,
                cancel,
                progress,
                error: None,
                pending: Vec::new(),
            };
            let mut config = flacenc::config::Encoder::default();
            config.multithread = false;
            let config = config.into_verified().map_err(|e| format!("{e:?}"))?;
            let block = config.block_size;
            let stream = flacenc::encode_with_fixed_block_size(&config, &mut src, block);
            if let Some(e) = src.error.take() {
                return Err(e);
            }
            let stream = stream.map_err(|e| format!("FLAC encoder: {e}"))?;
            let mut sink = flacenc::bitsink::ByteSink::new();
            stream
                .write(&mut sink)
                .map_err(|e| format!("FLAC encoder: {e:?}"))?;
            Ok(sink.into_inner())
        }

        /// One sector's 1176 samples (interleaved, host order).
        fn sector(&mut self) -> Option<&[i16]> {
            // SAFETY: the returned buffer holds one raw sector and stays valid
            // until the next read.
            let data = unsafe { (self.read)(self.p, callback, RETRIES) };
            (!data.is_null())
                .then(|| unsafe { std::slice::from_raw_parts(data, SECTOR_SAMPLES * 2) })
        }
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            // SAFETY: freed once, paranoia before its drive.
            unsafe {
                (self.free)(self.p);
                (self.close)(self.drive);
            }
        }
    }

    /// The sectors of one song, as a FLAC encoder source.
    struct Sectors<'a, P: Fn(f32)> {
        reader: &'a mut Reader,
        left: u32,
        total: u32,
        cancel: &'a AtomicBool,
        progress: P,
        error: Option<String>,
        /// Read but not yet handed to the encoder (interleaved).
        pending: Vec<i32>,
    }

    impl<P: Fn(f32)> Source for Sectors<'_, P> {
        fn channels(&self) -> usize {
            2
        }
        fn bits_per_sample(&self) -> usize {
            16
        }
        fn sample_rate(&self) -> usize {
            44_100
        }
        fn len_hint(&self) -> Option<usize> {
            Some(self.total as usize * SECTOR_SAMPLES)
        }
        fn read_samples<F: Fill>(
            &mut self,
            block_size: usize,
            dest: &mut F,
        ) -> Result<usize, flacenc::error::SourceError> {
            let fail = |s: &mut Self, e: String| {
                s.error = Some(e);
                Err(flacenc::error::SourceError::from_unknown())
            };
            // Every frame but the last must hold exactly `block_size` samples,
            // so sectors are read until there are enough (the rest waits).
            while self.pending.len() < block_size * 2 && self.left > 0 {
                if self.cancel.load(Ordering::Relaxed) {
                    return fail(self, "Cancelled".into());
                }
                match self.reader.sector() {
                    Some(s) => self.pending.extend(s.iter().map(|&x| i32::from(x))),
                    None => return fail(self, "the drive stopped reading the CD".into()),
                }
                self.left -= 1;
            }
            (self.progress)(1.0 - self.left as f32 / self.total.max(1) as f32);
            let n = self.pending.len().min(block_size * 2);
            if n == 0 {
                return Ok(0);
            }
            dest.fill_interleaved(&self.pending[..n])?;
            self.pending.drain(..n);
            Ok(n / 2)
        }
    }
}

/// The Vorbis comments of song `i` (0-based, number `number` of `total`).
fn tags(album: Option<&Album>, i: usize, number: u8, total: usize) -> Vec<(&'static str, String)> {
    let mut t = vec![
        ("TRACKNUMBER", number.to_string()),
        ("TRACKTOTAL", total.to_string()),
    ];
    match album {
        Some(a) => {
            t.push(("TITLE", a.track_title(i, number)));
            t.push(("ARTIST", a.track_artist(i)));
            t.push(("ALBUM", a.title.clone()));
            t.push(("ALBUMARTIST", a.artist.clone()));
            t.push(("DATE", a.date.clone()));
            if let Some(id) = &a.release_id {
                t.push(("MUSICBRAINZ_ALBUMID", id.clone()));
            }
        }
        None => t.push(("TITLE", format!("Track {number}"))),
    }
    t.retain(|(_, v)| !v.trim().is_empty());
    t
}

/// `flac` (a FLAC stream whose only metadata block is STREAMINFO, as the
/// encoder writes it) with the tags, the front cover and some padding (so
/// tag editors don't have to rewrite the file) added after STREAMINFO.
/// Written here, not by a tag library afterwards, so the audio frames are
/// never rewritten.
fn with_tags(
    flac: &[u8],
    tags: &[(&str, String)],
    cover: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    // "fLaC", then STREAMINFO: a 4-byte header and 34 bytes.
    if flac.len() < 42 || &flac[..4] != b"fLaC" || flac[4] & 0x7f != 0 {
        return Err("the FLAC encoder wrote something unexpected".into());
    }
    let block = |out: &mut Vec<u8>, kind: u8, last: bool, body: &[u8]| -> Result<(), String> {
        let len = u32::try_from(body.len())
            .ok()
            .filter(|&l| l < 1 << 24)
            .ok_or("a tag is too large")?;
        out.push(kind | if last { 0x80 } else { 0 });
        out.extend(&len.to_be_bytes()[1..]);
        out.extend(body);
        Ok(())
    };
    let mut comments = Vec::new();
    let vendor = concat!("Super Media Player ", env!("CARGO_PKG_VERSION"));
    comments.extend((vendor.len() as u32).to_le_bytes());
    comments.extend(vendor.as_bytes());
    comments.extend((tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let field = format!("{k}={}", v.replace(['\n', '\r'], " "));
        comments.extend((field.len() as u32).to_le_bytes());
        comments.extend(field.as_bytes());
    }
    let mut out =
        Vec::with_capacity(flac.len() + comments.len() + cover.map_or(0, <[u8]>::len) + 8300);
    out.extend(b"fLaC");
    out.push(flac[4] & 0x7f); // STREAMINFO, no longer the last block
    out.extend(&flac[5..42]);
    block(&mut out, 4, false, &comments)?;
    if let Some(data) = cover {
        let mime = if data.starts_with(b"\x89PNG") {
            "image/png"
        } else {
            "image/jpeg"
        };
        let mut pic = Vec::new();
        pic.extend(3u32.to_be_bytes()); // front cover
        pic.extend((mime.len() as u32).to_be_bytes());
        pic.extend(mime.as_bytes());
        pic.extend(0u32.to_be_bytes()); // no description
        pic.extend([0u8; 16]); // size and colours: unknown
        pic.extend((data.len() as u32).to_be_bytes());
        pic.extend(data);
        block(&mut out, 6, false, &pic)?;
    }
    block(&mut out, 1, true, &[0u8; 8192])?;
    out.extend(&flac[42..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example disc from python-discid's documentation and tests.
    #[test]
    fn disc_id_matches_musicbrainz() {
        let starts = [150u32, 15363, 32314, 46592, 63414, 80489];
        let mut toc: Vec<(u8, u32, bool)> = starts
            .iter()
            .enumerate()
            .map(|(i, &s)| (i as u8 + 1, s - 150, false))
            .collect();
        toc.push((0xAA, 95462 - 150, false));
        assert_eq!(
            disc_id(&toc).as_deref(),
            Some("49HHV7Eb8UKF3aQiNmu1GR8vKTY-")
        );
    }

    /// An Enhanced CD: the data track and its gap are not part of the ID.
    #[test]
    fn enhanced_cd_leaves_the_data_track_out() {
        let plain = vec![(1, 0, false), (2, 20_000, false), (0xAA, 40_000, false)];
        let enhanced = vec![
            (1, 0, false),
            (2, 20_000, false),
            (3, 51_400, true),
            (0xAA, 90_000, false),
        ];
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
        assert_eq!(
            a.tracks,
            vec![
                TrackName {
                    title: "One".into(),
                    artist: "A".into()
                },
                TrackName {
                    title: "Two".into(),
                    artist: "C".into()
                },
            ]
        );
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
        assert_eq!(
            a.tracks[1],
            TrackName {
                title: "First song".into(),
                artist: "Guest".into()
            }
        );
        assert_eq!(a.tracks[2].title, "Third");
        assert!(parse_cd_text(&[]).is_none());
    }

    #[test]
    fn rip_folder_names() {
        let a = Album {
            title: "A/B: C".into(),
            artist: ".Hidden".into(),
            ..Default::default()
        };
        assert_eq!(
            rip_folder(Path::new("/m"), Some(&a), None),
            Path::new("/m/Hidden/A_B_ C")
        );
        assert_eq!(
            rip_folder(Path::new("/m"), None, Some("49HHV7Eb8UKF")),
            Path::new("/m/Audio CD 49HHV7Eb")
        );
    }

    /// mpv takes the tags from the EDL header, escapes and all.
    #[test]
    fn edl_tags_reach_mpv() {
        let Ok(player) = player_core::Player::new(&player_core::PlayerConfig::headless(), |_| {})
        else {
            eprintln!("no libmpv, skipped");
            return;
        };
        let album = Album {
            title: "Greatest; Hits, Vol. 1".into(),
            artist: "Ünïcode = Band".into(),
            tracks: vec![TrackName {
                title: "Song, with %percent%".into(),
                artist: String::new(),
            }],
            ..Default::default()
        };
        let t = CdTrack {
            number: 1,
            start: 0,
            frames: 75 * 3,
        };
        // A WAV file stands in for the CD (mpv 0.42 has no av://lavfi).
        let wav = std::env::temp_dir().join(format!("smp-edl-{}.wav", std::process::id()));
        let samples = 44_100 * 3 * 2;
        let mut data = Vec::new();
        for (tag, v) in [(b"RIFF", 36 + samples * 2), (b"WAVE", 0), (b"fmt ", 16)] {
            data.extend(tag);
            if tag != b"WAVE" {
                data.extend((v as u32).to_le_bytes());
            }
        }
        data.extend([1u8, 0, 2, 0]); // PCM, stereo
        data.extend(44_100u32.to_le_bytes());
        data.extend((44_100u32 * 4).to_le_bytes());
        data.extend([4u8, 0, 16, 0]);
        data.extend(b"data");
        data.extend((samples as u32 * 2).to_le_bytes());
        data.resize(data.len() + samples * 2, 0);
        std::fs::write(&wav, data).unwrap();
        let edl = track_edl(&t, 0, Some(&album))
            .replace("%7%cdda://", &edl_escape(&wav.to_string_lossy()));
        let _ = player.set_string("ao", "null");
        if let Some(log) = std::env::var_os("SMP_TEST_MPV_LOG") {
            let _ = player.set_string("log-file", &log.to_string_lossy());
        }
        player.load(&edl, player_core::LoadMode::Replace).unwrap();
        let mut tags = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if let Some(t) = player.get_string("metadata/by-key/title") {
                tags = Some((
                    t,
                    player.get_string("metadata/by-key/artist"),
                    player.get_string("metadata/by-key/album"),
                ));
                break;
            }
        }
        let (title, artist, alb) = tags.expect("no metadata from the EDL");
        assert_eq!(title, "Song, with %percent%");
        assert_eq!(artist.as_deref(), Some("Ünïcode = Band"));
        assert_eq!(alb.as_deref(), Some("Greatest; Hits, Vol. 1"));
        let _ = std::fs::remove_file(&wav);
    }

    /// A ripped song's tags and cover, readable by tag libraries, and the
    /// audio still decodes to exactly the samples encoded.
    #[test]
    fn ripped_file_is_tagged() {
        use flacenc::component::BitRepr;
        use flacenc::error::Verify;
        let samples: Vec<i32> = (0..44_100 * 2)
            .map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i32)
            .collect();
        let config = flacenc::config::Encoder::default().into_verified().unwrap();
        let src = flacenc::source::MemSource::from_samples(&samples, 2, 16, 44_100);
        let stream =
            flacenc::encode_with_fixed_block_size(&config, src, config.block_size).unwrap();
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream.write(&mut sink).unwrap();
        let album = Album {
            title: "Album".into(),
            artist: "Band".into(),
            date: "1999".into(),
            tracks: vec![TrackName {
                title: "Song = One".into(),
                artist: "Guest".into(),
            }],
            release_id: Some("abc".into()),
            source: "MusicBrainz".into(),
        };
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0xFF, 0xD9];
        let bytes = with_tags(sink.as_slice(), &tags(Some(&album), 0, 1, 12), Some(&jpeg)).unwrap();
        let dir = std::env::temp_dir().join(format!("smp-cd-tag-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("01 - Song.flac");
        std::fs::write(&out, &bytes).unwrap();
        use lofty::prelude::*;
        let file = lofty::read_from_path(&out).unwrap();
        let t = file.primary_tag().unwrap();
        assert_eq!(t.title().as_deref(), Some("Song = One"));
        assert_eq!(t.artist().as_deref(), Some("Guest"));
        assert_eq!(t.album().as_deref(), Some("Album"));
        assert_eq!(t.track(), Some(1));
        assert_eq!(t.track_total(), Some(12));
        assert_eq!(t.get_string(&ItemKey::AlbumArtist), Some("Band"));
        assert_eq!(t.pictures().len(), 1);
        assert_eq!(file.properties().duration().as_millis(), 1000);
        // The ffmpeg program, when there, decodes every sample back.
        if let Ok(o) = crate::process::command("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-f", "s16le", "-"])
            .output()
        {
            let back: Vec<i32> = o
                .stdout
                .chunks(2)
                .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])))
                .collect();
            assert!(o.status.success() && back == samples, "decoded differently");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Names the CD in a drive as the app does and rips some of it:
    /// `SMP_TEST_CD=/dev/sr0 SMP_TEST_CD_SONGS=2 cargo test cd_this_machine -- --ignored --nocapture`
    /// (files in SMP_TEST_OUT, else the temporary folder).
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs an audio CD: SMP_TEST_CD"]
    fn cd_this_machine() {
        let Ok(device) = std::env::var("SMP_TEST_CD") else {
            return;
        };
        let songs: usize = std::env::var("SMP_TEST_CD_SONGS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let root = std::env::var_os("SMP_TEST_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let toc = crate::disc::toc::read(&device).expect("no table of contents");
        let tracks = crate::disc::audio_tracks(&toc);
        let id = disc_id(&toc);
        println!("{} songs, disc ID {id:?}", tracks.len());
        let text = crate::disc::toc::cd_text(&device);
        println!("CD-Text: {:?}", text.as_deref().and_then(parse_cd_text));
        println!("MusicBrainz: {:?}", musicbrainz(id.as_deref().unwrap()));
        let album = names(&toc, || text.as_deref().and_then(parse_cd_text), true);
        for (i, t) in tracks.iter().enumerate() {
            println!(
                "{:02} {:?} {}",
                t.number,
                album.as_ref().map(|a| a.track_title(i, t.number)),
                t.frames / 75
            );
        }
        let art = album.as_ref().and_then(|a| cover(a, true));
        println!("cover: {art:?}");
        let dir = rip_folder(&root, album.as_ref(), id.as_deref());
        let start = std::time::Instant::now();
        let origin = toc.iter().find(|e| !e.2).map_or(0, |e| e.1);
        let report = rip(
            &device,
            &tracks[..songs.min(tracks.len())],
            origin,
            album.as_ref(),
            art.as_deref(),
            &dir,
            &AtomicBool::new(false),
            |_, _| {},
        )
        .unwrap();
        println!("{report:?} in {:?}", start.elapsed());
    }
}
