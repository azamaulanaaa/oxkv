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

## Building for WebAssembly

```bash
wasm-pack build --target web   # or --target bundler, nodejs, etc.argo build
