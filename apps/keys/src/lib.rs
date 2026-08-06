// Keyboard demo — exercises every part of the input API added to the SDK.
//
// Drive the square with WASD or the arrow keys, hold Shift to sprint, press
// Space to leave a mark, and type to see composed text. Anything you press
// lights up in the key map, which is the quickest way to confirm a key code
// survives the trip from the host to a guest.

use annessaia_sdk::{Color, gpu, input, input::Key, sys};
use std::sync::Mutex;

const BG:    Color = Color::rgb(  8,  12,  24);
const PANEL: Color = Color::rgb( 16,  23,  40);
const EDGE:  Color = Color::rgb( 38,  50,  74);
const LIVE:  Color = Color::rgb( 56, 189, 248);
const HOT:   Color = Color::rgb(250, 204,  21);
const DIM:   Color = Color::rgb( 60,  78, 110);
const MARK:  Color = Color::rgb(167, 139, 250);

struct State {
    x: f32, y: f32,
    marks: Vec<(f32, f32)>,
    text: String,
    started: bool,
    t: f32, last_t: f32,
}

static S: Mutex<State> = Mutex::new(State {
    x: 0.0, y: 0.0, marks: Vec::new(), text: String::new(),
    started: false, t: 0.0, last_t: 0.0,
});

// The keys drawn in the map, laid out roughly as on a keyboard.
const ROWS: [&[(Key, f32)]; 5] = [
    &[(Key::Escape,1.0),(Key::F1,1.0),(Key::F2,1.0),(Key::F3,1.0),(Key::F4,1.0)],
    &[(Key::Num1,1.0),(Key::Num2,1.0),(Key::Num3,1.0),(Key::Num4,1.0),(Key::Num5,1.0),(Key::Backspace,2.0)],
    &[(Key::Q,1.0),(Key::W,1.0),(Key::E,1.0),(Key::R,1.0),(Key::T,1.0),(Key::Tab,1.6)],
    &[(Key::A,1.0),(Key::S,1.0),(Key::D,1.0),(Key::F,1.0),(Key::G,1.0),(Key::Enter,1.6)],
    &[(Key::Z,1.0),(Key::X,1.0),(Key::C,1.0),(Key::V,1.0),(Key::Space,3.0)],
];

#[no_mangle]
pub extern "C" fn init() {
    sys::log("keys: instantiated, keyboard imports resolved");
}

#[no_mangle]
pub extern "C" fn render_gpu() {
    let (w, h) = gpu::canvas();
    let mut s = S.lock().unwrap();

    s.t = gpu::time();
    if !s.started { s.started = true; s.last_t = s.t; s.x = w * 0.5; s.y = h * 0.55; }
    let dt = (s.t - s.last_t).clamp(0.0, 0.05);
    s.last_t = s.t;

    // ── Movement ──────────────────────────────────────────────────────────────
    let (ax, ay) = input::axis();
    let speed = if input::shift() { 620.0 } else { 260.0 };
    s.x = (s.x + ax * speed * dt).clamp(20.0, w - 20.0);
    s.y = (s.y + ay * speed * dt).clamp(210.0, h - 20.0);

    // Edge-triggered: fires once per press even if Space is held.
    if input::key_pressed(Key::Space) {
        let (x, y) = (s.x, s.y);
        s.marks.push((x, y));
        if s.marks.len() > 40 { s.marks.remove(0); }
    }
    if input::key_pressed(Key::Backspace) { s.marks.pop(); }
    if input::key_pressed(Key::Escape) { s.marks.clear(); s.text.clear(); }

    // Composed text, so layouts and accents work.
    let t = input::typed();
    if !t.is_empty() {
        s.text.push_str(&t);
        if s.text.chars().count() > 48 {
            let keep: String = s.text.chars().skip(s.text.chars().count() - 48).collect();
            s.text = keep;
        }
    }
    if input::key_pressed(Key::Backspace) { s.text.pop(); }

    // ── Draw ──────────────────────────────────────────────────────────────────
    gpu::clear(BG);

    for (mx, my) in &s.marks { gpu::circle(*mx, *my, 5.0, MARK.fade(0.75)); }

    let moving = ax != 0.0 || ay != 0.0;
    let c = if input::shift() && moving { HOT } else { LIVE };
    gpu::rect(s.x - 22.0, s.y - 22.0, 44.0, 44.0, 8.0, c.fade(0.18));
    gpu::rect(s.x - 15.0, s.y - 15.0, 30.0, 30.0, 6.0, c);
    if moving {
        gpu::line(s.x, s.y, s.x + ax * 34.0, s.y + ay * 34.0, c, 3.0);
    }

    draw_key_map(&s, 16.0, 16.0);
    draw_modifiers(w - 200.0, 16.0);
    draw_text_line(&s, 16.0, 176.0, w - 32.0);
}

fn draw_key_map(_s: &State, ox: f32, oy: f32) {
    let u = 30.0;      // one key unit
    let gap = 4.0;
    for (r, row) in ROWS.iter().enumerate() {
        let mut x = ox;
        let y = oy + r as f32 * (u + gap);
        for (k, wide) in row.iter() {
            let kw = u * wide;
            let held = input::key_down(*k);
            let hit  = input::key_pressed(*k);
            let off  = input::key_released(*k);
            let fill = if hit { HOT } else if held { LIVE }
                       else if off { LIVE.fade(0.45) }   // brief flash on release
                       else { PANEL };
            gpu::rect(x, y, kw, u, 5.0, fill);
            gpu::rect(x, y, kw, 2.0, 1.0, if held || hit { Color::WHITE.fade(0.5) } else { EDGE });
            // A dot per key so the shape reads even without labels
            gpu::circle(x + kw * 0.5, y + u * 0.5, 3.0,
                        if held || hit { BG } else { DIM });
            x += kw + gap;
        }
    }
}

fn draw_modifiers(ox: f32, oy: f32) {
    let mods = [("shift", input::shift()), ("ctrl", input::ctrl()),
                ("alt", input::alt()), ("cmd", input::cmd())];
    for (i, (_, on)) in mods.iter().enumerate() {
        let y = oy + i as f32 * 26.0;
        gpu::rect(ox, y, 180.0, 22.0, 5.0, if *on { LIVE.fade(0.30) } else { PANEL });
        gpu::circle(ox + 14.0, y + 11.0, 5.0, if *on { LIVE } else { DIM });
        gpu::rect(ox + 26.0, y + 9.0, 40.0 + i as f32 * 22.0, 4.0, 2.0,
                  if *on { LIVE.fade(0.8) } else { DIM.fade(0.6) });
    }
}

// The typed string as a row of bars — the runtime cannot draw text, so this
// shows that characters are arriving and in what quantity.
fn draw_text_line(s: &State, ox: f32, oy: f32, w: f32) {
    gpu::rect(ox, oy, w, 34.0, 6.0, PANEL);
    let n = s.text.chars().count();
    for (i, ch) in s.text.chars().enumerate() {
        let x = ox + 10.0 + i as f32 * 11.0;
        if x > ox + w - 14.0 { break; }
        // Bar height keyed off the character so different letters look different.
        let hgt = 6.0 + ((ch as u32 % 17) as f32) * 1.2;
        gpu::rect(x, oy + 26.0 - hgt, 7.0, hgt, 2.0, MARK.fade(0.9));
    }
    if n == 0 {
        gpu::rect(ox + 10.0, oy + 15.0, 60.0, 3.0, 1.5, DIM);
    }
}
