use annessaia_sdk::prelude::*;

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
