//! What a DVD menu draws over its video: the buttons of a navigation pack
//! (their places and colours), the subpicture that holds their shapes, and
//! the two put together as a picture.

/// Colours (palette indexes) and opacities (0-15) of the four pixel values.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Colors {
    pub color: [u8; 4],
    pub alpha: [u8; 4],
}

impl Colors {
    /// From a button colour word: four colours, then four opacities, the
    /// highest pixel value first.
    fn from_word(w: u32) -> Self {
        let n = |shift: u32| (w >> shift & 15) as u8;
        Self { color: [n(16), n(20), n(24), n(28)], alpha: [n(0), n(4), n(8), n(12)] }
    }
}

/// A rectangle in the video's own pixels, edges included.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x0: u16,
    pub y0: u16,
    pub x1: u16,
    pub y1: u16,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= i32::from(self.x0) && x <= i32::from(self.x1) && y >= i32::from(self.y0) && y <= i32::from(self.y1)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Button {
    pub rect: Rect,
    /// How it looks chosen, and at the moment it is pressed.
    pub selected: Colors,
    pub pressed: Colors,
    /// Reaching it presses it.
    pub auto: bool,
}

/// The highlight part of a navigation pack.
#[derive(Clone, PartialEq, Debug)]
pub enum Highlight {
    /// No buttons from here on.
    None,
    /// The buttons given before stay.
    Same,
    Buttons(Vec<Button>),
}

/// Bytes of the parsed PCI structure libdvdnav wants back (`pci_t`).
pub const PCI_SIZE: usize = 979;
/// Where the PCI data starts in a navigation pack.
const PCI: usize = 45;
const HLI: usize = PCI + 96;

/// What a navigation pack says about its moment.
#[derive(Clone, PartialEq, Debug)]
pub struct NavPack {
    /// First and last presentation time of its video (90 kHz).
    pub start: u32,
    pub end: u32,
    pub highlight: Highlight,
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Read a navigation pack (2048 bytes).
pub fn parse_nav(b: &[u8]) -> Option<NavPack> {
    if b.len() < 2048 || be32(b, 0) != 0x1BA || be32(b, PCI - 7) != 0x1BF || b[PCI - 1] != 0 {
        return None;
    }
    let highlight = match be16(b, HLI) & 3 {
        0 => Highlight::None,
        1 => Highlight::Buttons(parse_buttons(&b[HLI..])),
        _ => Highlight::Same,
    };
    Some(NavPack { start: be32(b, PCI + 12), end: be32(b, PCI + 16), highlight })
}

fn parse_buttons(h: &[u8]) -> Vec<Button> {
    let groups = usize::from(h[14] >> 4 & 3).max(1);
    let per_group = 36 / groups;
    // The same buttons, once per display mode: the first group that isn't
    // for a letterboxed or cropped picture. Like libdvdnav, else the first.
    let kinds = [h[14] & 7, h[15] >> 4 & 7, h[15] & 7];
    let group = (0..groups).find(|g| kinds[*g] & 6 == 0).unwrap_or(0);
    let count = usize::from(h[17] & 63).min(per_group);
    let colors = |set: usize, which: usize| Colors::from_word(be32(h, 22 + (set - 1) * 8 + which * 4));
    (0..count)
        .map(|i| {
            let b = &h[46 + (group * per_group + i) * 18..][..18];
            let set = usize::from(b[0] >> 6);
            let pair = |at: usize| {
                let hi = u16::from(b[at] & 0x3f) << 4 | u16::from(b[at + 1] >> 4);
                let lo = u16::from(b[at + 1] & 3) << 8 | u16::from(b[at + 2]);
                (hi, lo)
            };
            let ((x0, x1), (y0, y1)) = (pair(0), pair(3));
            let (selected, pressed) =
                if set == 0 { Default::default() } else { (colors(set, 0), colors(set, 1)) };
            Button { rect: Rect { x0, y0, x1, y1 }, selected, pressed, auto: b[3] >> 6 != 0 }
        })
        .collect()
}

/// A decoded subpicture: one of four values for each pixel.
#[derive(Clone, PartialEq, Debug)]
pub struct Subpicture {
    /// When it appears (stream time, 90 kHz).
    pub pts: i64,
    pub x: u16,
    pub y: u16,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub colors: Colors,
}

/// Decode one subpicture packet. `None`: broken, or nothing to show.
pub fn decode(d: &[u8], pts: i64) -> Option<Subpicture> {
    let get16 = |at: usize| Some(usize::from(*d.get(at)?) << 8 | usize::from(*d.get(at + 1)?));
    let first = get16(2)?;
    let (mut colors, mut area, mut fields, mut delay) = (Colors::default(), None, None, 0i64);
    let mut seq = first;
    let mut started = false;
    // Control sequences: a date, the next one's place, then commands.
    for _ in 0..64 {
        let date = get16(seq)?;
        let next = get16(seq + 2)?;
        let mut at = seq + 4;
        loop {
            let cmd = *d.get(at)?;
            at += 1;
            match cmd {
                0x00 | 0x01 => {
                    if !started {
                        started = true;
                        delay = date as i64 * 1024;
                    }
                }
                0x02 => {}
                0x03 | 0x04 => {
                    let (a, b) = (*d.get(at)?, *d.get(at + 1)?);
                    let v = [b & 15, b >> 4, a & 15, a >> 4];
                    if cmd == 0x03 { colors.color = v } else { colors.alpha = v }
                    at += 2;
                }
                0x05 => {
                    let b = d.get(at..at + 6)?;
                    let pair = |i: usize| {
                        (
                            usize::from(b[i]) << 4 | usize::from(b[i + 1] >> 4),
                            usize::from(b[i + 1] & 15) << 8 | usize::from(b[i + 2]),
                        )
                    };
                    area = Some((pair(0), pair(3)));
                    at += 6;
                }
                0x06 => {
                    fields = Some([get16(at)?, get16(at + 2)?]);
                    at += 4;
                }
                0x07 => at += get16(at)?.max(2),
                _ => break,
            }
        }
        // The first sequence that shows it is the one that counts.
        if started || next <= seq {
            break;
        }
        seq = next;
    }
    let ((x0, x1), (y0, y1)) = area?;
    let fields = fields?;
    if x1 < x0 || y1 < y0 || x1 > 2047 || y1 > 2047 {
        return None;
    }
    let (width, height) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut pixels = vec![0u8; width * height];
    for (field, offset) in fields.into_iter().enumerate() {
        let mut bits = Nibbles { data: d, pos: offset * 2 };
        for row in (field..height).step_by(2) {
            let line = &mut pixels[row * width..][..width];
            let mut x = 0;
            while x < width {
                let (run, value) = bits.run()?;
                // A run of 0 fills the line.
                let end = if run == 0 { width } else { (x + run).min(width) };
                line[x..end].fill(value);
                x = end;
            }
            bits.align();
        }
    }
    Some(Subpicture { pts: pts + delay, x: x0 as u16, y: y0 as u16, width, height, pixels, colors })
}

struct Nibbles<'a> {
    data: &'a [u8],
    /// In half bytes.
    pos: usize,
}

impl Nibbles<'_> {
    fn next(&mut self) -> Option<usize> {
        let b = *self.data.get(self.pos / 2)?;
        let n = if self.pos.is_multiple_of(2) { b >> 4 } else { b & 15 };
        self.pos += 1;
        Some(usize::from(n))
    }

    /// A run's length and pixel value: 1 to 4 half bytes.
    fn run(&mut self) -> Option<(usize, u8)> {
        let mut v = self.next()?;
        for limit in [0x4, 0x10, 0x40] {
            if v >= limit {
                break;
            }
            v = v << 4 | self.next()?;
        }
        Some((v >> 2, (v & 3) as u8))
    }

    fn align(&mut self) {
        self.pos = self.pos.next_multiple_of(2);
    }
}

/// A picture to lay over the video, as premultiplied BGRA (mpv's overlays).
pub struct Overlay {
    pub x: i32,
    pub y: i32,
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

/// Palette entry (0x00YYCrCb, studio range) to R, G, B.
pub fn rgb(ycc: u32) -> [u8; 3] {
    let y = (f64::from(ycc >> 16 & 255) - 16.0) * 255.0 / 219.0;
    let cr = (f64::from(ycc >> 8 & 255) - 128.0) * 255.0 / 224.0;
    let cb = (f64::from(ycc & 255) - 128.0) * 255.0 / 224.0;
    let c = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    [c(y + 1.402 * cr), c(y - 0.344_136 * cb - 0.714_136 * cr), c(y + 1.772 * cb)]
}

/// Where the video sits on the screen, and its own size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Place {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub video: (u32, u32),
}

impl Place {
    /// A point on the screen, in video pixels.
    pub fn to_video(self, x: f64, y: f64) -> (i32, i32) {
        (
            ((x - self.left) * f64::from(self.video.0) / self.width).floor() as i32,
            ((y - self.top) * f64::from(self.video.1) / self.height).floor() as i32,
        )
    }
}

/// Draw the subpicture, scaled to the screen: in its own colours, and inside
/// `button` in the button's. `None`: nothing visible.
pub fn draw(sp: &Subpicture, button: Option<(&Rect, &Colors)>, clut: &[u32; 16], place: &Place) -> Option<Overlay> {
    let (sx, sy) = (place.width / f64::from(place.video.0), place.height / f64::from(place.video.1));
    if !(sx > 0.0 && sy > 0.0 && sx < 64.0 && sy < 64.0) {
        return None;
    }
    let paint = |c: &Colors| -> [[f32; 4]; 4] {
        std::array::from_fn(|i| {
            let [r, g, b] = rgb(clut[usize::from(c.color[i])]);
            let a = f32::from(c.alpha[i]) / 15.0;
            [f32::from(b) * a, f32::from(g) * a, f32::from(r) * a, a * 255.0]
        })
    };
    let own = paint(&sp.colors);
    let lit = button.map(|(r, c)| (*r, paint(c)));
    let pixel = |x: i32, y: i32| -> [f32; 4] {
        let (lx, ly) = (x - i32::from(sp.x), y - i32::from(sp.y));
        if lx < 0 || ly < 0 || lx as usize >= sp.width || ly as usize >= sp.height {
            return [0.0; 4];
        }
        let v = usize::from(sp.pixels[ly as usize * sp.width + lx as usize]);
        match &lit {
            Some((r, c)) if r.contains(x, y) => c[v],
            _ => own[v],
        }
    };
    // Only the part that shows: the lit button, or the whole subpicture when
    // it is visible on its own.
    let full = Rect {
        x0: sp.x,
        y0: sp.y,
        x1: sp.x + sp.width as u16 - 1,
        y1: sp.y + sp.height as u16 - 1,
    };
    let area = if sp.colors.alpha.iter().any(|a| *a > 0) { full } else { *button?.0 };
    let x0 = (f64::from(area.x0) * sx).floor() as i32;
    let y0 = (f64::from(area.y0) * sy).floor() as i32;
    let width = ((f64::from(area.x1) + 1.0) * sx).ceil() as i32 - x0;
    let height = ((f64::from(area.y1) + 1.0) * sy).ceil() as i32 - y0;
    if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
        return None;
    }
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    let mut visible = false;
    for row in 0..height {
        // Between the four nearest pixels, so it isn't blocky when large.
        let v = (f64::from(y0 + row) + 0.5) / sy - 0.5;
        let (vy, fy) = (v.floor(), (v - v.floor()) as f32);
        for col in 0..width {
            let u = (f64::from(x0 + col) + 0.5) / sx - 0.5;
            let (ux, fx) = (u.floor(), (u - u.floor()) as f32);
            let (ix, iy) = (ux as i32, vy as i32);
            let (a, b) = (pixel(ix, iy), pixel(ix + 1, iy));
            let (c, d) = (pixel(ix, iy + 1), pixel(ix + 1, iy + 1));
            let at = (row as usize * width as usize + col as usize) * 4;
            for k in 0..4 {
                let top = a[k] + (b[k] - a[k]) * fx;
                let bottom = c[k] + (d[k] - c[k]) * fx;
                bgra[at + k] = (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8;
            }
            visible |= bgra[at + 3] > 0;
        }
    }
    visible.then(|| Overlay {
        x: (place.left + f64::from(x0)).round() as i32,
        y: (place.top + f64::from(y0)).round() as i32,
        width: width as usize,
        height: height as usize,
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_colours() {
        assert_eq!(rgb(0x0010_8080), [0, 0, 0]);
        assert_eq!(rgb(0x00EB_8080), [255, 255, 255]);
        let [r, g, b] = rgb(0x0051_F05A);
        assert!(r > 240 && g < 15 && b < 15, "{r} {g} {b}");
    }

    /// A 4×2 subpicture: top line 1 1 2 2, bottom line all 3.
    fn small() -> Vec<u8> {
        let mut d = vec![0, 0, 0, 0];
        let top = d.len();
        // run 2 of 1 (0x9), run 2 of 2 (0xA)
        d.push(0x9A);
        let bottom = d.len();
        // run 0 (to the end) of 3: 0x0003
        d.extend([0x00, 0x03]);
        let ctrl = d.len();
        d.extend([0, 0, (ctrl >> 8) as u8, ctrl as u8]);
        d.extend([0x01]);
        d.extend([0x03, 0x32, 0x10, 0x04, 0xFF, 0xF0]);
        // x 10..13, y 20..21
        d.extend([0x05, 0x00, 0xA0, 0x0D, 0x01, 0x40, 0x15]);
        d.extend([0x06, 0, top as u8, 0, bottom as u8, 0xFF]);
        let len = d.len();
        d[0] = (len >> 8) as u8;
        d[1] = len as u8;
        d[3] = ctrl as u8;
        d
    }

    #[test]
    fn subpictures() {
        let s = decode(&small(), 900).unwrap();
        assert_eq!((s.x, s.y, s.width, s.height, s.pts), (10, 20, 4, 2, 900));
        assert_eq!(s.pixels, [1, 1, 2, 2, 3, 3, 3, 3]);
        assert_eq!(s.colors.color, [0, 1, 2, 3]);
        assert_eq!(s.colors.alpha, [0, 15, 15, 15]);
        for cut in 0..small().len() {
            let _ = decode(&small()[..cut], 0);
        }
    }

    #[test]
    fn drawing() {
        let s = decode(&small(), 0).unwrap();
        let mut clut = [0x0010_8080; 16];
        clut[5] = 0x00EB_8080;
        let place = Place { left: 100.0, top: 50.0, width: 720.0, height: 480.0, video: (720, 480) };
        let lit = Colors { color: [5; 4], alpha: [15; 4] };
        let r = Rect { x0: 10, y0: 20, x1: 10, y1: 21 };
        let o = draw(&s, Some((&r, &lit)), &clut, &place).unwrap();
        assert_eq!((o.x, o.y, o.width, o.height), (110, 70, 4, 2));
        assert_eq!(&o.bgra[..8], &[255, 255, 255, 255, 0, 0, 0, 255]);
        // Invisible by itself: only the button is drawn.
        let mut s = s;
        s.colors.alpha = [0; 4];
        let o = draw(&s, Some((&r, &lit)), &clut, &place).unwrap();
        assert_eq!((o.width, o.height), (1, 2));
        assert!(draw(&s, None, &clut, &place).is_none());
        let big = Place { left: 0.0, top: 0.0, width: 1440.0, height: 960.0, video: (720, 480) };
        assert_eq!(big.to_video(21.0, 41.0), (10, 20));
        assert!(draw(&s, Some((&r, &lit)), &clut, &big).unwrap().width == 2);
    }

    #[test]
    fn buttons() {
        let mut b = vec![0u8; 2048];
        b[..4].copy_from_slice(&[0, 0, 1, 0xBA]);
        b[38..42].copy_from_slice(&[0, 0, 1, 0xBF]);
        b[PCI + 12..PCI + 16].copy_from_slice(&90_000u32.to_be_bytes());
        b[PCI + 16..PCI + 20].copy_from_slice(&135_000u32.to_be_bytes());
        let h = HLI;
        b[h + 1] = 1;
        b[h + 14] = 0x10;
        b[h + 17] = 2;
        b[h + 22..h + 26].copy_from_slice(&0x3210_F00Fu32.to_be_bytes());
        // Button 1: colours 1, x 100-200, y 50-80. Second: no colours, auto.
        let at = h + 46;
        b[at] = 0x40 | (100 >> 4) as u8;
        b[at + 1] = ((100 & 15) << 4) as u8;
        b[at + 2] = 200;
        b[at + 3] = (50 >> 4) as u8;
        b[at + 4] = ((50 & 15) << 4) as u8;
        b[at + 5] = 80;
        b[at + 18 + 3] = 0x40;
        let nav = parse_nav(&b).unwrap();
        assert_eq!((nav.start, nav.end), (90_000, 135_000));
        let Highlight::Buttons(list) = nav.highlight else { panic!("no buttons") };
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].rect, Rect { x0: 100, y0: 50, x1: 200, y1: 80 });
        assert_eq!(list[0].selected, Colors { color: [0, 1, 2, 3], alpha: [15, 0, 0, 15] });
        assert!(!list[0].auto && list[1].auto);
        assert!(list[0].rect.contains(200, 80) && !list[0].rect.contains(201, 80));
        b[h + 1] = 2;
        assert_eq!(parse_nav(&b).unwrap().highlight, Highlight::Same);
        assert!(parse_nav(&b[..100]).is_none());
    }
}
