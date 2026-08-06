// ═══════════════════════════════════════════════════════════════════════════════
// annessaia SDK — injected automatically by annessaia-build. Do not edit here.
// Every host function is available as a free function. Wrappers at the bottom
// give you a nicer API: log(), http_get(), store(), rgba(), hit_rect(), etc.
// ═══════════════════════════════════════════════════════════════════════════════

#![allow(dead_code, unused_unsafe, non_snake_case, unused_imports)]

// ── Raw host imports ──────────────────────────────────────────────────────────

#[link(wasm_import_module = "env")]
extern "C" {
    // ── Canvas / time ────────────────────────────────────────────────────────
    /// Seconds since the app was loaded.
    pub fn get_time() -> f64;
    /// Width of the canvas in logical pixels.
    pub fn get_width() -> f32;
    /// Height of the canvas in logical pixels.
    pub fn get_height() -> f32;

    // ── GPU rendering (render_gpu mode) ──────────────────────────────────────
    /// Fill the entire canvas with a packed RGBA color (use rgba() to build one).
    pub fn gpu_clear(color: i32);
    pub fn gpu_rect(x: f32, y: f32, w: f32, h: f32, rounding: f32, color: i32);
    pub fn gpu_circle(cx: f32, cy: f32, r: f32, color: i32);
    pub fn gpu_line(x1: f32, y1: f32, x2: f32, y2: f32, color: i32, thickness: f32);
    pub fn gpu_triangle(x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: i32);

    // ── Widget UI (render mode) ───────────────────────────────────────────────
    pub fn ui_heading(ptr: *const u8, len: usize);
    pub fn ui_label(ptr: *const u8, len: usize);
    pub fn ui_small(ptr: *const u8, len: usize);
    /// Returns 1 on the frame the button was clicked.
    pub fn ui_button(ptr: *const u8, len: usize) -> i32;
    pub fn ui_separator();
    pub fn ui_space(px: f32);

    // ── Input — mouse ────────────────────────────────────────────────────────
    pub fn input_mouse_x() -> f32;
    pub fn input_mouse_y() -> f32;
    /// 0=left, 1=right, 2=middle. Returns 1 while held.
    pub fn input_mouse_down(btn: i32) -> i32;
    /// Returns 1 on the frame the button was pressed.
    pub fn input_mouse_clicked(btn: i32) -> i32;
    pub fn input_scroll_x() -> f32;
    pub fn input_scroll_y() -> f32;
    /// Pointer delta this frame (non-zero while dragging).
    pub fn input_drag_x() -> f32;
    pub fn input_drag_y() -> f32;

    // ── Input — touch ────────────────────────────────────────────────────────
    pub fn input_touch_count() -> i32;
    pub fn input_touch_x(idx: i32) -> f32;
    pub fn input_touch_y(idx: i32) -> f32;

    // ── Storage (key-value, persisted to ~/.annessaia/storage/) ──────────────
    pub fn storage_set(kp: *const u8, kl: usize, vp: *const u8, vl: usize);
    /// Writes bytes into `op..op+om`. Returns bytes written, or -1 if missing.
    pub fn storage_get(kp: *const u8, kl: usize, op: *mut u8, om: usize) -> i32;
    /// Returns 1 if the key exists.
    pub fn storage_has(kp: *const u8, kl: usize) -> i32;
    pub fn storage_delete(kp: *const u8, kl: usize);
    pub fn storage_clear();

    // ── File download ─────────────────────────────────────────────────────────
    /// Opens a native Save dialog. `dp/dl` = data bytes, `np/nl` = suggested filename.
    pub fn file_save(dp: *const u8, dl: usize, np: *const u8, nl: usize);

    // ── Clipboard ────────────────────────────────────────────────────────────
    pub fn clipboard_write(ptr: *const u8, len: usize);
    /// Returns bytes written, -1 if clipboard unavailable.
    pub fn clipboard_read(op: *mut u8, om: usize) -> i32;

    // ── System ───────────────────────────────────────────────────────────────
    /// Open a URL in the host's default browser.
    pub fn open_url(ptr: *const u8, len: usize);
    /// Print a message to the host terminal (visible when running annessaia).
    pub fn log_str(ptr: *const u8, len: usize);

    // ── Navigation / history ─────────────────────────────────────────────────
    /// Load a different .wasm URL in the same browser window.
    pub fn nav_push(ptr: *const u8, len: usize);
    pub fn nav_back();
    pub fn nav_forward();
    /// Number of URLs in the all-time history log.
    pub fn history_len() -> i32;
    /// Write URL at `idx` into `op`. Returns bytes written, -1 if out of range.
    pub fn history_get(idx: i32, op: *mut u8, om: usize) -> i32;

    // ── HTTP / fetch ─────────────────────────────────────────────────────────
    /// Synchronous GET — blocks the frame until the response arrives.
    /// Writes body into `op..op+om`. Returns bytes written, -2 on network error.
    /// Use http_get() / http_get_str() wrappers for convenience.
    pub fn fetch_sync(up: *const u8, ul: usize, op: *mut u8, om: usize) -> i32;
    /// Start an async GET identified by `id` (any i32 you choose).
    /// Use http_poll(id) to check for the result on subsequent frames.
    pub fn fetch_start(id: i32, up: *const u8, ul: usize);
    /// Poll an async request. Returns -1 while pending, -2 on error, ≥0 = done
    /// (bytes written). The result is consumed; don't call again after ≥0.
    pub fn fetch_poll(id: i32, op: *mut u8, om: usize) -> i32;
    /// Start an async POST with a UTF-8 body (JSON, form data, etc.).
    /// Poll with http_poll(id).
    pub fn fetch_post(id: i32, up: *const u8, ul: usize, bp: *const u8, bl: usize);
}

// ── Color ─────────────────────────────────────────────────────────────────────

/// Pack RGBA bytes into an i32 for gpu_* calls.
#[inline] pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> i32 {
    ((r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | a as u32) as i32
}

/// HSL → opaque i32 color. All inputs in [0.0, 1.0].
pub fn hsl(h: f32, s: f32, l: f32) -> i32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = if h < 1.0/6.0 { (c,x,0.0) } else if h < 2.0/6.0 { (x,c,0.0) }
        else if h < 3.0/6.0 { (0.0,c,x) } else if h < 4.0/6.0 { (0.0,x,c) }
        else if h < 5.0/6.0 { (x,0.0,c) } else { (c,0.0,x) };
    rgba(((r+m)*255.0) as u8, ((g+m)*255.0) as u8, ((b+m)*255.0) as u8, 255)
}

// ── AtomicU32 ↔ f32 ──────────────────────────────────────────────────────────

/// Load an f32 stored as bit-cast bits in an AtomicU32.
#[inline] pub fn af_load(a: &core::sync::atomic::AtomicU32) -> f32 {
    f32::from_bits(a.load(core::sync::atomic::Ordering::Relaxed))
}
/// Store an f32 as bit-cast bits in an AtomicU32.
#[inline] pub fn af_store(a: &core::sync::atomic::AtomicU32, v: f32) {
    a.store(v.to_bits(), core::sync::atomic::Ordering::Relaxed);
}

// ── Hit testing ───────────────────────────────────────────────────────────────

#[inline] pub fn hit_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}
#[inline] pub fn hit_circle(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> bool {
    (px - cx) * (px - cx) + (py - cy) * (py - cy) <= r * r
}

// ── Widget helpers ────────────────────────────────────────────────────────────

pub fn heading(s: &str)        { unsafe { ui_heading(s.as_ptr(), s.len()) } }
pub fn label(s: &str)          { unsafe { ui_label(s.as_ptr(), s.len()) } }
pub fn small(s: &str)          { unsafe { ui_small(s.as_ptr(), s.len()) } }
pub fn button(s: &str) -> bool { unsafe { ui_button(s.as_ptr(), s.len()) == 1 } }
pub fn space(px: f32)          { unsafe { ui_space(px) } }
pub fn separator()             { unsafe { ui_separator() } }

// ── Log ───────────────────────────────────────────────────────────────────────

/// Print a debug message to the annessaia host terminal.
pub fn log(s: &str) { unsafe { log_str(s.as_ptr(), s.len()) } }

// ── Storage ───────────────────────────────────────────────────────────────────

/// Persist arbitrary bytes under `key`.
pub fn store(key: &str, val: &[u8]) {
    unsafe { storage_set(key.as_ptr(), key.len(), val.as_ptr(), val.len()) }
}

/// Load bytes from storage into `buf`. Returns `Some(&buf[..n])` or `None`.
pub fn load<'a>(key: &str, buf: &'a mut [u8]) -> Option<&'a [u8]> {
    let n = unsafe { storage_get(key.as_ptr(), key.len(), buf.as_mut_ptr(), buf.len()) };
    if n >= 0 { Some(&buf[..n as usize]) } else { None }
}

/// Persist a UTF-8 string.
pub fn store_str(key: &str, val: &str) { store(key, val.as_bytes()) }

/// Load a UTF-8 string (up to 4 KiB). Returns `None` if the key doesn't exist.
pub fn load_str(key: &str) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { storage_get(key.as_ptr(), key.len(), buf.as_mut_ptr(), buf.len()) };
    if n < 0 { return None; }
    String::from_utf8(buf[..n as usize].to_vec()).ok()
}

/// Returns true if `key` exists in storage.
pub fn has(key: &str) -> bool {
    unsafe { storage_has(key.as_ptr(), key.len()) != 0 }
}

// ── File / clipboard ──────────────────────────────────────────────────────────

/// Open a native Save dialog and write `data` to the chosen path.
pub fn save_file(suggested_name: &str, data: &[u8]) {
    unsafe { file_save(data.as_ptr(), data.len(), suggested_name.as_ptr(), suggested_name.len()) }
}

pub fn clip_write(s: &str) { unsafe { clipboard_write(s.as_ptr(), s.len()) } }

/// Read text from the system clipboard (up to 4 KiB).
pub fn clip_read() -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { clipboard_read(buf.as_mut_ptr(), buf.len()) };
    if n < 0 { return None; }
    String::from_utf8(buf[..n as usize].to_vec()).ok()
}

// ── HTTP ──────────────────────────────────────────────────────────────────────

/// Synchronous HTTP GET. Blocks the current frame until complete.
/// Returns the response body, or `None` on network error.
/// Fine for one-shot loads (e.g. in `init()`). Use `http_start`/`http_poll` for
/// background fetches that shouldn't freeze the UI.
pub fn http_get(url: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 4 * 1024 * 1024]; // 4 MiB cap
    let n = unsafe { fetch_sync(url.as_ptr(), url.len(), buf.as_mut_ptr(), buf.len()) };
    if n < 0 { return None; }
    buf.truncate(n as usize);
    Some(buf)
}

/// Like `http_get` but decodes the body as UTF-8.
pub fn http_get_str(url: &str) -> Option<String> {
    http_get(url).and_then(|b| String::from_utf8(b).ok())
}

/// Start a background GET. `id` is any i32 you pick (used to poll later).
pub fn http_start(id: i32, url: &str) {
    unsafe { fetch_start(id, url.as_ptr(), url.len()) }
}

/// Start a background POST with a UTF-8 body.
pub fn http_post(id: i32, url: &str, body: &str) {
    unsafe { fetch_post(id, url.as_ptr(), url.len(), body.as_ptr(), body.len()) }
}

/// Poll a background request started with `http_start` or `http_post`.
/// Returns `Some(body)` when done, `None` while still in-flight.
/// `-2` (error) also returns `None`; check `http_error(id)` if you need to distinguish.
pub fn http_poll(id: i32) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let n = unsafe { fetch_poll(id, buf.as_mut_ptr(), buf.len()) };
    if n < 0 { return None; }
    buf.truncate(n as usize);
    Some(buf)
}

/// Like `http_poll` but decodes the body as UTF-8.
pub fn http_poll_str(id: i32) -> Option<String> {
    http_poll(id).and_then(|b| String::from_utf8(b).ok())
}

// ── Pixel framebuffer (render_pixels mode) ────────────────────────────────────
// Export `render_pixels(w: i32, h: i32)` from your app and the host will call it
// each frame. Use the helpers below to write into the built-in RGBA framebuffer.
// The host reads the buffer via the auto-exported `framebuffer_ptr()`.

static _ANNESSAIA_FB: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

#[no_mangle]
pub extern "C" fn framebuffer_ptr() -> i32 {
    _ANNESSAIA_FB.lock().unwrap().as_ptr() as i32
}

/// Resize the framebuffer to `w×h` RGBA pixels (no-op if already the right size).
pub fn px_resize(w: i32, h: i32) {
    let need = (w * h * 4) as usize;
    let mut fb = _ANNESSAIA_FB.lock().unwrap();
    if fb.len() != need { fb.resize(need, 0); }
}

/// Write one pixel. Clips silently.
pub fn px_set(w: i32, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
    if x < 0 || y < 0 || x >= w { return; }
    let idx = ((y * w + x) * 4) as usize;
    let mut fb = _ANNESSAIA_FB.lock().unwrap();
    if idx + 3 < fb.len() {
        fb[idx] = r; fb[idx+1] = g; fb[idx+2] = b; fb[idx+3] = a;
    }
}

/// Fill the entire framebuffer with one color.
pub fn px_clear(w: i32, h: i32, r: u8, g: u8, b: u8) {
    let mut fb = _ANNESSAIA_FB.lock().unwrap();
    let need = (w * h * 4) as usize;
    if fb.len() != need { fb.resize(need, 255); }
    for i in 0..w*h {
        let i = (i * 4) as usize;
        fb[i] = r; fb[i+1] = g; fb[i+2] = b; fb[i+3] = 255;
    }
}

/// Fill an axis-aligned rectangle.
pub fn px_rect(w: i32, h: i32, x0: i32, y0: i32, rw: i32, rh: i32, r: u8, g: u8, b: u8, a: u8) {
    let mut fb = _ANNESSAIA_FB.lock().unwrap();
    for dy in 0..rh {
        for dx in 0..rw {
            let (px, py) = (x0 + dx, y0 + dy);
            if px < 0 || py < 0 || px >= w || py >= h { continue; }
            let idx = ((py * w + px) * 4) as usize;
            if idx + 3 < fb.len() {
                fb[idx] = r; fb[idx+1] = g; fb[idx+2] = b; fb[idx+3] = a;
            }
        }
    }
}

/// Draw a filled circle into the pixel framebuffer.
pub fn px_circle(w: i32, h: i32, cx: i32, cy: i32, rad: i32, r: u8, g: u8, b: u8, a: u8) {
    let mut fb = _ANNESSAIA_FB.lock().unwrap();
    for dy in -rad..=rad {
        for dx in -rad..=rad {
            if dx*dx + dy*dy <= rad*rad {
                let (px, py) = (cx + dx, cy + dy);
                if px < 0 || py < 0 || px >= w || py >= h { continue; }
                let idx = ((py * w + px) * 4) as usize;
                if idx + 3 < fb.len() {
                    fb[idx] = r; fb[idx+1] = g; fb[idx+2] = b; fb[idx+3] = a;
                }
            }
        }
    }
}
