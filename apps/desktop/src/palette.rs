//! The command palette (Ctrl+K): type a few letters of any action, setting
//! or playlist item and press Enter.
//!
//! Matching is fuzzy: the letters typed must appear in order in the title
//! (or its keywords); matches at the start of words and runs of letters
//! rank higher.

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, PaletteItem};

/// Id, title, keywords (not shown, but matched), shortcut.
const ACTIONS: &[(&str, &str, &str, &str)] = &[
    ("open-files", "Open files", "add load browse", "Ctrl+O"),
    (
        "open-folder",
        "Open folder",
        "directory album",
        "Ctrl+Shift+O",
    ),
    (
        "open-url",
        "Open location (URL or path)",
        "link stream youtube paste",
        "Ctrl+U",
    ),
    (
        "open-disc",
        "Play disc in drive",
        "dvd blu-ray bluray cd",
        "Ctrl+D",
    ),
    (
        "disc-menu",
        "Disc menu",
        "dvd menu root title",
        "Ctrl+Shift+D",
    ),
    (
        "dvd-film",
        "Play the DVD's main film (no menu)",
        "dvd skip menu",
        "",
    ),
    ("rip-disc", "Rip & play disc with MakeMKV", "copy", ""),
    ("add-subtitle", "Load subtitle file", "srt ass", ""),
    ("play", "Play / pause", "resume stop", "Space"),
    ("stop", "Stop", "", ""),
    ("next", "Next", "skip track", "N"),
    ("prev", "Previous", "back track", "P"),
    ("start-over", "Start over", "beginning restart", "Home"),
    (
        "skip",
        "Skip intro / credits / sponsor",
        "opening ending next episode sponsorblock youtube",
        "",
    ),
    ("ab-loop", "A-B repeat", "loop section", "A"),
    ("shuffle", "Shuffle", "random", ""),
    ("repeat-off", "Repeat: off", "loop", ""),
    ("repeat-all", "Repeat: whole playlist", "loop", ""),
    ("repeat-one", "Repeat: current item", "loop", ""),
    ("speed-0.5", "Speed 0.5×", "slow", ""),
    ("speed-1", "Speed 1× (normal)", "", ""),
    ("speed-1.5", "Speed 1.5×", "fast", ""),
    ("speed-2", "Speed 2×", "fast", ""),
    (
        "skip-silence",
        "Skip silence on / off",
        "pauses podcast lecture audiobook trim smart speed",
        "",
    ),
    ("mute", "Mute / unmute", "sound volume", "M"),
    (
        "night",
        "Night mode",
        "compress loud quiet volume level",
        "",
    ),
    (
        "dialogue",
        "Dialogue boost",
        "voice speech clear centre center",
        "",
    ),
    (
        "karaoke",
        "Karaoke (remove vocals, big lyrics)",
        "sing vocal remove",
        "",
    ),
    (
        "signal",
        "Signal path",
        "bit-perfect audiophile sample rate dac",
        "",
    ),
    ("equalizer", "Equalizer", "eq bass treble", "E"),
    ("tracks", "Audio and subtitle tracks", "language", ""),
    ("subs-off", "Subtitles off", "", ""),
    (
        "transcript",
        "Subtitles from speech, and search",
        "whisper transcribe generate find said",
        "",
    ),
    ("enhance-0", "Video enhancement: off", "shader", ""),
    (
        "enhance-1",
        "Video enhancement: sharpen (CAS)",
        "shader",
        "",
    ),
    (
        "enhance-2",
        "Video enhancement: upscale (AMD FSR)",
        "shader super resolution",
        "",
    ),
    (
        "enhance-3",
        "Video enhancement: anime (Anime4K)",
        "shader upscale",
        "",
    ),
    ("compare", "Compare before / after", "split enhancement", ""),
    (
        "tonemap-0",
        "HDR tone-mapping: automatic",
        "hdr dolby vision hdr10 brightness",
        "",
    ),
    (
        "tonemap-1",
        "HDR tone-mapping: balanced (BT.2390)",
        "hdr hdr10 brightness",
        "",
    ),
    (
        "tonemap-2",
        "HDR tone-mapping: smooth (spline)",
        "hdr hdr10 brightness",
        "",
    ),
    (
        "tonemap-3",
        "HDR tone-mapping: filmic (Hable)",
        "hdr hdr10 brightness",
        "",
    ),
    (
        "tonemap-4",
        "HDR tone-mapping: bright (Mobius)",
        "hdr hdr10 brightness",
        "",
    ),
    (
        "tonemap-5",
        "HDR tone-mapping: clip",
        "hdr hdr10 brightness",
        "",
    ),
    (
        "smooth",
        "Smooth motion",
        "interpolation frame blend judder",
        "",
    ),
    ("clip", "Save a clip", "export cut gif share", ""),
    ("screenshot", "Take screenshot", "picture", "S"),
    ("frame-next", "Next frame", "step", "."),
    ("frame-prev", "Previous frame", "step", ","),
    (
        "navigation",
        "Advanced navigation",
        "jog shuttle scrub step frame edit jkl",
        "Ctrl+J",
    ),
    ("fullscreen", "Fullscreen", "", "F"),
    ("playlist", "Show / hide playlist", "queue", "L"),
    (
        "music-info",
        "About this song",
        "lyrics artist album cover",
        "I",
    ),
    ("view-player", "View: media player", "skin", ""),
    ("view-compact", "View: compact", "skin small", ""),
    ("view-classic", "View: classic deck", "skin winamp", ""),
    ("view-silver", "View: silver deck", "skin", ""),
    ("next-skin", "Next skin", "theme", "Ctrl+M"),
    ("vis-0", "Music visual: spinning disc", "visualizer", ""),
    ("vis-1", "Music visual: particles", "visualizer beat", ""),
    ("vis-2", "Music visual: spectrum bars", "visualizer", ""),
    ("vis-3", "Music visual: waveform", "visualizer", ""),
    ("vis-4", "Music visual: stereo scope", "visualizer", ""),
    ("vis-5", "Music visual: spectrum (CQT)", "visualizer", ""),
    ("vis-6", "Music visual: spectrogram", "visualizer", ""),
    (
        "vis-7",
        "Music visual: MilkDrop (projectM)",
        "visualizer winamp presets",
        "",
    ),
    ("vis-8", "Music visual: none", "visualizer", ""),
    (
        "milkdrop-next",
        "Next MilkDrop preset",
        "visualizer projectm change",
        "",
    ),
    (
        "cast",
        "Cast to a TV or speaker",
        "chromecast dlna screen send stream",
        "",
    ),
    ("cast-stop", "Stop casting", "chromecast dlna", ""),
    ("remote", "Phone remote", "mobile control qr", ""),
    ("party", "Watch together", "sync friends party", ""),
    ("jump", "Jump to time", "go to seek position", "Ctrl+T"),
    (
        "media",
        "Media library",
        "albums artists songs videos music collection continue watching search",
        "Ctrl+Shift+M",
    ),
    (
        "media-albums",
        "Media library: albums",
        "covers records",
        "",
    ),
    (
        "media-artists",
        "Media library: artists",
        "bands singers",
        "",
    ),
    (
        "media-videos",
        "Media library: videos",
        "films movies series",
        "",
    ),
    (
        "library",
        "Show saved links",
        "library favourites favorites bookmarks urls streams radio stations",
        "Ctrl+Shift+L",
    ),
    ("radio", "Internet radio", "stations fm directory live", ""),
    ("podcasts", "Podcasts", "episodes shows subscribe rss", ""),
    (
        "network",
        "Network (DLNA, SMB, SFTP)",
        "nas share server samba webdav upnp",
        "",
    ),
    (
        "save-link",
        "Save what's playing to the Library",
        "favourite favorite star url",
        "",
    ),
    (
        "find-subs",
        "Find subtitles online",
        "opensubtitles download srt search",
        "",
    ),
    (
        "record",
        "Record this stream / stop recording",
        "save capture radio dump",
        "",
    ),
    (
        "convert",
        "Save a clip or convert the file",
        "export mp4 mp3 flac opus whole",
        "",
    ),
    ("seek+5", "Forward 5 seconds", "skip", ""),
    ("seek-5", "Back 5 seconds", "rewind", ""),
    ("seek+30", "Forward 30 seconds", "skip", ""),
    ("seek-30", "Back 30 seconds", "rewind", ""),
    ("seek+60", "Forward 1 minute", "skip", ""),
    ("seek-60", "Back 1 minute", "rewind", ""),
    ("seek+600", "Forward 10 minutes", "skip", ""),
    ("seek-600", "Back 10 minutes", "rewind", ""),
    ("volume-up", "Volume up", "louder", ""),
    ("volume-down", "Volume down", "quieter", ""),
    (
        "audio-delay+",
        "Sound later (+100 ms)",
        "sync audio delay",
        "",
    ),
    (
        "audio-delay-",
        "Sound earlier (-100 ms)",
        "sync audio delay",
        "",
    ),
    (
        "sub-delay+",
        "Subtitles later (+100 ms)",
        "sync subtitle delay",
        "",
    ),
    (
        "sub-delay-",
        "Subtitles earlier (-100 ms)",
        "sync subtitle delay",
        "",
    ),
    ("zoom-in", "Zoom in", "picture bigger", ""),
    ("zoom-out", "Zoom out", "picture smaller", ""),
    (
        "shortcuts-edit",
        "Change keyboard shortcuts",
        "keys bind mouse wheel",
        "",
    ),
    (
        "bookmark-add",
        "Add a bookmark here",
        "mark remember position",
        "Ctrl+B",
    ),
    (
        "playlist-save",
        "Save playlist",
        "m3u xspf export",
        "Ctrl+S",
    ),
    ("playlist-open", "Open playlist", "m3u xspf pls import", ""),
    ("playlist-sort", "Sort playlist by title", "order", ""),
    (
        "playlist-dedupe",
        "Remove duplicates from the playlist",
        "tidy",
        "",
    ),
    ("chapter-next", "Next chapter", "skip", "PgDn"),
    ("chapter-prev", "Previous chapter", "back", "PgUp"),
    (
        "media-info",
        "Media information",
        "codec bitrate hdr resolution dropped frames details",
        "Ctrl+I",
    ),
    (
        "picture",
        "Adjust picture",
        "brightness contrast saturation gamma hue zoom pan aspect rotate",
        "",
    ),
    (
        "picture-reset",
        "Reset picture",
        "zoom aspect brightness",
        "Alt+Backspace",
    ),
    (
        "auto-crop",
        "Crop black bars on / off",
        "letterbox pillarbox borders autocrop widescreen",
        "C",
    ),
    ("deinterlace", "Deinterlace", "interlaced tv dvd", ""),
    ("ontop", "Always on top", "pin window float", ""),
    ("audio-delay-reset", "Reset audio delay", "sync lip", ""),
    ("sub-delay-reset", "Reset subtitle delay", "sync", ""),
    (
        "sub-sync",
        "Line up subtitles with the speech",
        "sync timing delay early late automatic",
        "",
    ),
    (
        "subtitle-look",
        "Subtitle look",
        "size colour color position background box font",
        "",
    ),
    ("sleep-1", "Sleep timer: 15 minutes", "pause stop later", ""),
    ("sleep-2", "Sleep timer: 30 minutes", "pause stop later", ""),
    ("sleep-4", "Sleep timer: 1 hour", "pause stop later", ""),
    ("sleep-6", "Pause after this file", "sleep end stop", ""),
    ("sleep-0", "Sleep timer: off", "cancel", ""),
    ("settings", "Settings", "preferences options", "Ctrl+P"),
    ("scripts", "Scripts", "mpv lua user plugins folder", ""),
    ("check", "System check", "doctor problems diagnose", ""),
    ("ytdlp", "Install / update yt-dlp", "youtube", ""),
    ("keydb", "Download Blu-ray key database", "aacs", ""),
    ("shortcuts", "Keyboard shortcuts", "keys help", ""),
    ("about", "About Super Media Player", "version", "F1"),
    ("quit", "Quit", "exit close", "Ctrl+Q"),
];

/// Most entries shown at once.
const MAX_SHOWN: usize = 60;

/// How well `query` matches `text` (higher is better), or None. Every
/// letter of the query must appear in order.
fn score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut ti = 0;
    let mut run = 0;
    let mut first = None;
    for qc in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (ti..t.len()).find(|&i| t[i] == qc)?;
        first.get_or_insert(found);
        let word_start = found == 0 || !t[found - 1].is_alphanumeric();
        run = if found == ti && ti > 0 { run + 1 } else { 0 };
        score += 1 + run * 3 + if word_start { 6 } else { 0 };
        ti = found + 1;
    }
    // The typed word as it is beats letters picked from here and there:
    // "radio" is "Internet radio", not "Reset audio delay".
    let (q, text) = (query.trim().to_lowercase(), text.to_lowercase());
    if let Some(at) = text
        .match_indices(&q)
        .map(|m| m.0)
        .find(|&i| !text[..i].ends_with(char::is_alphanumeric))
    {
        score += if at == 0 { 110 } else { 100 };
    } else if q.chars().count() > 2 && text.contains(&q) {
        score += 40;
    }
    // Earlier matches and shorter titles first.
    Some(score * 4 - first.unwrap_or(0) as i32 - t.len() as i32 / 8)
}

/// The palette's entries for `query`: actions, what scripts offer (id,
/// title, key), playlist items.
pub fn entries(
    query: &str,
    playlist: &[String],
    scripts: &[(String, String, String)],
) -> Vec<PaletteItem> {
    let mut scored: Vec<(i32, PaletteItem)> = ACTIONS
        .iter()
        .filter_map(|(id, title, keys, shortcut)| {
            let s =
                score(query, title).max(score(query, &format!("{title} {keys}")).map(|s| s - 4))?;
            Some((s, item(id, title, shortcut)))
        })
        .collect();
    for (id, title, key) in scripts {
        if let Some(s) = score(query, title) {
            scored.push((s, item(id, title, key)));
        }
    }
    if !query.is_empty() {
        for (i, name) in playlist.iter().enumerate() {
            if let Some(s) = score(query, name) {
                scored.push((
                    s - 2,
                    item(&format!("play:{i}"), &format!("Play: {name}"), "Playlist"),
                ));
            }
        }
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    }
    scored.into_iter().take(MAX_SHOWN).map(|(_, i)| i).collect()
}

fn item(id: &str, title: &str, detail: &str) -> PaletteItem {
    PaletteItem {
        id: id.into(),
        title: title.into(),
        detail: detail.into(),
    }
}

/// Wire the palette's callbacks.
pub fn install(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.on_palette_query(move |q| {
        let Some(ui) = weak.upgrade() else { return };
        use slint::Model;
        let names: Vec<String> = ui
            .get_playlist()
            .iter()
            .map(|p| p.title.to_string())
            .collect();
        ui.set_palette_items(ModelRc::new(VecModel::from(entries(
            &q,
            &names,
            &crate::scripts::actions(),
        ))));
    });
    let weak = ui.as_weak();
    ui.on_palette_run(move |id| {
        if let Some(ui) = weak.upgrade() {
            run(&ui, &id);
        }
    });
}

/// Every action: id and title (for the shortcut editor).
pub fn actions() -> Vec<(&'static str, &'static str)> {
    ACTIONS.iter().map(|a| (a.0, a.1)).collect()
}

pub fn has_action(id: &str) -> bool {
    ACTIONS.iter().any(|a| a.0 == id)
}

pub fn action_title(id: &str) -> Option<&'static str> {
    ACTIONS.iter().find(|a| a.0 == id).map(|a| a.1)
}

/// Do what palette entry `id` says, as the menu would.
pub fn run(ui: &AppWindow, id: &str) {
    if let Some(s) = id.strip_prefix("seek").and_then(|s| s.parse::<f32>().ok()) {
        ui.invoke_seek_relative(s);
        return;
    }
    if let Some(i) = id.strip_prefix("play:").and_then(|i| i.parse::<i32>().ok()) {
        ui.invoke_play_index(i);
        return;
    }
    if let Some(s) = id
        .strip_prefix("speed-")
        .and_then(|s| s.parse::<f32>().ok())
    {
        ui.invoke_set_speed(s);
        return;
    }
    if let Some(a) = id.strip_prefix(crate::scripts::ACTION) {
        crate::scripts::run(ui, a);
        return;
    }
    let num = |prefix: &str| id.strip_prefix(prefix).and_then(|v| v.parse::<i32>().ok());
    if let Some(v) = num("vis-") {
        ui.invoke_choose_visual(v);
        return;
    }
    if let Some(v) = num("sleep-") {
        ui.invoke_basic("sleep".into(), v as f32);
        return;
    }
    if let Some(v) = num("enhance-") {
        ui.invoke_set_enhance(v);
        return;
    }
    if let Some(v) = num("tonemap-") {
        ui.invoke_set_tone_map(v);
        return;
    }
    match id {
        "volume-up" => ui.invoke_set_volume((ui.get_volume() + 5.0).min(150.0)),
        "volume-down" => ui.invoke_set_volume((ui.get_volume() - 5.0).max(0.0)),
        "audio-delay+" => ui.invoke_basic("audio-delay".into(), 0.1),
        "audio-delay-" => ui.invoke_basic("audio-delay".into(), -0.1),
        "sub-delay+" => ui.invoke_basic("sub-delay".into(), 0.1),
        "sub-delay-" => ui.invoke_basic("sub-delay".into(), -0.1),
        "zoom-in" => ui.invoke_basic("zoom".into(), 0.1),
        "zoom-out" => ui.invoke_basic("zoom".into(), -0.1),
        "scripts" => ui.invoke_show_settings_tab(10),
        "shortcuts-edit" => ui.invoke_show_settings_tab(6),
        "jump" => ui.invoke_show_jump(),
        "media" => ui.invoke_show_media(0),
        "media-albums" => ui.invoke_show_media(1),
        "media-artists" => ui.invoke_show_media(2),
        "media-videos" => ui.invoke_show_media(5),
        "library" => ui.invoke_show_library(0),
        "radio" => ui.invoke_show_library(1),
        "podcasts" => ui.invoke_show_library(2),
        "network" => ui.invoke_show_library(3),
        "save-link" => ui.invoke_library_save_current(),
        "find-subs" => ui.invoke_show_sub_search(),
        "record" => ui.invoke_basic("record".into(), 0.0),
        "convert" => ui.set_clip_open(true),
        "bookmark-add" => ui.invoke_basic("bookmark-add".into(), 0.0),
        "playlist-save" => ui.invoke_playlist_action("save".into(), 0),
        "playlist-open" => ui.invoke_playlist_action("open".into(), 0),
        "playlist-sort" => ui.invoke_playlist_action("title".into(), 0),
        "playlist-dedupe" => ui.invoke_playlist_action("dedupe".into(), 0),
        "chapter-next" => ui.invoke_basic("chapter-step".into(), 1.0),
        "chapter-prev" => ui.invoke_basic("chapter-step".into(), -1.0),
        "media-info" => {
            ui.set_media_info_open(true);
            ui.invoke_basic("media-info".into(), 0.0);
        }
        "picture" => ui.set_picture_open(true),
        "picture-reset" => ui.invoke_basic("reset".into(), 0.0),
        "auto-crop" => ui.invoke_toggle_auto_crop(),
        "deinterlace" => ui.invoke_basic(
            "deinterlace".into(),
            if ui.get_picture().deinterlace {
                0.0
            } else {
                1.0
            },
        ),
        "ontop" => {
            ui.set_s_always_on_top(!ui.get_s_always_on_top());
            ui.invoke_settings_changed();
        }
        "audio-delay-reset" => ui.invoke_basic("audio-delay-reset".into(), 0.0),
        "sub-delay-reset" => ui.invoke_basic("sub-delay-reset".into(), 0.0),
        "subtitle-look" => ui.invoke_show_settings_tab(5),
        "open-files" => ui.invoke_open_files(),
        "open-folder" => ui.invoke_open_folder(),
        "open-url" => ui.invoke_show_url(),
        "open-disc" => ui.invoke_open_disc("auto".into()),
        "disc-menu" => ui.invoke_dvd("menu".into()),
        "dvd-film" => ui.invoke_open_disc("dvd-film".into()),
        "rip-disc" => ui.invoke_rip_disc(),
        "add-subtitle" => ui.invoke_add_subtitle(),
        "play" => ui.invoke_toggle_pause(),
        "stop" => ui.invoke_stop(),
        "next" => ui.invoke_next(),
        "prev" => ui.invoke_prev(),
        "start-over" => ui.invoke_start_over(),
        "skip" => ui.invoke_skip_pressed(),
        "ab-loop" => ui.invoke_ab_loop(),
        "shuffle" => ui.invoke_toggle_shuffle(),
        "repeat-off" => ui.invoke_set_repeat(0),
        "repeat-all" => ui.invoke_set_repeat(1),
        "repeat-one" => ui.invoke_set_repeat(2),
        "skip-silence" => ui.invoke_toggle_skip_silence(),
        "mute" => ui.invoke_toggle_mute(),
        "night" => ui.invoke_toggle_night(),
        "dialogue" => ui.invoke_toggle_dialogue(),
        "karaoke" => ui.invoke_toggle_karaoke(),
        "signal" => ui.set_signal_open(true),
        "equalizer" => ui.set_eq_open(true),
        "tracks" => ui.invoke_show_tracks(),
        "subs-off" => ui.invoke_select_subtitle(-1),
        "transcript" => ui.set_transcript_open(true),
        "sub-sync" => {
            ui.set_subsync_open(true);
            ui.invoke_sub_sync();
        }
        "compare" => ui.invoke_toggle_compare(),
        "smooth" => ui.invoke_toggle_smooth(),
        "clip" => ui.set_clip_open(true),
        "screenshot" => ui.invoke_screenshot(),
        "navigation" => ui.invoke_toggle_nav(),
        "frame-next" => ui.invoke_frame_step(true),
        "frame-prev" => ui.invoke_frame_step(false),
        "fullscreen" => ui.invoke_toggle_fullscreen(),
        "playlist" => ui.set_playlist_open(!ui.get_playlist_open()),
        "music-info" => ui.set_info_open(true),
        "view-player" => ui.invoke_choose_view(0),
        "view-compact" => ui.invoke_choose_view(1),
        "view-classic" => ui.invoke_choose_view(2),
        "view-silver" => ui.invoke_choose_view(3),
        "next-skin" => {
            use slint::Model;
            let n = ui.get_skin_names().row_count().max(1) as i32;
            ui.invoke_choose_skin((ui.get_s_skin() + 1) % n);
        }
        "milkdrop-next" => crate::milkdrop::next_preset(),
        "cast" => ui.invoke_show_cast(),
        "cast-stop" => ui.invoke_cast_stop(),
        "remote" => ui.set_remote_open(true),
        "party" => ui.set_party_open(true),
        "settings" => ui.invoke_show_settings(),
        "check" => {
            ui.set_check_open(true);
            ui.invoke_run_check();
        }
        "ytdlp" => ui.invoke_install_ytdlp(),
        "keydb" => ui.invoke_install_keydb(),
        "shortcuts" => ui.invoke_show_shortcuts(),
        "about" => ui.invoke_show_about(),
        "quit" => ui.invoke_quit(),
        other => log::warn!("command palette: unknown action {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matching() {
        assert!(score("nm", "Night mode").is_some());
        assert!(score("mn", "Night mode").is_none());
        // Word starts beat letters in the middle.
        assert!(score("sp", "Signal path").unwrap() > score("sp", "Display").unwrap());
        for (q, id) in [
            ("radio", "radio"),
            ("network", "network"),
            ("disc menu", "disc-menu"),
            ("saved", "library"),
            ("mute", "mute"),
        ] {
            assert_eq!(entries(q, &[], &[])[0].id, id, "{q}");
        }
        let e = entries("karao", &[], &[]);
        assert_eq!(e[0].id, "karaoke");
        let e = entries("song two", &["Song One".into(), "Song Two".into()], &[]);
        assert!(e.iter().any(|i| i.id == "play:1"));
        assert_eq!(
            entries("", &["x".into()], &[]).len(),
            ACTIONS.len().min(MAX_SHOWN)
        );
    }

    /// Every action id is handled by `run` (no "unknown action" at runtime).
    #[test]
    fn every_action_is_handled() {
        let src = include_str!("palette.rs");
        let body = &src[src.find("fn run(").unwrap()..src.find("#[cfg(test)]").unwrap()];
        for (id, ..) in ACTIONS {
            let handled = body.contains(&format!("\"{id}\""))
                || ["speed-", "vis-", "enhance-", "tonemap-", "sleep-", "seek"]
                    .iter()
                    .any(|p| id.starts_with(p));
            assert!(handled, "{id}");
        }
    }
}
