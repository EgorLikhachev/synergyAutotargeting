//! Линковка с librknnrt.so при включённой фиче `npu`.
//!
//! Порядок поиска библиотеки:
//! 1. $RKNN_LIB_DIR (если задана)
//! 2. /usr/lib (apt-пакет rknpu2-rk3588 кладёт librknnrt.so именно туда)

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RKNN_LIB_DIR");

    #[cfg(feature = "npu")]
    {
        if let Some(dir) = std::env::var_os("RKNN_LIB_DIR") {
            println!("cargo:rustc-link-search=native={}", dir.to_string_lossy());
        } else {
            for dir in ["/usr/lib", "/usr/lib/aarch64-linux-gnu", "/usr/local/lib"] {
                println!("cargo:rustc-link-search=native={dir}");
            }
        }
        println!("cargo:rustc-link-lib=dylib=rknnrt");
    }
}
