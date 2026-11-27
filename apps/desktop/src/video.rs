//! Draws mpv video underneath the Slint UI using Slint's rendering notifier.
//!
//! Slint clears the window, calls `BeforeRendering`, then draws the UI. We
//! render the current video frame in `BeforeRendering`, so the UI (with its
//! transparent window background) is composited on top of the video.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::{CStr, c_void};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use player_core::{NativeDisplay, Player, RenderContext};
use slint::{ComponentHandle, GraphicsAPI, RenderingState};

use crate::AppWindow;

const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_SCISSOR_BOX: u32 = 0x0C10;
const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

/// The few raw GL entry points we need besides mpv's own.
#[derive(Clone, Copy)]
struct Gl {
    get_integerv: unsafe extern "C" fn(u32, *mut i32),
    viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    read_pixels: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void),
    pixel_storei: unsafe extern "C" fn(u32, i32),
    get_floatv: unsafe extern "C" fn(u32, *mut f32),
    is_enabled: unsafe extern "C" fn(u32) -> u8,
    enable: unsafe extern "C" fn(u32),
    disable: unsafe extern "C" fn(u32),
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
                get_integerv: f(get(c"glGetIntegerv"))?,
                viewport: f(get(c"glViewport"))?,
                read_pixels: f(get(c"glReadPixels"))?,
                pixel_storei: f(get(c"glPixelStorei"))?,
                get_floatv: f(get(c"glGetFloatv"))?,
                is_enabled: f(get(c"glIsEnabled"))?,
                enable: f(get(c"glEnable"))?,
                disable: f(get(c"glDisable"))?,
                scissor: f(get(c"glScissor"))?,
                clear_color: f(get(c"glClearColor"))?,
                clear: f(get(c"glClear"))?,
            })
        }
    }

    fn integers<const N: usize>(&self, pname: u32) -> [i32; N] {
        let mut v = [0i32; N];
        unsafe { (self.get_integerv)(pname, v.as_mut_ptr()) };
        v
    }

    /// Fill the top `height` pixels of a `win_height`-tall framebuffer with
    /// `color`, leaving the GL state the UI renderer relies on as it was.
    fn fill_top_strip(&self, win_height: i32, height: i32, color: [f32; 4]) {
        if height <= 0 {
            return;
        }
        let mut old_clear = [0f32; 4];
        let old_box = self.integers::<4>(GL_SCISSOR_BOX);
        unsafe {
            (self.get_floatv)(GL_COLOR_CLEAR_VALUE, old_clear.as_mut_ptr());
            let had_scissor = (self.is_enabled)(GL_SCISSOR_TEST) != 0;
            (self.enable)(GL_SCISSOR_TEST);
            // GL's origin is bottom-left.
            (self.scissor)(0, win_height - height, i32::MAX / 2, height);
            (self.clear_color)(color[0], color[1], color[2], color[3]);
            (self.clear)(GL_COLOR_BUFFER_BIT);
            (self.clear_color)(old_clear[0], old_clear[1], old_clear[2], old_clear[3]);
            (self.scissor)(old_box[0], old_box[1], old_box[2], old_box[3]);
            if !had_scissor {
                (self.disable)(GL_SCISSOR_TEST);
            }
        }
    }
}

/// Dev aid: save the composited window (video + UI) to a PPM file after a delay.
pub struct SnapshotRequest {
    pub path: PathBuf,
    pub after: Duration,
    pub then_quit: bool,
}

pub struct VideoRenderer {
    /// Filled in once the playback engine has started (after the first frame).
    player: Rc<OnceCell<Rc<Player>>>,
    ctx: Option<RenderContext>,
    /// The GL context exists, but mpv's render context is not created yet.
    setup_pending: bool,
    gl: Option<Gl>,
    /// Whether the current file has a video track to draw.
    pub active: Rc<Cell<bool>>,
    snapshot: Option<(SnapshotRequest, Instant)>,
    on_first_frame: Option<Box<dyn FnOnce()>>,
    on_ready: Option<Box<dyn FnOnce()>>,
    /// mpv video margins (left, top, right, bottom) last sent to mpv.
    margins: Rc<Cell<[f64; 4]>>,
    /// The MilkDrop visualizer, while it is on (milkdrop.rs).
    milkdrop: Option<crate::milkdrop::Renderer>,
    /// projectM failed to start in this context: don't retry every frame.
    milkdrop_failed: bool,
    /// Rounds the window's corners when it has no frame.
    corners: Option<crate::frame::Corners>,
}

impl VideoRenderer {
    /// Install the renderer on `ui`. `on_first_frame` runs after the window has
    /// been drawn once, so slow startup work can happen behind a visible
    /// splash. `on_ready` runs once `player` is set and mpv's render context
    /// exists (files given on the command line should be loaded only then,
    /// because mpv's `vo=libmpv` needs the render context to show video).
    pub fn install(
        ui: &AppWindow,
        player: Rc<OnceCell<Rc<Player>>>,
        active: Rc<Cell<bool>>,
        snapshot: Option<SnapshotRequest>,
        on_first_frame: impl FnOnce() + 'static,
        on_ready: impl FnOnce() + 'static,
    ) -> Result<(), slint::SetRenderingNotifierError> {
        let state = Rc::new(RefCell::new(VideoRenderer {
            player,
            ctx: None,
            setup_pending: false,
            gl: None,
            active,
            snapshot: snapshot.map(|s| (s, Instant::now())),
            on_first_frame: Some(Box::new(on_first_frame)),
            on_ready: Some(Box::new(on_ready)),
            margins: Rc::new(Cell::new([0.0; 4])),
            milkdrop: None,
            milkdrop_failed: false,
            corners: None,
        }));
        let weak = ui.as_weak();
        ui.window().set_rendering_notifier(move |rs, api| {
            let Some(ui) = weak.upgrade() else { return };
            state.borrow_mut().notify(&ui, rs, api);
        })
    }

    fn notify(&mut self, ui: &AppWindow, state: RenderingState, api: &GraphicsAPI) {
        match state {
            RenderingState::RenderingSetup => {
                if let GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                    self.gl = Gl::load(*get_proc_address);
                    self.corners = crate::frame::Corners::new(*get_proc_address);
                } else {
                    log::error!(
                        "video needs an OpenGL renderer, got {api:?}; video output disabled"
                    );
                }
                self.setup_pending = true;
            }
            RenderingState::BeforeRendering => {
                if self.setup_pending && self.player.get().is_some() {
                    self.setup_pending = false;
                    self.create_context(ui, api);
                }
                let Some(gl) = self.gl else { return };
                let size = ui.window().size();
                if let Some(ctx) = &self.ctx {
                    // Always let mpv process its update so it doesn't stall.
                    ctx.update();
                    if self.active.get() {
                        self.update_margins(ui);
                        let [fbo] = gl.integers::<1>(GL_FRAMEBUFFER_BINDING);
                        let viewport = gl.integers::<4>(GL_VIEWPORT);
                        if let Err(e) = ctx.render(fbo, size.width as i32, size.height as i32, true)
                        {
                            log::warn!("{e}");
                        }
                        // mpv changes the viewport; give the UI renderer its own back.
                        unsafe {
                            (gl.viewport)(viewport[0], viewport[1], viewport[2], viewport[3])
                        };
                    }
                }
                self.draw_milkdrop(ui, api, gl);
                // The window is transparent so the video shows through, and the
                // menu bar draws no background of its own: give it an opaque one,
                // or the desktop shows through behind the menu titles.
                let scale = ui.window().scale_factor() as f64;
                let menu = (size.height as f64 - ui.get_content_height() as f64 * scale)
                    .max(0.0)
                    .round() as i32;
                // The top of the app background's gradient.
                let bg = slint::RgbaColor::<f32>::from(ui.get_bar_color());
                gl.fill_top_strip(size.height as i32, menu, [bg.red, bg.green, bg.blue, 1.0]);
            }
            RenderingState::AfterRendering => {
                if let Some(ctx) = &self.ctx {
                    ctx.report_swap();
                }
                if self.milkdrop.is_some() {
                    // projectM animates by itself: keep frames coming.
                    ui.window().request_redraw();
                }
                if let Some(f) = self.on_first_frame.take() {
                    // A short pause lets the compositor actually show the frame.
                    slint::Timer::single_shot(Duration::from_millis(30), f);
                }
                if ui.get_own_frame()
                    && let Some(corners) = &self.corners
                {
                    let size = ui.window().size();
                    let r = crate::frame::RADIUS as f32 * ui.window().scale_factor();
                    corners.draw(size.width as i32, size.height as i32, r);
                }
                self.maybe_snapshot(ui);
            }
            RenderingState::RenderingTeardown => {
                self.milkdrop = None;
                self.milkdrop_failed = false;
                self.corners = None;
                self.ctx = None;
                self.gl = None;
            }
            _ => {}
        }
    }

    /// MilkDrop, in the area the UI keeps for the picture, while it is on.
    fn draw_milkdrop(&mut self, ui: &AppWindow, api: &GraphicsAPI, gl: Gl) {
        if !crate::milkdrop::active() {
            self.milkdrop = None;
            return;
        }
        if self.milkdrop.is_none() && !self.milkdrop_failed {
            if let GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                self.milkdrop = crate::milkdrop::Renderer::new(*get_proc_address);
            }
            self.milkdrop_failed = self.milkdrop.is_none();
            if self.milkdrop_failed {
                log::warn!("MilkDrop could not start in this window");
            }
        }
        let Some(md) = self.milkdrop.as_mut() else {
            return;
        };
        let window = ui.window();
        let scale = window.scale_factor() as f64;
        let h = window.size().height as f64;
        let menu = (h - ui.get_content_height() as f64 * scale).max(0.0);
        let x = ui.get_video_x() as f64 * scale;
        let top = ui.get_video_y() as f64 * scale + menu;
        let (vw, vh) = (
            ui.get_video_width() as f64 * scale,
            ui.get_video_height() as f64 * scale,
        );
        let [fbo] = gl.integers::<1>(GL_FRAMEBUFFER_BINDING);
        let rect = (
            x.round() as i32,
            (h - top - vh).round().max(0.0) as i32,
            vw.round() as i32,
            vh.round() as i32,
        );
        md.draw(fbo as u32, rect);
    }

    /// Keep the picture inside the area the UI reserves for it: below the menu
    /// bar, or the visualizer window of the compact view. mpv takes that as
    /// margins in fractions of the window. They are set from a timer, because
    /// calling into mpv from inside the render callback can deadlock with it.
    fn update_margins(&self, ui: &AppWindow) {
        let window = ui.window();
        let scale = window.scale_factor() as f64;
        let size = window.size();
        let (w, h) = (size.width as f64, size.height as f64);
        if w < 1.0 || h < 1.0 {
            return;
        }
        // The menu bar sits above the content and is not part of root.height.
        let menu = (h - ui.get_content_height() as f64 * scale).max(0.0);
        let (x, y) = (
            ui.get_video_x() as f64 * scale,
            ui.get_video_y() as f64 * scale + menu,
        );
        let (vw, vh) = (
            ui.get_video_width() as f64 * scale,
            ui.get_video_height() as f64 * scale,
        );
        let clamp = |v: f64| (v.clamp(0.0, 0.95) * 1000.0).round() / 1000.0;
        let m = [
            clamp(x / w),
            clamp(y / h),
            clamp((w - x - vw) / w),
            clamp((h - y - vh) / h),
        ];
        if m == self.margins.get() {
            return;
        }
        self.margins.set(m);
        log::debug!(
            "video area: window {w}x{h}, menu {menu}, rect {x},{y} {vw}x{vh} → margins {m:?}"
        );
        let Some(player) = self.player.get().cloned() else {
            return;
        };
        // Text scripts show (top left) goes inside the picture's area too, and
        // under the title. mpv counts these as if the window were 720 high.
        let title = if ui.get_s_view_mode() != 0 {
            0.0
        } else {
            56.0 * scale / h
        };
        let osd = [
            (m[0] * w / h * 720.0 + 25.0).min(300.0),
            ((m[1] + title) * 720.0 + 10.0).min(600.0),
        ];
        slint::Timer::single_shot(Duration::ZERO, move || {
            for (name, v) in ["osd-margin-x", "osd-margin-y"].iter().zip(osd) {
                let _ = player.set_string(name, &format!("{v:.0}"));
            }
            for (name, v) in ["left", "top", "right", "bottom"].iter().zip(m) {
                if let Err(e) = player.set_double(&format!("video-margin-ratio-{name}"), v) {
                    log::debug!("{e}");
                }
            }
            // Subtitles may use black bars, but not room kept for the
            // controls below the picture: they would be under them.
            let _ = player.set_string("sub-use-margins", if m[3] > 0.0 { "no" } else { "yes" });
        });
    }

    /// Create mpv's render context on the window's GL context. Runs inside a
    /// rendering callback, where that context is current.
    fn create_context(&mut self, ui: &AppWindow, api: &GraphicsAPI) {
        let (Some(player), GraphicsAPI::NativeOpenGL { get_proc_address }) =
            (self.player.get(), api)
        else {
            self.ready();
            return;
        };
        let weak = ui.as_weak();
        let redraw = move || {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            });
        };
        let display = native_display(ui);
        log::info!("video output: OpenGL render API, display {display:?}");
        match RenderContext::new_opengl(player, display, *get_proc_address, redraw) {
            Ok(ctx) => self.ctx = Some(ctx),
            Err(e) => log::error!("video output disabled: {e}"),
        }
        self.ready();
    }

    fn ready(&mut self) {
        if let Some(f) = self.on_ready.take() {
            // Defer so it runs outside the rendering callback.
            slint::Timer::single_shot(Duration::ZERO, f);
        }
    }

    fn maybe_snapshot(&mut self, ui: &AppWindow) {
        let due = matches!(&self.snapshot, Some((req, start)) if start.elapsed() >= req.after);
        if !due {
            if self.snapshot.is_some() {
                // Keep frames coming until the snapshot time.
                ui.window().request_redraw();
            }
            return;
        }
        let (req, _) = self.snapshot.take().unwrap();
        let Some(gl) = self.gl else { return };
        let size = ui.window().size();
        let (w, h) = (size.width as usize, size.height as usize);
        let mut rgba = vec![0u8; w * h * 4];
        unsafe {
            (gl.pixel_storei)(GL_PACK_ALIGNMENT, 1);
            (gl.read_pixels)(
                0,
                0,
                w as i32,
                h as i32,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                rgba.as_mut_ptr().cast(),
            );
        }
        // GL rows are bottom-up; PPM is top-down RGB.
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        for row in rgba.chunks_exact(w * 4).rev() {
            for px in row.as_chunks::<4>().0 {
                ppm.extend_from_slice(&px[..3]);
            }
        }
        match std::fs::write(&req.path, ppm) {
            Ok(()) => log::info!("snapshot saved to {}", req.path.display()),
            Err(e) => log::error!("snapshot failed: {e}"),
        }
        if req.then_quit {
            let _ = slint::quit_event_loop();
        }
    }
}

/// The window's Wayland/X11 display, for zero-copy hardware decoding on Linux.
fn native_display(ui: &AppWindow) -> NativeDisplay {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    let handle = ui.window().window_handle();
    let Ok(display) = handle.display_handle() else {
        return NativeDisplay::None;
    };
    match display.as_raw() {
        RawDisplayHandle::Wayland(h) => NativeDisplay::Wayland(h.display.as_ptr()),
        RawDisplayHandle::Xlib(h) => h
            .display
            .map_or(NativeDisplay::None, |d| NativeDisplay::X11(d.as_ptr())),
        _ => NativeDisplay::None,
    }
}
