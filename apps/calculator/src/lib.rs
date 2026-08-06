// Calculator — a standard four-function calculator for the annessaia runtime.
//
// Arithmetic runs on f64 and the display is formatted to trim floating-point
// noise, so 0.1 + 0.2 reads as 0.3 rather than 0.30000000000000004.

use annessaia_sdk::prelude::*;
use std::cell::Cell;
use std::sync::Mutex;

const INK:    Color = Color::rgb(226, 232, 240);
const DIM:    Color = Color::rgb(100, 116, 139);
const ACCENT: Color = Color::rgb( 37,  99, 235);
const OP_BG:  Color = Color::rgb( 30,  41,  59);
const NUM_BG: Color = Color::rgb( 22,  30,  48);
const FN_BG:  Color = Color::rgb( 51,  65,  85);

struct Calc {
    entry:   String,        // what the user is currently typing
    acc:     f64,           // left-hand side of a pending operation
    pending: Option<char>,
    fresh:   bool,          // next digit replaces the entry rather than appending
    last_op: Option<(char, f64)>,  // repeat on consecutive presses of =
    memo:    String,        // small line above the display showing the pending op
}

static CALC: Mutex<Calc> = Mutex::new(Calc {
    entry: String::new(), acc: 0.0, pending: None,
    fresh: true, last_op: None, memo: String::new(),
});

impl Calc {
    fn value(&self) -> f64 { self.entry.parse().unwrap_or(0.0) }

    fn show(&self) -> String {
        if self.entry.is_empty() { "0".into() } else { self.entry.clone() }
    }

    fn digit(&mut self, d: char) {
        if self.fresh { self.entry.clear(); self.fresh = false; }
        if d == '.' {
            if self.entry.contains('.') { return; }
            if self.entry.is_empty() { self.entry.push('0'); }
        }
        // Keep the display from overflowing its box.
        if self.entry.trim_start_matches('-').len() >= 14 { return; }
        self.entry.push(d);
    }

    fn clear(&mut self) {
        self.entry.clear();
        self.acc = 0.0;
        self.pending = None;
        self.fresh = true;
        self.last_op = None;
        self.memo.clear();
    }

    fn negate(&mut self) {
        if self.entry.starts_with('-') { self.entry.remove(0); }
        else if !self.entry.is_empty() && self.entry != "0" { self.entry.insert(0, '-'); }
    }

    fn percent(&mut self) {
        let v = self.value() / 100.0;
        self.entry = fmt(v);
        self.fresh = true;
    }

    fn op(&mut self, o: char) {
        // Chaining (2 + 3 + …) folds the previous operation before starting the next.
        if self.pending.is_some() && !self.fresh {
            self.equals();
        } else if self.entry.is_empty() {
            self.acc = 0.0;
        } else {
            self.acc = self.value();
        }
        self.pending = Some(o);
        self.fresh = true;
        self.memo = format!("{} {}", fmt(self.acc), o);
    }

    fn equals(&mut self) {
        let (o, rhs) = match self.pending {
            Some(o) => (o, self.value()),
            // Pressing = again repeats the last operation, as physical calculators do.
            None => match self.last_op { Some((o, r)) => (o, r), None => return },
        };
        let lhs = if self.pending.is_some() { self.acc } else { self.value() };
        let r = match o {
            '+' => lhs + rhs,
            '-' => lhs - rhs,
            '*' => lhs * rhs,
            '/' => if rhs == 0.0 { f64::NAN } else { lhs / rhs },
            _   => rhs,
        };
        self.last_op = Some((o, rhs));
        self.acc = r;
        self.entry = fmt(r);
        self.pending = None;
        self.fresh = true;
        self.memo.clear();
    }
}

// Trim float noise: render at 12 significant digits, then strip trailing zeros.
fn fmt(v: f64) -> String {
    if v.is_nan() { return "not a number".into(); }
    if v.is_infinite() { return "infinity".into(); }
    if v == 0.0 { return "0".into(); }

    let a = v.abs();
    if a >= 1e12 || a < 1e-9 {
        return format!("{:e}", v);
    }
    let mut s = format!("{:.*}", 12, v);
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    s
}

#[no_mangle]
pub extern "C" fn render() {
    let mut c = CALC.lock().unwrap();

    row(|| {
        text("Calculator", 22.0, INK);
    });
    space(10.0);

    // ── Display ───────────────────────────────────────────────────────────────
    card_color(Color::rgb(10, 16, 30), || {
        if c.memo.is_empty() { small(" "); } else { small(&c.memo); }
        let shown = c.show();
        // Shrink the type as the number grows so it never wraps out of the card.
        let size = if shown.len() > 11 { 26.0 } else if shown.len() > 8 { 32.0 } else { 40.0 };
        text(&shown, size, INK);
    });
    space(12.0);

    // ── Keypad ────────────────────────────────────────────────────────────────
    // A Cell rather than a plain local: columns4 takes four closures at once, and
    // four `&mut` captures of the same variable would not borrow-check.
    let hit: Cell<Option<char>> = Cell::new(None);

    columns4(
        || if fnbtn("  C  ")  { hit.set(Some('C')) },
        || if fnbtn("  ±  ")  { hit.set(Some('~')) },
        || if fnbtn("  %  ")  { hit.set(Some('%')) },
        || if opbtn("  ÷  ")  { hit.set(Some('/')) },
    );
    space(6.0);
    columns4(
        || if numbtn(" 7 ") { hit.set(Some('7')) },
        || if numbtn(" 8 ") { hit.set(Some('8')) },
        || if numbtn(" 9 ") { hit.set(Some('9')) },
        || if opbtn("  ×  ") { hit.set(Some('*')) },
    );
    space(6.0);
    columns4(
        || if numbtn(" 4 ") { hit.set(Some('4')) },
        || if numbtn(" 5 ") { hit.set(Some('5')) },
        || if numbtn(" 6 ") { hit.set(Some('6')) },
        || if opbtn("  −  ") { hit.set(Some('-')) },
    );
    space(6.0);
    columns4(
        || if numbtn(" 1 ") { hit.set(Some('1')) },
        || if numbtn(" 2 ") { hit.set(Some('2')) },
        || if numbtn(" 3 ") { hit.set(Some('3')) },
        || if opbtn("  +  ") { hit.set(Some('+')) },
    );
    space(6.0);
    columns4(
        || if numbtn(" 0 ") { hit.set(Some('0')) },
        || if numbtn(" . ") { hit.set(Some('.')) },
        || {},
        || if eqbtn("  =  ") { hit.set(Some('=')) },
    );

    if let Some(k) = hit.get() {
        match k {
            '0'..='9' | '.' => c.digit(k),
            '+' | '-' | '*' | '/' => c.op(k),
            '=' => c.equals(),
            'C' => c.clear(),
            '~' => c.negate(),
            '%' => c.percent(),
            _ => {}
        }
    }

    space(14.0);
    small("Chained operations fold as you go — 2 + 3 × 4 evaluates left to right.");
    small("Press = again to repeat the last operation.");
}

fn numbtn(s: &str) -> bool { button_styled(s, INK, NUM_BG, Color::rgb(40, 52, 76)) }
fn opbtn(s: &str)  -> bool { button_styled(s, INK, OP_BG, ACCENT) }
fn fnbtn(s: &str)  -> bool { button_styled(s, DIM, FN_BG, Color::rgb(71, 85, 105)) }
fn eqbtn(s: &str)  -> bool { button_styled(s, Color::WHITE, ACCENT, ACCENT) }
