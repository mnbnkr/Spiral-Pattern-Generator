# Spiral Pattern Generator (in alpha)
<br>
Inspired by <i>- Numberphile's 'Red & Black Knights' <a href="https://youtu.be/UiX4CFIiegM">video</a>, - Jonas Karlsson's <a href="https://jonka364.github.io/stendhal/stendhal.html">work</a>, - and Pitouli's 'Knights' <a href="https://github.com/Pitouli/knights">web application</a>.</i>

<br><br>

Rust/WASM application for deterministic spiral pattern simulations. The app uses Trunk, `wasm-bindgen`, `web-sys`, a dedicated Web Worker for simulation, and a WebGL renderer on one HTML5 `<canvas>`.

## Toolchain

- Rust `1.96.0-beta.6`
- Target `wasm32-unknown-unknown`
- Trunk `0.21.14`
- No npm, Node, React, Yew, or JS bundler.

Useful commands:

```powershell
cargo fmt --check
cargo test
cargo build --target wasm32-unknown-unknown
trunk build --release
trunk serve --port 8080
pnpm exec playwright test --project=chromium
```

## Architecture

- `src/bin/app.rs`: main-thread WASM entrypoint.
- `src/bin/worker.rs`: worker WASM entrypoint.
- `src/ui`: direct `web-sys` control binding and worker message dispatch.
- `src/render`: WebGL point-sprite canvas renderer and image export.
- `src/render_data.rs`: shared render-data packing for worker-side WebGL vertices.
- `src/engine`: deterministic worker-owned simulation engine and spatial indexes.
- `src/math`: square, hex, triangle, continuous spiral geometry, root solving, and collision predicates.
- `src/protocol.rs`: typed `serde` contracts for app/worker messages and placement data.

Trunk builds two Rust assets from `index.html`: `spg_app` as the main WASM binary and `spg_worker` as a worker binary with a loader shim. The main thread starts `spg_worker_loader.js`, which imports the generated worker glue and initializes the worker WASM.

`Trunk.toml` uses a relative default `public_url` so local static builds are path-portable. GitHub Pages deployment is handled by `.github/workflows/pages.yml`, which builds with a repository subpath public URL and uploads the generated `dist` assets through the Pages artifact flow.

The default visual run loop is a main-thread pull loop with at most one worker calculation batch in flight. After a batch arrives, the main thread appends the packed vertices and requests the next worker batch, while canvas drawing is coalesced onto `requestAnimationFrame`. This keeps the worker from pushing an unbounded queue while preventing WebGL redraws from throttling calculation throughput. When `Visual Progress` is disabled, the worker runs silently and sends final vertices/log samples only when the radius-bounded run completes or a long silent work slice yields.

Worker messages use `bincode` over transferable `Uint8Array`, not JSON strings. Each worker batch includes only the placement samples needed for the first/latest log display, plus a packed `[x, y, r, g, b]` `Vec<f32>` for direct WebGL upload. Most batches append vertices. Prime Gap recoloring and anchor-color changes send a full replacement vertex buffer so existing pieces stay visually consistent without making the main thread recompute all colors every frame.

## Engine Modes

### Custom Finite Army

Custom mode uses spot-seeking placement. The army is an ordered cyclic list of user-defined `(a,b)` pieces. For each turn, the current army piece scans the spiral outward from its own remembered scan cursor and chooses the lowest still-possible valid spot. Invalid spots remain empty. An empty custom army is allowed as an editing state and produces no placements until a piece is added.

Each custom army row is draggable and can also be reordered with buttons. Color is attached to list order, not permanently to a piece: the first row uses Anchor A, the last row uses Anchor B, and rows between them use a rainbow hue path between the anchors.

### Prime Knight And Prime Gap

Prime modes use piece-seeking placement. The engine visits spots sequentially and tries the lowest unused generated prime piece until one fits.

- Prime Knight: `(1, p_n)`, starting `(1,2), (1,3), (1,5), ...`.
- Prime Gap: `(p_n, p_{n+1})`, starting `(2,3), (3,5), (5,7), ...`.

With `Color` enemy mode, prime modes keep searching the infinite pool for the current spot when a future candidate color can still solve it. They still skip candidate-independent impossible spots: for example, a spot already attacked by multiple enemy color groups, or a Prime Gap spot that requires the one already-consumed gap-1 piece. With `Move-set` enemy mode, every prime move-set is unique, so a passively attacked spot cannot be fixed by trying a larger candidate. In those cases the engine skips the spot and continues instead of testing forever.

## Rule Engine

- Rule A, passive attacked-by: a candidate is invalid if the target spot is attacked by an existing enemy piece.
- Rule B, proactive attacking: when enabled, a candidate is also invalid if it attacks any existing enemy piece.
- Enemy mode `MoveSet`: pieces are enemies when normalized `(abs(a), abs(b))` move groups differ.
- Enemy mode `Color`: pieces are enemies when their color groups differ.

Lattice boards maintain an occupied-cell map and an attack map keyed by integer coordinates. The hex leaper attack set is generated from cube rotations of `(a,b)` and the optional swapped vector `(b,a)`, giving six or twelve directions instead of over-counted sign variants.

Continuous mode uses a spatial hash for local body collision checks. Attack-circle checks are evaluated algebraically against placed pieces because very large prime attack radii make center-proximity hash queries inefficient.

## Mathematical Core

### Square Lattice

The square spiral iterator matches the supplied Python generator:

```text
(0,0), (1,0), (1,1), (0,1), (-1,1), (-1,0), ...
```

### Hex Lattice

The hex spiral uses axial coordinates with cube-consistent directions. It emits the origin first, then starts immediately to the visual right and walks counterclockwise through adjacent cells. Ring transitions remain adjacent, complete rings have counts `6r`, and cumulative ring sizes are `1, 7, 19, 37, ...`.

### Triangle Lattice

The triangle board uses one fixed orientation of equilateral triangle cells as visible placement spots. The visible spot centers form a triangular axial lattice, and the spiral starts at the origin, moves immediately to the visual right, then turns 120 degrees left at triangular-number corners. Segment lengths are `1, 2, 3, 4, ...`, so the first spots are `0`, then `1` to the right, then two steps on the next vector, three on the next, and so on. The opposite-orientation triangles in the tiling are hidden from placement but are counted by triangle attack stepping.

Triangle attacks use three primary corner-aligned `A` rays. After walking through the alternating triangle tiling, the `B` leg resolves to the two nearest visible same-orientation cells perpendicular to that ray. `(1,1)`, `(2,1)`, and `(3,1)` each produce six visible attack targets after deduplication.

### Continuous Archimedean Spiral

The continuous board uses:

```text
r(theta) = theta / tau
x(theta) = r(theta) cos(theta)
y(theta) = r(theta) sin(theta)
```

One full revolution increases radius by exactly `1.0`. Consecutive continuous spots are generated by solving for the next `theta` whose Euclidean chord distance from the current center is exactly `1.0`. The solver uses Newton-Raphson with an analytic derivative and a bracketed bisection fallback.

The offset setting is interpreted as the first center shifted by `0.0..1.0` units of arc length from the origin along the spiral. Subsequent centers still use exact unit chord spacing.

The chord solver uses Newton-Raphson with a bracketed bisection fallback. At large `theta`, f64 midpoint rounding can stop shrinking the bracket before an absolute angle tolerance is reached, so the fallback tracks the best distance error and accepts a relative-angle precision limit instead of falsely ending the continuous iterator. This keeps high-Radius runs moving well past the old radius-100 failure zone.

## Radius Bound

`Radius` is both the Fit Screen view bound and the simulation generation bound. The worker stops with a `Complete` status when the board iterator leaves the requested radius.

- LatticeSquare uses Chebyshev spiral ring radius: `max(abs(x), abs(y)) <= floor(radius)`.
- LatticeHex uses cube/hex ring radius: `max(abs(x), abs(y), abs(z)) <= floor(radius)`.
- LatticeTriangle uses the triangular spiral shell index: shell `0` is spot `0`, shell `1` is segments `1..=3` and spots `1..=6`, shell `2` is segments `4..=6`, and so on. The worker stops when the next spot's shell first exceeds `floor(radius)`.
- ContinuousArchimedean uses center radius: `sqrt(x*x + y*y) <= radius`.

## Piece Radius, Collision, And Attacks

The UI has a Piece Radius slider. Lattice boards and `ContinuousArchimedean` default to `0.50`, so unit-spaced continuous centers touch at tangency without body overlap.

Continuous body overlap is rejected when:

```text
center_distance < 2 * piece_radius - EPS
```

In continuous mode, a move vector `(a,b)` becomes an infinitely thin attack circle with radius `sqrt(a*a + b*b)`. An attack occurs when that attack circle intersects or touches the target body:

```text
abs(center_distance - attack_radius) <= piece_radius + EPS
```

## Colors

- Custom finite colors are order-based rainbow colors between Anchor A and Anchor B.
- Default anchors are orange-red `#ff7800` and red `#ff0006`. In RGB/HSL picker terms these are Anchor A `R=255, G=120, B=0, Hue=19, Sat=240, Lum=120` and Anchor B `R=255, G=0, B=6, Hue=239, Sat=240, Lum=120`.
- Prime Knight uses the modulo divisor bounce rule. For divisor `12`, buckets are `1,2,3,4,5,0,5,4,3,2,1,0`.
- Prime Gap maps the current known minimum and maximum gap to the two anchors. Existing pieces are recolored by worker-sent replacement vertex buffers when bounds expand.
- Gradients use hue traversal, not linear RGB blending, so intermediate colors form a rainbow between the anchors.

## Rendering

The renderer uses WebGL point sprites on the same canvas. Each placement contributes one packed vertex of world position and RGB color. The shader handles Square, Circle, Hex, and Triangle piece shapes through `gl_PointCoord`; Continuous Archimedean still forces Circle because its simulation bodies are circular. Triangle Lattice offers Triangle and Circle rendering. On LatticeHex, Hex shape uses a full regular hex cell scale so Piece Radius `0.50` fills adjacent hex cells without overlap. On LatticeTriangle, Triangle shape scales the default `0.50` Piece Radius to the exact same-orientation triangle size for non-overlapping contact on the triangular center lattice.

The worker computes vertex positions and RGB colors once per emitted batch. The main thread appends or replaces the already-packed vertex buffer and uploads it to WebGL incrementally. Appended batches use `bufferSubData`; every visible `requestAnimationFrame` redraws the full uploaded point buffer in one `drawArrays(POINTS)` call. The full redraw is required because a normal WebGL canvas is not a persistent retained framebuffer: relying on previous frames to keep old point sprites can make older pieces disappear after browser compositing or GPU buffer growth. Shape, zoom, display-mode, spiral-track opacity, and lattice Piece Radius changes reuse the existing vertex data. This avoids the slow path of issuing one Canvas 2D call per piece from WASM, avoids repeatedly parsing color strings or rebuilding all vertices on every animation tick, and avoids re-uploading the whole simulation for every batch.

The optional Spiral Track slider is render-only. It draws the board's underlying square, hex, triangle, or continuous spiral path as WebGL line geometry behind the point sprites and defaults to Off. Track geometry is cached and capped for high radii so toggling visibility stays responsive.

Image export intentionally does not download the displayed viewport canvas. The PNG and JPEG 1/2 buttons render a deterministic offscreen pixel canvas from the current vertex buffer and download via `toBlob` object URLs rather than synchronous base64 data URLs:

- LatticeSquare with Square shape exports one board cell per image pixel across the requested Radius bound.
- Full PNG exports preserve the original deterministic export scale.
- JPEG 1/2 exports use half the export resolution and lossy JPEG compression for smaller, faster downloads.
- LatticeHex, LatticeTriangle, ContinuousArchimedean, Circle shape, Hex shape, and Triangle shape exports use a fixed world-unit scale so non-orthogonal centers and non-square bodies can be rasterized without viewport compression.
- Export bounds are based on the requested Radius, not the current browser viewport or Fit Screen scale.
- File names include artifact type, board, army preset, enemy mode, shape, radius, piece radius, attacking state, completion state, and placement count.
- Export remains strict full-scale: if the requested deterministic export exceeds browser canvas or memory limits, the status line shows an export error instead of silently doing nothing or downscaling.

## UI Notes

- Radius is a typed generation and view-bounding input; Piece Radius is a separate slider.
- Fastest is the default speed mode.
- Step advances one placement for precise inspection.
- The untouched default simulation auto-starts so the initial canvas is nonblank.
- Start runs pulled worker batches and yields between batches when `Visual Progress` is enabled. Fastest uses smaller settings-aware batches for prime modes, especially ContinuousArchimedean prime presets, so the first visible placements arrive quickly and controls remain responsive. Pause stops future batch requests after the current worker batch returns.
- Disabling `Visual Progress` makes the worker suppress live vertices and log updates until completion or a long silent work slice. Start shows an explicit silent-run status. Re-enabling Visual Progress while a silent run is active cancels that silent worker run, pauses, and lets the next Start run visually again.
- Refresh terminates any active worker run, recreates worker/render state, clears stale in-flight messages, pauses, and preserves the current settings and custom army.
- The canvas can be panned with left mouse drag when zoomed in. In `1:1 Pixel` mode, the mouse wheel changes Zoom around the cursor.
- Shape is forced to Circle for `ContinuousArchimedean`; Triangle Lattice offers Triangle and Circle; Square, Circle, and Hex are available on square/hex lattice boards. The first switch to Hex Lattice defaults to Hex shape, and later board switches remember the user's per-session shape choice for each lattice board.
- On lattice boards, changing Shape or Piece Radius redraws current pieces without resetting the worker simulation. In ContinuousArchimedean, changing Piece Radius resets because it changes collision and attack validity.
- The `Attacking` toggle resets the simulation because it changes Rule B. When enabled, status text includes active rejection counts so the UI shows when proactive attacking is affecting candidates.
- The placement log records settings, Radius, Piece Radius, anchor colors, first placements, latest placements, exact coordinates, pieces, color groups, and color rules. The worker sends only first/latest log samples, and the DOM text refresh is throttled to keep Fastest mode responsive. The Log export downloads the same inspection data with a settings-rich filename.
- Changing display mode, zoom, shape on lattice boards, piece radius on lattice boards, or anchor colors does not reset the worker simulation.
- Editing Radius updates Fit Screen rendering immediately but does not reset the worker on every keystroke. The worker keeps its last committed Radius until the field has been stable for 2 seconds, or until Start, Pause, or Step is clicked, then the simulation resets to the committed Radius because Radius limits generation.
- Changing board, continuous piece radius, rules, offset, army preset, custom army, or prime divisor resets the simulation because those inputs alter placement validity or generation bounds.
- Fit Screen maps the requested Radius to the viewport. It does not auto-expand to include every placement.

Continuous passive and proactive attack checks use the continuous spatial hash to probe only centers that can fall inside the relevant body or attack-ring radius, then apply the exact thin-ring predicate. Prime moves whose attack radius is larger than any possible distance inside the requested generation bound are skipped from broad spatial probes, which avoids the old Continuous Prime Knight/Gap slowdown without changing the attack semantics.

## Editor Notes

The WASM entrypoints and UI modules are guarded with `#[cfg(target_arch = "wasm32")]`. If rust-analyzer is checking the host target, it may show “inactive code” on `src/bin/app.rs`, `src/bin/worker.rs`, and the WASM-only modules. `.vscode/settings.json` sets `rust-analyzer.cargo.target` to `wasm32-unknown-unknown` so those files are analyzed as active WASM code.

## Verification

Current verification suite:

```powershell
cargo fmt --check
cargo test
cargo build --target wasm32-unknown-unknown
trunk build --release
pnpm exec playwright test --project=chromium
```

Browser smoke checks cover WebGL rendering, default auto-run, early progress for ContinuousArchimedean Prime Knight and Prime Gap in Fastest mode, Shape and lattice Piece Radius redraws without reset, Hex and Triangle shape wiring, spiral-track responsiveness, compressed image export, strict export errors, panning and wheel zoom, deleting all custom pieces, order-based custom row labels, placement logs, and absence of console errors. Manual browser checks should also cover Radius-bounded completion, active rejection counts with `Attacking` enabled, candidate-independent skipped spots in prime modes, draggable custom rows, Start/Pause responsiveness, and GitHub Pages-style subpath assets.
