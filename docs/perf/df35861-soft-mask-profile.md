# DF35861 soft-mask profile

**2026-10-15 · Status: measured native profile, uncommitted prototype.**
Branch: `perf/soft-mask-profile`; baseline:
`f90c755a93139307f29230c13d56b6afb36d5b67`, including the hayro pending-clip fix.
The customer PDF remains local and untracked. No customer artwork or derived
fixtures were added. No Adobe software/resources were used.

## Finding

**Full-viewport opacity-mask generation dominates redraws.** Each of the 92
masks creates an offscreen context/RGBA pixmap and converts every viewport pixel
to luminance coverage, every redraw. At 1440×900 this implies **119,232,000
coverage pixels**, although the combined padded mask-art bounds cover only
**564,943 pixels (0.47%)**. At 2880×1800 the full-buffer work quadruples.

A three-second macOS CPU sample at 2880×1800 put **2,057 of 2,104 main-thread
samples (97.8%) inside `Renderer::mask_values`**, primarily coverage conversion
and offscreen rendering. This is main-thread attribution, not total worker CPU.
The raw sample remains at `/tmp/vc-render-sample.txt`.

The prototype bounds mask rasterization/conversion while retaining the existing
full-size coverage array. It fills the remainder with the transparent-pixel
coverage, respecting Clip/Invert; unknown/nonfinite bounds use the original
path. It preserves editable objects and rendering resolution.

## Method and results

Release build, macOS, Apple M4 Pro, 12 cores, 24 GiB RAM. **Native shared render
path, not a browser trace.** Chrome exists, but Trunk, rustup and the WASM target
are absent; local `plan/` notes are also absent. The Homebrew toolchain only has
the native target.

The reusable CLI example opens through the headless backend, fits the first
artboard with 5% margin, enables canvas image mipmaps/progressive placement,
uses zero raster worker threads (matching web), and sets the image-cache budget
to the web default, 256 MiB. The import produces **755 tree nodes, 92 masks,
91 image nodes and 129 containers**; PDF object/OC marker counts are not tree
node counts.

Times below are **ms**. Redraw, pan, zoom and move rows are medians of seven
frames. Open, first render and selection are single observations. Pan steps are
3 output pixels; zoom steps are 2.5% about the viewport centre. Selection and
20-point moves use actual commands on the largest editable masked object.
Pixel hashes are calculated outside timed rendering. Measurements exclude GPU
upload/presentation, browser scheduling, panels and pointer hit testing.

| Operation | Before 1440×900 | Prototype 1440×900 | Before 2880×1800 | Prototype 2880×1800 |
|---|---:|---:|---:|---:|
| Open/import | 37.49 | 35.14 | 19.87 | 19.86 |
| First render, cold image cache | 154.40 | 19.38 | 590.11 | 28.43 |
| Same-view redraw | 128.55 | 6.08 | 582.67 | 35.92 |
| Pan redraw | 127.99 | 6.00 | 582.92 | 36.15 |
| Zoom redraw | 128.39 | 6.53 | 584.97 | 37.81 |
| Select command | 0.184 | 0.189 | 0.065 | 0.066 |
| Move command, excluding render | 0.056 | 0.036 | 0.053 | 0.048 |
| Redraw after moving one object | 127.82 | 6.02 | 581.27 | 36.83 |

Redraw improves **21.1× / 16.2×**. All **58 frame hashes match** across cold,
redraw, pan, zoom and move frames at both sizes. The prototype still misses a
16.7 ms frame budget at Retina resolution. A 1440×900 CSS canvas at DPR 2 can
require the larger render size; the Reporter's actual iframe size was not
measured.

Builds were stopped during the paired table runs. The host is shared (an earlier
load reading was 6.86 on 12 cores), so these are not isolated-machine guarantees.
Warm-redraw ranges were 127.93–139.43 → 5.82–7.60 ms and
576.99–586.85 → 21.49–36.42 ms respectively. A later repeat measured
854.60 → 70.29 ms at 2880×1800: absolute latency varies materially, but the large
speedup persists. Cold versus warm buffer residency/allocation also differs.

**Idle:** the verified headless canvas test rendered a 1440×900 texture with
1,345 drawn nodes, then measured **0.002 ms median over 50 idle frames, with
zero texture uploads**. This is canvas CPU after the prototype, not full-app or
GPU cost. The unchanged baseline cache logic also skips idle rasterization.

## Attribution

- **Decoding:** already cached by image content in `crates/render/src/images.rs`.
  Warm cache usage is about **10 MiB**, well below 256 MiB. Re-caching JPEG
  decoding would miss the repeated-mask bottleneck.
- **Masks:** offscreen mask contexts are single-threaded even when the main
  rasterizer has workers. Baseline diagnostic ablations at 1440×900 give
  **1.66 ms with masks removed**, **1.92 ms with image nodes hidden** (also
  skipping their masks). These deliberately change appearance; they are not fixes.
- **Shading, clipping, OC:** the ablations retain the remaining shading/clip
  tree. Path/clip geometry already has caches. These and the OC-derived
  containers are not the dominant cost at the measured views.
- **Invalidation:** `ui-egui/src/canvas.rs` caches the completed bitmap by
  document revision/view/options. Pan, zoom and edits redraw the visible
  document; there is no dirty-region redraw. Idle reuses the bitmap.
- **CPU/GPU/web:** wgpu displays the CPU-produced bitmap; it does not rasterize
  these masks. `render_worker::Worker::spawn` returns `None` on wasm, so slow
  renders block the web UI thread. Native heavy documents can render on a worker
  and reproject the preceding frame. Native responsiveness is not browser FPS.

## Fixes ranked by gain/risk

1. **Bound mask rasterization/conversion — implemented prototype.** Large
   measured gain, small change, no new persistent cache or invalidation scheme.
   Generic conservative bounds/edge cases still deserve review before shipping.
2. **Bound the coverage representation and compositing region.** The prototype
   still retains one byte per viewport pixel per mask until compositing: roughly
   477 MB for 92 masks at 2880×1800, before other buffers. Local/tiled coverage
   would reduce allocation/memory traffic. Medium risk; requires backend work.
3. **Cache image+mask or mask coverage in local coordinates, with zoom tiers.**
   Further pan/drag gains are plausible, not measured. Correct keys must include
   art/image content, transforms/linking, colour/document dependencies and
   resolution; preserve blending and mask editing. Medium/high complexity and
   memory risk. Do not destructively flatten the editable document.
4. **Web worker plus last-frame reprojection.** Improves responsiveness without
   reducing render work. Requires browser worker/data-transfer architecture;
   native threads are not a drop-in web solution.
5. **Dirty tiles/partial redraw or GPU vector rendering.** Broad potential,
   highest complexity around backdrop blending, clips, effects and invalidation.
   Additional shading tessellation caches are low priority for this file.

## Changes and validation

- `crates/render/src/lib.rs`: cropped mask work; original full-viewport routine
  retained as fallback/reference.
- `crates/render/src/tests.rs`: **120 reference combinations** covering
  Clip/Invert, smooth/hard edges, pan/scale/rotation/offscreen art, blur and feather.
- `apps/vectorcraft-cli/examples/soft_mask_profile.rs`: timing/ablation/hash harness.
- `crates/ui-egui/src/canvas.rs`: opt-in local-file idle test, ignored normally.

**Passed:** renderer suite (**260 passed, 2 ignored**), verified idle test,
workspace formatting, renderer/CLI all-target Clippy with `-D warnings`.

The synthetic **50,000-path performance budgets passed before and after**.
Fit/pan/400%-zoom: 38.06/36.96/8.51 ms before, 49.15/44.89/12.87 ms after.
This mask-free corpus does not exercise the changed path; timing variation is
material. Hit test, save/open, SVG, Pathfinder and handle/segment drag also passed.

**Limits:** combined UI Clippy encounters unchanged HEAD warnings at
`shortcuts.rs:91` (`type_complexity`) and `thumb_worker.rs:130`
(`manual_range_contains`). WASM check fails with E0463 because its target
libraries are absent. Full workspace CI and browser execution were not completed.
Peak-RSS comparison was unavailable: sandboxed `time -l` could not read
`kern.clockrate`; buffer arithmetic above is not a measured peak-memory claim.

## Reproduce

Run benchmarks serially after builds stop. Baseline needs the pre-change renderer;
the locally saved baseline executable is `/tmp/vc-soft-mask-before`.

```sh
cargo build --release -p vectorcraft-cli --example soft_mask_profile
# Repeat with 2880 1800; modes: normal, no-masks, no-images.
target/release/examples/soft_mask_profile ./DF35861_customer.local.pdf 1440 900 0 normal
cargo run --release -p vectorcraft-cli -- bench ./DF35861_customer.local.pdf --size 1440x900 --iters 5
cargo run --release -p vectorcraft-cli -- perf --paths 50000
cargo test --release -p vectorcraft-render
VECTORCRAFT_PROFILE_FILE="$PWD/DF35861_customer.local.pdf" cargo test --release -p vectorcraft-ui-egui profile_local_canvas_idle -- --ignored --nocapture
```

Nothing was staged, committed or pushed. This prototype does not change shipped
parity/readiness scores. Revision history: 2026-10-15, initial investigation.
