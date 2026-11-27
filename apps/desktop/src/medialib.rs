//! The media library's index: what music and videos are in the folders you
//! chose, with their tags. A SQLite file (`library.db` in the data folder);
//! nothing of it stays in memory while the library is closed.
//!
//! A scan walks the folders, reads the tags of new and changed files (by
//! time and size) and drops files that are gone. It never drops files it
//! couldn't look at: an unplugged drive or an unreadable folder keeps its
//! entries. Media files themselves are only ever read.
//!
//! Files without tags get their artist and title from names such as
//! "Artist - Title.mp3", and are always reachable through their folder.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use unicode_normalization::UnicodeNormalization;

use crate::settings;

/// Audio file extensions; the rest of the player's media files are videos.
const AUDIO: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "ape", "wv", "alac", "aiff",
    "aif", "mka", "ac3", "dts", "mid", "midi", "mod", "xm", "it", "s3m", "tta", "dsf", "dff",
];
/// Songs in one list at most (also what one "Play" queues).
pub const MAX_SONGS: usize = 10_000;
const SCHEMA: i64 = 1;

pub type Result<T> = std::result::Result<T, String>;

fn err(e: rusqlite::Error) -> String {
    format!("The library couldn't be read ({e})")
}

pub struct Db(Connection);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub path: String,
    pub dir: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track: u32,
    pub disc: u32,
    pub duration: f64,
    pub video: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Album {
    pub key: String,
    pub name: String,
    pub artist: String,
    pub year: u32,
    pub songs: u32,
    pub added: i64,
    /// One of its songs (the cover is found from it).
    pub path: String,
}

/// An artist, a genre or a folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub key: String,
    pub name: String,
    pub items: u32,
    pub albums: u32,
    pub videos: u32,
    pub path: String,
}

/// Which songs or videos.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Songs,
    Videos,
    Album(String),
    Artist(String),
    Genre(String),
    Folder(String),
    /// Songs (false) or videos (true) matching every word.
    Search(String, bool),
    /// The newest songs (false) or videos (true).
    Newest(bool, usize),
}

/// Which albums.
#[derive(Clone, Debug, PartialEq)]
pub enum Albums {
    All,
    Artist(String),
    Genre(String),
    Search(String),
    Newest(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Name,
    Artist,
    Album,
    Newest,
    Year,
    Duration,
}

// ---- text ----

/// The form names are compared and searched in: lower case, no accents
/// ("Șoapte" → "soapte"), single spaces.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfkd().filter(|c| !('\u{300}'..='\u{36f}').contains(c)) {
        match c {
            '’' | '‘' | '`' => out.push('\''),
            c if c.is_whitespace() || c.is_control() => {
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
            c => out.extend(c.to_lowercase()),
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// A tag as one clean line.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The LIKE pattern for text anywhere.
fn like(word: &str) -> String {
    let mut p = String::from("%");
    for c in word.chars() {
        if matches!(c, '%' | '_' | '\\') {
            p.push('\\');
        }
        p.push(c);
    }
    p.push('%');
    p
}

fn words(text: &str) -> Vec<String> {
    fold(text)
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(like)
        .collect()
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_audio(p: &Path) -> bool {
    AUDIO.contains(&ext_of(p).as_str())
}

/// Track number, artist and title from a file name: "03 - Artist - Title",
/// "Artist – Title", "07. Title", "Title".
pub fn from_name(path: &Path) -> (u32, String, String) {
    let mut stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // "Song.mp4.mp3" from download sites.
    let inner = Path::new(&stem);
    if crate::is_folder_media(inner) {
        stem = inner
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    let name = if stem.contains(' ') {
        stem.clone()
    } else {
        stem.replace('_', " ")
    };
    let mut rest = name.trim();
    let mut track = 0;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if (1..=3).contains(&digits) {
        let after = &rest[digits..];
        let trimmed = after.trim_start();
        let body = trimmed.trim_start_matches(['.', '-', ')', '–', '—']);
        // "03 - Title", "3. Title", "03 Title"; not "50 Cent", "1999" or "7-я".
        let marked = body.len() < trimmed.len() && body.starts_with(' ');
        if !body.trim().is_empty() && (marked || (rest.starts_with('0') && after.starts_with(' ')))
        {
            track = rest[..digits].parse().unwrap_or(0);
            rest = body.trim();
        }
    }
    let split = [" - ", " – ", " — "]
        .iter()
        .filter_map(|s| rest.find(s).map(|i| (i, s.len())))
        .min();
    match split {
        Some((i, n)) if !rest[..i].trim().is_empty() && !rest[i + n..].trim().is_empty() => (
            track,
            rest[..i].trim().to_owned(),
            rest[i + n..].trim().to_owned(),
        ),
        _ => (track, String::new(), rest.to_owned()),
    }
}

/// The folder an album's songs share: the song's own, or the one above for
/// a "CD1" / "Disc 2" folder.
fn album_dir(path: &Path) -> String {
    let dir = path.parent().unwrap_or(Path::new(""));
    let name = fold(
        &dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let disc = ["cd", "disc", "disk"].iter().any(|p| {
        name.strip_prefix(p).is_some_and(|r| {
            let r = r.trim_start_matches([' ', '_', '-', '.']);
            !r.is_empty() && r.len() <= 2 && r.chars().all(|c| c.is_ascii_digit())
        })
    });
    let dir = if disc {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    };
    dir.to_string_lossy().into_owned()
}

fn genre_list(genre: &str) -> Vec<String> {
    genre
        .split([';', ',', '\0'])
        .map(clean)
        .filter(|g| !g.is_empty())
        .collect()
}

// ---- reading a file ----

/// What the index keeps of a file.
#[derive(Clone, Debug, Default, PartialEq)]
struct Entry {
    path: String,
    dir: String,
    video: bool,
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    genre: String,
    year: u32,
    disc: u32,
    track: u32,
    duration: f64,
}

fn read_file(path: &Path, text: &str) -> Entry {
    let mut e = Entry {
        path: text.to_owned(),
        dir: path
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default(),
        video: !is_audio(path),
        ..Default::default()
    };
    // Videos: the file name says more than their tags, and names an
    // "artist" only by accident ("Film - Part 1").
    if e.video {
        e.title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        return e;
    }
    // A broken file mustn't end the scan, not even by a bug in the reader.
    let owned = path.to_path_buf();
    if let Ok(Some(t)) = std::panic::catch_unwind(move || read_tags(&owned)) {
        e = Entry {
            path: e.path,
            dir: e.dir,
            video: false,
            ..t
        };
    }
    let (track, artist, title) = from_name(path);
    if e.track == 0 {
        e.track = track;
    }
    if e.title.is_empty() {
        // "Artist - Title.mp3": both from the name, unless a tag has the artist.
        e.title = title;
        if e.artist.is_empty() {
            e.artist = artist;
        }
    }
    e
}

fn read_tags(path: &Path) -> Option<Entry> {
    use lofty::config::{ParseOptions, ParsingMode};
    use lofty::prelude::*;
    let options = ParseOptions::new()
        .read_cover_art(false)
        .parsing_mode(ParsingMode::Relaxed);
    let tagged = lofty::probe::Probe::open(path)
        .ok()?
        .options(options)
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    let mut e = Entry {
        duration: tagged.properties().duration().as_secs_f64(),
        ..Default::default()
    };
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        let text = |v: Option<std::borrow::Cow<str>>| v.map(|s| clean(&s)).unwrap_or_default();
        e.title = text(tag.title());
        e.artist = text(tag.artist());
        e.album = text(tag.album());
        e.album_artist = tag
            .get_string(ItemKey::AlbumArtist)
            .map(clean)
            .unwrap_or_default();
        let genres: Vec<String> = tag
            .get_strings(ItemKey::Genre)
            .flat_map(genre_list)
            .collect();
        e.genre = genres.join("; ");
        e.year = tag.date().map(|d| u32::from(d.year)).filter(|y| (1000..3000).contains(y)).unwrap_or(0);
        e.disc = tag.disk().unwrap_or(0);
        e.track = tag.track().unwrap_or(0);
    }
    Some(e)
}

/// The artist an item is listed under.
fn main_artist(e: &Entry) -> &str {
    let various = [
        "various artists",
        "various",
        "va",
        "v.a.",
        "verschiedene interpreten",
        "сборник",
    ];
    if e.album_artist.is_empty()
        || (various.contains(&fold(&e.album_artist).as_str()) && !e.artist.is_empty())
    {
        &e.artist
    } else {
        &e.album_artist
    }
}

// ---- the database ----

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn mtime(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

fn track_of(r: &Row) -> rusqlite::Result<Track> {
    Ok(Track {
        path: r.get(0)?,
        dir: r.get(1)?,
        title: r.get(2)?,
        artist: r.get(3)?,
        album: r.get(4)?,
        track: r.get(5)?,
        disc: r.get(6)?,
        duration: r.get(7)?,
        video: r.get(8)?,
    })
}

/// Whether `path` is inside folder `root`.
fn inside(path: &str, root: &str) -> bool {
    Path::new(path).starts_with(root)
}

impl Db {
    pub fn open() -> Result<Self> {
        let dir = settings::data_dir().ok_or("There is no data folder for the library")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Self::open_at(&dir.join("library.db"))
    }

    pub fn open_at(file: &Path) -> Result<Self> {
        let c = Connection::open(file).map_err(err)?;
        c.busy_timeout(Duration::from_secs(10)).map_err(err)?;
        // Readers go on while a scan writes.
        c.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(err)?;
        c.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA cache_size = -4096;")
            .map_err(err)?;
        let version: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version != SCHEMA {
            // Everything but the list of folders can be made again by a scan.
            c.execute_batch(&format!(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS folders (path TEXT PRIMARY KEY);
                 CREATE TABLE IF NOT EXISTS probes (
                    path TEXT PRIMARY KEY, mtime INTEGER NOT NULL, duration REAL NOT NULL, height INTEGER NOT NULL);
                 DROP TABLE IF EXISTS media;
                 CREATE TABLE media (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, dir TEXT NOT NULL,
                    mtime INTEGER NOT NULL, size INTEGER NOT NULL, added INTEGER NOT NULL, video INTEGER NOT NULL,
                    title TEXT NOT NULL, artist TEXT NOT NULL, album TEXT NOT NULL, album_artist TEXT NOT NULL,
                    genre TEXT NOT NULL, year INTEGER NOT NULL, disc INTEGER NOT NULL, track INTEGER NOT NULL,
                    duration REAL NOT NULL, artist_name TEXT NOT NULL, artist_key TEXT NOT NULL,
                    album_key TEXT NOT NULL, album_fold TEXT NOT NULL, genre_key TEXT NOT NULL, search TEXT NOT NULL);
                 CREATE INDEX media_dir ON media (dir);
                 CREATE INDEX media_album ON media (album_key);
                 CREATE INDEX media_artist ON media (artist_key);
                 CREATE INDEX media_added ON media (video, added);
                 PRAGMA user_version = {SCHEMA};
                 COMMIT;"
            ))
            .map_err(err)?;
        }
        Ok(Self(c))
    }

    fn list<T>(
        &self,
        sql: &str,
        args: &[&dyn ToSql],
        f: impl FnMut(&Row) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut st = self.0.prepare(sql).map_err(err)?;
        let rows = st.query_map(args, f).map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<T>>>().map_err(err)
    }

    pub fn folders(&self) -> Result<Vec<String>> {
        self.list("SELECT path FROM folders ORDER BY path", &[], |r| r.get(0))
    }

    pub fn add_folder(&self, dir: &Path) -> Result<()> {
        let dir = crate::process::full_path(dir);
        let text = dir.to_str().ok_or("That folder's name isn't valid text")?;
        self.0
            .execute("INSERT OR IGNORE INTO folders VALUES (?1)", [text])
            .map_err(err)?;
        Ok(())
    }

    /// Take a folder out of the library (its files stay on the disk).
    pub fn remove_folder(&mut self, dir: &str) -> Result<()> {
        let left: Vec<String> = self.folders()?.into_iter().filter(|f| f != dir).collect();
        let tx = self.0.transaction().map_err(err)?;
        tx.execute("DELETE FROM folders WHERE path = ?1", [dir])
            .map_err(err)?;
        let gone: Vec<(i64, String)> = {
            let mut st = tx.prepare("SELECT id, path FROM media").map_err(err)?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
        };
        for (id, path) in gone {
            if inside(&path, dir) && !left.iter().any(|f| inside(&path, f)) {
                tx.execute("DELETE FROM media WHERE id = ?1", [id])
                    .map_err(err)?;
            }
        }
        tx.commit().map_err(err)
    }

    /// Songs and videos in the library.
    pub fn counts(&self) -> Result<(u32, u32)> {
        self.0
            .query_row("SELECT COUNT(*) FILTER (WHERE video = 0), COUNT(*) FILTER (WHERE video = 1) FROM media", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(err)
    }

    pub fn tracks(&self, pick: &Pick, sort: Sort) -> Result<Vec<Track>> {
        let video = |v: bool| if v { "m.video = 1" } else { "m.video = 0" };
        let mut args: Vec<String> = Vec::new();
        let mut limit = MAX_SONGS;
        let filter = match pick {
            Pick::Songs => video(false).to_owned(),
            Pick::Videos => video(true).to_owned(),
            Pick::Album(k) => {
                args.push(k.clone());
                "m.video = 0 AND m.album_key = ?1".into()
            }
            Pick::Artist(k) => {
                args.push(k.clone());
                "m.video = 0 AND m.artist_key = ?1".into()
            }
            Pick::Genre(k) => {
                args.push(like(&format!(";{k};")));
                "m.video = 0 AND m.genre_key LIKE ?1 ESCAPE '\\'".into()
            }
            Pick::Folder(d) => {
                args.push(d.clone());
                "m.dir = ?1".into()
            }
            Pick::Search(text, v) => {
                args = words(text);
                if args.is_empty() {
                    return Ok(Vec::new());
                }
                let mut f = video(*v).to_owned();
                for i in 1..=args.len() {
                    f += &format!(" AND m.search LIKE ?{i} ESCAPE '\\'");
                }
                f
            }
            Pick::Newest(v, n) => {
                limit = *n;
                video(*v).to_owned()
            }
        };
        let newest = sort == Sort::Newest || matches!(pick, Pick::Newest(..));
        // Newest first must be cut off by the database, not after it.
        let order = if newest {
            "ORDER BY m.added DESC, m.id DESC"
        } else {
            ""
        };
        let cut = if newest {
            format!("LIMIT {limit}")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT m.path, m.dir, m.title, m.artist, m.album, m.track, m.disc,
                    CASE WHEN m.duration > 0 THEN m.duration ELSE COALESCE(p.duration, 0) END, m.video
             FROM media m LEFT JOIN probes p ON p.path = m.path WHERE {filter} {order} {cut}"
        );
        let args: Vec<&dyn ToSql> = args.iter().map(|a| a as &dyn ToSql).collect();
        let mut out = self.list(&sql, &args, track_of)?;
        if newest {
            return Ok(out);
        }
        // What decides first; the album's running order settles the rest.
        let first: fn(&Track) -> String = match (pick, sort) {
            (Pick::Folder(_), Sort::Name) => |t| fold(&t.path),
            (Pick::Album(_), _) => |_| String::new(),
            (Pick::Artist(_), Sort::Name) => |t| {
                if t.album.is_empty() {
                    "\u{10ffff}".into()
                } else {
                    fold(&t.album)
                }
            },
            (_, Sort::Artist) => |t| fold(&t.artist),
            (_, Sort::Album) => |t| {
                if t.album.is_empty() {
                    "\u{10ffff}".into()
                } else {
                    fold(&t.album)
                }
            },
            (_, Sort::Duration) => |t| format!("{:09}", 99_999_999 - t.duration as i64),
            _ => |t| fold(&t.title),
        };
        out.sort_by_cached_key(|t| {
            let running = !matches!(sort, Sort::Duration);
            Natural(format!(
                "{}\u{1}{}\u{1}{:04}{:05}\u{1}{}",
                first(t),
                if running {
                    fold(&t.album)
                } else {
                    String::new()
                },
                t.disc,
                t.track,
                fold(&t.path)
            ))
        });
        out.truncate(limit);
        Ok(out)
    }

    pub fn albums(&self, which: &Albums, sort: Sort) -> Result<Vec<Album>> {
        let mut args: Vec<String> = Vec::new();
        let mut limit = usize::MAX;
        let filter = match which {
            Albums::All => String::new(),
            Albums::Artist(k) => {
                args.push(k.clone());
                "AND album_key IN (SELECT album_key FROM media WHERE artist_key = ?1)".into()
            }
            Albums::Genre(k) => {
                args.push(like(&format!(";{k};")));
                "AND album_key IN (SELECT album_key FROM media WHERE genre_key LIKE ?1 ESCAPE '\\')"
                    .into()
            }
            Albums::Search(text) => {
                args = words(text);
                if args.is_empty() {
                    return Ok(Vec::new());
                }
                (1..=args.len())
                    .map(|i| format!(" AND album_fold LIKE ?{i} ESCAPE '\\'"))
                    .collect()
            }
            Albums::Newest(n) => {
                limit = *n;
                String::new()
            }
        };
        let sql = format!(
            "SELECT album_key, MIN(album),
                    CASE WHEN MAX(album_artist) <> '' THEN MAX(album_artist)
                         WHEN COUNT(DISTINCT artist_key) = 1 THEN MIN(artist_name) ELSE 'Various artists' END,
                    MAX(year), COUNT(*), MAX(added), MIN(path)
             FROM media WHERE video = 0 AND album <> '' {filter} GROUP BY album_key"
        );
        let args: Vec<&dyn ToSql> = args.iter().map(|a| a as &dyn ToSql).collect();
        let mut out = self.list(&sql, &args, |r| {
            Ok(Album {
                key: r.get(0)?,
                name: r.get(1)?,
                artist: r.get(2)?,
                year: r.get(3)?,
                songs: r.get(4)?,
                added: r.get(5)?,
                path: r.get(6)?,
            })
        })?;
        match (which, sort) {
            (Albums::Newest(_), _) | (_, Sort::Newest) => {
                out.sort_by_key(|a| (-a.added, fold(&a.name)))
            }
            (_, Sort::Artist) => {
                out.sort_by_cached_key(|a| {
                    Natural(format!(
                        "{}\u{1}{:04}{}",
                        fold(&a.artist),
                        a.year,
                        fold(&a.name)
                    ))
                });
            }
            (_, Sort::Year) => out.sort_by_key(|a| (std::cmp::Reverse(a.year), fold(&a.name))),
            _ => out.sort_by_cached_key(|a| Natural(fold(&a.name))),
        }
        out.truncate(limit);
        Ok(out)
    }

    /// Artists, all or those matching every word of `search`.
    pub fn artists(&self, search: Option<&str>) -> Result<Vec<Group>> {
        let args = search.map(words).unwrap_or_default();
        if search.is_some() && args.is_empty() {
            return Ok(Vec::new());
        }
        let filter: String = (1..=args.len())
            .map(|i| format!(" AND artist_key LIKE ?{i} ESCAPE '\\'"))
            .collect();
        let sql = format!(
            "SELECT artist_key, MIN(artist_name), COUNT(*), COUNT(DISTINCT CASE WHEN album <> '' THEN album_key END), MIN(path)
             FROM media WHERE video = 0 AND artist_key <> '' {filter} GROUP BY artist_key"
        );
        let args: Vec<&dyn ToSql> = args.iter().map(|a| a as &dyn ToSql).collect();
        let mut out = self.list(&sql, &args, |r| {
            Ok(Group {
                key: r.get(0)?,
                name: r.get(1)?,
                items: r.get(2)?,
                albums: r.get(3)?,
                videos: 0,
                path: r.get(4)?,
            })
        })?;
        out.sort_by_cached_key(|g| Natural(g.key.clone()));
        Ok(out)
    }

    pub fn genres(&self) -> Result<Vec<Group>> {
        let rows: Vec<(String, u32, String)> = self.list(
            "SELECT genre, COUNT(*), MIN(path) FROM media WHERE video = 0 AND genre <> '' GROUP BY genre",
            &[],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut map: HashMap<String, Group> = HashMap::new();
        for (genre, n, path) in rows {
            let mut seen = HashSet::new();
            for name in genre_list(&genre) {
                let key = fold(&name);
                if key.is_empty() || !seen.insert(key.clone()) {
                    continue;
                }
                let g = map.entry(key.clone()).or_insert_with(|| Group {
                    key,
                    name,
                    path: path.clone(),
                    ..Default::default()
                });
                g.items += n;
            }
        }
        let mut out: Vec<Group> = map.into_values().collect();
        out.sort_by_cached_key(|g| Natural(g.key.clone()));
        Ok(out)
    }

    /// The folders that hold media, as a path below the library's folders.
    pub fn dirs(&self) -> Result<Vec<Group>> {
        let roots = self.folders()?;
        let mut out = self.list(
            "SELECT dir, COUNT(*), SUM(video), MIN(path) FROM media GROUP BY dir",
            &[],
            |r| {
                Ok(Group {
                    key: r.get(0)?,
                    items: r.get(1)?,
                    videos: r.get(2)?,
                    path: r.get(3)?,
                    ..Default::default()
                })
            },
        )?;
        for g in &mut out {
            g.name = dir_name(&g.key, &roots);
        }
        out.sort_by_cached_key(|g| Natural(fold(&g.key)));
        Ok(out)
    }

    /// The length of a video, if it was ever measured and didn't change.
    pub fn duration(&self, path: &str) -> Option<f64> {
        let found: Option<(i64, f64)> = self
            .0
            .query_row(
                "SELECT mtime, duration FROM probes WHERE path = ?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()?;
        let tagged = || {
            self.0
                .query_row(
                    "SELECT duration FROM media WHERE path = ?1 AND duration > 0",
                    [path],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten()
        };
        let current = std::fs::metadata(path).map(|m| mtime(&m)).ok();
        found
            .filter(|(t, _)| Some(*t) == current)
            .map(|(_, d)| d)
            .or_else(tagged)
    }

    /// Keep what opening a video told about it.
    pub fn set_probe(&self, path: &str, duration: f64, height: i64) -> Result<()> {
        let t = std::fs::metadata(path)
            .map(|m| mtime(&m))
            .map_err(|e| e.to_string())?;
        self.0
            .execute(
                "INSERT OR REPLACE INTO probes VALUES (?1, ?2, ?3, ?4)",
                params![path, t, duration, height],
            )
            .map_err(err)?;
        Ok(())
    }

    /// The title the library shows for a file.
    pub fn title(&self, path: &str) -> Option<(String, String)> {
        self.0
            .query_row(
                "SELECT title, artist FROM media WHERE path = ?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()?
    }

    /// Bring the index up to date. `progress(done, of)` runs now and then;
    /// returning false stops the scan (what is done so far stays).
    pub fn scan(&mut self, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Scan> {
        let mut result = Scan::default();
        let roots = self.folders()?;
        let mut known: HashMap<String, (i64, i64, i64)> = self
            .list("SELECT path, id, mtime, size FROM media", &[], |r| {
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
            })?
            .into_iter()
            .collect();
        let mut todo: Vec<(PathBuf, String, i64, i64, i64)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Places that couldn't be looked at: what was known there stays.
        let mut unread: Vec<PathBuf> = Vec::new();
        for root in &roots {
            let was_empty = !known.keys().any(|p| inside(p, root));
            let walk = walk(Path::new(root));
            if walk.files.is_empty() && !was_empty {
                // An unmounted drive looks like an empty folder.
                unread.push(PathBuf::from(root));
                result.missing.push(root.clone());
                continue;
            }
            unread.extend(walk.unread);
            for (path, time, size) in walk.files {
                // A name that isn't valid text couldn't be played from a list.
                let Some(text) = path.to_str().map(str::to_owned) else {
                    continue;
                };
                if !seen.insert(text.clone()) {
                    continue;
                }
                if known
                    .get(&text)
                    .is_none_or(|(_, t, s)| (*t, *s) != (time, size))
                {
                    // A first scan: the age of the files says what is new.
                    let added = if was_empty { time.min(now()) } else { now() };
                    todo.push((path, text, time, size, added));
                }
            }
            if !progress(0, todo.len()) {
                return Ok(result);
            }
        }
        result.files = seen.len();
        let total = todo.len();
        for (n, chunk) in todo.chunks(100).enumerate() {
            // Tags are read outside the transaction, so it stays short.
            let entries: Vec<_> = chunk
                .iter()
                .map(|(p, text, t, s, a)| (read_file(p, text), *t, *s, *a))
                .collect();
            let tx = self.0.transaction().map_err(err)?;
            for (e, t, s, added) in &entries {
                put(&tx, e, *t, *s, *added).map_err(err)?;
            }
            tx.commit().map_err(err)?;
            result.read += entries.len();
            if !progress((n + 1) * 100, total) {
                return Ok(result);
            }
        }
        // Gone: known, not seen, and its folder could be looked at.
        known.retain(|p, _| {
            !seen.contains(p) && !unread.iter().any(|u| Path::new(p).starts_with(u))
        });
        let tx = self.0.transaction().map_err(err)?;
        for (id, ..) in known.values() {
            tx.execute("DELETE FROM media WHERE id = ?1", [id])
                .map_err(err)?;
        }
        tx.commit().map_err(err)?;
        result.removed = known.len();
        Ok(result)
    }
}

/// What a scan did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scan {
    pub files: usize,
    pub read: usize,
    pub removed: usize,
    /// Library folders that weren't there (or were empty).
    pub missing: Vec<String>,
}

fn put(c: &Connection, e: &Entry, time: i64, size: i64, added: i64) -> rusqlite::Result<()> {
    let artist = main_artist(e);
    let by = if e.album_artist.is_empty() {
        album_dir(Path::new(&e.path))
    } else {
        fold(&e.album_artist)
    };
    let album_key = if e.album.is_empty() {
        String::new()
    } else {
        format!("{}\u{1f}{by}", fold(&e.album))
    };
    let genres: Vec<String> = genre_list(&e.genre).iter().map(|g| fold(g)).collect();
    let genre_key = if genres.is_empty() {
        String::new()
    } else {
        format!(";{};", genres.join(";"))
    };
    let name = Path::new(&e.path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = Path::new(&e.dir)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let search = fold(&format!(
        "{} {} {} {} {name} {folder}",
        e.title, e.artist, e.album, e.album_artist
    ));
    c.prepare_cached(
        "INSERT INTO media (path, dir, mtime, size, added, video, title, artist, album, album_artist, genre, year,
                            disc, track, duration, artist_name, artist_key, album_key, album_fold, genre_key, search)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
         ON CONFLICT (path) DO UPDATE SET dir=?2, mtime=?3, size=?4, video=?6, title=?7, artist=?8, album=?9,
            album_artist=?10, genre=?11, year=?12, disc=?13, track=?14, duration=?15, artist_name=?16,
            artist_key=?17, album_key=?18, album_fold=?19, genre_key=?20, search=?21",
    )?
    .execute(params![
        e.path,
        e.dir,
        time,
        size,
        added,
        e.video,
        e.title,
        e.artist,
        e.album,
        e.album_artist,
        e.genre,
        e.year,
        e.disc,
        e.track,
        e.duration,
        artist,
        fold(artist),
        album_key,
        fold(&format!("{} {artist}", e.album)),
        genre_key,
        search,
    ])?;
    Ok(())
}

/// A folder's name in the library: below the library folder it is in, whose
/// own name comes first ("Music / Rock / Live").
pub fn dir_name(dir: &str, roots: &[String]) -> String {
    let p = Path::new(dir);
    let root = roots
        .iter()
        .filter(|r| inside(dir, r))
        .max_by_key(|r| r.len());
    let base = root.and_then(|r| Path::new(r).parent());
    match base.and_then(|b| p.strip_prefix(b).ok()) {
        Some(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join(" / "),
        None => dir.to_owned(),
    }
}

struct Walk {
    files: Vec<(PathBuf, i64, i64)>,
    unread: Vec<PathBuf>,
}

/// Every media file under `root`. Like opening a folder: no hidden entries,
/// disc structures or symlinked folders.
fn walk(root: &Path) -> Walk {
    let mut out = Walk {
        files: Vec::new(),
        unread: Vec::new(),
    };
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            out.unread.push(dir);
            continue;
        };
        for e in rd {
            let Ok(e) = e else {
                out.unread.push(dir.clone());
                continue;
            };
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                let skip = ["VIDEO_TS", "AUDIO_TS", "BDMV", "CERTIFICATE"]
                    .contains(&name.to_uppercase().as_str());
                if depth < 20 && !skip {
                    stack.push((path, depth + 1));
                }
            } else if crate::is_folder_media(&path)
                && let Ok(m) = std::fs::metadata(&path)
                && m.is_file()
            {
                out.files.push((path, mtime(&m), m.len() as i64));
            }
        }
    }
    out
}

/// Text in natural order ("Track 2" before "Track 10").
#[derive(PartialEq, Eq)]
struct Natural(String);

impl Ord for Natural {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        crate::natural_cmp(&self.0, &other.0)
    }
}

impl PartialOrd for Natural {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("smp-medialib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }

    /// A short silent WAV with an ID3 tag.
    fn song(path: &Path, title: &str, artist: &str, album: &str, genre: &str, track: u32) {
        use lofty::config::WriteOptions;
        use lofty::prelude::*;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data = vec![0u8; 8000];
        let mut w = Vec::new();
        w.extend(b"RIFF");
        w.extend((36 + data.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend([1, 0, 1, 0]);
        w.extend(8000u32.to_le_bytes());
        w.extend(8000u32.to_le_bytes());
        w.extend([1, 0, 8, 0]);
        w.extend(b"data");
        w.extend((data.len() as u32).to_le_bytes());
        w.extend(data);
        std::fs::write(path, w).unwrap();
        if title.is_empty() {
            return;
        }
        let mut tag = lofty::tag::Tag::new(lofty::tag::TagType::Id3v2);
        tag.set_title(title.into());
        tag.set_artist(artist.into());
        tag.set_album(album.into());
        tag.set_genre(genre.into());
        tag.set_track(track);
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    fn rescan(db: &mut Db) -> Scan {
        db.scan(&mut |_, _| true).unwrap()
    }

    #[test]
    fn folding_and_names() {
        assert_eq!(fold("  Șoapte   ÎN Noapte "), "soapte in noapte");
        assert_eq!(fold("Carla’s Dreams"), "carla's dreams");
        assert_eq!(fold("ЁЛКА Йога"), "елка иога");
        let n = |s: &str| from_name(Path::new(s));
        let t = |k, a: &str, b: &str| (k, a.to_owned(), b.to_owned());
        assert_eq!(n("AK 47 - Ты олень.mp3"), t(0, "AK 47", "Ты олень"));
        assert_eq!(n("NILETTO — Счастливым.mp3"), t(0, "NILETTO", "Счастливым"));
        assert_eq!(
            n("03 - Artist - Title - Live.flac"),
            t(3, "Artist", "Title - Live")
        );
        assert_eq!(n("07. Title.flac"), t(7, "", "Title"));
        assert_eq!(n("01 Title.flac"), t(1, "", "Title"));
        assert_eq!(n("50 Cent - In da Club.mp3"), t(0, "50 Cent", "In da Club"));
        assert_eq!(n("1999.mp3"), t(0, "", "1999"));
        assert_eq!(n("my_song_name.mp3"), t(0, "", "my song name"));
        assert_eq!(n("Song (VIDEO).mp4.mp3"), t(0, "", "Song (VIDEO)"));
        assert_eq!(album_dir(Path::new("/m/Album/CD 2/a.mp3")), "/m/Album");
        assert_eq!(album_dir(Path::new("/m/Cdefg/a.mp3")), "/m/Cdefg");
        assert_eq!(
            dir_name("/home/a/Music/Rock/Live", &["/home/a/Music".into()]),
            "Music / Rock / Live"
        );
    }

    #[test]
    fn scan_groups_and_search() {
        let root = temp("scan");
        song(
            &root.join("X/One/b.wav"),
            "Second",
            "Șatra",
            "One",
            "Rock; Pop",
            2,
        );
        song(
            &root.join("X/One/a.wav"),
            "Tenth",
            "Șatra",
            "One",
            "Rock",
            10,
        );
        song(
            &root.join("X/One/c.wav"),
            "First",
            "Șatra feat. Y",
            "One",
            "Rock",
            1,
        );
        song(
            &root.join("Y/One/a.wav"),
            "Other",
            "Someone",
            "One",
            "Jazz",
            1,
        );
        song(&root.join("loose/Band - 100% Tune.wav"), "", "", "", "", 0);
        std::fs::create_dir_all(root.join("films")).unwrap();
        std::fs::write(root.join("films/Film.2020.mkv"), b"x").unwrap();
        std::fs::write(root.join("films/notes.txt"), b"x").unwrap();
        std::fs::write(root.join("films/broken.mp3"), b"not music").unwrap();

        let db_file = temp("scan-db").join("library.db");
        let mut db = Db::open_at(&db_file).unwrap();
        db.add_folder(&root).unwrap();
        db.add_folder(&root.join("X")).unwrap(); // inside the other: counted once
        let s = rescan(&mut db);
        assert_eq!((s.files, s.read, s.removed), (7, 7, 0));
        assert_eq!(db.counts().unwrap(), (6, 1));

        // Two albums of one name stay apart.
        let albums = db.albums(&Albums::All, Sort::Artist).unwrap();
        assert_eq!(albums.len(), 2);
        assert_eq!((albums[0].artist.as_str(), albums[0].songs), ("Someone", 1));
        assert_eq!(
            (albums[1].artist.as_str(), albums[1].songs),
            ("Various artists", 3)
        );
        let songs = db
            .tracks(&Pick::Album(albums[1].key.clone()), Sort::Name)
            .unwrap();
        let titles: Vec<&str> = songs.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["First", "Second", "Tenth"]);
        assert!((songs[0].duration - 1.0).abs() < 0.1);

        let artists = db.artists(None).unwrap();
        assert!(artists.iter().any(|a| a.name == "Band" && a.albums == 0));
        assert_eq!(db.artists(Some("satra")).unwrap().len(), 2);
        assert_eq!(
            db.albums(&Albums::Artist("satra".into()), Sort::Name)
                .unwrap()
                .len(),
            1
        );
        let genres: Vec<_> = db
            .genres()
            .unwrap()
            .into_iter()
            .map(|g| (g.name, g.items))
            .collect();
        assert_eq!(
            genres,
            [("Jazz".into(), 1), ("Pop".into(), 1), ("Rock".into(), 3)]
        );
        assert_eq!(
            db.tracks(&Pick::Genre("pop".into()), Sort::Name)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.tracks(&Pick::Genre("ro".into()), Sort::Name)
                .unwrap()
                .len(),
            0
        );

        let find = |t: &str, v| {
            db.tracks(&Pick::Search(t.into(), v), Sort::Name)
                .unwrap()
                .len()
        };
        assert_eq!(find("ȘATRA second", false), 1);
        assert_eq!(find("100%", false), 1);
        assert_eq!(find("%", false), 1);
        assert_eq!(find("_", false), 0);
        assert_eq!(find("  ", false), 0);
        assert_eq!(find("film 2020", true), 1);
        assert_eq!(
            db.albums(&Albums::Search("one some".into()), Sort::Name)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.tracks(&Pick::Newest(false, 2), Sort::Name)
                .unwrap()
                .len(),
            2
        );

        // Nothing changed: nothing is read again. One gone, one changed.
        assert_eq!(rescan(&mut db).read, 0);
        std::fs::remove_file(root.join("Y/One/a.wav")).unwrap();
        std::fs::write(root.join("films/Film.2020.mkv"), b"longer").unwrap();
        let s = rescan(&mut db);
        assert_eq!((s.read, s.removed), (1, 1));

        db.remove_folder(root.to_str().unwrap()).unwrap();
        assert_eq!(db.counts().unwrap(), (3, 0)); // X is still in the library
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(db_file.parent().unwrap());
    }

    #[test]
    fn an_unplugged_drive_keeps_its_files() {
        let root = temp("drive");
        song(&root.join("drive/a.wav"), "A", "B", "C", "", 1);
        let dbdir = temp("drive-db");
        let mut db = Db::open_at(&dbdir.join("library.db")).unwrap();
        db.add_folder(&root.join("drive")).unwrap();
        rescan(&mut db);
        std::fs::rename(root.join("drive"), root.join("away")).unwrap();
        let s = rescan(&mut db);
        assert_eq!((s.removed, s.missing.len()), (0, 1));
        std::fs::create_dir(root.join("drive")).unwrap(); // an empty mount point
        assert_eq!(rescan(&mut db).removed, 0);
        assert_eq!(db.counts().unwrap(), (1, 0));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(dbdir);
    }

    #[test]
    fn folders_survive_a_new_layout() {
        let dir = temp("schema");
        let file = dir.join("library.db");
        Db::open_at(&file).unwrap().add_folder(&dir).unwrap();
        Connection::open(&file)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99")
            .unwrap();
        assert_eq!(Db::open_at(&file).unwrap().folders().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Speed with a big library: `cargo test --release big_library -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn big_library() {
        let dir = temp("big");
        let mut db = Db::open_at(&dir.join("library.db")).unwrap();
        let tx = db.0.transaction().unwrap();
        for i in 0..40_000u32 {
            let e = Entry {
                path: format!(
                    "/m/Artist {}/Album {}/{:02} Song {i}.flac",
                    i / 120,
                    i / 12,
                    i % 12 + 1
                ),
                dir: format!("/m/Artist {}/Album {}", i / 120, i / 12),
                title: format!("Song number {i}"),
                artist: format!("Artist {}", i / 120),
                album: format!("Album {}", i / 12),
                genre: "Rock; Pop".into(),
                track: i % 12 + 1,
                duration: 200.0,
                ..Default::default()
            };
            put(&tx, &e, 1, 1, i64::from(i)).unwrap();
        }
        tx.commit().unwrap();
        let time = |what: &str, f: &dyn Fn() -> usize| {
            let t = std::time::Instant::now();
            let n = f();
            println!("{what}: {n} in {:?}", t.elapsed());
        };
        time("albums", &|| {
            db.albums(&Albums::All, Sort::Name).unwrap().len()
        });
        time("artists", &|| db.artists(None).unwrap().len());
        time("songs", &|| {
            db.tracks(&Pick::Songs, Sort::Name).unwrap().len()
        });
        time("search", &|| {
            db.tracks(&Pick::Search("artist 7 song".into(), false), Sort::Name)
                .unwrap()
                .len()
        });
        time("one album", &|| {
            db.tracks(
                &Pick::Album("album 7\u{1f}/m/Artist 0/Album 7".into()),
                Sort::Name,
            )
            .unwrap()
            .len()
        });
        time("newest", &|| {
            db.tracks(&Pick::Newest(false, 18), Sort::Newest)
                .unwrap()
                .len()
        });
        time("folders", &|| db.dirs().unwrap().len());
        let _ = std::fs::remove_dir_all(dir);
    }
}
