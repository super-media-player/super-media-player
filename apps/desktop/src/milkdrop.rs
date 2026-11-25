//! The MilkDrop visualizer: libprojectM 4 (loaded at runtime, like libmpv)
//! drawing Winamp's MilkDrop presets on the GPU, in the window's own GL
//! context, under the UI.
//!
//! projectM reacts to sound it is fed. mpv does not hand out the samples
//! it plays, so the player feeds it a signal rebuilt from the levels its
//! meter measures (meter.rs, 20 bands, ~30 times a second): one sine per
//! band at that band's loudness. That carries what the presets react to —
//! bass, mids, treble and the beat — though not the exact waveform.
//!
//! Presets change every few seconds (Settings), chosen at random from the
//! preset folder, with projectM's smooth blend between them.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

type Handle = *mut c_void;

struct Lib {
    _lib: libloading::Library,
    create: unsafe extern "C" fn() -> Handle,
    destroy: unsafe extern "C" fn(Handle),
    load_preset: unsafe extern "C" fn(Handle, *const std::ffi::c_char, bool),
    window_size: unsafe extern "C" fn(Handle, usize, usize),
    render: unsafe extern "C" fn(Handle),
    pcm_float: unsafe extern "C" fn(Handle, *const f32, u32, i32),
    preset_duration: unsafe extern "C" fn(Handle, f64),
    hard_cut: unsafe extern "C" fn(Handle, bool),
    soft_cut: unsafe extern "C" fn(Handle, f64),
    fps: unsafe extern "C" fn(Handle, i32),
}

fn lib() -> Option<&'static Lib> {
    static LIB: std::sync::OnceLock<Option<Lib>> = std::sync::OnceLock::new();
    LIB.get_or_init(|| {
        let names = if cfg!(windows) {
            &["projectM-4.dll", "libprojectM-4.dll"][..]
        } else if cfg!(target_os = "macos") {
            &["libprojectM-4.4.dylib", "libprojectM-4.dylib"][..]
        } else {
            &["libprojectM-4.so.4", "libprojectM-4.so"][..]
        };
        let lib = crate::disc::open_bundled_library(names)?;
        unsafe {
            macro_rules! sym {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).ok()?
                };
            }
            Some(Lib {
                create: sym!("projectm_create"),
                destroy: sym!("projectm_destroy"),
                load_preset: sym!("projectm_load_preset_file"),
                window_size: sym!("projectm_set_window_size"),
                render: sym!("projectm_opengl_render_frame"),
                pcm_float: sym!("projectm_pcm_add_float"),
                preset_duration: sym!("projectm_set_preset_duration"),
                hard_cut: sym!("projectm_set_hard_cut_enabled"),
                soft_cut: sym!("projectm_set_soft_cut_duration"),
                fps: sym!("projectm_set_fps"),
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether MilkDrop can run here (libprojectM found and presets present).
pub fn available() -> bool {
    lib().is_some() && !presets().is_empty()
}

/// Where the presets are: `$SMP_MILKDROP_PRESETS`, the package's
/// `milkdrop/` folder, then the data folder's.
fn preset_dirs() -> Vec<PathBuf> {
    let mut d = Vec::new();
    if let Some(p) = std::env::var_os("SMP_MILKDROP_PRESETS") {
        d.push(PathBuf::from(p));
    }
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        d.push(exe.join("milkdrop"));
    }
    if let Some(data) = crate::settings::data_dir() {
        d.push(data.join("milkdrop"));
    }
    d
}

/// Every `.milk` preset, except the "transition" ones (fades to black).
fn presets() -> &'static [PathBuf] {
    static LIST: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        let mut out = Vec::new();
        for dir in preset_dirs() {
            let mut stack = vec![dir];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    let name = e.file_name().to_string_lossy().to_lowercase();
                    if p.is_dir() && !name.contains("transition") {
                        stack.push(p);
                    } else if name.ends_with(".milk") {
                        out.push(p);
                    }
                }
            }
            if !out.is_empty() {
                break;
            }
        }
        log::info!("MilkDrop: {} presets", out.len());
        out
    })
}

/// Shared between the meter (UI thread timer) and the renderer.
struct Feed {
    levels: Vec<f32>,
    phases: Vec<f64>,
}

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static SECONDS: Cell<u32> = const { Cell::new(20) };
    static FEED: RefCell<Feed> = RefCell::new(Feed { levels: vec![0.0; crate::meter::BANDS], phases: vec![0.0; crate::meter::BANDS] });
    static NEXT: Cell<bool> = const { Cell::new(false) };
}

/// Turn the visualizer on or off (it is drawn while on).
pub fn set_active(on: bool) {
    ACTIVE.with(|a| a.set(on));
}

pub fn active() -> bool {
    ACTIVE.with(Cell::get)
}

/// Seconds between preset changes.
pub fn set_preset_seconds(s: u32) {
    SECONDS.with(|c| c.set(s.max(5)));
}

/// Go to another preset now.
pub fn next_preset() {
    NEXT.with(|n| n.set(true));
}

/// The meter's latest levels (0..1 per band; None while silent or paused).
pub fn set_levels(levels: Option<&[f32]>) {
    FEED.with_borrow_mut(|f| match levels {
        Some(l) => f.levels = l.to_vec(),
        None => f.levels.iter_mut().for_each(|x| *x *= 0.8),
    });
}

/// `n` stereo samples at 44.1 kHz rebuilt from the band levels.
fn synth(n: usize) -> Vec<f32> {
    const RATE: f64 = 44_100.0;
    FEED.with_borrow_mut(|f| {
        let bands = f.levels.len();
        let mut out = vec![0f32; n * 2];
        for i in 0..bands {
            // The meter's band centres: 50 Hz .. 14 kHz, log-spaced.
            let hz = 50.0 * (14_000f64 / 50.0).powf(i as f64 / (bands - 1).max(1) as f64);
            let amp = f64::from(f.levels[i]).powi(2) * 0.5 / (1.0 + i as f64 * 0.15);
            let step = std::f64::consts::TAU * hz / RATE;
            let mut ph = f.phases[i];
            for s in 0..n {
                let v = (amp * ph.sin()) as f32;
                out[2 * s] += v;
                out[2 * s + 1] += v;
                ph += step;
            }
            f.phases[i] = ph % std::f64::consts::TAU;
        }
        out
    })
}

// ---- drawing ----

const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_RGBA: u32 = 0x1908;
const GL_RGBA8: u32 = 0x8058;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_NEAREST: u32 = 0x2600;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_BLEND: u32 = 0x0BE2;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;

#[allow(clippy::type_complexity)]
#[derive(Clone, Copy)]
struct Gl {
    bind_fb: unsafe extern "C" fn(u32, u32),
    blit: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32),
    gen_fb: unsafe extern "C" fn(i32, *mut u32),
    del_fb: unsafe extern "C" fn(i32, *const u32),
    gen_tex: unsafe extern "C" fn(i32, *mut u32),
    del_tex: unsafe extern "C" fn(i32, *const u32),
    bind_tex: unsafe extern "C" fn(u32, u32),
    tex_image: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    fb_tex: unsafe extern "C" fn(u32, u32, u32, u32, i32),
    viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    get_integerv: unsafe extern "C" fn(u32, *mut i32),
    is_enabled: unsafe extern "C" fn(u32) -> u8,
    enable: unsafe extern "C" fn(u32),
    disable: unsafe extern "C" fn(u32),
    use_program: unsafe extern "C" fn(u32),
    bind_vao: unsafe extern "C" fn(u32),
    bind_buffer: unsafe extern "C" fn(u32, u32),
    active_texture: unsafe extern "C" fn(u32),
    scissor: unsafe extern "C" fn(i32, i32, i32, i32),
    clear_color: unsafe extern "C" fn(f32, f32, f32, f32),
    clear: unsafe extern "C" fn(u32),
}

impl Gl {
    fn load(get: &dyn Fn(&CStr) -> *const c_void) -> Option<Self> {
        unsafe fn f<T: Copy>(p: *const c_void) -> Option<T> {
            (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
        }
        unsafe {
            Some(Self {
                bind_fb: f(get(c"glBindFramebuffer"))?,
                blit: f(get(c"glBlitFramebuffer"))?,
                gen_fb: f(get(c"glGenFramebuffers"))?,
                del_fb: f(get(c"glDeleteFramebuffers"))?,
                gen_tex: f(get(c"glGenTextures"))?,
                del_tex: f(get(c"glDeleteTextures"))?,
                bind_tex: f(get(c"glBindTexture"))?,
                tex_image: f(get(c"glTexImage2D"))?,
                fb_tex: f(get(c"glFramebufferTexture2D"))?,
                viewport: f(get(c"glViewport"))?,
                get_integerv: f(get(c"glGetIntegerv"))?,
                is_enabled: f(get(c"glIsEnabled"))?,
                enable: f(get(c"glEnable"))?,
                disable: f(get(c"glDisable"))?,
                use_program: f(get(c"glUseProgram"))?,
                bind_vao: f(get(c"glBindVertexArray"))?,
                bind_buffer: f(get(c"glBindBuffer"))?,
                active_texture: f(get(c"glActiveTexture"))?,
                scissor: f(get(c"glScissor"))?,
                clear_color: f(get(c"glClearColor"))?,
                clear: f(get(c"glClear"))?,
            })
        }
    }
}

/// projectM in the window's GL context. Create and use it only inside the
/// rendering callbacks (where that context is current).
pub struct Renderer {
    lib: &'static Lib,
    gl: Gl,
    handle: Handle,
    size: (i32, i32),
    /// Offscreen copy for areas that don't start in the corner.
    fbo: u32,
    tex: u32,
    tex_size: (i32, i32),
    last_frame: Instant,
    changed: Instant,
    rng: u64,
}

impl Renderer {
    pub fn new(get_proc: &dyn Fn(&CStr) -> *const c_void) -> Option<Self> {
        let lib = lib()?;
        let gl = Gl::load(get_proc)?;
        if presets().is_empty() {
            return None;
        }
        let handle = unsafe { (lib.create)() };
        if handle.is_null() {
            log::warn!("MilkDrop: projectM could not start (needs OpenGL ES 3 / OpenGL 3.3)");
            return None;
        }
        unsafe {
            // The player picks the presets; projectM only blends.
            (lib.preset_duration)(handle, 1.0e9);
            (lib.hard_cut)(handle, false);
            (lib.soft_cut)(handle, 3.0);
            (lib.fps)(handle, 60);
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
            | 1;
        let mut r = Self {
            lib,
            gl,
            handle,
            size: (0, 0),
            fbo: 0,
            tex: 0,
            tex_size: (0, 0),
            last_frame: Instant::now(),
            changed: Instant::now() - Duration::from_secs(3600),
            rng: seed,
        };
        r.pick_preset(false);
        Some(r)
    }

    fn pick_preset(&mut self, smooth: bool) {
        let list = presets();
        // xorshift: good enough for picking presets.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let p = &list[(self.rng % list.len() as u64) as usize];
        log::debug!("MilkDrop preset: {}", p.display());
        if let Ok(c) = CString::new(p.to_string_lossy().as_bytes()) {
            unsafe { (self.lib.load_preset)(self.handle, c.as_ptr(), smooth) };
        }
        self.changed = Instant::now();
    }

    /// Draw a frame into rectangle `(x, y, w, h)` (GL coordinates: from the
    /// bottom left) of framebuffer `target`.
    pub fn draw(&mut self, target: u32, rect: (i32, i32, i32, i32)) {
        let (x, y, w, h) = rect;
        if w < 8 || h < 8 {
            return;
        }
        let secs = SECONDS.with(Cell::get);
        if NEXT.with(|n| n.replace(false)) || self.changed.elapsed() >= Duration::from_secs(u64::from(secs)) {
            self.pick_preset(true);
        }
        // Sound since the last frame.
        let dt = self.last_frame.elapsed().as_secs_f64().clamp(0.0, 0.1);
        self.last_frame = Instant::now();
        let n = ((44_100.0 * dt) as usize).clamp(256, 4096);
        let pcm = synth(n);
        let g = self.gl;
        let mut viewport = [0i32; 4];
        unsafe {
            (g.get_integerv)(GL_VIEWPORT, viewport.as_mut_ptr());
            let scissor = (g.is_enabled)(GL_SCISSOR_TEST) != 0;
            (g.disable)(GL_SCISSOR_TEST);
            (self.lib.pcm_float)(self.handle, pcm.as_ptr(), n as u32, 2);
            if self.size != (w, h) {
                (self.lib.window_size)(self.handle, w as usize, h as usize);
                self.size = (w, h);
            }
            (g.bind_fb)(GL_FRAMEBUFFER, target);
            (self.lib.render)(self.handle);
            // projectM draws at the corner of the default framebuffer.
            if (x, y) != (0, 0) || target != 0 {
                self.move_into_place(target, rect);
            }
            // Leave GL as the UI renderer expects it.
            (g.bind_fb)(GL_FRAMEBUFFER, target);
            (g.use_program)(0);
            (g.bind_vao)(0);
            (g.bind_buffer)(GL_ARRAY_BUFFER, 0);
            (g.bind_buffer)(GL_ELEMENT_ARRAY_BUFFER, 0);
            (g.active_texture)(GL_TEXTURE0);
            (g.bind_tex)(GL_TEXTURE_2D, 0);
            (g.disable)(GL_DEPTH_TEST);
            (g.disable)(GL_BLEND);
            if scissor {
                (g.enable)(GL_SCISSOR_TEST);
            }
            (g.viewport)(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
    }

    /// Copy the frame from the corner to `rect` through an offscreen
    /// texture, and clear the corner again.
    unsafe fn move_into_place(&mut self, target: u32, (x, y, w, h): (i32, i32, i32, i32)) {
        let g = self.gl;
        unsafe {
            if self.tex_size != (w, h) {
                if self.fbo == 0 {
                    (g.gen_fb)(1, &mut self.fbo);
                    (g.gen_tex)(1, &mut self.tex);
                }
                (g.bind_tex)(GL_TEXTURE_2D, self.tex);
                (g.tex_image)(GL_TEXTURE_2D, 0, GL_RGBA8 as i32, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, std::ptr::null());
                (g.bind_fb)(GL_FRAMEBUFFER, self.fbo);
                (g.fb_tex)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, self.tex, 0);
                self.tex_size = (w, h);
            }
            (g.bind_fb)(GL_READ_FRAMEBUFFER, 0);
            (g.bind_fb)(GL_DRAW_FRAMEBUFFER, self.fbo);
            (g.blit)(0, 0, w, h, 0, 0, w, h, GL_COLOR_BUFFER_BIT, GL_NEAREST);
            // Clear the corner (Slint cleared it before; projectM drew there).
            (g.bind_fb)(GL_FRAMEBUFFER, 0);
            (g.enable)(GL_SCISSOR_TEST);
            (g.scissor)(0, 0, w, h);
            (g.clear_color)(0.0, 0.0, 0.0, 0.0);
            (g.clear)(GL_COLOR_BUFFER_BIT);
            (g.disable)(GL_SCISSOR_TEST);
            (g.bind_fb)(GL_READ_FRAMEBUFFER, self.fbo);
            (g.bind_fb)(GL_DRAW_FRAMEBUFFER, target);
            (g.blit)(0, 0, w, h, x, y, x + w, y + h, GL_COLOR_BUFFER_BIT, GL_NEAREST);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            (self.lib.destroy)(self.handle);
            if self.fbo != 0 {
                (self.gl.del_fb)(1, &self.fbo);
                (self.gl.del_tex)(1, &self.tex);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn synth_follows_levels() {
        super::set_levels(Some(&[0.0; crate::meter::BANDS]));
        assert!(super::synth(512).iter().all(|s| *s == 0.0));
        let mut l = [0.0; crate::meter::BANDS];
        l[0] = 1.0;
        super::set_levels(Some(&l));
        let s = super::synth(2048);
        let peak = s.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.3 && peak <= 1.0, "{peak}");
    }
}
