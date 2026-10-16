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
