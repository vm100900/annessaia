// SERVER is baked in via option_env!, and cargo does not otherwise track
// arbitrary environment variables — without this, switching ANNESSAIA_SERVER
// would silently reuse a stale binary pointed at the previous registry.
fn main() {
    println!("cargo:rerun-if-env-changed=ANNESSAIA_SERVER");
    println!("cargo:rerun-if-env-changed=ANNESSAIA_READONLY");
}
