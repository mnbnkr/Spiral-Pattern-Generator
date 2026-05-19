# AI Agent Architecture Directive

This repository is a Rust/WASM spiral simulation engine. Treat correctness, performance, and mathematical semantics as first-order requirements.

## Current Prompt Rule

If `current_prompt.md` exists, read it before planning or editing. Its instructions are the active task checklist for the current agent session. Treat plans as flexible working maps; when a plan and `current_prompt.md` differ, follow `current_prompt.md` unless the user explicitly says otherwise.

## Required Stack

- Use Rust `1.96.0-beta.6`.
- Target `wasm32-unknown-unknown`.
- Use `trunk` for browser builds and local serving.
- Do not introduce npm, Node build tooling, React, Yew, or heavy UI frameworks.
- Do not run npm commands. Playwright is a pnpm-only browser regression harness and is not part of the app build.
- Use `wasm-bindgen`, `web-sys`, and `js-sys` for browser APIs.
- Keep the app deployable as static GitHub Pages assets.

## Architecture

- Main app entrypoint: `src/bin/app.rs`.
- Worker entrypoint: `src/bin/worker.rs`.
- UI controls and worker messaging: `src/ui`.
- Simulation engine and state machines: `src/engine`.
- Math and geometry: `src/math`.
- Renderer: `src/render`.
- Message contracts: `src/protocol.rs`.

The mathematical engine must run in the Web Worker. The main thread owns UI and rendering only.

The run loop is a bounded main-thread pull loop: the main thread requests the next worker batch only after receiving the previous one, and canvas drawing is coalesced onto `requestAnimationFrame`. Do not revert to an unbounded worker-push loop, because that can make visuals and controls lag behind queued worker messages.

Worker transport uses `bincode` over transferable `Uint8Array`, not JSON strings. Control and result messages carry a worker epoch so stale same-worker reset messages cannot mutate a newer board/radius run. The worker owns hot-path render-data packing and sends `[x, y, r, g, b]` `Vec<f32>` vertex updates with each placement batch. The main thread should append those vertices, upload appended ranges with `bufferSubData`, redraw the full uploaded point buffer through rAF, and replace the full buffer only for recoloring events; it should not rebuild and recolor the full placement list every redraw.

## Agent Navigation

Keep this file high-signal and agent-focused. Detailed mathematical and UI semantics belong in `MANUAL.md`; `AGENTS.md` should stay concise enough to fit normal agent context budgets. Before editing, use the architecture map above and `rg` / `rg --files` to jump directly to the relevant module instead of scanning generated directories.

## Rendering

Simulation pieces must render on one HTML5 `<canvas>`. The current renderer uses WebGL point sprites through `web-sys`, not per-piece Canvas 2D calls.

Do not replace pieces with DOM nodes. Do not reintroduce per-piece Canvas 2D draw calls for large simulations unless a benchmark proves it is faster. The WebGL path exists to avoid thousands of WASM-to-JS canvas calls per frame.

## Mathematical Semantics

- Use `f64` for continuous geometry.
- Continuous spiral: `r(theta) = theta / tau`.
- Consecutive continuous spot centers are solved so their Euclidean chord distance is exactly `1.0`.
- Continuous offset is an arc-length offset from the origin in the range `0.0..1.0`; it is not a chord-length shortcut.
- Continuous placement bodies use the configurable `piece_radius`.
- Continuous attack circles are infinitely thin. A hit is:

```text
abs(center_distance - attack_radius) <= piece_radius + EPS
```

- Body overlap is:

```text
center_distance < 2 * piece_radius - EPS
```

- Keep Newton-Raphson plus bracketed fallback behavior for the continuous chord solver.
- The continuous chord solver must not stop high-radius iterators due f64 bisection midpoint precision. Preserve best-error fallback behavior around large `theta`.

## Radius Semantics

Radius is both a rendering/view bound and a simulation generation bound. The worker must stop with exhausted stats when the board iterator first leaves the requested radius.

- LatticeSquare bound: `max(abs(x), abs(y)) <= floor(radius)`.
- LatticeHex bound: `max(abs(cube_x), abs(cube_y), abs(cube_z)) <= floor(radius)`.
- LatticeTriangle bound: triangular spiral shell index `<= floor(radius)`, where shell `1` covers the first three turn segments and spots `1..=6`.
- ContinuousArchimedean bound: center distance from origin is `<= radius`.

## Engine Modes

Custom finite armies use spot-seeking with one forward scan cursor per army entry. In default `SpiralPath` mode each entry places on its turn at its next valid spiral cell, so chronological placements may fill earlier spiral cells after another entry skipped them; read final color/attack-set sequences by walking the spiral from the start. In `CenterDistance` mode each entry scans by origin distance with spiral index as the tie-breaker. Empty custom armies are valid editing states and should produce no placements until a piece is added.

Placement search has two explicit modes. `SpiralPath` is the default spiral order. `CenterDistance` searches valid spots by Euclidean center distance from the spiral origin, using spiral index only as a tie-breaker. Both modes must work on every board and with custom, Prime Knight, and Prime Gap Knight presets.

Prime Knight and Prime Gap Knight use piece-seeking. Every generated prime piece is consumed at most once.

Important exceptions:

- In prime modes with `Attack-set` enemy mode, all future prime attack-sets are unique. If a spot is passively attacked, no larger candidate can change that passive attacked-by fact, so the engine must skip that spot instead of testing the infinite pool forever.
- In prime modes with `Color` enemy mode, keep searching the current spot only while some future candidate color could solve it. Skip candidate-independent impossible spots, such as spots already attacked by multiple enemy color groups, or Prime Gap Knight spots that require the already-consumed gap-1 piece.

## Hex Rules

Hex spiral order starts at origin, moves visually right for spot 1, walks counterclockwise, and keeps adjacent ring transitions. Do not revert it to prebuilt ring jumps.

Hex lattice attacks are generated by cube rotations of axial `(a,b)` and, when distinct, `(b,a)`. Do not add extra sign-variant bases that double-count the attack field.

Interpreting a hex leaper `(a,b)` with a larger leg means moving the larger value straight in any of the six hex directions, then turning 60 degrees left or right for the smaller leg. `(1,2)` therefore has 12 destinations; equal-leg pieces have 6.

## Triangle Rules

Triangle Lattice placement spots are one fixed orientation of equilateral triangle cells. The opposite-orientation triangles in the tiling are hidden from placement but count for attack stepping.

Triangle spiral order starts at origin, moves visually right for spot 1, then turns 120 degrees left at triangular-number corners. Segment lengths are `1, 2, 3, 4, ...`; do not use the hex six-direction ring walker for Triangle Lattice.

Triangle attacks use three corner-aligned `A` rays. The `B` leg resolves to the two nearest visible same-orientation cells perpendicular to the `A` direction after counting through hidden flipped triangles. Deduplicate attacks and remove origin; `(1,1)`, `(2,1)`, and `(3,1)` should each produce six visible attack targets.

## Colors And Custom Army UI

- Custom finite colors are attached to list order, not permanently to pieces.
- First custom row maps to Anchor A, last row maps to Anchor B, and intermediate rows use a rainbow hue path.
- Added pieces automatically receive the color for their resulting order.
- Rows must remain draggable/reorderable, with order affecting placement and color group.
- The last custom piece can be deleted; keep an empty placeholder row instead of silently restoring a default piece.
- Prime Knight keeps the modulo bounce rule.
- Prime Gap Knight keeps dynamic min/max gap recoloring.
- Color gradients should use hue traversal, not linear RGB blending.
- Default anchors are orange-red `#ff7800` and red `#ff0006`.

## UI And Controls

- Keep Board Type, Piece Shape, view/generation Radius, Piece Radius, Speed/Fastest, Display Mode, Spiral Track, Zoom, Proactive Attack Rule, Ally/Enemy Condition, Army Preset, color anchors, and export controls wired.
- Switching to `ContinuousArchimedean` should default Piece Radius to `0.50`.
- Lattice boards default Piece Radius to `0.50`.
- Continuous shape is forced to Circle.
- Triangle Lattice offers Triangle and Circle render shapes only; Square and Hex are not valid triangle-board shapes.
- Hex is a render-only lattice Piece Shape. On LatticeHex, Piece Radius `0.50` must render as full regular hex cells that touch without overlap.
- LatticeTriangle Triangle shape must render default Piece Radius `0.50` as non-overlapping same-orientation equilateral triangles that touch on the triangular center lattice.
- LatticeHex should default to Hex shape the first time it is selected in a session, then remember user-overridden shape choices per lattice board for that session.
- `Attack-set`, `Color`, and `Color-Attack-set` enemy modes must be available for lattice and continuous boards.
- Shape changes on LatticeSquare, LatticeHex, and LatticeTriangle are render-only and must not reset the worker simulation.
- Piece Radius changes on lattice boards are render-only and must not reset the worker simulation. Piece Radius changes on ContinuousArchimedean must reset because they change placement validity.
- Radius edits update Fit Screen immediately, but worker Radius commits are debounced for 2 seconds. Start, Pause, and Step force any pending Radius commit immediately. Compatible higher `SpiralPath` radius commits update the worker in place and resume from the first previously out-of-radius spot; lower radius changes and `CenterDistance` radius changes reset/stage generation because Radius limits generation and can change the valid search surface.
- Do not auto-expand Fit Screen to the placement extent, because that makes the user-entered Radius appear ignored.
- The untouched default simulation auto-starts so first load is nonblank.
- Simulation-affecting setting edits should stage the next generation on a fresh worker without immediately clearing the current canvas/log snapshot. Keep the old snapshot visible at reduced saturation while incompatible settings are staged, restore full saturation if the settings become compatible again, and clear only on Refresh or when Start/Step begins the staged generation.
- `Visual Progress` is enabled by default. When disabled, the worker suppresses live vertices/logs and sends final render/log data only on completion or after a long silent work slice yields. Re-enabling Visual Progress during a silent run must cancel/pause that run cleanly rather than corrupting later visual runs.
- The Refresh button terminates active worker work, recreates worker/render state, pauses, clears stale in-flight messages, and preserves current settings and custom pieces. Reselecting the current Board Type should refresh the same way without changing board selection.
- Canvas panning uses left mouse drag in `1:1 Pixel`; mouse wheel adjusts Zoom around the cursor.
- When `Attacking` is enabled, status text should include active rejection counts so Rule B behavior is visible when it is actually rejecting candidates.
- Keep the placement log wired. It must show settings, first placements, latest placements, exact coordinates, pieces, color groups, and color rules, and it must be downloadable.
- Spiral Track opacity is render-only and must not reset the worker simulation.
- Spiral Track geometry must draw exact adjacent lattice segments through normal high radii such as `150`. For extreme radii, sampling/capping must still span the full requested radius rather than stopping early at the cap.
- Image downloads must render from the placement/vertex data into a deterministic pixel export, not copy the currently displayed viewport canvas. Full PNG preserves strict deterministic scale, regular PNG caps non-square piece diameter to about 2 output pixels, and JPEG 1/2 intentionally uses half resolution. LatticeSquare Square PNG exports use one cell per pixel and nearest-neighbor sampling; non-square, hex, triangle, and continuous exports use smoothed supersampled rasterization. Strict full-scale exports that exceed browser limits must show a visible status error rather than silently doing nothing. Download filenames should include the render settings and placement count.

## Editor Configuration

WASM-only files are guarded with `#[cfg(target_arch = "wasm32")]`. The repository includes `.vscode/settings.json` with `rust-analyzer.cargo.target = "wasm32-unknown-unknown"` so rust-analyzer does not mark app, worker, UI, and renderer code as inactive.

## Documentation Rule

After any behavior, architecture, UI, or build-flow change, update `MANUAL.md`. Keep `README.md` concise and linked to `MANUAL.md`; do not edit the README "Inspired by" section. Keep this `AGENTS.md` aligned when architectural constraints or invariants change.

## Verification

Before finishing meaningful changes, run:

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo build --target wasm32-unknown-unknown
trunk build --release
```

For frontend or rendering changes, also run the app with `trunk serve --port 8080` and verify in a browser that:

- WebGL canvas rendering is visible and nonblank.
- Start/Pause remain responsive during Fastest mode.
- Radius-bounded runs complete instead of continuing to generate outside the radius.
- Shape changes and lattice Piece Radius changes redraw existing pieces without resetting placement counts.
- Spiral Track changes redraw without resetting placement counts.
- LatticeHex and ContinuousArchimedean custom runs with `Attacking` enabled show nonzero active rejection counts when the placing piece can actually hit enemies.
- LatticeTriangle custom runs render and log exact triangle coordinates.
- Refresh after a silent ContinuousArchimedean prime run leaves the app responsive without requiring a browser reload.
- ContinuousArchimedean Prime Knight and Prime Gap Knight with `Color` progress past the old early stalls and skip only candidate-independent impossible spots.
- ContinuousArchimedean Prime Knight with `Attacking` enabled progresses and logs `attacking=true`.
- LatticeHex Prime Knight and Prime Gap Knight with `Color` progress past the old early stalls.
- Attack-set prime modes skip passively attacked spots instead of silently testing an impossible candidate pool forever.
- Custom army rows show order colors and are draggable.
- Deleting the last custom piece leaves one empty placeholder row.
- Placement logs include exact coordinates for early and latest pieces.
- Browser console has no errors.
