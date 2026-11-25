//! Video enhancement: GPU shaders that sharpen or upscale the picture, a
//! before / after split to compare, and smoother motion.
//!
//! The shaders ship inside the player (`shaders/`): our own contrast
//! adaptive sharpening, AMD FidelityFX Super Resolution 1.0 ported to mpv
//! (MIT), and Anime4K v4 "mode A" (MIT). They are written to the cache
//! folder and given to mpv as `glsl-shaders`.
//!
//! For the comparison every pass that writes the final picture is wrapped:
//! left of the split it hands its input through unchanged (scaled the
//! plain way), right of it the enhanced result. Passes that only make
//! intermediate textures are left alone.

use std::path::PathBuf;

use player_core::Player;

const SHADERS: &[(&str, &str)] = &[
    ("SMP_CAS.glsl", include_str!("../shaders/SMP_CAS.glsl")),
    ("FSR.glsl", include_str!("../shaders/FSR.glsl")),
    ("Anime4K_Clamp_Highlights.glsl", include_str!("../shaders/Anime4K_Clamp_Highlights.glsl")),
    ("Anime4K_Restore_CNN_M.glsl", include_str!("../shaders/Anime4K_Restore_CNN_M.glsl")),
    ("Anime4K_Upscale_CNN_x2_M.glsl", include_str!("../shaders/Anime4K_Upscale_CNN_x2_M.glsl")),
    ("Anime4K_AutoDownscalePre_x2.glsl", include_str!("../shaders/Anime4K_AutoDownscalePre_x2.glsl")),
    ("Anime4K_AutoDownscalePre_x4.glsl", include_str!("../shaders/Anime4K_AutoDownscalePre_x4.glsl")),
    ("Anime4K_Upscale_CNN_x2_S.glsl", include_str!("../shaders/Anime4K_Upscale_CNN_x2_S.glsl")),
];

/// The shaders of each enhancement (settings::ENHANCE), in order.
fn chain(mode: &str) -> &'static [&'static str] {
    match mode {
        "sharpen" => &["SMP_CAS.glsl"],
        "fsr" => &["FSR.glsl"],
        // Anime4K's "Mode A (fast)" for 1080p screens.
        "anime4k" => &[
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Restore_CNN_M.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
            "Anime4K_AutoDownscalePre_x2.glsl",
            "Anime4K_AutoDownscalePre_x4.glsl",
            "Anime4K_Upscale_CNN_x2_S.glsl",
        ],
        _ => &[],
    }
}

/// `src` with every final pass showing its input left of `split` (0..1
/// across the video).
pub fn with_split(src: &str, split: f64) -> String {
    // Blocks: a run of `//!` header lines and the code after it.
    let mut blocks: Vec<(Vec<&str>, Vec<&str>)> = Vec::new();
    let mut in_header = false;
    for line in src.lines() {
        if line.starts_with("//!") {
            if !in_header {
                blocks.push((Vec::new(), Vec::new()));
                in_header = true;
            }
            blocks.last_mut().unwrap().0.push(line);
        } else {
            in_header = false;
            match blocks.last_mut() {
                Some(b) => b.1.push(line),
                None => blocks.push((Vec::new(), vec![line])),
            }
        }
    }
    let mut out = String::new();
    for (header, code) in blocks {
        let arg = |k: &str| header.iter().find_map(|h| h.strip_prefix(k)).map(str::trim);
        let hook = arg("//!HOOK ");
        let save = arg("//!SAVE ");
        let is_final = hook.is_some() && (save.is_none() || save == hook);
        let code = code.join("\n");
        let wrap = is_final && code.contains("vec4 hook()");
        for h in &header {
            out += h;
            out.push('\n');
        }
        // The wrapper reads the pass's input: under the name the pass binds
        // it by (HOOKED, or its own name, e.g. MAIN), else bound here.
        let bound = |name: &str| header.iter().any(|h| h.trim() == format!("//!BIND {name}"));
        let input = match hook {
            _ if bound("HOOKED") => "HOOKED",
            Some(h) if bound(h) => h,
            _ => {
                if wrap {
                    out += "//!BIND HOOKED\n";
                }
                "HOOKED"
            }
        };
        if wrap {
            out += &code.replacen("vec4 hook()", "vec4 smp_hook_inner()", 1);
            out += &format!(
                "\nvec4 hook() {{\n    if ({input}_pos.x < {split:.4}) return {input}_tex({input}_pos);\n    return smp_hook_inner();\n}}\n"
            );
        } else {
            out += &code;
            out.push('\n');
        }
    }
    out
}

fn shader_dir() -> Option<PathBuf> {
    crate::settings::cache_dir().map(|d| d.join("shaders"))
}

/// Write the shaders for `mode` (split for comparing, or not) and return
/// their paths.
fn write_chain(mode: &str, split: Option<f64>) -> Result<Vec<PathBuf>, String> {
    let dir = shader_dir().ok_or("no cache folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    chain(mode)
        .iter()
        .map(|name| {
            let src = SHADERS.iter().find(|(n, _)| n == name).map(|(_, s)| *s).ok_or("missing shader")?;
            let (file, text) = match split {
                Some(s) => (format!("split-{:04}-{name}", (s * 1000.0).round() as u32), with_split(src, s)),
                None => (name.to_string(), src.to_owned()),
            };
            let path = dir.join(file);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Ok(path)
        })
        .collect()
}

/// Put enhancement `mode` on the player; with `split`, the comparison.
pub fn apply(player: &Player, mode: &str, split: Option<f64>) -> Result<(), String> {
    let paths = write_chain(mode, split)?;
    player.command(&["change-list", "glsl-shaders", "clr", ""]).map_err(|e| e.to_string())?;
    for p in paths {
        player
            .command(&["change-list", "glsl-shaders", "append", &p.to_string_lossy()])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A split across the window (0..1) as a split across the picture, which
/// may have black bars (mpv's `osd-dimensions`).
pub fn window_to_video(player: &Player, split: f64) -> f64 {
    let Some(d) = player.get_value("osd-dimensions") else { return split };
    let f = |k: &str| d.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let (w, ml, mr) = (f("w"), f("ml"), f("mr"));
    let vw = w - ml - mr;
    if vw <= 1.0 { split } else { crate::autocrop::across_frame(((split * w - ml) / vw).clamp(0.0, 1.0)) }
}

/// Smoother motion: mpv shows each frame for a share of the screen's
/// refresh, blending neighbours (no invented frames). `refresh` in Hz.
pub fn set_smooth(player: &Player, on: bool, refresh: Option<f64>) {
    let set = |k: &str, v: &str| {
        if let Err(e) = player.set_string(k, v) {
            log::debug!("{k}: {e}");
        }
    };
    if on {
        if let Some(hz) = refresh.filter(|h| *h > 20.0) {
            set("display-fps-override", &format!("{hz:.3}"));
        }
        set("video-sync", "display-resample");
        set("interpolation", "yes");
        set("tscale", "oversample");
    } else {
        set("interpolation", "no");
        set("video-sync", "audio");
    }
}

/// How HDR video is fitted to the (SDR) screen: the curve (one of
/// `settings::TONE_MAPS`) and whether each scene's brightness is measured,
/// so dark scenes are not dimmed for highlights they don't have. SDR video
/// is not touched by either.
pub fn set_tone_mapping(player: &Player, curve: &str, peak_detect: bool) {
    for (k, v) in [("tone-mapping", curve), ("hdr-compute-peak", if peak_detect { "auto" } else { "no" })] {
        if let Err(e) = player.set_string(k, v) {
            log::warn!("{k}={v}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mpv knows every curve we offer, and the peak switch.
    #[test]
    fn tone_mapping_options_are_accepted() {
        let Ok(player) = Player::new(&player_core::PlayerConfig::headless(), |_| {}) else {
            eprintln!("no libmpv, skipped");
            return;
        };
        for curve in crate::settings::TONE_MAPS {
            assert!(player.set_string("tone-mapping", curve).is_ok(), "{curve}");
            assert_eq!(player.get_string("tone-mapping").as_deref(), Some(*curve));
        }
        for v in ["auto", "no"] {
            assert!(player.set_string("hdr-compute-peak", v).is_ok(), "{v}");
        }
    }

    #[test]
    fn split_wraps_only_final_passes() {
        let src = "//!HOOK MAIN\n//!BIND HOOKED\n//!SAVE tmp\nvec4 hook() { return vec4(1); }\n\
                   //!HOOK MAIN\n//!BIND tmp\nvec4 hook() { return tmp_tex(tmp_pos); }\n\
                   //!HOOK MAIN\n//!BIND MAIN\nvec4 hook() { return MAIN_tex(MAIN_pos); }\n";
        let out = with_split(src, 0.25);
        assert_eq!(out.matches("smp_hook_inner").count(), 4, "{out}");
        assert!(out.contains("HOOKED_pos.x < 0.2500"));
        // The second pass didn't bind its input; now it does.
        assert_eq!(out.matches("//!BIND HOOKED").count(), 2, "{out}");
        // A pass binding its input by name uses that name, not a second binding.
        assert!(out.contains("if (MAIN_pos.x < 0.2500) return MAIN_tex(MAIN_pos);"), "{out}");
        // The intermediate pass is untouched.
        assert!(out.contains("//!SAVE tmp\nvec4 hook() { return vec4(1); }"));
        // Every shipped shader keeps one final pass per file at least.
        for (name, s) in SHADERS {
            assert!(with_split(s, 0.5).contains("smp_hook_inner"), "{name}");
        }
        for mode in crate::settings::ENHANCE {
            for n in chain(mode) {
                assert!(SHADERS.iter().any(|(s, _)| s == n), "{n}");
            }
        }
    }

    /// The shaders compile in mpv and the picture still comes out, split or
    /// not. Skipped without libmpv or ffmpeg. (vo=null doesn't run shaders,
    /// so this checks loading; the render path runs them.)
    #[test]
    fn mpv_accepts_the_chains() {
        let Ok(p) = Player::new(&player_core::PlayerConfig::headless(), |_| {}) else {
            eprintln!("libmpv not available; skipping");
            return;
        };
        for mode in crate::settings::ENHANCE {
            apply(&p, mode, None).unwrap();
            apply(&p, mode, Some(0.5)).unwrap();
            let n = p.get_value("glsl-shaders").map(|v| v.as_array().len()).unwrap_or(0);
            assert_eq!(n, chain(mode).len(), "{mode}");
        }
    }
}
