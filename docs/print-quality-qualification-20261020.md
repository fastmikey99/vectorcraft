# Practical print checks — qualification

## Completed local qualification — 20 October 2026

The current sep-only implementation and its local qualification are complete for the supported measurement domains. This does not claim complete geometric coverage of the supplied files or replace every old-checker measurement.

- VectorCraft: all nine `cargo xtask ci` gates pass; the checker suite has 263 passing tests and six ignored historical/environment-specific tests. Controls include wrong ink/choke, clipped/erased dimensions, partial pages, DeviceN/stencils, bounded resources, supported rectangle detail, harmless panel/drawing order changes and actual required-ink knockout.
- Reporter: 38 workflow/transport tests, 42 construction-model/editor tests and one actual native-worker integration test pass. The memory-aware web build passes. A synthetic Chrome preview verified recipe selection, report grouping, numeric measurements and filtering; this is not a deployed WASM end-to-end test.
- Supplied corpus: 15 separation outputs, four supporting originals, 42 executions. Eight files have explicit job-variant bindings; seven remain unknown with 34 separate candidate observations. No worker errors. The eight bound cases contain 54 passed questions, 1 recipe discrepancy and 109 explicitly unmeasured questions. Whole-file printing approval is not implied.
- All eight bound reports agree exactly through Reporter’s real worker transport and the native CLI. The current engine is `0.12.0/checks-1/a4d15dcce047e98bbcfa13d11ed6d4e287e023d46fdf8f3d08c99bbee2325360`.
- Codex read-only reviews were used at each implementation milestone. Verified findings were corrected and covered by focused controls; dispositions are in the accompanying review record. No Fable or Opus was used.

The readable private report is retained at `/Users/mikemodgill/dev/Supaflow-Reporting/data/hybrid-checker/print-quality-20261020/README.md` (ignored local data). It includes per-output passes, recipe discrepancies, measurements, concrete limits and old-checker comparisons. Customer PDFs and full private reports are not committed. Sequence-only failures are retracted generally; the remaining bound-file discrepancy concerns an overprint flag. Missing acceptance criteria remain unset.

General curved/raster minimum features, halftones, shared/protected source edges, coalesced white contributors and unresolved image/compositing coverage remain explicitly unsupported where applicable. Existing legacy size/geometry warnings are retained rather than silently cleared. No Switch/PitStop extraction equivalence is claimed.

No migration, production recipe edit, merge or deployment is included. Rollout requires the companion VectorCraft web artifact and native worker from the updated branch; Reporter’s existing default web version is unchanged. Historical preparation/renderer diagnostics remain separate engineering utilities, and the retired independent-certification service path has been removed from Reporter.

## Codex review disposition

All reviews used Codex with read-only, source-only packets. No Fable, Opus, customer artwork, external review comments or reviewer test/build execution was used. Static review is separate from runtime qualification.

The first review identified aggregate name allocation, pre-clip size bounds, incomplete import handling, DeviceN colourants, required-screen fallback, inherited stencil attribution, incomplete sequence counterparts and stale selector responses. Those were corrected and regressed. The later scope clarification removes sequence verdicts entirely, superseding counterpart-order handling.

The closure review identified bypassing the raw-name budget into the decoded census, later paint erasing dimensional bounds, stencil attribution affecting output flags, unrelated image pages disabling vector checks, unrelated partial tones suppressing feature measurement and supported rectangular detail never reaching Pass. Fixes now bound expansion, retain incomplete size where erasure is unresolved, scope attribution and coverage, and allow supported detail to pass.

A subsequent review found a same-layer image on another page still suppressed relationship measurements. The correction retains supported vector-page measurements and proven defects while downgrading any partial whole-rule Pass to Incomplete. The narrow Codex closure reported no actionable findings.

The final order-semantics review confirmed that no order-only verdict rows or layer_order comparisons remain in the practical code. It identified an unresolved-layer guard missing from the physical-coverage fallback. The guard now applies to relevant ink and knockout contributors, with a regression control preventing uncertain coverage from becoming either Pass or Fail. Page reconciliation also refuses unsupported global-layer shortcuts.

Reviewed limitations remain explicit: curved/raster minimum features, halftones, selected/shared/protected edges, coalesced white contributors, and unresolved image/compositing coverage. Missing manufacturing criteria are not replaced with defaults. Actual sequence is information; complete required-ink loss is a defect when supported coverage proves it. Layer-list order and press sequence are separate.

Final implementation inspection also found that an entirely missing recipe route list could label observed inks as unexpected. Empty route lists now leave that question Incomplete; declared routes retain wrong-ink detection. Both the checker control and actual native-worker integration cover this case.
