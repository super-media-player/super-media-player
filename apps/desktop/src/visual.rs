//! Music visualizers. Each one is an FFmpeg filter graph that mpv runs on the
//! audio (`lavfi-complex`): the audio still plays through `[ao]`, and the
//! filter's picture comes out as the video `[vo]`, which the GPU renders like
//! any video. So the visuals react to the real sound.

/// mpv `lavfi-complex` graph for visualizer `kind` on audio track `aid`.
/// `None` for the visuals the UI draws itself ("disc", "particles",
/// "spectrum" bars) or none at all.
pub fn graph(kind: &str, aid: i64) -> Option<String> {
    let filter = match kind {
        "waves" => {
            "showwaves=s=1280x720:mode=cline:rate=30:scale=sqrt:draw=full:colors=0x39d0ff|0xa46bff,format=yuv420p"
        }
        "scope" => {
            "avectorscope=s=640x640:mode=lissajous_xy:draw=line:scale=sqrt:zoom=1.3:\
             rc=57:gc=208:bc=255:ac=255:rf=8:gf=4:bf=2:af=6,format=yuv420p"
        }
        // Only the bars, filling the whole picture (no scrolling sonogram).
        "cqt" => {
            "showcqt=s=1280x720:fps=30:bar_g=2:bar_v=9:axis=0:sono_h=0:count=2,format=yuv420p"
        }
        // Small (the GPU scales it up smoothly): ~6 s of music across, so it
        // fills at once and the beats stand out. `fps`: without it, it makes
        // hundreds of pictures a second.
        "spectrogram" => {
            "showspectrum=s=480x270:slide=scroll:mode=combined:color=fiery:scale=log:\
             fscale=log:legend=0:overlap=0:fps=30:drange=90,format=yuv420p"
        }
        _ => return None,
    };
    Some(format!("[aid{aid}]asplit[ao][vis];[vis]{filter}[vo]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphs_route_audio_through() {
        let g = graph("cqt", 2).unwrap();
        assert!(g.starts_with("[aid2]asplit[ao][vis];[vis]showcqt"));
        assert!(g.ends_with("[vo]"));
        for k in crate::settings::VISUALIZERS {
            let has = graph(k, 1).is_some();
            assert_eq!(
                has,
                !matches!(*k, "disc" | "particles" | "spectrum" | "milkdrop" | "none"),
                "{k}"
            );
        }
    }
}
