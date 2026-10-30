//! The main program of the worker's Trunk page (see Cargo.toml). Never shipped.

fn main() {
    // wasm-bindgen can't post-process a module that uses nothing of it.
    #[cfg(target_arch = "wasm32")]
    drop(js_sys::Object::new());
}
