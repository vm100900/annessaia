// Paint — a pixel-art editor for the annessaia runtime.
//
// The artwork is a fixed 64×48 grid of palette indices, drawn as GPU rectangles.
// Working on a coarse grid rather than the raw framebuffer keeps flood fill cheap,
// makes the art crisp at any window size, and means the whole drawing persists in
// storage as 3 KB of indices.
//
// Tools: brush, eraser, flood fill, and an eyedropper. Left-drag paints.

use annessaia_sdk::{Color, gpu, input, storage, sys};
use std::sync::Mutex;

const EXPORT_SCALE: u32 = 8;   // 64×48 art exports as a 512×384 PNG

const GW: usize = 64;          // grid cells across
const GH: usize = 48;          // grid cells down
const BAR: f32 = 74.0;         // toolbar width in pixels

const BG:     Color = Color::rgb(  9,  13,  24);
const PANEL:  Color = Color::rgb( 17,  24,  40);
const EDGE:   Color = Color::rgb( 38,  50,  74);
const GRIDLN: Color = Color::rgb(255, 255, 255);
const SEL:    Color = Color::rgb( 56, 189, 248);
const CANVAS: Color = Color::rgb( 24,  30,  46);

// Index 0 is "empty" and renders as the canvas backdrop.
const PALETTE: [Color; 17] = [
    Color::rgb( 24,  30,  46),   // 0 empty
    Color::rgb(  0,   0,   0),
    Color::rgb(255, 255, 255),
    Color::rgb(155, 173, 200),
    Color::rgb( 90, 105, 136),
    Color::rgb(239,  68,  68),
    Color::rgb(251, 146,  60),
    Color::rgb(250, 204,  21),
    Color::rgb( 74, 222, 128),
    Color::rgb( 16, 185, 129),
    Color::rgb( 56, 189, 248),
    Color::rgb( 59, 130, 246),
    Color::rgb(139,  92, 246),
    Color::rgb(217,  70, 239),
    Color::rgb(244, 114, 182),
    Color::rgb(120,  53,  15),
    Color::rgb(180, 120,  70),
];

#[derive(Clone, Copy, PartialEq)]
enum Tool { Brush, Eraser, Fill, Pick }

struct Paint {
    cells: [u8; GW * GH],
    color: u8,
    size:  i32,          // brush radius in cells: 1, 2 or 3
    tool:  Tool,
    prev:  Option<(i32, i32)>,   // last painted cell, for interpolating fast drags
    grid:  bool,
    dirty: bool,
    was_down: bool,
    loaded: bool,
}

static P: Mutex<Paint> = Mutex::new(Paint {
    cells: [0; GW * GH],
    color: 2, size: 1, tool: Tool::Brush,
    prev: None, grid: true, dirty: false, was_down: false, loaded: false,
});

#[no_mangle]
pub extern "C" fn init() {
    let mut p = P.lock().unwrap();
    if let Some(bytes) = storage::get("paint.canvas") {
        if bytes.len() == GW * GH {
            p.cells.copy_from_slice(&bytes);
        }
    }
    p.loaded = true;
}

#[no_mangle]
pub extern "C" fn render_gpu() {
    let (w, h) = gpu::canvas();
    let mut p = P.lock().unwrap();

    gpu::clear(BG);

    // Fit the grid into whatever space is left beside the toolbar.
    let avail_w = (w - BAR - 24.0).max(40.0);
    let avail_h = (h - 24.0).max(40.0);
    let cell = (avail_w / GW as f32).min(avail_h / GH as f32).max(1.0);
    let cw = cell * GW as f32;
    let ch = cell * GH as f32;
    let ox = BAR + 12.0 + (avail_w - cw) * 0.5;
    let oy = 12.0 + (avail_h - ch) * 0.5;

    let (mx, my) = input::mouse();
    let down = input::left_down();

    // ── Input ─────────────────────────────────────────────────────────────────
    if mx < BAR {
        if input::left_clicked() { toolbar_click(&mut p, mx, my, h); }
        p.prev = None;
    } else if down {
        let gx = ((mx - ox) / cell).floor() as i32;
        let gy = ((my - oy) / cell).floor() as i32;
        if gx >= 0 && gy >= 0 && (gx as usize) < GW && (gy as usize) < GH {
            match p.tool {
                Tool::Pick => {
                    let c = p.cells[gy as usize * GW + gx as usize];
                    if c != 0 { p.color = c; }
                    p.tool = Tool::Brush;
                }
                Tool::Fill => {
                    if !p.was_down {           // one fill per press, not per frame
                        let target = p.cells[gy as usize * GW + gx as usize];
                        let repl = p.color;
                        flood(&mut p, gx, gy, target, repl);
                        p.dirty = true;
                    }
                }
                Tool::Brush | Tool::Eraser => {
                    let ink = if p.tool == Tool::Eraser { 0 } else { p.color };
                    // Interpolate from the previous cell so fast drags don't leave gaps.
                    if let Some((px, py)) = p.prev {
                        line_cells(&mut p, px, py, gx, gy, ink);
                    } else {
                        stamp(&mut p, gx, gy, ink);
                    }
                    p.prev = Some((gx, gy));
                    p.dirty = true;
                }
            }
        }
    } else {
        p.prev = None;
    }

    // Save once on release rather than every frame while drawing.
    if p.was_down && !down && p.dirty {
        let bytes = p.cells.to_vec();
        storage::set("paint.canvas", &bytes);
        p.dirty = false;
    }
    p.was_down = down;

    // ── Canvas ────────────────────────────────────────────────────────────────
    gpu::rect(ox - 3.0, oy - 3.0, cw + 6.0, ch + 6.0, 4.0, EDGE);
    gpu::rect(ox, oy, cw, ch, 0.0, CANVAS);

    for y in 0..GH {
        for x in 0..GW {
            let c = p.cells[y * GW + x];
            if c == 0 { continue; }
            gpu::rect(
                ox + x as f32 * cell, oy + y as f32 * cell,
                cell + 0.5, cell + 0.5, 0.0,
                PALETTE[c as usize],
            );
        }
    }

    // Grid overlay, only when cells are big enough for it to read as guidance
    if p.grid && cell >= 7.0 {
        for x in 0..=GW {
            let gx = ox + x as f32 * cell;
            gpu::line1(gx, oy, gx, oy + ch, GRIDLN.fade(0.05));
        }
        for y in 0..=GH {
            let gy = oy + y as f32 * cell;
            gpu::line1(ox, gy, ox + cw, gy, GRIDLN.fade(0.05));
        }
    }

    // Hover preview of the brush footprint
    if mx >= BAR {
        let gx = ((mx - ox) / cell).floor() as i32;
        let gy = ((my - oy) / cell).floor() as i32;
        if gx >= 0 && gy >= 0 && (gx as usize) < GW && (gy as usize) < GH {
            let r = if p.tool == Tool::Fill || p.tool == Tool::Pick { 1 } else { p.size };
            let half = (r - 1) as f32;
            gpu::rect(
                ox + (gx as f32 - half) * cell, oy + (gy as f32 - half) * cell,
                cell * (half * 2.0 + 1.0), cell * (half * 2.0 + 1.0),
                0.0, SEL.fade(0.30),
            );
        }
    }

    draw_toolbar(&p, h);
}

// ── Painting ──────────────────────────────────────────────────────────────────

fn stamp(p: &mut Paint, cx: i32, cy: i32, ink: u8) {
    let r = p.size - 1;
    for dy in -r..=r {
        for dx in -r..=r {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x as usize >= GW || y as usize >= GH { continue; }
            p.cells[y as usize * GW + x as usize] = ink;
        }
    }
}

// Bresenham between two cells so a fast drag paints a continuous stroke.
fn line_cells(p: &mut Paint, x0: i32, y0: i32, x1: i32, y1: i32, ink: u8) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y) = (x0, y0);
    let mut e = dx + dy;
    loop {
        stamp(p, x, y, ink);
        if x == x1 && y == y1 { break; }
        let e2 = 2 * e;
        if e2 >= dy { e += dy; x += sx; }
        if e2 <= dx { e += dx; y += sy; }
    }
}

// Iterative flood fill — a recursive one would blow the WASM stack on a full canvas.
fn flood(p: &mut Paint, sx: i32, sy: i32, target: u8, repl: u8) {
    if target == repl { return; }
    let mut stack = vec![(sx, sy)];
    while let Some((x, y)) = stack.pop() {
        if x < 0 || y < 0 || x as usize >= GW || y as usize >= GH { continue; }
        let i = y as usize * GW + x as usize;
        if p.cells[i] != target { continue; }
        p.cells[i] = repl;
        stack.push((x + 1, y));
        stack.push((x - 1, y));
        stack.push((x, y + 1));
        stack.push((x, y - 1));
    }
}

// ── PNG export ────────────────────────────────────────────────────────────────
// Written by hand rather than pulled from a crate: PNG permits *stored* (literally
// uncompressed) deflate blocks, so a valid file needs only CRC-32, Adler-32 and a
// little framing. Empty cells export with alpha 0, which is what you want when
// dropping pixel art onto another background.

fn export_png(p: &Paint) -> Vec<u8> {
    let (w, h) = (GW as u32 * EXPORT_SCALE, GH as u32 * EXPORT_SCALE);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let c = p.cells[(y / EXPORT_SCALE) as usize * GW + (x / EXPORT_SCALE) as usize];
            if c == 0 { continue; }                     // leave transparent
            let col = PALETTE[c as usize].0 as u32;     // packed 0xRRGGBBAA
            let i = ((y * w + x) * 4) as usize;
            rgba[i]     = (col >> 24) as u8;
            rgba[i + 1] = (col >> 16) as u8;
            rgba[i + 2] = (col >> 8) as u8;
            rgba[i + 3] = 255;
        }
    }
    encode_png(w, h, &rgba)
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    // Each scanline is prefixed with a filter byte; 0 means "no filtering".
    let stride = (w * 4) as usize;
    let mut raw = Vec::with_capacity(h as usize * (1 + stride));
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);   // 8 bits/channel, RGBA, deflate, no filter, no interlace
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    // The CRC covers the type code as well as the payload.
    let mut crc_in = Vec::with_capacity(4 + data.len());
    crc_in.extend_from_slice(kind);
    crc_in.extend_from_slice(data);
    out.extend_from_slice(&crc_in);
    out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
}

// A zlib stream of stored blocks: no compression, but entirely valid.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];               // CM=8, CINFO=7, FCHECK making it a multiple of 31
    let mut i = 0usize;
    loop {
        let n = (raw.len() - i).min(65535);
        let last = if i + n >= raw.len() { 1u8 } else { 0 };
        z.push(last);
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[i..i + n]);
        i += n;
        if last == 1 { break; }                 // also emits one empty block if raw is empty
    }
    z.extend_from_slice(&adler32(raw).to_be_bytes());
    z
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ── Toolbar ───────────────────────────────────────────────────────────────────
// Laid out by the same arithmetic in both the hit-test and the draw pass, so the
// two can't drift apart.

// Everything is two columns of 26 px on a 30 px pitch, which is exactly what fits
// inside BAR: 8 + 30 + 26 = 64 < 74. Laying the four tools out in a single row
// would need 124 px and spill onto the canvas.
fn tool_rect(i: usize) -> (f32, f32, f32, f32) {
    (8.0 + (i % 2) as f32 * 30.0, 14.0 + (i / 2) as f32 * 30.0, 26.0, 26.0)
}
// Three small buttons on one row: 8 + 2*21 + 18 = 68 < 74.
fn size_rect(i: usize) -> (f32, f32, f32, f32) { (8.0 + i as f32 * 21.0, 78.0, 18.0, 18.0) }
fn swatch_rect(i: usize) -> (f32, f32, f32, f32) {
    (8.0 + (i % 2) as f32 * 30.0, 106.0 + (i / 2) as f32 * 30.0, 26.0, 26.0)
}
// Three stacked buttons: save, grid toggle, clear.
fn bottom_rect(i: usize, h: f32) -> (f32, f32, f32, f32) { (8.0, h - 110.0 + i as f32 * 34.0, 56.0, 28.0) }

fn inside(x: f32, y: f32, r: (f32, f32, f32, f32)) -> bool {
    x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3
}

fn toolbar_click(p: &mut Paint, mx: f32, my: f32, h: f32) {
    for (i, t) in [Tool::Brush, Tool::Eraser, Tool::Fill, Tool::Pick].iter().enumerate() {
        if inside(mx, my, tool_rect(i)) { p.tool = *t; return; }
    }
    for i in 0..3 {
        if inside(mx, my, size_rect(i)) { p.size = i as i32 + 1; return; }
    }
    for i in 1..PALETTE.len() {
        if inside(mx, my, swatch_rect(i - 1)) {
            p.color = i as u8;
            if p.tool == Tool::Eraser { p.tool = Tool::Brush; }
            return;
        }
    }
    if inside(mx, my, bottom_rect(0, h)) { sys::save("art.png", &export_png(p)); return; }
    if inside(mx, my, bottom_rect(1, h)) { p.grid = !p.grid; return; }
    if inside(mx, my, bottom_rect(2, h)) {
        p.cells = [0; GW * GH];
        storage::set("paint.canvas", &p.cells.to_vec());
        p.dirty = false;
    }
}

fn draw_toolbar(p: &Paint, h: f32) {
    gpu::rect(0.0, 0.0, BAR, h, 0.0, PANEL);
    gpu::line1(BAR, 0.0, BAR, h, EDGE);

    // Tools — each drawn as a small glyph made of primitives
    let tools = [Tool::Brush, Tool::Eraser, Tool::Fill, Tool::Pick];
    for (i, t) in tools.iter().enumerate() {
        let (x, y, w2, h2) = tool_rect(i);
        let on = p.tool == *t;
        gpu::rect(x, y, w2, h2, 6.0, if on { SEL.fade(0.28) } else { Color::rgb(28, 38, 58) });
        if on { gpu::rect(x, y + h2 - 2.0, w2, 2.0, 1.0, SEL); }
        let (cx, cy) = (x + w2 * 0.5, y + h2 * 0.5);
        let ink = if on { SEL } else { Color::rgb(140, 158, 186) };
        match t {
            Tool::Brush  => { gpu::line(cx - 5.0, cy + 5.0, cx + 4.0, cy - 5.0, ink, 3.0);
                              gpu::circle(cx + 4.0, cy - 5.0, 2.5, ink); }
            Tool::Eraser => { gpu::rect(cx - 6.0, cy - 3.0, 12.0, 7.0, 2.0, ink); }
            Tool::Fill   => { gpu::triangle(cx - 6.0, cy + 1.0, cx + 2.0, cy - 6.0, cx + 6.0, cy + 1.0, ink);
                              gpu::circle(cx + 5.0, cy + 5.0, 2.5, ink); }
            Tool::Pick   => { gpu::circle(cx, cy, 5.0, ink.fade(0.35));
                              gpu::circle(cx, cy, 2.5, ink); }
        }
    }

    // Brush sizes — dot scales with the setting, kept inside the smaller button
    for i in 0..3 {
        let (x, y, w2, h2) = size_rect(i);
        let on = p.size == i as i32 + 1;
        gpu::rect(x, y, w2, h2, 5.0, if on { SEL.fade(0.28) } else { Color::rgb(28, 38, 58) });
        gpu::circle(x + w2 * 0.5, y + h2 * 0.5, 1.5 + i as f32 * 2.0,
                    if on { SEL } else { Color::rgb(140, 158, 186) });
    }

    // Palette
    for i in 1..PALETTE.len() {
        let (x, y, w2, h2) = swatch_rect(i - 1);
        gpu::rect(x, y, w2, h2, 5.0, PALETTE[i]);
        if p.color == i as u8 && p.tool != Tool::Eraser {
            gpu::rect(x - 2.0, y - 2.0, w2 + 4.0, h2 + 4.0, 7.0, SEL.fade(0.55));
            gpu::rect(x, y, w2, h2, 5.0, PALETTE[i]);
        }
    }

    // Save — arrow descending into a tray
    let (sx, sy, sw, sh) = bottom_rect(0, h);
    gpu::rect(sx, sy, sw, sh, 6.0, Color::rgb(20, 62, 44));
    let ink = Color::rgb(74, 222, 128);
    let mid = sx + sw * 0.5;
    gpu::line(mid, sy + 7.0, mid, sy + 15.0, ink, 2.5);
    gpu::triangle(mid - 5.0, sy + 13.0, mid + 5.0, sy + 13.0, mid, sy + 19.0, ink);
    gpu::rect(mid - 9.0, sy + 20.0, 18.0, 2.0, 1.0, ink);

    // Grid toggle
    let (gx, gy, gw, gh) = bottom_rect(1, h);
    gpu::rect(gx, gy, gw, gh, 6.0, if p.grid { SEL.fade(0.25) } else { Color::rgb(28, 38, 58) });
    for k in 0..3 {
        let s = gx + 20.0 + k as f32 * 8.0;
        gpu::line1(s, gy + 6.0, s, gy + gh - 6.0, if p.grid { SEL } else { Color::rgb(110, 128, 156) });
    }

    // Clear
    let (cx, cy, cw2, ch2) = bottom_rect(2, h);
    gpu::rect(cx, cy, cw2, ch2, 6.0, Color::rgb(80, 26, 26));
    let cm = cx + cw2 * 0.5;
    gpu::line(cm - 7.0, cy + 8.0, cm + 7.0, cy + ch2 - 8.0, Color::rgb(248, 113, 113), 2.5);
    gpu::line(cm + 7.0, cy + 8.0, cm - 7.0, cy + ch2 - 8.0, Color::rgb(248, 113, 113), 2.5);
}
