//! The DVD's own program, run by libdvdnav (loaded at runtime, like libmpv),
//! and the video stream it plays, for mpv to read.
//!
//! Only menus and short clips come through here. When the disc wants to play
//! a title, the stream stops with [`Halt::Title`] and mpv's own DVD support
//! plays it, with seeking, chapters and tracks; when that title ends,
//! [`Machine::title_done`] lets the disc's program go on after it.
//!
//! Times are rewritten so that they never jump back when a menu loops.

use std::collections::{HashMap, VecDeque};
use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use player_core::StreamSource;

use super::spu::{self, Button, Highlight, NavPack, Subpicture};

const BLOCK: usize = 2048;
/// Titles shorter than this (logos, warnings) play as part of the menus, so
/// that the disc's program simply goes on after them.
const SHORT_TITLE: i64 = 60 * 90_000;
/// A title that starts with buttons on it is a menu in disguise, up to this.
const INTERACTIVE_TITLE: i64 = 20 * 60 * 90_000;
/// The stream's first moment (some streams start before it).
const START: i64 = 90_000;
/// Give up looking for the end of a title after this much.
const MAX_DRAIN: u64 = 300 << 20;

const DVDNAV_STATUS_OK: i32 = 1;
const EV_BLOCK: i32 = 0;
const EV_STILL: i32 = 2;
const EV_SPU_STREAM: i32 = 3;
const EV_AUDIO_STREAM: i32 = 4;
const EV_CELL: i32 = 6;
const EV_NAV: i32 = 7;
const EV_STOP: i32 = 8;
const EV_CLUT: i32 = 10;
const EV_HOP: i32 = 12;
const EV_WAIT: i32 = 13;

/// A menu the disc may have.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Menu {
    /// The whole disc's.
    Title = 2,
    /// The current title set's (chapters, languages).
    Root = 3,
}

type Nav = *mut c_void;

macro_rules! nav_functions {
    ($( $name:ident : fn($($arg:ty),*) $(-> $ret:ty)?; )*) => {
        #[allow(non_snake_case)]
        struct Lib {
            _lib: libloading::Library,
            $( $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
        }

        #[allow(non_snake_case)]
        impl Lib {
            unsafe fn from_library(lib: libloading::Library) -> Option<Self> {
                $(
                    let $name = unsafe {
                        *lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                            concat!(stringify!($name), "\0").as_bytes(),
                        ).ok()?
                    };
                )*
                Some(Self { _lib: lib, $($name,)* })
            }
        }
    };
}

nav_functions! {
    dvdnav_open: fn(*mut Nav, *const c_char) -> i32;
    dvdnav_close: fn(Nav) -> i32;
    dvdnav_err_to_string: fn(Nav) -> *const c_char;
    dvdnav_set_readahead_flag: fn(Nav, i32) -> i32;
    dvdnav_set_PGC_positioning_flag: fn(Nav, i32) -> i32;
    dvdnav_set_region_mask: fn(Nav, i32) -> i32;
    dvdnav_menu_language_select: fn(Nav, *mut c_char) -> i32;
    dvdnav_audio_language_select: fn(Nav, *mut c_char) -> i32;
    dvdnav_spu_language_select: fn(Nav, *mut c_char) -> i32;
    dvdnav_get_next_block: fn(Nav, *mut u8, *mut i32, *mut i32) -> i32;
    dvdnav_still_skip: fn(Nav) -> i32;
    dvdnav_wait_skip: fn(Nav) -> i32;
    dvdnav_menu_call: fn(Nav, c_int) -> i32;
    dvdnav_title_play: fn(Nav, i32) -> i32;
    dvdnav_current_title_info: fn(Nav, *mut i32, *mut i32) -> i32;
    dvdnav_get_position: fn(Nav, *mut u32, *mut u32) -> i32;
    dvdnav_sector_search: fn(Nav, i64, i32) -> i32;
    dvdnav_next_pg_search: fn(Nav) -> i32;
    dvdnav_prev_pg_search: fn(Nav) -> i32;
    dvdnav_get_current_highlight: fn(Nav, *mut i32) -> i32;
    dvdnav_get_current_nav_pci: fn(Nav) -> *const u8;
    dvdnav_upper_button_select: fn(Nav, *const u8) -> i32;
    dvdnav_lower_button_select: fn(Nav, *const u8) -> i32;
    dvdnav_left_button_select: fn(Nav, *const u8) -> i32;
    dvdnav_right_button_select: fn(Nav, *const u8) -> i32;
    dvdnav_button_select: fn(Nav, *const u8, i32) -> i32;
    dvdnav_button_activate: fn(Nav, *const u8) -> i32;
    dvdnav_button_select_and_activate: fn(Nav, *const u8, i32) -> i32;
    dvdnav_get_video_aspect: fn(Nav) -> u8;
    dvdnav_is_domain_vts: fn(Nav) -> i8;
    dvdnav_get_angle_info: fn(Nav, *mut i32, *mut i32) -> i32;
    dvdnav_get_title_string: fn(Nav, *mut *const c_char) -> i32;
    // libdvdread, which libdvdnav depends on.
    DVDOpen: fn(*const c_char) -> *mut c_void;
    DVDClose: fn(*mut c_void);
    DVDOpenFile: fn(*mut c_void, c_int, c_int) -> *mut c_void;
    DVDCloseFile: fn(*mut c_void);
    DVDReadBytes: fn(*mut c_void, *mut c_void, usize) -> isize;
}

fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = crate::disc::open_bundled_library(&[
            "libdvdnav.so.4",
            "libdvdnav-4.dll",
            "libdvdnav.4.dylib",
        ])?;
        // SAFETY: the signatures are those of dvdnav.h and dvd_reader.h.
        let lib = unsafe { Lib::from_library(lib) };
        if lib.is_none() {
            log::warn!("libdvdnav is too old for DVD menus");
        }
        lib
    })
    .as_ref()
}

/// Whether DVD menus can work on this computer.
pub fn available() -> bool {
    lib().is_some()
}

/// Regions the disc may be played in (bit 0 is region 1), from its
/// VIDEO_TS.IFO: some discs refuse to go on in any other.
fn regions(lib: &Lib, path: &CString) -> u8 {
    let mut head = [0u8; 64];
    let mut got = 0;
    // SAFETY: plain libdvdread calls, each result checked before use.
    unsafe {
        let dvd = (lib.DVDOpen)(path.as_ptr());
        if dvd.is_null() {
            return 0xFF;
        }
        let file = (lib.DVDOpenFile)(dvd, 0, 0);
        if !file.is_null() {
            got = (lib.DVDReadBytes)(file, head.as_mut_ptr().cast(), head.len());
            (lib.DVDCloseFile)(file);
        }
        (lib.DVDClose)(dvd);
    }
    if got == head.len() as isize && head.starts_with(b"DVDVIDEO-VMG") {
        // The prohibited ones are set.
        !head[0x23]
    } else {
        0xFF
    }
}

/// A title the disc wants played.
#[derive(Clone, PartialEq, Debug)]
pub struct TitleStart {
    /// From 1, as on the disc.
    pub title: i32,
    pub chapter: i32,
    /// The chosen sound and subtitle streams (MPEG stream numbers), where
    /// the disc says; subtitles `None`: off.
    pub audio: Option<i32>,
    pub subtitle: Option<i32>,
    pub angle: i32,
}

/// Why the stream ended.
#[derive(Clone, PartialEq, Debug)]
pub enum Halt {
    /// A still picture: for this many seconds, or until a button is pressed.
    Still(Option<u32>),
    Title(TitleStart),
    /// The disc's program ended.
    End,
}

/// What an action led to.
#[derive(Clone, PartialEq, Debug)]
pub enum Outcome {
    /// The stream goes on.
    Same,
    /// It is a new stream now: mpv has to open it again.
    Reload,
    Title(TitleStart),
    End,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    /// Choose button n (from 1).
    Select(i32),
    /// Press the chosen button, or button n.
    Press(Option<i32>),
    Menu(Menu),
    /// Go on after a still picture.
    StillOver,
    /// To the next part, or out of a still picture.
    Skip,
    Back,
    /// The title mpv played has ended.
    TitleDone,
}

/// The buttons from a moment of the stream on.
#[derive(Clone)]
pub struct Moment {
    /// Stream time, 90 kHz.
    pub pts: i64,
    pub buttons: Arc<Vec<Button>>,
    /// libdvdnav's own copy of them, to give back when one is used.
    pci: Arc<[u64; spu::PCI_SIZE.div_ceil(8)]>,
}

#[derive(Default)]
struct Assembly {
    pts: i64,
    data: Vec<u8>,
}

/// Reading on to the end of a title that mpv has played.
struct Drain {
    title: i32,
    cell_start: Option<i64>,
    read: u64,
}

enum Step {
    Block(Box<[u8; BLOCK]>),
    /// The program jumped (a button was pressed).
    Hop,
    Halt(Halt),
}

pub struct Machine {
    lib: &'static Lib,
    nav: Nav,
    /// The stream mpv should be reading; older ones just end.
    pub generation: u64,
    queue: VecDeque<Box<[u8; BLOCK]>>,
    pub halt: Option<Halt>,
    /// A jump was seen by the reader itself.
    pub hopped: bool,
    sent: u64,
    errors: u32,
    /// Added to the disc's times.
    shift: Option<i64>,
    last_end: i64,
    new_cell: bool,
    pgc_length: i64,
    audio: Option<i32>,
    subtitle: Option<i32>,
    spu_stream: i32,
    pub clut: [u32; 16],
    pub moments: VecDeque<Moment>,
    pub pictures: VecDeque<(u8, Subpicture)>,
    assembling: HashMap<u8, Assembly>,
    pub button: i32,
    handed_over: Option<i32>,
    drain: Option<Drain>,
}

// SAFETY: libdvdnav handles may be used from any thread, one at a time,
// which the Mutex around every Machine sees to.
unsafe impl Send for Machine {}

impl Drop for Machine {
    fn drop(&mut self) {
        unsafe { (self.lib.dvdnav_close)(self.nav) };
    }
}

impl Machine {
    /// Open a drive, image or disc folder. Blocks while the drive reads.
    pub fn open(device: &str, language: &str) -> Result<Self, String> {
        let lib = lib().ok_or("libdvdnav is missing")?;
        let path = CString::new(device).map_err(|_| "bad path")?;
        let allowed = regions(lib, &path);
        let mut nav: Nav = std::ptr::null_mut();
        // SAFETY: `nav` is only used when the open succeeded.
        unsafe {
            if (lib.dvdnav_open)(&mut nav, path.as_ptr()) != DVDNAV_STATUS_OK || nav.is_null() {
                return Err("libdvdnav couldn't open the disc".into());
            }
            (lib.dvdnav_set_readahead_flag)(nav, 0);
            (lib.dvdnav_set_PGC_positioning_flag)(nav, 1);
            let region = 1 << allowed.trailing_zeros().min(7);
            (lib.dvdnav_set_region_mask)(nav, region);
            for f in [
                lib.dvdnav_menu_language_select,
                lib.dvdnav_audio_language_select,
                lib.dvdnav_spu_language_select,
            ] {
                let mut code = [0u8; 3];
                code[..2].copy_from_slice(&language.as_bytes()[..2]);
                f(nav, code.as_mut_ptr().cast());
            }
        }
        Ok(Self {
            lib,
            nav,
            generation: 1,
            queue: VecDeque::new(),
            halt: None,
            hopped: false,
            sent: 0,
            errors: 0,
            shift: None,
            last_end: START,
            new_cell: false,
            pgc_length: 0,
            audio: None,
            subtitle: None,
            spu_stream: 0,
            clut: [0x0010_8080; 16],
            moments: VecDeque::new(),
            pictures: VecDeque::new(),
            assembling: HashMap::new(),
            button: 0,
            handed_over: None,
            drain: None,
        })
    }

    /// The disc's name, if it has one.
    pub fn name(&self) -> String {
        let mut p: *const c_char = std::ptr::null();
        // SAFETY: the string belongs to the handle and is copied at once.
        let ok =
            unsafe { (self.lib.dvdnav_get_title_string)(self.nav, &mut p) } == DVDNAV_STATUS_OK;
        if !ok || p.is_null() {
            return String::new();
        }
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .replace(['_', ','], " ")
            .trim()
            .to_owned()
    }

    /// Stand at title `title`, so that its title set's menu can be called.
    pub fn enter_title(&mut self, title: i32) {
        unsafe { (self.lib.dvdnav_title_play)(self.nav, title) };
    }

    fn error(&self) -> String {
        // SAFETY: the string belongs to the handle and is copied at once.
        let p = unsafe { (self.lib.dvdnav_err_to_string)(self.nav) };
        if p.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned()
        }
    }

    fn in_title(&self) -> bool {
        unsafe { (self.lib.dvdnav_is_domain_vts)(self.nav) != 0 }
    }

    fn title(&self) -> (i32, i32) {
        let (mut t, mut p) = (0, 0);
        unsafe { (self.lib.dvdnav_current_title_info)(self.nav, &mut t, &mut p) };
        (t, p)
    }

    /// Run the program to its next block of video. `report_hop`: stop at a
    /// jump; otherwise it is only noted.
    fn step(&mut self, report_hop: bool) -> Step {
        let mut buf = Box::new([0u8; BLOCK]);
        loop {
            let (mut event, mut len) = (0, 0);
            // SAFETY: the buffer holds a whole block, the largest event too.
            let rc = unsafe {
                (self.lib.dvdnav_get_next_block)(self.nav, buf.as_mut_ptr(), &mut event, &mut len)
            };
            if rc != DVDNAV_STATUS_OK {
                self.errors += 1;
                log::debug!("dvdnav: {}", self.error());
                // A scratch: go on after it. Too many: stop.
                if self.errors > 20
                    || unsafe { (self.lib.dvdnav_next_pg_search)(self.nav) } != DVDNAV_STATUS_OK
                {
                    return Step::Halt(Halt::End);
                }
                continue;
            }
            match event {
                EV_BLOCK | EV_NAV => {
                    self.errors = 0;
                    if let Some(d) = self.drain.as_mut() {
                        d.read += BLOCK as u64;
                        if d.read > MAX_DRAIN {
                            log::warn!(
                                "DVD: no end of title {} in sight; calling its menu",
                                d.title
                            );
                            self.drain = None;
                            self.call_menu(Menu::Root);
                        }
                        continue;
                    }
                    if event == EV_NAV
                        && std::mem::take(&mut self.new_cell)
                        && let Some(t) = self.wants_title(&buf[..])
                    {
                        self.handed_over = Some(t.title);
                        self.queue.push_back(buf);
                        return Step::Halt(Halt::Title(t));
                    }
                    return Step::Block(buf);
                }
                EV_STILL => {
                    if self.drain.is_some() {
                        unsafe { (self.lib.dvdnav_still_skip)(self.nav) };
                        continue;
                    }
                    let secs = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
                    return Step::Halt(Halt::Still(
                        (0..0xff).contains(&secs).then_some(secs as u32),
                    ));
                }
                EV_WAIT => {
                    unsafe { (self.lib.dvdnav_wait_skip)(self.nav) };
                }
                EV_CLUT => {
                    for (i, c) in self.clut.iter_mut().enumerate() {
                        *c = u32::from_ne_bytes(
                            buf[i * 4..i * 4 + 4].try_into().unwrap_or_default(),
                        );
                    }
                }
                EV_SPU_STREAM => {
                    let n = |i: usize| {
                        i32::from_ne_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap_or_default())
                    };
                    let wide = unsafe { (self.lib.dvdnav_get_video_aspect)(self.nav) } == 3;
                    let stream = if wide { n(0) } else { n(1) };
                    self.spu_stream = stream & 0x1f;
                    self.subtitle = (stream & 0x80 == 0 && stream >= 0).then_some(stream & 0x1f);
                }
                EV_AUDIO_STREAM => {
                    let n = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
                    self.audio = (0..8).contains(&n).then_some(n);
                }
                EV_CELL => {
                    let n = |at: usize| {
                        i64::from_ne_bytes(buf[at..at + 8].try_into().unwrap_or_default())
                    };
                    self.pgc_length = n(24);
                    self.new_cell = true;
                    let (in_title, (title, _)) = (self.in_title(), self.title());
                    if let Some(d) = self.drain.as_mut() {
                        let start = n(32);
                        let left = !in_title
                            || title != d.title
                            || d.cell_start.is_some_and(|s| start <= s);
                        d.cell_start = Some(start);
                        if left {
                            self.drain = None;
                        }
                    }
                }
                EV_HOP => {
                    if report_hop && self.sent > 0 {
                        return Step::Hop;
                    }
                    self.hopped = true;
                }
                EV_STOP => return Step::Halt(Halt::End),
                _ => {}
            }
        }
    }

    /// The title that starts with this navigation pack, if mpv should play it.
    fn wants_title(&self, block: &[u8]) -> Option<TitleStart> {
        if !self.in_title() || self.pgc_length < SHORT_TITLE {
            return None;
        }
        let (title, chapter) = self.title();
        if title < 1 {
            return None;
        }
        let buttons =
            spu::parse_nav(block).is_some_and(|n| matches!(n.highlight, Highlight::Buttons(_)));
        if buttons && self.pgc_length < INTERACTIVE_TITLE {
            return None;
        }
        let (mut angle, mut angles) = (1, 1);
        unsafe { (self.lib.dvdnav_get_angle_info)(self.nav, &mut angle, &mut angles) };
        Some(TitleStart {
            title,
            chapter: chapter.max(1),
            audio: self.audio,
            subtitle: self.subtitle,
            angle,
        })
    }

    fn call_menu(&mut self, menu: Menu) -> bool {
        unsafe { (self.lib.dvdnav_menu_call)(self.nav, menu as c_int) == DVDNAV_STATUS_OK }
    }

    /// Forget the stream so far; the next one starts at time 0.
    fn new_stream(&mut self) {
        self.generation += 1;
        self.sent = 0;
        self.halt = None;
        self.hopped = false;
        self.shift = None;
        self.last_end = START;
        self.moments.clear();
        self.pictures.clear();
        self.assembling.clear();
    }

    /// Run to the first block after an action, so that mpv never opens a
    /// stream that turns out to be empty.
    pub fn advance(&mut self) -> Outcome {
        let was_halted = self.halt.take();
        match self.step(false) {
            Step::Block(b) => {
                let fresh = self.hopped || was_halted.is_some();
                if fresh {
                    self.queue.clear();
                }
                self.queue.push_back(b);
                if fresh {
                    self.new_stream();
                    return Outcome::Reload;
                }
                Outcome::Same
            }
            Step::Halt(Halt::Title(t)) => {
                self.halt = Some(Halt::Title(t.clone()));
                Outcome::Title(t)
            }
            Step::Halt(Halt::End) => {
                self.halt = Some(Halt::End);
                Outcome::End
            }
            // Still the same still picture (the button did something else).
            Step::Halt(h) => {
                self.halt = Some(h);
                Outcome::Same
            }
            Step::Hop => Outcome::Reload,
        }
    }

    /// `pci`: the buttons on screen at the time.
    pub fn act(&mut self, action: Action, on_screen: Option<&Moment>) -> Outcome {
        let pci = on_screen.map(|m| m.pci.as_ptr().cast::<u8>());
        let (lib, nav) = (self.lib, self.nav);
        // SAFETY: `pci` points to a whole pci_t, kept alive by `on_screen`.
        let done = unsafe {
            match (action, pci) {
                (Action::Up, Some(p)) => (lib.dvdnav_upper_button_select)(nav, p),
                (Action::Down, Some(p)) => (lib.dvdnav_lower_button_select)(nav, p),
                (Action::Left, Some(p)) => (lib.dvdnav_left_button_select)(nav, p),
                (Action::Right, Some(p)) => (lib.dvdnav_right_button_select)(nav, p),
                (Action::Select(n), Some(p)) => (lib.dvdnav_button_select)(nav, p, n),
                (Action::Press(None), Some(p)) => (lib.dvdnav_button_activate)(nav, p),
                (Action::Press(Some(n)), Some(p)) => {
                    (lib.dvdnav_button_select_and_activate)(nav, p, n)
                }
                (Action::Menu(m), _) => {
                    self.drain = None;
                    // The other menu is better than none.
                    let other = if m == Menu::Root {
                        Menu::Title
                    } else {
                        Menu::Root
                    };
                    i32::from(self.call_menu(m) || self.call_menu(other))
                }
                (Action::StillOver, _) => (lib.dvdnav_still_skip)(nav),
                (Action::Skip, _) => {
                    if matches!(self.halt, Some(Halt::Still(_))) {
                        (lib.dvdnav_still_skip)(nav)
                    } else {
                        (lib.dvdnav_next_pg_search)(nav)
                    }
                }
                (Action::Back, _) => (lib.dvdnav_prev_pg_search)(nav),
                (Action::TitleDone, _) => {
                    self.title_done();
                    1
                }
                _ => 0,
            }
        };
        self.note_button();
        if done != DVDNAV_STATUS_OK {
            log::debug!("dvdnav: {action:?}: {}", self.error());
            return Outcome::Same;
        }
        match action {
            // Choosing a button doesn't move the program (an auto-press does).
            Action::Select(_) => Outcome::Same,
            _ => self.advance(),
        }
    }

    /// Run the program on from the end of the title mpv played.
    fn title_done(&mut self) {
        // What comes next is a new stream, whatever it is.
        self.hopped = true;
        let Some(title) = self.handed_over.take() else {
            return;
        };
        self.queue.clear();
        let (mut pos, mut len) = (0u32, 0u32);
        // SAFETY: plain calls; SEEK_SET is 0.
        unsafe {
            (self.lib.dvdnav_get_position)(self.nav, &mut pos, &mut len);
            (self.lib.dvdnav_sector_search)(self.nav, i64::from(len.saturating_sub(64)), 0);
        }
        self.drain = Some(Drain {
            title,
            cell_start: None,
            read: 0,
        });
    }

    fn note_button(&mut self) {
        let mut n = 0;
        if unsafe { (self.lib.dvdnav_get_current_highlight)(self.nav, &mut n) } == DVDNAV_STATUS_OK
        {
            self.button = n;
        }
    }

    /// Take what a block says (buttons, subpictures) and put its times right.
    fn deliver(&mut self, b: &mut [u8; BLOCK]) {
        if let Some(nav) = spu::parse_nav(b) {
            self.navigation(&nav);
        }
        let shift = self.shift.unwrap_or(0);
        let mut at = 14 + usize::from(b[13] & 7);
        while at + 9 < BLOCK && b[at..at + 3] == [0, 0, 1] {
            let id = b[at + 3];
            let len = usize::from(b[at + 4]) << 8 | usize::from(b[at + 5]);
            let end = (at + 6 + len).min(BLOCK);
            if matches!(id, 0xBD | 0xC0..=0xDF | 0xE0..=0xEF) && b[at + 6] >> 6 == 2 {
                let (flags, head) = (b[at + 7] >> 6, usize::from(b[at + 8]));
                let mut pts = None;
                if flags & 2 != 0 && at + 14 <= end {
                    pts = Some(shift_time(&mut b[at + 9..at + 14], shift));
                    if flags == 3 && at + 19 <= end {
                        shift_time(&mut b[at + 14..at + 19], shift);
                    }
                }
                let data = at + 9 + head;
                if id == 0xBD && data < end && b[data] & 0xE0 == 0x20 {
                    self.subpicture(b[data] & 0x1f, pts, &b[data + 1..end]);
                }
            }
            at = end;
        }
        self.sent += BLOCK as u64;
    }

    fn navigation(&mut self, nav: &NavPack) {
        let (start, end) = (i64::from(nav.start), i64::from(nav.end));
        // Each part of the disc starts its own count: continue ours.
        let expected = self.last_end;
        let shift = match self.shift {
            Some(s) if (start + s - expected).abs() <= 45_000 => s,
            _ => expected - start,
        };
        self.shift = Some(shift);
        self.last_end = end.max(start) + shift;
        let buttons = match &nav.highlight {
            Highlight::Same => return,
            Highlight::None => Vec::new(),
            Highlight::Buttons(b) => b.clone(),
        };
        let pts = start + shift;
        if buttons.is_empty() && self.moments.back().is_none_or(|m| m.buttons.is_empty()) {
            return;
        }
        let mut pci = [0u64; spu::PCI_SIZE.div_ceil(8)];
        // SAFETY: libdvdnav's pci_t is PCI_SIZE bytes, and this one is at
        // least as large.
        unsafe {
            let p = (self.lib.dvdnav_get_current_nav_pci)(self.nav);
            if p.is_null() {
                return;
            }
            std::ptr::copy_nonoverlapping(p, pci.as_mut_ptr().cast::<u8>(), spu::PCI_SIZE);
        }
        self.note_button();
        let same = self.moments.back().is_some_and(|m| *m.buttons == buttons);
        if same && let Some(m) = self.moments.back_mut() {
            m.pci = Arc::new(pci);
            return;
        }
        self.moments.push_back(Moment {
            pts,
            buttons: Arc::new(buttons),
            pci: Arc::new(pci),
        });
        if self.moments.len() > 64 {
            self.moments.pop_front();
        }
    }

    fn subpicture(&mut self, stream: u8, pts: Option<i64>, data: &[u8]) {
        let a = self.assembling.entry(stream).or_default();
        if let Some(pts) = pts {
            *a = Assembly {
                pts,
                data: Vec::new(),
            };
        }
        a.data.extend_from_slice(data);
        if a.data.len() < 4 {
            return;
        }
        let size = usize::from(a.data[0]) << 8 | usize::from(a.data[1]);
        if a.data.len() < size {
            return;
        }
        let a = std::mem::take(a);
        if let Some(sp) = spu::decode(&a.data[..size], a.pts) {
            self.pictures.push_back((stream, sp));
            if self.pictures.len() > 32 {
                self.pictures.pop_front();
            }
        }
    }

    /// The buttons and the subpicture on screen at `pts`.
    pub fn at(&self, pts: i64) -> (Option<Moment>, Option<Subpicture>) {
        let moment = self
            .moments
            .iter()
            .rev()
            .find(|m| m.pts <= pts)
            .or(self.moments.front())
            .cloned();
        let shown = |any: bool| {
            let right = |s: &u8| any || i32::from(*s) == self.spu_stream;
            self.pictures
                .iter()
                .rev()
                .find(|(s, p)| right(s) && p.pts <= pts)
                .map(|(_, p)| p.clone())
        };
        (moment, shown(false).or_else(|| shown(true)))
    }

    /// In a menu (not a short clip).
    pub fn in_menu(&self) -> bool {
        !self.in_title()
    }
}

/// Add `shift` to a 33-bit time of a packet header; the new time.
fn shift_time(b: &mut [u8], shift: i64) -> i64 {
    let t = i64::from(b[0] >> 1 & 7) << 30
        | i64::from(b[1]) << 22
        | i64::from(b[2] >> 1) << 15
        | i64::from(b[3]) << 7
        | i64::from(b[4] >> 1);
    let t = (t + shift).max(0) & 0x1_FFFF_FFFF;
    b[0] = (b[0] & 0xF0) | ((t >> 29) as u8 & 0x0E) | 1;
    b[1] = (t >> 22) as u8;
    b[2] = (t >> 14) as u8 | 1;
    b[3] = (t >> 7) as u8;
    b[4] = ((t << 1) as u8) | 1;
    t
}

/// A machine shared by mpv's reading thread, the worker and the UI.
pub type Shared = Arc<Mutex<Machine>>;

/// What mpv reads.
pub struct Reader {
    machine: Shared,
    generation: u64,
    /// Told when the stream ends or jumps.
    notify: Arc<dyn Fn() + Send + Sync>,
    rest: Vec<u8>,
}

impl Reader {
    pub fn new(machine: Shared, generation: u64, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            machine,
            generation,
            notify,
            rest: Vec::new(),
        }
    }
}

impl StreamSource for Reader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        if self.rest.is_empty() {
            let Ok(mut m) = self.machine.lock() else {
                return 0;
            };
            if m.generation != self.generation || m.halt.is_some() || m.hopped && m.sent > 0 {
                return 0;
            }
            let step = match m.queue.pop_front() {
                Some(b) => Step::Block(b),
                None => m.step(true),
            };
            match step {
                Step::Block(mut b) => {
                    m.deliver(&mut b);
                    self.rest = b.to_vec();
                }
                Step::Halt(h) => {
                    log::debug!("dvd: {h:?}");
                    m.halt = Some(h);
                    drop(m);
                    (self.notify)();
                    return 0;
                }
                Step::Hop => {
                    m.hopped = true;
                    drop(m);
                    (self.notify)();
                    return 0;
                }
            }
        }
        let n = self.rest.len().min(buf.len());
        buf[..n].copy_from_slice(&self.rest[..n]);
        self.rest.drain(..n);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walks a real disc (or image): SMP_TEST_DVD=/dev/sr0 cargo test
    /// walk_a_disc -- --ignored --nocapture
    #[test]
    #[ignore = "needs a DVD: SMP_TEST_DVD"]
    fn walk_a_disc() {
        let Ok(device) = std::env::var("SMP_TEST_DVD") else {
            return;
        };
        let machine: Shared = Arc::new(Mutex::new(Machine::open(&device, "en").expect("open")));
        let mut outcome = machine.lock().unwrap().advance();
        println!("{:?}", machine.lock().unwrap().name());
        for round in 0..12 {
            println!("{round}: {outcome:?}");
            let mut action = Action::TitleDone;
            match &outcome {
                Outcome::End => break,
                Outcome::Title(_) => {}
                _ => {
                    let generation = machine.lock().unwrap().generation;
                    let mut reader = Reader::new(machine.clone(), generation, Arc::new(|| {}));
                    let (mut total, mut buf) = (0, vec![0u8; 65536]);
                    let mut found = None;
                    while total < 40 << 20 {
                        let n = reader.read(&mut buf);
                        let m = machine.lock().unwrap();
                        found = m.moments.back().filter(|m| !m.buttons.is_empty()).cloned();
                        if n == 0 || found.is_some() && total > 2 << 20 {
                            break;
                        }
                        total += n;
                    }
                    let m = machine.lock().unwrap();
                    let buttons = found.as_ref().map_or(0, |f| f.buttons.len());
                    let pictures = m.pictures.len();
                    println!(
                        "   {total} bytes, {buttons} buttons, chosen {}, {pictures} subpictures, {:?}",
                        m.button, m.halt
                    );
                    action = match (&m.halt, found) {
                        (_, Some(_)) => Action::Press(Some((round % buttons.max(1)) as i32 + 1)),
                        (Some(Halt::Still(_)), _) => Action::StillOver,
                        (Some(Halt::Title(t)), _) => {
                            outcome = Outcome::Title(t.clone());
                            continue;
                        }
                        _ => Action::Skip,
                    };
                }
            }
            println!("   {action:?}");
            let mut m = machine.lock().unwrap();
            let moment = m.moments.back().cloned();
            outcome = m.act(action, moment.as_ref());
        }
    }

    #[test]
    fn times() {
        let enc = |t: i64| {
            let mut b = [0x21u8, 0, 1, 0, 1];
            shift_time(&mut b, t);
            b
        };
        for t in [0, 1, 90_000, 0x1_2345_6789] {
            let mut b = enc(t);
            assert_eq!(shift_time(&mut b, 0), t);
            assert_eq!(shift_time(&mut b, 4500), t + 4500);
            assert_eq!((b[0] & 0xF1, b[2] & 1, b[4] & 1), (0x21, 1, 1));
        }
        let mut b = enc(1000);
        assert_eq!(shift_time(&mut b, -5000), 0);
    }
}
