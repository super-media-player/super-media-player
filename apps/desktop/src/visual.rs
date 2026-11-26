//! Music visualizers. Each one is an FFmpeg filter graph that mpv runs on the
//! audio (`lavfi-complex`): the audio still plays through `[ao]`, and the
//! filter's picture comes out as the video `[vo]`, which the GPU renders like
//! any video. So the visuals react to the real sound.

/// mpv `lavfi-complex` graph for visualizer `kind` on audio track `aid`.
/// `None` for the visuals the UI draws itself ("disc", "particles") or none at all.
pub fn graph(kind: &str, aid: i64) -> Option<String> {
    let filter = match kind {
        "spectrum" => {
            "showfreqs=s=1280x720:mode=bar:ascale=log:fscale=log:win_size=2048:averaging=3:\
             colors=0x39d0ff|0xa46bff,format=yuv420p"
        }
        "waves" => {
            "showwaves=s=1280x720:mode=cline:rate=30:scale=sqrt:draw=full:colors=0x39d0ff|0xa46bff,format=yuv420p"
        }
        "scope" => {
            "avectorscope=s=640x640:mode=lissajous_xy:draw=line:scale=sqrt:zoom=1.3:\
             rc=57:gc=208:bc=255:ac=255:rf=8:gf=4:bf=2:af=6,format=yuv420p"
        }
        "cqt" => {
            "showcqt=s=1280x720:fps=30:bar_g=2:sono_g=4:bar_v=9:sono_v=17:axis=0:count=2,format=yuv420p"
        }
        "spectrogram" => {
            "showspectrum=s=1280x720:slide=scroll:mode=combined:color=cool:scale=log:\
             fscale=log:legend=0:win_func=hann,format=yuv420p"
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
        let g = graph("spectrum", 2).unwrap();
        assert!(g.starts_with("[aid2]asplit[ao][vis];[vis]showfreqs"));
        assert!(g.ends_with("[vo]"));
        for k in crate::settings::VISUALIZERS {
            let has = graph(k, 1).is_some();
            assert_eq!(
                has,
                !matches!(*k, "disc" | "particles" | "milkdrop" | "none"),
                "{k}"
            );
        }
    }
}
