// annessaia — CLI for the annessaia runtime.
//
// Usage:
//   annessaia new <name>                    create a new project with a hello-world template
//   annessaia build [file.rs] [options]     compile to <stem>.wasmh (bundling assets/ in
//                                            if it has files) — defaults to src/lib.rs
//                                            when run inside a project
//
// Options for build:
//   --out, -o <path>   exact output path — a plain .wasm/.wasmpackage, no hash trailer
//   --verbose, -v      show cargo output

use std::{env, fs, io::Write, path::{Path, PathBuf}, process::{Command, Stdio, exit}};

// ── Embedded SDK ──────────────────────────────────────────────────────────────
const SDK_CARGO: &str = include_str!("../../../sdk/Cargo.toml");
const SDK_LIB:   &str = include_str!("../../../sdk/src/lib.rs");

// ── Hello-world template ──────────────────────────────────────────────────────
const TEMPLATE: &str = r#"use annessaia_sdk::prelude::*;

use core::sync::atomic::{AtomicI32, Ordering::Relaxed};
static CLICKS: AtomicI32 = AtomicI32::new(0);

#[no_mangle]
pub extern "C" fn render() {
    heading("Hello, annessaia!");
    label("A WASM app written in Rust.");
    separator();

    let n = CLICKS.load(Relaxed);
    label(&format!("Clicked {} time{}", n, if n == 1 { "" } else { "s" }));
    space(8.0);

    row(|| {
        if button("  Click me!  ") {
            CLICKS.fetch_add(1, Relaxed);
        }
        if button("  Reset  ") {
            CLICKS.store(0, Relaxed);
        }
    });
}
"#;

// ── SDK / build dirs ──────────────────────────────────────────────────────────

fn base_dir() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::Path::new(&home).join(".annessaia")
}

fn sdk_dir()   -> PathBuf { base_dir().join("sdk") }
fn build_dir() -> PathBuf { base_dir().join("build") }

fn write_if_changed(path: &PathBuf, content: &str) {
    if fs::read_to_string(path).ok().as_deref() != Some(content) {
        fs::write(path, content).unwrap_or_else(|e| {
            eprintln!("error writing {}: {e}", path.display()); exit(1);
        });
    }
}

fn deploy_sdk() {
    let sdk = sdk_dir();
    fs::create_dir_all(sdk.join("src")).unwrap_or_else(|e| {
        eprintln!("error creating sdk dir: {e}"); exit(1);
    });
    write_if_changed(&sdk.join("Cargo.toml"), SDK_CARGO);
    write_if_changed(&sdk.join("src/lib.rs"), SDK_LIB);
}

// ── annessaia new ─────────────────────────────────────────────────────────────

fn cmd_new(name: &str) {
    let dir = PathBuf::from(name);
    if dir.exists() {
        eprintln!("error: '{}' already exists", dir.display());
        exit(1);
    }

    deploy_sdk();

    let sdk_path = sdk_dir();
    let cargo_toml = format!(
r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
annessaia-sdk = {{ path = "{sdk}" }}

[profile.release]
opt-level = "s"
lto = true
strip = true
"#,
        sdk = sdk_path.display()
    );

    fs::create_dir_all(dir.join("src")).unwrap_or_else(|e| {
        eprintln!("error creating project: {e}"); exit(1);
    });
    fs::write(dir.join("Cargo.toml"), &cargo_toml).unwrap_or_else(|e| {
        eprintln!("error: {e}"); exit(1);
    });
    fs::write(dir.join("src/lib.rs"), TEMPLATE).unwrap_or_else(|e| {
        eprintln!("error: {e}"); exit(1);
    });

    // Every new project gets an assets/ folder up front, empty or not: drop
    // images/audio/etc. in here and `annessaia build` bundles them into a
    // .wasmpackage automatically, no separate opt-in step. An empty
    // directory alone wouldn't survive git, hence the placeholder file.
    fs::create_dir_all(dir.join("assets")).unwrap_or_else(|e| {
        eprintln!("error creating assets dir: {e}"); exit(1);
    });
    fs::write(dir.join("assets/README.md"), ASSETS_README).unwrap_or_else(|e| {
        eprintln!("error: {e}"); exit(1);
    });

    eprintln!("✓ Created {name}/");
    eprintln!();
    eprintln!("  cd {name}");
    eprintln!("  annessaia build");
    eprintln!("  # or: cargo build --target wasm32-unknown-unknown --release");
    eprintln!();
    eprintln!("  Drop images/audio/etc. in assets/ and load them with");
    eprintln!("  assets::load(\"name\") — annessaia build bundles them into a");
    eprintln!("  .wasmpackage automatically once assets/ has any files in it.");
}

const ASSETS_README: &str = "\
Drop files here — images, audio, anything else your app needs at runtime.

Load one from your app with:

    let bytes = annessaia_sdk::assets::load(\"sprite.png\").unwrap();
    let sprite = annessaia_sdk::image::decode(&bytes);

Paths are relative to this folder, not the project root (a file at
assets/sfx/hit.wav is loaded as assets::load(\"sfx/hit.wav\")).

As soon as this folder has any file in it besides this README, `annessaia
build` bundles app.wasm together with everything here into a single
.wasmpackage instead of a bare .wasm — nothing else to configure.
";

// ── annessaia build ───────────────────────────────────────────────────────────

fn cmd_build(args: &[String]) {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut verbose = false;

    let mut i = 0;
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

    // No file given — assume the same layout `annessaia new` scaffolds
    // (src/lib.rs next to Cargo.toml) rather than requiring it spelled out
    // every time, the same way `cargo build` needs no path when run inside
    // a project. Tracked separately from `input` itself so the output name
    // can come from the project directory ("testing.wasm") rather than the
    // file's own stem ("lib.wasm") in this case — matching what `cargo
    // build` would call it.
    let defaulted = input.is_none();
    let input = match input.or_else(|| {
        let default = PathBuf::from("src/lib.rs");
        default.exists().then_some(default)
    }) {
        Some(p) => p,
        None => {
            eprintln!("error: no input file, and no src/lib.rs in the current directory");
            eprintln!();
            eprintln!("  usage: annessaia build [file.rs]");
            eprintln!("  (run with no arguments from inside a project to build src/lib.rs)");
            exit(1);
        }
    };
    if !input.exists() {
        eprintln!("error: file not found: {}", input.display()); exit(1);
    }

    // "lib" (the file stem) isn't a name anyone chose — when the file was
    // defaulted rather than given explicitly, name the output after the
    // project directory instead, e.g. "testing.wasm" not "lib.wasm".
    let stem = if defaulted {
        env::current_dir().ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "app".to_string())
    } else {
        input.file_stem().and_then(|s| s.to_str()).unwrap_or("app").to_string()
    };
    let stem = stem.as_str();

    // assets/ is relative to the project root (cwd), not the input file — it
    // lives next to Cargo.toml, the same place `annessaia new` created it,
    // regardless of which src/*.rs is being built. The scaffolded README.md
    // doesn't count: a fresh, still-empty project must build a plain .wasm,
    // not a one-file "package" nobody asked for.
    let assets_dir = env::current_dir().unwrap_or_default().join("assets");
    let asset_files: Vec<(String, PathBuf)> = collect_assets(&assets_dir)
        .into_iter()
        .filter(|(rel, _)| rel != "README.md")
        .collect();
    let use_package = !asset_files.is_empty();

    // An explicit -o is respected exactly as given — plain bytes, no hash
    // trailer appended, on a name someone chose deliberately. Left unset,
    // the output becomes <stem>.wasmh with a trailer appended once the
    // content (and so its hash) is actually known — see below.
    let output_explicit = output.clone();

    let user_source = match fs::read_to_string(&input) {
        Ok(s)  => s,
        Err(e) => { eprintln!("error reading {}: {e}", input.display()); exit(1); }
    };

    deploy_sdk();

    let build = build_dir();
    fs::create_dir_all(build.join("src")).unwrap_or_else(|e| {
        eprintln!("error creating build dir: {e}"); exit(1);
    });

    let sdk_path = sdk_dir();
    let cargo_toml = format!(
r#"[package]
name = "annessaia-app"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
annessaia-sdk = {{ path = "{sdk}" }}

[profile.release]
opt-level = "s"
lto = true
strip = true
"#,
        sdk = sdk_path.display()
    );
    fs::write(build.join("Cargo.toml"), &cargo_toml).unwrap_or_else(|e| {
        eprintln!("error writing Cargo.toml: {e}"); exit(1);
    });

    let header = if user_source.contains("use annessaia_sdk::prelude") { "" } else { "use annessaia_sdk::prelude::*;\n\n" };
    let lib_rs = format!(
        "{}// ── {} ──\n\n{user_source}",
        header,
        input.display()
    );
    fs::write(build.join("src/lib.rs"), &lib_rs).unwrap_or_else(|e| {
        eprintln!("error writing src/lib.rs: {e}"); exit(1);
    });

    eprintln!("→ annessaia build: compiling {} …", input.display());
    if verbose {
        eprintln!("  sdk       : {}", sdk_dir().display());
        eprintln!("  build dir : {}", build.display());
        match &output_explicit {
            Some(p) => eprintln!("  output    : {}", p.display()),
            None    => eprintln!("  output    : <stem>.wasmh"),
        }
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--target").arg("wasm32-unknown-unknown")
        .arg("--release")
        .current_dir(&build);
    if !verbose { cmd.stdout(Stdio::null()); }

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
        eprintln!("error: expected output not found at {}", wasm_src.display()); exit(1);
    }

    // Assembled into a scratch location first — the hash trailer has to be
    // computed from the finished bytes, so nothing is written to the user's
    // own directory until that's done, just below.
    let staged = if use_package {
        let path = build.join("staged.wasmpackage");
        if let Err(e) = write_package(&wasm_src, &asset_files, &path) {
            eprintln!("error writing package: {e}"); exit(1);
        }
        path
    } else {
        wasm_src.clone()
    };
    let bytes = fs::read(&staged).unwrap_or_else(|e| {
        eprintln!("error reading {}: {e}", staged.display()); exit(1);
    });

    // An explicit -o gets exactly those bytes, exactly that name — a plain,
    // portable .wasm/.wasmpackage usable anywhere else that reads one, with
    // no proprietary trailer tacked on. Left unset, the default .wasmh
    // format appends a raw 32-byte SHA-256 digest of `bytes` to its own end;
    // annessaia's loader (see strip_hash_trailer in annessaia/src/main.rs)
    // reads that trailer back out — via an HTTP Range request for just those
    // last bytes, not a full download — to tell whether a URL's content has
    // actually changed before fetching the rest of it.
    let hash = sha256_hex(&bytes);
    let (output, final_bytes): (PathBuf, std::borrow::Cow<[u8]>) = match output_explicit {
        Some(p) => (p, std::borrow::Cow::Borrowed(&bytes)),
        None => {
            let path = env::current_dir().unwrap_or_default().join(format!("{stem}.wasmh"));
            let mut with_trailer = bytes.clone();
            with_trailer.extend_from_slice(&hex_to_bytes(&hash));
            (path, std::borrow::Cow::Owned(with_trailer))
        }
    };

    if let Some(parent) = output.parent() { let _ = fs::create_dir_all(parent); }
    if let Err(e) = fs::write(&output, &final_bytes) {
        eprintln!("error writing {}: {e}", output.display()); exit(1);
    }

    let size_kb = bytes.len() / 1024;
    if use_package {
        eprintln!("✓ {} ({size_kb} KiB, {} asset file(s), hash {hash})", output.display(), asset_files.len());
    } else {
        eprintln!("✓ {} ({size_kb} KiB, hash {hash})", output.display());
    }
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// Every file under `dir`, recursively, as (path relative to dir with forward
// slashes, absolute path) — used both to detect whether assets/ has
// anything in it and to enumerate what to bundle.
fn collect_assets(dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(base: &Path, current: &Path, out: &mut Vec<(String, PathBuf)>) {
        let Ok(entries) = fs::read_dir(current) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.push((rel.to_string_lossy().replace('\\', "/"), path));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

// Bundles the compiled module and every asset file into a .wasmpackage — a
// zip with `app.wasm` at its root and everything else under `assets/`,
// matching exactly what the annessaia host's extract_package expects.
// Written Stored (uncompressed): every asset here is already almost always
// a compressed format (PNG/JPEG/MP3/...), so a second compression pass would
// mostly just spend CPU proving there's nothing left to save.
fn write_package(wasm_path: &Path, assets: &[(String, PathBuf)], output: &Path) -> std::io::Result<()> {
    let file = fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);

    zip.start_file("app.wasm", options)?;
    zip.write_all(&fs::read(wasm_path)?)?;

    for (rel, path) in assets {
        zip.start_file(format!("assets/{rel}"), options)?;
        zip.write_all(&fs::read(path)?)?;
    }

    zip.finish()?;
    Ok(())
}

// ── Help ──────────────────────────────────────────────────────────────────────

fn print_help() {
    eprintln!(
r#"annessaia — CLI for the annessaia runtime

USAGE:
  annessaia new <name>              create a new project (hello-world template)
  annessaia build                   build src/lib.rs of the project in the
                                     current directory, as <name>.wasmh
                                     (bundling assets/ into it automatically
                                     if that folder has any files in it)
  annessaia build <file.rs>         build a specific file instead
  annessaia build -o out.wasm       plain output, no hash trailer, this exact path
  annessaia build -v                verbose cargo output

EXAMPLES:
  annessaia new my-game
  cd my-game && annessaia build

  annessaia build sketch.rs --out dist/sketch.wasm

Every default (non -o) build is a .wasmh: the real content (a .wasm, or a
.wasmpackage if assets/ has files) with a raw SHA-256 digest of itself
appended to the end. annessaia (the runtime) reads just that trailer via an
HTTP Range request — not a full download — before deciding whether to fetch
the rest, so reopening a URL whose exact content it already has cached
needs no full network request at all.

The annessaia SDK is deployed to ~/.annessaia/sdk/ automatically.
Your file gets `use annessaia_sdk::prelude::*;` injected — export one of:
  pub extern "C" fn render()              widget UI mode
  pub extern "C" fn render_gpu()          GPU draw mode
  pub extern "C" fn render_pixels(w, h)   pixel buffer mode

Assets (images, audio, ...) go in assets/, next to Cargo.toml — load them
with assets::load("name"). Their presence is all it takes: `annessaia build`
then bundles app.wasm together with assets/ into a .wasmpackage instead of a
bare .wasm, no extra flag needed.

First-time setup:
  rustup target add wasm32-unknown-unknown"#
    );
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("new") => {
            let name = args.get(2).unwrap_or_else(|| {
                eprintln!("error: missing project name\n  usage: annessaia new <name>");
                exit(1);
            });
            if name.starts_with('-') {
                eprintln!("error: invalid project name '{name}'"); exit(1);
            }
            cmd_new(name);
        }
        Some("build") => {
            cmd_build(&args[2..]);
        }
        Some("--help") | Some("-h") | None => { print_help(); }
        Some(cmd) => {
            eprintln!("error: unknown command '{cmd}'\n");
            print_help();
            exit(1);
        }
    }
}
