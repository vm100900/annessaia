// Pixel app — pixel-art paint tool. Demos storage persistence and file_save.
// Exports: init(), framebuffer_ptr(), render_pixels(w, h)

use core::sync::atomic::{AtomicU32, AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

#[link(wasm_import_module = "env")]
extern "C" {
    fn input_mouse_x() -> f32;
    fn input_mouse_y() -> f32;
    fn input_mouse_down(btn: i32) -> i32;
    fn input_mouse_clicked(btn: i32) -> i32;
    fn input_touch_count() -> i32;
    fn input_touch_x(idx: i32) -> f32;
    fn input_touch_y(idx: i32) -> f32;

    fn storage_set(kp: *const u8, kl: usize, vp: *const u8, vl: usize);
    fn storage_get(kp: *const u8, kl: usize, op: *mut u8, om: usize) -> i32;
    fn file_save(dp: *const u8, dl: usize, np: *const u8, nl: usize);
    fn log_str(ptr: *const u8, len: usize);
}

fn store_bytes(key: &str, val: &[u8]) {
    unsafe { storage_set(key.as_ptr(), key.len(), val.as_ptr(), val.len()) }
}
fn load_bytes<'a>(key: &str, buf: &'a mut [u8]) -> Option<&'a [u8]> {
    let n = unsafe { storage_get(key.as_ptr(), key.len(), buf.as_mut_ptr(), buf.len()) };
    if n >= 0 { Some(&buf[..n as usize]) } else { None }
}
fn log(s: &str) { unsafe { log_str(s.as_ptr(), s.len()) } }

// ── Framebuffer ───────────────────────────────────────────────────────────────

static FB:     Mutex<Vec<u8>>  = Mutex::new(Vec::new());
static CANVAS: Mutex<Vec<u32>> = Mutex::new(Vec::new()); // 32×32 packed 0xRRGGBB, 0=empty

#[no_mangle]
pub extern "C" fn framebuffer_ptr() -> i32 { FB.lock().unwrap().as_ptr() as i32 }

// ── Palette ───────────────────────────────────────────────────────────────────

const PAL: [u32; 16] = [
    0x0D1117, 0xF0F6FC, 0xFF5555, 0x55FF55,
    0x5599FF, 0xFFFF55, 0x55FFFF, 0xFF55FF,
    0xFF8800, 0x88FF00, 0x00FF88, 0x0088FF,
    0x8800FF, 0xFF0088, 0xAA7744, 0x778899,
];

static SEL:   AtomicU32  = AtomicU32::new(2);  // selected palette color
static DIRTY: AtomicBool = AtomicBool::new(false);
static FRAME: AtomicU32  = AtomicU32::new(0);

fn rgb(c: u32) -> (u8, u8, u8) { ((c>>16) as u8, (c>>8) as u8, c as u8) }

// ── Canvas persistence ────────────────────────────────────────────────────────

const CANVAS_KEY: &str = "px_canvas";

fn save_canvas(canvas: &[u32]) {
    // Serialize as flat little-endian u32 array
    let mut bytes = Vec::with_capacity(32 * 32 * 4);
    for &c in canvas { bytes.extend_from_slice(&c.to_le_bytes()); }
    store_bytes(CANVAS_KEY, &bytes);
    log("canvas saved");
}

fn load_canvas_into(canvas: &mut Vec<u32>) {
    let mut buf = vec![0u8; 32 * 32 * 4];
    if let Some(b) = load_bytes(CANVAS_KEY, &mut buf) {
        if b.len() == 32 * 32 * 4 {
            canvas.resize(32 * 32, 0);
            for (i, chunk) in b.chunks(4).enumerate() {
                canvas[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
            log("canvas loaded");
        }
    }
}

fn export_ppm(canvas: &[u32]) {
    let header = b"P6\n32 32\n255\n";
    let mut out = Vec::with_capacity(header.len() + 32 * 32 * 3);
    out.extend_from_slice(header);
    for &c in canvas {
        let (r, g, b) = if c == 0 { (20u8, 26u8, 40u8) } else { rgb(c) };
        out.push(r); out.push(g); out.push(b);
    }
    let name = b"canvas.ppm";
    unsafe { file_save(out.as_ptr(), out.len(), name.as_ptr(), name.len()) }
}

// ── init — load persisted canvas ─────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn init() {
    let mut cv = CANVAS.lock().unwrap();
    cv.resize(32 * 32, 0);
    load_canvas_into(&mut cv);
}

// ── Render ────────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn render_pixels(w: i32, h: i32) {
    let mx     = unsafe { input_mouse_x() };
    let my     = unsafe { input_mouse_y() };
    let ldown  = unsafe { input_mouse_down(0) != 0 };
    let lclick = unsafe { input_mouse_clicked(0) != 0 };
    let tc     = unsafe { input_touch_count() };

    let fw = w as usize;
    let fh = h as usize;
    let fw_f = w as f32;
    let fh_f = h as f32;

    // ── Layout ────────────────────────────────────────────────────────────────
    let top_h   = 40usize;
    let pal_w   = 80usize;   // 2 cols × 8 rows
    let cell_w  = pal_w / 2;
    let cell_h  = (fh - top_h) / 8;
    let avail_w = fw - pal_w;
    let avail_h = fh - top_h;
    let ps      = (avail_w.min(avail_h) / 32).max(1); // pixel size
    let cdw     = ps * 32;
    let cdh     = ps * 32;
    let cx0     = pal_w + (avail_w - cdw) / 2;
    let cy0     = top_h + (avail_h - cdh) / 2;

    // Buttons in top-right (Export | Clear)
    let btn_h   = 26usize;
    let btn_y   = (top_h - btn_h) / 2;
    let clr_w   = 56usize;
    let exp_w   = 72usize;
    let gap     = 6usize;
    let clr_x   = fw.saturating_sub(clr_w + gap);
    let exp_x   = clr_x.saturating_sub(exp_w + gap);

    // ── Canvas init ───────────────────────────────────────────────────────────
    {
        let mut cv = CANVAS.lock().unwrap();
        if cv.is_empty() { cv.resize(32 * 32, 0); }
    }

    // ── Auto-save every 90 frames when dirty ──────────────────────────────────
    let frame = FRAME.fetch_add(1, Relaxed);
    if DIRTY.load(Relaxed) && frame % 90 == 0 {
        DIRTY.store(false, Relaxed);
        save_canvas(&CANVAS.lock().unwrap());
    }

    // ── Interaction ───────────────────────────────────────────────────────────

    let screen_to_cell = |px: f32, py: f32| -> Option<(usize, usize)> {
        let cx = ((px - cx0 as f32) / ps as f32) as i32;
        let cy = ((py - cy0 as f32) / ps as f32) as i32;
        if cx >= 0 && cx < 32 && cy >= 0 && cy < 32 { Some((cx as usize, cy as usize)) }
        else { None }
    };

    let sel = SEL.load(Relaxed) as usize;
    let paint_col = PAL[sel.min(15)];

    // Paint with mouse drag
    if ldown {
        if let Some((cx, cy)) = screen_to_cell(mx, my) {
            CANVAS.lock().unwrap()[cy * 32 + cx] = paint_col | 0xFF000000;
            DIRTY.store(true, Relaxed);
        }
    }
    // Paint with touch
    for i in 0..tc {
        let (tx, ty) = unsafe { (input_touch_x(i), input_touch_y(i)) };
        if let Some((cx, cy)) = screen_to_cell(tx, ty) {
            CANVAS.lock().unwrap()[cy * 32 + cx] = paint_col | 0xFF000000;
            DIRTY.store(true, Relaxed);
        }
    }

    // Palette click
    let click_palette = |px: f32, py: f32| {
        if px < pal_w as f32 && py >= top_h as f32 {
            let col = (px / cell_w as f32) as usize;
            let row = ((py - top_h as f32) / cell_h as f32) as usize;
            let idx = row * 2 + col;
            if idx < 16 { SEL.store(idx as u32, Relaxed); }
        }
    };
    if lclick { click_palette(mx, my); }
    for i in 0..tc {
        let (tx, ty) = unsafe { (input_touch_x(i), input_touch_y(i)) };
        click_palette(tx, ty);
    }

    // Export button → save as PPM via file_save dialog
    let clr_y = btn_y;
    let clr_h = btn_h;
    let exp_hover = mx >= exp_x as f32 && mx < (exp_x+exp_w) as f32
                 && my >= btn_y as f32 && my < (btn_y+btn_h) as f32;
    if lclick && exp_hover {
        export_ppm(&CANVAS.lock().unwrap());
    }

    // Clear button
    let clr_hover = mx >= clr_x as f32 && mx < (clr_x+clr_w) as f32
                 && my >= clr_y as f32 && my < (clr_y+clr_h) as f32;
    if lclick && clr_hover {
        CANVAS.lock().unwrap().iter_mut().for_each(|c| *c = 0);
        save_canvas(&CANVAS.lock().unwrap()); // save cleared state immediately
        DIRTY.store(false, Relaxed);
    }

    // ── Write framebuffer ─────────────────────────────────────────────────────
    let mut fb = FB.lock().unwrap();
    fb.resize(fw * fh * 4, 255);

    let px = |x: usize, y: usize, r: u8, g: u8, b: u8| -> usize {
        let _ = (x, y, r, g, b); 0 // placeholder; we use inline below
    };
    let _ = px;

    macro_rules! put {
        ($fb:expr, $fw:expr, $fh:expr, $x:expr, $y:expr, $r:expr, $g:expr, $b:expr) => {{
            let (x, y): (usize, usize) = ($x, $y);
            if x < $fw && y < $fh {
                let i = (y * $fw + x) * 4;
                $fb[i] = $r; $fb[i+1] = $g; $fb[i+2] = $b; $fb[i+3] = 255;
            }
        }};
    }

    // Background
    for i in (0..fw*fh*4).step_by(4) {
        fb[i] = 13; fb[i+1] = 17; fb[i+2] = 27; fb[i+3] = 255;
    }

    // ── Top bar ───────────────────────────────────────────────────────────────
    for y in 0..top_h {
        for x in 0..fw {
            put!(fb, fw, fh, x, y, 20, 27, 44);
        }
    }
    // bottom line of top bar
    for x in 0..fw { put!(fb, fw, fh, x, top_h, 38, 52, 78); }

    // Selected color swatch in top bar
    let (sr, sg, sb) = rgb(PAL[sel.min(15)]);
    for dy in 0..28usize {
        for dx in 0..28usize {
            let x = 8 + dx;
            let y = 6 + dy;
            let border = dx == 0 || dy == 0 || dx == 27 || dy == 27;
            let (r, g, b) = if border { (200,200,200) } else { (sr, sg, sb) };
            put!(fb, fw, fh, x, y, r, g, b);
        }
    }

    // Export button (green)
    let (er, eg_col, eb) = if exp_hover { (60u8, 180u8, 80u8) } else { (40u8, 120u8, 55u8) };
    for dy in 0..btn_h {
        for dx in 0..exp_w {
            let x = exp_x + dx;
            let y = btn_y + dy;
            let border = dx == 0 || dy == 0 || dx == exp_w-1 || dy == btn_h-1;
            let (r, g, b) = if border { (er+40, eg_col+30, eb+30) } else { (er, eg_col, eb) };
            put!(fb, fw, fh, x, y, r, g, b);
        }
    }
    // Draw down-arrow icon for export
    let ex_cx = exp_x + exp_w / 2;
    let ex_cy = btn_y + btn_h / 2;
    for d in -4i32..=4 {
        put!(fb, fw, fh, (ex_cx as i32 + d) as usize, (ex_cy as i32 - 3) as usize, 255, 255, 255);
    }
    for d in 0i32..=4 {
        put!(fb, fw, fh, ex_cx, (ex_cy as i32 - 3 + d) as usize, 255, 255, 255);
        put!(fb, fw, fh, (ex_cx as i32 - d) as usize, (ex_cy as i32 + d - 1) as usize, 255, 255, 255);
        put!(fb, fw, fh, (ex_cx as i32 + d) as usize, (ex_cy as i32 + d - 1) as usize, 255, 255, 255);
    }

    // Clear button (red X)
    let (br, bg_col, bb) = if clr_hover { (210u8, 60u8, 60u8) } else { (150u8, 40u8, 40u8) };
    for dy in 0..clr_h {
        for dx in 0..clr_w {
            let x = clr_x + dx;
            let y = clr_y + dy;
            let border = dx == 0 || dy == 0 || dx == clr_w-1 || dy == clr_h-1;
            let (r, g, b) = if border { (br+40, bg_col+20, bb+20) } else { (br, bg_col, bb) };
            put!(fb, fw, fh, x, y, r, g, b);
        }
    }
    // X icon on clear button
    let bx = clr_x + clr_w/2;
    let by = clr_y + clr_h/2;
    for d in -5i32..=5i32 {
        let (x1, y1) = ((bx as i32+d) as usize, (by as i32+d) as usize);
        let (x2, y2) = ((bx as i32+d) as usize, (by as i32-d) as usize);
        put!(fb, fw, fh, x1, y1, 255, 255, 255);
        put!(fb, fw, fh, x2, y2, 255, 255, 255);
    }

    // ── Palette ───────────────────────────────────────────────────────────────
    for i in 0..16usize {
        let col_i = i % 2;
        let row_i = i / 2;
        let px0 = col_i * cell_w;
        let py0 = top_h + row_i * cell_h;
        let (pr, pg, pb) = rgb(PAL[i]);
        let selected = i == sel;
        for dy in 0..cell_h {
            for dx in 0..cell_w {
                let x = px0 + dx;
                let y = py0 + dy;
                let border = dx < 2 || dy < 2 || dx >= cell_w-2 || dy >= cell_h-2;
                let (r, g, b) = if border && selected { (255,255,255) }
                    else if border { (20, 27, 42) }
                    else { (pr, pg, pb) };
                put!(fb, fw, fh, x, y, r, g, b);
            }
        }
    }
    // Right border of palette
    for y in top_h..fh { put!(fb, fw, fh, pal_w, y, 38, 52, 78); }

    // ── Canvas ────────────────────────────────────────────────────────────────
    let canvas = CANVAS.lock().unwrap();
    for cy in 0..32usize {
        for cx in 0..32usize {
            let cell = canvas[cy * 32 + cx];
            let (cr, cg, cb) = if cell == 0 {
                if (cx + cy) % 2 == 0 { (28u8, 34u8, 50u8) } else { (20u8, 26u8, 40u8) }
            } else {
                rgb(cell)
            };
            for dy in 0..ps {
                for dx in 0..ps {
                    let x = cx0 + cx * ps + dx;
                    let y = cy0 + cy * ps + dy;
                    let grid = (dx == 0 || dy == 0) && cell == 0;
                    let (r, g, b) = if grid { (18, 22, 34) } else { (cr, cg, cb) };
                    put!(fb, fw, fh, x, y, r, g, b);
                }
            }
        }
    }

    // Hover highlight on canvas
    if let Some((hcx, hcy)) = screen_to_cell(mx, my) {
        let (hr, hg, hb) = rgb(PAL[sel.min(15)]);
        for d in 0..ps {
            // top / bottom row
            put!(fb, fw, fh, cx0 + hcx*ps + d, cy0 + hcy*ps,        hr, hg, hb);
            put!(fb, fw, fh, cx0 + hcx*ps + d, cy0 + hcy*ps + ps-1, hr, hg, hb);
            // left / right col
            put!(fb, fw, fh, cx0 + hcx*ps,        cy0 + hcy*ps + d, hr, hg, hb);
            put!(fb, fw, fh, cx0 + hcx*ps + ps-1, cy0 + hcy*ps + d, hr, hg, hb);
        }
    }
}
