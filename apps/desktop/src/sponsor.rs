//! Sponsors and other filler in YouTube videos, marked by viewers
//! (SponsorBlock, sponsor.ajay.app; its data is CC BY-NC-SA 4.0).
//!
//! Works for YouTube links and for downloaded videos that still say which
//! video they are: yt-dlp's "Title [dQw4w9WgXcQ].mkv" names, or the link it
//! keeps in the file's tags.
//!
//! Only the first four characters of the SHA-256 of the video's ID are
//! sent. The answer holds the segments of every video whose hash starts
//! that way, and the right one is picked out here, so the service doesn't
//! learn what is being watched.

use serde_json::Value;

use crate::artist::{begin_lookup, http_get, lookup_failed};

const API: &str = "https://sponsor.ajay.app/api/skipSegments";
/// The answer covers a few hundred videos at most.
const MAX_REPLY: u64 = 4 << 20;
/// Segments this close together are skipped in one go, seconds.
const JOIN: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sponsor,
    SelfPromo,
    Reminder,
    Intro,
    Ending,
    Recap,
    NonMusic,
}

impl Kind {
    const ALL: &[(&str, Kind)] = &[
        ("sponsor", Kind::Sponsor),
        ("selfpromo", Kind::SelfPromo),
        ("interaction", Kind::Reminder),
        ("intro", Kind::Intro),
        ("outro", Kind::Ending),
        ("preview", Kind::Recap),
        ("hook", Kind::Recap),
        ("music_offtopic", Kind::NonMusic),
    ];

    /// What the Skip button calls it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Sponsor => "sponsor",
            Kind::SelfPromo => "self-promotion",
            Kind::Reminder => "reminder",
            Kind::Intro => "intro",
            Kind::Ending => "ending",
            Kind::Recap => "preview",
            Kind::NonMusic => "part without music",
        }
    }

    /// Whether the chosen level (one of `settings::SPONSOR_KINDS`) covers it.
    pub fn in_level(self, level: &str) -> bool {
        match level {
            "sponsors" => self == Kind::Sponsor,
            "promotion" => matches!(self, Kind::Sponsor | Kind::SelfPromo | Kind::Reminder),
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub kind: Kind,
    /// Length of the video it was marked on (0: not known).
    video: f64,
}

/// One thing to skip: touching segments joined together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skip {
    pub start: f64,
    pub end: f64,
    pub kind: Kind,
}

// ---- which video ----

fn is_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The video's ID in a YouTube link (watch, youtu.be, shorts, live, embed,
/// YouTube Music, mpv's ytdl://).
pub fn id_in_link(link: &str) -> Option<&str> {
    let link = link.trim();
    let id_at = |s| {
        let s: &str = s;
        let end = s.find(['?', '&', '#', '/']).unwrap_or(s.len());
        Some(&s[..end]).filter(|id| is_id(id))
    };
    if let Some(rest) = link.strip_prefix("ytdl://")
        && let Some(id) = id_at(rest)
    {
        return Some(id);
    }
    let rest = link.strip_prefix("ytdl://").unwrap_or(link);
    let (_, rest) = rest.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next()?.to_ascii_lowercase();
    let of = |domain: &str| {
        host == domain || host.strip_suffix(domain).is_some_and(|h| h.ends_with('.'))
    };
    if of("youtu.be") {
        return id_at(path);
    }
    if !of("youtube.com") && !of("youtube-nocookie.com") {
        return None;
    }
    for start in ["shorts/", "live/", "embed/", "v/"] {
        if let Some(rest) = path.strip_prefix(start) {
            return id_at(rest);
        }
    }
    let query = path
        .strip_prefix("watch")?
        .trim_start_matches('/')
        .strip_prefix('?')?;
    let query = query.split('#').next()?;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("v="))
        .filter(|id| is_id(id))
}

/// The ID at the end of a downloaded file's name: "Title [dQw4w9WgXcQ].mkv".
pub fn id_in_name(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next()?;
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let (_, id) = stem.strip_suffix(']')?.rsplit_once('[')?;
    Some(id).filter(|id| is_id(id))
}

// ---- asking ----

/// The first characters of the hash of the ID: all that is sent.
fn hash_prefix(id: &str) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(id.as_bytes());
    format!("{:02x}{:02x}", h[0], h[1])
}

/// The segments marked in video `id`. Blocking. Err: couldn't ask (offline).
pub fn fetch(id: &str) -> Result<Vec<Segment>, ()> {
    let names: Vec<String> = Kind::ALL
        .iter()
        .map(|(n, _)| format!("%22{n}%22"))
        .collect();
    let url = format!(
        "{API}/{}?categories=%5B{}%5D&actionTypes=%5B%22skip%22%5D",
        hash_prefix(id),
        names.join(",")
    );
    begin_lookup();
    match http_get(&url, MAX_REPLY) {
        Some(body) => Ok(parse(&body, id)),
        None if lookup_failed() => Err(()),
        None => Ok(Vec::new()),
    }
}

fn parse(body: &[u8], id: &str) -> Vec<Segment> {
    let Ok(json) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let mut list: Vec<Segment> = json
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["videoID"].as_str() == Some(id))
        .flat_map(|v| v["segments"].as_array().into_iter().flatten())
        .filter(|s| s["actionType"].as_str().is_none_or(|a| a == "skip"))
        .filter_map(|s| {
            let name = s["category"].as_str()?;
            let kind = Kind::ALL.iter().find(|(n, _)| *n == name)?.1;
            let (start, end) = (s["segment"][0].as_f64()?, s["segment"][1].as_f64()?);
            (start >= 0.0 && end > start).then(|| Segment {
                start,
                end,
                kind,
                video: s["videoDuration"].as_f64().unwrap_or(0.0),
            })
        })
        .take(500)
        .collect();
    list.sort_by(|a, b| a.start.total_cmp(&b.start));
    list
}

// ---- what to skip ----

/// The parts to skip at `level` in a video lasting `duration`, in order.
/// Segments marked on another cut of the video (its length differs, as in
/// yt-dlp's check) are left out; touching ones are joined under the first
/// one's name.
pub fn skips(segments: &[Segment], level: &str, duration: f64) -> Vec<Skip> {
    let mut out: Vec<Skip> = Vec::new();
    for s in segments.iter().filter(|s| s.kind.in_level(level)) {
        let start = if s.start <= 1.0 { 0.0 } else { s.start };
        let end = if duration - s.end <= 1.0 {
            duration
        } else {
            s.end
        };
        if end <= start {
            continue;
        }
        let diff = if s.video > 0.0 {
            (duration - s.video).abs()
        } else {
            0.0
        };
        if !(diff < 1.0 || (diff < 5.0 && diff / (end - start) < 0.05)) {
            continue;
        }
        match out.last_mut() {
            Some(last) if start <= last.end + JOIN => last.end = last.end.max(end),
            _ => out.push(Skip {
                start,
                end,
                kind: s.kind,
            }),
        }
    }
    out
}

/// The part being played at `pos`, if any. Its last moments don't count:
/// there is nothing left to skip.
pub fn at(skips: &[Skip], pos: f64) -> Option<usize> {
    skips
        .iter()
        .position(|s| pos >= s.start - 0.2 && pos < s.end - 0.5)
}

/// Whether playback went from `last` to `pos` by just playing, at `speed`
/// (rather than by a seek), between two looks a quarter second apart.
pub fn flowed(last: f64, pos: f64, speed: f64) -> bool {
    let d = pos - last;
    d > 0.0 && d < speed.max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "dQw4w9WgXcQ";

    #[test]
    fn ids_in_links() {
        for link in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "http://youtube.com/watch?list=PL1&v=dQw4w9WgXcQ&t=3s",
            "https://m.youtube.com/watch/?v=dQw4w9WgXcQ#t=1",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&si=x",
            "https://youtu.be/dQw4w9WgXcQ?t=4",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
            "ytdl://dQw4w9WgXcQ",
            "ytdl://https://youtu.be/dQw4w9WgXcQ",
        ] {
            assert_eq!(id_in_link(link), Some(ID), "{link}");
        }
        for link in [
            "https://www.youtube.com/playlist?list=PL1234567890",
            "https://www.youtube.com/@channel",
            "https://notyoutube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
            "https://vimeo.com/12345678901",
            "https://www.youtube.com/watch?v=short",
            "ytdl://ytsearch:song",
            "/home/me/video.mkv",
            "",
        ] {
            assert_eq!(id_in_link(link), None, "{link}");
        }
    }

    #[test]
    fn ids_in_names() {
        assert_eq!(id_in_name("/v/A song [live] [dQw4w9WgXcQ].mkv"), Some(ID));
        assert_eq!(id_in_name("A song [dQw4w9WgXcQ].f137.mp4"), None);
        assert_eq!(id_in_name("/v/Film [1080p].mkv"), None);
        assert_eq!(id_in_name("/v/été [éééééé].mkv"), None);
        assert_eq!(id_in_name("/v/film.mkv"), None);
    }

    /// SponsorBlock's own example of the hash.
    #[test]
    fn only_a_short_prefix_is_sent() {
        assert_eq!(hash_prefix(ID), "5f6b");
    }

    fn reply() -> Vec<u8> {
        br#"[{"videoID":"otherVideo1","segments":[{"category":"sponsor","actionType":"skip","segment":[1,2],"videoDuration":60}]},
            {"videoID":"dQw4w9WgXcQ","segments":[
              {"category":"selfpromo","actionType":"skip","segment":[50.3,70],"videoDuration":600.2},
              {"category":"sponsor","actionType":"skip","segment":[20,50],"videoDuration":600},
              {"category":"sponsor","actionType":"mute","segment":[80,90],"videoDuration":600},
              {"category":"filler","actionType":"skip","segment":[100,110],"videoDuration":600},
              {"category":"intro","actionType":"skip","segment":[0.4,9],"videoDuration":0},
              {"category":"sponsor","actionType":"skip","segment":[300,290],"videoDuration":600},
              {"category":"sponsor","actionType":"skip","segment":[200,230],"videoDuration":640},
              {"category":"outro","actionType":"skip","segment":[580,599.6],"videoDuration":600},
              {"category":"sponsor","actionType":"skip","segment":["x"]}]}]"#
            .to_vec()
    }

    #[test]
    fn picks_the_right_video_and_segments() {
        let s = parse(&reply(), ID);
        assert_eq!(s.len(), 5);
        // Sponsors only; the one from a longer cut is left out.
        assert_eq!(
            skips(&s, "sponsors", 600.0),
            vec![Skip {
                start: 20.0,
                end: 50.0,
                kind: Kind::Sponsor
            }]
        );
        // The self-promotion right after is skipped along with it.
        assert_eq!(
            skips(&s, "promotion", 600.0),
            vec![Skip {
                start: 20.0,
                end: 70.0,
                kind: Kind::Sponsor
            }]
        );
        let all = skips(&s, "all", 600.0);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].start, all[0].kind), (0.0, Kind::Intro));
        assert_eq!((all[2].end, all[2].kind), (600.0, Kind::Ending));
        // A different video length: nothing fits but what has no length.
        assert_eq!(skips(&s, "all", 500.0).len(), 1);
    }

    #[test]
    fn bad_replies_are_no_segments() {
        for b in [
            &b""[..],
            b"Not Found",
            b"{}",
            b"[]",
            b"[1,2]",
            br#"[{"videoID":"dQw4w9WgXcQ"}]"#,
        ] {
            assert!(parse(b, ID).is_empty());
        }
    }

    #[test]
    fn where_to_skip() {
        let s = [Skip {
            start: 20.0,
            end: 50.0,
            kind: Kind::Sponsor,
        }];
        assert_eq!(at(&s, 19.0), None);
        assert_eq!(at(&s, 19.9), Some(0));
        assert_eq!(at(&s, 49.0), Some(0));
        assert_eq!(at(&s, 49.8), None);
        assert!(flowed(10.0, 10.25, 1.0) && flowed(10.0, 10.5, 2.0));
        assert!(!flowed(10.0, 10.0, 1.0) && !flowed(10.0, 14.0, 1.0) && !flowed(10.0, 9.0, 1.0));
    }

    /// Online: run with `--ignored`.
    #[test]
    #[ignore]
    fn asks_sponsorblock() {
        assert!(
            fetch("ByhVrrP1QkQ")
                .unwrap()
                .iter()
                .any(|s| s.kind == Kind::Sponsor)
        );
        assert!(fetch("00000000000").unwrap().is_empty());
    }
}
