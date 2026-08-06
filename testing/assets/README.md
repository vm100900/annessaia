Drop files here — images, audio, anything else your app needs at runtime.

Load one from your app with:

    let bytes = annessaia_sdk::assets::load("sprite.png").unwrap();
    let sprite = annessaia_sdk::image::decode(&bytes);

Paths are relative to this folder, not the project root (a file at
assets/sfx/hit.wav is loaded as assets::load("sfx/hit.wav")).

As soon as this folder has any file in it besides this README, `annessaia
build` bundles app.wasm together with everything here into a single
.wasmpackage instead of a bare .wasm — nothing else to configure.
