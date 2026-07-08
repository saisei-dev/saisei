//! Patched build: link an EXTERNAL libcapstone (chosen version) via
//! `SAISEI_CAPSTONE_LIB_DIR`, instead of compiling the vendored 5.0-rc2 sources.
//! The pre-generated bindings are ABI-stable across capstone 5.0.x, so they bind
//! the external 5.0.x library correctly.
#![allow(dead_code)]

use std::env;
use std::fs::copy;
use std::path::PathBuf;

include!("common.rs");

fn main() {
    let libdir = env::var("SAISEI_CAPSTONE_LIB_DIR").expect(
        "SAISEI_CAPSTONE_LIB_DIR must point to the directory containing \
         libcapstone.so (e.g. the reference's capstone/lib dir)",
    );
    println!("cargo:rustc-link-search=native={libdir}");
    println!("cargo:rustc-link-lib=dylib=capstone");
    println!("cargo:rerun-if-env-changed=SAISEI_CAPSTONE_LIB_DIR");
    println!("cargo:rerun-if-changed=pre_generated/{BINDINGS_FILE}");
    println!("cargo:rerun-if-changed=pre_generated/{BINDINGS_IMPL_FILE}");
    println!("cargo:rerun-if-changed=build.rs");

    // lib.rs includes these from OUT_DIR; copy the checked-in bindings across.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let man = env::var("CARGO_MANIFEST_DIR").unwrap();
    copy(format!("{man}/pre_generated/{BINDINGS_FILE}"), out.join(BINDINGS_FILE))
        .expect("copy capstone.rs bindings");
    copy(format!("{man}/pre_generated/{BINDINGS_IMPL_FILE}"), out.join(BINDINGS_IMPL_FILE))
        .expect("copy capstone_archs_impl.rs bindings");
}
