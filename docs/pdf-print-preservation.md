# Imported print PDFs: preservation experiment

Status: 16 October 2026, local validation branch. Production approval pending.

The branch preserves editable artwork while retaining recognized public PDF
print settings. User-owned reference artwork remains local and is not a test
fixture in this repository. Synthetic tests are original PDF constructions.

## Supported behaviour

- Separation/DeviceN bitmap samples named Cyan, Magenta, Yellow and Black map
  directly into their process channels. Reordered or absent channels are handled;
  absent channels receive zero ink. The native image encoding remains CMYK TIFF.
- Imported path fills and strokes retain PDF overprint mode 0 or 1 across native
  save and PDF export. Text overprint mode 0 remains unsupported and warns.
- Compatible PDF exports restore recognized page boxes, their original origin,
  device-space page transparency groups and embedded output intent profiles.
  This metadata is typed and stored under `pdf.printSettings.v1` in the native
  document's foreign-data map; source content streams and private application
  data are not copied.
- Separations Preview reads retained bitmap CMYK samples directly; RGB images
  are separated through the working profile. The bounded image cache includes
  selected plates and colour settings. Opacity masks retain their luminance.
- `document.exportPdf` and `document.export` accept
  `disableImageInterpolation: true` to keep bitmap and mask edges sharp, and
  `discardImportedPrintSettings: true` to explicitly omit retained settings.
  Both are exposed through `document.pdfSettings` and format options.

For a hard-edged layered separation, use PDF 1.6, `createLayers: true`,
`disableImageInterpolation: true`, no colour conversion, no downsampling,
automatic image compression (unchanged JPEGs pass through), and `advanced.overprint: "preserve"`. This is a
validation configuration, not a production recipe or automatic release.

## Bounds and limitations

Original boxes are restored only when the imported artboard frame, bleed and
printer marks have not changed. Rotated pages, non-unit UserUnit and Bounding
imports warn rather than claim preservation. PDF standards, flattening and
colour conversion take precedence and warn when retained print settings are
omitted. Explicit output-intent settings take precedence over the source intent.
Only DeviceRGB, DeviceCMYK and DeviceGray page blending spaces are retained.

Named spot bitmap channels, mixed spot/process bitmap channels, non-eight-bit
sample precision (16-bit samples are quantized to eight bits) and exact path
coordinates are not solved here. CMYK vector values and spot tints now retain
f32 precision rather than being quantized to eight-bit steps.
Separations Preview still uses the existing approximate Multiply overprint
model; preserving a PDF's page group does not make the native canvas a certified
RIP. Importing a file's output intent does not assign it as the working preview
profile. These limits matter for approving production work.

## Evidence

Original regression fixtures: `crates/pdf/src/tests_cmykimages.rs`,
`crates/pdf/src/tests_import_overprint.rs`, `crates/pdf/src/preserve.rs`,
`crates/render/src/bitmap_proof.rs`, and the command-option test in
`crates/engine/src/cmd/fileio/tests_pdf.rs`.

Local comparison of four user-confirmed PAsharp files checks all six plates
using Ghostscript at 300 dpi. All bitmap process samples, five page boxes and
embedded profile bytes survive. Plate names and active layer order survive.
Residual process-tone and edge differences prevent claiming exact equivalence.
A controlled production extraction/ganging test remains required.

## Opening and saving existing separations

Use `document.open {path, printProduction: true}` for existing print separations.
It reads all public PDF pages on their media frames, preserves optional-content
layers and print settings, defaults text to outlines, and bypasses private
editing copies, including embedded native documents. Conflicting page, crop,
layer and colour-conversion options are rejected. Normal illustration opening
keeps its existing editing-data behaviour.

Export with `document.exportPdf {path, preset: "Separation Roundtrip"}` after
saving and reopening the native document. The preset keeps PDF 1.6 layers,
overprint, hard image/mask edges, original print settings and image resolution.
CMYK remains composite artwork; spot/screen layers retain their names.

Unambiguous DeviceCMYK JPEGs with the normal PDF Decode polarity retain their
original compressed bytes alongside a lossless working image. Native save/load
preserves that attachment; a changed working image invalidates it. Automatic
compression reuses it only without conversion or resizing. Explicit ZIP,
resampling or colour conversion deliberately uses working pixels. Custom
DecodeParms, nonstandard Decode ranges and other image colour spaces do not
use this pass-through. This increases native-file size, avoiding another lossy
encoding or decoder-rounding change on export.

Headless screenshots now honour active proof/separation settings, matching the
canvas rendering path. Ordinary PNG export still exports the artwork.

Revision: 16 October 2026 — print import mode, roundtrip preset, fractional CMYK
and spot values, preserved JPEG payloads and proof-aware screenshots. Production
Switch/Indigo/screen RIP validation remains an independent acceptance gate.

Validation: 7,190 native tests passed; formatting, workspace clippy, dependency,
asset, brand, layering and no-bundled-font checks passed. All 23 wasm checks
passed using the installed Rust source with `RUSTC_BOOTSTRAP=1` and
`CARGO_UNSTABLE_BUILD_STD=std,panic_abort` (this Homebrew toolchain has no
prebuilt wasm standard library). Shared PDF tests and workspace clippy passed;
all non-UI shared-library tests passed. The shared UI graphics test could not
create a GPU adapter in the sandbox and is not claimed passed.

The release CLI passed all ten 50,000-path performance budgets after the build
finished (fit-page render 41.53 ms, native save 38.80 ms, native open 112.93 ms).

## Edge and tone follow-up — 16 October 2026

The editable route now retains fractional ink components on import, reads page
boxes in double precision, and writes path/clip coordinates and placement matrices
without first reducing them to f32. Repeated vertices are retained without the
reader's 0.0001-point raster workaround; zero-length round-capped strokes remain
PDF strokes. The shared reader/writer are pinned together at `9a7ceb6f`.

Writer-created transparency groups inherit the retained source page blending
space. Previously a CMYK working document forced intermediate groups to CMYK even
inside a source RGB page group, introducing an extra colour conversion. The
WE134155 broad one-level CMYK discrepancy disappears with this correction. Mask
luminance groups are not traversed as painted forms. A form shared by source pages
with conflicting blending spaces is refused rather than silently choosing one.
This does not yet preserve arbitrary mixed original group-level colour spaces.

No original-PDF pass-through was added. All comparisons exercise editable import,
native save/reopen and regenerated PDF export. A raster comparison can still show
isolated boundary pixels after curves/transforms are restructured; pixel identity
is not asserted. Independent PDF geometry comparison and production RIP validation
remain separate from these synthetic regression tests.

Hybrid colour policy confirmed by the user: retain RGB and source profiles, retain
CMYK and its meaning, and retain named spot inks/tints. The exact agreed CMYK
working profile is Coated FOGRA39 (ISO 12647-2:2004), not an interchangeable ECI
profile. Do not force RGB to CMYK to enable spreading; final process conversion
belongs to the configured Indigo RIP. This policy is a requirement for the maker,
not a claim that every mixed-colour import/spread/export case is already supported.

Final local corpus validation: 12 editable native roundtrips retain page boxes
and output profile inventories. WE134155 has identical CMYK raster samples at
300 dpi. Independent line/cubic control comparison measures 69,196 filled
segments on 27 technical screen plates within 1.37e-12 pt; this does not verify
clipping semantics or outlined-stroke equivalence. Across 30 screen/spot raster
plates, 277 coverage pixels differ, all on existing boundaries. Checker warning
and error findings are unchanged; PA Sharp and Esko trap edits survive native
reopen and leave WHP/WP pixels unchanged. Production RIP validation remains open.

All nine application CI gates pass (7,193 tests passed, 12 ignored; 23 wasm
checks). All ten performance budgets pass; before/after builds used different
optimization profiles, so the timings are not a controlled regression measure.
No customer PDF fixtures or rendered artwork are committed.


### 17 October 2026: explicit source-colour preservation (local qualification)

RGB and grey PDF output now retain fractional components through the shared
writer's normalized constructors. DeviceRGB import reads the original components
instead of an 8-bit display conversion. The `output.conversion: "preserveSource"`
setting retains RGB/CMYK/grey values in either document colour mode. It refuses
conflicting destination conversions, PDF/X-1a, PDF 1.3 flattening, ordinary Lab
fallback and profile conversions that would change the source RGB values. Named
spots continue using their exact names and tints. Imported print/page settings
are retained with this setting when their existing preservation checks pass.

Evidence: `tests_output::preserve_source_*`, the craft-pdf writer's fractional
RGB/grey serialization test, and Reporter's recipe-driven WE qualification
harness. The shared writer is published and pinned to immutable revision
`2bbf5d2a591f81ae3b7b088db97c646d912120ee`; no local dependency override is used.
No headline parity score changes and no production compatibility claim.

Validation: all nine `cargo xtask ci` stages passed against the published pin,
including the immutable PDF dependency gate and all 23 wasm crates. Homebrew Rust
used `RUSTC_BOOTSTRAP=1 CARGO_UNSTABLE_BUILD_STD=std,panic_abort`. The strict WE
Dark/Light probe also passed with the CLI built against this published revision.
