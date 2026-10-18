# Recipe-driven production checks

Status: 18 October 2026, implementation in progress. This is not a qualified production-release checker. Existing parity scores and estimates are unchanged.

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
