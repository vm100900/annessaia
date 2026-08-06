// PRISM — a brick-breaker for the annessaia runtime.
//
// The paddle tracks your cursor, and where the ball strikes it decides the
// outgoing angle: catch it near an edge and it flies off sharply, so aiming is
// a matter of paddle placement rather than luck. Bricks drop power-ups, levels
// rotate through patterns, and the ball gains speed the longer a rally lasts.
//
// The runtime offers no keyboard and no text drawing, so: mouse for everything,
// and numbers are built from rectangles as seven-segment digits.

use annessaia_sdk::{Color, gpu, input, storage};
use std::sync::Mutex;

// ── Palette ───────────────────────────────────────────────────────────────────

const BG:     Color = Color::rgb(  8,  11,  24);
const HAZE:   Color = Color::rgb( 22,  30,  55);
const PADDLE: Color = Color::rgb(120, 235, 255);
const BALL:   Color = Color::rgb(255, 255, 255);
const WALL:   Color = Color::rgb( 40,  54,  86);
const DIM:    Color = Color::rgb( 62,  80, 112);
const GOLD:   Color = Color::rgb(255, 205,  85);
const STEEL:  Color = Color::rgb(140, 158, 186);

const ROW_COLORS: [Color; 6] = [
    Color::rgb(239,  68,  68),
    Color::rgb(251, 146,  60),
    Color::rgb(250, 204,  21),
    Color::rgb( 74, 222, 128),
    Color::rgb( 56, 189, 248),
    Color::rgb(167, 139, 250),
];

// ── Tuning ────────────────────────────────────────────────────────────────────

const PAD_W:     f32 = 118.0;
const PAD_H:     f32 = 14.0;
const PAD_Y:     f32 = 52.0;    // distance from the bottom
const BALL_R:    f32 = 7.0;
const BASE_SPEED:f32 = 430.0;
const MAX_SPEED: f32 = 900.0;
const WALL_T:    f32 = 10.0;    // side/top wall thickness
const TOP_HUD:   f32 = 54.0;
const COLS:      usize = 11;
const POW_CHANCE:f32 = 0.13;
const POW_FALL:  f32 = 165.0;

#[derive(Clone, Copy, PartialEq)]
enum Phase { Menu, Serve, Play, Dead, Clear }

#[derive(Clone, Copy, PartialEq)]
enum Pow { Multi, Wide, Slow, Life }

struct Ball { x: f32, y: f32, vx: f32, vy: f32 }
struct Brick { x: f32, y: f32, w: f32, h: f32, hp: u8, tone: usize, steel: bool }
struct Drop { x: f32, y: f32, kind: Pow }
struct Part { x: f32, y: f32, vx: f32, vy: f32, life: f32, max: f32, r: f32, c: Color }

struct Game {
    phase: Phase,
    t: f32, last_t: f32, started: bool,

    pad_x: f32, pad_w: f32,
    balls: Vec<Ball>,
    bricks: Vec<Brick>,
    drops: Vec<Drop>,
    parts: Vec<Part>,

    score: u32, best: u32, lives: u32, level: u32,
    combo: u32,
    speed_mul: f32,
    slow_t: f32,
    wide_t: f32,
    shake: f32, flash: f32,
    clear_t: f32, dead_t: f32,
    seed: u32,
}

static G: Mutex<Game> = Mutex::new(Game {
    phase: Phase::Menu,
    t: 0.0, last_t: 0.0, started: false,
    pad_x: 0.0, pad_w: PAD_W,
    balls: Vec::new(), bricks: Vec::new(), drops: Vec::new(), parts: Vec::new(),
    score: 0, best: 0, lives: 3, level: 1, combo: 0,
    speed_mul: 1.0, slow_t: 0.0, wide_t: 0.0,
    shake: 0.0, flash: 0.0, clear_t: 0.0, dead_t: 0.0,
    seed: 0x2545F491,
});

impl Game {
    fn rnd(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed >> 8) as f32 / 16_777_216.0
    }

    fn burst(&mut self, x: f32, y: f32, n: usize, c: Color, power: f32) {
        for _ in 0..n {
            let a = self.rnd() * core::f32::consts::TAU;
            let s = 50.0 + self.rnd() * power;
            let life = 0.3 + self.rnd() * 0.6;
            let r = 1.5 + self.rnd() * 3.0;
            self.parts.push(Part { x, y, vx: a.cos()*s, vy: a.sin()*s, life, max: life, r, c });
        }
    }

    fn new_run(&mut self, w: f32, h: f32) {
        self.score = 0;
        self.lives = 3;
        self.level = 1;
        self.pad_w = PAD_W;
        self.slow_t = 0.0;
        self.wide_t = 0.0;
        self.parts.clear();
        self.build_level(w, h);
        self.serve(w, h);
    }

    fn serve(&mut self, w: f32, _h: f32) {
        self.phase = Phase::Serve;
        self.balls.clear();
        self.drops.clear();
        self.combo = 0;
        self.speed_mul = 1.0;
        if self.pad_x == 0.0 { self.pad_x = w * 0.5; }
    }

    fn build_level(&mut self, w: f32, _h: f32) {
        self.bricks.clear();
        let margin = 44.0;
        let bw = (w - margin * 2.0) / COLS as f32;
        let bh = 26.0;
        let rows = (5 + (self.level.min(4)) as usize).min(8);
        let pattern = (self.level - 1) % 4;

        for r in 0..rows {
            for c in 0..COLS {
                // Each level rotates through a different silhouette so the board
                // doesn't feel like the same wall with more rows bolted on.
                let keep = match pattern {
                    0 => true,
                    1 => (r + c) % 2 == 0,
                    2 => {
                        let mid = COLS as i32 / 2;
                        (c as i32 - mid).abs() <= (r as i32).min(mid)
                    }
                    _ => !(c > 1 && c < COLS - 2 && r > 0 && r < rows - 1),
                };
                if !keep { continue; }

                // A few steel bricks from level 3 on: they never break, so they
                // shape the board and force angled shots.
                let steel = self.level >= 3 && r == 0 && c % 5 == 2;
                let hp = if steel { 255 } else if r < rows / 3 && self.level >= 2 { 2 } else { 1 };
                self.bricks.push(Brick {
                    x: margin + c as f32 * bw + 2.0,
                    y: TOP_HUD + 16.0 + r as f32 * bh + 2.0,
                    w: bw - 4.0, h: bh - 4.0,
                    hp, tone: r % ROW_COLORS.len(), steel,
                });
            }
        }
    }

    fn breakable_left(&self) -> usize { self.bricks.iter().filter(|b| !b.steel).count() }
}

// ── Entry points ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn init() {
    let mut g = G.lock().unwrap();
    g.best = storage::get_str("prism.best").and_then(|s| s.parse().ok()).unwrap_or(0);
    g.seed ^= (gpu::time() * 100_000.0) as u32 | 1;
}

#[no_mangle]
pub extern "C" fn render_gpu() {
    let (w, h) = gpu::canvas();
    let mut g = G.lock().unwrap();

    g.t = gpu::time();
    if !g.started { g.started = true; g.last_t = g.t; g.pad_x = w * 0.5; }
    let dt = (g.t - g.last_t).clamp(0.0, 0.05);
    g.last_t = g.t;

    match g.phase {
        Phase::Menu  => { if input::left_clicked() { g.new_run(w, h); } }
        Phase::Serve => update_serve(&mut g, w, h, dt),
        Phase::Play  => update_play(&mut g, w, h, dt),
        Phase::Clear => {
            g.clear_t += dt;
            step_parts(&mut g, dt);
            if g.clear_t > 1.1 { g.level += 1; g.build_level(w, h); g.serve(w, h); }
        }
        Phase::Dead => {
            g.dead_t += dt;
            step_parts(&mut g, dt);
            if g.dead_t > 0.8 && input::left_clicked() { g.phase = Phase::Menu; }
        }
    }

    // ── Draw ──────────────────────────────────────────────────────────────────
    let (sx, sy) = shake(&mut g);
    gpu::clear(BG);
    draw_walls(w, h, sx, sy);
    draw_bricks(&g, sx, sy);
    draw_drops(&g, sx, sy);
    draw_parts(&g, sx, sy);
    if g.phase != Phase::Dead && g.phase != Phase::Menu { draw_paddle(&g, h, sx, sy); }
    draw_balls(&g, sx, sy);
    draw_hud(&g, w);

    match g.phase {
        Phase::Menu  => draw_menu(&g, w, h),
        Phase::Serve => draw_serve_hint(&g, w, h),
        Phase::Dead  => draw_dead(&g, w, h),
        _ => {}
    }

    if g.flash > 0.0 { gpu::rect(0.0, 0.0, w, h, 0.0, Color::WHITE.fade(g.flash * 0.4)); }
}

// ── Update ────────────────────────────────────────────────────────────────────

fn move_paddle(g: &mut Game, w: f32) {
    let (mx, _) = input::mouse();
    let half = g.pad_w * 0.5;
    g.pad_x = mx.clamp(WALL_T + half, w - WALL_T - half);
}

fn update_serve(g: &mut Game, w: f32, h: f32, dt: f32) {
    move_paddle(g, w);
    step_parts(g, dt);
    step_timers(g, dt);
    if input::left_clicked() {
        let speed = BASE_SPEED + (g.level as f32 - 1.0) * 22.0;
        g.balls.push(Ball { x: g.pad_x, y: h - PAD_Y - PAD_H - BALL_R - 2.0, vx: speed * 0.45, vy: -speed });
        g.phase = Phase::Play;
    }
}

fn update_play(g: &mut Game, w: f32, h: f32, dt: f32) {
    move_paddle(g, w);
    step_timers(g, dt);

    let slow = if g.slow_t > 0.0 { 0.62 } else { 1.0 };
    let step = dt * slow * g.speed_mul;

    let pad_top = h - PAD_Y - PAD_H;
    let pad_l = g.pad_x - g.pad_w * 0.5;
    let pad_r = g.pad_x + g.pad_w * 0.5;

    let mut hits: Vec<(f32, f32, usize)> = Vec::new();   // x, y, brick index
    let mut lost: Vec<usize> = Vec::new();

    for i in 0..g.balls.len() {
        let (mut bx, mut by, mut vx, mut vy) = {
            let b = &g.balls[i];
            (b.x, b.y, b.vx, b.vy)
        };
        bx += vx * step;
        by += vy * step;

        // Walls
        if bx - BALL_R < WALL_T { bx = WALL_T + BALL_R; vx = vx.abs(); }
        if bx + BALL_R > w - WALL_T { bx = w - WALL_T - BALL_R; vx = -vx.abs(); }
        if by - BALL_R < TOP_HUD { by = TOP_HUD + BALL_R; vy = vy.abs(); }

        // Paddle — the contact point sets the outgoing angle, which is the whole
        // control scheme: edges send it wide, centre sends it straight.
        if vy > 0.0 && by + BALL_R >= pad_top && by - BALL_R <= pad_top + PAD_H
            && bx >= pad_l - BALL_R && bx <= pad_r + BALL_R
        {
            by = pad_top - BALL_R;
            let off = ((bx - g.pad_x) / (g.pad_w * 0.5)).clamp(-1.0, 1.0);
            let speed = (vx * vx + vy * vy).sqrt().min(MAX_SPEED);
            let angle = off * 1.05;                     // up to ~60° off vertical
            vx = speed * angle.sin();
            vy = -speed * angle.cos();
            g.combo = 0;
        }

        // Bricks
        let mut j = 0;
        while j < g.bricks.len() {
            let br = &g.bricks[j];
            let (cxb, cyb) = (br.x + br.w * 0.5, br.y + br.h * 0.5);
            let ox = (BALL_R + br.w * 0.5) - (bx - cxb).abs();
            let oy = (BALL_R + br.h * 0.5) - (by - cyb).abs();
            if ox > 0.0 && oy > 0.0 {
                // Resolve along the shallower axis — that is the face it actually met.
                if ox < oy {
                    vx = -vx;
                    bx += if bx < cxb { -ox } else { ox };
                } else {
                    vy = -vy;
                    by += if by < cyb { -oy } else { oy };
                }
                hits.push((cxb, cyb, j));
                break;
            }
            j += 1;
        }

        if by - BALL_R > h { lost.push(i); }

        let b = &mut g.balls[i];
        b.x = bx; b.y = by; b.vx = vx; b.vy = vy;
    }

    // Apply brick hits (collected first so the ball loop isn't mutating the list)
    let mut destroyed: Vec<(f32, f32, usize, bool)> = Vec::new();
    for (hx, hy, idx) in hits {
        if idx >= g.bricks.len() { continue; }
        if g.bricks[idx].steel {
            g.shake = (g.shake + 2.0).min(14.0);
            g.burst(hx, hy, 5, STEEL, 90.0);
            continue;
        }
        g.bricks[idx].hp = g.bricks[idx].hp.saturating_sub(1);
        if g.bricks[idx].hp == 0 {
            let tone = g.bricks[idx].tone;
            g.bricks.remove(idx);
            destroyed.push((hx, hy, tone, true));
        } else {
            g.burst(hx, hy, 6, ROW_COLORS[g.bricks[idx].tone], 110.0);
            g.shake = (g.shake + 1.5).min(14.0);
        }
    }

    for (hx, hy, tone, _) in destroyed {
        g.combo += 1;
        g.score += 10 * g.combo.min(8);
        g.shake = (g.shake + 3.0).min(16.0);
        g.burst(hx, hy, 16, ROW_COLORS[tone], 190.0);
        // Rallies speed the ball up, so a long run gets progressively hairier.
        g.speed_mul = (g.speed_mul + 0.012).min(1.7);

        if g.rnd() < POW_CHANCE {
            let kind = match (g.rnd() * 4.0) as i32 {
                0 => Pow::Multi,
                1 => Pow::Wide,
                2 => Pow::Slow,
                _ => Pow::Life,
            };
            g.drops.push(Drop { x: hx, y: hy, kind });
        }
    }

    // Lost balls
    for i in lost.into_iter().rev() {
        if i < g.balls.len() { g.balls.remove(i); }
    }
    if g.balls.is_empty() {
        g.lives = g.lives.saturating_sub(1);
        g.shake = 18.0;
        g.flash = 0.6;
        if g.lives == 0 {
            g.phase = Phase::Dead;
            g.dead_t = 0.0;
            if g.score > g.best {
                g.best = g.score;
                storage::set_str("prism.best", &g.best.to_string());
            }
        } else {
            g.serve(w, h);
        }
    }

    // Power-up drops
    let pad_top2 = h - PAD_Y - PAD_H;
    let mut caught: Vec<Pow> = Vec::new();
    let (px, pw) = (g.pad_x, g.pad_w);
    g.drops.retain_mut(|d| {
        d.y += POW_FALL * dt;
        if d.y > pad_top2 && d.y < pad_top2 + PAD_H + 14.0
            && d.x > px - pw * 0.5 - 10.0 && d.x < px + pw * 0.5 + 10.0
        {
            caught.push(d.kind);
            return false;
        }
        d.y < h + 20.0
    });

    for k in caught {
        g.score += 40;
        g.flash = 0.35;
        match k {
            Pow::Multi => {
                // Split every ball in two, fanning the copies outward.
                let mut extra = Vec::new();
                for b in g.balls.iter() {
                    let s = (b.vx * b.vx + b.vy * b.vy).sqrt();
                    let a = b.vy.atan2(b.vx);
                    for d in [-0.42f32, 0.42] {
                        extra.push(Ball { x: b.x, y: b.y, vx: s * (a + d).cos(), vy: s * (a + d).sin() });
                    }
                }
                for b in extra { if g.balls.len() < 12 { g.balls.push(b); } }
            }
            Pow::Wide => { g.wide_t = 12.0; g.pad_w = PAD_W * 1.6; }
            Pow::Slow => { g.slow_t = 8.0; }
            Pow::Life => { g.lives = (g.lives + 1).min(6); }
        }
    }

    step_parts(g, dt);

    if g.breakable_left() == 0 {
        g.phase = Phase::Clear;
        g.clear_t = 0.0;
        g.flash = 0.7;
        g.score += 250;
        let (cx, cy) = (w * 0.5, h * 0.45);
        g.burst(cx, cy, 70, GOLD, 320.0);
    }
}

fn step_timers(g: &mut Game, dt: f32) {
    if g.slow_t > 0.0 { g.slow_t -= dt; }
    if g.wide_t > 0.0 {
        g.wide_t -= dt;
        if g.wide_t <= 0.0 { g.pad_w = PAD_W; }
    }
}

fn step_parts(g: &mut Game, dt: f32) {
    for p in g.parts.iter_mut() {
        p.x += p.vx * dt;
        p.y += p.vy * dt;
        p.vy += 260.0 * dt;          // a little gravity so debris falls away
        p.life -= dt;
    }
    g.parts.retain(|p| p.life > 0.0);
}

fn shake(g: &mut Game) -> (f32, f32) {
    if g.shake <= 0.01 { g.shake = 0.0; g.flash *= 0.88; return (0.0, 0.0); }
    let a = g.rnd() * core::f32::consts::TAU;
    let m = g.shake;
    g.shake *= 0.85;
    g.flash *= 0.88;
    (a.cos() * m, a.sin() * m)
}

// ── Draw ──────────────────────────────────────────────────────────────────────

fn draw_walls(w: f32, h: f32, sx: f32, sy: f32) {
    gpu::rect(sx, TOP_HUD + sy, WALL_T, h, 0.0, WALL);
    gpu::rect(w - WALL_T + sx, TOP_HUD + sy, WALL_T, h, 0.0, WALL);
    gpu::rect(sx, TOP_HUD + sy, w, WALL_T, 0.0, WALL);
    // Soft glow along the play field edge
    gpu::rect(WALL_T + sx, TOP_HUD + WALL_T + sy, w - WALL_T * 2.0, 2.0, 0.0, HAZE);
}

fn draw_bricks(g: &Game, sx: f32, sy: f32) {
    for b in &g.bricks {
        let c = if b.steel { STEEL } else { ROW_COLORS[b.tone] };
        let (x, y) = (b.x + sx, b.y + sy);
        gpu::rect(x, y, b.w, b.h, 4.0, c.fade(if b.steel { 0.85 } else { 1.0 }));
        // Top highlight gives the tiles a little depth
        gpu::rect(x + 2.0, y + 2.0, b.w - 4.0, 3.0, 1.5, Color::WHITE.fade(0.22));
        if b.hp == 2 {
            gpu::rect(x + b.w * 0.5 - 7.0, y + b.h * 0.5 - 1.5, 14.0, 3.0, 1.5, Color::WHITE.fade(0.5));
        }
        if b.steel {
            gpu::circle(x + b.w * 0.5, y + b.h * 0.5, 3.0, Color::WHITE.fade(0.35));
        }
    }
}

fn draw_paddle(g: &Game, h: f32, sx: f32, sy: f32) {
    let x = g.pad_x - g.pad_w * 0.5 + sx;
    let y = h - PAD_Y + sy;
    let c = if g.wide_t > 0.0 { GOLD } else { PADDLE };
    gpu::rect(x - 3.0, y - 3.0, g.pad_w + 6.0, PAD_H + 6.0, 9.0, c.fade(0.16));
    gpu::rect(x, y, g.pad_w, PAD_H, 7.0, c);
    gpu::rect(x + 6.0, y + 3.0, g.pad_w - 12.0, 3.0, 1.5, Color::WHITE.fade(0.45));
}

fn draw_balls(g: &Game, sx: f32, sy: f32) {
    for b in &g.balls {
        gpu::circle(b.x + sx, b.y + sy, BALL_R * 2.6, PADDLE.fade(0.13));
        gpu::circle(b.x + sx, b.y + sy, BALL_R, BALL);
    }
}

fn draw_drops(g: &Game, sx: f32, sy: f32) {
    for d in &g.drops {
        let (c, glyph) = match d.kind {
            Pow::Multi => (Color::rgb( 56, 189, 248), 0),
            Pow::Wide  => (GOLD, 1),
            Pow::Slow  => (Color::rgb(167, 139, 250), 2),
            Pow::Life  => (Color::rgb( 74, 222, 128), 3),
        };
        let (x, y) = (d.x + sx, d.y + sy);
        gpu::circle(x, y, 15.0, c.fade(0.16));
        gpu::circle(x, y, 10.0, c);
        match glyph {
            0 => { gpu::circle(x - 3.5, y, 2.5, BG); gpu::circle(x + 3.5, y, 2.5, BG); }
            1 => { gpu::rect(x - 7.0, y - 2.0, 14.0, 4.0, 2.0, BG); }
            2 => { gpu::circle(x, y, 5.0, BG); gpu::rect(x - 0.8, y - 4.0, 1.6, 4.5, 0.0, c); }
            _ => { gpu::rect(x - 1.8, y - 6.0, 3.6, 12.0, 1.0, BG);
                   gpu::rect(x - 6.0, y - 1.8, 12.0, 3.6, 1.0, BG); }
        }
    }
}

fn draw_parts(g: &Game, sx: f32, sy: f32) {
    for p in &g.parts {
        let k = (p.life / p.max).clamp(0.0, 1.0);
        gpu::circle(p.x + sx, p.y + sy, p.r * k, p.c.fade(k));
    }
}

fn draw_hud(g: &Game, w: f32) {
    draw_number(16.0, 14.0, 24.0, g.score, PADDLE);

    // Lives as pips
    for i in 0..g.lives {
        gpu::circle(w * 0.5 - 30.0 + i as f32 * 18.0, 26.0, 6.0, Color::rgb(74, 222, 128));
    }

    // Level, right side
    draw_number(w - 16.0 - number_width(g.level, 16.0), 18.0, 16.0, g.level, DIM);

    // Active power-up timers
    if g.slow_t > 0.0 {
        gpu::rect(16.0, 44.0, 60.0 * (g.slow_t / 8.0).min(1.0), 4.0, 2.0, Color::rgb(167, 139, 250));
    }
    if g.wide_t > 0.0 {
        gpu::rect(84.0, 44.0, 60.0 * (g.wide_t / 12.0).min(1.0), 4.0, 2.0, GOLD);
    }
}

fn draw_menu(g: &Game, w: f32, h: f32) {
    gpu::rect(0.0, 0.0, w, h, 0.0, BG.fade(0.72));
    let (cx, cy) = (w * 0.5, h * 0.42);

    // Title mark: a prism splitting into the six brick colours
    gpu::triangle(cx - 34.0, cy + 20.0, cx, cy - 40.0, cx + 34.0, cy + 20.0, PADDLE.fade(0.20));
    gpu::line(cx - 34.0, cy + 20.0, cx, cy - 40.0, PADDLE, 2.0);
    gpu::line(cx + 34.0, cy + 20.0, cx, cy - 40.0, PADDLE, 2.0);
    gpu::line(cx - 34.0, cy + 20.0, cx + 34.0, cy + 20.0, PADDLE, 2.0);
    for (i, c) in ROW_COLORS.iter().enumerate() {
        let yy = cy - 12.0 + i as f32 * 7.0;
        gpu::rect(cx + 40.0, yy, 90.0 + i as f32 * 9.0, 4.0, 2.0, c.fade(0.9));
    }
    gpu::line(cx - 130.0, cy + 2.0, cx - 34.0, cy + 2.0, Color::WHITE.fade(0.8), 3.0);

    // Click-to-start pip
    let pulse = 0.5 + (g.t * 2.6).sin() * 0.25;
    gpu::rect(cx - 86.0, cy + 70.0, 172.0, 44.0, 22.0, PADDLE.fade(pulse * 0.20));
    gpu::triangle(cx - 11.0, cy + 81.0, cx - 11.0, cy + 103.0, cx + 15.0, cy + 92.0, PADDLE);

    if g.best > 0 {
        draw_number(cx - number_width(g.best, 18.0) * 0.5, cy + 136.0, 18.0, g.best, GOLD);
    }
}

fn draw_serve_hint(g: &Game, w: f32, h: f32) {
    // Ghost ball resting on the paddle, plus an arrow showing the launch
    let x = g.pad_x;
    let y = h - PAD_Y - PAD_H - BALL_R - 2.0;
    gpu::circle(x, y, BALL_R, BALL.fade(0.55 + (g.t * 4.0).sin() * 0.2));
    let a = 0.45f32;
    gpu::line(x, y - 8.0, x + a.sin() * 34.0, y - 8.0 - a.cos() * 34.0, PADDLE.fade(0.5), 2.0);
    let _ = w;
}

fn draw_dead(g: &Game, w: f32, h: f32) {
    let k = (g.dead_t / 0.5).clamp(0.0, 1.0);
    gpu::rect(0.0, 0.0, w, h, 0.0, BG.fade(0.78 * k));
    let (cx, cy) = (w * 0.5, h * 0.46);

    draw_number(cx - number_width(g.score, 36.0) * 0.5, cy - 60.0, 36.0, g.score, PADDLE);

    let is_best = g.score >= g.best && g.score > 0;
    let c = if is_best { GOLD } else { DIM };
    gpu::rect(cx - 72.0, cy, 144.0, 4.0, 2.0, c.fade(0.5));
    draw_number(cx - number_width(g.best, 18.0) * 0.5, cy + 16.0, 18.0, g.best, c);

    if g.dead_t > 0.8 {
        let pulse = 0.5 + (g.t * 2.6).sin() * 0.25;
        gpu::rect(cx - 80.0, cy + 62.0, 160.0, 40.0, 20.0, PADDLE.fade(pulse * 0.20));
        gpu::triangle(cx - 10.0, cy + 72.0, cx - 10.0, cy + 92.0, cx + 14.0, cy + 82.0, PADDLE);
    }
}

// ── Seven-segment digits ──────────────────────────────────────────────────────
// The runtime cannot draw text, so numbers are assembled from rectangles.
// Bits: 0=top 1=top-left 2=top-right 3=middle 4=bottom-left 5=bottom-right 6=bottom
const SEG: [u8; 10] = [119, 36, 93, 109, 46, 107, 123, 37, 127, 111];

fn digit_count(mut n: u32) -> usize {
    if n == 0 { return 1; }
    let mut c = 0;
    while n > 0 { c += 1; n /= 10; }
    c
}

fn number_width(n: u32, h: f32) -> f32 {
    digit_count(n) as f32 * (h * 0.62 + h * 0.16) - h * 0.16
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
    let t = (h * 0.15).max(2.0);
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
