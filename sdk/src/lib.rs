//! annessaia SDK — write WASM apps for the annessaia runtime.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use annessaia_sdk::prelude::*;
//!
//! #[no_mangle]
//! pub extern "C" fn render_gpu() {
//!     let (w, h) = canvas();
//!     clear(Color::rgb(8, 12, 22));
//!     circle(w / 2.0, h / 2.0, 60.0, Color::CYAN);
//!     if left_clicked() { sys::log("clicked!"); }
//! }
//! ```
//!
//! Three rendering modes — export exactly one:
//! - `render_gpu()`       → draw with GPU primitives
//! - `render_pixels(w,h)` → write raw RGBA pixels
//! - `render()`           → immediate-mode widget UI
//!
//! Optional: `init()` — called once when the WASM loads.

#![allow(dead_code, unused_unsafe)]

// ── Raw host imports (private) ────────────────────────────────────────────────

mod raw {
    #[link(wasm_import_module = "env")]
    extern "C" {
        // Canvas / time
        pub fn get_time() -> f64;
        pub fn get_width() -> f32;
        pub fn get_height() -> f32;

        // GPU
        pub fn gpu_clear(color: i32);
        pub fn gpu_rect(x: f32, y: f32, w: f32, h: f32, rounding: f32, color: i32);
        pub fn gpu_circle(cx: f32, cy: f32, r: f32, color: i32);
        pub fn gpu_line(x1: f32, y1: f32, x2: f32, y2: f32, color: i32, thickness: f32);
        pub fn gpu_triangle(x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: i32);

        // Widget UI — text
        pub fn ui_heading(ptr: *const u8, len: usize);
        pub fn ui_label(ptr: *const u8, len: usize);
        pub fn ui_small(ptr: *const u8, len: usize);
        pub fn ui_colored_label(ptr: *const u8, len: usize, color: i32);
        pub fn ui_code(ptr: *const u8, len: usize);

        // Widget UI — interactive
        pub fn ui_button(ptr: *const u8, len: usize) -> i32;
        pub fn ui_button_styled(ptr: *const u8, len: usize, fg: i32, bg: i32, border: i32) -> i32;
        /// Returns current checked state (host-managed).
        pub fn ui_checkbox(id: i32, ptr: *const u8, len: usize) -> i32;
        /// Returns current value (host-managed). `def` used on first call.
        pub fn ui_slider(id: i32, ptr: *const u8, len: usize, min: f32, max: f32, def: f32) -> f32;
        /// Progress bar, value in [0.0, 1.0].
        pub fn ui_progress(value: f32);
        pub fn ui_progress_text(value: f32, ptr: *const u8, len: usize);
        /// Text input. Returns bytes of current text written to out_ptr.
        pub fn ui_text_edit(id: i32, hp: *const u8, hl: usize, op: *mut u8, om: usize) -> i32;
        pub fn ui_text_edit_secret(id: i32, hp: *const u8, hl: usize, op: *mut u8, om: usize) -> i32;

        // Widget UI — text/badge
        pub fn ui_text(ptr: *const u8, len: usize, size: f32, color: i32);
        pub fn ui_badge(ptr: *const u8, len: usize, color: i32);

        // Widget UI — layout
        pub fn ui_row_begin();
        pub fn ui_card_color_begin(color: i32);
        pub fn ui_row_end();
        pub fn ui_card_begin();
        pub fn ui_card_end();
        pub fn ui_columns_begin(n: i32);
        pub fn ui_column_next();
        pub fn ui_columns_end();

        // Widget UI — misc
        pub fn ui_separator();
        pub fn ui_space(px: f32);

        // Input
        pub fn input_mouse_x() -> f32;
        pub fn input_mouse_y() -> f32;
        pub fn input_mouse_down(btn: i32) -> i32;
        pub fn input_mouse_clicked(btn: i32) -> i32;
        pub fn input_scroll_x() -> f32;
        pub fn input_scroll_y() -> f32;
        pub fn input_drag_x() -> f32;
        pub fn input_drag_y() -> f32;
        pub fn input_touch_count() -> i32;
        pub fn input_touch_x(idx: i32) -> f32;
        pub fn input_touch_y(idx: i32) -> f32;
        pub fn input_key_down(code: i32) -> i32;
        pub fn input_key_pressed(code: i32) -> i32;
        pub fn input_key_released(code: i32) -> i32;
        pub fn input_modifiers() -> i32;
        pub fn input_text(ptr: *mut u8, max: usize) -> i32;

        // Storage
        pub fn storage_set(kp: *const u8, kl: usize, vp: *const u8, vl: usize);
        pub fn storage_get(kp: *const u8, kl: usize, op: *mut u8, om: usize) -> i32;
        pub fn storage_has(kp: *const u8, kl: usize) -> i32;
        pub fn storage_delete(kp: *const u8, kl: usize);
        pub fn storage_clear();

        // File / clipboard / system
        pub fn file_save(dp: *const u8, dl: usize, np: *const u8, nl: usize);
        pub fn file_pick(fp: *const u8, fl: usize) -> i32;
        pub fn file_pick_data(ptr: *mut u8, max: usize) -> i32;
        pub fn file_pick_name(ptr: *mut u8, max: usize) -> i32;
        pub fn clipboard_write(ptr: *const u8, len: usize);
        pub fn clipboard_read(op: *mut u8, om: usize) -> i32;
        pub fn open_url(ptr: *const u8, len: usize);
        pub fn log_str(ptr: *const u8, len: usize);

        // Navigation / history
        pub fn nav_push(ptr: *const u8, len: usize);
        pub fn nav_back();
        pub fn nav_forward();
        pub fn history_len() -> i32;
        pub fn history_get(idx: i32, op: *mut u8, om: usize) -> i32;

        // HTTP
        pub fn fetch_sync(up: *const u8, ul: usize, op: *mut u8, om: usize) -> i32;
        pub fn fetch_start(id: i32, up: *const u8, ul: usize);
        pub fn fetch_poll(id: i32, op: *mut u8, om: usize) -> i32;
        pub fn fetch_post(id: i32, up: *const u8, ul: usize, bp: *const u8, bl: usize);

        // Local text embedding — runs on the host, never leaves the machine.
        // Shares fetch_poll's result slot: an id used here polls the same way an
        // id used for fetch_start does.
        pub fn embed_start(id: i32, tp: *const u8, tl: usize);
        /// Like `embed_start`, but framed as a document rather than a query —
        /// a different, host-side prompt convention that produces vectors
        /// comparable against other apps' indexed content, not against a
        /// search query. A sibling function rather than a flag, matching how
        /// `fetch_start`/`fetch_post` are already split by shape rather than
        /// parameterized.
        pub fn embed_start_doc(id: i32, tp: *const u8, tl: usize);

        // Sound — decoding happens host-side, so the guest just hands over raw
        // audio bytes (WAV/MP3/OGG/FLAC). Synchronous: unlike fetch/embed there
        // is no download or model load in this path to justify polling.
        pub fn sound_play(ptr: *const u8, len: usize);
        pub fn sound_play_looped(ptr: *const u8, len: usize) -> i32;
        pub fn sound_stop(handle: i32);
        pub fn sound_set_volume(handle: i32, volume: f32);

        // Image — decoding happens host-side (PNG/JPEG/...), so the guest just
        // hands over raw file bytes. Synchronous, same reasoning as sound.
        pub fn image_decode(ptr: *const u8, len: usize) -> i32;
        pub fn image_width(id: i32) -> i32;
        pub fn image_height(id: i32) -> i32;
        pub fn gpu_image(id: i32, x: f32, y: f32, w: f32, h: f32);
        pub fn ui_image(id: i32, w: f32, h: f32);

        // Assets bundled in this app's .wasmpackage (empty if it's a bare
        // .wasm — nothing to load). Already in memory host-side by the time
        // any guest code runs, so this is a synchronous lookup, not a fetch.
        pub fn asset_load(np: *const u8, nl: usize, op: *mut u8, om: usize) -> i32;
    }
}

// ── Color ─────────────────────────────────────────────────────────────────────

/// A packed RGBA color, ready to pass to any GPU or pixel function.
///
/// ```rust,no_run
/// let sky   = Color::rgb(100, 180, 240);
/// let glow  = Color::hsl(0.55, 0.9, 0.6);
/// let faded = sky.fade(0.4);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Color(pub i32);

impl Color {
    /// Pack red, green, blue, alpha bytes into a Color.
    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | a as u32) as i32)
    }

    /// Fully-opaque RGB color.
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    /// Color from HSL. All inputs in [0.0, 1.0].
    pub fn hsl(h: f32, s: f32, l: f32) -> Self {
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
        let m = l - c / 2.0;
        let (r, g, b) = if h < 1.0/6.0      { (c, x, 0.0) }
            else if h < 2.0/6.0 { (x, c, 0.0) }
            else if h < 3.0/6.0 { (0.0, c, x) }
            else if h < 4.0/6.0 { (0.0, x, c) }
            else if h < 5.0/6.0 { (x, 0.0, c) }
            else                { (c, 0.0, x) };
        Self::rgb(
            ((r + m) * 255.0) as u8,
            ((g + m) * 255.0) as u8,
            ((b + m) * 255.0) as u8,
        )
    }

    /// Return this color with a different alpha. `a` is in [0.0, 1.0].
    #[inline]
    pub fn fade(self, a: f32) -> Self {
        let a = (a.clamp(0.0, 1.0) * 255.0) as u8;
        let bits = self.0 as u32;
        Self(((bits & 0xFFFFFF00) | a as u32) as i32)
    }

    /// Return this color with a different alpha. `a` is in [0, 255].
    #[inline]
    pub fn with_alpha(self, a: u8) -> Self {
        let bits = self.0 as u32;
        Self(((bits & 0xFFFFFF00) | a as u32) as i32)
    }

    /// Linearly interpolate toward `other`. `t = 0.0` is `self`, `t = 1.0` is `other`.
    pub fn lerp(self, other: Color, t: f32) -> Self {
        let (ar, ag, ab, aa) = self.parts();
        let (br, bg, bb, ba) = other.parts();
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
        Self::rgba(f(ar, br), f(ag, bg), f(ab, bb), f(aa, ba))
    }

    fn parts(self) -> (u8, u8, u8, u8) {
        let c = self.0 as u32;
        ((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8)
    }

    pub const BLACK:       Color = Color::rgb(0,   0,   0);
    pub const WHITE:       Color = Color::rgb(255, 255, 255);
    pub const RED:         Color = Color::rgb(239, 68,  68);
    pub const GREEN:       Color = Color::rgb(74,  222, 128);
    pub const BLUE:        Color = Color::rgb(96,  165, 250);
    pub const CYAN:        Color = Color::rgb(56,  189, 248);
    pub const MAGENTA:     Color = Color::rgb(248, 113, 163);
    pub const YELLOW:      Color = Color::rgb(250, 204, 21);
    pub const ORANGE:      Color = Color::rgb(251, 146, 60);
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
}

// ── GPU module ────────────────────────────────────────────────────────────────

/// GPU draw primitives. All coordinates are in logical pixels from the canvas origin.
///
/// Use in a `render_gpu()` export:
/// ```rust,no_run
/// use annessaia_sdk::prelude::*;
/// #[no_mangle]
/// pub extern "C" fn render_gpu() {
///     clear(Color::BLACK);
///     circle(400.0, 300.0, 50.0, Color::CYAN);
/// }
/// ```
pub mod gpu {
    use super::{Color, raw, image::Image};

    /// Seconds elapsed since the app loaded.
    #[inline] pub fn time() -> f32  { unsafe { raw::get_time() as f32 } }
    /// Canvas width in logical pixels.
    #[inline] pub fn width() -> f32  { unsafe { raw::get_width() } }
    /// Canvas height in logical pixels.
    #[inline] pub fn height() -> f32 { unsafe { raw::get_height() } }
    /// `(width, height)` in one call.
    #[inline] pub fn canvas() -> (f32, f32) { (width(), height()) }

    /// Fill the canvas with `color`.
    #[inline] pub fn clear(color: Color)  { unsafe { raw::gpu_clear(color.0) } }

    /// Filled rectangle with optional corner rounding.
    #[inline] pub fn rect(x: f32, y: f32, w: f32, h: f32, rounding: f32, color: Color) {
        unsafe { raw::gpu_rect(x, y, w, h, rounding, color.0) }
    }

    /// Filled rectangle with 8 px corner rounding.
    #[inline] pub fn box_(x: f32, y: f32, w: f32, h: f32, color: Color) {
        unsafe { raw::gpu_rect(x, y, w, h, 8.0, color.0) }
    }

    /// Filled circle.
    #[inline] pub fn circle(cx: f32, cy: f32, r: f32, color: Color) {
        unsafe { raw::gpu_circle(cx, cy, r, color.0) }
    }

    /// Stroked line.
    #[inline] pub fn line(x1: f32, y1: f32, x2: f32, y2: f32, color: Color, thickness: f32) {
        unsafe { raw::gpu_line(x1, y1, x2, y2, color.0, thickness) }
    }

    /// Thin (1 px) line shorthand.
    #[inline] pub fn line1(x1: f32, y1: f32, x2: f32, y2: f32, color: Color) {
        unsafe { raw::gpu_line(x1, y1, x2, y2, color.0, 1.0) }
    }

    /// Filled triangle.
    #[inline] pub fn triangle(
        x1: f32, y1: f32,
        x2: f32, y2: f32,
        x3: f32, y3: f32,
        color: Color,
    ) {
        unsafe { raw::gpu_triangle(x1, y1, x2, y2, x3, y3, color.0) }
    }

    /// Draw a decoded [`image::Image`](super::image::Image) at `(x, y)`,
    /// stretched to `w × h`.
    #[inline] pub fn image(img: Image, x: f32, y: f32, w: f32, h: f32) {
        unsafe { raw::gpu_image(img.id, x, y, w, h) }
    }
}

// ── Input module ──────────────────────────────────────────────────────────────

/// Query mouse, keyboard, touch, and scroll state.
///
/// All values are in canvas-local coordinates (origin = top-left of the canvas).
pub mod input {
    use super::raw;

    /// Mouse position `(x, y)`.
    #[inline] pub fn mouse() -> (f32, f32) { (mouse_x(), mouse_y()) }
    #[inline] pub fn mouse_x() -> f32 { unsafe { raw::input_mouse_x() } }
    #[inline] pub fn mouse_y() -> f32 { unsafe { raw::input_mouse_y() } }

    /// `true` while the left button is held.
    #[inline] pub fn left_down()    -> bool { unsafe { raw::input_mouse_down(0) != 0 } }
    /// `true` while the right button is held.
    #[inline] pub fn right_down()   -> bool { unsafe { raw::input_mouse_down(1) != 0 } }
    /// `true` while the middle button is held.
    #[inline] pub fn middle_down()  -> bool { unsafe { raw::input_mouse_down(2) != 0 } }

    /// `true` on the frame the left button is pressed.
    #[inline] pub fn left_clicked()  -> bool { unsafe { raw::input_mouse_clicked(0) != 0 } }
    /// `true` on the frame the right button is pressed.
    #[inline] pub fn right_clicked() -> bool { unsafe { raw::input_mouse_clicked(1) != 0 } }

    /// Scroll delta this frame `(horizontal, vertical)`.
    #[inline] pub fn scroll()  -> (f32, f32) { unsafe { (raw::input_scroll_x(), raw::input_scroll_y()) } }
    /// Pointer drag delta this frame `(dx, dy)`.
    #[inline] pub fn drag()    -> (f32, f32) { unsafe { (raw::input_drag_x(),   raw::input_drag_y()) } }

    /// Number of active touch points.
    #[inline] pub fn touch_count() -> usize { unsafe { raw::input_touch_count() as usize } }
    /// Position of touch point `i`.
    #[inline] pub fn touch(i: usize) -> (f32, f32) {
        unsafe { (raw::input_touch_x(i as i32), raw::input_touch_y(i as i32)) }
    }

    /// `true` if the mouse is inside the given rectangle.
    #[inline] pub fn hover_rect(x: f32, y: f32, w: f32, h: f32) -> bool {
        let (mx, my) = mouse();
        mx >= x && mx < x + w && my >= y && my < y + h
    }

    /// `true` if the mouse is inside the given circle.
    #[inline] pub fn hover_circle(cx: f32, cy: f32, r: f32) -> bool {
        let (mx, my) = mouse();
        (mx - cx) * (mx - cx) + (my - cy) * (my - cy) <= r * r
    }

    // ── Keyboard ──────────────────────────────────────────────────────────────

    /// A key on the keyboard.
    ///
    /// These are physical keys by position, not by the character they produce —
    /// [`Key::Q`] is the same key whatever the layout. For text entry use
    /// [`typed`], which respects layout, dead keys and IME composition.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[repr(i32)]
    pub enum Key {
        A = 0, B, C, D, E, F, G, H, I, J, K, L, M,
        N, O, P, Q, R, S, T, U, V, W, X, Y, Z,

        Num0 = 26, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9,

        Left = 36, Right, Up, Down,

        Space = 40, Enter, Escape, Tab, Backspace, Delete,
        Insert, Home, End, PageUp, PageDown,

        Minus = 51, Plus, Equals, Comma, Period, Slash,
        Backslash, Semicolon, Colon, Backtick,
        OpenBracket, CloseBracket, Pipe, Question,

        F1 = 70, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    }

    /// Is the key held down right now?
    #[inline] pub fn key_down(k: Key) -> bool { unsafe { raw::input_key_down(k as i32) != 0 } }

    /// Did the key go down this frame? Auto-repeat is filtered out, so this fires
    /// once per physical press however long the key is held.
    #[inline] pub fn key_pressed(k: Key) -> bool { unsafe { raw::input_key_pressed(k as i32) != 0 } }

    /// Did the key come up this frame?
    #[inline] pub fn key_released(k: Key) -> bool { unsafe { raw::input_key_released(k as i32) != 0 } }

    #[inline] pub fn shift() -> bool { unsafe { raw::input_modifiers() & 1 != 0 } }
    #[inline] pub fn ctrl()  -> bool { unsafe { raw::input_modifiers() & 2 != 0 } }
    #[inline] pub fn alt()   -> bool { unsafe { raw::input_modifiers() & 4 != 0 } }
    /// Command on macOS, Control elsewhere — the platform's shortcut modifier.
    #[inline] pub fn cmd()   -> bool { unsafe { raw::input_modifiers() & 8 != 0 } }

    /// Characters typed this frame, already composed by the platform.
    ///
    /// Empty on most frames. Prefer this to assembling text from [`key_down`],
    /// which cannot account for keyboard layout or accented input.
    pub fn typed() -> String {
        let mut buf = [0u8; 256];
        let n = unsafe { raw::input_text(buf.as_mut_ptr(), buf.len()) };
        if n <= 0 { return String::new(); }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    /// `(x, y)` in −1..1 from the arrow keys and WASD, ready to scale by speed.
    /// Diagonals are normalised so they are not faster than the axes.
    pub fn axis() -> (f32, f32) {
        let mut x = 0.0;
        let mut y = 0.0;
        if key_down(Key::Left)  || key_down(Key::A) { x -= 1.0; }
        if key_down(Key::Right) || key_down(Key::D) { x += 1.0; }
        if key_down(Key::Up)    || key_down(Key::W) { y -= 1.0; }
        if key_down(Key::Down)  || key_down(Key::S) { y += 1.0; }
        if x != 0.0 && y != 0.0 {
            const INV: f32 = core::f32::consts::FRAC_1_SQRT_2;
            x *= INV; y *= INV;
        }
        (x, y)
    }
}

// ── Storage module ────────────────────────────────────────────────────────────

/// Persistent key-value storage, backed by `~/.annessaia/storage/`.
///
/// Values survive app reloads and host restarts.
///
/// ```rust,no_run
/// use annessaia_sdk::storage;
/// storage::set_str("high_score", "42");
/// if let Some(s) = storage::get_str("high_score") { /* ... */ }
/// ```
pub mod storage {
    use super::raw;

    /// Store raw bytes.
    pub fn set(key: &str, val: &[u8]) {
        unsafe { raw::storage_set(key.as_ptr(), key.len(), val.as_ptr(), val.len()) }
    }

    /// Store a UTF-8 string.
    #[inline] pub fn set_str(key: &str, val: &str) { set(key, val.as_bytes()) }

    /// Load raw bytes. Returns `None` if the key doesn't exist.
    pub fn get(key: &str) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 64 * 1024];
        let n = unsafe { raw::storage_get(key.as_ptr(), key.len(), buf.as_mut_ptr(), buf.len()) };
        if n < 0 { return None; }
        buf.truncate(n as usize);
        Some(buf)
    }

    /// Load a UTF-8 string. Returns `None` if the key doesn't exist.
    pub fn get_str(key: &str) -> Option<String> {
        get(key).and_then(|b| String::from_utf8(b).ok())
    }

    /// Load and parse as a type that implements `FromStr`.
    pub fn get_parsed<T: std::str::FromStr>(key: &str) -> Option<T> {
        get_str(key).and_then(|s| s.parse().ok())
    }

    /// Returns `true` if `key` exists.
    pub fn has(key: &str) -> bool {
        unsafe { raw::storage_has(key.as_ptr(), key.len()) != 0 }
    }

    /// Delete a key.
    pub fn del(key: &str) {
        unsafe { raw::storage_delete(key.as_ptr(), key.len()) }
    }

    /// Delete all keys in storage.
    pub fn clear() { unsafe { raw::storage_clear() } }
}

// ── Net module ────────────────────────────────────────────────────────────────

/// HTTP networking. Sync fetches are fine in `init()`; use async in `render_*`.
///
/// ```rust,no_run
/// use annessaia_sdk::{net, prelude::*};
///
/// #[no_mangle]
/// pub extern "C" fn init() {
///     net::get(1, "https://httpbin.org/get"); // start async GET with id=1
/// }
///
/// #[no_mangle]
/// pub extern "C" fn render_gpu() {
///     if let Some(body) = net::poll_str(1) {
///         sys::log(&format!("got {} bytes", body.len()));
///     }
/// }
/// ```
/// Local text embedding — runs entirely on the host (no network, no external
/// service), so it works the same whether the app is talking to a self-hosted
/// node or a read-only, keyword-only index. A registry that receives the
/// resulting vector alongside a search query can rank by meaning, not just
/// shared words, even if it has no way to run a model itself.
///
/// Async only, by the same reasoning as `net`: embedding a short query still
/// takes real time, and — the first time it's ever called on a given machine —
/// may need to download a model first. Blocking the frame for that would freeze
/// the UI; polling across frames keeps it responsive throughout.
///
/// ```rust,no_run
/// use annessaia_sdk::prelude::*;
/// // In a click handler:
/// embed::start(1, "pixel art drawing tool");
/// // On later frames:
/// if let Some(vec_b64) = embed::poll_str(1) {
///     // append &vec=<vec_b64> to a /api/search request
/// }
/// ```
pub mod embed {
    use super::raw;

    /// Start embedding `text` on the host. Identify the request by any `id` —
    /// shares its result slot with `net`'s async requests, so pick an id that
    /// isn't also in flight as a fetch.
    pub fn start(id: i32, text: &str) {
        unsafe { raw::embed_start(id, text.as_ptr(), text.len()) }
    }

    /// Like [`start`], but for indexing an app's own content rather than a
    /// search query — e.g. a gallery embedding a blob of all its widgets'
    /// names and descriptions, so it can be found by things that never
    /// appear in its own name/tags/description. The resulting vector is
    /// meant to be sent to a registry as an *additional* vector alongside
    /// the normal one, not a replacement for it, and — like every embedding
    /// in this SDK — only the vector should ever leave the machine, never
    /// the text it was computed from.
    ///
    /// ```rust,no_run
    /// use annessaia_sdk::prelude::*;
    /// // Once, e.g. in init():
    /// embed::start_doc(1, "Buttons: click, styled, danger, success, ghost. \
    ///                       Sliders: drag to set a value. Sound: play, loop, stop, volume.");
    /// // On later frames:
    /// if let Some(doc_vec_b64) = embed::poll_str(1) {
    ///     // include doc_vec_b64 as an extra field when submitting this app
    /// }
    /// ```
    pub fn start_doc(id: i32, text: &str) {
        unsafe { raw::embed_start_doc(id, text.as_ptr(), text.len()) }
    }

    /// Poll an embedding started with `start`. Returns a base64-encoded,
    /// quantized vector once ready. `None` while pending — and, once the
    /// request is no longer pending, also `None` if the host has no local
    /// model available, which callers should treat as "no semantic search
    /// right now" and fall back to a plain keyword search.
    ///
    /// This collapses "still working" and "will never work" into the same
    /// `None` — indistinguishable to a caller, and indistinguishable to a user
    /// watching the UI, which looks identical whether a large model is still
    /// loading for the first time or has failed outright. Prefer
    /// [`poll_result_str`] when the UI should say which.
    pub fn poll_str(id: i32) -> Option<String> {
        super::net::poll_str(id)
    }

    /// Like [`poll_str`], but distinguishes still-pending from genuinely
    /// unavailable — use this to show "computing…" instead of going silent
    /// while a model is loading for the first time.
    pub fn poll_result_str(id: i32) -> super::net::PollStr {
        super::net::poll_result_str(id)
    }
}

/// Play sound. Decoding happens on the host — bytes go in as WAV, MP3, OGG, or
/// FLAC, whatever you embed with `include_bytes!` — so no format handling is
/// needed on the guest side.
///
/// Two shapes: [`play`] is fire-and-forget (several may overlap — good for UI
/// clicks and effects), [`play_looped`] returns a handle so it can be stopped
/// or have its volume adjusted later (good for background music). If a
/// machine has no audio output, both are silent no-ops rather than errors —
/// there is nothing a WASM app could usefully do about a missing sound card.
///
/// ```rust,no_run
/// use annessaia_sdk::prelude::*;
/// static CLICK: &[u8] = include_bytes!("click.wav");
/// static MUSIC: &[u8] = include_bytes!("theme.ogg");
///
/// if button(" Play ") { sound::play(CLICK); }
///
/// // Once, e.g. in init():
/// let handle = sound::play_looped(MUSIC);
/// // Later:
/// sound::set_volume(handle, 0.4);
/// sound::stop(handle);
/// ```
pub mod sound {
    use super::raw;

    /// Play `bytes` once. Returns immediately — does not wait for playback to
    /// finish. Several calls can overlap.
    pub fn play(bytes: &[u8]) {
        unsafe { raw::sound_play(bytes.as_ptr(), bytes.len()) }
    }

    /// Play `bytes` on a loop until [`stop`] is called. Returns a handle for
    /// that purpose, or `-1` if playback couldn't start (no audio output, or
    /// the bytes didn't decode) — a caller should treat `-1` as "there is
    /// nothing to stop", not pass it to [`stop`]/[`set_volume`] expecting them
    /// to no-op safely on an unrelated handle.
    pub fn play_looped(bytes: &[u8]) -> i32 {
        unsafe { raw::sound_play_looped(bytes.as_ptr(), bytes.len()) }
    }

    /// Stop a loop started with [`play_looped`].
    pub fn stop(handle: i32) {
        unsafe { raw::sound_stop(handle) }
    }

    /// Set the volume of a loop started with [`play_looped`]. `1.0` is the
    /// clip's original level; higher amplifies, lower quiets it.
    pub fn set_volume(handle: i32, volume: f32) {
        unsafe { raw::sound_set_volume(handle, volume) }
    }
}

/// Decode and draw images. Decoding (PNG, JPEG, and several other formats)
/// happens on the host — bytes go in as whatever you embed with
/// `include_bytes!`, so no format handling is needed on the guest side.
///
/// Synchronous, like [`sound`]: decoding a typical image is fast enough that
/// this doesn't need the poll-across-frames dance `net`/`embed` use for
/// slower work.
///
/// ```rust,no_run
/// use annessaia_sdk::prelude::*;
/// static LOGO: &[u8] = include_bytes!("logo.png");
///
/// #[no_mangle]
/// pub extern "C" fn render_gpu() {
///     if let Some(logo) = image::decode(LOGO) {
///         gpu::image(logo, 20.0, 20.0, logo.width as f32, logo.height as f32);
///     }
/// }
/// ```
pub mod image {
    use super::raw;

    /// A decoded image, ready to draw. Cheap to copy — holds an opaque
    /// host-side handle plus the dimensions decoding already told us.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct Image {
        pub(crate) id: i32,
        pub width: i32,
        pub height: i32,
    }

    /// Decode image bytes (PNG, JPEG, and more). Returns `None` if the host
    /// doesn't recognize the format or the bytes are corrupt.
    ///
    /// Decoding the same bytes repeatedly re-decodes every time — call this
    /// once (e.g. in `init()`) and hold on to the returned [`Image`], the
    /// same way you'd hold a `sound::play_looped` handle.
    pub fn decode(bytes: &[u8]) -> Option<Image> {
        let id = unsafe { raw::image_decode(bytes.as_ptr(), bytes.len()) };
        if id < 0 { return None; }
        let width  = unsafe { raw::image_width(id) };
        let height = unsafe { raw::image_height(id) };
        Some(Image { id, width, height })
    }
}

/// Files bundled alongside this app in a `.wasmpackage` — the format
/// `annessaia build` produces automatically once a project has an `assets/`
/// folder (see `annessaia new`). A bare `.wasm` load (no package) has no
/// assets at all; every [`load`] call returns `None` in that case, the same
/// way a missing key does for [`storage`](super::storage).
///
/// ```rust,no_run
/// use annessaia_sdk::prelude::*;
/// // assets/sprite.png inside the project becomes "sprite.png" here —
/// // paths are relative to the assets/ folder, not the archive root.
/// if let Some(bytes) = assets::load("sprite.png") {
///     let sprite = image::decode(&bytes);
/// }
/// ```
pub mod assets {
    use super::raw;

    /// Load a bundled asset by its path under `assets/` (e.g. `"sprite.png"`,
    /// `"sfx/hit.wav"`). Returns `None` if this app wasn't loaded from a
    /// package, or the package has no file at that path.
    pub fn load(name: &str) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 8 * 1024 * 1024];
        let n = unsafe { raw::asset_load(name.as_ptr(), name.len(), buf.as_mut_ptr(), buf.len()) };
        if n < 0 { return None; }
        buf.truncate(n as usize);
        Some(buf)
    }
}

pub mod net {
    use super::raw;

    /// Blocking GET. Freezes the frame until the response arrives.
    /// Ideal for `init()`. Returns the body bytes, or `None` on error.
    pub fn fetch(url: &str) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        let n = unsafe { raw::fetch_sync(url.as_ptr(), url.len(), buf.as_mut_ptr(), buf.len()) };
        if n < 0 { return None; }
        buf.truncate(n as usize);
        Some(buf)
    }

    /// Blocking GET, decoded as UTF-8.
    pub fn fetch_str(url: &str) -> Option<String> {
        fetch(url).and_then(|b| String::from_utf8(b).ok())
    }

    /// Start an async GET. Identify the request by any `id` you choose.
    /// Check the result with [`poll`] on subsequent frames.
    pub fn get(id: i32, url: &str) {
        unsafe { raw::fetch_start(id, url.as_ptr(), url.len()) }
    }

    /// Start an async POST with a UTF-8 body (JSON, form data, etc.).
    pub fn post(id: i32, url: &str, body: &str) {
        unsafe { raw::fetch_post(id, url.as_ptr(), url.len(), body.as_ptr(), body.len()) }
    }

    /// Start an async POST with a raw byte body — for uploading files and other
    /// binary payloads that would not survive being treated as UTF-8.
    pub fn post_bytes(id: i32, url: &str, body: &[u8]) {
        unsafe { raw::fetch_post(id, url.as_ptr(), url.len(), body.as_ptr(), body.len()) }
    }

    /// Poll an async request.
    /// Returns `Some(body)` when done, `None` while in-flight or on error.
    /// The result is consumed — calling again returns `None`.
    ///
    /// Because this collapses "pending" and "failed" into `None`, a UI built on
    /// it can wait forever on a request that already failed. Prefer
    /// [`poll_result`] when you need to show an error state.
    pub fn poll(id: i32) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        let n = unsafe { raw::fetch_poll(id, buf.as_mut_ptr(), buf.len()) };
        if n < 0 { return None; }
        buf.truncate(n as usize);
        Some(buf)
    }

    /// Like [`poll`] but decodes the body as UTF-8.
    pub fn poll_str(id: i32) -> Option<String> {
        poll(id).and_then(|b| String::from_utf8(b).ok())
    }

    /// Outcome of an async request.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Poll {
        /// Still in flight — check again next frame.
        Pending,
        /// Finished; carries the response body.
        Done(Vec<u8>),
        /// The request failed (connection refused, DNS, non-2xx status, …).
        Failed,
    }

    /// Poll an async request, distinguishing "still waiting" from "failed".
    /// The result is consumed once it is `Done` or `Failed`.
    pub fn poll_result(id: i32) -> Poll {
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        let n = unsafe { raw::fetch_poll(id, buf.as_mut_ptr(), buf.len()) };
        match n {
            -1 => Poll::Pending,
            n if n < 0 => Poll::Failed,
            n => { buf.truncate(n as usize); Poll::Done(buf) }
        }
    }

    /// Like [`poll_result`] but decodes the body as UTF-8.
    /// Invalid UTF-8 is reported as [`Poll::Failed`].
    pub fn poll_result_str(id: i32) -> PollStr {
        match poll_result(id) {
            Poll::Pending => PollStr::Pending,
            Poll::Failed  => PollStr::Failed,
            Poll::Done(b) => match String::from_utf8(b) {
                Ok(s)  => PollStr::Done(s),
                Err(_) => PollStr::Failed,
            },
        }
    }

    /// String-bodied counterpart to [`Poll`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PollStr { Pending, Done(String), Failed }
}

// ── Sys module ────────────────────────────────────────────────────────────────

/// System utilities: logging, clipboard, file save, browser navigation.
pub mod sys {
    use super::raw;

    /// Print a message to the annessaia host terminal.
    pub fn log(msg: &str) { unsafe { raw::log_str(msg.as_ptr(), msg.len()) } }

    /// Open a native Save dialog and write `data` to the chosen path.
    pub fn save(filename: &str, data: &[u8]) {
        unsafe { raw::file_save(data.as_ptr(), data.len(), filename.as_ptr(), filename.len()) }
    }

    /// Write text to the system clipboard.
    pub fn copy(text: &str) { unsafe { raw::clipboard_write(text.as_ptr(), text.len()) } }

    /// Read text from the system clipboard. Returns `None` if unavailable.
    pub fn paste() -> Option<String> {
        let mut buf = vec![0u8; 4096];
        let n = unsafe { raw::clipboard_read(buf.as_mut_ptr(), buf.len()) };
        if n < 0 { return None; }
        String::from_utf8(buf[..n as usize].to_vec()).ok()
    }

    /// Open a URL in the host's default browser.
    pub fn open(url: &str) { unsafe { raw::open_url(url.as_ptr(), url.len()) } }

    /// Ask the user to choose a file, returning `(filename, contents)`.
    ///
    /// `ext` is a comma-separated extension filter, e.g. `"wasm"` or `"png,jpg"`;
    /// pass `""` to accept anything. Returns `None` if the dialog was cancelled.
    ///
    /// Blocks the frame while the dialog is open, so call it from a click rather
    /// than every frame.
    pub fn pick_file(ext: &str) -> Option<(String, Vec<u8>)> {
        let len = unsafe { raw::file_pick(ext.as_ptr(), ext.len()) };
        if len < 0 { return None; }

        let mut data = vec![0u8; len as usize];
        let n = unsafe { raw::file_pick_data(data.as_mut_ptr(), data.len()) };
        if n < 0 { return None; }
        data.truncate(n as usize);

        let mut namebuf = [0u8; 512];
        let n = unsafe { raw::file_pick_name(namebuf.as_mut_ptr(), namebuf.len()) };
        let name = if n > 0 {
            String::from_utf8_lossy(&namebuf[..n as usize]).into_owned()
        } else {
            String::new()
        };
        Some((name, data))
    }

    /// Navigate the annessaia browser to a different `.wasm` URL.
    pub fn nav(url: &str) { unsafe { raw::nav_push(url.as_ptr(), url.len()) } }

    pub fn back()    { unsafe { raw::nav_back() } }
    pub fn forward() { unsafe { raw::nav_forward() } }

    /// Number of entries in the all-time URL history.
    pub fn history_len() -> i32 { unsafe { raw::history_len() } }

    /// Get the URL at position `idx` in the history log.
    pub fn history_entry(idx: i32) -> Option<String> {
        let mut buf = vec![0u8; 512];
        let n = unsafe { raw::history_get(idx, buf.as_mut_ptr(), buf.len()) };
        if n < 0 { return None; }
        String::from_utf8(buf[..n as usize].to_vec()).ok()
    }
}

// ── Widget module ─────────────────────────────────────────────────────────────

/// Immediate-mode widget UI. Use in a `render()` export.
///
/// # Layout
///
/// Widgets stack vertically by default. Use [`row`] for side-by-side layout
/// and [`card`] for a visual grouping box — both accept closures:
///
/// ```rust,no_run
/// use annessaia_sdk::prelude::*;
/// #[no_mangle]
/// pub extern "C" fn render() {
///     heading("Settings");
///     let vol = slider(0, "Volume", 0.0, 1.0, 0.8);
///     let muted = checkbox(1, "Mute");
///     row(|| {
///         if button("Save") { storage::set_str("vol", &vol.to_string()); }
///         if button("Reset") { /* … */ }
///     });
///     card(|| {
///         colored("Status: OK", Color::GREEN);
///         progress_bar(vol, "Volume");
///     });
/// }
/// ```
pub mod widget {
    use super::{Color, raw, image::Image};

    // ── Text ─────────────────────────────────────────────────────────────────

    pub fn heading(s: &str)  { unsafe { raw::ui_heading(s.as_ptr(), s.len()) } }
    pub fn label(s: &str)    { unsafe { raw::ui_label(s.as_ptr(), s.len()) } }
    pub fn small(s: &str)    { unsafe { raw::ui_small(s.as_ptr(), s.len()) } }

    /// Label with a custom color.
    pub fn colored(s: &str, color: Color) {
        unsafe { raw::ui_colored_label(s.as_ptr(), s.len(), color.0) }
    }

    /// Monospace code block with green-on-dark styling.
    pub fn code(s: &str) { unsafe { raw::ui_code(s.as_ptr(), s.len()) } }

    // ── Interactive ───────────────────────────────────────────────────────────

    /// Returns `true` on the frame the button is clicked.
    pub fn button(s: &str) -> bool { unsafe { raw::ui_button(s.as_ptr(), s.len()) == 1 } }

    /// Button with fully custom foreground, background, and border colors.
    pub fn button_styled(s: &str, fg: Color, bg: Color, border: Color) -> bool {
        unsafe { raw::ui_button_styled(s.as_ptr(), s.len(), fg.0, bg.0, border.0) == 1 }
    }
    /// Red destructive-action button.
    pub fn button_danger(s: &str) -> bool {
        button_styled(s, Color::WHITE, Color::rgb(127,29,29), Color::rgb(239,68,68))
    }
    /// Green confirm/success button.
    pub fn button_success(s: &str) -> bool {
        button_styled(s, Color::WHITE, Color::rgb(20,83,45), Color::rgb(74,222,128))
    }
    /// Transparent outline-only button.
    pub fn button_ghost(s: &str) -> bool {
        button_styled(s, Color::rgb(148,163,184), Color::TRANSPARENT, Color::rgb(71,85,105))
    }

    /// Toggle checkbox. State is managed by the host; `id` identifies this checkbox.
    /// Returns the current checked state.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// let enabled = checkbox(0, "Enable notifications");
    /// if enabled { label("  notifications are on"); }
    /// ```
    pub fn checkbox(id: i32, label: &str) -> bool {
        unsafe { raw::ui_checkbox(id, label.as_ptr(), label.len()) != 0 }
    }

    /// Draggable slider. State is managed by the host; `id` identifies this slider.
    /// `default` is used the first time this slider is shown.
    /// Returns the current value in [`min`, `max`].
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// let speed = slider(0, "Speed", 0.1, 4.0, 1.0);
    /// ```
    pub fn slider(id: i32, label: &str, min: f32, max: f32, default: f32) -> f32 {
        unsafe { raw::ui_slider(id, label.as_ptr(), label.len(), min, max, default) }
    }

    /// Horizontal progress bar. `value` in [0.0, 1.0].
    pub fn progress(value: f32) {
        unsafe { raw::ui_progress(value.clamp(0.0, 1.0)) }
    }

    /// Progress bar with a text label (e.g. `"Loading…"` or `"73%"`).
    pub fn progress_bar(value: f32, label: &str) {
        unsafe { raw::ui_progress_text(value.clamp(0.0, 1.0), label.as_ptr(), label.len()) }
    }

    /// Single-line text input. State is managed by the host; `id` identifies this field.
    /// Returns the current text content.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// let name = text_field(0, "Enter your name…");
    /// label(&format!("Hello, {name}!"));
    /// ```
    pub fn text_field(id: i32, hint: &str) -> String {
        let mut buf = vec![0u8; 4096];
        let n = unsafe { raw::ui_text_edit(id, hint.as_ptr(), hint.len(), buf.as_mut_ptr(), buf.len()) };
        if n <= 0 { return String::new(); }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    /// Same as [`text_field`], but the host masks entered characters — for
    /// password/secret entry.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// let password = text_field_secret(0, "Password…");
    /// ```
    pub fn text_field_secret(id: i32, hint: &str) -> String {
        let mut buf = vec![0u8; 4096];
        let n = unsafe { raw::ui_text_edit_secret(id, hint.as_ptr(), hint.len(), buf.as_mut_ptr(), buf.len()) };
        if n <= 0 { return String::new(); }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    // ── Text / badge ──────────────────────────────────────────────────────────

    /// Label with a fully custom font size and color.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// text("Big title", 32.0, Color::WHITE);
    /// text("Tiny hint", 10.0, Color::rgb(100,100,100));
    /// ```
    pub fn text(s: &str, size: f32, color: Color) {
        unsafe { raw::ui_text(s.as_ptr(), s.len(), size, color.0) }
    }

    /// Small colored pill badge — great for status labels, tags, counts.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// badge("NEW",  Color::CYAN);
    /// badge("BETA", Color::ORANGE);
    /// badge("3",    Color::RED);
    /// ```
    pub fn badge(s: &str, color: Color) {
        unsafe { raw::ui_badge(s.as_ptr(), s.len(), color.0) }
    }

    // ── Layout ────────────────────────────────────────────────────────────────

    /// Arrange child widgets side-by-side in a horizontal row.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// row(|| {
    ///     if button("◀ Back") { sys::back(); }
    ///     if button("▶ Forward") { sys::forward(); }
    /// });
    /// ```
    pub fn row<F: FnOnce()>(f: F) {
        unsafe { raw::ui_row_begin() }
        f();
        unsafe { raw::ui_row_end() }
    }

    /// Wrap widgets in a visual card / group box.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// card(|| {
    ///     heading("Stats");
    ///     label("Frames: 1234");
    ///     progress(0.72);
    /// });
    /// ```
    pub fn card<F: FnOnce()>(f: F) {
        unsafe { raw::ui_card_begin() }
        f();
        unsafe { raw::ui_card_end() }
    }

    /// Card with a custom solid background color.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// card_color(Color::rgb(30,10,10), || {
    ///     colored("Warning", Color::RED);
    ///     label("Something went wrong.");
    /// });
    /// ```
    pub fn card_color<F: FnOnce()>(bg: Color, f: F) {
        unsafe { raw::ui_card_color_begin(bg.0) }
        f();
        unsafe { raw::ui_card_end() }
    }

    /// Two equal-width columns side by side.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// columns2(
    ///     || { heading("Left"); label("Some content"); },
    ///     || { heading("Right"); label("Other content"); },
    /// );
    /// ```
    pub fn columns2<A: FnOnce(), B: FnOnce()>(a: A, b: B) {
        unsafe { raw::ui_columns_begin(2) }
        a();
        unsafe { raw::ui_column_next() }
        b();
        unsafe { raw::ui_columns_end() }
    }

    /// Three equal-width columns side by side.
    pub fn columns3<A: FnOnce(), B: FnOnce(), C: FnOnce()>(a: A, b: B, c: C) {
        unsafe { raw::ui_columns_begin(3) }
        a();
        unsafe { raw::ui_column_next() }
        b();
        unsafe { raw::ui_column_next() }
        c();
        unsafe { raw::ui_columns_end() }
    }

    /// Four equal-width columns side by side.
    pub fn columns4<A: FnOnce(), B: FnOnce(), C: FnOnce(), D: FnOnce()>(a: A, b: B, c: C, d: D) {
        unsafe { raw::ui_columns_begin(4) }
        a();
        unsafe { raw::ui_column_next() }
        b();
        unsafe { raw::ui_column_next() }
        c();
        unsafe { raw::ui_column_next() }
        d();
        unsafe { raw::ui_columns_end() }
    }

    // ── Misc ──────────────────────────────────────────────────────────────────

    pub fn space(px: f32) { unsafe { raw::ui_space(px) } }
    pub fn separator()    { unsafe { raw::ui_separator() } }

    /// Draw a decoded [`image::Image`](super::image::Image), sized to `w × h`.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// static LOGO: &[u8] = include_bytes!("logo.png");
    /// if let Some(logo) = image::decode(LOGO) {
    ///     image(logo, 64.0, 64.0);
    /// }
    /// ```
    pub fn image(img: Image, w: f32, h: f32) {
        unsafe { raw::ui_image(img.id, w, h) }
    }
}

// ── Pixel module ──────────────────────────────────────────────────────────────

/// Managed RGBA pixel framebuffer for `render_pixels(w, h)` apps.
///
/// The buffer is maintained inside the WASM module. Call [`resize`] at the start
/// of your `render_pixels` and the host reads the result via `framebuffer_ptr()`,
/// which is exported automatically by the [`pixel_app!`] macro.
///
/// ```rust,no_run
/// use annessaia_sdk::{pixel, prelude::*, pixel_app};
/// pixel_app!();
///
/// #[no_mangle]
/// pub extern "C" fn render_pixels(w: i32, h: i32) {
///     pixel::resize(w, h);
///     pixel::fill(Color::rgb(10, 10, 30));
///     pixel::circle(w / 2, h / 2, 40, Color::CYAN);
/// }
/// ```
pub mod pixel {
    use super::Color;
    use std::sync::Mutex;

    static FB:   Mutex<Vec<u8>> = Mutex::new(Vec::new());
    static DIMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Called by the [`pixel_app!`] macro — not for direct use.
    pub fn _ptr() -> i32 { FB.lock().unwrap().as_ptr() as i32 }

    fn dims() -> (i32, i32) {
        let v = DIMS.load(std::sync::atomic::Ordering::Relaxed);
        ((v >> 32) as i32, v as i32)
    }

    /// Resize the framebuffer to `w × h` pixels. Call at the start of `render_pixels`.
    pub fn resize(w: i32, h: i32) {
        DIMS.store(((w as u64) << 32) | h as u64, std::sync::atomic::Ordering::Relaxed);
        let need = (w * h * 4) as usize;
        let mut fb = FB.lock().unwrap();
        if fb.len() != need { fb.resize(need, 0); }
    }

    /// Fill the entire framebuffer with a solid color.
    pub fn fill(color: Color) {
        let (w, h) = dims();
        let c = color.0 as u32;
        let (r, g, b, a) = ((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8);
        let mut fb = FB.lock().unwrap();
        let need = (w * h * 4) as usize;
        if fb.len() != need { fb.resize(need, 0); }
        for i in (0..fb.len()).step_by(4) {
            fb[i] = r; fb[i+1] = g; fb[i+2] = b; fb[i+3] = a;
        }
    }

    /// Set a single pixel. Silently clips out-of-bounds writes.
    pub fn put(x: i32, y: i32, color: Color) {
        let (w, h) = dims();
        if x < 0 || y < 0 || x >= w || y >= h { return; }
        let idx = ((y * w + x) * 4) as usize;
        let c = color.0 as u32;
        let mut fb = FB.lock().unwrap();
        if idx + 3 < fb.len() {
            fb[idx]   = (c >> 24) as u8;
            fb[idx+1] = (c >> 16) as u8;
            fb[idx+2] = (c >>  8) as u8;
            fb[idx+3] =  c        as u8;
        }
    }

    /// Fill an axis-aligned rectangle.
    pub fn rect(x0: i32, y0: i32, rw: i32, rh: i32, color: Color) {
        let (w, h) = dims();
        let c = color.0 as u32;
        let (r, g, b, a) = ((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8);
        let mut fb = FB.lock().unwrap();
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

    /// Fill a circle.
    pub fn circle(cx: i32, cy: i32, rad: i32, color: Color) {
        let (w, h) = dims();
        let c = color.0 as u32;
        let (r, g, b, a) = ((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8);
        let mut fb = FB.lock().unwrap();
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

    /// Draw a horizontal line.
    pub fn hline(y: i32, x0: i32, x1: i32, color: Color) {
        rect(x0, y, x1 - x0, 1, color);
    }

    /// Draw a vertical line.
    pub fn vline(x: i32, y0: i32, y1: i32, color: Color) {
        rect(x, y0, 1, y1 - y0, color);
    }

    /// Read a pixel. Returns `Color::TRANSPARENT` for out-of-bounds.
    pub fn get(x: i32, y: i32) -> Color {
        let (w, h) = dims();
        if x < 0 || y < 0 || x >= w || y >= h { return Color::TRANSPARENT; }
        let idx = ((y * w + x) * 4) as usize;
        let fb = FB.lock().unwrap();
        if idx + 3 >= fb.len() { return Color::TRANSPARENT; }
        Color::rgba(fb[idx], fb[idx+1], fb[idx+2], fb[idx+3])
    }

    // Internal: used by the raw import in apps that need it
    #[allow(unused)]
    pub(crate) fn _buf() -> std::sync::MutexGuard<'static, Vec<u8>> { FB.lock().unwrap() }
}

// ── pixel_app! macro ──────────────────────────────────────────────────────────

/// Generate the required `framebuffer_ptr()` export for pixel-buffer apps.
///
/// Place this once at the top level of your WASM app (not inside a function).
///
/// ```rust,no_run
/// use annessaia_sdk::{pixel, pixel_app, prelude::*};
/// pixel_app!();
///
/// #[no_mangle]
/// pub extern "C" fn render_pixels(w: i32, h: i32) {
///     pixel::resize(w, h);
///     pixel::fill(Color::BLACK);
/// }
/// ```
#[macro_export]
macro_rules! pixel_app {
    () => {
        #[no_mangle]
        pub extern "C" fn framebuffer_ptr() -> i32 {
            $crate::pixel::_ptr()
        }
    };
}

// ── Prelude ───────────────────────────────────────────────────────────────────

/// Bring the most-used items into scope with `use annessaia_sdk::prelude::*;`.
///
/// This imports:
/// - `Color` and its constants (`Color::CYAN`, `Color::BLACK`, …)
/// - All GPU draw functions: `clear`, `rect`, `box_`, `circle`, `line`, `triangle`
/// - Canvas/time: `time`, `width`, `height`, `canvas`
/// - Input helpers: `mouse`, `mouse_x`, `mouse_y`, `left_down`, `left_clicked`, `scroll`, `drag`, `hover_rect`, `hover_circle`
/// - Modules: `gpu`, `pixel`, `input`, `storage`, `net`, `sys`, `widget`
/// - Math constants: `PI`, `TAU`
/// - Atomic types: `AtomicU32`, `AtomicI32`, `AtomicBool`, `AtomicUsize`, `Ordering`
pub mod prelude {
    pub use crate::Color;
    pub use crate::image::Image;

    // GPU draw functions — available unqualified
    pub use crate::gpu::{
        time, width, height, canvas,
        clear, rect, box_, circle, line, line1, triangle,
    };

    // Input helpers — available unqualified
    pub use crate::input::{
        mouse, mouse_x, mouse_y,
        left_down, right_down, middle_down,
        left_clicked, right_clicked,
        scroll, drag,
        touch_count, touch,
        hover_rect, hover_circle,
        Key, key_down, key_pressed, key_released,
        shift, ctrl, alt, cmd, typed, axis,
    };

    // Widget functions — available unqualified
    pub use crate::widget::{
        heading, label, small, colored, code,
        button, button_styled, button_danger, button_success, button_ghost,
        checkbox, slider,
        progress, progress_bar, text_field, text_field_secret,
        text, badge,
        row, card, card_color, columns2, columns3, columns4,
        space, separator,
    };

    // Modules for everything else (no name conflicts between them). `image`
    // deliberately stays qualified (`image::decode`, `gpu::image`,
    // `widget::image`) rather than joining the unqualified widget-function
    // list above: gpu and widget each have their own `image` draw function,
    // and importing both unqualified into the same scope would be ambiguous.
    pub use crate::{gpu, pixel, input, storage, net, sys, widget, embed, sound, image, assets};

    // Async request outcomes
    pub use crate::net::{Poll, PollStr};

    // Math constants
    pub use core::f32::consts::{PI, TAU};

    // Atomic primitives (essential for frame state)
    pub use core::sync::atomic::{
        AtomicU32, AtomicI32, AtomicBool, AtomicUsize,
        Ordering,
    };
}
