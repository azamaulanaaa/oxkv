use wasm_bindgen::prelude::*;

/// Adds two unsigned 64-bit integers.
#[wasm_bindgen]
#[must_use]
pub fn add(left: u64, right: u64) -> u64 {
    super::add(left, right)
}

/// the main function when WASM module is loaded
#[wasm_bindgen(start)]
pub fn main_wasm() {
    console_error_panic_hook::set_once();
}
