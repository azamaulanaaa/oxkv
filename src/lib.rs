//! A minimal WASM library template with strict linting.

use wasm_bindgen::prelude::*;

/// Adds two unsigned 64-bit integers.
#[must_use]
#[wasm_bindgen]
pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

/// the main function when WASM module is loaded
#[wasm_bindgen(start)]
pub fn main_wasm() {
    console_error_panic_hook::set_once();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
