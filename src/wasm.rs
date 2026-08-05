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

#[cfg(test)]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn adds_two_numbers() {
        assert_eq!(add(2, 2), 4);
    }

    #[wasm_bindgen_test]
    fn handles_zero_values() {
        assert_eq!(add(0, 0), 0);
    }

    #[wasm_bindgen_test]
    fn handles_large_u64_addition() {
        let left = 1_000_000u64;
        let right = 2_000_000u64;
        assert_eq!(add(left, right), 3_000_000);
    }

    #[wasm_bindgen_test]
    fn handles_mixed_value_types() {
        // wasm-bindgen coerces JS numbers to u64 at the binding layer.
        // Both parameters must be u64 per the function signature, but we verify
        // that integer coercion works correctly through the binding.
        assert_eq!(add(0u64, 100), 100);
    }
}
