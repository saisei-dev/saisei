//! Patched build: compile the VENDORED capstone 5.0.7 x86-only C sources
//! statically (see capstone/ in this crate — the exact `src/` tree from the
//! PyPI capstone 5.0.7 sdist, i.e. the same sources the Python reference's
//! wheel `libcapstone.so` was built from). No env vars, no external library,
//! no runtime .so dependency: `cargo build` works out of the box and the
//! resulting binaries are self-contained.
//!
//! Escape hatch: set `SAISEI_CAPSTONE_LIB_DIR` to a directory containing
//! `libcapstone.so` to link an external dylib instead (the pre-vendoring
//! behavior, useful for comparing against a different capstone build). The
//! external .so is resolved by SONAME at runtime, so that dir must also carry
//! a `libcapstone.so.5` symlink.
//!
//! Why 5.0.7 exactly: upstream capstone-sys 0.16.0 bundles capstone 5.0-rc2,
//! which disagrees with 5.0.7 on some operand-access flags (e.g.
//! `test [mem], imm` -> readwrite vs read), and that flag feeds the
//! translator's flag-liveness analysis. The pre-generated bindings are
//! ABI-stable across capstone 5.0.x.
#![allow(dead_code)]

use std::env;
use std::fs::copy;
use std::path::PathBuf;

include!("common.rs");

/// Core (arch-independent) capstone sources.
const CORE_SRCS: &[&str] = &[
    "capstone/cs.c",
    "capstone/Mapping.c",
    "capstone/MCInst.c",
    "capstone/MCInstrDesc.c",
    "capstone/MCRegisterInfo.c",
    "capstone/SStream.c",
    "capstone/utils.c",
];

/// x86 module sources (the only architecture Saisei decodes).
const X86_SRCS: &[&str] = &[
    "capstone/arch/X86/X86ATTInstPrinter.c",
    "capstone/arch/X86/X86Disassembler.c",
    "capstone/arch/X86/X86DisassemblerDecoder.c",
    "capstone/arch/X86/X86InstPrinterCommon.c",
    "capstone/arch/X86/X86IntelInstPrinter.c",
    "capstone/arch/X86/X86Mapping.c",
    "capstone/arch/X86/X86Module.c",
];

fn build_vendored_capstone() {
    let mut build = cc::Build::new();
    build
        .include("capstone/include")
        .include("capstone")
        // Match the reference wheel's core build (make.sh + config.mk defaults):
        // USE_SYS_DYN_MEM=yes, DIET=no, X86_REDUCE=no, X86_ATT_DISABLE unset.
        .define("CAPSTONE_HAS_X86", None)
        .define("CAPSTONE_USE_SYS_DYN_MEM", None)
        .flag_if_supported("-std=gnu99")
        .warnings(false);
    for src in CORE_SRCS.iter().chain(X86_SRCS) {
        build.file(src);
    }
    // Emits cargo:rustc-link-lib=static=capstone + the OUT_DIR link-search.
    build.compile("capstone");
}

fn main() {
    println!("cargo:rerun-if-env-changed=SAISEI_CAPSTONE_LIB_DIR");
    println!("cargo:rerun-if-changed=pre_generated/{BINDINGS_FILE}");
    println!("cargo:rerun-if-changed=pre_generated/{BINDINGS_IMPL_FILE}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=capstone");

    match env::var("SAISEI_CAPSTONE_LIB_DIR") {
        Ok(libdir) => {
            println!("cargo:rustc-link-search=native={libdir}");
            println!("cargo:rustc-link-lib=dylib=capstone");
        }
        Err(_) => build_vendored_capstone(),
    }

    // lib.rs includes these from OUT_DIR; copy the checked-in bindings across.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let man = env::var("CARGO_MANIFEST_DIR").unwrap();
    copy(format!("{man}/pre_generated/{BINDINGS_FILE}"), out.join(BINDINGS_FILE))
        .expect("copy capstone.rs bindings");
    copy(format!("{man}/pre_generated/{BINDINGS_IMPL_FILE}"), out.join(BINDINGS_IMPL_FILE))
        .expect("copy capstone_archs_impl.rs bindings");
}
