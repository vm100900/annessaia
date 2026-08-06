// See apps/search/build.rs — SERVER comes from option_env! and cargo needs to
// be told to watch it, or a rebuild with a different target is a no-op.
fn main() {
    println!("cargo:rerun-if-env-changed=ANNESSAIA_SERVER");
}
