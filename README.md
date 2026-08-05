# Rust WASM Template

A minimal, linted Rust library that compiles to WebAssembly (WASM).

## Purpose

This repository provides a clean starting point for Rust‑to‑WASM projects. It includes:

- Cargo metadata with `wasm-bindgen` dependency
- Strict Rust and Clippy lints enforced at the crate level
- Example exported functions (`add`)
- Panic hook for better error messages in the browser/Node.js

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (latest stable)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/) – for building and packaging the WASM module

## Testing

### Native Tests

```bash
cargo test
```

### Wasm Tests (wasm-bindgen-test)

Requires Node.js (`https://nodejs.org/`).

```bash
# Build for WASM with wasm-bindgen-test support
wasm-pack build --target web

# Run tests using Node.js runtime
wasm-pack test --node
```

## Building for WebAssembly

```bash
wasm-pack build --target web   # or --target bundler, nodejs, etc.argo build
