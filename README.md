# Spiral Pattern Generator

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
```

## Architecture

- `src/bin/app.rs`: main-thread WASM entrypoint.
- `src/bin/worker.rs`: worker WASM entrypoint.
- `src/ui`: direct `web-sys` control binding and worker message dispatch.
- `src/render`: WebGL point-sprite canvas renderer and image export.
- `src/render_data.rs`: shared render-data packing for worker-side WebGL vertices.
- `src/engine`: deterministic worker-owned simulation engine and spatial indexes.
- `src/math`: square, hex, continuous spiral geometry, root solving, and collision predicates.
- `src/protocol.rs`: typed `serde` contracts for app/worker messages and placement data.

Trunk builds two Rust assets from `index.html`: `spg_app` as the main WASM binary and `spg_worker` as a worker binary with a loader shim. The main thread starts `spg_worker_loader.js`, which imports the generated worker glue and initializes the worker WASM.

The run loop is a main-thread pull loop with at most one worker calculation batch in flight. After a batch arrives, the main thread appends the packed vertices and requests the next worker batch, while canvas drawing is coalesced onto `requestAnimationFrame`. This keeps the worker from pushing an unbounded queue while preventing WebGL redraws from throttling calculation throughput.

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

The hex spiral uses axial coordinates with cube-consistent directions. It emits the origin first and then complete rings with counts `6r`, giving cumulative ring sizes of `1, 7, 19, 37, ...`.

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
- ContinuousArchimedean uses center radius: `sqrt(x*x + y*y) <= radius`.

## Piece Radius, Collision, And Attacks

The UI has a Piece Radius slider. Lattice boards default to `0.50`; switching to `ContinuousArchimedean` sets the default to `0.25` so unit-spaced centers leave visible space between bodies.

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

The renderer uses WebGL point sprites on the same canvas. Each placement contributes one packed vertex of world position and RGB color. The shader handles square versus circle shape, including circular discard via `gl_PointCoord`.

The worker computes vertex positions and RGB colors once per emitted batch. The main thread appends or replaces the already-packed vertex buffer and uploads it to WebGL incrementally. Appended batches use `bufferSubData`; every visible `requestAnimationFrame` redraws the full uploaded point buffer in one `drawArrays(POINTS)` call. The full redraw is required because a normal WebGL canvas is not a persistent retained framebuffer: relying on previous frames to keep old point sprites can make older pieces disappear after browser compositing or GPU buffer growth. Shape, zoom, display-mode, and lattice Piece Radius changes reuse the existing vertex data. This avoids the slow path of issuing one Canvas 2D call per piece from WASM, avoids repeatedly parsing color strings or rebuilding all vertices on every animation tick, and avoids re-uploading the whole simulation for every batch.

Image export intentionally does not download the displayed viewport canvas. The PNG/WebP buttons render a deterministic offscreen pixel canvas from the current vertex buffer:

- LatticeSquare with Square shape exports one board cell per image pixel across the requested Radius bound.
- LatticeHex, ContinuousArchimedean, and Circle shape exports use four pixels per world unit so non-orthogonal centers and circular bodies can be rasterized without viewport compression.
- Export bounds are based on the requested Radius, not the current browser viewport or Fit Screen scale.
- File names include artifact type, board, army preset, enemy mode, shape, radius, piece radius, attacking state, completion state, and placement count.

## UI Notes

- Radius is a typed generation and view-bounding input; Piece Radius is a separate slider.
- Fastest is the default speed mode.
- Step advances one placement for precise inspection.
- Start runs large pulled worker batches; Fastest uses 4096 placements or 1,000,000 candidate-work units per batch and yields between batches. Pause stops future batch requests after the current worker batch returns.
- Shape is forced to Circle for `ContinuousArchimedean`.
- On lattice boards, changing Shape or Piece Radius redraws current pieces without resetting the worker simulation. In ContinuousArchimedean, changing Piece Radius resets because it changes collision and attack validity.
- The `Attacking` toggle resets the simulation because it changes Rule B. When enabled, status text includes active rejection counts so the UI shows when proactive attacking is affecting candidates.
- The placement log records settings, Radius, Piece Radius, anchor colors, first placements, latest placements, exact coordinates, pieces, color groups, and color rules. The worker sends only first/latest log samples, and the DOM text refresh is throttled to keep Fastest mode responsive. The Log export downloads the same inspection data with a settings-rich filename.
- Changing display mode, zoom, shape on lattice boards, piece radius on lattice boards, or anchor colors does not reset the worker simulation.
- Editing Radius updates Fit Screen rendering immediately but does not reset the worker on every keystroke. The worker keeps its last committed Radius until the field has been stable for 2 seconds, or until Start, Pause, or Step is clicked, then the simulation resets to the committed Radius because Radius limits generation.
- Changing board, continuous piece radius, rules, offset, army preset, custom army, or prime divisor resets the simulation because those inputs alter placement validity or generation bounds.
- Fit Screen maps the requested Radius to the viewport. It does not auto-expand to include every placement.

Continuous passive and proactive attack checks use the continuous spatial hash to probe only centers that can fall inside the relevant body or attack-ring radius, then apply the exact thin-ring predicate. This avoids scanning every previously placed continuous piece for every candidate while preserving the mathematical rule.

## Editor Notes

The WASM entrypoints and UI modules are guarded with `#[cfg(target_arch = "wasm32")]`. If rust-analyzer is checking the host target, it may show “inactive code” on `src/bin/app.rs`, `src/bin/worker.rs`, and the WASM-only modules. `.vscode/settings.json` sets `rust-analyzer.cargo.target` to `wasm32-unknown-unknown` so those files are analyzed as active WASM code.

## Verification

Current verification suite:

```powershell
cargo fmt --check
cargo test
cargo build --target wasm32-unknown-unknown
trunk build --release
```

Browser smoke checks cover Radius-bounded completion, Shape and lattice Piece Radius redraws without reset, ContinuousArchimedean and LatticeHex custom runs with visible active rejection counts, ContinuousArchimedean Prime Knight and Prime Gap with `Color`, LatticeHex Prime Knight and Prime Gap with `Color`, Piece Radius defaulting to `0.25` on continuous boards, candidate-independent skipped spots in prime modes, deleting all custom pieces, placement logs with exact coordinates, custom order color rows, draggable row attributes, WebGL rendering, Start/Pause responsiveness, and absence of console errors.
