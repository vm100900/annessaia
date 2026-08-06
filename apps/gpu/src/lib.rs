// GPU app — draws its own UI using only gpu_* primitives. No widget system.
// Left panel: play/pause, speed slider, theme buttons — all gpu_rect/circle/triangle.
// Right area: the animated scene controlled by those inputs.

use core::f32::consts::PI;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

#[link(wasm_import_module = "env")]
extern "C" {
    fn gpu_clear(color: i32);
    fn gpu_rect(x: f32, y: f32, w: f32, h: f32, rounding: f32, color: i32);
    fn gpu_circle(cx: f32, cy: f32, r: f32, color: i32);
    fn gpu_line(x1: f32, y1: f32, x2: f32, y2: f32, color: i32, thickness: f32);
    fn gpu_triangle(x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: i32);
    fn get_time() -> f64;
    fn get_width() -> f32;
    fn get_height() -> f32;
    fn input_mouse_x() -> f32;
    fn input_mouse_y() -> f32;
    fn input_mouse_down(btn: i32) -> i32;
    fn input_mouse_clicked(btn: i32) -> i32;
    fn input_touch_count() -> i32;
    fn input_touch_x(idx: i32) -> f32;
    fn input_touch_y(idx: i32) -> f32;

    fn storage_set(kp: *const u8, kl: usize, vp: *const u8, vl: usize);
    fn storage_get(kp: *const u8, kl: usize, op: *mut u8, om: usize) -> i32;
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

fn rgba(r: u8, g: u8, b: u8, a: u8) -> i32 {
    ((r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | a as u32) as i32
}

fn hsl(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = if h < 1.0/6.0 { (c,x,0.0) } else if h < 2.0/6.0 { (x,c,0.0) }
        else if h < 3.0/6.0 { (0.0,c,x) } else if h < 4.0/6.0 { (0.0,x,c) }
        else if h < 5.0/6.0 { (x,0.0,c) } else { (c,0.0,x) };
    (((r+m)*255.0) as u8, ((g+m)*255.0) as u8, ((b+m)*255.0) as u8)
}

fn load(a: &AtomicU32) -> f32 { f32::from_bits(a.load(Relaxed)) }
fn save(a: &AtomicU32, v: f32) { a.store(v.to_bits(), Relaxed) }

fn hit_circle(mx: f32, my: f32, cx: f32, cy: f32, r: f32) -> bool {
    (mx-cx)*(mx-cx) + (my-cy)*(my-cy) <= r*r
}
fn hit_rect(mx: f32, my: f32, x: f32, y: f32, w: f32, h: f32) -> bool {
    mx>=x && mx<x+w && my>=y && my<y+h
}

// ── State ─────────────────────────────────────────────────────────────────────

static TIME_ACC:    AtomicU32 = AtomicU32::new(0);
static LAST_REAL:   AtomicU32 = AtomicU32::new(0);
static PLAYING:     AtomicU32 = AtomicU32::new(1);
static SPEED:       AtomicU32 = AtomicU32::new(0x3F800000); // 1.0
static THEME:       AtomicU32 = AtomicU32::new(0);          // 0=cyan 1=green 2=rose
static SL_DRAG:     AtomicU32 = AtomicU32::new(0);          // slider dragging
static SAVED_THEME: AtomicU32 = AtomicU32::new(u32::MAX);   // sentinel: nothing saved yet
static SAVED_SPEED: AtomicU32 = AtomicU32::new(u32::MAX);

// ── init — load persisted state ───────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn init() {
    let mut buf = [0u8; 16];
    // theme: stored as single ASCII digit
    if let Some(b) = load_bytes("gpu_theme", &mut buf) {
        if let Some(&d) = b.first() {
            if d >= b'0' && d <= b'2' {
                let t = (d - b'0') as u32;
                THEME.store(t, Relaxed);
                SAVED_THEME.store(t, Relaxed);
                log(&format!("loaded theme={t}"));
            }
        }
    }
    // speed: stored as 4 little-endian bytes (f32 bits)
    if let Some(b) = load_bytes("gpu_speed", &mut buf) {
        if b.len() == 4 {
            let bits = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            let spd = f32::from_bits(bits).clamp(0.25, 4.0);
            save(&SPEED, spd);
            SAVED_SPEED.store(bits, Relaxed);
            log(&format!("loaded speed={spd}"));
        }
    }
}

// ── render_gpu ────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn render_gpu() {
    // Delta time for pausing
    let now = unsafe { get_time() as f32 };
    let prev = load(&LAST_REAL);
    let dt = if prev > 0.0 { (now - prev).min(0.05) } else { 0.0 };
    save(&LAST_REAL, now);
    if PLAYING.load(Relaxed) != 0 {
        save(&TIME_ACC, load(&TIME_ACC) + dt * load(&SPEED));
    }
    let t = load(&TIME_ACC);

    let w  = unsafe { get_width() };
    let h  = unsafe { get_height() };
    let mx = unsafe { input_mouse_x() };
    let my = unsafe { input_mouse_y() };
    let ldown  = unsafe { input_mouse_down(0) != 0 };
    let lclick = unsafe { input_mouse_clicked(0) != 0 };

    // ── Panel layout ──────────────────────────────────────────────────────────
    let pw = 190.0f32;

    // Play/pause button
    let pp_cx = pw * 0.5;
    let pp_cy = 76.0f32;

    // Speed slider
    let sl_x  = 18.0f32;
    let sl_y  = 168.0f32;
    let sl_w  = pw - 36.0;
    // speed range [0.25, 4.0] mapped to [0, 1]
    let speed_t = (load(&SPEED) - 0.25) / 3.75;
    let sl_hx = sl_x + speed_t * sl_w;

    // Theme buttons (3 cols)
    let tb_y = 236.0f32;
    let tb_h = 38.0f32;
    let tb_gap = 8.0f32;
    let tb_w = (pw - tb_gap * 4.0) / 3.0;
    let tb_xs = [tb_gap, tb_gap*2.0 + tb_w, tb_gap*3.0 + tb_w*2.0];

    let theme = THEME.load(Relaxed);

    // ── Interaction ───────────────────────────────────────────────────────────

    // Play/pause toggle
    if lclick && hit_circle(mx, my, pp_cx, pp_cy, 34.0) {
        let p = PLAYING.load(Relaxed);
        PLAYING.store(1 - p, Relaxed);
    }

    // Slider: start drag when clicking near handle, continue while held
    let sl_drag = SL_DRAG.load(Relaxed) != 0;
    if ldown && (sl_drag || (lclick && hit_circle(mx, my, sl_hx, sl_y, 16.0))) {
        SL_DRAG.store(1, Relaxed);
        let new_speed = (((mx - sl_x) / sl_w) * 3.75 + 0.25).clamp(0.25, 4.0);
        save(&SPEED, new_speed);
    } else if !ldown {
        SL_DRAG.store(0, Relaxed);
    }

    // Theme buttons
    for i in 0..3u32 {
        if lclick && hit_rect(mx, my, tb_xs[i as usize], tb_y, tb_w, tb_h) {
            THEME.store(i, Relaxed);
        }
    }

    // Touch on panel buttons
    let tc = unsafe { input_touch_count() };
    for i in 0..tc {
        let (tx, ty) = unsafe { (input_touch_x(i), input_touch_y(i)) };
        if hit_circle(tx, ty, pp_cx, pp_cy, 34.0) {
            // toggle handled via click; touch already maps to click in most contexts
        }
        for j in 0..3u32 {
            if hit_rect(tx, ty, tb_xs[j as usize], tb_y, tb_w, tb_h) {
                THEME.store(j, Relaxed);
            }
        }
    }

    // ── Persist theme and speed on change ────────────────────────────────────
    {
        let cur_theme = THEME.load(Relaxed);
        if cur_theme != SAVED_THEME.load(Relaxed) {
            SAVED_THEME.store(cur_theme, Relaxed);
            let digit = [b'0' + cur_theme as u8];
            store_bytes("gpu_theme", &digit);
            log(&format!("saved theme={cur_theme}"));
        }
        // Save speed only when slider released (drag just ended)
        if !ldown && SL_DRAG.load(Relaxed) == 0 {
            let spd_bits = SPEED.load(Relaxed);
            if spd_bits != SAVED_SPEED.load(Relaxed) {
                SAVED_SPEED.store(spd_bits, Relaxed);
                store_bytes("gpu_speed", &spd_bits.to_le_bytes());
                log(&format!("saved speed={}", load(&SPEED)));
            }
        }
    }

    // Re-read after updates
    let playing = PLAYING.load(Relaxed) != 0;
    let theme   = THEME.load(Relaxed);
    let speed   = load(&SPEED);
    let speed_t = (speed - 0.25) / 3.75;
    let sl_hx   = sl_x + speed_t * sl_w;
    let sl_drag = SL_DRAG.load(Relaxed) != 0;

    let (tr, tg, tb_) = match theme {
        0 => (56u8,  189u8, 248u8),
        1 => (74u8,  222u8, 128u8),
        _ => (248u8, 113u8, 163u8),
    };

    // ── Draw background ───────────────────────────────────────────────────────
    unsafe { gpu_clear(rgba(8, 12, 20, 255)) };

    // ── Animation (right of panel) ────────────────────────────────────────────
    let cx = pw + (w - pw) * 0.5;
    let cy = h * 0.5;
    let r  = cy.min((w - pw) * 0.5) * 0.85;

    for i in 0..12u32 {
        let a = i as f32 * (2.0*PI/12.0);
        unsafe { gpu_line(cx, cy, cx+a.cos()*r, cy+a.sin()*r, rgba(30,41,59,100), 0.7) };
    }
    for ring in 1..=4u32 {
        let rr = r * ring as f32 / 4.5;
        for s in 0..64u32 {
            let a1 = s as f32 / 64.0 * 2.0*PI;
            let a2 = (s+1) as f32 / 64.0 * 2.0*PI;
            unsafe { gpu_line(cx+a1.cos()*rr, cy+a1.sin()*rr,
                               cx+a2.cos()*rr, cy+a2.sin()*rr, rgba(30,41,59,70), 0.5) };
        }
    }
    for i in 0..8u32 {
        let a = t*0.55 + i as f32*(2.0*PI/8.0);
        let orbit = r*0.62;
        let (ox,oy) = (cx+a.cos()*orbit, cy+a.sin()*orbit);
        let size = 22.0 + (t*2.0 + i as f32*0.9).sin()*7.0;
        let (cr,cg,cb) = hsl(i as f32/8.0, 0.9, 0.65);
        unsafe { gpu_circle(ox,oy,size+7.0,rgba(cr,cg,cb,35)); gpu_circle(ox,oy,size,rgba(cr,cg,cb,200)); }
    }
    for i in 0..12u32 {
        let a = -t*1.1 + i as f32*(2.0*PI/12.0);
        let orbit = r*0.36;
        let (cr,cg,cb) = hsl((i as f32/12.0 + t*0.04)%1.0, 0.8, 0.7);
        unsafe { gpu_circle(cx+a.cos()*orbit, cy+a.sin()*orbit, 10.0, rgba(cr,cg,cb,180)) };
    }
    for (sp,tri_r,col) in [(0.9f32,0.22f32,rgba(99,102,241,190)), (-1.3,0.12,rgba(tr,tg,tb_,190))] {
        let ta = t*sp; let tr2 = r*tri_r;
        unsafe { gpu_triangle(cx+ta.cos()*tr2, cy+ta.sin()*tr2,
                               cx+(ta+2.094).cos()*tr2, cy+(ta+2.094).sin()*tr2,
                               cx+(ta+4.189).cos()*tr2, cy+(ta+4.189).sin()*tr2, col) };
    }
    let pulse = 28.0 + (t*4.0).sin()*8.0;
    unsafe {
        gpu_circle(cx,cy,pulse+14.0,rgba(tr,tg,tb_,28));
        gpu_circle(cx,cy,pulse,     rgba(tr,tg,tb_,180));
        gpu_circle(cx,cy,pulse*0.5, rgba(255,255,255,220));
    }
    if playing {
        let scan = (t*80.0) % h;
        unsafe {
            gpu_rect(pw, scan,             w-pw, 1.5, 0.0, rgba(tr,tg,tb_,32));
            gpu_rect(pw, (scan+h*0.5)%h,   w-pw, 0.8, 0.0, rgba(tr,tg,tb_,16));
        }
    }
    // Touch circles on animation side
    for i in 0..tc {
        let (tx2, ty2) = unsafe { (input_touch_x(i), input_touch_y(i)) };
        if tx2 > pw {
            let (cr,cg,cb) = hsl(i as f32*0.33, 0.9, 0.7);
            unsafe { gpu_circle(tx2,ty2,30.0,rgba(cr,cg,cb,40)); gpu_circle(tx2,ty2,15.0,rgba(cr,cg,cb,180)); }
        }
    }

    // ── GPU-drawn panel ───────────────────────────────────────────────────────
    unsafe {
        // Shadow + body
        gpu_rect(0.0, 0.0, pw+5.0, h, 0.0, rgba(0,0,0,100));
        gpu_rect(0.0, 0.0, pw,     h, 0.0, rgba(14,20,34,255));
        // Right border accent
        gpu_line(pw, 0.0, pw, h, rgba(tr,tg,tb_,60), 1.5);
        // Title bar
        gpu_rect(0.0, 0.0, pw, 40.0, 0.0, rgba(tr/6,tg/6,tb_/5,255));
        gpu_line(0.0, 40.0, pw, 40.0, rgba(tr/3,tg/3,tb_/3,255), 1.0);
    }
    // Logo dots
    for i in 0..9u32 {
        let (cr,cg,cb) = hsl(i as f32/9.0, 0.9, 0.6);
        unsafe { gpu_circle(16.0 + i as f32*18.0, 20.0, 4.5, rgba(cr,cg,cb,255)) };
    }

    // ── Play / Pause button ───────────────────────────────────────────────────
    let pp_hover = hit_circle(mx, my, pp_cx, pp_cy, 34.0);
    unsafe {
        gpu_circle(pp_cx, pp_cy, 34.0, rgba(tr,tg,tb_, if pp_hover { 35 } else { 15 }));
        gpu_circle(pp_cx, pp_cy, 26.0, rgba(tr/4,tg/4,tb_/4+12,255));
        gpu_circle(pp_cx, pp_cy, 24.0, rgba(18,26,42,255));
    }
    if playing {
        // Pause: two vertical rects
        unsafe {
            gpu_rect(pp_cx-10.0, pp_cy-11.0, 7.0, 22.0, 2.0, rgba(tr,tg,tb_,230));
            gpu_rect(pp_cx+3.0,  pp_cy-11.0, 7.0, 22.0, 2.0, rgba(tr,tg,tb_,230));
        }
    } else {
        // Play: triangle
        unsafe { gpu_triangle(pp_cx-9.0, pp_cy-12.0, pp_cx-9.0, pp_cy+12.0, pp_cx+13.0, pp_cy, rgba(tr,tg,tb_,230)) };
    }

    // ── Divider ───────────────────────────────────────────────────────────────
    unsafe { gpu_line(12.0, 118.0, pw-12.0, 118.0, rgba(25,38,60,255), 1.0) };
    // Section bar for speed
    unsafe { gpu_rect(18.0, 132.0, 3.0, 12.0, 1.0, rgba(tr,tg,tb_,200)) };
    // Speed dots (5 dot indicator)
    let ndots = ((speed / 4.0 * 5.0).round() as u32).clamp(1, 5);
    for i in 0..5u32 {
        let active = i < ndots;
        unsafe { gpu_circle(28.0 + i as f32 * 15.0, 138.0, 4.5,
            rgba(if active{tr}else{28}, if active{tg}else{38}, if active{tb_}else{55}, if active{220}else{100})) };
    }

    // ── Speed slider ──────────────────────────────────────────────────────────
    let sl_hover = hit_circle(mx, my, sl_hx, sl_y, 16.0);
    unsafe {
        // Track
        gpu_rect(sl_x, sl_y-3.0, sl_w, 6.0, 3.0, rgba(22,32,50,255));
        // Fill
        gpu_rect(sl_x, sl_y-3.0, (sl_hx-sl_x).max(0.0), 6.0, 3.0, rgba(tr,tg,tb_,160));
        // Handle glow + circle
        gpu_circle(sl_hx, sl_y, if sl_drag{16.0}else if sl_hover{13.0}else{11.0}+5.0, rgba(tr,tg,tb_,40));
        gpu_circle(sl_hx, sl_y, if sl_drag{16.0}else if sl_hover{13.0}else{11.0},     rgba(tr,tg,tb_,255));
        gpu_circle(sl_hx, sl_y, 4.0, rgba(255,255,255,200));
    }

    // ── Divider ───────────────────────────────────────────────────────────────
    unsafe { gpu_line(12.0, sl_y+28.0, pw-12.0, sl_y+28.0, rgba(25,38,60,255), 1.0) };
    // Section bar for theme
    unsafe { gpu_rect(18.0, tb_y-18.0, 3.0, 12.0, 1.0, rgba(tr,tg,tb_,200)) };

    // ── Theme buttons ─────────────────────────────────────────────────────────
    let theme_colors = [(56u8,189u8,248u8), (74u8,222u8,128u8), (248u8,113u8,163u8)];
    for i in 0..3usize {
        let (tcr,tcg,tcb) = theme_colors[i];
        let sel = i as u32 == theme;
        let hov = hit_rect(mx, my, tb_xs[i], tb_y, tb_w, tb_h);
        unsafe {
            gpu_rect(tb_xs[i], tb_y, tb_w, tb_h, 9.0,
                rgba(if sel{tcr/5}else{14}, if sel{tcg/5}else{20}, if sel{tcb/4}else{32}, 255));
            // border
            gpu_rect(tb_xs[i]+1.0, tb_y+1.0, tb_w-2.0, tb_h-2.0, 8.0,
                rgba(tcr, tcg, tcb, if sel{60} else if hov{30} else {12}));
            // dot
            gpu_circle(tb_xs[i]+tb_w*0.5, tb_y+tb_h*0.5,
                if sel{11.0}else if hov{8.0}else{6.0}, rgba(tcr,tcg,tcb,if sel{255}else{160}));
            if sel {
                gpu_circle(tb_xs[i]+tb_w*0.5, tb_y+tb_h*0.5, 4.0, rgba(255,255,255,200));
            }
        }
    }
}
