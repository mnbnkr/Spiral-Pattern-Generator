# <a href="https://mnbnkr.github.io/Spiral-Pattern-Generator/">Spiral Pattern Generator</a>
<br>
Inspired by <i>- Numberphile's 'Red & Black Knights' <a href="https://youtu.be/UiX4CFIiegM">video</a>, - Jonas Karlsson's <a href="https://jonka364.github.io/stendhal/stendhal.html">work</a>, - and Pitouli's 'Knights' <a href="https://github.com/Pitouli/knights">web application</a>.</i>

<br><br>

Rust/WASM application for deterministic spiral pattern simulations. The engine runs in a Web Worker, the main thread owns UI and one WebGL canvas, and the build output is static GitHub Pages-compatible assets.

## Manual

The detailed source of truth is [MANUAL.md](MANUAL.md). It documents the board math, rule engine, placement modes, radius semantics, rendering/export behavior, UI state rules, and verification checklist.

## Requirements

- Rust `1.96` stable
- `wasm32-unknown-unknown` target
- Trunk `0.21.x`
- Corepack-managed `pnpm` for Playwright tests only
- `wasm-bindgen`, `web-sys`, `js-sys`
- Binary worker transport with `bincode` over transferable `Uint8Array`

## Run And Build

```powershell
# Development server
trunk serve --port 8080

# Release build for static hosting
trunk build --release
```

## Verify

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo build --target wasm32-unknown-unknown
trunk build --release

# Browser regression suite
pnpm exec playwright test --project=chromium
```

## Layout

- `src/bin/app.rs`: main-thread WASM entrypoint.
- `src/bin/worker.rs`: worker WASM entrypoint.
- `src/ui`: controls, worker lifecycle, placement log, downloads.
- `src/engine`: simulation state machines and spatial indexes.
- `src/math`: square, hex, triangle, and continuous geometry.
- `src/render`: WebGL renderer and deterministic image export.
- `src/render_data.rs`: packed worker-side render vertices.
- `src/protocol.rs`: app/worker message contracts.

GitHub Pages deployment is handled by `.github/workflows/pages.yml`; browser regression tests are in `tests/app.spec.ts`.
