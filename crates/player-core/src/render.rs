//! OpenGL video rendering through mpv's render API.

use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;

use mpv_sys::*;

use crate::{Error, Inner, Player, Result, as_void};

type ProcLoader<'a> = &'a dyn Fn(&CStr) -> *const c_void;
type UpdateFn = Box<dyn Fn() + Send + Sync>;

/// Native display connection of the window that owns the GL context.
///
/// Passing it lets mpv use zero-copy hardware decoding (e.g. VAAPI via EGL)
/// instead of copying decoded frames through system memory.
#[derive(Debug, Clone, Copy)]
pub enum NativeDisplay {
    None,
    /// `wl_display*`
    Wayland(*mut c_void),
    /// Xlib `Display*`
    X11(*mut c_void),
}

/// Draws mpv's video into an OpenGL framebuffer.
///
/// Create, use and drop it on the thread that owns the GL context, with that
/// context current. It is deliberately neither `Send` nor `Sync`.
pub struct RenderContext {
    inner: Arc<Inner>,
    ctx: *mut mpv_render_context,
    // Boxed twice so the pointer handed to mpv stays stable.
    _update: Box<UpdateFn>,
}

unsafe extern "C" fn get_proc_address_trampoline(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    let loader = unsafe { &*(ctx as *const ProcLoader<'_>) };
    loader(unsafe { CStr::from_ptr(name) }) as *mut c_void
}

unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    let f = unsafe { &*(ctx as *const UpdateFn) };
    f();
}

impl RenderContext {
    /// Create the render context for the current OpenGL context.
    ///
    /// `on_update` is called from an mpv thread whenever a new frame should be
    /// drawn. It must not call into the player; it should only schedule a redraw.
    pub fn new_opengl(
        player: &Player,
        display: NativeDisplay,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        on_update: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        let inner = player.inner().clone();
        let api = inner.api;

        let mut loader: ProcLoader<'_> = get_proc_address;
        let mut init = mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address_trampoline),
            get_proc_address_ctx: as_void(&mut loader),
        };
        let mut params = vec![
            mpv_render_param {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: as_void(&mut init),
            },
        ];
        match display {
            NativeDisplay::Wayland(d) => params.push(mpv_render_param {
                type_: MPV_RENDER_PARAM_WL_DISPLAY,
                data: d,
            }),
            NativeDisplay::X11(d) => params.push(mpv_render_param {
                type_: MPV_RENDER_PARAM_X11_DISPLAY,
                data: d,
            }),
            NativeDisplay::None => {}
        }
        params.push(mpv_render_param {
            type_: MPV_RENDER_PARAM_INVALID,
            data: std::ptr::null_mut(),
        });

        let mut ctx = std::ptr::null_mut();
        let rc =
            unsafe { (api.mpv_render_context_create)(&mut ctx, inner.handle, params.as_mut_ptr()) };
        if rc < 0 {
            let message = unsafe { crate::node::cstr_to_string((api.mpv_error_string)(rc)) };
            return Err(Error::Mpv {
                code: rc,
                message,
                context: "mpv_render_context_create".into(),
            });
        }

        let update: Box<UpdateFn> = Box::new(Box::new(on_update));
        unsafe {
            (api.mpv_render_context_set_update_callback)(
                ctx,
                Some(update_trampoline),
                &*update as *const UpdateFn as *mut c_void,
            )
        };
        Ok(Self {
            inner,
            ctx,
            _update: update,
        })
    }

    /// Process pending updates. Returns true if a new video frame is ready.
    /// Call it after `on_update` fired, before [`render`](Self::render).
    pub fn update(&self) -> bool {
        let flags = unsafe { (self.inner.api.mpv_render_context_update)(self.ctx) };
        flags & MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Render the current frame into framebuffer `fbo` of size `w`×`h` pixels.
    /// Set `flip_y` when rendering to the default framebuffer (fbo 0).
    pub fn render(&self, fbo: i32, w: i32, h: i32, flip_y: bool) -> Result<()> {
        let mut target = mpv_opengl_fbo {
            fbo,
            w,
            h,
            internal_format: 0,
        };
        let mut flip: i32 = flip_y.into();
        // Never block the UI thread waiting for the frame's display time.
        let mut block: i32 = 0;
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: as_void(&mut target),
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: as_void(&mut flip),
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: as_void(&mut block),
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let rc =
            unsafe { (self.inner.api.mpv_render_context_render)(self.ctx, params.as_mut_ptr()) };
        self.inner.check(rc, || "mpv_render_context_render".into())
    }

    /// Tell mpv the frame was presented (improves frame timing).
    pub fn report_swap(&self) {
        unsafe { (self.inner.api.mpv_render_context_report_swap)(self.ctx) };
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe {
            (self.inner.api.mpv_render_context_set_update_callback)(
                self.ctx,
                None,
                std::ptr::null_mut(),
            );
            (self.inner.api.mpv_render_context_free)(self.ctx);
        }
    }
}
