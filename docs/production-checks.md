# Recipe-driven production checks

Status: 19 October 2026, implementation in progress. This is not a qualified production-release checker. Existing parity scores and estimates are unchanged.

`vectorcraft-checks` provides a shared, headless Rust contract and measurement core. The editor exposes it through `production.checks.configure`, `production.checks.run`, cancellation and finding selection. The Checks panel shows the full obligation denominator, expected and measured evidence, unresolved work, validity and page-local finding locations. Preview visibility cannot change a request's obligations.

## Evidence and validity

A request binds an immutable recipe, garment, job revision, original, approved preparation, assignments, numerical acceptance, document and exact PDF bytes. The engine version incorporates the source hash. Every generated rule, contribution, file link, physical plate and global obligation has one result: Pass, Fail, Incomplete or independently evidenced Not applicable. Confirmed failures remain visible alongside incomplete coverage. Local reports are provisional; a separate, fully bound independent receipt is necessary for current validity and cannot turn an incomplete report into Pass.

The browser Checks action transfers one frozen source-preserving PDF to the host service and validates returned findings against the complete catalogue. Cancellation, reconfiguration and changed input identities reject late responses; edits made during the run remain intact and make the result stale.

The public PDF content is decoded afresh. Embedded editing documents do not substitute for printed content. Unsupported features, omissions, ambiguous routing, missing criteria and resource exhaustion remain explicit.

## Implemented measurement paths

- Approved source-region attribution, tri-state predicates, assignment policies and construction graphs. Names identify data; they do not dispatch geometry algorithms.
- Physical combined coverage, including white components, round offsets, dependency chains, protected regions, object/ink/union partitions and selected shared/nonshared edges. Measurement path operations retain tiny contours.
- Boundary maximum-distance intervals, topology, holes/islands and local witnesses in page-local millimetres. Spatial indexing accelerates queries without removing features. Ambiguous topology enclosures trigger bounded precision refinement; genuine crossings remain unresolved. Acceptance comes from evidenced request data, with no manufacturing fallback.
- Complete passing physical coverage supplies evidence for its mapped rule geometry, topology and contributions, including legal merged paths. Failing merged coverage remains a physical failure without automatically blaming every rule; direct outputs retain attributable failures. Proven-empty contributions remain inapplicable even when another contribution prints on the same plate. Unrouted rule pieces and unobserved intermediate constructions remain unresolved.
- Indexed DeviceCMYK images retain their palette ink values, row padding, Decode mapping and opacity masks across import/export, avoiding an unapproved RGB preview conversion. This preserves content; raster-mask conformity remains unfinished.
- Opaque vector fill/clip coverage, embedded text outlines and bounded PDF stroke expansion including caps, joins, dashes and dots. Hairlines remain device dependent.
- Bounded raw PDF path-paint order before editor OCG aggregation. Matching event and decoded-path counts restores real ordering for independent compositing. Missing content, ambiguous layers, unsupported print states or unmatched paints cannot establish ordering.
- Per-colour coverage detects swapped paints, changed components and missing colour regions despite equal overall outlines. Legal splitting of equal-colour coverage is accepted. For one non-overprinting output, a reconciled original/output paint stack compares visible opaque colour coverage, accepting legal flattening and rejecting reversed visible paints. Occluded paint is removed with bounded path booleans, preserving holes and page isolation. Ambiguous source ordering, merged output links and overprint remain unresolved where colours overlap; profile definitions and technical tint requirements remain separate criteria.
- Original capture and deterministic replay of attributed crop and affine scale/rotation/mirror/placement decisions. Original census and native identity remain fixed; region geometry, source facts and recorded decisions are rechecked against a fresh original. There is no automatic alignment to the finished output.
- Raw paint-source dictionaries distinguish direct device colours, named process Separations and process-only DeviceN before import converts them to editable CMYK. Independent tonal predictions preserve unspecified named channels and apply OPM 1 zero suppression only to direct DeviceCMYK. Source/order reconciliation is required; default colour-space replacements, special colourants and transparency-group compositing remain explicit unresolved cases. Original fixtures cover fill/stroke state, form inheritance, aliases and actual independent planes at two resolutions in both overprint modes.
- An optional external pinned Ghostscript `tiffsep` adapter retains exact inputs, configuration, per-ink tonal planes, hashes and diagnostics. Solid process/grey/spot predictions handle knockout and overprint modes at two resolutions. Rendering successfully is not runtime qualification.

## Remaining scope

Complete printable-content reconciliation, raster prepared masks and paint continuity, gradients/patterns/transparency/DeviceN, exact colour/profile semantics, job size/placement criteria, defect attribution within failing merged outputs, unobserved intermediate constructions, minimum-feature criteria, qualified independent runtime receipts and representative corpus adjudication are unfinished. The asynchronous browser/service bridge is implemented; the complete interaction/export lifecycle still needs qualification. The full catalogue remains present while these categories are unfinished.

The global colour, integrity and independent obligations deliberately remain Incomplete when only the supported subset has been measured. No score, hidden threshold, acknowledgement, metadata label or successful import can waive these obligations.

## Worker and resource bounds

The `sep-check` CLI accepts a request and exact PDF, optionally followed by the original. It also exposes `--enumerate`, `--prepare`, `--prepare-edits`, `--print-order`, `--digest-json` and native-only independent-render/verification commands. Request/capture JSON is bounded to 16 MiB and PDF inputs to 256 MiB. Geometry and event budgets fail explicitly; they do not truncate the coverage denominator.

Tests use original synthetic ISO PDF constructions and independent expected geometry. Private customer originals and rendered planes must remain outside this repository. The Ghostscript executable is external and is neither linked nor redistributed here.

The `Separation checker worker` workflow tests and builds a Linux x86-64 musl executable, then retains its SHA-256, engine source version, commit and lockfile hash as a CI artifact. This is deployment packaging, not runtime qualification or a production release. The host must pin the exact executable hash and engine version; independent interpreter configuration and qualification are separate.

Source-space semantics follow [ISO 32000-1 §§8.6.5–8.6.7 and 11.7.4](https://opensource.adobe.com/dc-acrobat-sdk-docs/standards/pdfstandards/pdf/PDF32000_2008.pdf). Only original synthetic fixtures are included.

Raw image accounting records each image use reached through page/form inline or XObject content, its path-sequence position, page/layer, inherited `cm`/form transform, clipping context, overprint flags, dimensions, sample depth, Decode array, colour-space kind and exact named-colourant bytes. Repeated uses count separately; unused resources and image-mask dependencies do not become extra page paints. Inline long/short key collisions remain explicit, following the [PDF Association's documented ambiguity](https://pdfa.org/safedocs-unearths-pdf-inline-image-issue/). Resource depth, image-use and metadata limits fail or report unresolved data rather than claiming a complete inventory. The checker retains bounded raw samples and full counts even when editable-image reconciliation fails. Pattern/shading content remains explicitly unresolved, and image masks are recorded as dependencies rather than recursively qualified. Placement rectangles are not ink coverage: image samples, masks, interpolation and compositing still require independent reconciliation. Public specification: ISO 32000-1 §§8.9–8.10 (link above).

## Review fixes — 18 October 2026

Physical coverage selects process paint across the complete observed PDF, independently of OCG names. Exact technical inks stay on their declared physical plates; an unaccounted named ink or multiple possible process/source destinations yields Incomplete instead of selecting a convenient layer. File-layer placement remains a separate obligation.

The web bridge retains the latest exported document and reports validity by its check/export token. Hosts can track results while another panel run is pending or fails, and while saved-byte verification is in progress. Edits make the report stale; Undo to the retained snapshot restores only provisional validity. See [the host protocol](web-embed.md#asynchronous-separation-checks). Production conformity still requires the unfinished feature and independent-runtime qualification.


## HC.10 — explicit page and output-intent requirements (19 October 2026)

The `page_specification` acceptance fact supplies version 1, `units: "mm"`, the exact job and baseline identities, a nonnegative tolerance and all five page boxes, rotation and UserUnit for each output page. Boxes are absolute PDF user-space coordinates, y up, in physical millimetres; they are not fitted to the observed artwork. Boundary uncertainty remains Incomplete. Page dictionaries alone cannot pass placement: every physical output must agree with the recipe construction and the original preparation must be freshly replayed.

The operator-approved `pdf_metadata` fact supplies version 1, exact job/baseline bindings and exact ordered `catalog_intents` / `page_intents` lists. Each intent carries its subtype and nullable embedded-profile SHA-256. A null value explicitly requires absence; an empty list requires an explicitly present empty array. This checks profile identity, not ICC validity, colour conversion or PDF/X conformance. External profiles, unreadable dictionaries and exceeded budgets remain unresolved. No production facts or acceptance limits have been added. Reporter displays archived job criteria for explicit review and retains the exact criteria with the authenticated preparation decision. Browsers submit only the reviewed digest; the server re-derives the criteria, and both server and Rust require operator provenance. Numeric recipe acceptance cannot supply these structured keys. Invalid unreviewed upstream candidates produce a notice and leave the criteria unresolved.

This closes the worker implementation of the specified page and output-intent facets. Broader image/mask/colour qualification, minimum features and production requirements remain in progress. Candidate source and Fable disposition are retained in the private completion evidence directory.


## HC.11 — source image samples and independent image-only prediction (19 October 2026)

Bounded decoding retains physical image-use transforms and source ink samples for direct Gray/CMYK, Indexed palettes, and named Separation/DeviceN channels. Decode arrays, row padding, 1/2/4/8/16-bit samples, matching-resolution gray soft masks, explicit one-bit masks and colour-key masks are measured without an RGB preview conversion. The inventory hashes all ink/alpha samples while keeping packed storage out of report JSON. Unsupported codecs, colour substitutions, masks and over-budget content remain explicit. Indexed images use at most eight-bit indices with default/reversed Decode; fractional mappings and 16-bit colour-key masks remain unresolved after pinned-interpreter disagreements. One sample-work budget spans all pages.

The native image-only predictor reconciles source samples with hashed independent tiffsep planes, accounting for CropBox/MediaBox, rotation, UserUnit and each image transform. It preserves an uncertainty envelope around sample boundaries and refuses unresolved clipping, interpolation, profiles, graphics states, repeated fractional-alpha stacks or mixed vector/image pages. Pixel and sample-evaluation budgets apply before full plane decoding. Actual interpreter regressions cover masks, rotation and units at two resolutions and detect deliberately changed pixels.

This is a feature-specific measurement path, not runtime qualification or a prepared raster mask. Full source continuity and mixed compositing remain open; successful sample decoding cannot close those obligations.

## HC.12 — compact production-size samples (19 October 2026)

Source rows, Decode values, palettes and masks remain packed; individual ink/alpha values are evaluated in binary64 when needed. The normalized v1 sample hash keeps the same meaning across this storage change. A cumulative 256 Mi sample-work ceiling covers decoding, normalized evaluations and palette storage across the document; unsupported or over-budget content remains Incomplete. A four-million-pixel CMYK image plus gray soft mask retains 20 MB of sample bytes instead of 160 MB of floating-point tone/alpha arrays, without rounding its fractional Decode endpoints. Compressed sample and palette streams are checked beyond the declared length so a truncated prefix cannot masquerade as a complete stream. This removes a storage limitation; it does not qualify masks, colour conversion, interpolation or mixed compositing.

Unfiltered inline-image sample length is separated from the EI delimiter using dimensions; leading/trailing whitespace-valued pixels are retained. Direct 2/4/16-bit and inline regressions run without an interpreter. The original synthetic interpreter matrix includes raw and ASCIIHex inline images at two resolutions. Fuzz coverage exercises raw image sample and page-metadata entry points alongside the existing importer tests.
