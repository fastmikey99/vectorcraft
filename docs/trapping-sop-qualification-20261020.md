# Trapping SOP integration — 20 October 2026

Status: scoped implementation and local qualification complete. No deployment, live database mutation or merge.

## Source and shared settings

Owner-supplied `Trapping SOP.pdf`, page 4 standard trapping screenshot and pages 6–8 mixed instructions. The screenshot confirms 0.20 mm, round joins, Image Traps Extend Data, close gaps smaller than 0.01 mm, allow overshoot on, pullback off. These are construction settings. **0.01 mm is not an acceptance tolerance.**

| Requirement | Shared field | Change |
|---|---|---|
| Applicable distance | Existing rule `distance_mm` | Reused; no override replaced |
| Join | Existing rule `join` | Fill only missing `indigo-trap` join with round |
| Image edge colour extension | Existing `foundation.geometry.paint_extension = extend_source` | Reused |
| Gap closing | `foundation.geometry.trapping.close_gaps_mm` | New construction control |
| Overshoot | `foundation.geometry.trapping.allow_overshoot` | New construction control |
| Pullback | `foundation.geometry.trapping.pullback` | New construction control |
| Trap / no trap / mixed | Explicit `trapping_intent.mode` | Independent of garment Dark/Light |
| Known exclusions | `trapping_intent.exclusions` | Page-local rectangles in mm; null is unknown, [] is explicitly none |
| Editable No trap selection | Native `Document.trapping.objects` | Stable native object IDs, persisted and undoable; no temporary white overlay |

Reporter uses the existing recipe editor and print-check selector. Native `production.trapping` reads/edits the same intent; its panel can save the current selection as No trap. The frozen native snapshot transmits intent with the exported PDF to Reporter. Explicit clearing is distinct from omitting an override. The request's existing acceptance envelope transports this intent; it does not make intent a numerical tolerance.

Migration `367-trapping-sop.sql` adds missing construction settings to existing drafts, clones the shared rule definition when adding a missing join, increments revision and records a snapshot. It is idempotent and leaves published recipes, explicit overrides, distances, UK Green cumulative dependencies and acceptance limits untouched. `scripts/trapping-sop-seed.mjs` is the matching maintained seed transformer, not a runtime fallback. Historical seed migrations remain unchanged. No live migration was run.

## Implemented observations and limits

The bounded colour-trap observation checks round outer-band distance/shape, edge colour and gaps for one eligible solid-colour, convex straight-edged source contour per page. It respects per-object partitioning by refusing unresolved multi-object unions. Filtered/restricted source selection, concave/multi-contour gap closing, images, unsupported generation overrides and unresolved compositing remain Incomplete. It never substitutes a simple offset for unsupported settings.

Known rectangular exclusions can be checked independently of source eligibility. Clear surviving positive trap coverage inside a known exclusion fails; geometry within the numerical error bound stays unresolved. Later paint that may alter the observed coverage prevents a definitive result. These measurements apply only to applicable Indigo source-colour trap routes, not screen spreads. NE/NB neon exclusion remains colour-trap eligibility; the existing screen/UK Green relationships remain authoritative.

Native No trap object IDs are retained for the future maker. **Resolving arbitrary native object selections to exact final-PDF exclusion geometry is not implemented in this increment.** Such selections remain explicitly unmeasured; they are not replaced by bounding boxes. Only supplied rectangular exclusion regions are currently measured.

A legacy mixed sep without exclusions reports `mixed-trap exclusions not known` while independent checks continue. Garment wording, missing bleed, `LIGHT NO BLEED` absence, and lack of an ArtPro/PA Sharp ticket cannot supply that intent or fail the final sep. The SOP creates no layer-sequence or Auto Trap overprint rule. Existing separately confirmed recipe flags remain unchanged.

No production acceptance tolerance was added. Synthetic controls use an explicit **test-only 0.002 mm** tolerance. Unset tolerance remains unresolved even for a nominally correct trap.

## Focused qualification

Original PDF controls, authored from public PDF syntax, exercise the actual native import/check path:

| Control | Colour-trap finding |
|---|---|
| Correct 0.20 mm round trap | Pass with test-only acceptance |
| 0.30 mm instead of 0.20 mm | Fail |
| Wrong edge colour | Fail |
| Cut gap in trap band | Fail |
| Mixed, known rectangular exclusion, correctly clipped band | Pass |
| Same mixed exclusion with bleed present | Fail |
| Legacy mixed, exclusions unknown | Incomplete; independent ink check still runs |
| Correct trap, acceptance unset | Incomplete |

Additional regressions cover unsupported generation/edge overrides, screen-route isolation, page-local coordinate orientation, occluded source art, overwritten target paint, per-object versus union semantics, persistent native intent/undo, explicit clearing, stale service completion and exact-PDF intent forwarding. Two focused read-only Codex reviews were completed; no Fable or Opus review was invoked.

Three supplied dark-reference files were rerun locally as explicit Trap scenarios: one user-confirmed PA Sharp reference and two supplied Esko/production references. **No existing verdict changed.** This comparison is against the preceding practical reports. The previously recorded legacy-checker comparisons remain in the parent qualification report; the legacy checker was not rerun for this SOP increment. The new intent finding confirms the scenario choice only; it does not certify trap geometry. The real recipes' filtered source eligibility still prevents a complete trap-distance verdict from final artwork alone. The prior measured blocker choke and other independent observations remain available. No actual mixed preparation with known exclusion regions was present; the known-region positive/negative pair above is synthetic, not claimed as a real-file qualification.

The private report retains all requests, reports, PDF hashes, binary identity and the comparison with the previous reports. Customer PDFs and SOP screenshots are not committed.

## Codex review disposition

1. Generation settings ignored: fixed; unsupported pullback/overshoot and gap-closing geometries return Incomplete.
2. Restricted treatment semantics: fixed; explicit eligibility guards prevent a union/all-edge expectation from representing a filtered rule.
3. Screen-route leakage: fixed; applicable Indigo routes only, with separate protection resolution.
4. Inherited legacy lookup properties: fixed with Map and regression cases.
5. Missing geometry parent in migration: valid foundations already require this object by schema constraint; migration additionally filters explicitly for an object. No invalid foundation is fabricated.
6. Source occlusion: fixed; unresolved later-paint interference prevents a definitive verdict.
7. Target paint overwrite: fixed; overlapping later target paint cannot silently establish final colour.
8. Object/union mismatch: fixed; unsupported multi-object per-object construction remains Incomplete.
9. Explicit clear restoring old intent: fixed with a bound null intent and native/service regressions.

## Final validation

- All nine VectorCraft CI steps passed: fmt, clippy, tests, no-bundled-fonts, PDF dependencies, assets, brands, layers and wasm.
- Checker suite: 269 passed, six ignored; all six focused trapping test groups passed.
- Reporter: 55 focused tests, one disposable-schema migration test and one actual native-worker integration test passed. Web build passed.
- All 11 focused qualification scenarios were rerun against the final native binary: eight synthetic controls and three supplied dark references. Existing real-file verdicts did not change.
- Two read-only Codex reviews completed; verified findings and regressions are listed above.
- Browser inspection exercised the independent Dark/Light and trapping selectors and unknown-mixed message. Native headless panel rendering, persisted intent, undo, clearing and frozen-snapshot transport passed.
- Native desktop smoke check saved a synthetic No trap selection successfully. Window screenshot returned `no frame was presented`; the documented fallback artboard render was visually inspected and native panel headless tests passed. No native window pixel inspection is claimed.
- Reporter repository-wide typecheck retains unrelated baseline errors; no errors in this increment's changed files.

Rollout requires the matching VectorCraft web/native artifacts and Reporter changes. The existing default web artifact remains unchanged. Migration 367 is prepared and locally tested, not applied to a live database. No merge or deployment was performed.
