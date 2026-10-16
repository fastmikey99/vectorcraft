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
lossless image compression, and `advanced.overprint: "preserve"`. This is a
validation configuration, not a production recipe or automatic release.

## Bounds and limitations

Original boxes are restored only when the imported artboard frame, bleed and
printer marks have not changed. Rotated pages, non-unit UserUnit and Bounding
imports warn rather than claim preservation. PDF standards, flattening and
colour conversion take precedence and warn when retained print settings are
omitted. Explicit output-intent settings take precedence over the source intent.
Only DeviceRGB, DeviceCMYK and DeviceGray page blending spaces are retained.

Named spot bitmap channels, mixed spot/process bitmap channels, non-eight-bit
sample precision (16-bit samples are quantized to eight bits), exact floating
point vector ink values and exact path coordinates are not solved here.
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
