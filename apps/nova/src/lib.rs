// NOVA — a mouse-only survival game for the annessaia runtime.
//
// Steer the core with your cursor. It drifts rather than snapping, so momentum
// is the whole skill. Collect blue motes for score; a fast pickup keeps the
// combo alive and the multiplier climbing. Hunters spawn from the edges and
// home in. Gold motes detonate a shockwave that clears everything near you —
// the risk being that you have to go somewhere dangerous to reach one.
//
// The runtime gives no keyboard and no text drawing, so: mouse for everything,
// and every number on screen is drawn from rectangles as seven-segment digits.

use annessaia_sdk::{Color, gpu, input, storage};
use std::sync::Mutex;

// ── Palette ───────────────────────────────────────────────────────────────────

const BG:      Color = Color::rgb(  6,  10,  22);
const GRID:    Color = Color::rgb( 20,  30,  55);
const CORE:    Color = Color::rgb(120, 235, 255);
const CORE_HOT:Color = Color::rgb(255, 255, 255);
const MOTE:    Color = Color::rgb( 90, 190, 255);
const GOLD:    Color = Color::rgb(255, 205,  85);
const HUNTER:  Color = Color::rgb(255,  70,  95);
const WARN:    Color = Color::rgb(255, 140, 100);
const DIM:     Color = Color::rgb( 60,  78, 110);

// ── Tuning ────────────────────────────────────────────────────────────────────

const PLAYER_R:    f32 = 11.0;
const FOLLOW:      f32 = 7.5;   // higher = snappier steering
const MOTE_R:      f32 = 8.0;
const HUNTER_R:    f32 = 13.0;
const SPAWN_WARN:  f32 = 0.9;   // seconds a hunter telegraphs before going live
const COMBO_GRACE: f32 = 2.4;   // seconds to grab the next mote before combo resets
const SHOCK_R:     f32 = 190.0;

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Phase { Menu, Play, Dead }

struct Mote { x: f32, y: f32, gold: bool, born: f32 }
struct Hunter { x: f32, y: f32, vx: f32, vy: f32, born: f32, speed: f32 }
struct Part { x: f32, y: f32, vx: f32, vy: f32, life: f32, max: f32, r: f32, c: Color }
struct Ring { x: f32, y: f32, t: f32, c: Color }

struct Game {
    phase: Phase,
    t: f32,
    last_t: f32,
    started: bool,

    px: f32, py: f32,
    trail: Vec<(f32, f32)>,

    motes: Vec<Mote>,
    hunters: Vec<Hunter>,
    parts: Vec<Part>,
    rings: Vec<Ring>,

    score: u32,
    best: u32,
    combo: u32,
    combo_t: f32,
    elapsed: f32,
    spawn_t: f32,
    shake: f32,
    death_t: f32,
    flash: f32,
    seed: u32,
}

static GAME: Mutex<Game> = Mutex::new(Game {
    phase: Phase::Menu,
    t: 0.0, last_t: 0.0, started: false,
    px: 0.0, py: 0.0,
    trail: Vec::new(),
    motes: Vec::new(), hunters: Vec::new(), parts: Vec::new(), rings: Vec::new(),
    score: 0, best: 0, combo: 0, combo_t: 0.0,
    elapsed: 0.0, spawn_t: 0.0, shake: 0.0, death_t: 0.0, flash: 0.0,
    seed: 0x9E3779B9,
});

impl Game {
    fn rnd(&mut self) -> f32 {
        // xorshift32 — deterministic and dependency-free
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed >> 8) as f32 / 16_777_216.0
    }
    fn range(&mut self, a: f32, b: f32) -> f32 { a + self.rnd() * (b - a) }

    fn reset(&mut self, w: f32, h: f32) {
        self.phase = Phase::Play;
        self.px = w * 0.5;
        self.py = h * 0.5;
        self.trail.clear();
        self.motes.clear();
        self.hunters.clear();
        self.parts.clear();
        self.rings.clear();
        self.score = 0;
        self.combo = 0;
        self.combo_t = 0.0;
        self.elapsed = 0.0;
        self.spawn_t = 0.0;
        self.shake = 0.0;
        self.death_t = 0.0;
        self.flash = 0.0;
        for _ in 0..5 { self.spawn_mote(w, h); }
    }

    fn spawn_mote(&mut self, w: f32, h: f32) {
        let m = 60.0;
        let x = self.range(m, w - m);
        let y = self.range(m, h - m);
        // Roughly one in seven is a gold shockwave mote.
        let gold = self.rnd() < 0.14;
        let born = self.t;
        self.motes.push(Mote { x, y, gold, born });
    }

    fn spawn_hunter(&mut self, w: f32, h: f32) {
        // Always enter from off-screen so nothing materialises on top of you.
        let edge = (self.rnd() * 4.0) as i32;
        let (x, y) = match edge {
            0 => (self.range(0.0, w), -40.0),
            1 => (self.range(0.0, w), h + 40.0),
            2 => (-40.0, self.range(0.0, h)),
            _ => (w + 40.0, self.range(0.0, h)),
        };
        let speed = 62.0 + (self.elapsed * 1.5).min(85.0) + self.range(-8.0, 8.0);
        let born = self.t;
        self.hunters.push(Hunter { x, y, vx: 0.0, vy: 0.0, born, speed });
    }

    fn burst(&mut self, x: f32, y: f32, n: usize, c: Color, power: f32) {
        for _ in 0..n {
            let a = self.rnd() * core::f32::consts::TAU;
            let s = self.range(40.0, 40.0 + power);
            let life = self.range(0.35, 0.95);
            let r = self.range(1.5, 4.0);
            self.parts.push(Part {
                x, y, vx: a.cos() * s, vy: a.sin() * s,
                life, max: life, r, c,
            });
        }
    }
}

// ── Entry points ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn init() {
    let mut g = GAME.lock().unwrap();
    g.best = storage::get_str("nova.best").and_then(|s| s.parse().ok()).unwrap_or(0);
    g.seed ^= (gpu::time() * 100_000.0) as u32 | 1;
}

#[no_mangle]
pub extern "C" fn render_gpu() {
    let (w, h) = gpu::canvas();
    let mut g = GAME.lock().unwrap();

    // ── Timing ────────────────────────────────────────────────────────────────
    g.t = gpu::time();
    if !g.started { g.started = true; g.last_t = g.t; g.px = w * 0.5; g.py = h * 0.5; }
    let dt = (g.t - g.last_t).clamp(0.0, 0.05);   // clamp so a stalled frame can't teleport anything
    g.last_t = g.t;

    let phase = g.phase;
    match phase {
        Phase::Menu => update_menu(&mut g, w, h),
        Phase::Play => update_play(&mut g, w, h, dt),
        Phase::Dead => update_dead(&mut g, w, h, dt),
    }

    // ── Draw ──────────────────────────────────────────────────────────────────
    let (sx, sy) = shake_offset(&mut g);
    gpu::clear(BG);
    draw_grid(&g, w, h, sx, sy);
    draw_world(&g, sx, sy);
    draw_hud(&g, w, h);

    match g.phase {
        Phase::Menu => draw_menu(&g, w, h),
        Phase::Dead => draw_dead(&g, w, h),
        Phase::Play => {}
    }

    if g.flash > 0.0 {
        gpu::rect(0.0, 0.0, w, h, 0.0, Color::WHITE.fade(g.flash * 0.55));
    }
}

// ── Update ────────────────────────────────────────────────────────────────────

fn update_menu(g: &mut Game, w: f32, h: f32) {
    // Idle drift so the menu isn't static.
    let (mx, my) = input::mouse();
    g.px += (mx - g.px) * 0.04;
    g.py += (my - g.py) * 0.04;
    if input::left_clicked() { g.reset(w, h); }
}

fn update_dead(g: &mut Game, w: f32, h: f32, dt: f32) {
    g.death_t += dt;
    step_particles(g, dt);
    step_rings(g, dt);
    // Brief lockout so a click that caused the death can't skip the score screen.
    if g.death_t > 0.8 && input::left_clicked() { g.reset(w, h); }
}

fn update_play(g: &mut Game, w: f32, h: f32, dt: f32) {
    g.elapsed += dt;

    // ── Player: drift toward the cursor ───────────────────────────────────────
    let (mx, my) = input::mouse();
    let k = 1.0 - (-FOLLOW * dt).exp();     // frame-rate independent smoothing
    g.px += (mx - g.px) * k;
    g.py += (my - g.py) * k;
    g.px = g.px.clamp(0.0, w);
    g.py = g.py.clamp(0.0, h);

    g.trail.push((g.px, g.py));
    if g.trail.len() > 26 { g.trail.remove(0); }

    // ── Combo decay ───────────────────────────────────────────────────────────
    if g.combo > 0 {
        g.combo_t -= dt;
        if g.combo_t <= 0.0 { g.combo = 0; }
    }

    // ── Spawning ──────────────────────────────────────────────────────────────
    g.spawn_t -= dt;
    if g.spawn_t <= 0.0 {
        // Pressure ramps up but flattens out, so late game is fast yet survivable.
        let target = 2.0 + (g.elapsed / 9.0).min(9.0);
        if (g.hunters.len() as f32) < target { g.spawn_hunter(w, h); }
        g.spawn_t = (1.5 - g.elapsed * 0.012).max(0.45);
    }
    while g.motes.len() < 5 { g.spawn_mote(w, h); }

    // ── Motes ─────────────────────────────────────────────────────────────────
    let mut collected: Vec<(f32, f32, bool)> = Vec::new();
    g.motes.retain(|m| {
        let d = dist(m.x, m.y, g.px, g.py);
        if d < PLAYER_R + MOTE_R + 4.0 {
            collected.push((m.x, m.y, m.gold));
            false
        } else { true }
    });

    for (x, y, gold) in collected {
        g.combo = (g.combo + 1).min(99);
        g.combo_t = COMBO_GRACE;
        let mult = 1 + g.combo / 5;
        g.score += if gold { 25 * mult } else { 10 * mult };
        g.shake = (g.shake + if gold { 9.0 } else { 2.2 }).min(16.0);

        if gold {
            g.burst(x, y, 46, GOLD, 260.0);
            g.rings.push(Ring { x, y, t: 0.0, c: GOLD });
            g.flash = 0.5;
            // Shockwave: everything inside the radius dies.
            let (px, py) = (g.px, g.py);
            let mut killed: Vec<(f32, f32)> = Vec::new();
            g.hunters.retain(|hh| {
                if dist(hh.x, hh.y, px, py) < SHOCK_R { killed.push((hh.x, hh.y)); false } else { true }
            });
            for (kx, ky) in killed {
                g.score += 15;
                g.burst(kx, ky, 20, HUNTER, 170.0);
            }
        } else {
            g.burst(x, y, 16, MOTE, 130.0);
            g.rings.push(Ring { x, y, t: 0.0, c: MOTE });
        }
        g.spawn_mote(w, h);
    }

    // ── Hunters ───────────────────────────────────────────────────────────────
    let (px, py) = (g.px, g.py);
    let t = g.t;
    for hh in g.hunters.iter_mut() {
        let live = t - hh.born > SPAWN_WARN;
        if !live { continue; }
        let (dx, dy) = (px - hh.x, py - hh.y);
        let d = (dx * dx + dy * dy).sqrt().max(0.001);
        // Steer toward the player rather than snapping, so they can be outmanoeuvred.
        hh.vx += (dx / d * hh.speed - hh.vx) * 2.2 * dt;
        hh.vy += (dy / d * hh.speed - hh.vy) * 2.2 * dt;
        hh.x += hh.vx * dt;
        hh.y += hh.vy * dt;
    }

    // ── Death ─────────────────────────────────────────────────────────────────
    let hit = g.hunters.iter().any(|hh| {
        t - hh.born > SPAWN_WARN && dist(hh.x, hh.y, px, py) < PLAYER_R + HUNTER_R - 3.0
    });
    if hit {
        g.phase = Phase::Dead;
        g.death_t = 0.0;
        g.shake = 22.0;
        g.flash = 0.85;
        g.burst(px, py, 90, CORE, 330.0);
        g.rings.push(Ring { x: px, y: py, t: 0.0, c: CORE });
        if g.score > g.best {
            g.best = g.score;
            storage::set_str("nova.best", &g.best.to_string());
        }
    }

    step_particles(g, dt);
    step_rings(g, dt);
}

fn step_particles(g: &mut Game, dt: f32) {
    for p in g.parts.iter_mut() {
        p.x += p.vx * dt;
        p.y += p.vy * dt;
        p.vx *= 1.0 - 2.2 * dt;
        p.vy *= 1.0 - 2.2 * dt;
        p.life -= dt;
    }
    g.parts.retain(|p| p.life > 0.0);
}

fn step_rings(g: &mut Game, dt: f32) {
    for r in g.rings.iter_mut() { r.t += dt; }
    g.rings.retain(|r| r.t < 0.55);
}

fn shake_offset(g: &mut Game) -> (f32, f32) {
    if g.shake <= 0.01 { g.shake = 0.0; return (0.0, 0.0); }
    let a = g.rnd() * core::f32::consts::TAU;
    let m = g.shake;
    g.shake *= 0.86;
    g.flash *= 0.88;
    (a.cos() * m, a.sin() * m)
}

fn dist(x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let (dx, dy) = (x1 - x2, y1 - y2);
    (dx * dx + dy * dy).sqrt()
}

// ── Draw ──────────────────────────────────────────────────────────────────────

fn draw_grid(g: &Game, w: f32, h: f32, sx: f32, sy: f32) {
    let step = 46.0;
    let drift = (g.t * 9.0) % step;
    let mut x = -step + drift + sx * 0.3;
    while x < w { gpu::line1(x, 0.0, x, h, GRID); x += step; }
    let mut y = -step + drift + sy * 0.3;
    while y < h { gpu::line1(0.0, y, w, y, GRID); y += step; }
}

fn draw_world(g: &Game, sx: f32, sy: f32) {
    // Shockwave rings
    for r in &g.rings {
        let k = r.t / 0.55;
        let rad = 12.0 + k * (if r.c.0 == GOLD.0 { SHOCK_R } else { 60.0 });
        gpu::circle(r.x + sx, r.y + sy, rad, r.c.fade((1.0 - k) * 0.20));
    }

    // Particles
    for p in &g.parts {
        let k = (p.life / p.max).clamp(0.0, 1.0);
        gpu::circle(p.x + sx, p.y + sy, p.r * k, p.c.fade(k));
    }

    // Motes — pulsing so they read as collectible
    for m in &g.motes {
        let pulse = 1.0 + ((g.t - m.born) * 4.0).sin() * 0.16;
        let c = if m.gold { GOLD } else { MOTE };
        let r = MOTE_R * pulse;
        gpu::circle(m.x + sx, m.y + sy, r * 2.5, c.fade(0.10));
        gpu::circle(m.x + sx, m.y + sy, r * 1.5, c.fade(0.22));
        gpu::circle(m.x + sx, m.y + sy, r, c);
        if m.gold { gpu::circle(m.x + sx, m.y + sy, r * 0.45, Color::WHITE.fade(0.9)); }
    }

    // Hunters — telegraphed before they become lethal
    for hh in &g.hunters {
        let age = g.t - hh.born;
        let x = hh.x + sx;
        let y = hh.y + sy;
        if age < SPAWN_WARN {
            let k = age / SPAWN_WARN;
            let blink = if (age * 14.0).sin() > 0.0 { 0.75 } else { 0.28 };
            gpu::circle(x, y, HUNTER_R * (0.4 + k * 0.6), WARN.fade(blink));
            gpu::circle(x, y, HUNTER_R * 2.0 * (1.0 - k) + HUNTER_R, WARN.fade(0.16));
        } else {
            gpu::circle(x, y, HUNTER_R * 2.2, HUNTER.fade(0.12));
            gpu::circle(x, y, HUNTER_R, HUNTER);
            gpu::circle(x, y, HUNTER_R * 0.5, Color::WHITE.fade(0.55));
            // Small tail showing travel direction
            let s = (hh.vx * hh.vx + hh.vy * hh.vy).sqrt().max(0.001);
            gpu::line(x, y, x - hh.vx / s * 20.0, y - hh.vy / s * 20.0, HUNTER.fade(0.35), 3.0);
        }
    }

    // Player trail
    let n = g.trail.len();
    for (i, (tx, ty)) in g.trail.iter().enumerate() {
        let k = i as f32 / n.max(1) as f32;
        gpu::circle(tx + sx, ty + sy, PLAYER_R * k * 0.8, CORE.fade(k * k * 0.30));
    }

    // Player core (hidden once dead — it just exploded)
    if g.phase != Phase::Dead {
        let x = g.px + sx;
        let y = g.py + sy;
        let pulse = 1.0 + (g.t * 6.0).sin() * 0.07;
        gpu::circle(x, y, PLAYER_R * 3.0, CORE.fade(0.10));
        gpu::circle(x, y, PLAYER_R * 1.8, CORE.fade(0.18));
        gpu::circle(x, y, PLAYER_R * pulse, CORE);
        gpu::circle(x, y, PLAYER_R * 0.5, CORE_HOT);
    }
}

fn draw_hud(g: &Game, w: f32, _h: f32) {
    // Score, top-left
    draw_number(16.0, 16.0, 22.0, g.score, CORE);

    // Best, top-right, dimmer
    let digits = digit_count(g.best);
    let dw = 13.0 * 0.62 + 3.0;
    draw_number(w - 16.0 - digits as f32 * dw * 0.72 - 8.0, 18.0, 13.0, g.best, DIM);

    // Combo meter under the score
    if g.combo > 1 {
        let mult = 1 + g.combo / 5;
        let frac = (g.combo_t / COMBO_GRACE).clamp(0.0, 1.0);
        gpu::rect(16.0, 16.0 + 22.0 + 10.0, 92.0, 5.0, 2.5, DIM.fade(0.5));
        gpu::rect(16.0, 16.0 + 22.0 + 10.0, 92.0 * frac, 5.0, 2.5, GOLD);
        draw_number(114.0, 16.0 + 22.0 + 4.0, 12.0, mult as u32, GOLD);
    }
}

fn draw_menu(g: &Game, w: f32, h: f32) {
    gpu::rect(0.0, 0.0, w, h, 0.0, BG.fade(0.55));
    let cx = w * 0.5;
    let cy = h * 0.5;

    // Title mark: a ring of motes orbiting the core
    for i in 0..9 {
        let a = i as f32 / 9.0 * core::f32::consts::TAU + g.t * 0.55;
        let r = 74.0;
        gpu::circle(cx + a.cos() * r, cy - 92.0 + a.sin() * r * 0.42, 5.0, MOTE.fade(0.75));
    }
    gpu::circle(cx, cy - 92.0, 20.0, CORE.fade(0.18));
    gpu::circle(cx, cy - 92.0, 11.0, CORE);

    // "Click to begin" — a pulsing button drawn from primitives
    let pulse = 0.55 + (g.t * 2.6).sin() * 0.22;
    gpu::rect(cx - 92.0, cy + 4.0, 184.0, 44.0, 22.0, CORE.fade(pulse * 0.22));
    gpu::rect(cx - 92.0, cy + 4.0, 184.0, 44.0, 22.0, BG.fade(0.0));
    gpu::triangle(cx - 12.0, cy + 15.0, cx - 12.0, cy + 37.0, cx + 14.0, cy + 26.0, CORE);

    // Legend: what each colour does
    legend(cx - 118.0, cy + 78.0, MOTE,   false);
    legend(cx - 118.0, cy + 106.0, GOLD,  true);
    legend(cx - 118.0, cy + 134.0, HUNTER, false);

    if g.best > 0 {
        draw_number(cx - 24.0, cy + 176.0, 16.0, g.best, DIM);
    }
}

// One legend row: a sample dot plus a bar suggesting its effect.
fn legend(x: f32, y: f32, c: Color, wide: bool) {
    gpu::circle(x + 10.0, y + 8.0, 8.0, c.fade(0.22));
    gpu::circle(x + 10.0, y + 8.0, 5.0, c);
    let len = if wide { 190.0 } else { 120.0 };
    gpu::rect(x + 28.0, y + 5.0, len, 6.0, 3.0, c.fade(0.28));
}

fn draw_dead(g: &Game, w: f32, h: f32) {
    let k = (g.death_t / 0.5).clamp(0.0, 1.0);
    gpu::rect(0.0, 0.0, w, h, 0.0, BG.fade(0.72 * k));
    let cx = w * 0.5;
    let cy = h * 0.5;

    draw_number(cx - number_width(g.score, 34.0) * 0.5, cy - 62.0, 34.0, g.score, CORE);

    let is_best = g.score >= g.best && g.score > 0;
    let c = if is_best { GOLD } else { DIM };
    gpu::rect(cx - 70.0, cy - 4.0, 140.0, 4.0, 2.0, c.fade(0.55));
    draw_number(cx - number_width(g.best, 16.0) * 0.5, cy + 12.0, 16.0, g.best, c);

    if g.death_t > 0.8 {
        let pulse = 0.5 + (g.t * 2.6).sin() * 0.25;
        gpu::rect(cx - 80.0, cy + 58.0, 160.0, 40.0, 20.0, CORE.fade(pulse * 0.20));
        gpu::triangle(cx - 10.0, cy + 68.0, cx - 10.0, cy + 88.0, cx + 14.0, cy + 78.0, CORE);
    }
}

// ── Seven-segment digits ──────────────────────────────────────────────────────
// The runtime has no text drawing, so numbers are built from rectangles.
// Bits: 0=top 1=top-left 2=top-right 3=middle 4=bottom-left 5=bottom-right 6=bottom
const SEG: [u8; 10] = [119, 36, 93, 109, 46, 107, 123, 37, 127, 111];

fn digit_count(mut n: u32) -> usize {
    if n == 0 { return 1; }
    let mut c = 0;
    while n > 0 { c += 1; n /= 10; }
    c
}

fn number_width(n: u32, h: f32) -> f32 {
    let dw = h * 0.62;
    digit_count(n) as f32 * (dw + h * 0.16) - h * 0.16
}

fn draw_number(x: f32, y: f32, h: f32, n: u32, c: Color) {
    let dw = h * 0.62;
    let gap = h * 0.16;
    let count = digit_count(n);
    let mut digits = [0u32; 10];
    let mut v = n;
    for i in (0..count).rev() { digits[i] = v % 10; v /= 10; }
    for i in 0..count {
        draw_digit(x + i as f32 * (dw + gap), y, dw, h, digits[i] as usize, c);
    }
}

fn draw_digit(x: f32, y: f32, w: f32, h: f32, d: usize, c: Color) {
    let s = SEG[d.min(9)];
    let t = (h * 0.15).max(2.0);          // segment thickness
    let half = h * 0.5;
    let on = |bit: u8| s & (1 << bit) != 0;

    if on(0) { gpu::rect(x, y, w, t, t * 0.4, c); }
    if on(3) { gpu::rect(x, y + half - t * 0.5, w, t, t * 0.4, c); }
    if on(6) { gpu::rect(x, y + h - t, w, t, t * 0.4, c); }
    if on(1) { gpu::rect(x, y, t, half, t * 0.4, c); }
    if on(2) { gpu::rect(x + w - t, y, t, half, t * 0.4, c); }
    if on(4) { gpu::rect(x, y + half, t, half, t * 0.4, c); }
    if on(5) { gpu::rect(x + w - t, y + half, t, half, t * 0.4, c); }
}
