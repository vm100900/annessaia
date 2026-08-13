// NOVA — a mouse-only survival game for the annessaia runtime.
//
// Steer the core with your cursor. It drifts rather than snapping, so momentum
// is the whole skill. Collect blue motes for score; a fast pickup keeps the
// combo alive and the multiplier climbing. Gold motes detonate a shockwave
// that clears everything near you — the risk being that you have to go
// somewhere dangerous to reach one. Green motes teleport you to a random spot
// on the map — a free escape from a bad position, or a gamble into a worse
// one. Hunters spawn from the edges and home in, and mutually push apart so
// they always present as separate threats rather than stacking into one blob.
//
// A four-step intro plays on first launch (skippable, and reachable again
// later from the menu's "help" button); the menu also has a "scores" button
// showing the last 10 runs, worst to best.
//
// The runtime gives no keyboard, so mouse for everything; text is drawn via
// gpu::text (single-line, centered — no word-wrap), and every number on
// screen is drawn from rectangles as seven-segment digits, since gpu::text
// is a late addition and the score/combo HUD predates it.
//
// This is a port of the mobile build (nova_mobile, Flutter/Dart, touch
// instead of mouse) back onto this original — same palette, same tuning,
// same state machine, same seven-segment digit renderer. The touch-specific
// press-and-drag mechanic doesn't apply here (a mouse always has a position,
// no "finger down" needed — this file follows it directly, same as the
// mobile build's own desktop/hover code path does), but the teleport
// "follow-offset" mechanic is ported as-is: it isn't touch-specific, it's
// what keeps a teleport from being erased the instant the control-follow
// smoothing pulls the core back toward the input device's literal position.
// The mobile build's online (Game Center) leaderboard tab has no equivalent
// here, so the leaderboard screen only ever shows local runs.

use annessaia_sdk::{Color, gpu, input, storage};
use std::sync::Mutex;

// ── Palette ───────────────────────────────────────────────────────────────────

const BG:      Color = Color::rgb(  6,  10,  22);
const GRID:    Color = Color::rgb( 20,  30,  55);
const CORE:    Color = Color::rgb(120, 235, 255);
const CORE_HOT:Color = Color::rgb(255, 255, 255);
const MOTE:    Color = Color::rgb( 90, 190, 255);
const GOLD:    Color = Color::rgb(255, 205,  85);
const TELEPORT:Color = Color::rgb( 90, 255, 140);
const HUNTER:  Color = Color::rgb(255,  70,  95);
const WARN:    Color = Color::rgb(255, 140, 100);
const DIM:     Color = Color::rgb( 60,  78, 110);

// ── Tuning ────────────────────────────────────────────────────────────────────

const PLAYER_R:       f32 = 11.0;
const FOLLOW:         f32 = 7.5;   // higher = snappier steering
const MOTE_R:         f32 = 8.0;
const HUNTER_R:       f32 = 13.0;
const SPAWN_WARN:     f32 = 0.9;   // seconds a hunter telegraphs before going live
const COMBO_GRACE:    f32 = 2.4;   // seconds to grab the next mote before combo resets
const SHOCK_R:        f32 = 190.0;
const TELEPORT_SETTLE:f32 = 1.6;   // per-second decay of the teleport follow-offset back to 0
const INTRO_STEPS:    i32 = 4;     // blue, teleport, gold, hunters
const INTRO_STEP_DURATION: f32 = 5.0;  // seconds before an intro step auto-advances
const HUNTER_MIN_DIST:f32 = 100.0; // hunters mutually push apart to stay at least this far

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Phase { Intro, Menu, Play, Dead, Leaderboard }

struct Mote { x: f32, y: f32, gold: bool, teleport: bool, born: f32 }
struct Hunter { x: f32, y: f32, vx: f32, vy: f32, born: f32, speed: f32 }
struct Part { x: f32, y: f32, vx: f32, vy: f32, life: f32, max: f32, r: f32, c: Color }
struct Ring { x: f32, y: f32, t: f32, c: Color }

struct Game {
    phase: Phase,
    intro_step: i32,
    intro_t: f32,
    t: f32,
    last_t: f32,
    started: bool,

    px: f32, py: f32,
    trail: Vec<(f32, f32)>,
    // Added to the raw cursor position to get the follow target. The core
    // always converges on that target, so teleporting px/py alone would just
    // snap back next frame — the cursor hasn't moved. A teleport instead
    // shifts this offset so the landing spot becomes the new anchor; the
    // cursor keeps steering relative to it from there, decaying back to zero
    // over TELEPORT_SETTLE seconds.
    off_x: f32, off_y: f32,

    motes: Vec<Mote>,
    hunters: Vec<Hunter>,
    parts: Vec<Part>,
    rings: Vec<Ring>,

    score: u32,
    best: u32,
    top_scores: Vec<u32>,   // last 10 runs, worst to best... sorted descending, capped at 10
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
    phase: Phase::Intro,
    intro_step: 0, intro_t: 0.0,
    t: 0.0, last_t: 0.0, started: false,
    px: 0.0, py: 0.0,
    trail: Vec::new(),
    off_x: 0.0, off_y: 0.0,
    motes: Vec::new(), hunters: Vec::new(), parts: Vec::new(), rings: Vec::new(),
    score: 0, best: 0, top_scores: Vec::new(), combo: 0, combo_t: 0.0,
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
        self.off_x = 0.0;
        self.off_y = 0.0;
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
        // One draw decides both: 14% gold, 10% teleport, the rest blue.
        let r = self.rnd();
        let gold = r < 0.14;
        let teleport = !gold && r < 0.24;
        let born = self.t;
        self.motes.push(Mote { x, y, gold, teleport, born });
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

    // Fully random — no safety check against nearby hunters. That risk is the
    // point: chasing the teleport mote for its score can just as easily drop
    // you into danger.
    fn teleport_point(&mut self, w: f32, h: f32) -> (f32, f32) {
        (self.range(60.0, w - 60.0), self.range(60.0, h - 60.0))
    }

    // Each hunter homes on the player independently, so with no mutual
    // collision they'd happily stack into a single blob — trivial to dodge.
    // Push overlapping pairs apart (a few relaxation passes so it holds with
    // several hunters at once) so they always present as separate threats.
    fn separate_hunters(&mut self) {
        for _ in 0..4 {
            for i in 0..self.hunters.len() {
                for j in (i + 1)..self.hunters.len() {
                    let (ax, ay) = (self.hunters[i].x, self.hunters[i].y);
                    let (bx, by) = (self.hunters[j].x, self.hunters[j].y);
                    let (dx, dy) = (bx - ax, by - ay);
                    let d = (dx * dx + dy * dy).sqrt();
                    if d < 0.0001 {
                        let ang = self.rnd() * core::f32::consts::TAU;
                        let push = HUNTER_MIN_DIST * 0.5;
                        self.hunters[i].x -= ang.cos() * push;
                        self.hunters[i].y -= ang.sin() * push;
                        self.hunters[j].x += ang.cos() * push;
                        self.hunters[j].y += ang.sin() * push;
                    } else if d < HUNTER_MIN_DIST {
                        let overlap = (HUNTER_MIN_DIST - d) * 0.5;
                        let (nx, ny) = (dx / d, dy / d);
                        self.hunters[i].x -= nx * overlap;
                        self.hunters[i].y -= ny * overlap;
                        self.hunters[j].x += nx * overlap;
                        self.hunters[j].y += ny * overlap;
                    }
                }
            }
        }
    }

    // Records every run (not just new bests) into local top-10 history.
    fn record_run(&mut self, score: u32) {
        self.top_scores.push(score);
        self.top_scores.sort_by(|a, b| b.cmp(a));
        self.top_scores.truncate(10);
        let joined = self.top_scores.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",");
        storage::set_str("nova.topScores", &joined);
    }
}

// ── Entry points ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn init() {
    let mut g = GAME.lock().unwrap();
    g.best = storage::get_str("nova.best").and_then(|s| s.parse().ok()).unwrap_or(0);
    g.top_scores = storage::get_str("nova.topScores")
        .map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect())
        .unwrap_or_default();
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
        Phase::Intro       => update_intro(&mut g, w, h, dt),
        Phase::Menu        => update_menu(&mut g, w, h),
        Phase::Play        => update_play(&mut g, w, h, dt),
        Phase::Dead        => update_dead(&mut g, w, h, dt),
        Phase::Leaderboard => update_leaderboard(&mut g),
    }

    // ── Draw ──────────────────────────────────────────────────────────────────
    let (sx, sy) = shake_offset(&mut g);
    gpu::clear(BG);
    draw_grid(&g, w, h, sx, sy);
    if g.phase != Phase::Intro {
        draw_world(&g, sx, sy);
        draw_hud(&g, w, h);
    }

    match g.phase {
        Phase::Intro       => draw_intro(&g, w, h),
        Phase::Menu        => draw_menu(&g, w, h),
        Phase::Dead        => draw_dead(&g, w, h),
        Phase::Leaderboard => draw_leaderboard(&g, w, h),
        Phase::Play        => {}
    }

    if g.flash > 0.0 {
        gpu::rect(0.0, 0.0, w, h, 0.0, Color::WHITE.fade(g.flash * 0.55));
    }
}

// ── Update ────────────────────────────────────────────────────────────────────

fn update_intro(g: &mut Game, w: f32, h: f32, dt: f32) {
    g.intro_t += dt;
    if input::left_clicked() {
        let (mx, my) = input::mouse();
        let (ux, uy, uw, uh) = utility_button_rect(w, h);
        if in_rect(mx, my, ux, uy, uw, uh) {
            g.phase = Phase::Menu;
            return;
        }
    }
    if input::left_clicked() || g.intro_t >= INTRO_STEP_DURATION {
        g.intro_step += 1;
        g.intro_t = 0.0;
        if g.intro_step >= INTRO_STEPS { g.phase = Phase::Menu; }
    }
}

fn update_menu(g: &mut Game, w: f32, h: f32) {
    // Idle drift so the menu isn't static.
    let (mx, my) = input::mouse();
    g.px += (mx - g.px) * 0.04;
    g.py += (my - g.py) * 0.04;
    if input::left_clicked() {
        let (ux, uy, uw, uh) = utility_button_rect(w, h);
        if in_rect(mx, my, ux, uy, uw, uh) {
            g.phase = Phase::Intro;
            g.intro_step = 0;
            g.intro_t = 0.0;
            return;
        }
        let (lx, ly, lw, lh) = leaderboard_button_rect(w, h);
        if in_rect(mx, my, lx, ly, lw, lh) {
            g.phase = Phase::Leaderboard;
            return;
        }
        g.reset(w, h);
    }
}

fn update_leaderboard(g: &mut Game) {
    // Any click leaves the leaderboard — nothing on this screen to click for
    // any other reason now that there's only one (local) source of scores.
    if input::left_clicked() { g.phase = Phase::Menu; }
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

    // Let the offset relax back toward 0 so control gradually re-syncs with
    // where the cursor actually is, instead of staying skewed for the rest
    // of the run.
    let settle = (-TELEPORT_SETTLE * dt).exp();
    g.off_x *= settle;
    g.off_y *= settle;

    // ── Player: drift toward the cursor (offset by any live teleport skew) ───
    let (mx, my) = input::mouse();
    let (tx, ty) = (mx + g.off_x, my + g.off_y);
    let k = 1.0 - (-FOLLOW * dt).exp();     // frame-rate independent smoothing
    g.px += (tx - g.px) * k;
    g.py += (ty - g.py) * k;
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
        // Uncapped — the pack just grows, so late game is genuinely relentless.
        let target = 3.0 + g.elapsed / 8.0;
        if (g.hunters.len() as f32) < target { g.spawn_hunter(w, h); }
        g.spawn_t = (1.2 - g.elapsed * 0.014).max(0.4);
    }
    while g.motes.len() < 5 { g.spawn_mote(w, h); }

    // ── Motes ─────────────────────────────────────────────────────────────────
    let mut collected: Vec<(f32, f32, bool, bool)> = Vec::new();
    g.motes.retain(|m| {
        let d = dist(m.x, m.y, g.px, g.py);
        if d < PLAYER_R + MOTE_R + 4.0 {
            collected.push((m.x, m.y, m.gold, m.teleport));
            false
        } else { true }
    });

    for (x, y, is_gold, is_teleport) in collected {
        g.combo = (g.combo + 1).min(99);
        g.combo_t = COMBO_GRACE;
        let mult = 1 + g.combo / 5;
        g.score += if is_gold { 25 * mult } else if is_teleport { 35 * mult } else { 10 * mult };
        g.shake = (g.shake + if is_gold { 9.0 } else if is_teleport { 4.0 } else { 2.2 }).min(16.0);

        if is_gold {
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
        } else if is_teleport {
            g.burst(x, y, 24, TELEPORT, 180.0);
            g.rings.push(Ring { x, y, t: 0.0, c: TELEPORT });
            let (nx, ny) = g.teleport_point(w, h);
            g.px = nx;
            g.py = ny;
            let (mx, my) = input::mouse();
            g.off_x = nx - mx;
            g.off_y = ny - my;
            g.trail.clear();
            g.flash = g.flash.max(0.35);
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
    g.separate_hunters();

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
        g.record_run(g.score);
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

fn in_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32) -> bool {
    px >= x && px <= x + w && py >= y && py <= y + h
}

// Top-center click target shared by the intro ("skip") and menu ("help")
// screens — they're never shown at once, so one hitbox does both jobs.
fn utility_button_rect(w: f32, _h: f32) -> (f32, f32, f32, f32) { (w * 0.5 - 44.0, 14.0, 88.0, 30.0) }

// Menu's entry point into the leaderboard screen.
fn leaderboard_button_rect(w: f32, h: f32) -> (f32, f32, f32, f32) {
    (w * 0.5 - 60.0, h * 0.5 + 224.0, 120.0, 32.0)
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
    // Shockwave / mote-collect rings
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
        let c = if m.gold { GOLD } else if m.teleport { TELEPORT } else { MOTE };
        let r = MOTE_R * pulse;
        gpu::circle(m.x + sx, m.y + sy, r * 2.5, c.fade(0.10));
        gpu::circle(m.x + sx, m.y + sy, r * 1.5, c.fade(0.22));
        gpu::circle(m.x + sx, m.y + sy, r, c);
        if m.gold { gpu::circle(m.x + sx, m.y + sy, r * 0.45, Color::WHITE.fade(0.9)); }
        if m.teleport {
            let ring_r = r * 1.6 + ((g.t - m.born) * 3.0).sin() * 1.5;
            gpu::circle_stroke(m.x + sx, m.y + sy, ring_r, Color::WHITE.fade(0.75), 1.6);
        }
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

fn draw_intro(g: &Game, w: f32, h: f32) {
    let cx = w * 0.5;
    let cy = h * 0.5;
    let big_r = 30.0 * (1.0 + (g.t * 3.0).sin() * 0.06);

    if g.intro_step < 3 {
        let (col, desc): (Color, &str) = match g.intro_step {
            0 => (MOTE, "+10 points"),
            1 => (TELEPORT, "+35 points -- teleports you to a random spot on the map"),
            _ => (GOLD, "+25 points -- explodes every hunter near you"),
        };
        gpu::circle(cx, cy - 70.0, big_r * 2.6, col.fade(0.10));
        gpu::circle(cx, cy - 70.0, big_r * 1.6, col.fade(0.22));
        gpu::circle(cx, cy - 70.0, big_r, col);
        if g.intro_step == 2 { gpu::circle(cx, cy - 70.0, big_r * 0.45, Color::WHITE.fade(0.9)); }
        if g.intro_step == 1 { gpu::circle_stroke(cx, cy - 70.0, big_r * 1.5, Color::WHITE.fade(0.75), 2.0); }
        gpu::text(cx, cy + 10.0, desc, 17.0, Color::WHITE);
    } else {
        gpu::circle(cx, cy - 80.0, big_r * 1.8, HUNTER.fade(0.15));
        gpu::circle(cx, cy - 80.0, big_r * 0.85, HUNTER);
        gpu::text(cx, cy - 20.0, "Get points.", 19.0, Color::WHITE);
        gpu::text(cx, cy + 4.0, "Stay away from the hunters, which can catch you.", 19.0, Color::WHITE);
    }

    gpu::rect(16.0, h - 26.0, (w - 32.0) * (g.intro_step + 1) as f32 / INTRO_STEPS as f32, 3.0, 1.5, CORE.fade(0.5));
    let pulse = 0.5 + (g.t * 2.6).sin() * 0.25;
    gpu::text(cx, h - 56.0, "click to continue", 13.0, DIM.fade(0.55 + pulse * 0.3));

    utility_button(w, h, "skip");
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
    gpu::triangle(cx - 12.0, cy + 15.0, cx - 12.0, cy + 37.0, cx + 14.0, cy + 26.0, CORE);

    // Legend: what each colour does
    legend(cx - 118.0, cy + 78.0,  MOTE,     false);
    legend(cx - 118.0, cy + 106.0, GOLD,     true);
    legend(cx - 118.0, cy + 134.0, HUNTER,   false);
    legend(cx - 118.0, cy + 162.0, TELEPORT, false);

    if g.best > 0 {
        draw_number(cx - 24.0, cy + 204.0, 16.0, g.best, DIM);
    }

    pill_button(leaderboard_button_rect(w, h), "scores", 0.16);
    utility_button(w, h, "help");
}

// One legend row: a sample dot plus a bar suggesting its effect.
fn legend(x: f32, y: f32, c: Color, wide: bool) {
    gpu::circle(x + 10.0, y + 8.0, 8.0, c.fade(0.22));
    gpu::circle(x + 10.0, y + 8.0, 5.0, c);
    let len = if wide { 190.0 } else { 120.0 };
    gpu::rect(x + 28.0, y + 5.0, len, 6.0, 3.0, c.fade(0.28));
}

fn utility_button(w: f32, h: f32, label: &str) {
    let rect = utility_button_rect(w, h);
    pill_button(rect, label, 0.16);
}

fn pill_button(rect: (f32, f32, f32, f32), label: &str, fill_alpha: f32) {
    let (x, y, w, h) = rect;
    gpu::rect(x, y, w, h, h * 0.5, CORE.fade(fill_alpha));
    gpu::text(x + w * 0.5, y + 7.0, label, 12.0, Color::WHITE.fade(0.85));
}

fn draw_leaderboard(g: &Game, w: f32, h: f32) {
    gpu::rect(0.0, 0.0, w, h, 0.0, BG.fade(0.88));
    let cx = w * 0.5;

    gpu::text(cx, 44.0, "LEADERBOARD", 20.0, Color::WHITE);

    let mut y = 120.0;
    if g.top_scores.is_empty() {
        gpu::text(cx, y, "No runs recorded yet -- play a round.", 14.0, Color::WHITE.fade(0.6));
    } else {
        for (i, s) in g.top_scores.iter().enumerate() {
            let c = if i == 0 { GOLD } else { Color::WHITE };
            gpu::text(cx, y, &format!("#{}   {}", i + 1, s), 16.0, c);
            y += 34.0;
        }
    }

    gpu::text(cx, h - 32.0, "click anywhere to go back", 12.0, DIM.fade(0.8));
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
// gpu::text is a late addition (see the file header) and the digit HUD
// predates it — kept as rectangles rather than switched to gpu::text so the
// score/combo/best readouts keep their exact original look.
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
