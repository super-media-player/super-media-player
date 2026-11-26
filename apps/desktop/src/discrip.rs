//! Rip a DVD or Blu-ray to video files, named so libraries recognise them.
//!
//! MakeMKV copies the titles (lossless MKV with every audio track, subtitle
//! and chapter, see `ripper.rs`); this module works out what they are:
//!
//! - a series disc ("Reacher: Season Three Disc 2", "FRIENDS_S01_D2"): the
//!   show, season and disc come from the disc's name, the episodes are the
//!   titles of about the same length (a "play all" title, which is the
//!   others joined, and short extras are left out), and their names come
//!   from TVmaze (free, no account; only the show's name is sent). Disc 2
//!   with four episodes holds episodes 5 to 8, unless the user moves them.
//!   Files: `Videos/Show/Season 03/Show - S03E05 - Name.mkv`, the layout
//!   Jellyfin, Plex, Kodi and the player's own library know;
//! - a film: the longest title, `Videos/Film/Film.mkv`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ripper::{DiscInfo, Title};

/// What the disc's name says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Label {
    /// The show or film, in readable form ("Reacher").
    pub name: String,
    pub season: Option<u32>,
    pub disc: Option<u32>,
}

/// "one" … "twenty", "first" … "tenth".
fn number_word(w: &str) -> Option<u32> {
    const WORDS: [&str; 20] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
        "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen", "twenty",
    ];
    const ORDINALS: [&str; 10] =
        ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth"];
    let w = w.to_lowercase();
    if let Ok(n) = w.parse::<u32>() {
        return Some(n);
    }
    WORDS
        .iter()
        .position(|x| *x == w)
        .or_else(|| ORDINALS.iter().position(|x| *x == w))
        .map(|i| i as u32 + 1)
}

/// "LEAGUE_OF_GODS" → "League Of Gods"; mixed case is kept as it is.
fn readable(s: &str) -> String {
    let s = s.replace(['_', '.'], " ");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim_matches(|c: char| c == '-' || c == ':' || c == ',' || c.is_whitespace()).to_owned();
    if s.chars().any(char::is_lowercase) {
        return s;
    }
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Read the show, season and disc from a disc's name.
pub fn parse_label(label: &str) -> Label {
    // MakeMKV tells discs with the same name apart with "#F09C" at the end.
    let label = match label.rsplit_once('#') {
        Some((head, tag)) if tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit()) => head,
        _ => label,
    };
    let spaced = label.replace(['_', '.'], " ");
    let words: Vec<&str> = spaced.split(|c: char| c.is_whitespace() || c == '-' || c == ',' || c == ':').filter(|w| !w.is_empty()).collect();
    let mut season = None;
    let mut disc = None;
    // Where the name ends: at the first word that is about seasons or discs.
    let mut name_end = words.len();
    let mut i = 0;
    while i < words.len() {
        let w = words[i].to_lowercase();
        let next = words.get(i + 1).copied().and_then(number_word);
        let mut used = false;
        if matches!(w.as_str(), "season" | "series" | "saison" | "staffel" | "temporada") && next.is_some() {
            season = next;
            used = true;
            i += 1;
        } else if matches!(w.as_str(), "disc" | "disk" | "dvd" | "bd" | "cd") && next.is_some() {
            disc = next;
            used = true;
            i += 1;
        } else if let Some((s, d)) = code(&w) {
            if s.is_some() {
                season = s;
            }
            if d.is_some() {
                disc = d;
            }
            used = s.is_some() || d.is_some();
        } else if (w.ends_with("st") || w.ends_with("nd") || w.ends_with("rd") || w.ends_with("th"))
            && words.get(i + 1).is_some_and(|n| n.eq_ignore_ascii_case("season"))
            && let Some(n) = number_word(&w).or_else(|| w[..w.len() - 2].parse().ok())
        {
            season = Some(n);
            used = true;
            i += 1;
        }
        if used && name_end == words.len() {
            name_end = words[..i].len().min(name_end);
            // The word that started it ("Season", "S03D2", "Third").
            name_end = name_end.saturating_sub(usize::from(!matches!(code(&w), Some((Some(_), _)) | Some((_, Some(_))))));
        }
        i += 1;
    }
    let name_words: Vec<&str> = words[..name_end.min(words.len())].to_vec();
    // Keep the original punctuation of the name part ("Grey's Anatomy").
    let name = if name_words.is_empty() { readable(label) } else { readable(&name_words.join(" ")) };
    Label { name, season, disc }
}

/// What the disc's name and its volume label say together: the name's
/// spelling, the season and disc from whichever has them.
pub fn disc_label(info: &DiscInfo) -> Label {
    let name = parse_label(if info.name.is_empty() { &info.volume } else { &info.name });
    let volume = parse_label(&info.volume);
    Label { name: name.name, season: name.season.or(volume.season), disc: name.disc.or(volume.disc) }
}

/// "s03", "s3d2", "s01e01"-free disc codes: (season, disc).
fn code(w: &str) -> Option<(Option<u32>, Option<u32>)> {
    let w = w.to_lowercase();
    let digits = |s: &str| -> Option<(u32, usize)> {
        let n: String = s.chars().take_while(char::is_ascii_digit).collect();
        (!n.is_empty() && n.len() <= 2).then(|| (n.parse().unwrap_or(0), n.len()))
    };
    let mut season = None;
    let mut disc = None;
    let mut rest = w.as_str();
    if let Some(r) = rest.strip_prefix('s')
        && let Some((n, len)) = digits(r)
    {
        season = Some(n);
        rest = &r[len..];
    }
    if let Some(r) = rest.strip_prefix("disc").or_else(|| rest.strip_prefix('d'))
        && let Some((n, len)) = digits(r)
    {
        disc = Some(n);
        rest = &r[len..];
    }
    (rest.is_empty() && (season.is_some() || disc.is_some())).then_some((season, disc))
}

/// The titles that are episodes, in disc order: about the same length, at
/// least 15 minutes, not a "play all" title made of the others. Empty for
/// a film disc (one long title stands out).
pub fn episode_titles(info: &DiscInfo) -> Vec<u32> {
    let long: Vec<&Title> = info.titles.iter().filter(|t| t.seconds >= 15 * 60).collect();
    // "Play all": as long as two or more of the others together (2 %).
    let play_all = |t: &Title| {
        let others: u64 = long.iter().filter(|o| o.id != t.id && o.seconds < t.seconds).map(|o| o.seconds).sum();
        let count = long.iter().filter(|o| o.id != t.id && o.seconds < t.seconds).count();
        count >= 2 && (others as f64 - t.seconds as f64).abs() <= t.seconds as f64 * 0.02
    };
    let mut eps: Vec<&Title> = long.iter().copied().filter(|t| !play_all(t)).collect();
    if eps.len() < 2 {
        return Vec::new();
    }
    let mut lengths: Vec<u64> = eps.iter().map(|t| t.seconds).collect();
    lengths.sort_unstable();
    let median = lengths[lengths.len() / 2] as f64;
    eps.retain(|t| (t.seconds as f64) >= median * 0.6 && (t.seconds as f64) <= median * 1.5);
    // The same episode twice (another angle or language cut): keep the first.
    let mut seen: Vec<(u64, u64)> = Vec::new();
    eps.retain(|t| {
        let key = (t.seconds, t.bytes / 1_000_000);
        let dup = seen.iter().any(|s| s.0 == key.0 && s.1.abs_diff(key.1) <= 1);
        seen.push(key);
        !dup
    });
    if eps.len() < 2 {
        return Vec::new();
    }
    eps.iter().map(|t| t.id).collect()
}

/// One episode of a show, from TVmaze.
#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
    pub season: u32,
    pub number: u32,
    pub name: String,
    /// Minutes, as listed (rounded).
    pub runtime: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Show {
    pub name: String,
    pub year: Option<u32>,
    pub episodes: Vec<Episode>,
}

impl Show {
    pub fn season(&self, season: u32) -> Vec<&Episode> {
        self.episodes.iter().filter(|e| e.season == season).collect()
    }
}

fn cache_file(query: &str) -> Option<PathBuf> {
    let key: String = query.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    crate::settings::cache_dir().map(|d| d.join("tvmaze").join(format!("{key}.json")))
}

/// Look `name` up on TVmaze (cached for a week). Blocks for up to a few
/// seconds; run it off the UI thread.
pub fn lookup(name: &str) -> Result<Option<Show>, String> {
    let cached = cache_file(name);
    let fresh = |f: &Path| {
        std::fs::metadata(f)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(7 * 24 * 3600))
    };
    if let Some(f) = cached.as_deref().filter(|f| fresh(f))
        && let Ok(text) = std::fs::read_to_string(f)
    {
        return Ok(serde_json::from_str(&text).ok().as_ref().and_then(parse_show));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).build().into();
    let url = "https://api.tvmaze.com/singlesearch/shows";
    let resp = agent
        .get(url)
        .query("q", name)
        .query("embed", "episodes")
        .header("User-Agent", concat!("SuperMediaPlayer/", env!("CARGO_PKG_VERSION")))
        .call();
    let mut resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(format!("TVmaze: {e}")),
    };
    let body = resp.body_mut().with_config().limit(8 << 20).read_to_string().map_err(|e| format!("TVmaze: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("TVmaze: {e}"))?;
    if let Some(f) = &cached {
        let _ = std::fs::create_dir_all(f.parent().unwrap_or(Path::new(".")));
        let _ = std::fs::write(f, &body);
    }
    Ok(parse_show(&json))
}

fn parse_show(v: &serde_json::Value) -> Option<Show> {
    let name = v["name"].as_str()?.to_owned();
    let year = v["premiered"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok());
    let episodes = v["_embedded"]["episodes"]
        .as_array()
        .map(|eps| {
            eps.iter()
                .filter_map(|e| {
                    Some(Episode {
                        season: e["season"].as_u64()? as u32,
                        number: e["number"].as_u64()? as u32,
                        name: e["name"].as_str().unwrap_or_default().to_owned(),
                        runtime: e["runtime"].as_u64().map(|r| r as u32),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Show { name, year, episodes })
}

/// A file or folder name: no slashes or characters some systems refuse.
pub fn safe_name(s: &str) -> String {
    let clean: String = s
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() || ":*?\"<>|".contains(c) { ' ' } else { c })
        .take(150)
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean = clean.trim_start_matches('.').trim().to_owned();
    if clean.is_empty() { "Disc".into() } else { clean }
}

/// The plan for one title.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub title: Title,
    pub rip: bool,
    /// The file name, without the folder.
    pub file: String,
    /// "S03E05" for episodes.
    pub episode: Option<(u32, u32)>,
}

/// The whole plan: where the files go and what each title becomes.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub series: bool,
    /// Show or film name as used in the names (editable).
    pub name: String,
    pub season: u32,
    /// The episode number of the first episode title on this disc.
    pub first: u32,
    pub show: Option<Show>,
    pub items: Vec<Item>,
}

/// The first episode on disc `disc` of a season, when each disc holds `per_disc`.
pub fn first_episode(disc: Option<u32>, per_disc: usize) -> u32 {
    disc.unwrap_or(1).saturating_sub(1) * per_disc as u32 + 1
}

impl Plan {
    /// Work out the plan from MakeMKV's titles and the disc's name. `show`:
    /// the TVmaze answer for a series disc, if looked up.
    pub fn new(info: &DiscInfo, label: &Label, show: Option<Show>) -> Plan {
        let episodes = episode_titles(info);
        let series = !episodes.is_empty() || label.season.is_some();
        let name = show.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| label.name.clone());
        let season = label.season.unwrap_or(1);
        let first = first_episode(label.disc, episodes.len().max(1));
        let main = info.main_title().map(|t| t.id);
        let items = info
            .titles
            .iter()
            .map(|t| Item {
                title: t.clone(),
                rip: if series && !episodes.is_empty() { episodes.contains(&t.id) } else { Some(t.id) == main },
                file: String::new(),
                episode: None,
            })
            .collect();
        let mut plan = Plan { series: series && !episodes.is_empty(), name, season, first, show, items };
        plan.rename();
        plan
    }

    /// Name every title again (after the name, season or first episode changed).
    pub fn rename(&mut self) {
        let episodes: Vec<u32> = if self.series {
            episode_titles(&DiscInfo { titles: self.items.iter().map(|i| i.title.clone()).collect(), ..Default::default() })
        } else {
            Vec::new()
        };
        let name = safe_name(&self.name);
        let season_eps: Vec<Episode> =
            self.show.as_ref().map(|s| s.season(self.season).into_iter().cloned().collect()).unwrap_or_default();
        let mut extra = 0;
        for item in &mut self.items {
            item.episode = None;
            if let Some(pos) = episodes.iter().position(|&id| id == item.title.id) {
                let number = self.first + pos as u32;
                let ep_name = season_eps.iter().find(|e| e.number == number).map(|e| e.name.trim()).filter(|n| !n.is_empty());
                item.episode = Some((self.season, number));
                item.file = match ep_name {
                    Some(n) => format!("{name} - S{:02}E{:02} - {}.mkv", self.season, number, safe_name(n)),
                    None => format!("{name} - S{:02}E{:02}.mkv", self.season, number),
                };
            } else if !self.series && item.rip {
                item.file = format!("{name}.mkv");
            } else {
                extra += 1;
                item.file = if self.series {
                    format!("{name} - S{:02} Extra {extra:02}.mkv", self.season)
                } else {
                    format!("{name} - Extra {extra:02}.mkv")
                };
            }
        }
    }

    /// The folder the files go in, under `videos`.
    pub fn folder(&self, videos: &Path) -> PathBuf {
        let name = safe_name(&self.name);
        if self.series {
            videos.join(&name).join(format!("Season {:02}", self.season))
        } else {
            videos.join(name)
        }
    }

    /// How this disc's episodes fit TVmaze's list: "" when unknown, else a
    /// line for the panel.
    pub fn summary(&self) -> String {
        let eps: Vec<u32> = self.items.iter().filter_map(|i| i.episode.map(|e| e.1)).collect();
        let (Some(first), Some(last)) = (eps.first(), eps.last()) else {
            return String::new();
        };
        match &self.show {
            Some(s) => {
                let count = s.season(self.season).len();
                let year = s.year.map(|y| format!(" ({y})")).unwrap_or_default();
                if count == 0 {
                    format!("{}{year} on TVmaze has no season {}: check the season.", s.name, self.season)
                } else if *last as usize > count {
                    format!(
                        "{}{year}: season {} has {count} episodes, but these would be {first} to {last}: check the first episode.",
                        s.name, self.season
                    )
                } else {
                    format!("{}{year}, season {}: episodes {first} to {last} of {count}, names from TVmaze.", s.name, self.season)
                }
            }
            None => format!("Episodes {first} to {last} of season {}.", self.season),
        }
    }
}

/// The user's Videos folder (XDG), else ~/Videos.
pub fn videos_dir() -> PathBuf {
    crate::clip::user_dir("XDG_VIDEOS_DIR", "Videos")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u32, secs: u64, mb: u64) -> Title {
        Title { id, name: String::new(), seconds: secs, bytes: mb * 1_000_000, chapters: 8, output: format!("t{id:02}.mkv") }
    }

    #[test]
    fn labels() {
        let l = parse_label("Reacher: Season Three Disc 2");
        assert_eq!(l, Label { name: "Reacher".into(), season: Some(3), disc: Some(2) });
        assert_eq!(parse_label("FRIENDS_S01_D2"), Label { name: "Friends".into(), season: Some(1), disc: Some(2) });
        assert_eq!(parse_label("BREAKING_BAD_SEASON_1_DISC_3"), Label { name: "Breaking Bad".into(), season: Some(1), disc: Some(3) });
        assert_eq!(parse_label("THE_OFFICE_S2D1"), Label { name: "The Office".into(), season: Some(2), disc: Some(1) });
        assert_eq!(parse_label("The Wire - Series 4 - Disc 1"), Label { name: "The Wire".into(), season: Some(4), disc: Some(1) });
        assert_eq!(parse_label("Grey's Anatomy Third Season Disc 4"), Label { name: "Grey's Anatomy".into(), season: Some(3), disc: Some(4) });
        assert_eq!(parse_label("Reacher_ Season Three Disc 2#F09C"), Label { name: "Reacher".into(), season: Some(3), disc: Some(2) });
        assert_eq!(parse_label("LEAGUE_OF_GODS"), Label { name: "League Of Gods".into(), season: None, disc: None });
        assert_eq!(parse_label("Dune Part Two"), Label { name: "Dune Part Two".into(), season: None, disc: None });
    }

    #[test]
    fn name_and_volume_together() {
        let info = DiscInfo { name: "Reacher: Season Three".into(), volume: "REACHER_S3_D2".into(), titles: Vec::new() };
        assert_eq!(disc_label(&info), Label { name: "Reacher".into(), season: Some(3), disc: Some(2) });
    }

    #[test]
    fn episodes_leave_play_all_and_extras_out() {
        // Four episodes, the four joined ("play all"), two extras.
        let info = DiscInfo {
            name: "x".into(),
            volume: String::new(),
            titles: vec![t(0, 3 * 3600 + 25 * 60, 7600), t(1, 3060, 1900), t(2, 3000, 1880), t(3, 3300, 2050), t(4, 2940, 2040), t(5, 300, 90), t(6, 1200, 300)],
        };
        assert_eq!(episode_titles(&info), vec![1, 2, 3, 4]);
        // A film with extras: no episodes.
        let film = DiscInfo { name: "x".into(), volume: String::new(), titles: vec![t(0, 7200, 6000), t(1, 600, 200), t(2, 1000, 300)] };
        assert!(episode_titles(&film).is_empty());
    }

    fn show() -> Show {
        Show {
            name: "Reacher".into(),
            year: Some(2022),
            episodes: (1..=8)
                .map(|n| Episode { season: 3, number: n, name: format!("Episode/{n}"), runtime: Some(50) })
                .chain([Episode { season: 1, number: 1, name: "Welcome".into(), runtime: Some(50) }])
                .collect(),
        }
    }

    #[test]
    fn plans_names_and_folders() {
        let info = DiscInfo { name: "Reacher".into(), volume: String::new(), titles: vec![t(0, 3060, 1900), t(1, 3000, 1880), t(2, 3300, 2050), t(3, 2940, 2040), t(4, 300, 90)] };
        let label = parse_label("Reacher: Season Three Disc 2");
        let mut plan = Plan::new(&info, &label, Some(show()));
        assert!(plan.series);
        assert_eq!(plan.first, 5);
        let files: Vec<&str> = plan.items.iter().map(|i| i.file.as_str()).collect();
        assert_eq!(files, [
            "Reacher - S03E05 - Episode 5.mkv",
            "Reacher - S03E06 - Episode 6.mkv",
            "Reacher - S03E07 - Episode 7.mkv",
            "Reacher - S03E08 - Episode 8.mkv",
            "Reacher - S03 Extra 01.mkv",
        ]);
        assert_eq!(plan.items.iter().filter(|i| i.rip).count(), 4);
        assert_eq!(plan.folder(Path::new("/v")), Path::new("/v/Reacher/Season 03"));
        assert!(plan.summary().contains("episodes 5 to 8 of 8"), "{}", plan.summary());
        // One too far: says so.
        plan.first = 6;
        plan.rename();
        assert_eq!(plan.items[3].file, "Reacher - S03E09.mkv");
        assert!(plan.summary().contains("check the first episode"));
    }

    #[test]
    fn film_plan() {
        let info = DiscInfo { name: "LEAGUE_OF_GODS".into(), volume: String::new(), titles: vec![t(0, 6600, 30000), t(1, 600, 200)] };
        let plan = Plan::new(&info, &parse_label("LEAGUE_OF_GODS"), None);
        assert!(!plan.series);
        assert_eq!(plan.items[0].file, "League Of Gods.mkv");
        assert!(plan.items[0].rip && !plan.items[1].rip);
        assert_eq!(plan.folder(Path::new("/v")), Path::new("/v/League Of Gods"));
    }

    #[test]
    fn tvmaze_answer() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"name":"Reacher","premiered":"2022-02-04","_embedded":{"episodes":[
            {"season":3,"number":5,"name":"Smackdown","runtime":51},{"season":3,"number":null,"name":"Special"}]}}"#,
        )
        .unwrap();
        let s = parse_show(&v).unwrap();
        assert_eq!(s.year, Some(2022));
        assert_eq!(s.episodes, vec![Episode { season: 3, number: 5, name: "Smackdown".into(), runtime: Some(51) }]);
    }

    /// `cargo test -p super-media-player discrip_this_machine -- --ignored --nocapture`
    /// asks MakeMKV about the disc in SMP_TEST_DISC (default /dev/sr1) and
    /// TVmaze about its show, and prints the plan.
    #[test]
    #[ignore = "uses this machine's MakeMKV, drive and the network"]
    fn discrip_this_machine() {
        let device = std::env::var("SMP_TEST_DISC").unwrap_or("/dev/sr1".into());
        let tool = crate::ripper::tool().expect("no MakeMKV");
        let info = crate::ripper::disc_info(&tool, &device).expect("MakeMKV");
        for t in &info.titles {
            println!("{t:?}");
        }
        let label = disc_label(&info);
        println!("{:?} / {:?} → {label:?}", info.name, info.volume);
        let show = lookup(&label.name).unwrap();
        let plan = Plan::new(&info, &label, show);
        for i in &plan.items {
            println!("[{}] {} ({})", if i.rip { "x" } else { " " }, i.file, crate::ripper::format_duration(i.title.seconds));
        }
        println!("{}\n{}", plan.summary(), plan.folder(&videos_dir()).display());
    }
}
