# Engine Audit

## 1. Initial state

- The project is a single Rust binary using `wgpu`, `winit`, egui, glTF/OBJ import,
  scene serialization, PBR lighting, IBL, shadows, skinning, and animation.
- The baseline `cargo check` passed.
- The baseline `cargo test` passed all 94 tests.
- The baseline `cargo clippy --all-targets --all-features` passed with warnings.
- Strict `cargo clippy --all-targets --all-features -- -D warnings` identified
  43 warnings, mostly Clippy style warnings and functions with many arguments.

## 2. Architecture

- `src/app.rs` owns the application loop, renderer lifetime, world, animation
  state, and imports.
- `src/scene/` owns scene objects, model hierarchies, transforms, and scene I/O.
- `src/renderer/` owns GPU resources, render pipelines, meshes, materials,
  glTF/OBJ loading, environment processing, and debug UI.
- The current design is functional but has a large application/renderer/UI
  coupling surface. `Renderer` and `DebugUi` contain substantial state and
  orchestration that could eventually become separate subsystems.

## 3. Findings

### P0 - Stability

- GPU adapter/device/window-surface initialization previously used `unwrap` or
  `expect`, causing process-level panic on unsupported or unavailable devices.
- The main event loop and window creation also used panic-based error handling.
- The embedded default material could panic while initialization was performed.

### P1 - Maintainability

- Strict Clippy reports style warnings across `app`, `debug_ui`, `gltf`,
  `environment`, `material`, `mesh`, `scene`, and `world`.
- Several texture/material constructors have many arguments. These remain
  functional but would benefit from texture and image descriptors.
- Several nested `if` expressions can be expressed as guard combinations.
- `Renderer` is a large state owner for pipelines, buffers, textures, caches,
  settings, and asset loading.

### P2 - Performance

- Hierarchical world matrices are recomputed through the world hierarchy during
  scene traversal/render preparation. A cached local/world transform pass would
  improve large scenes.
- Shadow passes rebuild/copy cached instance and skinning data. Cache validity
  should remain explicit and isolated from scene and render-state updates.
- Hash-map instance batching is reasonably bounded for the current renderer, but
  batching vectors should be represented by dedicated batch structures for
  future sorting and draw-count budgets.
- Background environment/model loading is already separated from the render
  thread; polling and receiver ownership should remain a cross-system boundary.

### P3 - Rendering

- The renderer has PBR, IBL, direct light, shadows, HDR/ACES, MSAA, skinning,
  glass refraction, UV transforms, transmission, and emissive features.
- Screen-space glass is explicitly approximate and requires a complete opaque
  scene buffer for useful refraction.
- Point and spot lights, clearcoat, sheen, cascaded shadows, TAA, SSR, and
  GPU-driven culling are not implemented and should not be added without
  workload evidence.

### P4 - Project organization

- The project has a strong unit-test layer (94 tests), including hierarchy,
  serialization, animation, image, mesh, shader-layout, and MSAA tests.
- There is no separate benchmark or profiling harness. Frame timings in the UI
  are useful, but automated CPU/GPU regression benchmarks are still missing.
- License metadata and a public API policy are not yet established.

## 4. Implemented improvements

### `src/main.rs`

- Converted `EventLoop::new` and `run_app` from panic-based unwrap calls to
  `Result`, allowing startup failures to be reported by the process entry point.

### `src/app.rs`

- Renderer initialization now matches `Result` and exits cleanly with an error
  message instead of continuing with a missing renderer.
- Collapsed the nested animation-clip guard into a clear combined condition.

### `src/renderer/state.rs`

- Changed `Renderer::new` to return `Result<Self, String>`.
- Added explicit error mapping for surface creation, adapter lookup, device
  creation, and embedded default-material creation.
- Preserved the existing renderer construction and environment-loading behavior.

## 5. Verification

- `cargo fmt --all`: passed.
- `cargo check`: passed.
- `cargo test`: passed, 94 tests.
- Standard `cargo clippy --all-targets --all-features`: passed with warnings.
- Strict Clippy remains red due to style debt and large constructor signatures.

## 6. Remaining priorities

1. Replace remaining process-critical `unwrap`/`expect` paths with explicit
   fallible APIs or invariant comments.
2. Introduce small resource descriptors for material texture creation to reduce
   constructor arity and accidental parameter mismatch.
3. Split world transform propagation into a cached transform pass.
4. Add dedicated render-batch structures and optional CPU/GPU profiling counters.
5. Add benchmark coverage only after a stable scene workload is available.

## 7. Phase 2 status

### Cached transform system

- `World` now caches local and world matrices for model-group nodes and objects.
- Group mutations mark the node and all descendant nodes dirty.
- Object mutations invalidate object world-matrix caches.
- Hierarchy changes, parent reparenting, removal, duplication, and direct
  transform mutations now invalidate only the affected descendants.
- World/object matrix cache entries are also checked against the actual
  local-matrix value, preventing stale matrices after direct mutable access.
- Added tests for nested propagation, cache reuse, and removal of cached parent
  descendants.

### Render profiling

- Added renderer counters for measured draw calls, triangles, active meshes,
  active materials, batches, and instances.
- Added a profiler section to the egui debug UI.
- GPU frame time remains explicitly `unavailable` until timestamp queries are
  implemented; no fake GPU timing is reported.

### Verification

- `cargo fmt --all`: passed.
- `cargo check`: passed.
- `cargo test`: passed, 97 tests.
- Strict Clippy cleanup is in progress; the remaining 42 warnings are tracked
  as maintainability debt and are being fixed incrementally without `allow`.

## 8. Phase 3 status

### Critical rendering and Release diagnosis

- Release build is available and has been verified with
  `cargo build --release`.
- Corrected frustum plane extraction to use matrix rows and the wgpu
  zero-to-one clip-depth range. The previous column-based planes could falsely
  reject visible meshes; this shared culling path also applied to transparent
  and transmission objects, including the default glass sphere.
- Added frustum tests for near/far clipping, full camera yaw, the default floor,
  large scene bounds, and the glass-sphere bounds. These validate the culling
  math, not a live rendered image.
- Renderer scene preparation now retrieves each object's stable ID by index
  rather than scanning all objects by pointer for each rendered object,
  removing an O(N^2) lookup pattern.
- Removed an unnecessary full-world transform-cache traversal from
  `World::update`, reducing per-frame scene work to animation and cube updates.
- The glass shader's screen-space sampling and render-pass setup were inspected;
  no independent shader/pass defect has been established or changed.
- GPU timestamp support is not yet available in the active render path.
  GPU frame time therefore remains reported as unavailable.

### Added verification

- `cargo fmt --all -- --check`: passed.
- `cargo check`: passed.
- `cargo test`: passed, 101 tests.
- Release build passed with `cargo build --release --target-dir target\phase31`.
  The normal `target\release` executable was open in the app, so the standard
  command could not replace it; the isolated build directory was cleaned up.
- `cargo clippy --all-targets --all-features -- -D warnings`: still fails on
  existing Clippy findings, including excessive function arguments and
  collapsible conditionals.
- The Release executable launched successfully, but the UI capture facility
  could not capture its rendered surface. Therefore camera rotation, resize,
  long-duration visual behavior, and live FPS could not be verified here.
- No before/after FPS or CPU/GPU frame-time measurements are available. The
  O(N^2) lookup removal is a concrete complexity fix, but is not evidence that
  it caused the reported 10 FPS on the small default scene or that FPS improved.

## 9. Phase 3.1 status

### Glass and shadow-path changes

- Transparent and transmission pipelines no longer unconditionally cull
  backfaces. The existing fragment shaders already honor `doubleSided` by
  flipping back-face normals or discarding them when the material is
  single-sided; pipeline culling had prevented that shader logic from running.
  Opaque rendering retains backface culling.
- Transparent objects are sorted back-to-front by camera-space depth of the
  transformed mesh-bounds center, rather than radial distance from the object
  origin.
- Rendering is split into alpha transparency and transmission glass passes.
  The opaque plus alpha-transparent result is copied into the glass sampling
  texture before transmission is rendered, so alpha-blended cabin geometry is
  available to the windshield shader. All transmission panes sample this same
  snapshot, avoiding recursive/double screen-space refraction. Glass depth
  writes retain the nearest pane at each pixel.
- Shadow casters are now collected independently of camera-frustum visibility.
  The shadow pass renders its own stable opaque batches, while camera culling
  continues to limit the main scene pass. This prevents off-screen vehicle
  parts from being dropped as shadow casters. Shadow instances use a separate
  GPU buffer and are uploaded only when the shadow cache is invalidated.
- Alpha-blended, non-transmissive materials are now included in the shadow
  pass; the existing shadow fragment shader uses the material alpha cutoff to
  create cutout shadows. Transmission glass remains excluded.
- The supplied `2021_mercedes-benz_amg_gt_black_series.glb` was inspected from
  Downloads. It has separate inner/outer window meshes, double-sided glass
  materials, two `KHR_materials_transmission` panes (`transmissionFactor=1`,
  `IOR=1.5`, base alpha `0.25`), and alpha-blended cabin materials. The cabin
  was absent from the glass shader's opaque-only scene-color snapshot, which
  explains why the interior could disappear from outside.
- The glass shader's fixed refraction ray segment remains unchanged; its
  screen-space displacement is clamped to two internal-resolution pixels.
  Multi-pane sampling and double-sided rasterization were corrected, but the
  remaining perceived magnification requires visual confirmation on this car.

### Verification

- `cargo fmt --all -- --check`: passed.
- `cargo check`: passed.
- `cargo test`: passed, 104 tests, including shadow-queue eligibility and
  transparent-depth sorting cases.
- `cargo build --release`: passed.
- Strict Clippy remains failing: 29 errors for the binary and 36 in the test
  target, mostly existing style and API-shape findings.
- The graphics capture API could not capture the running engine window.
  Automotive glass behavior, wheel/body shadows, Default Scene visual
  regression checks, and runtime FPS therefore remain visually unverified.
