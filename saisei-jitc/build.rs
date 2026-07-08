//! Bake an rpath to the external libcapstone so the built binary finds it at
//! runtime without LD_LIBRARY_PATH. See vendor/capstone-sys.
fn main() {
    if let Ok(libdir) = std::env::var("SAISEI_CAPSTONE_LIB_DIR") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{libdir}");
    }
    println!("cargo:rerun-if-env-changed=SAISEI_CAPSTONE_LIB_DIR");
}
