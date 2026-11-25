//! Minimal FFI bindings to the libmpv client API (`client.h`) and render API
//! (`render.h`, `render_gl.h`).
//!
//! The library is loaded at runtime with [`libloading`] instead of being linked
//! at build time. This lets the app build without libmpv installed, and lets the
//! packaged app pick up the copy of libmpv that ships next to the executable.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Types from client.h
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct mpv_handle {
    _private: [u8; 0],
}

pub type mpv_format = c_int;
pub const MPV_FORMAT_NONE: mpv_format = 0;
pub const MPV_FORMAT_STRING: mpv_format = 1;
pub const MPV_FORMAT_OSD_STRING: mpv_format = 2;
pub const MPV_FORMAT_FLAG: mpv_format = 3;
pub const MPV_FORMAT_INT64: mpv_format = 4;
pub const MPV_FORMAT_DOUBLE: mpv_format = 5;
pub const MPV_FORMAT_NODE: mpv_format = 6;
pub const MPV_FORMAT_NODE_ARRAY: mpv_format = 7;
pub const MPV_FORMAT_NODE_MAP: mpv_format = 8;
pub const MPV_FORMAT_BYTE_ARRAY: mpv_format = 9;

#[repr(C)]
#[derive(Clone, Copy)]
pub union mpv_node_u {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: c_double,
    pub list: *mut mpv_node_list,
    pub ba: *mut mpv_byte_array,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_node {
    pub u: mpv_node_u,
    pub format: mpv_format,
}

#[repr(C)]
pub struct mpv_node_list {
    pub num: c_int,
    pub values: *mut mpv_node,
    pub keys: *mut *mut c_char,
}

#[repr(C)]
pub struct mpv_byte_array {
    pub data: *mut c_void,
    pub size: usize,
}

pub type mpv_event_id = c_int;
pub const MPV_EVENT_NONE: mpv_event_id = 0;
pub const MPV_EVENT_SHUTDOWN: mpv_event_id = 1;
pub const MPV_EVENT_LOG_MESSAGE: mpv_event_id = 2;
pub const MPV_EVENT_GET_PROPERTY_REPLY: mpv_event_id = 3;
pub const MPV_EVENT_SET_PROPERTY_REPLY: mpv_event_id = 4;
pub const MPV_EVENT_COMMAND_REPLY: mpv_event_id = 5;
pub const MPV_EVENT_START_FILE: mpv_event_id = 6;
pub const MPV_EVENT_END_FILE: mpv_event_id = 7;
pub const MPV_EVENT_FILE_LOADED: mpv_event_id = 8;
pub const MPV_EVENT_IDLE: mpv_event_id = 11;
pub const MPV_EVENT_TICK: mpv_event_id = 14;
pub const MPV_EVENT_CLIENT_MESSAGE: mpv_event_id = 16;
pub const MPV_EVENT_VIDEO_RECONFIG: mpv_event_id = 17;
pub const MPV_EVENT_AUDIO_RECONFIG: mpv_event_id = 18;
pub const MPV_EVENT_SEEK: mpv_event_id = 20;
pub const MPV_EVENT_PLAYBACK_RESTART: mpv_event_id = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: mpv_event_id = 22;
pub const MPV_EVENT_QUEUE_OVERFLOW: mpv_event_id = 24;
pub const MPV_EVENT_HOOK: mpv_event_id = 25;

#[repr(C)]
pub struct mpv_event {
    pub event_id: mpv_event_id,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_property {
    pub name: *const c_char,
    pub format: mpv_format,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_log_message {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

pub type mpv_end_file_reason = c_int;
pub const MPV_END_FILE_REASON_EOF: mpv_end_file_reason = 0;
pub const MPV_END_FILE_REASON_STOP: mpv_end_file_reason = 2;
pub const MPV_END_FILE_REASON_QUIT: mpv_end_file_reason = 3;
pub const MPV_END_FILE_REASON_ERROR: mpv_end_file_reason = 4;
pub const MPV_END_FILE_REASON_REDIRECT: mpv_end_file_reason = 5;

#[repr(C)]
pub struct mpv_event_end_file {
    pub reason: mpv_end_file_reason,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

#[repr(C)]
pub struct mpv_event_start_file {
    pub playlist_entry_id: i64,
}

// ---------------------------------------------------------------------------
// Types from stream_cb.h
// ---------------------------------------------------------------------------

pub type mpv_stream_cb_read_fn = unsafe extern "C" fn(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64;
pub type mpv_stream_cb_seek_fn = unsafe extern "C" fn(cookie: *mut c_void, offset: i64) -> i64;
pub type mpv_stream_cb_size_fn = unsafe extern "C" fn(cookie: *mut c_void) -> i64;
pub type mpv_stream_cb_close_fn = unsafe extern "C" fn(cookie: *mut c_void);
pub type mpv_stream_cb_cancel_fn = unsafe extern "C" fn(cookie: *mut c_void);

#[repr(C)]
pub struct mpv_stream_cb_info {
    pub cookie: *mut c_void,
    pub read_fn: Option<mpv_stream_cb_read_fn>,
    pub seek_fn: Option<mpv_stream_cb_seek_fn>,
    pub size_fn: Option<mpv_stream_cb_size_fn>,
    pub close_fn: Option<mpv_stream_cb_close_fn>,
    pub cancel_fn: Option<mpv_stream_cb_cancel_fn>,
}

pub type mpv_stream_cb_open_ro_fn =
    unsafe extern "C" fn(user_data: *mut c_void, uri: *mut c_char, info: *mut mpv_stream_cb_info) -> c_int;

pub const MPV_ERROR_LOADING_FAILED: c_int = -13;

// ---------------------------------------------------------------------------
// Types from render.h / render_gl.h
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct mpv_render_context {
    _private: [u8; 0],
}

pub type mpv_render_param_type = c_int;
pub const MPV_RENDER_PARAM_INVALID: mpv_render_param_type = 0;
pub const MPV_RENDER_PARAM_API_TYPE: mpv_render_param_type = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: mpv_render_param_type = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: mpv_render_param_type = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: mpv_render_param_type = 4;
pub const MPV_RENDER_PARAM_DEPTH: mpv_render_param_type = 5;
pub const MPV_RENDER_PARAM_X11_DISPLAY: mpv_render_param_type = 8;
pub const MPV_RENDER_PARAM_WL_DISPLAY: mpv_render_param_type = 9;
pub const MPV_RENDER_PARAM_ADVANCED_CONTROL: mpv_render_param_type = 10;
pub const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: mpv_render_param_type = 12;

pub const MPV_RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
pub struct mpv_render_param {
    pub type_: mpv_render_param_type,
    pub data: *mut c_void,
}

pub type GetProcAddressFn =
    unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void;

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address: Option<GetProcAddressFn>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

pub type mpv_render_update_fn = unsafe extern "C" fn(cb_ctx: *mut c_void);

// ---------------------------------------------------------------------------
// Runtime-loaded function table
// ---------------------------------------------------------------------------

macro_rules! mpv_functions {
    ($( $name:ident : fn($($arg:ty),*) $(-> $ret:ty)?; )*) => {
        /// Function table for one loaded copy of libmpv.
        pub struct Mpv {
            _lib: libloading::Library,
            /// Path the library was loaded from (for diagnostics).
            pub path: PathBuf,
            $( pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
        }

        impl Mpv {
            unsafe fn from_library(lib: libloading::Library, path: PathBuf) -> Result<Self, LoadError> {
                $(
                    let $name = unsafe {
                        *lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                            concat!(stringify!($name), "\0").as_bytes(),
                        )
                        .map_err(|e| LoadError::MissingSymbol(stringify!($name), e.to_string()))?
                    };
                )*
                Ok(Self { _lib: lib, path, $($name,)* })
            }
        }
    };
}

mpv_functions! {
    mpv_client_api_version: fn() -> std::ffi::c_ulong;
    mpv_error_string: fn(c_int) -> *const c_char;
    mpv_free: fn(*mut c_void);
    mpv_create: fn() -> *mut mpv_handle;
    mpv_initialize: fn(*mut mpv_handle) -> c_int;
    mpv_terminate_destroy: fn(*mut mpv_handle);
    mpv_set_option_string: fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int;
    mpv_command: fn(*mut mpv_handle, *mut *const c_char) -> c_int;
    mpv_command_async: fn(*mut mpv_handle, u64, *mut *const c_char) -> c_int;
    mpv_command_ret: fn(*mut mpv_handle, *mut *const c_char, *mut mpv_node) -> c_int;
    mpv_set_property: fn(*mut mpv_handle, *const c_char, mpv_format, *mut c_void) -> c_int;
    mpv_set_property_string: fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int;
    mpv_get_property: fn(*mut mpv_handle, *const c_char, mpv_format, *mut c_void) -> c_int;
    mpv_get_property_string: fn(*mut mpv_handle, *const c_char) -> *mut c_char;
    mpv_observe_property: fn(*mut mpv_handle, u64, *const c_char, mpv_format) -> c_int;
    mpv_request_log_messages: fn(*mut mpv_handle, *const c_char) -> c_int;
    mpv_wait_event: fn(*mut mpv_handle, c_double) -> *mut mpv_event;
    mpv_wakeup: fn(*mut mpv_handle);
    mpv_free_node_contents: fn(*mut mpv_node);
    mpv_stream_cb_add_ro: fn(*mut mpv_handle, *const c_char, *mut c_void, Option<mpv_stream_cb_open_ro_fn>) -> c_int;
    mpv_render_context_create: fn(*mut *mut mpv_render_context, *mut mpv_handle, *mut mpv_render_param) -> c_int;
    mpv_render_context_set_update_callback: fn(*mut mpv_render_context, Option<mpv_render_update_fn>, *mut c_void);
    mpv_render_context_update: fn(*mut mpv_render_context) -> u64;
    mpv_render_context_render: fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int;
    mpv_render_context_report_swap: fn(*mut mpv_render_context);
    mpv_render_context_free: fn(*mut mpv_render_context);
}

// SAFETY: the table only holds function pointers and the library handle;
// libmpv's API is thread-safe per its documentation.
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

#[derive(Debug)]
pub enum LoadError {
    /// No candidate library could be opened. Holds every path tried with its error.
    NotFound(Vec<(PathBuf, String)>),
    MissingSymbol(&'static str, String),
    /// The library's client API major version is not the one these bindings target.
    UnsupportedVersion(u64),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound(tried) => {
                writeln!(f, "libmpv could not be loaded. Tried:")?;
                for (path, err) in tried {
                    writeln!(f, "  {}: {}", path.display(), err)?;
                }
                write!(f, "Install libmpv or set SMP_LIBMPV to its path.")
            }
            LoadError::MissingSymbol(name, e) => write!(f, "libmpv is missing symbol {name}: {e}"),
            LoadError::UnsupportedVersion(v) => {
                write!(
                    f,
                    "libmpv client API {}.{} is not supported (need 2.x)",
                    v >> 16,
                    v & 0xffff
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Library file names to try, per platform, in order.
fn library_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
    } else if cfg!(target_os = "macos") {
        &["libmpv.2.dylib", "libmpv.dylib"]
    } else {
        &["libmpv.so.2", "libmpv.so"]
    }
}

/// Paths to try: `$SMP_LIBMPV`, then next to the executable (and in
/// `lib/` beside it), then the bare names, which the OS loader resolves.
fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Empty counts as unset: loading "" would open the executable itself.
    if let Some(p) = std::env::var_os("SMP_LIBMPV").filter(|p| !p.is_empty()) {
        out.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        for name in library_names() {
            out.push(dir.join(name));
            out.push(dir.join("lib").join(name));
        }
    }
    out.extend(library_names().iter().map(PathBuf::from));
    out
}

/// Load libmpv once per process and return the shared function table.
pub fn load() -> Result<&'static Mpv, &'static LoadError> {
    static MPV: OnceLock<Result<Mpv, LoadError>> = OnceLock::new();
    MPV.get_or_init(load_uncached).as_ref()
}

fn load_uncached() -> Result<Mpv, LoadError> {
    let mut tried = Vec::new();
    for path in candidate_paths() {
        // Skip absolute paths that don't exist to keep the error list short.
        if path.is_absolute() && !path.exists() {
            continue;
        }
        // SAFETY: loading libmpv runs its library constructors, which is expected.
        match unsafe { libloading::Library::new(&path) } {
            Ok(lib) => {
                let mpv = unsafe { Mpv::from_library(lib, path)? };
                // c_ulong is 32-bit on Windows, 64-bit elsewhere.
                #[allow(clippy::useless_conversion)]
                let version = u64::from(unsafe { (mpv.mpv_client_api_version)() });
                if version >> 16 != 2 {
                    return Err(LoadError::UnsupportedVersion(version));
                }
                return Ok(mpv);
            }
            Err(e) => tried.push((path, e.to_string())),
        }
    }
    Err(LoadError::NotFound(tried))
}
