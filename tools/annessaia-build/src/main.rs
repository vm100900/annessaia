// annessaia-build — compile a single .rs file into a .wasm for the annessaia runtime.
//
// Usage:
//   annessaia-build game.rs                  → game.wasm in current directory
//   annessaia-build game.rs --out dist/game.wasm
//
// The annessaia-sdk crate is deployed to ~/.annessaia/sdk/ and linked automatically.
// Your file just needs to export render_gpu(), render_pixels(w,h), or render().

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Stdio, exit},
};

// ── Embedded SDK source (written to ~/.annessaia/sdk/ on each run) ────────────
const SDK_CARGO: &str  = include_str!("../../../sdk/Cargo.toml");
const SDK_LIB:   &str  = include_str!("../../../sdk/src/lib.rs");

// ── Injected header — just the use statement, no raw host function code ───────
const HEADER: &str = "use annessaia_sdk::prelude::*;\n";

fn base_dir() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::Path::new(&home).join(".annessaia")
}

fn sdk_dir()   -> PathBuf { base_dir().join("sdk") }
fn build_dir() -> PathBuf { base_dir().join("build") }

/// Write a file only if its content has changed (avoids spurious Cargo recompiles).
fn write_if_changed(path: &PathBuf, content: &str) {
    if fs::read_to_string(path).ok().as_deref() != Some(content) {
        fs::write(path, content).unwrap_or_else(|e| {
            eprintln!("error writing {}: {e}", path.display()); exit(1);
        });
    }
}

/// Deploy SDK source files to ~/.annessaia/sdk/.
/// Only writes when content has changed so Cargo doesn't rebuild the SDK every time.
fn deploy_sdk() {
    let sdk = sdk_dir();
    fs::create_dir_all(sdk.join("src")).unwrap_or_else(|e| {
        eprintln!("error creating sdk dir: {e}"); exit(1);
    });
    write_if_changed(&sdk.join("Cargo.toml"), SDK_CARGO);
    write_if_changed(&sdk.join("src/lib.rs"), SDK_LIB);
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 || args[1] == "--help" || args[1] == "-h" {
        print_help();
        return;
    }

    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut verbose = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" | "-o" => {
                i += 1;
                if i < args.len() { output = Some(PathBuf::from(&args[i])); }
            }
            "--verbose" | "-v" => { verbose = true; }
            a if !a.starts_with('-') => {
                if input.is_none() { input = Some(PathBuf::from(a)); }
            }
            flag => { eprintln!("unknown flag: {flag}"); exit(1); }
        }
        i += 1;
    }

    let input = match input {
        Some(p) => p,
        None => { eprintln!("error: no input file specified\n"); print_help(); exit(1); }
    };
    if !input.exists() {
        eprintln!("error: file not found: {}", input.display());
        exit(1);
    }

    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("app");
    let output = output.unwrap_or_else(|| {
        env::current_dir().unwrap_or_default().join(format!("{stem}.wasm"))
    });

    let user_source = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => { eprintln!("error reading {}: {e}", input.display()); exit(1); }
    };

    // Deploy SDK to ~/.annessaia/sdk/ (always overwrite to keep in sync with this binary)
    deploy_sdk();

    // Set up persistent build project at ~/.annessaia/build/
    let build = build_dir();
    fs::create_dir_all(build.join("src")).unwrap_or_else(|e| {
        eprintln!("error creating build dir: {e}"); exit(1);
    });

    // Cargo.toml references the deployed SDK via path
    let sdk_path = sdk_dir();
    let cargo_toml = format!(
r#"[package]
name = "annessaia-app"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
annessaia-sdk = {{ path = "{sdk_path}" }}

[profile.release]
opt-level = "s"
lto = true
strip = true
"#,
        sdk_path = sdk_path.display()
    );
    fs::write(build.join("Cargo.toml"), &cargo_toml).unwrap_or_else(|e| {
        eprintln!("error writing Cargo.toml: {e}"); exit(1);
    });

    // lib.rs = prelude import + user source
    let lib_rs = format!(
        "{HEADER}\n// ── {} ──────────────────────────────────────────────────────\n\n{user_source}",
        input.display()
    );
    fs::write(build.join("src/lib.rs"), &lib_rs).unwrap_or_else(|e| {
        eprintln!("error writing src/lib.rs: {e}"); exit(1);
    });

    eprintln!("→ annessaia-build: compiling {} …", input.display());
    if verbose {
        eprintln!("  sdk       : {}", sdk_dir().display());
        eprintln!("  build dir : {}", build.display());
        eprintln!("  output    : {}", output.display());
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--target").arg("wasm32-unknown-unknown")
        .arg("--release")
        .current_dir(&build);

    if !verbose {
        cmd.stdout(Stdio::null());
    }

    let status = cmd.status().unwrap_or_else(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            eprintln!("error: `cargo` not found — install Rust from https://rustup.rs");
        } else {
            eprintln!("error running cargo: {e}");
        }
        exit(1);
    });

    if !status.success() {
        eprintln!("\n✗ build failed.");
        eprintln!("  hint: if the wasm32 target is missing, run:");
        eprintln!("        rustup target add wasm32-unknown-unknown");
        exit(status.code().unwrap_or(1));
    }

    let wasm_src = build.join("target/wasm32-unknown-unknown/release/annessaia_app.wasm");
    if !wasm_src.exists() {
        eprintln!("error: expected output not found at {}", wasm_src.display());
        exit(1);
    }

    if let Some(parent) = output.parent() { let _ = fs::create_dir_all(parent); }
    if let Err(e) = fs::copy(&wasm_src, &output) {
        eprintln!("error copying wasm: {e}"); exit(1);
    }

    let size_kb = fs::metadata(&output).map(|m| m.len() / 1024).unwrap_or(0);
    eprintln!("✓ {} ({size_kb} KiB)", output.display());
}

fn print_help() {
    eprintln!(
r#"annessaia-build — compile a Rust file into an annessaia WASM app

USAGE:
  annessaia-build <file.rs> [OPTIONS]

OPTIONS:
  --out, -o <path>   output path (default: <stem>.wasm in cwd)
  --verbose, -v      show cargo output
  --help, -h         show this message

EXAMPLE:
  annessaia-build game.rs
  annessaia-build game.rs --out dist/game.wasm

The annessaia SDK (annessaia-sdk crate) is available automatically.
use annessaia_sdk::prelude::* is injected — write your app like this:

  #[no_mangle]
  pub extern "C" fn render_gpu() {{
      clear(Color::BLACK);
      circle(400.0, 300.0, 60.0, Color::CYAN);
      if left_clicked() {{ sys::log("clicked!"); }}
      net::get(1, "https://api.example.com");
      if let Some(body) = net::poll_str(1) {{ sys::log(&body); }}
  }}

For a proper Cargo project instead of single-file builds:
  Add to Cargo.toml: annessaia-sdk = {{ path = "~/.annessaia/sdk" }}
  Then:             use annessaia_sdk::prelude::*;

Your file must export one of:
  pub extern "C" fn render_gpu()         — GPU draw mode
  pub extern "C" fn render_pixels(w, h)  — pixel buffer mode (+ pixel_app!())
  pub extern "C" fn render()             — widget UI mode

Optional: pub extern "C" fn init()  (called once on load)

First-time setup:
  rustup target add wasm32-unknown-unknown"#
    );
}
