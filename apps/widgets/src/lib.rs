// Widget Gallery — a live reference for every widget the SDK offers. Each entry
// shows the widget running, plus the exact call that produced it, so this
// doubles as documentation you can copy from rather than just a demo.

use annessaia_sdk::prelude::*;
use core::sync::atomic::{AtomicI32, Ordering::Relaxed};

static COUNTER: AtomicI32 = AtomicI32::new(0);
// The handle sound::play_looped returned for the currently-running background
// loop, or -1 if nothing is playing — the same sentinel the SDK itself uses
// for "nothing to stop", so a stale -1 is always safe to pass to stop/set_volume.
static LOOP_HANDLE: AtomicI32 = AtomicI32::new(-1);

const CODE_BG: Color = Color::rgb(15, 30, 15);

// ── Self-reporting an additional, indexed vector ─────────────────────────────
//
// This gallery has far more searchable substance than its own three-line
// registry listing (name/tags/desc) captures — every widget's name and
// description below, including the whole Sound section. A search for
// "sound" or "playback" has nothing to match against otherwise, even though
// there's a dedicated section about it.
//
// Only this app itself, while running, has that content in hand — a human
// submitting it by URL through the search app's submit form has no way to
// see or paste it. So this self-reports: compute an additional embedding
// from richer text and send it alongside the app's own name/tags/desc on
// launch, same as any other submission. Only the vector ever travels — the
// raw text below never leaves this machine (see the SDK's embed::start_doc).
//
// Self-identity must match this app's actual published listing exactly
// (confirmed live via GET /api/apps), or a mismatch here turns what should
// be an unnoticed backfill into a full content overwrite — see same_content
// in annessaia-server. No build.rs needed, unlike apps/search's per-node
// SERVER: this app's identity and hosted URL are fixed by design, not
// something that varies per build.
const SELF_NAME:   &str = "Widget Gallery";
const SELF_DESC:   &str = "Every widget the SDK offers: buttons, sliders, cards, rows and columns.";
const SELF_AUTHOR: &str = "annessaia";
const SELF_TAGS:   &str = "demo, reference";
const SELF_URL:    &str = "https://bootstrap.annessaia.workers.dev/widgets.wasm";
const SERVER:      &str = "https://bootstrap.annessaia.workers.dev";

// Shown under the description in search results as a short, Google-style
// preview — one chunk per widget, so a query like "how to use sliders" shows
// the slider chunk specifically rather than a generic one-line summary of
// the whole app. The registry (or Worker) picks whichever chunk best matches
// the query's words at search time; chunk 0 is the fallback shown when
// there's no query to match against (e.g. plain browsing). Each chunk is
// capped server-side (~200 chars, ~40 chunks max) regardless of what's sent.
const DOC_CHUNKS: &[&str] = &[
    "Widget Gallery — every widget the SDK offers, with a live example and copyable code for each.",
    "heading — a large, bold title. heading(\"Section title\");",
    "label — the default body text style. label(\"Regular body text.\");",
    "small — dimmer, smaller captions and fine print. small(\"A quieter caption.\");",
    "colored — a label in an arbitrary color. colored(text, color);",
    "text — size and color set explicitly, not tied to a fixed style. text(s, size, color);",
    "code — monospace, for snippets. code(\"let x = 1 + 1;\");",
    "badge — a small colored pill, usually next to a label. badge(\"NEW\", color);",
    "button — returns true on the frame it was clicked.",
    "button_success / button_danger / button_ghost — pre-styled variants for common intents.",
    "button_styled — full control over foreground, background, and border color.",
    "checkbox — toggles and returns its new state.",
    "slider — drag to change; returns the current value every frame.",
    "text_field — a single-line editable field; returns its current text.",
    "progress / progress_bar — a fill bar, with or without a label.",
    "row — lays out whatever the closure draws left to right instead of stacked.",
    "columns2 / columns3 / columns4 — side-by-side panes, each with its own closure.",
    "space — a fixed gap, in logical pixels.",
    "separator — a thin horizontal rule.",
    "card — a bordered panel with default styling.",
    "card_color — same as card, with a custom background — used for alerts, warnings, highlights.",
    "sound::play — fire-and-forget playback, several can overlap. Decoding happens on the host.",
    "sound::play_looped / sound::stop / sound::set_volume — loops until stopped; the handle also adjusts volume.",
];

// Everything indexable beyond name/tags/desc — every widget's name and
// description, duplicating the literals already passed to entry() below.
// Kept in sync by hand; there are few enough widgets that this is simpler
// than a runtime accumulator over what's ultimately static content anyway.
const DOC_TEXT: &str =
    "Text widgets: heading, a large bold title. label, the default body \
     text style. small, dimmer smaller captions and fine print. colored, \
     label in an arbitrary color. text, size and color set explicitly. \
     code, monospace for snippets. badge, a small colored pill. \
     Buttons: button, returns true on the frame it was clicked. \
     button_success, button_danger, button_ghost, pre-styled variants for \
     common intents. button_styled, full control over foreground, \
     background, and border color. \
     Input: checkbox, toggles and returns its new state. slider, drag to \
     change, returns the current value. text_field, a single-line editable \
     field. progress, progress_bar, a fill bar with or without a label. \
     Layout: row, lays out side by side. columns2, columns3, columns4, \
     side-by-side panes. space, a fixed gap. separator, a thin horizontal \
     rule. \
     Containers: card, a bordered panel. card_color, the same with a \
     custom background. \
     Sound: sound::play, fire-and-forget one-shot playback, several can \
     overlap. sound::play_looped, sound::stop, sound::set_volume, loop a \
     clip until stopped and adjust its volume.";

const SELF_EMBED_ID:  i32 = 9001;
const SELF_SUBMIT_ID: i32 = 9002;
// 0=idle 1=embedding 2=submitting 3=done-or-gave-up. Fixed IDs are safe here:
// this app does no other embed/net calls, so there is nothing for 9001/9002
// to collide with.
static SELF_STATE: AtomicI32 = AtomicI32::new(0);

#[no_mangle]
pub extern "C" fn init() {
    SELF_STATE.store(1, Relaxed);
    embed::start_doc(SELF_EMBED_ID, DOC_TEXT);
}

// Advances the self-report state machine one step per frame. Called from the
// top of render() — a failed or unavailable embedder (no local model) just
// leaves this app's doc_vec unset, same as any app that hasn't opted in;
// it's additional signal, not something the gallery depends on to function.
fn poll_self_report() {
    match SELF_STATE.load(Relaxed) {
        1 => match embed::poll_result_str(SELF_EMBED_ID) {
            PollStr::Pending => {}
            PollStr::Failed => SELF_STATE.store(3, Relaxed),
            PollStr::Done(doc_vec) => {
                let doc_snippet = DOC_CHUNKS.join("\u{1e}");
                let body = format!("{SELF_NAME}\t{SELF_DESC}\t{SELF_URL}\t{SELF_AUTHOR}\t{SELF_TAGS}\t{doc_snippet}\t{doc_vec}");
                net::post(SELF_SUBMIT_ID, &format!("{SERVER}/api/submit"), &body);
                SELF_STATE.store(2, Relaxed);
            }
        },
        2 => {
            if net::poll(SELF_SUBMIT_ID).is_some() {
                SELF_STATE.store(3, Relaxed);
            }
        }
        _ => {}
    }
}

// A short sine-wave beep, synthesized on the spot rather than bundled as a
// file — this gallery has nothing to include_bytes! from, and generating one
// is a handful of samples either way. A real app would more likely do
// `static CLICK: &[u8] = include_bytes!("click.wav");`, which is what the code
// snippet below actually shows, since that's the realistic pattern to copy.
fn beep_wav(hz: f32, secs: f32) -> Vec<u8> {
    let sr = 44100u32;
    let n = (sr as f32 * secs) as u32;
    let mut samples = Vec::with_capacity(n as usize * 2);
    for i in 0..n {
        let t = i as f32 / sr as f32;
        // Fade out over the last third so it clicks off cleanly, not abruptly.
        let fade = (1.0 - (t / secs - 0.66).max(0.0) / 0.34).min(1.0);
        let v = (t * hz * core::f32::consts::TAU).sin() * 0.25 * fade * i16::MAX as f32;
        samples.extend_from_slice(&(v as i16).to_le_bytes());
    }
    let data_len = samples.len() as u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());        // PCM
    wav.extend_from_slice(&1u16.to_le_bytes());        // mono
    wav.extend_from_slice(&sr.to_le_bytes());
    wav.extend_from_slice(&(sr * 2).to_le_bytes());    // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes());        // block align
    wav.extend_from_slice(&16u16.to_le_bytes());       // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&samples);
    wav
}

#[no_mangle]
pub extern "C" fn render() {
    poll_self_report();

    row(|| {
        text("annessaia", 28.0, Color::CYAN);
        space(8.0);
        badge("widgets", Color::rgb(37, 99, 235));
        space(4.0);
        badge("reference", Color::rgb(71, 85, 105));
    });
    label("Every widget the SDK offers, live, with the code that made it.");
    separator();
    space(8.0);

    section("Text", || {
        entry("heading", "A large, bold title.", r#"heading("Section title");"#, || {
            heading("Section title");
        });
        entry("label", "The default body text style.", r#"label("Regular body text.");"#, || {
            label("Regular body text.");
        });
        entry("small", "Dimmer, smaller — captions and fine print.", r#"small("A quieter caption.");"#, || {
            small("A quieter caption.");
        });
        entry("colored", "Label in an arbitrary color.", r#"colored("Careful — this can't be undone.", Color::rgb(251, 146, 60));"#, || {
            colored("Careful — this can't be undone.", Color::rgb(251, 146, 60));
        });
        entry("text", "Size and color set explicitly, not tied to a fixed style.", r#"text("Custom size", 20.0, Color::rgb(167, 139, 250));"#, || {
            text("Custom size", 20.0, Color::rgb(167, 139, 250));
        });
        entry("code", "Monospace, for snippets — this whole gallery is built from it.", r#"code("let x = 1 + 1;");"#, || {
            code("let x = 1 + 1;");
        });
        entry("badge", "A small colored pill, usually next to a label.", r#"badge("NEW", Color::rgb(74, 222, 128));"#, || {
            badge("NEW", Color::rgb(74, 222, 128));
        });
    });

    section("Buttons", || {
        entry("button", "Returns true on the frame it was clicked.",
            "if button(\"  Click me  \") {\n    // runs once, the frame of the click\n}", || {
            let _ = button("  Click me  ");
        });
        entry("button_success / button_danger / button_ghost", "Pre-styled variants for common intents.",
            "button_success(\"  Confirm  \");\nbutton_danger(\"  Delete  \");\nbutton_ghost(\"  Cancel  \");", || {
            row(|| {
                let _ = button_success("  Confirm  ");
                let _ = button_danger("  Delete  ");
                let _ = button_ghost("  Cancel  ");
            });
        });
        entry("button_styled", "Full control over foreground, background, and border color.",
            "button_styled(\"  Custom  \", Color::WHITE, Color::rgb(124, 58, 237), Color::rgb(167, 139, 250));", || {
            let _ = button_styled("  Custom  ", Color::WHITE, Color::rgb(124, 58, 237), Color::rgb(167, 139, 250));
        });
    });

    section("Input", || {
        entry("checkbox", "Toggles and returns its new state.", r#"let on = checkbox(0, "Enable notifications");"#, || {
            let _ = checkbox(100, "Enable notifications");
        });
        entry("slider", "Drag to change; returns the current value every frame.",
            r#"let v = slider(1, "Volume", 0.0, 100.0, 80.0);"#, || {
            let _ = slider(101, "Volume", 0.0, 100.0, 80.0);
        });
        entry("text_field", "A single-line editable field; returns its current text.",
            r#"let name = text_field(2, "Type here…");"#, || {
            let _ = text_field(102, "Type here…");
        });
        entry("progress / progress_bar", "A fill bar, with or without a label.",
            "progress(0.6);\nprogress_bar(0.6, \"3 / 5\");", || {
            progress(0.6);
            space(4.0);
            progress_bar(0.6, "3 / 5");
        });
    });

    section("Layout", || {
        entry("row", "Lays out whatever the closure draws left to right instead of stacked.",
            "row(|| {\n    label(\"left\");\n    space(8.0);\n    label(\"right\");\n});", || {
            row(|| {
                label("left");
                space(8.0);
                label("right");
            });
        });
        entry("columns2 / columns3 / columns4", "Side-by-side panes, each with its own closure.",
            "columns2(\n    || label(\"first column\"),\n    || label(\"second column\"),\n);", || {
            columns2(
                || label("first column"),
                || label("second column"),
            );
        });
        entry("space", "A fixed gap, in logical pixels.", "space(16.0);", || {
            label("above");
            space(16.0);
            label("below");
        });
        entry("separator", "A thin horizontal rule.", "separator();", || {
            label("above");
            separator();
            label("below");
        });
    });

    section("Containers", || {
        entry("card", "A bordered panel with default styling — the box every entry on this page is drawn in.",
            "card(|| {\n    label(\"Content inside a card.\");\n});", || {
            card(|| {
                label("Content inside a card.");
            });
        });
        entry("card_color", "Same as card, with a custom background — used for alerts, warnings, highlights.",
            "card_color(Color::rgb(20, 83, 45), || {\n    label(\"A green-tinted card.\");\n});", || {
            card_color(Color::rgb(20, 83, 45), || {
                label("A green-tinted card.");
            });
        });
    });

    section("Sound", || {
        entry("sound::play", "Fire-and-forget playback — several can overlap. Decoding (WAV/MP3/OGG/FLAC) happens on the host.",
            "static CLICK: &[u8] = include_bytes!(\"click.wav\");\n\nif button(\" Play \") {\n    sound::play(CLICK);\n}", || {
            if button(" ▶ Play beep ") {
                sound::play(&beep_wav(880.0, 0.15));
            }
        });
        entry("sound::play_looped / stop / set_volume", "Loops until stopped; the returned handle is also how its volume gets adjusted.",
            "let handle = sound::play_looped(MUSIC);\nsound::set_volume(handle, 0.4);\n// ...later:\nsound::stop(handle);", || {
            let playing = LOOP_HANDLE.load(Relaxed) >= 0;
            row(|| {
                if playing {
                    if button_danger(" ■ Stop loop ") {
                        sound::stop(LOOP_HANDLE.load(Relaxed));
                        LOOP_HANDLE.store(-1, Relaxed);
                    }
                } else if button_success(" ▶ Start loop ") {
                    LOOP_HANDLE.store(sound::play_looped(&beep_wav(220.0, 0.5)), Relaxed);
                }
                space(8.0);
                badge(if playing { "LOOPING" } else { "STOPPED" },
                      if playing { Color::GREEN } else { Color::rgb(100, 116, 139) });
            });
            if playing {
                space(6.0);
                let vol = slider(103, "Volume", 0.0, 2.0, 1.0);
                sound::set_volume(LOOP_HANDLE.load(Relaxed), vol);
            }
        });
    });

    section("Putting it together", || {
        card(|| {
            row(|| {
                text("Counter", 16.0, Color::WHITE);
                space(8.0);
                let count = COUNTER.load(Relaxed);
                let color = if count > 0 { Color::GREEN } else if count < 0 { Color::RED } else { Color::rgb(100, 116, 139) };
                badge(&count.to_string(), color);
            });
            space(8.0);
            row(|| {
                if button_success("  +  ") { COUNTER.fetch_add(1, Relaxed); }
                if button_danger("  −  ")  { COUNTER.fetch_sub(1, Relaxed); }
                if button_ghost(" Reset ") { COUNTER.store(0, Relaxed); }
            });
            space(10.0);
            code("row + button_success/button_danger/button_ghost + badge, wired to one AtomicI32");
        });
    });
}

// One group of related widgets under a heading.
fn section<F: FnOnce()>(title: &str, f: F) {
    text(title, 20.0, Color::WHITE);
    space(8.0);
    f();
    space(16.0);
}

// One widget: name, a one-line description, the live thing, then its code.
fn entry<F: FnOnce()>(name: &str, desc: &str, snippet: &str, f: F) {
    card(|| {
        text(name, 15.0, Color::rgb(56, 189, 248));
        small(desc);
        space(8.0);
        f();
        space(8.0);
        card_color(CODE_BG, || {
            for line in snippet.lines() {
                code(line);
            }
        });
    });
    space(10.0);
}
