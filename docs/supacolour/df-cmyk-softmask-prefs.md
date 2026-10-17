# DF CMYK soft masks and preferences

Status: local regression verification, 17 October 2026. No customer files are
included. Base: `0016bd97` (regression also observed in `aa97e68e`).

## CMYK images

The public PDF importer already retains CMYK samples and carries soft masks as
opacity masks. Ordinary `document.open` subsequently tries the embedded editing
copy. DF33155's editing copy contains RGB image previews, and the visual comparison
accepted those in place of the PDF's CMYK images. This was not depth-limit
flattening. `editingData:false` and `printProduction:true` bypass that substitution.

Automatic substitution now requires every used CMYK image's dimensions and ink
samples to survive in the editing copy. Otherwise the imported public PDF artwork
and masks are retained, with a warning. Encoding/resource names may differ;
explicit `editingData:"always"` retains its existing opt-in semantics.

Synthetic engine regression coverage constructs a two-pixel CMYK PDF with an RGB
editing preview, testing image `/SMask`, luminosity and alpha graphics-state masks.
It asserts CMYK samples and masks survive ordinary open, and ordinary PDF export
(without editing data) contains DeviceCMYK and reimports with the same inks.
Existing CMYK editing-data import remains covered.

DF33155 local measurements use Ghostscript's default resolution:

```sh
gs -q -dBATCH -dNOPAUSE -sDEVICE=inkcov -o - FILE.pdf
```

| File | C | M | Y | K |
|---|---:|---:|---:|---:|
| Supplied source | 0.18126 | 0.18217 | 0.17432 | 0.13300 |
| Live `e510c801` export | 0.18528 | 0.18538 | 0.18019 | 0.14991 |
| Regressed `aa97e68e` / base `0016bd97` export | 0.18497 | 0.18525 | 0.17641 | 0.12575 |
| Fixed ordinary CLI open/export | 0.18530 | 0.18539 | 0.18019 | 0.14992 |

The supplied source measures differently from the live export: the reported
K ≈ 0.150 baseline is the live export, not this source under this invocation.
The fixed CLI invocation used `run --in DF33155.pdf --cmd document.exportPdf`
with `{"path":"/tmp/vc-cmyk-fixed.pdf","preserveEditing":false}`, without any
production/preservation import or export flag. `pdfimages -list` confirms CMYK
JPEG content and separate gray masks, replacing the regressed RGB images.

This fix targets the CMYK-to-RGB regression and does not claim exact source ink
coverage. Customer input/output files remain under `/tmp` only.

Revision history: 2026-10-17 — document the substitution cause and synthetic coverage.
