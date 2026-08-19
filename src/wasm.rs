use wasm_bindgen::prelude::*;

/// the main function when WASM module is loaded
#[wasm_bindgen(start)]
pub fn main_wasm() {
    console_error_panic_hook::set_once();
}
