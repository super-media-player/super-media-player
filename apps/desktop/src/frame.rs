//! The window's own frame in the media player view: what titlebar.slint can't
//! do by itself. Maximizing, resizing from the edges, the desktop's window
//! menu, and rounding the window's corners.

use std::ffi::{CStr, CString, c_char, c_void};
use std::time::Duration;

use slint::ComponentHandle;
use slint::platform::PointerEventButton;
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::dpi::LogicalPosition;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::winit::window::{ResizeDirection, Theme, Window};

use crate::AppWindow;

/// Radius of the window's corners, logical pixels (as in app.slint).
pub const RADIUS: f64 = 10.0;

pub fn install(ui: &AppWindow) {
    // With the desktop's own menu bar there is no row of ours to draw into.
    ui.set_own_bar_possible(cfg!(target_os = "linux"));
    {
        let weak = ui.as_weak();
        ui.on_toggle_maximize(move || {
            let Some(ui) = weak.upgrade() else { return };
            let maximized = ui.window().with_winit_window(Window::is_maximized);
            ui.window().set_maximized(!maximized.unwrap_or(false));
            follow_state(&ui);
        });
    }
    {
        // Windows without a frame: their edges ask the window manager to
        // resize the window.
        let weak = ui.as_weak();
        ui.on_start_window_resize(move |edge| {
            let Some(ui) = weak.upgrade() else { return };
            let dir = match edge {
                1 => ResizeDirection::South,
                2 => ResizeDirection::SouthEast,
                3 => ResizeDirection::North,
                4 => ResizeDirection::NorthEast,
                5 => ResizeDirection::NorthWest,
                6 => ResizeDirection::West,
                7 => ResizeDirection::SouthWest,
                _ => ResizeDirection::East,
            };
            ui.window().with_winit_window(|w| {
                if let Err(e) = w.drag_resize_window(dir) {
                    log::debug!("window resize: {e}");
                }
            });
            release_mouse(&ui, PointerEventButton::Left);
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_system_window_menu(move |x, y| {
            // winit has it on Wayland, but not on X11.
            let Some(ui) = weak.upgrade() else {
                return true;
            };
            if !crate::basics::on_wayland(&ui) {
                return false;
            }
            ui.window()
                .with_winit_window(|w| w.show_window_menu(LogicalPosition::new(x, y)));
            release_mouse(&ui, PointerEventButton::Right);
            true
        });
    }
}

/// The window manager took over the mouse while `button` was down, and keeps
/// its release to itself: without one, the next press would go to the same
/// place again.
fn release_mouse(ui: &AppWindow, button: PointerEventButton) {
    use slint::platform::WindowEvent;
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        let Some(ui) = weak.upgrade() else { return };
        let position = slint::LogicalPosition::new(-1.0, -1.0);
        for event in [
            WindowEvent::PointerReleased { position, button },
            WindowEvent::PointerExited,
        ] {
            let _ = ui.window().dispatch_event_with_result(event);
        }
    });
}

/// The main window's events, for what the title bar shows.
pub fn window_event(ui: &AppWindow, event: &WindowEvent) {
    match event {
        WindowEvent::Focused(on) => {
            ui.set_window_focused(*on);
            follow_state(ui);
        }
        WindowEvent::Resized(_) => follow_state(ui),
        _ => {}
    }
}

/// Follow the window being maximized or restored, also by the desktop. Some
/// window managers say so only after the new size, so look again. The
/// desktop's own bar is asked to be dark, where it lets a window ask: Slint
/// sets it to the desktop's preference when that arrives or changes.
fn follow_state(ui: &AppWindow) {
    thread_local! {
        // Started over by every event, so a resize by hand looks only a few times.
        static LOOK: [slint::Timer; 3] = Default::default();
    }
    LOOK.with(|timers| {
        for (timer, delay) in timers.iter().zip([0, 150, 1000]) {
            let weak = ui.as_weak();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(delay),
                move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let maximized = ui.window().with_winit_window(|w| {
                        if w.theme() != Some(Theme::Dark) {
                            w.set_theme(Some(Theme::Dark));
                        }
                        w.is_maximized()
                    });
                    let maximized = maximized.unwrap_or(false);
                    if maximized != ui.get_window_maximized() {
                        ui.set_window_maximized(maximized);
                    }
                },
            );
        }
    });
}

// ---- rounded corners ----

const GL_VERSION: u32 = 0x1F02;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_CURRENT_PROGRAM: u32 = 0x8B8D;
const GL_VERTEX_ARRAY_BINDING: u32 = 0x85B5;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_BLEND: u32 = 0x0BE2;
const GL_BLEND_SRC_RGB: u32 = 0x80C9;
const GL_BLEND_DST_RGB: u32 = 0x80C8;
const GL_BLEND_SRC_ALPHA: u32 = 0x80CB;
const GL_BLEND_DST_ALPHA: u32 = 0x80CA;
const GL_CULL_FACE: u32 = 0x0B44;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_STENCIL_TEST: u32 = 0x0B90;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_COLOR_WRITEMASK: u32 = 0x0C23;
const GL_TRIANGLE_STRIP: u32 = 0x0005;
const GL_ZERO: u32 = 0;
const GL_SRC_ALPHA: u32 = 0x0302;

// One square per corner; inside it the picture is multiplied by how much of
// the pixel the rounded window covers (the window's pixels are premultiplied).
const VERTEX: &str = "
uniform vec2 u_size;
uniform float u_r;
void main() {
    vec2 c = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    vec2 k = vec2(float(gl_InstanceID & 1), float((gl_InstanceID >> 1) & 1));
    vec2 p = k * (u_size - u_r) + c * u_r;
    gl_Position = vec4(p / u_size * 2.0 - 1.0, 0.0, 1.0);
}
";
const FRAGMENT: &str = "
uniform vec2 u_size;
uniform float u_r;
out vec4 color;
void main() {
    vec2 d = max(abs(gl_FragCoord.xy - u_size * 0.5) - (u_size * 0.5 - u_r), 0.0);
    color = vec4(0.0, 0.0, 0.0, clamp(u_r - length(d) + 0.5, 0.0, 1.0));
}
";

#[derive(Clone, Copy)]
struct Gl {
    get_string: unsafe extern "C" fn(u32) -> *const c_char,
    get_integerv: unsafe extern "C" fn(u32, *mut i32),
    get_booleanv: unsafe extern "C" fn(u32, *mut u8),
    is_enabled: unsafe extern "C" fn(u32) -> u8,
    enable: unsafe extern "C" fn(u32),
    disable: unsafe extern "C" fn(u32),
    viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    color_mask: unsafe extern "C" fn(u8, u8, u8, u8),
    blend_func_separate: unsafe extern "C" fn(u32, u32, u32, u32),
    create_shader: unsafe extern "C" fn(u32) -> u32,
    shader_source: unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32),
    compile_shader: unsafe extern "C" fn(u32),
    get_shaderiv: unsafe extern "C" fn(u32, u32, *mut i32),
    delete_shader: unsafe extern "C" fn(u32),
    create_program: unsafe extern "C" fn() -> u32,
    attach_shader: unsafe extern "C" fn(u32, u32),
    link_program: unsafe extern "C" fn(u32),
    get_programiv: unsafe extern "C" fn(u32, u32, *mut i32),
    use_program: unsafe extern "C" fn(u32),
    get_uniform_location: unsafe extern "C" fn(u32, *const c_char) -> i32,
    uniform_1f: unsafe extern "C" fn(i32, f32),
    uniform_2f: unsafe extern "C" fn(i32, f32, f32),
    gen_vertex_arrays: unsafe extern "C" fn(i32, *mut u32),
    bind_vertex_array: unsafe extern "C" fn(u32),
    draw_arrays_instanced: unsafe extern "C" fn(u32, i32, i32, i32),
}

/// Cuts the window's corners off, after everything else has been drawn.
pub struct Corners {
    gl: Gl,
    program: u32,
    vao: u32,
    size: i32,
    radius: i32,
}

impl Corners {
    /// `None` when this OpenGL is too old for it: the corners stay square.
    pub fn new(get: &dyn Fn(&CStr) -> *const c_void) -> Option<Self> {
        unsafe fn f<T: Copy>(p: *const c_void) -> Option<T> {
            (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
        }
        // SAFETY: these are the GL functions of those names, and the window's
        // GL context is current.
        unsafe {
            let gl = Gl {
                get_string: f(get(c"glGetString"))?,
                get_integerv: f(get(c"glGetIntegerv"))?,
                get_booleanv: f(get(c"glGetBooleanv"))?,
                is_enabled: f(get(c"glIsEnabled"))?,
                enable: f(get(c"glEnable"))?,
                disable: f(get(c"glDisable"))?,
                viewport: f(get(c"glViewport"))?,
                color_mask: f(get(c"glColorMask"))?,
                blend_func_separate: f(get(c"glBlendFuncSeparate"))?,
                create_shader: f(get(c"glCreateShader"))?,
                shader_source: f(get(c"glShaderSource"))?,
                compile_shader: f(get(c"glCompileShader"))?,
                get_shaderiv: f(get(c"glGetShaderiv"))?,
                delete_shader: f(get(c"glDeleteShader"))?,
                create_program: f(get(c"glCreateProgram"))?,
                attach_shader: f(get(c"glAttachShader"))?,
                link_program: f(get(c"glLinkProgram"))?,
                get_programiv: f(get(c"glGetProgramiv"))?,
                use_program: f(get(c"glUseProgram"))?,
                get_uniform_location: f(get(c"glGetUniformLocation"))?,
                uniform_1f: f(get(c"glUniform1f"))?,
                uniform_2f: f(get(c"glUniform2f"))?,
                gen_vertex_arrays: f(get(c"glGenVertexArrays"))?,
                bind_vertex_array: f(get(c"glBindVertexArray"))?,
                draw_arrays_instanced: f(get(c"glDrawArraysInstanced"))?,
            };
            let version = (gl.get_string)(GL_VERSION);
            let es =
                !version.is_null() && CStr::from_ptr(version).to_bytes().starts_with(b"OpenGL ES");
            let head = if es {
                "#version 300 es\nprecision highp float;\n"
            } else {
                "#version 150\n"
            };
            let program = (gl.create_program)();
            for (kind, body) in [(GL_VERTEX_SHADER, VERTEX), (GL_FRAGMENT_SHADER, FRAGMENT)] {
                let source = CString::new(format!("{head}{body}")).ok()?;
                let shader = (gl.create_shader)(kind);
                (gl.shader_source)(shader, 1, &source.as_ptr(), std::ptr::null());
                (gl.compile_shader)(shader);
                let mut ok = 0;
                (gl.get_shaderiv)(shader, GL_COMPILE_STATUS, &mut ok);
                (gl.attach_shader)(program, shader);
                (gl.delete_shader)(shader);
                if ok == 0 {
                    log::info!("rounded window corners: the shader doesn't compile here");
                    return None;
                }
            }
            (gl.link_program)(program);
            let mut ok = 0;
            (gl.get_programiv)(program, GL_LINK_STATUS, &mut ok);
            if ok == 0 {
                log::info!("rounded window corners: the shader doesn't link here");
                return None;
            }
            let mut vao = 0;
            (gl.gen_vertex_arrays)(1, &mut vao);
            Some(Self {
                gl,
                program,
                vao,
                size: (gl.get_uniform_location)(program, c"u_size".as_ptr()),
                radius: (gl.get_uniform_location)(program, c"u_r".as_ptr()),
            })
        }
    }

    /// Round the corners of a `w`×`h` window by `r` pixels. The GL state is
    /// left as it was.
    pub fn draw(&self, w: i32, h: i32, r: f32) {
        let gl = &self.gl;
        let int = |name| {
            let mut v = 0i32;
            // SAFETY: these names hold one integer.
            unsafe { (gl.get_integerv)(name, &mut v) };
            v as u32
        };
        // SAFETY: the window's GL context is current.
        unsafe {
            let (program, vao) = (int(GL_CURRENT_PROGRAM), int(GL_VERTEX_ARRAY_BINDING));
            let blend = [
                GL_BLEND_SRC_RGB,
                GL_BLEND_DST_RGB,
                GL_BLEND_SRC_ALPHA,
                GL_BLEND_DST_ALPHA,
            ]
            .map(int);
            let mut view = [0i32; 4];
            (gl.get_integerv)(GL_VIEWPORT, view.as_mut_ptr());
            let mut mask = [1u8; 4];
            (gl.get_booleanv)(GL_COLOR_WRITEMASK, mask.as_mut_ptr());
            let off = [
                GL_CULL_FACE,
                GL_DEPTH_TEST,
                GL_STENCIL_TEST,
                GL_SCISSOR_TEST,
            ];
            let was = off.map(|c| (gl.is_enabled)(c) != 0);
            let blended = (gl.is_enabled)(GL_BLEND) != 0;

            off.iter().for_each(|c| (gl.disable)(*c));
            (gl.enable)(GL_BLEND);
            (gl.blend_func_separate)(GL_ZERO, GL_SRC_ALPHA, GL_ZERO, GL_SRC_ALPHA);
            (gl.color_mask)(1, 1, 1, 1);
            (gl.viewport)(0, 0, w, h);
            (gl.use_program)(self.program);
            (gl.bind_vertex_array)(self.vao);
            (gl.uniform_2f)(self.size, w as f32, h as f32);
            (gl.uniform_1f)(self.radius, r);
            (gl.draw_arrays_instanced)(GL_TRIANGLE_STRIP, 0, 4, 4);

            (gl.bind_vertex_array)(vao);
            (gl.use_program)(program);
            (gl.viewport)(view[0], view[1], view[2], view[3]);
            (gl.color_mask)(mask[0], mask[1], mask[2], mask[3]);
            (gl.blend_func_separate)(blend[0], blend[1], blend[2], blend[3]);
            if !blended {
                (gl.disable)(GL_BLEND);
            }
            off.iter()
                .zip(was)
                .filter(|(_, on)| *on)
                .for_each(|(c, _)| (gl.enable)(*c));
        }
    }
}
