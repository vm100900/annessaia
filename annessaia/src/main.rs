use arboard::Clipboard;
#[cfg(feature = "ai")]
use base64::{engine::general_purpose::STANDARD, Engine as _};
use eframe::egui::{self, Color32, RichText, Stroke};
#[cfg(feature = "ai")]
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{Arc, Mutex, mpsc::{self, Receiver}};
#[cfg(feature = "ai")]
use std::sync::OnceLock;
use std::time::Instant;
use wasmtime::{Caller, Engine, Linker, Memory, Module, Store, TypedFunc};

// ── Local embedding ──────────────────────────────────────────────────────────
//
// Lets a WASM app (namely search.wasm) embed a search query on-device, so even
// a read-only, keyword-only hosted index can be searched by meaning: the query
// vector travels to the server, the server never has to run a model itself.
//
// Deliberately a process-wide lazy singleton rather than per-app state: each
// WASM app load creates a fresh HostState, and reloading a ~300M-parameter ONNX
// model every time the user switched apps would be a real cost most apps never
// need. The first call — from any app, ever, this run — pays for loading it
// once; every app after that (including a different one) reuses the same
// instance.
//
// Same model, quantization scheme, and truncated dimension as
// annessaia-server's, so a vector computed here compares correctly against one
// a node computed for an app — EmbeddingGemma-300M, native 768 dims truncated
// to 256 (see EXPECTED_DIMS in server/src/main.rs for why 256 specifically).
// Unlike annessaia-server's AppState, nothing outside the two functions
// below ever references this — so, unlike that Embedder alias, this one is
// gated out entirely rather than kept around as a zero-cost `()` stand-in.
#[cfg(feature = "ai")]
type Embedder = TextEmbedding;

#[cfg(feature = "ai")]
static EMBEDDER: OnceLock<Mutex<Option<Embedder>>> = OnceLock::new();

#[cfg(feature = "ai")]
fn embed_query_blocking(text: &str) -> Option<String> {
    if AI_PREF.load(std::sync::atomic::Ordering::Relaxed) == 0 { return None; }
    let lock = EMBEDDER.get_or_init(|| {
        Mutex::new(match TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::EmbeddingGemma300M)) {
            Ok(m)  => { println!("[embed] local model ready"); Some(m) }
            Err(e) => { eprintln!("[embed] model unavailable, no semantic search: {e}"); None }
        })
    });
    let mut guard = lock.lock().unwrap();
    let model = guard.as_mut()?;
    // EmbeddingGemma's task-prefixed query format — matches annessaia-server's
    // embed_query. Getting this convention wrong (as happened with bge-small's
    // simpler prefix initially being omitted) miscalibrates similarity scores
    // across the board rather than just weakening them.
    let out = model.embed(vec![format!("task: search result | query: {text}")], None).ok()?;
    Some(quantize(&out.into_iter().next()?))
}
#[cfg(not(feature = "ai"))]
fn embed_query_blocking(_text: &str) -> Option<String> { None }

/// Sibling to `embed_query_blocking` for the *document* side of the same
/// prefix convention — used when a WASM app wants to index its own richer
/// content (e.g. a gallery embedding all its widgets' names/descriptions),
/// not a search query. Matches annessaia-server's `embed_input` convention
/// exactly, including the 1000-char cap: unlike a query, an app-supplied
/// content blob has no natural length limit, so this needs its own
/// truncation where `embed_query_blocking` doesn't.
#[cfg(feature = "ai")]
fn embed_document_blocking(text: &str) -> Option<String> {
    if AI_PREF.load(std::sync::atomic::Ordering::Relaxed) == 0 { return None; }
    let lock = EMBEDDER.get_or_init(|| {
        Mutex::new(match TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::EmbeddingGemma300M)) {
            Ok(m)  => { println!("[embed] local model ready"); Some(m) }
            Err(e) => { eprintln!("[embed] model unavailable, no semantic search: {e}"); None }
        })
    });
    let mut guard = lock.lock().unwrap();
    let model = guard.as_mut()?;
    let mut input = format!("title: none | text: {text}");
    input.truncate(input.char_indices().nth(1000).map(|(i, _)| i).unwrap_or(input.len()));
    let out = model.embed(vec![input], None).ok()?;
    Some(quantize(&out.into_iter().next()?))
}
#[cfg(not(feature = "ai"))]
fn embed_document_blocking(_text: &str) -> Option<String> { None }

// L2-normalize then scale to int8, truncating to the first 256 of the model's
// native 768 dims first — identical algorithm to annessaia-server's `quantize`,
// so vectors from either side compare correctly. Matryoshka Representation
// Learning is specifically designed so a prefix of the full vector is a valid,
// independently-usable embedding at that length.
#[cfg(feature = "ai")]
fn quantize(v: &[f32]) -> String {
    let v = &v[..256.min(v.len())];
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    let bytes: Vec<u8> = v.iter()
        .map(|x| (((x / norm) * 127.0).round().clamp(-127.0, 127.0)) as i8 as u8)
        .collect();
    STANDARD.encode(bytes)
}

// ── Local audio ───────────────────────────────────────────────────────────────
//
// Lets a WASM app play sound: short one-shot effects (fire-and-forget, several
// may overlap) and longer looped clips it can later stop or adjust the volume
// of. Decoding happens host-side via `rodio` — the guest just hands over raw
// audio bytes (WAV/MP3/OGG/FLAC, whatever the app embeds via include_bytes!)
// and, for the looped case, a small integer handle to refer back to it.
//
// Thread-local rather than a shared static: the underlying audio stream isn't
// Send (confirmed by the compiler, not assumed), so it can't live behind a
// plain `static Mutex<...>`. Every host import in this file already runs on
// whichever thread drives the wasmtime Store — the same one, every frame —
// so a thread-local costs nothing in practice and sidesteps the Send
// requirement entirely rather than working around it.
struct AudioState {
    stream: OutputStream,   // must stay alive — dropping it silences everything
    next_id: i32,
    looped: HashMap<i32, Sink>,
}

enum AudioSlot { Uninit, Ready(AudioState), Unavailable }

thread_local! {
    static AUDIO: std::cell::RefCell<AudioSlot> = std::cell::RefCell::new(AudioSlot::Uninit);
}

fn with_audio<R>(f: impl FnOnce(&mut AudioState) -> R) -> Option<R> {
    AUDIO.with(|cell| {
        let mut slot = cell.borrow_mut();
        if matches!(*slot, AudioSlot::Uninit) {
            *slot = match OutputStreamBuilder::open_default_stream() {
                Ok(stream) => {
                    println!("[sound] audio output ready");
                    AudioSlot::Ready(AudioState { stream, next_id: 1, looped: HashMap::new() })
                }
                Err(e) => {
                    eprintln!("[sound] no audio output available: {e}");
                    AudioSlot::Unavailable
                }
            };
        }
        match &mut *slot {
            AudioSlot::Ready(state) => Some(f(state)),
            _ => None,
        }
    })
}

fn decode(bytes: &[u8]) -> Option<Decoder<std::io::Cursor<Vec<u8>>>> {
    Decoder::new(std::io::Cursor::new(bytes.to_vec())).ok()
}

// Fire-and-forget: play once, several may overlap, nothing to hold onto.
fn sound_play(bytes: &[u8]) {
    let Some(source) = decode(bytes) else {
        eprintln!("[sound] could not decode audio ({} bytes)", bytes.len());
        return;
    };
    with_audio(|state| {
        let sink = Sink::connect_new(state.stream.mixer());
        sink.append(source);
        sink.detach();   // plays to completion without anything holding it alive
    });
}

// Looped: kept in the registry so the guest can stop it or change its volume
// later via the returned handle. Returns -1 on failure (no audio device, or
// the bytes didn't decode) rather than a handle that would silently do nothing.
fn sound_play_looped(bytes: &[u8]) -> i32 {
    let Some(source) = decode(bytes) else {
        eprintln!("[sound] could not decode audio ({} bytes)", bytes.len());
        return -1;
    };
    with_audio(|state| {
        let sink = Sink::connect_new(state.stream.mixer());
        sink.append(source.repeat_infinite());
        let id = state.next_id;
        state.next_id += 1;
        state.looped.insert(id, sink);
        id
    }).unwrap_or(-1)
}

fn sound_stop(handle: i32) {
    with_audio(|state| { if let Some(sink) = state.looped.remove(&handle) { sink.stop(); } });
}

fn sound_set_volume(handle: i32, volume: f32) {
    with_audio(|state| { if let Some(sink) = state.looped.get(&handle) { sink.set_volume(volume.max(0.0)); } });
}

// ── Toggle switch widget ─────────────────────────────────────────────────────
//
// egui has no built-in switch — this is the standard recipe (from egui's own
// demo lib) for a two-state pill with an animated knob, used wherever an
// on/off choice reads better as a switch than a button (the AI setup screen).
#[cfg(feature = "ai")]
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        ui.painter().rect(rect, radius, visuals.bg_fill, visuals.bg_stroke);
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(circle_x, rect.center().y);
        ui.painter().circle(center, 0.75 * radius, visuals.fg_stroke.color, visuals.fg_stroke);
    }
    response
}

// ── Color helper ──────────────────────────────────────────────────────────────

fn unpack(c: i32) -> Color32 {
    let c = c as u32;
    Color32::from_rgba_unmultiplied(
        ((c >> 24) & 0xFF) as u8,
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}

// ── Widget / GPU command types ────────────────────────────────────────────────

enum WidgetCmd {
    Heading(String),
    Label(String),
    Small(String),
    ColoredLabel(String, Color32),
    Code(String),
    // `key` is the label plus its occurrence index within the frame. Identity by
    // label alone made every button sharing a caption a single button: a list of
    // rows each with an " Open " button would all fire together.
    Button { label: String, key: String },
    ButtonStyled { label: String, key: String, fg: Color32, bg: Color32, border: Color32 },
    Checkbox { id: i32, label: String, checked: bool },
    Slider   { id: i32, label: String, min: f32, max: f32, value: f32 },
    Progress { value: f32, label: String },
    TextEdit { id: i32, hint: String, secret: bool },
    Text { s: String, size: f32, color: Color32 },
    Badge { s: String, color: Color32 },
    RowBegin, RowEnd,
    CardBegin, CardEnd, CardBeginColor(Color32),
    ColumnsBegin(i32), ColumnNext, ColumnsEnd,
    Separator,
    Space(f32),
    Image { id: i32, w: f32, h: f32 },
}

// ── Widget state updates (collected during render, applied back to HostState) ──

#[derive(Default)]
struct WidgetUpdates {
    clicks:     Vec<String>,
    checkboxes: Vec<(i32, bool)>,
    sliders:    Vec<(i32, f32)>,
    texts:      Vec<(i32, String)>,
}

// ── Recursive widget renderer ─────────────────────────────────────────────────

fn split_columns(cmds: &[WidgetCmd]) -> Vec<&[WidgetCmd]> {
    let mut segments: Vec<&[WidgetCmd]> = Vec::new();
    let mut depth = 0i32;
    let mut seg_start = 0;
    for (i, cmd) in cmds.iter().enumerate() {
        match cmd {
            WidgetCmd::RowBegin | WidgetCmd::CardBegin | WidgetCmd::CardBeginColor(_) | WidgetCmd::ColumnsBegin(_) => depth += 1,
            WidgetCmd::RowEnd   | WidgetCmd::CardEnd   | WidgetCmd::ColumnsEnd      => depth -= 1,
            WidgetCmd::ColumnNext if depth == 0 => {
                segments.push(&cmds[seg_start..i]);
                seg_start = i + 1;
            }
            _ => {}
        }
    }
    segments.push(&cmds[seg_start..]);
    segments
}

fn find_end(cmds: &[WidgetCmd], begin: fn(&WidgetCmd) -> bool, end: fn(&WidgetCmd) -> bool) -> usize {
    let mut depth = 0i32;
    for (i, c) in cmds.iter().enumerate() {
        if begin(c) { depth += 1; }
        else if end(c) { if depth == 0 { return i; } depth -= 1; }
    }
    cmds.len()
}

fn render_widgets(
    ui: &mut egui::Ui,
    cmds: &[WidgetCmd],
    text_states: &mut HashMap<i32, String>,
    upd: &mut WidgetUpdates,
    images: &HashMap<i32, egui::TextureHandle>,
) {
    let mut i = 0;
    while i < cmds.len() {
        match &cmds[i] {
            // ── Layout containers ─────────────────────────────────────────────
            WidgetCmd::RowBegin => {
                let rest = &cmds[i+1..];
                let end  = find_end(rest, |c| matches!(c, WidgetCmd::RowBegin), |c| matches!(c, WidgetCmd::RowEnd));
                ui.horizontal(|ui| render_widgets(ui, &rest[..end], text_states, upd, images));
                i += end + 2; continue;
            }
            WidgetCmd::CardBegin => {
                let rest = &cmds[i+1..];
                let end  = find_end(rest, |c| matches!(c, WidgetCmd::CardBegin | WidgetCmd::CardBeginColor(_)), |c| matches!(c, WidgetCmd::CardEnd));
                egui::Frame::group(ui.style()).show(ui, |ui| render_widgets(ui, &rest[..end], text_states, upd, images));
                i += end + 2; continue;
            }
            WidgetCmd::CardBeginColor(bg) => {
                let rest = &cmds[i+1..];
                let end  = find_end(rest, |c| matches!(c, WidgetCmd::CardBegin | WidgetCmd::CardBeginColor(_)), |c| matches!(c, WidgetCmd::CardEnd));
                egui::Frame::none()
                    .fill(*bg)
                    .rounding(8.0)
                    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255,255,255,20)))
                    .inner_margin(12.0)
                    .show(ui, |ui| render_widgets(ui, &rest[..end], text_states, upd, images));
                i += end + 2; continue;
            }
            WidgetCmd::RowEnd | WidgetCmd::CardEnd | WidgetCmd::ColumnsEnd | WidgetCmd::ColumnNext => { i += 1; continue; }
            WidgetCmd::ColumnsBegin(n) => {
                let rest = &cmds[i+1..];
                let end  = find_end(rest, |c| matches!(c, WidgetCmd::ColumnsBegin(_)), |c| matches!(c, WidgetCmd::ColumnsEnd));
                let segments = split_columns(&rest[..end]);
                let n_cols = *n as usize;
                ui.columns(n_cols, |cols| {
                    for (idx, seg) in segments.iter().enumerate() {
                        if idx < cols.len() {
                            render_widgets(&mut cols[idx], seg, text_states, upd, images);
                        }
                    }
                });
                i += end + 2; continue;
            }

            // ── Text ──────────────────────────────────────────────────────────
            WidgetCmd::Heading(s) => {
                ui.label(RichText::new(s).size(26.0).color(Color32::WHITE).strong());
                ui.add_space(4.0);
            }
            WidgetCmd::Label(s) => {
                ui.label(RichText::new(s).size(14.0).color(Color32::from_rgb(148,163,184)));
            }
            WidgetCmd::Small(s) => {
                ui.label(RichText::new(s).size(11.0).color(Color32::from_rgb(71,85,105)));
            }
            WidgetCmd::ColoredLabel(s, color) => {
                ui.label(RichText::new(s).size(14.0).color(*color));
            }
            WidgetCmd::Code(s) => {
                ui.label(
                    RichText::new(s).size(13.0)
                        .monospace()
                        .color(Color32::from_rgb(134,239,172))
                        .background_color(Color32::from_rgb(15,30,15)),
                );
            }

            // ── Button ────────────────────────────────────────────────────────
            WidgetCmd::Button { label, key } => {
                let btn = egui::Button::new(RichText::new(label.as_str()).color(Color32::from_rgb(56,189,248)))
                    .fill(Color32::from_rgb(8,47,73))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(3,105,161)));
                if ui.add(btn).clicked() { upd.clicks.push(key.clone()); }
            }
            WidgetCmd::ButtonStyled { label, key, fg, bg, border } => {
                let btn = egui::Button::new(RichText::new(label.as_str()).color(*fg))
                    .fill(*bg)
                    .stroke(Stroke::new(1.0, *border))
                    .rounding(6.0);
                if ui.add(btn).clicked() { upd.clicks.push(key.clone()); }
            }

            // ── Checkbox ──────────────────────────────────────────────────────
            WidgetCmd::Checkbox { id, label, checked } => {
                let mut v = *checked;
                let resp = ui.checkbox(&mut v, label.as_str());
                if resp.changed() { upd.checkboxes.push((*id, v)); }
            }

            // ── Slider ────────────────────────────────────────────────────────
            WidgetCmd::Slider { id, label, min, max, value } => {
                let mut v = *value;
                let resp = ui.add(
                    egui::Slider::new(&mut v, *min..=*max)
                        .text(label.as_str())
                        .clamping(egui::SliderClamping::Always)
                );
                if resp.changed() { upd.sliders.push((*id, v)); }
            }

            // ── Progress bar ──────────────────────────────────────────────────
            WidgetCmd::Progress { value, label } => {
                let bar = egui::ProgressBar::new(*value)
                    .fill(Color32::from_rgb(56,189,248));
                let bar = if label.is_empty() { bar } else { bar.text(label.as_str()) };
                ui.add(bar);
            }

            // ── Text input ────────────────────────────────────────────────────
            WidgetCmd::TextEdit { id, hint, secret } => {
                let entry = text_states.entry(*id).or_default();
                let resp = ui.add(
                    egui::TextEdit::singleline(entry)
                        .hint_text(hint.as_str())
                        .desired_width(f32::INFINITY)
                        .password(*secret)
                );
                if resp.changed() { upd.texts.push((*id, entry.clone())); }
            }

            // ── Text (arbitrary size + color) ──────────────────────────────────
            WidgetCmd::Text { s, size, color } => {
                ui.label(RichText::new(s).size(*size).color(*color));
            }

            // ── Badge (colored pill) ───────────────────────────────────────────
            WidgetCmd::Badge { s, color } => {
                egui::Frame::none()
                    .fill(*color)
                    .rounding(10.0)
                    .inner_margin(egui::Margin { left: 8.0, right: 8.0, top: 2.0, bottom: 2.0 })
                    .show(ui, |ui| {
                        ui.label(RichText::new(s).color(Color32::WHITE).size(12.0).strong());
                    });
            }

            // ── Misc ──────────────────────────────────────────────────────────
            WidgetCmd::Separator => { ui.add_space(8.0); ui.separator(); ui.add_space(8.0); }
            WidgetCmd::Space(px) => { ui.add_space(*px); }

            // ── Image ─────────────────────────────────────────────────────────
            // Silently draws nothing until the texture cache catches up (see
            // paint_gpu's Image arm) — at most one frame behind image_decode.
            WidgetCmd::Image { id, w, h } => {
                if let Some(tex) = images.get(id) {
                    ui.add(egui::Image::new((tex.id(), egui::vec2(*w, *h))));
                }
            }
        }
        i += 1;
    }
}

enum GpuCmd {
    Clear(Color32),
    Rect { x: f32, y: f32, w: f32, h: f32, rounding: f32, color: Color32 },
    Circle { cx: f32, cy: f32, r: f32, color: Color32 },
    CircleStroke { cx: f32, cy: f32, r: f32, color: Color32, thickness: f32 },
    Line { x1: f32, y1: f32, x2: f32, y2: f32, color: Color32, thickness: f32 },
    Triangle { x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: Color32 },
    // Horizontally centered on (x, y): x is the center, y is the top —
    // matches every call site in practice (titles, HUD labels, button
    // captions), so the guest never has to measure text width itself.
    Text { x: f32, y: f32, s: String, size: f32, color: Color32 },
    Image { id: i32, x: f32, y: f32, w: f32, h: f32 },
}

fn paint_gpu(painter: &egui::Painter, origin: egui::Pos2, cmd: &GpuCmd, images: &HashMap<i32, egui::TextureHandle>) {
    match cmd {
        GpuCmd::Clear(c) => { painter.rect_filled(painter.clip_rect(), 0.0, *c); }
        GpuCmd::Rect { x, y, w, h, rounding, color } => {
            painter.rect_filled(egui::Rect::from_min_size(origin + egui::vec2(*x,*y), egui::vec2(*w,*h)), *rounding, *color);
        }
        GpuCmd::Circle { cx, cy, r, color } => {
            painter.circle_filled(origin + egui::vec2(*cx,*cy), *r, *color);
        }
        GpuCmd::CircleStroke { cx, cy, r, color, thickness } => {
            painter.circle_stroke(origin + egui::vec2(*cx,*cy), *r, Stroke::new(*thickness, *color));
        }
        GpuCmd::Text { x, y, s, size, color } => {
            painter.text(
                origin + egui::vec2(*x,*y),
                egui::Align2::CENTER_TOP,
                s,
                egui::FontId::proportional(*size),
                *color,
            );
        }
        GpuCmd::Line { x1, y1, x2, y2, color, thickness } => {
            painter.line_segment([origin+egui::vec2(*x1,*y1), origin+egui::vec2(*x2,*y2)], Stroke::new(*thickness,*color));
        }
        GpuCmd::Triangle { x1,y1,x2,y2,x3,y3,color } => {
            painter.add(egui::Shape::convex_polygon(
                vec![origin+egui::vec2(*x1,*y1), origin+egui::vec2(*x2,*y2), origin+egui::vec2(*x3,*y3)],
                *color, Stroke::NONE));
        }
        // Silently does nothing until the texture cache (populated once per id,
        // in Browser::update — the only place with access to `ctx`) catches up;
        // that's at most one frame behind a fresh image_decode.
        GpuCmd::Image { id, x, y, w, h } => {
            if let Some(tex) = images.get(id) {
                painter.image(
                    tex.id(),
                    egui::Rect::from_min_size(origin + egui::vec2(*x,*y), egui::vec2(*w,*h)),
                    egui::Rect::from_min_max(egui::pos2(0.0,0.0), egui::pos2(1.0,1.0)),
                    Color32::WHITE,
                );
            }
        }
    }
}

// ── Input snapshot ────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct InputSnapshot {
    mouse_x: f32, mouse_y: f32,
    mouse_left_down: bool, mouse_right_down: bool, mouse_middle_down: bool,
    mouse_left_clicked: bool, mouse_right_clicked: bool,
    scroll_x: f32, scroll_y: f32,
    drag_x: f32, drag_y: f32,
    touches: Vec<(f32, f32)>,
    // Keyboard. Codes are the shared numbering in key_code(); guests get them
    // from the SDK's Key enum, so neither side hardcodes the other's values.
    keys_down: HashSet<i32>,
    keys_pressed: HashSet<i32>,
    keys_released: HashSet<i32>,
    modifiers: i32,      // bitmask: 1 shift, 2 ctrl, 4 alt, 8 command
    typed: String,       // characters produced this frame, already composed
}

// Stable key numbering shared with the SDK. Appending is safe; renumbering is not,
// because guests compiled against an older SDK would silently read the wrong keys.
fn key_code(k: egui::Key) -> i32 {
    use egui::Key::*;
    match k {
        A=>0, B=>1, C=>2, D=>3, E=>4, F=>5, G=>6, H=>7, I=>8, J=>9, K=>10, L=>11, M=>12,
        N=>13, O=>14, P=>15, Q=>16, R=>17, S=>18, T=>19, U=>20, V=>21, W=>22, X=>23, Y=>24, Z=>25,

        Num0=>26, Num1=>27, Num2=>28, Num3=>29, Num4=>30,
        Num5=>31, Num6=>32, Num7=>33, Num8=>34, Num9=>35,

        ArrowLeft=>36, ArrowRight=>37, ArrowUp=>38, ArrowDown=>39,

        Space=>40, Enter=>41, Escape=>42, Tab=>43, Backspace=>44, Delete=>45,
        Insert=>46, Home=>47, End=>48, PageUp=>49, PageDown=>50,

        Minus=>51, Plus=>52, Equals=>53, Comma=>54, Period=>55, Slash=>56,
        Backslash=>57, Semicolon=>58, Colon=>59, Backtick=>60,
        OpenBracket=>61, CloseBracket=>62, Pipe=>63, Questionmark=>64,

        F1=>70, F2=>71, F3=>72, F4=>73, F5=>74, F6=>75,
        F7=>76, F8=>77, F9=>78, F10=>79, F11=>80, F12=>81,

        _ => -1,   // Copy/Cut/Paste and F13+ are not exposed
    }
}

// ── Persistent storage helpers ────────────────────────────────────────────────

fn storage_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::Path::new(&home).join(".annessaia").join("storage");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn history_file() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::Path::new(&home).join(".annessaia");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history.txt"))
}

fn safe_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}

fn disk_storage_get(key: &str) -> Option<Vec<u8>> {
    std::fs::read(storage_dir()?.join(safe_key(key))).ok()
}

fn disk_storage_set(key: &str, val: &[u8]) {
    if let Some(dir) = storage_dir() {
        let _ = std::fs::write(dir.join(safe_key(key)), val);
    }
}

fn disk_storage_delete(key: &str) {
    if let Some(dir) = storage_dir() {
        let _ = std::fs::remove_file(dir.join(safe_key(key)));
    }
}

fn disk_storage_clear() {
    if let Some(dir) = storage_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() { let _ = std::fs::remove_file(e.path()); }
        }
    }
}

fn load_history() -> Vec<String> {
    history_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

fn append_history(url: &str) {
    if let Some(p) = history_file() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{url}");
        }
    }
}

// ── .wasmpackage — a zip of app.wasm + assets/ ─────────────────────────────────
//
// Detected by content, not by URL/file extension: a server can mislabel or
// omit an extension entirely, but the zip local-file-header signature is
// always the first four bytes of a real zip, package or not.
fn is_zip(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[..4] == [0x50, 0x4B, 0x03, 0x04]
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// A .wasmh file is its real content (a bare .wasm, or a .wasmpackage zip)
// with a raw 32-byte SHA-256 digest of that content appended at the very
// end — not a hash baked into the filename. Checked by recomputing and
// comparing, not by trusting a ".wasmh" extension: a plain .wasm/.wasmpackage
// with no trailer at all (the overwhelming majority of files that will ever
// reach this function) simply fails the comparison and is returned
// untouched. A file coincidentally ending in 32 bytes that happen to equal
// the SHA-256 of everything before them is a 2^-256 event — safe to treat
// as certain either way.
const HASH_TRAILER_LEN: usize = 32;

fn strip_hash_trailer(raw: &[u8]) -> (Option<String>, &[u8]) {
    if raw.len() <= HASH_TRAILER_LEN { return (None, raw); }
    let split = raw.len() - HASH_TRAILER_LEN;
    let (body, trailer) = raw.split_at(split);
    use sha2::{Digest, Sha256};
    if Sha256::digest(body).as_slice() == trailer {
        (Some(hex_encode(trailer)), body)
    } else {
        (None, raw)
    }
}

fn pkg_cache_dir(hash: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::Path::new(&home).join(".annessaia").join("pkg_cache").join(hash))
}

// Reads a previously-cached app.wasm + assets/ back off disk, if present.
// Shared by resolve_wasm (checking after a fetch/read) and load_url
// (checking *before* one, for a URL whose name already tells us the hash).
fn load_from_cache(hash: &str) -> Option<(Vec<u8>, HashMap<String, Vec<u8>>)> {
    let dir = pkg_cache_dir(hash)?;
    let wasm = std::fs::read(dir.join("app.wasm")).ok()?;
    let mut assets = HashMap::new();
    if let Ok(entries) = walk_files(&dir.join("assets")) {
        for (rel, path) in entries {
            if let Ok(data) = std::fs::read(&path) { assets.insert(rel, data); }
        }
    }
    Some((wasm, assets))
}

fn save_to_cache(hash: &str, wasm: &[u8], assets: &HashMap<String, Vec<u8>>) {
    let Some(dir) = pkg_cache_dir(hash) else { return };
    let _ = std::fs::create_dir_all(dir.join("assets"));
    let _ = std::fs::write(dir.join("app.wasm"), wasm);
    for (rel, data) in assets {
        let dest = dir.join("assets").join(rel);
        if let Some(parent) = dest.parent() { let _ = std::fs::create_dir_all(parent); }
        let _ = std::fs::write(dest, data);
    }
}

// Turns whatever was fetched/read (a bare .wasm module, or a .wasmpackage
// zip of app.wasm + assets/) into (content hash, wasm bytes, assets) —
// exactly what a .wasmh-named build's hash refers to, whichever kind it is.
// Keyed and cached on disk by the SHA-256 of the *whole* input — the whole
// archive for a package, since a changed asset must hash differently even
// if app.wasm itself didn't change. Loading the exact same bytes twice (the
// common case — reopening an app, or another node serving an unchanged
// build) reuses what's already on disk instead of re-inflating a zip and
// rewriting every asset file again; a changed build hashes differently and
// simply gets its own cache directory, nothing to invalidate.
fn resolve_wasm(raw: &[u8]) -> anyhow::Result<(String, Vec<u8>, HashMap<String, Vec<u8>>)> {
    let (trailer_hash, body) = strip_hash_trailer(raw);
    // A genuine trailer's own digest *is* the canonical hash — reuse it
    // rather than hashing body a second time. No trailer at all (an
    // ordinary .wasm/.wasmpackage) falls back to hashing the whole input,
    // same as before this format existed.
    let hash = trailer_hash.unwrap_or_else(|| sha256_hex(body));

    if let Some((wasm, assets)) = load_from_cache(&hash) {
        println!("[wasmh] {hash} — cache hit, reusing on-disk copy");
        return Ok((hash, wasm, assets));
    }

    let (wasm_bytes, assets) = if is_zip(body) {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(body))?;
        let mut wasm_bytes: Option<Vec<u8>> = None;
        let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            if entry.is_dir() { continue; }
            let name = entry.name().to_string();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data)?;
            if name == "app.wasm" {
                wasm_bytes = Some(data);
            } else if let Some(rel) = name.strip_prefix("assets/") {
                if !rel.is_empty() { assets.insert(rel.to_string(), data); }
            }
        }
        (wasm_bytes.ok_or_else(|| anyhow::anyhow!(".wasmpackage has no app.wasm"))?, assets)
    } else {
        (body.to_vec(), HashMap::new())
    };

    save_to_cache(&hash, &wasm_bytes, &assets);
    println!("[wasmh] {hash} — cached ({} asset file(s))", assets.len());
    Ok((hash, wasm_bytes, assets))
}

// Peeks at just the trailing HASH_TRAILER_LEN bytes of a .wasmh URL via an
// HTTP Range request, so load_url can find out whether it already has this
// exact content cached *before* downloading the (potentially large) rest of
// it. Returns None on anything but a clean 206 Partial Content of exactly
// the right length — a server that ignores Range and returns 200 with the
// whole body is common and not an error, just not this shortcut; load_url
// falls back to a normal full GET in every None case.
fn peek_hash_via_range(url: &str) -> Option<String> {
    let resp = ureq::get(url).set("Range", &format!("bytes=-{HASH_TRAILER_LEN}")).call().ok()?;
    if resp.status() != 206 { return None; }
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).ok()?;
    (buf.len() == HASH_TRAILER_LEN).then(|| hex_encode(&buf))
}

// Every file under `dir`, recursively, as (path relative to dir, absolute path).
fn walk_files(dir: &std::path::Path) -> std::io::Result<Vec<(String, std::path::PathBuf)>> {
    let mut out = Vec::new();
    fn walk(base: &std::path::Path, current: &std::path::Path, out: &mut Vec<(String, std::path::PathBuf)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.push((rel.to_string_lossy().replace('\\', "/"), path.clone()));
            }
        }
        Ok(())
    }
    walk(dir, dir, &mut out)?;
    Ok(out)
}

// ── Host state ────────────────────────────────────────────────────────────────

struct HostState {
    widget_cmds: Vec<WidgetCmd>,
    gpu_cmds: Vec<GpuCmd>,
    clicked_prev: HashMap<String, bool>,
    clicked_curr: HashMap<String, bool>,
    // How many times each button label has been emitted so far this frame, so
    // repeated labels get distinct identities. Cleared at the start of each tick.
    btn_seq: HashMap<String, u32>,
    start_time: Instant,
    canvas_w: f32,
    canvas_h: f32,
    input: InputSnapshot,

    // Storage: in-memory cache backed by ~/.annessaia/storage/
    storage: HashMap<String, Vec<u8>>,

    // Navigation request from WASM (handled by Browser after tick)
    nav_request: Option<NavRequest>,

    // Read-only view of browser history (synced before each tick)
    nav_history: Vec<String>,
    nav_pos: usize,

    // File save request from WASM (handled by Browser after tick)
    pending_save: Option<(Vec<u8>, String)>,
    picked: Option<(String, Vec<u8>)>,   // most recent file_pick result

    // In-flight HTTP requests shared with spawned threads
    fetches: FetchMap,

    // Retained state for stateful widgets (persists across frames)
    checkbox_states: HashMap<i32, bool>,
    slider_states:   HashMap<i32, f32>,
    text_states:     HashMap<i32, String>,

    // Decoded images (id -> width, height, RGBA8 pixels), persists for the
    // app's lifetime — decoding happens once; Browser::update turns each id
    // into a cached egui texture the first time it's actually drawn.
    images: HashMap<i32, (u32, u32, Vec<u8>)>,
    next_image_id: i32,

    // Files bundled inside this app's .wasmpackage (empty for a bare .wasm
    // load) — keyed by their path under assets/ in the archive. Populated
    // before init() runs, so a guest can load an asset at startup.
    assets: HashMap<String, Vec<u8>>,
}

enum NavRequest { Push(String), Back, Forward }

// ── HTTP fetch state ──────────────────────────────────────────────────────────

#[derive(Clone)]
enum FetchResult { Pending, Done(Vec<u8>), Error }

type FetchMap = Arc<Mutex<HashMap<i32, FetchResult>>>;

impl HostState {
    fn new(assets: HashMap<String, Vec<u8>>) -> Self {
        Self {
            widget_cmds: Vec::new(),
            gpu_cmds: Vec::new(),
            clicked_prev: HashMap::new(),
            clicked_curr: HashMap::new(),
            btn_seq: HashMap::new(),
            start_time: Instant::now(),
            canvas_w: 960.0,
            canvas_h: 600.0,
            input: InputSnapshot::default(),
            storage: HashMap::new(),
            nav_request: None,
            nav_history: Vec::new(),
            nav_pos: 0,
            pending_save: None,
            picked: None,
            fetches: Arc::new(Mutex::new(HashMap::new())),
            checkbox_states: HashMap::new(),
            slider_states:   HashMap::new(),
            text_states:     HashMap::new(),
            images: HashMap::new(),
            next_image_id: 0,
            assets,
        }
    }
}

// ── String reader ─────────────────────────────────────────────────────────────

fn read_str(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> String {
    let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else { return String::new(); };
    let (ptr, len) = (ptr as usize, len as usize);
    let data = mem.data(&caller);
    if ptr + len > data.len() { return String::new(); }
    String::from_utf8_lossy(&data[ptr..ptr+len]).into_owned()
}

fn read_bytes(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Vec<u8> {
    let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else { return Vec::new(); };
    let (ptr, len) = (ptr as usize, len as usize);
    let data = mem.data(&caller);
    if ptr + len > data.len() { return Vec::new(); }
    data[ptr..ptr+len].to_vec()
}

fn write_bytes(caller: &mut Caller<'_, HostState>, dst_ptr: i32, dst_max: i32, src: &[u8]) -> i32 {
    let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else { return -1; };
    let copy_len = src.len().min(dst_max as usize);
    let dst = dst_ptr as usize;
    if dst + copy_len <= mem.data(&caller).len() {
        mem.data_mut(caller)[dst..dst+copy_len].copy_from_slice(&src[..copy_len]);
        copy_len as i32
    } else { -1 }
}

// ── Linker ────────────────────────────────────────────────────────────────────

fn make_linker(engine: &Engine) -> anyhow::Result<Linker<HostState>> {
    let mut l = Linker::new(engine);

    // ── Widget — text ─────────────────────────────────────────────────────────
    l.func_wrap("env", "ui_heading", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
        let s = read_str(&mut c, ptr, len); c.data_mut().widget_cmds.push(WidgetCmd::Heading(s));
    })?;
    l.func_wrap("env", "ui_label", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
        let s = read_str(&mut c, ptr, len); c.data_mut().widget_cmds.push(WidgetCmd::Label(s));
    })?;
    l.func_wrap("env", "ui_small", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
        let s = read_str(&mut c, ptr, len); c.data_mut().widget_cmds.push(WidgetCmd::Small(s));
    })?;
    // Colored label: color is packed RGBA i32 (same format as gpu_*)
    l.func_wrap("env", "ui_colored_label", |mut c: Caller<'_, HostState>, ptr: i32, len: i32, color: i32| {
        let s = read_str(&mut c, ptr, len);
        c.data_mut().widget_cmds.push(WidgetCmd::ColoredLabel(s, unpack(color)));
    })?;
    // Monospace code block
    l.func_wrap("env", "ui_code", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
        let s = read_str(&mut c, ptr, len); c.data_mut().widget_cmds.push(WidgetCmd::Code(s));
    })?;

    // ── Widget — interactive ──────────────────────────────────────────────────
    // Buttons are identified by label + how many times that label has already
    // appeared this frame. Two buttons reading " Open " in a list are then
    // distinct, where keying on the label alone made them the same button.
    fn next_btn_key(c: &mut Caller<'_, HostState>, label: &str) -> String {
        let d = c.data_mut();
        let n = d.btn_seq.entry(label.to_string()).or_insert(0);
        let key = format!("{label}#{n}");
        *n += 1;
        key
    }

    l.func_wrap("env", "ui_button", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
        let s = read_str(&mut c, ptr, len);
        let key = next_btn_key(&mut c, &s);
        let clicked = *c.data().clicked_prev.get(&key).unwrap_or(&false);
        c.data_mut().widget_cmds.push(WidgetCmd::Button { label: s, key });
        clicked as i32
    })?;

    // Checkbox — returns current checked state (host-managed); emits cmd for rendering
    l.func_wrap("env", "ui_checkbox", |mut c: Caller<'_, HostState>, id: i32, ptr: i32, len: i32| -> i32 {
        let label   = read_str(&mut c, ptr, len);
        let checked = *c.data().checkbox_states.get(&id).unwrap_or(&false);
        c.data_mut().widget_cmds.push(WidgetCmd::Checkbox { id, label, checked });
        checked as i32
    })?;

    // Slider — returns current value (host-managed); default used on first call
    l.func_wrap("env", "ui_slider", |mut c: Caller<'_, HostState>, id: i32, ptr: i32, len: i32, min: f32, max: f32, def: f32| -> f32 {
        let label = read_str(&mut c, ptr, len);
        let value = *c.data().slider_states.get(&id).unwrap_or(&def);
        c.data_mut().widget_cmds.push(WidgetCmd::Slider { id, label, min, max, value });
        value
    })?;

    // Progress bar — value in [0.0, 1.0], optional label
    l.func_wrap("env", "ui_progress", |mut c: Caller<'_, HostState>, value: f32| {
        c.data_mut().widget_cmds.push(WidgetCmd::Progress { value: value.clamp(0.0, 1.0), label: String::new() });
    })?;
    l.func_wrap("env", "ui_progress_text", |mut c: Caller<'_, HostState>, value: f32, ptr: i32, len: i32| {
        let label = read_str(&mut c, ptr, len);
        c.data_mut().widget_cmds.push(WidgetCmd::Progress { value: value.clamp(0.0, 1.0), label });
    })?;

    // Single-line text input — returns bytes of current text written to out_ptr, -1 if empty
    l.func_wrap("env", "ui_text_edit", |mut c: Caller<'_, HostState>, id: i32, hp: i32, hl: i32, op: i32, om: i32| -> i32 {
        let hint = read_str(&mut c, hp, hl);
        let text = c.data().text_states.get(&id).cloned().unwrap_or_default();
        c.data_mut().widget_cmds.push(WidgetCmd::TextEdit { id, hint, secret: false });
        write_bytes(&mut c, op, om, text.as_bytes())
    })?;
    // Masked variant of ui_text_edit — identical contract, but the host draws
    // entered characters as dots. Used for password fields.
    l.func_wrap("env", "ui_text_edit_secret", |mut c: Caller<'_, HostState>, id: i32, hp: i32, hl: i32, op: i32, om: i32| -> i32 {
        let hint = read_str(&mut c, hp, hl);
        let text = c.data().text_states.get(&id).cloned().unwrap_or_default();
        c.data_mut().widget_cmds.push(WidgetCmd::TextEdit { id, hint, secret: true });
        write_bytes(&mut c, op, om, text.as_bytes())
    })?;

    // ── Widget — layout ───────────────────────────────────────────────────────
    l.func_wrap("env", "ui_row_begin",      |mut c: Caller<'_, HostState>| { c.data_mut().widget_cmds.push(WidgetCmd::RowBegin); })?;
    l.func_wrap("env", "ui_row_end",        |mut c: Caller<'_, HostState>| { c.data_mut().widget_cmds.push(WidgetCmd::RowEnd); })?;
    l.func_wrap("env", "ui_card_begin",     |mut c: Caller<'_, HostState>| { c.data_mut().widget_cmds.push(WidgetCmd::CardBegin); })?;
    l.func_wrap("env", "ui_card_end",       |mut c: Caller<'_, HostState>| { c.data_mut().widget_cmds.push(WidgetCmd::CardEnd); })?;
    l.func_wrap("env", "ui_button_styled", |mut c: Caller<'_, HostState>, ptr:i32,len:i32,fg:i32,bg:i32,border:i32| -> i32 {
        let label  = read_str(&mut c, ptr, len);
        let key = next_btn_key(&mut c, &label);
        let clicked = *c.data().clicked_prev.get(&key).unwrap_or(&false);
        c.data_mut().widget_cmds.push(WidgetCmd::ButtonStyled { label, key, fg: unpack(fg), bg: unpack(bg), border: unpack(border) });
        clicked as i32
    })?;
    l.func_wrap("env", "ui_text", |mut c: Caller<'_, HostState>, ptr:i32,len:i32,size:f32,color:i32| {
        let s = read_str(&mut c, ptr, len);
        c.data_mut().widget_cmds.push(WidgetCmd::Text { s, size, color: unpack(color) });
    })?;
    l.func_wrap("env", "ui_badge", |mut c: Caller<'_, HostState>, ptr:i32,len:i32,color:i32| {
        let s = read_str(&mut c, ptr, len);
        c.data_mut().widget_cmds.push(WidgetCmd::Badge { s, color: unpack(color) });
    })?;
    l.func_wrap("env", "ui_card_color_begin", |mut c: Caller<'_, HostState>, color:i32| {
        c.data_mut().widget_cmds.push(WidgetCmd::CardBeginColor(unpack(color)));
    })?;

    l.func_wrap("env", "ui_columns_begin",  |mut c: Caller<'_, HostState>, n: i32| { c.data_mut().widget_cmds.push(WidgetCmd::ColumnsBegin(n)); })?;
    l.func_wrap("env", "ui_column_next",    |mut c: Caller<'_, HostState>| { c.data_mut().widget_cmds.push(WidgetCmd::ColumnNext); })?;
    l.func_wrap("env", "ui_columns_end",    |mut c: Caller<'_, HostState>| { c.data_mut().widget_cmds.push(WidgetCmd::ColumnsEnd); })?;

    // ── Widget — misc ─────────────────────────────────────────────────────────
    l.func_wrap("env", "ui_separator", |mut c: Caller<'_, HostState>| {
        c.data_mut().widget_cmds.push(WidgetCmd::Separator);
    })?;
    l.func_wrap("env", "ui_space", |mut c: Caller<'_, HostState>, px: f32| {
        c.data_mut().widget_cmds.push(WidgetCmd::Space(px));
    })?;

    // ── GPU ───────────────────────────────────────────────────────────────────
    l.func_wrap("env", "gpu_clear", |mut c: Caller<'_, HostState>, color: i32| {
        c.data_mut().gpu_cmds.push(GpuCmd::Clear(unpack(color)));
    })?;
    l.func_wrap("env", "gpu_rect", |mut c: Caller<'_, HostState>, x:f32,y:f32,w:f32,h:f32,rounding:f32,color:i32| {
        c.data_mut().gpu_cmds.push(GpuCmd::Rect{x,y,w,h,rounding,color:unpack(color)});
    })?;
    l.func_wrap("env", "gpu_circle", |mut c: Caller<'_, HostState>, cx:f32,cy:f32,r:f32,color:i32| {
        c.data_mut().gpu_cmds.push(GpuCmd::Circle{cx,cy,r,color:unpack(color)});
    })?;
    l.func_wrap("env", "gpu_circle_stroke", |mut c: Caller<'_, HostState>, cx:f32,cy:f32,r:f32,color:i32,thickness:f32| {
        c.data_mut().gpu_cmds.push(GpuCmd::CircleStroke{cx,cy,r,color:unpack(color),thickness});
    })?;
    l.func_wrap("env", "gpu_line", |mut c: Caller<'_, HostState>, x1:f32,y1:f32,x2:f32,y2:f32,color:i32,thickness:f32| {
        c.data_mut().gpu_cmds.push(GpuCmd::Line{x1,y1,x2,y2,color:unpack(color),thickness});
    })?;
    l.func_wrap("env", "gpu_triangle", |mut c: Caller<'_, HostState>, x1:f32,y1:f32,x2:f32,y2:f32,x3:f32,y3:f32,color:i32| {
        c.data_mut().gpu_cmds.push(GpuCmd::Triangle{x1,y1,x2,y2,x3,y3,color:unpack(color)});
    })?;
    // Horizontally centered on x; y is the top of the text, matching the
    // gpu::text SDK wrapper's contract.
    l.func_wrap("env", "gpu_text", |mut c: Caller<'_, HostState>, x:f32,y:f32,ptr:i32,len:i32,size:f32,color:i32| {
        let s = read_str(&mut c, ptr, len);
        c.data_mut().gpu_cmds.push(GpuCmd::Text{x,y,s,size,color:unpack(color)});
    })?;

    // ── Canvas / time ─────────────────────────────────────────────────────────
    l.func_wrap("env", "get_time",   |c: Caller<'_, HostState>| -> f64 { c.data().start_time.elapsed().as_secs_f64() })?;
    l.func_wrap("env", "get_width",  |c: Caller<'_, HostState>| -> f32 { c.data().canvas_w })?;
    l.func_wrap("env", "get_height", |c: Caller<'_, HostState>| -> f32 { c.data().canvas_h })?;

    // ── Input — mouse ─────────────────────────────────────────────────────────
    l.func_wrap("env", "input_mouse_x", |c: Caller<'_, HostState>| -> f32 { c.data().input.mouse_x })?;
    l.func_wrap("env", "input_mouse_y", |c: Caller<'_, HostState>| -> f32 { c.data().input.mouse_y })?;
    l.func_wrap("env", "input_mouse_down", |c: Caller<'_, HostState>, btn: i32| -> i32 {
        let i = &c.data().input;
        (match btn { 0=>i.mouse_left_down, 1=>i.mouse_right_down, 2=>i.mouse_middle_down, _=>false }) as i32
    })?;
    l.func_wrap("env", "input_mouse_clicked", |c: Caller<'_, HostState>, btn: i32| -> i32 {
        let i = &c.data().input;
        (match btn { 0=>i.mouse_left_clicked, 1=>i.mouse_right_clicked, _=>false }) as i32
    })?;

    // ── Keyboard ──────────────────────────────────────────────────────────────
    l.func_wrap("env", "input_key_down", |c: Caller<'_, HostState>, code: i32| -> i32 {
        c.data().input.keys_down.contains(&code) as i32
    })?;
    l.func_wrap("env", "input_key_pressed", |c: Caller<'_, HostState>, code: i32| -> i32 {
        c.data().input.keys_pressed.contains(&code) as i32
    })?;
    l.func_wrap("env", "input_key_released", |c: Caller<'_, HostState>, code: i32| -> i32 {
        c.data().input.keys_released.contains(&code) as i32
    })?;
    l.func_wrap("env", "input_modifiers", |c: Caller<'_, HostState>| -> i32 {
        c.data().input.modifiers
    })?;
    // Characters typed this frame, already composed — use this for text entry
    // rather than reconstructing from key codes, which would ignore layout.
    l.func_wrap("env", "input_text", |mut c: Caller<'_, HostState>, ptr: i32, max: i32| -> i32 {
        let s = c.data().input.typed.clone();
        write_bytes(&mut c, ptr, max, s.as_bytes())
    })?;
    l.func_wrap("env", "input_scroll_x", |c: Caller<'_, HostState>| -> f32 { c.data().input.scroll_x })?;
    l.func_wrap("env", "input_scroll_y", |c: Caller<'_, HostState>| -> f32 { c.data().input.scroll_y })?;
    l.func_wrap("env", "input_drag_x",   |c: Caller<'_, HostState>| -> f32 { c.data().input.drag_x })?;
    l.func_wrap("env", "input_drag_y",   |c: Caller<'_, HostState>| -> f32 { c.data().input.drag_y })?;

    // ── Input — touch ─────────────────────────────────────────────────────────
    l.func_wrap("env", "input_touch_count", |c: Caller<'_, HostState>| -> i32 { c.data().input.touches.len() as i32 })?;
    l.func_wrap("env", "input_touch_x", |c: Caller<'_, HostState>, idx: i32| -> f32 {
        c.data().input.touches.get(idx as usize).map(|t| t.0).unwrap_or(-1.0)
    })?;
    l.func_wrap("env", "input_touch_y", |c: Caller<'_, HostState>, idx: i32| -> f32 {
        c.data().input.touches.get(idx as usize).map(|t| t.1).unwrap_or(-1.0)
    })?;

    // ── Storage ───────────────────────────────────────────────────────────────
    // All keys are also persisted to ~/.annessaia/storage/ on disk.
    l.func_wrap("env", "storage_set", |mut c: Caller<'_, HostState>, kp:i32,kl:i32,vp:i32,vl:i32| {
        let key = read_str(&mut c, kp, kl);
        let val = read_bytes(&mut c, vp, vl);
        disk_storage_set(&key, &val);
        c.data_mut().storage.insert(key, val);
    })?;
    // Returns bytes written into out_ptr, or -1 if key not found.
    l.func_wrap("env", "storage_get", |mut c: Caller<'_, HostState>, kp:i32,kl:i32,op:i32,om:i32| -> i32 {
        let key = read_str(&mut c, kp, kl);
        let val = c.data().storage.get(&key).cloned()
            .or_else(|| disk_storage_get(&key).map(|v| { c.data_mut().storage.insert(key.clone(), v.clone()); v }));
        match val {
            None => -1,
            Some(v) => write_bytes(&mut c, op, om, &v),
        }
    })?;
    l.func_wrap("env", "storage_has", |mut c: Caller<'_, HostState>, kp:i32,kl:i32| -> i32 {
        let key = read_str(&mut c, kp, kl);
        (c.data().storage.contains_key(&key) || disk_storage_get(&key).is_some()) as i32
    })?;
    l.func_wrap("env", "storage_delete", |mut c: Caller<'_, HostState>, kp:i32,kl:i32| {
        let key = read_str(&mut c, kp, kl);
        disk_storage_delete(&key);
        c.data_mut().storage.remove(&key);
    })?;
    l.func_wrap("env", "storage_clear", |mut c: Caller<'_, HostState>| {
        disk_storage_clear();
        c.data_mut().storage.clear();
    })?;

    // ── File save ─────────────────────────────────────────────────────────────
    // Queues a save dialog; Browser executes it after the tick.
    l.func_wrap("env", "file_save", |mut c: Caller<'_, HostState>, dp:i32,dl:i32,np:i32,nl:i32| {
        let data = read_bytes(&mut c, dp, dl);
        let name = read_str(&mut c, np, nl);
        c.data_mut().pending_save = Some((data, name));
    })?;

    // ── File picker ───────────────────────────────────────────────────────────
    // Blocking, unlike file_save: the guest gets the bytes back from this call,
    // so there is nothing to poll. It stalls the frame for as long as the dialog
    // is open, which is the same thing the toolbar's Browse button already does.
    // Returns the byte length, or -1 if the user cancelled.
    l.func_wrap("env", "file_pick", |mut c: Caller<'_, HostState>, fp:i32, fl:i32| -> i32 {
        let filter = read_str(&mut c, fp, fl);
        let mut dlg = rfd::FileDialog::new();
        if !filter.is_empty() {
            let exts: Vec<&str> = filter.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            if !exts.is_empty() { dlg = dlg.add_filter("file", &exts); }
        }
        let Some(path) = dlg.pick_file() else {
            c.data_mut().picked = None;
            return -1;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            c.data_mut().picked = None;
            return -1;
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let len = bytes.len() as i32;
        c.data_mut().picked = Some((name, bytes));
        len
    })?;
    l.func_wrap("env", "file_pick_data", |mut c: Caller<'_, HostState>, ptr:i32, max:i32| -> i32 {
        let Some((_, bytes)) = c.data().picked.clone() else { return -1 };
        write_bytes(&mut c, ptr, max, &bytes)
    })?;
    l.func_wrap("env", "file_pick_name", |mut c: Caller<'_, HostState>, ptr:i32, max:i32| -> i32 {
        let Some((name, _)) = c.data().picked.clone() else { return -1 };
        write_bytes(&mut c, ptr, max, name.as_bytes())
    })?;

    // ── Clipboard ─────────────────────────────────────────────────────────────
    l.func_wrap("env", "clipboard_write", |mut c: Caller<'_, HostState>, ptr:i32,len:i32| {
        let s = read_str(&mut c, ptr, len);
        if let Ok(mut cb) = Clipboard::new() { let _ = cb.set_text(s); }
    })?;
    // Returns bytes written, -1 if clipboard empty/unavailable.
    l.func_wrap("env", "clipboard_read", |mut c: Caller<'_, HostState>, op:i32,om:i32| -> i32 {
        let text = Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()).unwrap_or_default();
        write_bytes(&mut c, op, om, text.as_bytes())
    })?;

    // ── System ────────────────────────────────────────────────────────────────
    // Open a URL in the system default browser.
    l.func_wrap("env", "open_url", |mut c: Caller<'_, HostState>, ptr:i32,len:i32| {
        let url = read_str(&mut c, ptr, len);
        let _ = std::process::Command::new("open").arg(&url).spawn();
    })?;
    // Print a debug message to stdout.
    l.func_wrap("env", "log_str", |mut c: Caller<'_, HostState>, ptr:i32,len:i32| {
        let s = read_str(&mut c, ptr, len);
        println!("[wasm] {s}");
    })?;

    // ── Navigation ────────────────────────────────────────────────────────────
    l.func_wrap("env", "nav_push", |mut c: Caller<'_, HostState>, ptr:i32,len:i32| {
        let url = read_str(&mut c, ptr, len);
        c.data_mut().nav_request = Some(NavRequest::Push(url));
    })?;
    l.func_wrap("env", "nav_back",    |mut c: Caller<'_, HostState>| { c.data_mut().nav_request = Some(NavRequest::Back); })?;
    l.func_wrap("env", "nav_forward", |mut c: Caller<'_, HostState>| { c.data_mut().nav_request = Some(NavRequest::Forward); })?;

    // ── History ───────────────────────────────────────────────────────────────
    // All-time URL history (every URL ever loaded), read from ~/.annessaia/history.txt.
    l.func_wrap("env", "history_len", |c: Caller<'_, HostState>| -> i32 {
        c.data().nav_history.len() as i32
    })?;
    // Writes URL at index idx into WASM memory; returns bytes written, -1 if out of range.
    l.func_wrap("env", "history_get", |mut c: Caller<'_, HostState>, idx:i32, op:i32, om:i32| -> i32 {
        let url = c.data().nav_history.get(idx as usize).cloned();
        match url {
            None => -1,
            Some(u) => write_bytes(&mut c, op, om, u.as_bytes()),
        }
    })?;

    // ── HTTP — synchronous GET ────────────────────────────────────────────────
    // Blocks the egui frame for the duration of the request. Intended for
    // one-shot loads in init(); use fetch_start/fetch_poll for background work.
    l.func_wrap("env", "fetch_sync", |mut c: Caller<'_, HostState>, up:i32,ul:i32,op:i32,om:i32| -> i32 {
        let url = read_str(&mut c, up, ul);
        let mut body: Vec<u8> = Vec::new();
        let ok = ureq::get(&url).call()
            .ok()
            .and_then(|r| r.into_reader().read_to_end(&mut body).ok())
            .is_some();
        if !ok { return -2; }
        write_bytes(&mut c, op, om, &body)
    })?;

    // ── HTTP — async GET ──────────────────────────────────────────────────────
    l.func_wrap("env", "fetch_start", |mut c: Caller<'_, HostState>, id:i32, up:i32, ul:i32| {
        let url = read_str(&mut c, up, ul);
        let map = Arc::clone(&c.data().fetches);
        map.lock().unwrap().insert(id, FetchResult::Pending);
        std::thread::spawn(move || {
            let mut body: Vec<u8> = Vec::new();
            let result = ureq::get(&url).call()
                .ok()
                .and_then(|r| r.into_reader().read_to_end(&mut body).ok())
                .map(|_| FetchResult::Done(body))
                .unwrap_or(FetchResult::Error);
            map.lock().unwrap().insert(id, result);
        });
    })?;

    // ── HTTP — async POST ─────────────────────────────────────────────────────
    l.func_wrap("env", "fetch_post", |mut c: Caller<'_, HostState>, id:i32, up:i32,ul:i32,bp:i32,bl:i32| {
        let url  = read_str(&mut c, up, ul);
        let body = read_bytes(&mut c, bp, bl);
        let map  = Arc::clone(&c.data().fetches);
        map.lock().unwrap().insert(id, FetchResult::Pending);
        std::thread::spawn(move || {
            let mut resp: Vec<u8> = Vec::new();
            let result = ureq::post(&url).send_bytes(&body)
                .ok()
                .and_then(|r| r.into_reader().read_to_end(&mut resp).ok())
                .map(|_| FetchResult::Done(resp))
                .unwrap_or(FetchResult::Error);
            map.lock().unwrap().insert(id, result);
        });
    })?;

    // ── Local embedding — async, shares the fetch result slot ─────────────────
    // Runs on a spawned thread, same reasoning as fetch_start: the first call
    // ever may need to download the model, which would freeze the UI for
    // however long that takes if done inline on the frame that requested it.
    l.func_wrap("env", "embed_start", |mut c: Caller<'_, HostState>, id: i32, tp: i32, tl: i32| {
        let text = read_str(&mut c, tp, tl);
        let map = Arc::clone(&c.data().fetches);
        map.lock().unwrap().insert(id, FetchResult::Pending);
        std::thread::spawn(move || {
            let result = embed_query_blocking(&text)
                .map(|b64| FetchResult::Done(b64.into_bytes()))
                .unwrap_or(FetchResult::Error);
            map.lock().unwrap().insert(id, result);
        });
    })?;
    // Document-flavored sibling — same async/thread/result-slot shape as
    // embed_start above, differing only in which _blocking function it calls.
    l.func_wrap("env", "embed_start_doc", |mut c: Caller<'_, HostState>, id: i32, tp: i32, tl: i32| {
        let text = read_str(&mut c, tp, tl);
        let map = Arc::clone(&c.data().fetches);
        map.lock().unwrap().insert(id, FetchResult::Pending);
        std::thread::spawn(move || {
            let result = embed_document_blocking(&text)
                .map(|b64| FetchResult::Done(b64.into_bytes()))
                .unwrap_or(FetchResult::Error);
            map.lock().unwrap().insert(id, result);
        });
    })?;

    // ── Sound ─────────────────────────────────────────────────────────────────
    // Synchronous, unlike net/embed: decoding a short in-memory clip and handing
    // it to the audio device is fast — there is no download or model load in
    // this path that would justify a background thread and polling.
    l.func_wrap("env", "sound_play", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
        let bytes = read_bytes(&mut c, ptr, len);
        sound_play(&bytes);
    })?;
    l.func_wrap("env", "sound_play_looped", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
        let bytes = read_bytes(&mut c, ptr, len);
        sound_play_looped(&bytes)
    })?;
    l.func_wrap("env", "sound_stop", |_c: Caller<'_, HostState>, handle: i32| {
        sound_stop(handle);
    })?;
    l.func_wrap("env", "sound_set_volume", |_c: Caller<'_, HostState>, handle: i32, volume: f32| {
        sound_set_volume(handle, volume);
    })?;

    // ── Image ─────────────────────────────────────────────────────────────────
    // Synchronous, same reasoning as sound: decoding a typical PNG/JPEG is fast
    // enough that a poll-across-frames dance would only add complexity. Decoded
    // pixels are kept in HostState by id — Browser::update (the only place with
    // access to an egui::Context) turns a given id into a cached GPU texture the
    // first time it's actually drawn, not on every decode.
    l.func_wrap("env", "image_decode", |mut c: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
        let bytes = read_bytes(&mut c, ptr, len);
        match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = (rgba.width(), rgba.height());
                let d = c.data_mut();
                let id = d.next_image_id;
                d.next_image_id += 1;
                d.images.insert(id, (w, h, rgba.into_raw()));
                id
            }
            Err(e) => {
                eprintln!("[image] decode failed ({} bytes): {e}", bytes.len());
                -1
            }
        }
    })?;
    l.func_wrap("env", "image_width", |c: Caller<'_, HostState>, id: i32| -> i32 {
        c.data().images.get(&id).map(|(w, _, _)| *w as i32).unwrap_or(-1)
    })?;
    l.func_wrap("env", "image_height", |c: Caller<'_, HostState>, id: i32| -> i32 {
        c.data().images.get(&id).map(|(_, h, _)| *h as i32).unwrap_or(-1)
    })?;
    l.func_wrap("env", "gpu_image", |mut c: Caller<'_, HostState>, id: i32, x: f32, y: f32, w: f32, h: f32| {
        c.data_mut().gpu_cmds.push(GpuCmd::Image { id, x, y, w, h });
    })?;
    l.func_wrap("env", "ui_image", |mut c: Caller<'_, HostState>, id: i32, w: f32, h: f32| {
        c.data_mut().widget_cmds.push(WidgetCmd::Image { id, w, h });
    })?;

    // ── Assets (from a .wasmpackage) ──────────────────────────────────────────
    // Already fully in memory by the time any guest code runs (see
    // WasmRuntime::from_raw), so this is a plain synchronous lookup — no
    // polling, no thread, unlike fetch/embed.
    l.func_wrap("env", "asset_load", |mut c: Caller<'_, HostState>, np: i32, nl: i32, op: i32, om: i32| -> i32 {
        let name = read_str(&mut c, np, nl);
        let data = c.data().assets.get(&name).cloned();
        match data {
            None => -1,
            Some(d) => write_bytes(&mut c, op, om, &d),
        }
    })?;

    // ── HTTP — poll async result ──────────────────────────────────────────────
    // Returns -1 while pending, -2 on error, ≥0 = bytes written (result consumed).
    l.func_wrap("env", "fetch_poll", |mut c: Caller<'_, HostState>, id:i32, op:i32, om:i32| -> i32 {
        let result = c.data().fetches.lock().unwrap().get(&id).cloned();
        match result {
            None | Some(FetchResult::Pending) => -1,
            Some(FetchResult::Error) => {
                c.data().fetches.lock().unwrap().remove(&id);
                -2
            }
            Some(FetchResult::Done(data)) => {
                c.data().fetches.lock().unwrap().remove(&id);
                write_bytes(&mut c, op, om, &data)
            }
        }
    })?;

    Ok(l)
}

// ── WASM runtime ──────────────────────────────────────────────────────────────

struct WasmRuntime {
    store: Store<HostState>,
    memory: Option<Memory>,
    widget_render: Option<TypedFunc<(), ()>>,
    pixel_render: Option<TypedFunc<(i32, i32), ()>>,
    framebuffer_ptr: Option<TypedFunc<(), i32>>,
    gpu_render: Option<TypedFunc<(), ()>>,
}

struct PixelFrame { width: usize, height: usize, rgba: Vec<u8> }

struct FrameOutput {
    widgets:      Vec<WidgetCmd>,
    gpu:          Vec<GpuCmd>,
    pixels:       Option<PixelFrame>,
    nav_request:  Option<NavRequest>,
    pending_save: Option<(Vec<u8>, String)>,
    text_states:  HashMap<i32, String>, // snapshot for text edit rendering
}

impl WasmRuntime {
    fn from_path(engine: &Engine, path: &str) -> anyhow::Result<Self> {
        Self::from_raw(engine, &std::fs::read(path)?)
    }
    fn from_bytes(engine: &Engine, bytes: &[u8]) -> anyhow::Result<Self> {
        Self::from_raw(engine, bytes)
    }
    // Reads assets out first (if this is a .wasmpackage, not a bare .wasm) so
    // they're already in HostState before the guest's own init() runs — a
    // guest that loads an asset at startup, the most natural place to do it,
    // must not find an empty asset table just because of load order.
    fn from_raw(engine: &Engine, raw: &[u8]) -> anyhow::Result<Self> {
        let (_hash, wasm_bytes, assets) = resolve_wasm(raw)?;
        Self::from_module(engine, Module::new(engine, &wasm_bytes)?, assets)
    }
    fn from_module(engine: &Engine, module: Module, assets: HashMap<String, Vec<u8>>) -> anyhow::Result<Self> {
        let mut store = Store::new(engine, HostState::new(assets));
        let linker = make_linker(engine)?;
        let instance = linker.instantiate(&mut store, &module)?;
        if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, "init") {
            init.call(&mut store, ())?;
        }
        let memory = instance.get_memory(&mut store, "memory");
        let widget_render = instance.get_typed_func::<(), ()>(&mut store, "render").ok();
        let pixel_render  = instance.get_typed_func::<(i32,i32), ()>(&mut store, "render_pixels").ok();
        let framebuffer_ptr = instance.get_typed_func::<(), i32>(&mut store, "framebuffer_ptr").ok();
        let gpu_render    = instance.get_typed_func::<(), ()>(&mut store, "render_gpu").ok();
        if widget_render.is_none() && pixel_render.is_none() && gpu_render.is_none() {
            anyhow::bail!("WASM must export render(), render_gpu(), or render_pixels(w,h)");
        }
        Ok(Self { store, memory, widget_render, pixel_render, framebuffer_ptr, gpu_render })
    }

    fn set_input(&mut self, snap: InputSnapshot) { self.store.data_mut().input = snap; }

    fn set_nav_context(&mut self, history: &[String], pos: usize) {
        let d = self.store.data_mut();
        d.nav_history = history.to_vec();
        d.nav_pos = pos;
    }

    fn tick(&mut self, canvas_w: f32, canvas_h: f32) -> anyhow::Result<FrameOutput> {
        let d = self.store.data_mut();
        d.canvas_w = canvas_w;
        d.canvas_h = canvas_h;
        let prev = std::mem::take(&mut d.clicked_curr);
        d.clicked_prev = prev;
        d.btn_seq.clear();
        d.widget_cmds.clear();
        d.gpu_cmds.clear();
        d.nav_request = None;
        d.pending_save = None;

        if let Some(f) = self.gpu_render.clone() { f.call(&mut self.store, ())?; }
        if let Some(f) = self.widget_render.clone() { f.call(&mut self.store, ())?; }

        let pixels = if let (Some(pr), Some(fp)) = (self.pixel_render.clone(), self.framebuffer_ptr.clone()) {
            pr.call(&mut self.store, (canvas_w as i32, canvas_h as i32))?;
            let ptr = fp.call(&mut self.store, ())? as usize;
            if let Some(mem) = self.memory.clone() {
                let (w, h) = (canvas_w as usize, canvas_h as usize);
                let raw = mem.data(&self.store);
                if ptr + w*h*4 <= raw.len() {
                    Some(PixelFrame { width: w, height: h, rgba: raw[ptr..ptr+w*h*4].to_vec() })
                } else { None }
            } else { None }
        } else { None };

        let d = self.store.data_mut();
        let widgets      = std::mem::take(&mut d.widget_cmds);
        let gpu          = std::mem::take(&mut d.gpu_cmds);
        let nav_request  = d.nav_request.take();
        let pending_save = d.pending_save.take();
        let text_states  = d.text_states.clone();
        Ok(FrameOutput { widgets, gpu, pixels, nav_request, pending_save, text_states })
    }

    fn apply_widget_updates(&mut self, upd: WidgetUpdates) {
        let d = self.store.data_mut();
        for label       in upd.clicks     { d.clicked_curr.insert(label, true); }
        for (id, v)     in upd.checkboxes { d.checkbox_states.insert(id, v); }
        for (id, v)     in upd.sliders    { d.slider_states.insert(id, v); }
        for (id, text)  in upd.texts      { d.text_states.insert(id, text); }
    }

    // Cloned rather than borrowed: the caller (Browser::update) only needs
    // this once per id, the first frame that id is ever drawn — every frame
    // after that hits the already-populated texture cache instead.
    fn image_data(&self, id: i32) -> Option<(u32, u32, Vec<u8>)> {
        self.store.data().images.get(&id).cloned()
    }
}

// ── Browser ───────────────────────────────────────────────────────────────────

// ── AI setup preference ──────────────────────────────────────────────────────
//
// Whether the on-device embedding model is allowed to load at all. Asked once,
// on first launch, by the setup screen in Browser::update — not a compile-time
// choice, so declining here still leaves fastembed in the binary (`cargo build`
// behaves the same as always) but guarantees the ~1.3GB download it would
// trigger on first use never happens. embed_query_blocking / embed_document_
// blocking both check this before touching EMBEDDER at all.
//
// Persisted as a single "1"/"0" byte rather than folded into config/storage —
// it has to be readable before HostState (and its disk-backed storage) exists,
// since the answer decides whether the very first frame is the app or the
// setup prompt.
#[cfg(feature = "ai")]
fn ai_pref_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::Path::new(&home).join(".annessaia");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("ai_pref"))
}

#[cfg(feature = "ai")]
fn load_ai_pref() -> Option<bool> {
    match std::fs::read_to_string(ai_pref_path()?).ok()?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _   => None,
    }
}

#[cfg(feature = "ai")]
fn save_ai_pref(enabled: bool) {
    if let Some(p) = ai_pref_path() {
        let _ = std::fs::write(p, if enabled { "1" } else { "0" });
    }
}

// -1 unset (never read before setup answers it), 0 off, 1 on.
#[cfg(feature = "ai")]
static AI_PREF: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

struct Browser {
    engine: Engine,
    runtime: Option<WasmRuntime>,
    loading: Option<Receiver<anyhow::Result<WasmRuntime>>>,
    path_input: String,
    error: Option<String>,
    pixel_texture: Option<egui::TextureHandle>,
    // Decoded-image ids (from HostState) -> uploaded GPU textures. Populated
    // lazily the first time a given id is actually drawn; cleared on
    // navigation, same reasoning as pixel_texture — ids are only meaningful
    // within the app instance that produced them, a new app can reuse id 0.
    image_textures: HashMap<i32, egui::TextureHandle>,
    // Navigation history (persisted to ~/.annessaia/history.txt)
    nav_history: Vec<String>,
    nav_pos: usize,  // index into nav_history of current page (-1 = nothing loaded)
    // Home URL waiting behind the "enable AI search?" prompt — Some only while
    // that prompt is showing (first launch, before ai_pref has been answered).
    // Doesn't exist on a build without the "ai" feature: there's no model to
    // ask about, so nothing to gate the first frame on.
    #[cfg(feature = "ai")]
    pending_ai_setup: Option<String>,
    // Switch position on the setup screen, flipped freely before Continue is
    // pressed — separate from AI_PREF/ai_pref because nothing is decided (or
    // saved) until then.
    #[cfg(feature = "ai")]
    ai_setup_preview: bool,
}

// Where the runtime points on launch and when Home is pressed. The apps are
// hosted on the bootstrap Worker, so a fresh install has somewhere to go without
// cloning the repo or running a server first.
const HOME: &str = "https://bootstrap.annessaia.workers.dev/search.wasm";

// Anyone running their own node wants its registry, not the public one — theirs
// has whatever they have published locally and accepts submissions, which the
// hosted index does not.
//
//   ANNESSAIA_HOME    an exact URL, wins over everything
//   ANNESSAIA_SERVER  a node's base URL; its /search.wasm becomes home
fn home_url() -> String {
    if let Ok(h) = std::env::var("ANNESSAIA_HOME") {
        let h = h.trim().to_string();
        if !h.is_empty() { return h; }
    }
    if let Ok(s) = std::env::var("ANNESSAIA_SERVER") {
        let s = s.trim().trim_end_matches('/').to_string();
        if !s.is_empty() { return format!("{s}/search.wasm"); }
    }
    HOME.to_string()
}

impl Browser {
    fn new() -> Self {
        // A URL on the command line wins, so `annessaia foo.wasm` still works.
        let start = std::env::args().nth(1).unwrap_or_else(home_url);

        #[cfg(feature = "ai")]
        let ai_answered = match load_ai_pref() {
            Some(enabled) => { AI_PREF.store(enabled as i8, std::sync::atomic::Ordering::Relaxed); true }
            None => false,
        };
        #[cfg(not(feature = "ai"))]
        let ai_answered = true;

        let mut b = Self {
            engine: Engine::default(),
            runtime: None,
            loading: None,
            path_input: String::new(),
            error: None,
            pixel_texture: None,
            image_textures: HashMap::new(),
            nav_history: load_history(),
            nav_pos: usize::MAX,
            #[cfg(feature = "ai")]
            pending_ai_setup: if ai_answered { None } else { Some(start.clone()) },
            #[cfg(feature = "ai")]
            ai_setup_preview: true,
        };
        if ai_answered {
            b.load_url(start, true);
        }
        b
    }

    // Records the first-run "enable AI search?" answer and, having been
    // waiting on exactly that, finally loads the real start page.
    #[cfg(feature = "ai")]
    fn choose_ai(&mut self, enabled: bool) {
        save_ai_pref(enabled);
        AI_PREF.store(enabled as i8, std::sync::atomic::Ordering::Relaxed);
        if let Some(start) = self.pending_ai_setup.take() {
            self.load_url(start, true);
        }
    }

    fn load_url(&mut self, url: String, push: bool) {
        self.runtime = None;
        self.loading = None;
        self.error = None;
        self.pixel_texture = None;
        self.image_textures.clear();
        self.path_input = url.clone();

        if push {
            // Truncate forward history then append
            if self.nav_pos != usize::MAX {
                self.nav_history.truncate(self.nav_pos + 1);
            }
            self.nav_history.push(url.clone());
            self.nav_pos = self.nav_history.len() - 1;
            append_history(&url);
        }

        if url.starts_with("http://") || url.starts_with("https://") {
            let (tx, rx) = mpsc::channel();
            let engine = self.engine.clone();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<WasmRuntime> {
                    // A .wasmh file carries its own content hash as a
                    // trailer, not in the URL — the only way to learn it
                    // without downloading the whole (potentially large) body
                    // is an HTTP Range request for just those last bytes. If
                    // that hash is already on disk, the real GET below is
                    // skipped entirely: the trailer already proved the
                    // content hasn't changed. A server that doesn't honor
                    // Range, or a cache miss, both fall straight through to
                    // the normal full fetch — never an error, just no
                    // shortcut this time.
                    if let Some(hash) = peek_hash_via_range(&url) {
                        if let Some((wasm, assets)) = load_from_cache(&hash) {
                            println!("[wasmh] {hash} — cache hit, skipping full fetch");
                            let module = Module::new(&engine, &wasm)?;
                            return WasmRuntime::from_module(&engine, module, assets);
                        }
                    }

                    let mut bytes = Vec::new();
                    ureq::get(&url).call()
                        .map_err(|e| anyhow::anyhow!("fetch: {e}"))?
                        .into_reader().read_to_end(&mut bytes)
                        .map_err(|e| anyhow::anyhow!("read: {e}"))?;
                    WasmRuntime::from_bytes(&engine, &bytes)
                })();
                let _ = tx.send(result);
            });
            self.loading = Some(rx);
        } else {
            match WasmRuntime::from_path(&self.engine, &url) {
                Ok(rt)  => { self.runtime = Some(rt); }
                Err(e)  => { self.error = Some(format!("{e:#}")); }
            }
        }
    }

    fn load(&mut self) {
        let url = self.path_input.trim().to_string();
        self.load_url(url, true);
    }

    fn nav_back(&mut self) {
        if self.nav_pos > 0 && self.nav_pos != usize::MAX {
            self.nav_pos -= 1;
            let url = self.nav_history[self.nav_pos].clone();
            self.load_url(url, false);
        }
    }

    fn nav_forward(&mut self) {
        if self.nav_pos != usize::MAX && self.nav_pos + 1 < self.nav_history.len() {
            self.nav_pos += 1;
            let url = self.nav_history[self.nav_pos].clone();
            self.load_url(url, false);
        }
    }
}

impl eframe::App for Browser {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── First-run "enable AI search?" prompt ──────────────────────────────
        // Blocks everything else until answered — the home page hasn't loaded
        // yet (see Browser::new), so there's nothing behind this to show.
        #[cfg(feature = "ai")]
        if self.pending_ai_setup.is_some() {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(Color32::from_rgb(10,14,26)))
                .show(ctx, |ui| {
                    ui.add_space(160.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Welcome to annessaia").size(30.0).color(Color32::from_rgb(56,189,248)).strong());
                        ui.add_space(14.0);
                        ui.label(RichText::new("Enable AI-powered semantic search?").size(16.0).color(Color32::WHITE));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Lets search understand meaning, not just keywords, using a model \
                                 that runs entirely on this machine. It downloads (~1.3GB) the \
                                 first time it's actually used — not now.\n\
                                 You can flip this anytime later from the toggle in the nav bar."
                            ).size(12.0).color(Color32::from_rgb(148,163,184)),
                        );
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 140.0).max(0.0) / 2.0);
                            ui.label(RichText::new("Keyword-only").size(13.0).color(Color32::from_rgb(148,163,184)));
                            toggle_switch(ui, &mut self.ai_setup_preview);
                            ui.label(RichText::new("AI search").size(13.0).color(Color32::WHITE));
                        });
                        ui.add_space(20.0);
                        if ui.add_sized([120.0, 34.0], egui::Button::new("Continue")).clicked() {
                            self.choose_ai(self.ai_setup_preview);
                        }
                    });
                });
            ctx.request_repaint();
            return;
        }

        let can_back    = self.nav_pos > 0 && self.nav_pos != usize::MAX;
        let can_forward = self.nav_pos != usize::MAX && self.nav_pos + 1 < self.nav_history.len();

        // ── Nav bar ────────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("nav")
            .frame(egui::Frame::none().fill(Color32::from_rgb(15,23,42)).inner_margin(8.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("annessaia").color(Color32::from_rgb(56,189,248)).strong());
                    ui.separator();

                    // ← Back
                    if ui.add_enabled(can_back, egui::Button::new("←")).clicked() {
                        self.nav_back();
                    }
                    // → Forward
                    if ui.add_enabled(can_forward, egui::Button::new("→")).clicked() {
                        self.nav_forward();
                    }
                    // ⌂ Home
                    if ui.button("⌂").on_hover_text("Home — the app registry").clicked() {
                        self.load_url(home_url(), true);
                    }

                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .hint_text("path/to/app.wasm  or  http://…")
                            .desired_width(f32::INFINITY),
                    );
                    let go = ui.button("Load").clicked()
                        || (r.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)));
                    if ui.button("Browse…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("WASM", &["wasm", "wasmpackage"]).pick_file() {
                            self.path_input = p.display().to_string();
                            self.load();
                        }
                    }
                    if go { self.load(); }

                    // AI toggle — always here, not just at first launch, so
                    // changing your mind never requires finding and deleting
                    // ~/.annessaia/ai_pref by hand.
                    #[cfg(feature = "ai")]
                    {
                        let ai_on = AI_PREF.load(std::sync::atomic::Ordering::Relaxed) != 0;
                        let clicked = if ai_on {
                            ui.add(egui::Button::new(RichText::new(" AI search: ON ").color(Color32::WHITE))
                                .fill(Color32::from_rgb(20,83,45))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(74,222,128))))
                                .on_hover_text("Meaning-based search is on. Click to switch to keyword-only.")
                                .clicked()
                        } else {
                            ui.add(egui::Button::new(RichText::new(" AI search: OFF ").color(Color32::from_rgb(148,163,184))))
                                .on_hover_text("Keyword-only search. Click to enable meaning-based (AI) search.")
                                .clicked()
                        };
                        if clicked { self.choose_ai(!ai_on); }
                    }
                });
            });

        // ── Poll background loader ─────────────────────────────────────────────
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(Ok(rt))  => { self.loading = None; self.runtime = Some(rt); }
                Ok(Err(e))  => { self.loading = None; self.error = Some(format!("{e:#}")); }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(_) => { self.loading = None; }
            }
        }

        // ── Central panel ──────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::from_rgb(10,14,26)))
            .show(ctx, |ui| {
                if self.loading.is_some() {
                    ui.add_space(140.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Fetching…").size(15.0).color(Color32::from_rgb(56,189,248)));
                    });
                    return;
                }
                if let Some(err) = &self.error.clone() {
                    ui.add_space(20.0);
                    ui.label(RichText::new(format!("Error: {err}")).color(Color32::from_rgb(248,113,113)));
                    return;
                }
                if self.runtime.is_none() {
                    ui.add_space(140.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("annessaia").size(36.0).color(Color32::from_rgb(56,189,248)).strong());
                        ui.add_space(10.0);
                        ui.label(RichText::new("Load a .wasm file to run it").size(14.0).color(Color32::from_rgb(71,85,105)));
                    });
                    return;
                }

                let panel_rect = ui.max_rect();
                let origin = panel_rect.min;
                let size = panel_rect.size();

                // ── Input snapshot ─────────────────────────────────────────────
                let snap = ctx.input(|i| {
                    let mouse_pos = i.pointer.hover_pos().map(|p| p - origin).unwrap_or_default();
                    let touches: Vec<(f32,f32)> = i.events.iter().filter_map(|e| {
                        if let egui::Event::Touch { phase, pos, .. } = e {
                            if matches!(phase, egui::TouchPhase::Start | egui::TouchPhase::Move) {
                                return Some((pos.x - origin.x, pos.y - origin.y));
                            }
                        }
                        None
                    }).collect();
                    // Keyboard: held keys come from the input state, edges and
                    // typed characters from this frame's event queue.
                    let keys_down: HashSet<i32> =
                        i.keys_down.iter().map(|k| key_code(*k)).filter(|c| *c >= 0).collect();
                    let mut keys_pressed = HashSet::new();
                    let mut keys_released = HashSet::new();
                    let mut typed = String::new();
                    for e in &i.events {
                        match e {
                            egui::Event::Key { key, pressed, repeat, .. } => {
                                let c = key_code(*key);
                                if c < 0 { continue; }
                                // Skip auto-repeat so `pressed` means one physical press.
                                if *pressed { if !*repeat { keys_pressed.insert(c); } }
                                else { keys_released.insert(c); }
                            }
                            egui::Event::Text(t) => typed.push_str(t),
                            _ => {}
                        }
                    }
                    let m = i.modifiers;
                    let modifiers = (m.shift as i32)
                        | ((m.ctrl as i32) << 1)
                        | ((m.alt as i32) << 2)
                        | ((m.command as i32) << 3);

                    InputSnapshot {
                        mouse_x: mouse_pos.x, mouse_y: mouse_pos.y,
                        mouse_left_down:    i.pointer.button_down(egui::PointerButton::Primary),
                        mouse_right_down:   i.pointer.button_down(egui::PointerButton::Secondary),
                        mouse_middle_down:  i.pointer.button_down(egui::PointerButton::Middle),
                        mouse_left_clicked: i.pointer.primary_clicked(),
                        mouse_right_clicked:i.pointer.secondary_clicked(),
                        scroll_x: i.raw_scroll_delta.x,
                        scroll_y: i.raw_scroll_delta.y,
                        drag_x: i.pointer.delta().x,
                        drag_y: i.pointer.delta().y,
                        touches,
                        keys_down, keys_pressed, keys_released, modifiers, typed,
                    }
                });

                let rt = self.runtime.as_mut().unwrap();
                rt.set_input(snap);
                rt.set_nav_context(&self.nav_history, self.nav_pos);

                // ── Tick ──────────────────────────────────────────────────────
                let output = match rt.tick(size.x, size.y) {
                    Ok(o)  => o,
                    Err(e) => { self.error = Some(format!("{e:#}")); self.runtime = None; return; }
                };

                // ── Handle post-tick requests ──────────────────────────────────

                // File save dialog
                if let Some((data, name)) = output.pending_save {
                    let mut dlg = rfd::FileDialog::new();
                    if !name.is_empty() {
                        let p = std::path::Path::new(&name);
                        if let Some(fname) = p.file_name() { dlg = dlg.set_file_name(fname.to_string_lossy().as_ref()); }
                        if let Some(ext) = p.extension() { dlg = dlg.add_filter("file", &[ext.to_string_lossy().as_ref()]); }
                    }
                    if let Some(dest) = dlg.save_file() {
                        let _ = std::fs::write(dest, data);
                    }
                }

                // Navigation request. Each of these swaps the runtime out, so the
                // widget pass below must not touch it afterwards.
                let mut navigated = false;
                if let Some(req) = output.nav_request {
                    navigated = true;
                    match req {
                        NavRequest::Push(url) => self.load_url(url, true),
                        NavRequest::Back      => self.nav_back(),
                        NavRequest::Forward   => self.nav_forward(),
                    }
                }

                // ── Render layers ──────────────────────────────────────────────

                // Ensure every image id this frame's commands reference has a
                // cached texture before paint_gpu/render_widgets look one up.
                // Skipped after a navigation: those ids belong to whatever app
                // just produced this frame's commands, not whatever
                // self.runtime holds now (see the same `navigated` reasoning
                // just above, for apply_widget_updates).
                if !navigated {
                    if let Some(rt) = self.runtime.as_ref() {
                        let mut needed: Vec<i32> = output.gpu.iter()
                            .filter_map(|c| if let GpuCmd::Image { id, .. } = c { Some(*id) } else { None })
                            .chain(output.widgets.iter()
                                .filter_map(|c| if let WidgetCmd::Image { id, .. } = c { Some(*id) } else { None }))
                            .collect();
                        needed.sort_unstable();
                        needed.dedup();
                        for id in needed {
                            if !self.image_textures.contains_key(&id) {
                                if let Some((w, h, rgba)) = rt.image_data(id) {
                                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                                    let tex = ctx.load_texture(format!("img{id}"), img, egui::TextureOptions::LINEAR);
                                    self.image_textures.insert(id, tex);
                                }
                            }
                        }
                    }
                }

                if !output.gpu.is_empty() {
                    let painter = ui.painter_at(panel_rect);
                    for cmd in &output.gpu { paint_gpu(&painter, origin, cmd, &self.image_textures); }
                }

                if let Some(frame) = output.pixels {
                    let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
                    let tex = ctx.load_texture("framebuffer", image, egui::TextureOptions::LINEAR);
                    self.pixel_texture = Some(tex);
                }
                if let Some(tex) = &self.pixel_texture {
                    ui.painter_at(panel_rect).image(
                        tex.id(), panel_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0,0.0), egui::pos2(1.0,1.0)),
                        Color32::WHITE,
                    );
                }

                if !output.widgets.is_empty() {
                    egui::Frame::none().inner_margin(egui::Margin::symmetric(40.0, 32.0)).show(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let mut text_states = output.text_states;
                            let mut upd = WidgetUpdates::default();
                            render_widgets(ui, &output.widgets, &mut text_states, &mut upd, &self.image_textures);
                            // These updates belong to the app that produced this frame.
                            // If it navigated away, that app is gone and the runtime now
                            // holds a different one — feeding it stale widget ids would
                            // be wrong even where it isn't outright missing.
                            if !navigated {
                                if let Some(rt) = self.runtime.as_mut() {
                                    rt.apply_widget_updates(upd);
                                }
                            }
                        });
                    });
                }
            });

        ctx.request_repaint();
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 700.0])
            .with_title("annessaia"),
        ..Default::default()
    };
    eframe::run_native("annessaia", options, Box::new(|_cc| Ok(Box::new(Browser::new()))))
}
