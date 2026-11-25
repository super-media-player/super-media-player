//! Streams the app makes itself: `protocol://…` links that mpv reads through
//! a callback (mpv's `stream_cb` API).

use std::ffi::{CStr, c_char, c_int, c_void};

use mpv_sys::*;

use crate::{Player, Result, cstring};

/// One open stream. Reads happen on an mpv thread, one at a time.
pub trait StreamSource: Send {
    /// Fill the start of `buf`; the number of bytes, 0 at the end.
    fn read(&mut self, buf: &mut [u8]) -> usize;
}

type OpenFn = dyn Fn(&str) -> Option<Box<dyn StreamSource>> + Send + Sync;

impl Player {
    /// Answer `protocol://` links: `open` gets the whole link and returns its
    /// stream, which can't seek. Once per protocol and player.
    pub fn add_protocol(
        &self,
        protocol: &str,
        open: impl Fn(&str) -> Option<Box<dyn StreamSource>> + Send + Sync + 'static,
    ) -> Result<()> {
        let name = cstring(protocol)?;
        // mpv may call it for as long as the player lives, and there is no
        // way to take it back: it is never freed.
        let open: Box<Box<OpenFn>> = Box::new(Box::new(open));
        let inner = self.inner();
        let rc = unsafe {
            (inner.api.mpv_stream_cb_add_ro)(
                inner.handle,
                name.as_ptr(),
                Box::into_raw(open).cast(),
                Some(open_trampoline),
            )
        };
        inner.check(rc, || format!("protocol {protocol}"))
    }
}

unsafe extern "C" fn open_trampoline(
    user_data: *mut c_void,
    uri: *mut c_char,
    info: *mut mpv_stream_cb_info,
) -> c_int {
    let open = unsafe { &*(user_data as *const Box<OpenFn>) };
    let uri = unsafe { CStr::from_ptr(uri) }.to_string_lossy();
    let source = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| open(&uri)));
    let Ok(Some(source)) = source else { return MPV_ERROR_LOADING_FAILED };
    let cookie: Box<Box<dyn StreamSource>> = Box::new(source);
    let info = unsafe { &mut *info };
    info.cookie = Box::into_raw(cookie).cast();
    info.read_fn = Some(read_trampoline);
    info.close_fn = Some(close_trampoline);
    0
}

unsafe extern "C" fn read_trampoline(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64 {
    let source = unsafe { &mut *(cookie as *mut Box<dyn StreamSource>) };
    let buf = unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), nbytes as usize) };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source.read(buf))).map_or(-1, |n| n as i64)
}

unsafe extern "C" fn close_trampoline(cookie: *mut c_void) {
    drop(unsafe { Box::from_raw(cookie as *mut Box<dyn StreamSource>) });
}
