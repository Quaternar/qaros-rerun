// QAROS_RERUN_VERSION: the full version of qaros-rerun (crates/build_version.rs).
include!("../build_version.rs");

fn main() {
    println!("cargo:rustc-env=QAROS_RERUN_VERSION={}", full_version());
}
