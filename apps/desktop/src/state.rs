//! Applies player events to the UI. Runs on the UI thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use player_core::{
    EndReason, Player, PlayerEvent, PlaylistEntry, Property, Track, TrackKind, display_name_for,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::settings::Settings;
use crate::{AppWindow, PlaylistItem, TrackItem, artist, musicinfo, visual};

pub struct UiState {
    tracks: Vec<Track>,
    metadata: Vec<(String, String)>,
    hwdec: Option<String>,
    buffering: Option<i64>,
    paused_for_cache: bool,
    /// Between "start-file" and the first decoded frame of a new file.
    opening: bool,
    loop_file: bool,
    loop_playlist: bool,
    /// Why the file being opened will fail, when mpv's log said so before
    /// "end-file" (its own error there is only "loading failed").
    open_failure_hint: Option<String>,
    /// Hint for the next file to start (e.g. a disc missing its decryption
    /// library), shown only if that file fails to open.
    next_file_hint: Option<String>,
    /// Shared with the video renderer: true while there is a picture to draw
    /// (a video track, or a visualizer for music).
    pub video_active: Rc<Cell<bool>>,
    player: Rc<Player>,
    settings: Rc<RefCell<Settings>>,
    /// Path or URL of the current file.
    path: Option<String>,
    /// The last file mpv reported, kept after it ends (to retry a link).
    last_path: Option<String>,
    /// The visualizer graph running for the current file, if any.
    visual_graph: Option<String>,
    /// Artist whose picture is shown (or being looked up).
    artist_shown: Option<String>,
    /// Song whose cover, lyrics and artist summary are shown (or being looked up).
    info_shown: Option<String>,
    bitrate: Option<f64>,
    samplerate: Option<i64>,
    /// yt-dlp was reported missing while opening the current file.
    pub ytdl_missing: bool,
    /// A disc that failed to open was tried once more (see EndFile).
    disc_retried: bool,
}

impl UiState {
    pub fn new(
        video_active: Rc<Cell<bool>>,
        player: Rc<Player>,
        settings: Rc<RefCell<Settings>>,
    ) -> Self {
        Self {
            tracks: Vec::new(),
            metadata: Vec::new(),
            hwdec: None,
            buffering: None,
            paused_for_cache: false,
            opening: false,
            loop_file: false,
            loop_playlist: false,
            open_failure_hint: None,
            next_file_hint: None,
            video_active,
            player,
            settings,
            path: None,
            last_path: None,
            visual_graph: None,
            artist_shown: None,
            info_shown: None,
            bitrate: None,
            samplerate: None,
            ytdl_missing: false,
            disc_retried: false,
        }
    }

    pub fn set_next_file_hint(&mut self, hint: Option<String>) {
        self.next_file_hint = hint;
    }

    pub fn repeat_mode(&self) -> i32 {
        if self.loop_file {
            2
        } else if self.loop_playlist {
            1
        } else {
            0
        }
    }

    pub fn apply(&mut self, ui: &AppWindow, event: PlayerEvent) {
        match event {
            PlayerEvent::Property(p) => self.apply_property(ui, p),
            PlayerEvent::StartFile => {
                self.open_failure_hint = self.next_file_hint.take();
                crate::dvdmenu::file_started(ui, self.player.get_string("path").as_deref());
                self.ytdl_missing = false;
                crate::pace::opened(ui, &self.player);
                crate::autocrop::file_started();
                // mpv drops file-local options (our visualizer) per file.
                self.visual_graph = None;
                self.set_opening(ui, true);
            }
            PlayerEvent::Seek => {
                crate::silence::note_seek();
                crate::cast::player_sought(false)
            }
            // A script asked mpv to quit: without the engine there is no player.
            PlayerEvent::Shutdown => {
                let _ = slint::quit_event_loop();
            }
            PlayerEvent::FileLoaded => crate::dvdmenu::file_loaded(&self.player),
            PlayerEvent::PlaybackRestart => {
                crate::cast::player_sought(true);
                self.disc_retried = false;
                self.set_opening(ui, false);
            }
            PlayerEvent::EndFile { reason, error } => {
                self.set_opening(ui, false);
                let hint = self.open_failure_hint.take();
                if crate::dvdmenu::file_ended(ui, reason) {
                    return;
                }
                if reason == EndReason::Error {
                    if self.ytdl_missing {
                        // main.rs offers to install yt-dlp and retries. Deferred:
                        // its dialog must not run while this state is borrowed.
                        let url = self.last_path.clone().unwrap_or_default();
                        let weak = ui.as_weak();
                        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.invoke_ytdlp_missing(url.into());
                            }
                        });
                        return;
                    }
                    // A Blu-ray or DVD whose decryption handshake with the
                    // drive failed reads as garbage ("unrecognized file
                    // format"). The second try looks at how a Blu-ray
                    // drive reads now (bluray.rs); if that fails too, an
                    // eject resets the drive.
                    let url = self.last_path.clone().unwrap_or_default();
                    let disc = url.starts_with("bd://") || url.starts_with("dvd://");
                    let unreadable = error.as_deref().is_some_and(|e| {
                        let e = e.to_lowercase();
                        e.contains("unrecognized") || e.contains("format") || e.contains("loading failed")
                    });
                    if disc && unreadable && hint.is_none() {
                        if !self.disc_retried {
                            self.disc_retried = true;
                            log::warn!("{url}: couldn't read the disc; trying once more");
                            ui.set_toast("Couldn't read the disc yet; trying again…".into());
                            let weak = ui.as_weak();
                            slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
                                if let Some(ui) = weak.upgrade() {
                                    crate::retry_disc(&ui, &url);
                                }
                            });
                            return;
                        }
                        self.disc_retried = false;
                        ui.set_toast(DISC_UNREADABLE.into());
                        return;
                    }
                    let message = match hint {
                        Some(hint) => hint,
                        None => format!("Could not play: {}", error.unwrap_or_default()),
                    };
                    ui.set_toast(message.into());
                }
            }
            PlayerEvent::Log {
                level,
                prefix,
                text,
            } => {
                match level.as_str() {
                    _ if is_noise(&prefix, &text) => log::debug!("[{prefix}] {text}"),
                    _ if self.playing_disc() && is_disc_timing_note(&prefix, &text) => {
                        log::debug!("[{prefix}] {text}")
                    }
                    "fatal" | "error" => log::error!("[{prefix}] {text}"),
                    _ => log::warn!("[{prefix}] {text}"),
                }
                crate::scripts::log_line(ui, &level, &prefix, &text);
                if let Some(hint) = disc_protection_hint(&prefix, &text) {
                    if prefix == "bd" {
                        log::error!("{}", DISC_PROTECTION_HELP);
                    }
                    // An Ultra HD disc's own explanation says more than AACS's.
                    if self.open_failure_hint.as_deref() != Some(crate::disc::UHD_HELP) {
                        self.open_failure_hint = Some(hint.to_owned());
                    }
                }
                if prefix.contains("ytdl") && text.contains("not found") {
                    self.ytdl_missing = true;
                }
            }
        }
    }

    fn apply_property(&mut self, ui: &AppWindow, p: Property) {
        match p {
            // While casting these are the device's (cast::tick).
            Property::TimePos(_) | Property::Pause(_) | Property::Volume(_) | Property::Mute(_)
                if crate::cast::active() => {}
            Property::TimePos(t) => {
                ui.set_position(t.unwrap_or(0.0) as f32);
                show_lyric_line(ui, t.unwrap_or(0.0));
            }
            // A menu has no length: it grows while it is read.
            Property::Duration(_) if crate::dvdmenu::menu_active() => ui.set_duration(0.0),
            Property::Duration(d) => ui.set_duration(d.unwrap_or(0.0) as f32),
            Property::Pause(p) => ui.set_paused(p),
            Property::Volume(v) => ui.set_volume(v as f32),
            Property::Mute(m) => ui.set_muted(m),
            Property::Speed(s) => ui.set_speed(s as f32),
            Property::MediaTitle(t) => ui.set_media_title(t.unwrap_or_default().into()),
            Property::Idle(idle) => {
                ui.set_idle(idle);
                if idle {
                    crate::dvdmenu::went_idle(ui);
                    self.set_opening(ui, false);
                    ui.set_position(0.0);
                    ui.set_duration(0.0);
                }
            }
            Property::Tracks(tracks) => {
                self.tracks = tracks;
                self.update_tracks(ui);
            }
            Property::Playlist(entries) => {
                ui.set_playlist(playlist_model(&entries));
                crate::playlists::refresh_filter(ui);
                if !entries.is_empty() {
                    crate::scripts::queued(ui);
                }
            }
            Property::PlaylistPos(_) => {}
            Property::Buffering(b) => {
                self.buffering = b;
                self.update_buffering(ui);
            }
            Property::PausedForCache(p) => {
                self.paused_for_cache = p;
                self.update_buffering(ui);
            }
            Property::HwDecoder(h) => {
                if h != self.hwdec {
                    log::info!(
                        "hardware decoder: {}",
                        h.as_deref().unwrap_or("none (software)")
                    );
                }
                self.hwdec = h;
                self.update_tech_info(ui);
            }
            Property::Metadata(m) => {
                self.metadata = m;
                ui.set_media_subtitle(self.subtitle_line().into());
                ui.set_media_artist(self.artist().unwrap_or_default().into());
                ui.set_media_album(self.tag("album").unwrap_or_default().into());
                self.update_artist_image(ui);
                self.update_music_info(ui);
            }
            Property::LoopFile(on) => {
                self.loop_file = on;
                ui.set_repeat_mode(self.repeat_mode());
            }
            Property::LoopPlaylist(on) => {
                self.loop_playlist = on;
                ui.set_repeat_mode(self.repeat_mode());
            }
            Property::Path(p) => {
                if p.is_some() {
                    self.last_path = p.clone();
                }
                self.path = p;
                self.update_artist_image(ui);
                self.update_music_info(ui);
            }
            Property::AudioBitrate(b) => {
                self.bitrate = b;
                self.update_audio_info(ui);
            }
            Property::Fullscreen(_) => {
                // A script's wish. Deferred (the callback isn't for while
                // this state is borrowed), and asked again then: the window
                // may have changed since.
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                    let Some(ui) = weak.upgrade() else { return };
                    crate::with_player(|p| {
                        let wish = p.get_string("fullscreen").is_some_and(|v| v == "yes");
                        if wish != ui.get_fullscreen() {
                            ui.invoke_toggle_fullscreen();
                        }
                    });
                });
            }
            Property::SampleRate(r) => {
                self.samplerate = r;
                self.update_audio_info(ui);
            }
        }
    }

    fn update_tracks(&mut self, ui: &AppWindow) {
        let has_video = self
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Video && t.selected && !t.albumart);
        ui.set_has_video(has_video);
        self.update_visual(ui);

        let items = |kind| -> ModelRc<TrackItem> {
            let v: Vec<TrackItem> = self
                .tracks
                .iter()
                .filter(|t| t.kind == kind)
                .map(|t| TrackItem {
                    id: t.id as i32,
                    label: t.label().into(),
                    selected: t.selected,
                })
                .collect();
            ModelRc::new(VecModel::from(v))
        };
        ui.set_audio_tracks(items(TrackKind::Audio));
        ui.set_subtitle_tracks(items(TrackKind::Subtitle));
        ui.set_subtitles_off(
            !self
                .tracks
                .iter()
                .any(|t| t.kind == TrackKind::Subtitle && t.selected),
        );
        self.update_tech_info(ui);
        ui.window().request_redraw();
    }

    /// Start, change or stop the music visualizer to match the settings. Only
    /// for files without a real video track; the graph is a file-local option,
    /// so mpv drops it by itself when the next file starts.
    pub fn update_visual(&mut self, ui: &AppWindow) {
        let has_video = ui.get_has_video();
        let audio = self
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio && t.selected);
        // While a graph runs, mpv reports the track it consumes as selected.
        let want = match audio {
            Some(a) if !has_video => visual::graph(&self.settings.borrow().visualizer, a.id),
            _ => None,
        };
        if want != self.visual_graph {
            let value = want.as_deref().unwrap_or("");
            if let Err(e) = self
                .player
                .set_string("file-local-options/lavfi-complex", value)
            {
                log::warn!("visualizer: {e}");
            } else {
                // Visuals fill their area (the compact view's is a wide strip);
                // file-local, so the next video keeps its aspect ratio.
                let keep = if want.is_some() { "no" } else { "yes" };
                let _ = self
                    .player
                    .set_string("file-local-options/keepaspect", keep);
                self.visual_graph = want;
            }
        }
        // MilkDrop draws in the GL context itself (milkdrop.rs), not through mpv.
        let milkdrop = audio.is_some()
            && !has_video
            && self.settings.borrow().visualizer == "milkdrop"
            && crate::milkdrop::available();
        crate::milkdrop::set_active(milkdrop);
        let visual = self.visual_graph.is_some() || milkdrop;
        ui.set_visual_active(visual);
        self.video_active.set(has_video || visual);
        ui.window().request_redraw();
    }

    /// A DVD or Blu-ray is playing (its menus and chapters jump in time).
    fn playing_disc(&self) -> bool {
        self.path
            .as_deref()
            .is_some_and(|p| {
                p.starts_with("dvd://")
                    || p.starts_with("dvdnav://")
                    || p.starts_with("bd://")
                    || crate::dvdmenu::is_menu_path(p)
            })
    }

    /// A non-empty tag of the current file's metadata.
    fn tag(&self, k: &str) -> Option<String> {
        self.metadata
            .iter()
            .find(|(key, v)| key.eq_ignore_ascii_case(k) && !v.trim().is_empty())
            .map(|(_, v)| v.trim().to_owned())
    }

    /// Look up the album cover, lyrics and artist summary (musicinfo.rs)
    /// on a worker thread when the song changes; music only.
    pub fn update_music_info(&mut self, ui: &AppWindow) {
        let path = self.path.clone().unwrap_or_default();
        let settings = self.settings.borrow();
        let q = musicinfo::Query {
            path: path.clone(),
            title: self.tag("title").unwrap_or_default(),
            artist: self.artist().unwrap_or_default(),
            album: self.tag("album").unwrap_or_default(),
            duration: f64::from(ui.get_duration()),
            online_info: settings.artist_image_online,
            online_lyrics: settings.lyrics_online,
        };
        drop(settings);
        let key = format!("{}\n{}\n{}\n{}", q.path, q.artist, q.title, q.album);
        if self.info_shown.as_deref() == Some(key.as_str()) {
            return;
        }
        self.info_shown = Some(key.clone());
        INFO_KEY.with_borrow_mut(|k| *k = Some(key.clone()));
        apply_music_info(ui, musicinfo::Info::default());
        if path.is_empty() || ui.get_has_video() {
            return;
        }
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let info = musicinfo::lookup(&q);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if INFO_KEY.with_borrow(|k| k.as_deref() == Some(key.as_str())) {
                    apply_music_info(&ui, info);
                }
            });
        });
    }

    /// Look the song up again (the online lookup settings changed).
    pub fn refresh_music_info(&mut self, ui: &AppWindow) {
        self.info_shown = None;
        self.update_music_info(ui);
    }

    fn artist(&self) -> Option<String> {
        let get = |k: &str| {
            self.metadata
                .iter()
                .find(|(key, v)| key == k && !v.trim().is_empty())
                .map(|(_, v)| v.trim().to_owned())
        };
        get("artist")
            .or_else(|| get("album_artist"))
            .or_else(|| get("albumartist"))
    }

    /// Show a picture of the artist: one next to the file, else (when turned
    /// on) one looked up online in a worker thread.
    pub fn update_artist_image(&mut self, ui: &AppWindow) {
        let settings = self.settings.borrow();
        let path = self.path.clone().unwrap_or_default();
        let artist = self.artist();
        let key = format!("{}\n{}", artist.as_deref().unwrap_or(""), path);
        if settings.artist_image == "off" || path.is_empty() {
            self.artist_shown = None;
            ui.set_has_artist_image(false);
            return;
        }
        if self.artist_shown.as_deref() == Some(key.as_str()) {
            return;
        }
        self.artist_shown = Some(key.clone());
        ui.set_has_artist_image(false);
        let online = settings.artist_image_online;
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let found = artist::find_local(&path).or_else(|| {
                artist
                    .filter(|_| online)
                    .and_then(|a| artist::fetch_online(&a))
            });
            let Some(file) = found else { return };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                UI_KEY.with_borrow(|current| {
                    if current.as_deref() != Some(key.as_str()) {
                        return; // the song changed meanwhile
                    }
                    match slint::Image::load_from_path(&file) {
                        Ok(img) => {
                            ui.set_artist_image(img);
                            ui.set_has_artist_image(true);
                        }
                        Err(_) => log::debug!("could not load {}", file.display()),
                    }
                });
            });
        });
        UI_KEY.with_borrow_mut(|k| *k = self.artist_shown.clone());
    }

    /// "320 kbps · 44.1 kHz" for the compact view.
    fn update_audio_info(&self, ui: &AppWindow) {
        let mut parts = Vec::new();
        if let Some(b) = self.bitrate.filter(|b| *b > 0.0) {
            parts.push(format!("{} kbps", (b / 1000.0).round()));
        }
        if let Some(r) = self.samplerate.filter(|r| *r > 0) {
            let khz = r as f64 / 1000.0;
            parts.push(if khz.fract() == 0.0 {
                format!("{khz} kHz")
            } else {
                format!("{khz:.1} kHz")
            });
        }
        ui.set_audio_info(parts.join(" · ").into());
    }

    /// Short codec summary for the top bar, e.g. "1920×1080 · h264 · vaapi".
    fn update_tech_info(&self, ui: &AppWindow) {
        let selected = |kind| self.tracks.iter().find(|t| t.kind == kind && t.selected);
        let mut parts = Vec::new();
        if let Some(v) = selected(TrackKind::Video).filter(|t| !t.albumart) {
            if let (Some(w), Some(h)) = (v.width, v.height) {
                parts.push(format!("{w}×{h}"));
            }
            parts.extend(v.codec.clone());
            parts.push(match &self.hwdec {
                Some(h) => format!("GPU: {h}"),
                None => "CPU decode".into(),
            });
        }
        if let Some(a) = selected(TrackKind::Audio) {
            parts.extend(a.codec.clone());
            parts.extend(a.channels.clone());
        }
        ui.set_tech_info(parts.join(" · ").into());
    }

    fn set_opening(&mut self, ui: &AppWindow, opening: bool) {
        let path = opening.then(|| self.player.get_string("path")).flatten();
        crate::scripts::opening(ui, path.as_deref());
        if self.opening != opening {
            self.opening = opening;
            self.update_buffering(ui);
        }
    }

    fn update_buffering(&self, ui: &AppWindow) {
        let text = match (self.paused_for_cache, self.buffering) {
            (true, Some(p)) => format!("Buffering… {p}%"),
            (true, None) => "Buffering…".into(),
            _ if self.opening => "Opening…".into(),
            _ => String::new(),
        };
        ui.set_buffering_text(text.into());
    }

    /// "Artist — Album" from the file's tags, or the stream's ICY title.
    fn subtitle_line(&self) -> String {
        let get = |k: &str| {
            self.metadata
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        let parts: Vec<&str> = [get("artist").or(get("album_artist")), get("album")]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            return parts.join(" — ");
        }
        get("icy-name").unwrap_or_default().to_owned()
    }
}

thread_local! {
    /// Which song the artist picture being loaded belongs to.
    static UI_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Which song the music info being looked up belongs to.
    static INFO_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The current song's lyrics, and the line shown.
    static LYRICS: RefCell<Option<musicinfo::Lyrics>> = const { RefCell::new(None) };
    static LYRIC_LINE: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Show what musicinfo found (or clear it, for `Info::default()`).
/// A disc that still can't be read after a second try.
const DISC_UNREADABLE: &str = "The disc couldn't be read: most often the drive's copy-protection check \
    failed. Eject the disc, put it back in and play it again (that resets the drive). If it keeps \
    happening, Tools → System check looks at the drive and the decryption libraries.";

fn apply_music_info(ui: &AppWindow, info: musicinfo::Info) {
    let cover = info
        .cover
        .as_deref()
        .and_then(|p| slint::Image::load_from_path(p).ok());
    ui.set_has_cover(cover.is_some());
    ui.set_cover_image(cover.unwrap_or_default());
    ui.set_media_year(info.year.map(|y| y.to_string()).unwrap_or_default().into());
    ui.set_media_genre(info.genre.into());
    ui.set_media_track(info.track.map(|t| t.to_string()).unwrap_or_default().into());
    ui.set_about_text(info.about.into());
    ui.set_about_source(info.about_source.into());
    ui.set_lyrics_text(info.lyrics.as_ref().map(|l| l.text()).unwrap_or_default().into());
    ui.set_lyrics_synced(info.lyrics.as_ref().is_some_and(|l| l.synced()));
    LYRICS.with_borrow_mut(|l| *l = info.lyrics);
    LYRIC_LINE.with(|l| l.set(None));
    ui.set_lyric_prev("".into());
    ui.set_lyric_current("".into());
    ui.set_lyric_next("".into());
    show_lyric_line(ui, f64::from(ui.get_position()));
}

/// Timed lyrics: show the line being sung at `t`, with its neighbours.
pub fn show_lyric_line(ui: &AppWindow, t: f64) {
    LYRICS.with_borrow(|lyrics| {
        let Some(lyrics) = lyrics else { return };
        let Some(i) = lyrics.line_at(t + 0.15) else {
            // Before the first line: show it coming up.
            if LYRIC_LINE.with(|l| l.replace(None)).is_some() || ui.get_lyric_next().is_empty() {
                ui.set_lyric_prev("".into());
                ui.set_lyric_current("".into());
                let first = lyrics.synced().then(|| lyrics.lines.first()).flatten();
                ui.set_lyric_next(first.map(|(_, l)| l.as_str()).unwrap_or("").into());
            }
            return;
        };
        // How far through the line the song is, for the karaoke wipe: from
        // its time to the next line's (or 4 s for the last).
        if ui.get_karaoke_open() {
            let start = lyrics.lines[i].0.unwrap_or(t);
            let end = lyrics
                .lines
                .get(i + 1)
                .and_then(|(e, _)| *e)
                .unwrap_or(start + 4.0)
                .max(start + 0.1);
            ui.set_lyric_progress((((t + 0.15) - start) / (end - start)).clamp(0.0, 1.0) as f32);
        }
        if LYRIC_LINE.with(|l| l.replace(Some(i))) == Some(i) {
            return;
        }
        let line = |j: Option<usize>| -> SharedString {
            j.and_then(|j| lyrics.lines.get(j)).map(|(_, l)| l.as_str()).unwrap_or("").into()
        };
        ui.set_lyric_prev(line(i.checked_sub(1)));
        ui.set_lyric_current(line(Some(i)));
        ui.set_lyric_next(line(Some(i + 1)));
    });
}

/// Log lines that are expected and harmless, kept out of the error log:
/// - hwdec=auto probes every decoder API; failed probes (CUDA on a machine
///   without NVIDIA) are normal.
/// - Joining a live stream (HLS/MPEG-TS) in the middle of a video GOP makes
///   the H.264 decoder complain until the next keyframe, a second or so.
fn is_noise(prefix: &str, text: &str) -> bool {
    if text.contains("CUDA") || text.contains("libcuda") {
        return true;
    }
    // Blu-ray subtitle (PGS) streams carry no size until their first
    // picture; FFmpeg's probe complains, playback is fine.
    if prefix.starts_with("ffmpeg/demuxer")
        && (text.contains("Could not find codec parameters for stream")
            && text.contains("pgssub")
            || text.contains("Consider increasing the value for the 'analyzeduration'"))
    {
        return true;
    }
    // The spectrum meter merges mono bands with the music; amerge notes the
    // layouts it combines, and the meter restores the layout itself.
    if text.contains("Parsed_amerge")
        && (text.contains("No channel layout for input")
            || text.contains("Input channel layouts overlap"))
    {
        return true;
    }
    prefix.starts_with("ffmpeg/video")
        && [
            "non-existing PPS",
            "decode_slice_header error",
            "no frame!",
            "co located POCs unavailable",
            "mmco: unref short failure",
        ]
        .iter()
        .any(|n| text.contains(n))
}

/// mpv's notes when timestamps jump, which a DVD or Blu-ray does at every
/// menu, logo and chapter change: each piece restarts its clock, and mpv
/// re-syncs at once. On a disc they are routine; elsewhere they can mean a
/// real problem, so only [`UiState`] on a disc treats them as noise.
///
/// Menu navigation adds two more: a disc is read as a one-way stream, so
/// mpv's attempt to rewind its cache at a menu jump is refused ("Cannot seek
/// backward"), and the audio frame cut in half by the jump fails to decode.
fn is_disc_timing_note(prefix: &str, text: &str) -> bool {
    let t = text.trim();
    match prefix {
        "ad" => t.starts_with("Invalid audio PTS") || t.starts_with("Error decoding audio"),
        "vd" => t.starts_with("Invalid video PTS"),
        "disc" | "dvdnav" | "bd" => {
            t.starts_with("PTS discontinuity")
                || t.starts_with("Cannot seek backward in linear streams")
        }
        "cplayer" => {
            t.is_empty()
                || t.starts_with("Invalid video timestamp")
                || t.starts_with("Invalid audio timestamp")
                || t.starts_with("Audio/Video desynchronisation detected")
                || t.starts_with("hardware, temporary CPU spikes")
                || t.starts_with("position will not match to the video")
                || t.starts_with("Consider trying `--profile=fast`")
        }
        _ => false,
    }
}

const DISC_PROTECTION_HELP: &str = "This Blu-ray is encrypted. Commercial discs use \
AACS (and some BD+). mpv decrypts them through libbluray, which needs either libaacs with \
a key database (~/.config/aacs/KEYDB.cfg, holding this disc's key) or MakeMKV's libmmbd \
(run with LIBAACS_PATH=libmmbd LIBBDPLUS_PATH=libmmbd).";

/// A short explanation for the user when mpv's Blu-ray stream (log prefix "bd")
/// reports that the disc's copy protection could not be removed.
fn disc_protection_hint(prefix: &str, text: &str) -> Option<&'static str> {
    if prefix.starts_with("dvdnav") || prefix.starts_with("dvd") {
        return (text.contains("Encrypted DVD support unavailable")
            || text.contains("No css library available")
            || text.contains("Error cracking CSS key"))
        .then_some(crate::disc::DVDCSS_MISSING);
    }
    if prefix != "bd" {
        return None;
    }
    if text.contains("cannot find libaacs") {
        Some("This Blu-ray is copy-protected (AACS) and libaacs is not installed")
    } else if text.contains("AACS error: missing config file") {
        Some(
            "This Blu-ray is copy-protected (AACS): add a key database in ~/.config/aacs/KEYDB.cfg",
        )
    } else if text.contains("AACS error") {
        Some("This Blu-ray is copy-protected (AACS) and its key is not in your key database")
    } else if text.contains("BD+") {
        Some("This Blu-ray is copy-protected (BD+) and could not be decrypted")
    } else {
        None
    }
}

fn playlist_model(entries: &[PlaylistEntry]) -> ModelRc<PlaylistItem> {
    let items: Vec<PlaylistItem> = entries
        .iter()
        .map(|e| PlaylistItem {
            title: e.display_name().into(),
            detail: source_detail(&e.filename),
            current: e.current || e.playing,
        })
        .collect();
    ModelRc::new(VecModel::from(items))
}

/// Host name for URLs, parent folder for files.
fn source_detail(filename: &str) -> SharedString {
    // Audio CD songs are EDL segments of the cdda stream (see disc.rs).
    if filename.starts_with("cdda://") || filename.contains("%cdda://") {
        return "Audio CD".into();
    }
    if filename.starts_with("dvd://") || crate::dvdmenu::is_menu_path(filename) {
        return "DVD".into();
    }
    if let Some((scheme, rest)) = filename.split_once("://") {
        let host = rest.split('/').next().unwrap_or_default();
        return if host.is_empty() {
            scheme.to_uppercase().into()
        } else {
            host.into()
        };
    }
    std::path::Path::new(filename)
        .parent()
        .map(|p| display_name_for(&p.to_string_lossy()))
        .unwrap_or_default()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_for_urls_and_files() {
        assert_eq!(
            source_detail("https://example.com/live/stream.m3u8"),
            "example.com"
        );
        assert_eq!(source_detail("dvd://"), "DVD");
        assert_eq!(
            source_detail("edl://%7%cdda://,start=0,length=1"),
            "Audio CD"
        );
        assert_eq!(source_detail("/home/me/Music/Album/01.flac"), "Album");
    }

    #[test]
    fn explains_blu_ray_copy_protection() {
        // Messages as printed by mpv's stream_bluray.c.
        let hint = |text| disc_protection_hint("bd", text);
        assert!(
            hint("AACS error: missing config file\n")
                .unwrap()
                .contains("KEYDB.cfg")
        );
        assert!(
            hint("AACS error: no matching processing key\n")
                .unwrap()
                .contains("not in your key")
        );
        assert!(
            hint("AACS encryption detected but cannot find libaacs.\n")
                .unwrap()
                .contains("libaacs")
        );
        assert!(
            hint("Cannot decrypt BD+ encryption.\n")
                .unwrap()
                .contains("BD+")
        );
        assert_eq!(hint("Opening bd://\n"), None);
        assert_eq!(
            disc_protection_hint("cplayer", "AACS error: missing config file"),
            None
        );
        let dvd = disc_protection_hint("dvdnav", "libdvdread: Encrypted DVD support unavailable.");
        assert!(dvd.unwrap().contains("libdvdcss"));
    }

    #[test]
    fn disc_timing_notes() {
        // From a DVD (2026-11-23).
        assert!(is_disc_timing_note("ad", "Invalid audio PTS: 1.825367 -> -0.157733"));
        assert!(is_disc_timing_note("cplayer", "Invalid video timestamp: 1.951933 -> 0.018900"));
        assert!(is_disc_timing_note("disc", "PTS discontinuity: 68.284111->0.242256"));
        assert!(is_disc_timing_note("cplayer", "Audio/Video desynchronisation detected! Possible reasons include too slow"));
        assert!(is_disc_timing_note("cplayer", ""));
        assert!(is_disc_timing_note("dvdnav", "Cannot seek backward in linear streams!"));
        assert!(is_disc_timing_note("ad", "Error decoding audio."));
        assert!(!is_disc_timing_note("vd", "Error decoding audio."));
        assert!(!is_disc_timing_note("cplayer", "Audio device underrun detected."));
        assert!(!is_disc_timing_note("ffmpeg", "Invalid audio PTS"));
    }

    #[test]
    fn harmless_ffmpeg_notes_are_noise() {
        assert!(is_noise(
            "ffmpeg/demuxer",
            "mpegts: Could not find codec parameters for stream 3 (Subtitle: hdmv_pgs_subtitle (pgssub) ([144][0][0][0] / 0x0090)): unspecified size"
        ));
        assert!(is_noise(
            "ffmpeg/demuxer",
            "Consider increasing the value for the 'analyzeduration' (0) and 'probesize' (5000000) options"
        ));
        assert!(is_noise("ffmpeg", "Parsed_amerge_23: No channel layout for input 1"));
        // A video stream that can't be probed is a real problem.
        assert!(!is_noise(
            "ffmpeg/demuxer",
            "mpegts: Could not find codec parameters for stream 0 (Video: h264)"
        ));
    }

    #[test]
    fn stream_join_noise_is_not_an_error() {
        assert!(is_noise(
            "ffmpeg/video",
            "h264: non-existing PPS 0 referenced"
        ));
        assert!(is_noise("vd", "Cannot load libcuda.so.1"));
        assert!(!is_noise("ffmpeg/video", "h264: Invalid NAL unit size"));
    }
}
