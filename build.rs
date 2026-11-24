fn main() {
    // Find libduckdb where it was linked from when the binary runs.
    if let Ok(dir) = std::env::var("DUCKDB_LIB_DIR") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
    println!("cargo:rerun-if-env-changed=DUCKDB_LIB_DIR");
}
