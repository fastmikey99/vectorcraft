# PAL player — preparation, WE Light, WE Dark and WE Mixed

`sep.runActionList` runs an ordered sequence of ArtPro PAL XML action lists on a
single-page, unrotated PDF. It is an engine command available through the CLI, MCP's command
execution tool, and the control channel.

```sh
vectorcraft-cli run --cmd sep.runActionList --params \
  '{"root":"/path/to/action-library","lists":["Preparation","Recipe"],"input":"/path/to/input.pdf","output":"/path/to/prepared.pdf"}'
```

For Dark, insert the literal `"TrapTask"` in `lists` between the trap-ticket PAL
and the recipe PAL. It is an explicit AE step, not a filename or an automatically
inferred operation. It uses the established 0.2 mm outward, image-extend trap with
K ≥ 70% excluded, creates `Trap Layer 1` above the art, and records its time and
tag/ink-rule conflicts in `messages`. The PALs then lock, rename and move that
layer, rasterize it and run TRAPPY.

`root` is optional and defaults to the folder named by the
`VECTORCRAFT_ACTION_LISTS` environment variable. List names are relative to that root, with an optional `.pal`
extension. Backslashes are accepted. Folder-qualified calls resolve from the
root; bare calls resolve from the calling list's folder. Calls cannot escape the
root. XML size, nesting, recursion and total execution are bounded.

`profiles` optionally names a local folder of `.icc`/`.icm` files; it overrides
`VECTORCRAFT_PROFILES`. `AssignDocumentProfile` matches CMYK profile descriptions,
ignoring case, whitespace and punctuation, and allowing an omitted parenthetical
qualifier. Exact descriptions win; ambiguous matches are reported, never guessed.
Only the matched profile is loaded through `color.loadProfile` and assigned through
`edit.assignProfile`. The configured folder is authoritative; without one, already
registered profiles remain available. No ICC files are bundled.

The assigned profile is used for RGB ink selection, PDF-alternate conversion and
process-ink deletion, and for remaining RGB at PDF export; existing CMYK values and
spot tints are preserved. `ConvertToCMYK` follows `Option=UseFromPDF` or the explicit
`Custom` values. Esko's documented action has no rendering-intent or BPC settings;
these PAL variants do not supply them. Profile conversions use the existing
`edit.colorSettings` intent and BPC (defaults: relative colorimetric, BPC on), not an
inferred Esko default. Missing profiles retain the PAL severity and mention
`VECTORCRAFT_PROFILES` in the diagnostic.

The result contains `status` (`completed` or `stopped`), `stoppedAt` (list, one-based
position, action and reason), `traceLength`, `trace`, `layers`, `spots`, `artwork`,
`warnings`, `importWarnings` and `messages`. Each trace entry also prints to stderr,
leaving stdout available for the CLI result or MCP protocol. Nested groups have
local one-based positions, with a trace entry for the group and each executed child.
The output PDF is written atomically, including when execution stops. The input
cannot be overwritten. The prepared document remains open in the engine. The PDF also embeds VectorCraft
editing data to retain object names on reopen; its public page content, layers and
ink resources remain independently readable.

## Supported preparation actions

- Calls and control: `CallActionList`, `Group` (action container), `Conditional`
  (object-selection rules; All/Any/None), `ShowMessage`.
- Selection: `SelectLayerByName`, `SelectObjectsInLayer`, `SelectAllObjects`,
  `SelectObjectsByAttributes` (NameRule, SeparationNameRule, SeparationTypeRule,
  ObjectSelectionRule, ObjectTypeRule), `InvertObjectSelection`, `SelectEmptyLayers`,
  `SelectPrintingLayers`, `SelectSeparationByName`,
  `SelectSeparationsInPrintingLayers`, `InvertSeparationSelection`.
- Names and layers: `RenameObjects`, `RenameLayers`, `RenameLayersWithWildcards`,
  `CreateLayerOnTop`, `DeleteLayers`, `MoveObjectsToLayer`, `ChangeLayerOrder`,
  `LockUnlockLayers`, `SetLayerType` (None), `SetLayerPrintingState`.
- Objects: `DeleteObjects`, `GroupObjects`, `UngroupObjects`, `Copy`, `PasteInFront`,
  `ArrangeObjects` (Front, Back, Forward, Backward).
- Paint: `SetFillFlatPaint`, `UpdateFillFlatPaint`, `SetStrokeFlatPaint`,
  `UpdateStrokeFlatPaint`, `SetFillOverprint`, `SetFillKnockout`,
  `SetStrokeOverprint`, `SetStrokeKnockout`, `SetStrokeWidth`, `SetStrokeCorners`.
- Inks: `CreateSeparation`, `DeleteInk`, `ConvertToCMYK` (PDF alternate or Custom),
  `AssignDocumentProfile`. Missing profiles follow the PAL's severity; no profile
  is substituted. Unpainted spot inks are retained as PDF color-space resources.
- Page boxes: `SetPageBoxSizeByContent` (FitAll or FitToSelection/Both), `SetPageBoxSizeRelative`.
  PAL distances are millimetres, converted to points; output includes all five boxes.
- `CleanObjects`: ExecuteClip, CleanUpGroupsAndClippingMasks, ContourizeText,
  OutlineStrokes. Curve intersections and stroke outlines use VectorCraft's
  existing path operations. Named groups are retained. Images remain clipped
  without resampling.

## Vector separation actions

- `AddWhiteUnderPrint`: union of the selected painted footprints (including stroke
  outlines), spread/choke in millimetres, Round/Miter/Bevel joins and miter limit.
  `ExcludeCoveredAreas` subtracts unselected art **above** the selected art, in
  painting order; selected objects do not knock each other out. `ExcludeWhiteObjects`
  excludes zero-ink objects. The result is added to `White Underprint`, and only the
  new result is selected. `UseSelectedSeparation=0` uses `Opaque White`; otherwise
  exactly one selected ink is required. Subsequent PAL paint and overprint actions
  determine the final ink, tint and overprint state.
- `Choke`, `Spread`: OriginalPath, CopyInFront and CopyBehind; the resulting paths
  are selected. A choke that removes a shape is a valid empty result.
- `Shaper`: Unite, Intersect, Exclude, MinusFront and MinusBack, with WorkOnCopy.
  Uses the same curve boolean and stroke outline operations as Pathfinder.
- `CreateShapeOnSelection`: a rectangular selection bound; `SetObjectsSize`:
  physical width/height about the selection center. `Mode=Relative` selects that
  anchor rather than the crosshair; it is not percentage scaling. Content page
  boxes also accept `FitToSelection`.
- `SetTrapTags`: the WE ticket variant (Trapping, external Always/Never, Outwards,
  0.2 mm, internal Never/0 mm, pullback Never). Unsupported ticket variants stop.
  Permissions live on objects, inherit through groups, and survive native save/reopen.
  `RemoveTrappingTags` removes selected-level tags, or descendants too with `AllLevels=1`.
  Permission and the existing epsilon/K eligibility checks must both allow trapping;
  `Always` does not force white or high-K paint to trap. Conflict counts are reported.
- `Rasterize`: 600 dpi, AntiAliasing None, ConsiderBackground 0. Renders the selected
  art into its original process/spot planes, retaining transparency as an opacity mask
  and creating a vector clip. Background artwork is not composited. The new group
  remains selected. TRAPPY's Spread/Choke preserves the clipping-path flag.
  Unsupported resolution/background/AA variants stop. Empty selections are harmless.

Before any PAL runs, the house preparation rule deletes objects painted with the
spot `SupaBG`. The trace records this separately as `Preparation / DeleteBackground`.
It is not a separation rule inferred from shipped output.

Unknown actions and unsupported parameter variants stop with a reason. Pattern
selection, mixed process/spot flat paint, parameterized calls, dynamic marks,
nonopaque vector geometry and raster geometry remain unsupported. The result
reports the stop explicitly; it never silently skips a nonempty unsupported action.
Layer summaries are in painting order (bottom to top) and include observed fill
and stroke overprint flags.

Semantics reference: [Esko ArtPro+ 25.07 public user guide](https://docs.esko.com/docs/en-us/artproplus/25.07/userguide/pdf/artproplus.pdf),
particularly White Underprint (pp. 310–311), Shaper (pp. 194–195), and the action
catalogue (Add White Underprint, p. 427; Create Shape on Selection, p. 434;
Set Object Size, pp. 457–458). Tests generate
small synthetic PDFs and PALs inline; no production artwork or action-library
files are included in the repository.

Profile semantics: the same guide, Color Management (pp. 88–90), Convert to CMYK
(pp. 130–131, 430).

Revision: 2026-10-21 — local ICC discovery and assigned-profile conversion.

# Trap layer (`sep.trap`)

Status: experimental, 21 October 2026. Standalone `sep.trap` and the PAL player's
explicit AE TrapTask share the same band and ink computation. The clean Dark set
runs end to end, but production parity is not claimed: customer-to-shipped colour
changes and geometry/tint differences remain in the local report.

`sep.trap {"blackThreshold": 0.7}` replaces the bottom `Auto Trap-1` layer from exactly
one top-level `Artwork` layer, in one undo step. The K threshold is required: the example
is the accepted K-coverage rule; the command remains a prototype.

The command uses the same VectorCraft separation renderer as headless screenshots,
at 600 dpi with antialiasing off. Opaque vector fills, stroke outlines and clips
are united with pathops and rasterised once, preventing internal raster seams
from becoming bands while retaining real blank counters. Transparency and live
effects use renderer occupancy with a one-pixel closing fallback. Band distance
is measured from occupied pixel cells, avoiding a half-pixel width shortfall.
Colour samples use a balanced spatial index over cubic-segment bounds to find the
nearest vector boundary and its own ink values for opaque single-fill paths.
Exact distances and original-order ties agree with the previous linear search. The existing epsilon/K predicate is applied to that
owner. Complex appearances retain original renderer sampling. The command extends
the chosen process and spot ink values by 0.2 mm, traces the
pixel edges, then spreads and chokes the clip by 0.01 mm with miter joins (limit 4).
The standalone output is a clip and a process/DeviceN image with a matching opacity
mask; PDF export retains the spot channels and SMask. The clip is unpainted: it
must not add black ink where smoothing exposes a gap in the image mask. Trap
overprint is off.

TrapTask instead emits vector runs on that same 600 dpi band grid with an uncleaned
clip. It does not perform Rasterize or TRAPPY. The runs preserve the sampled ink
values; the PAL Rasterize makes the image, then TRAPPY spreads/chokes the clip once.
A checked footprint cache avoids repeatedly uniting those runs during underprint;
it is valid only while the complete generated group equals its saved snapshot.
Synthetic process/spot plate comparisons cover equivalence with standalone trapping.
Both paths report `tagConflicts` (tagged paints excluded by ink rules and ink-eligible
paints excluded by tags). RGB process sources are converted through the assigned
CMYK profile consistently with the player's PDF output; existing CMYK and spots
keep their values.

Eligibility uses the PAL's `SeparationTypeRule Any / NotUsing` epsilon of 0.005:
all process coverages and the spot tint must be below 0.5% to count as no ink.
Black eligibility uses K coverage at or above the explicit `blackThreshold`,
including a spot's alternate K. It does not use RGB luminance. CMYK values are
preserved; non-CMYK alternates use the colour library's CMYK conversion.
Excluded objects still take part in nearest-boundary ownership.

PAL name matching is case-sensitive. WHP's `NO WHP` and `NO WHP OR POWDER`
exclusions apply to underprint, not trap tags; `NO POWDER` is not a trap exclusion
either. The standalone command does not execute those underprint actions. Metallic
and neon names now fail explicitly as outside the experiment's scope instead of
silently producing a partially trapped job. Neither underprint layer is edited.

Limits: solid vector paints and path/group/layer/compound nodes only, plus
opaque unlinked CMYK linear/radial gradients whose stop bounds prove uniform
PAL epsilon/K eligibility. Crossing gradients remain unsupported; accepted
gradients retain nearest rendered colour samples. There are no raster-art
inputs. The grid is limited to 64 million pixels at 600 dpi; TrapTask's pre-raster vector
form is limited to one million horizontal runs. An unsupported input fails before changing the document. The
exterior is the complement of the art union, including blank enclosed openings;
ink-to-ink edges never receive a band.
No page trap-ticket `/Properties` are written by this prototype.

The expanded local edge study supports the accepted K coverage rule, but does
not identify the trap ticket's parameter semantics uniquely. High-K spot
alternate behaviour remains unvalidated.
The no-ink tolerance removes a major false band and K coverage retains trapped
dark chromatic inks. A retired manually prepared no-trap case remains unresolved; it is outside
the clean-input comparison set. No size filter, file exception or additional
production rule was introduced to hide that mismatch. Per-file results and
customer data remain outside the public repository.

Synthetic tests cover vector colour ownership (including implicit fill closure),
excluded nearer owners, renderer fallback, gradient eligibility bounds and ramp
preservation, vector union, small counters, clips, seam closing,
pixel-cell distance, nearest excluded ownership, white and
rich black, PAL epsilon boundaries, specialty-input rejection, spot alternates, spot-plane preservation, PDF DeviceN/SMask export,
replacement, undo and cancellation. There are no customer fixtures.

## Dark validation and remaining differences

All eleven clean customer inputs complete the supplied sequence with local profiles.
The white-only input produces no trap. One coloured input also produces no trap:
its customer and shipped artwork have different K coverage on opposite sides of
the required K≥70% threshold. The rule excludes the customer ink despite its Always tag. No threshold exception or
colour correction was added.

The supplied New template ends with SelectAllObjects then InvertObjectSelection.
Consequently the following 2b RemoveFromSelection/rename-LIGHT sequence starts
empty. This behavior is retained, not repaired with an extra selection action.
Specialty-name lists are reached with empty selections on this cohort; no metallic
or neon implementation was added.

Local comparisons use every process, white and PANTONE plate at 600 dpi, translated
to register Artwork bounds without scaling. Full plates use the existing headless
preview; trap-only ink measurements additionally disable antialiasing and image
smoothing. Reports include XOR area, percent of shipped ink, symmetric worst
distance, shared-ink mean/max tint error, layer order/overprint and build/trap times.
No customer data, identities or per-job reports are included here.

Revision: 2026-10-21 — explicit Dark TrapTask, inherited trap tags, 600 dpi Rasterize,
TRAPPY clip preservation and indexed nearest-vector colour ownership. Changes are
uncommitted for lead review.


## Mixed garment intent

`sep.runActionList` accepts an optional, independently keyed human selection:

```json
{"intents":{"noTrap":{"objectIds":[12,19],"rectangles":[{"x":10,"y":20,"width":15,"height":8}]}}}
```

Use the same explicit list sequence as Dark: preparation (0), template (2),
`Trapping_Supacolour_Standard (NEW)`, `TrapTask`, then WE Separation (3).
For Mixed, `noTrap` replaces the selection **immediately before** the one explicit
2b call. This represents the artist's manual 2b run; AE proceeds straight to
TrapTask without a second 2b run. The player records `Intent / SelectNoTrap`
before the PAL trace. Missing or repeated tickets, and a ticket not immediately
followed by TrapTask, are rejected before opening the input.

Object IDs refer to prepared Artwork immediately after (0)/(2), obtainable from
`document.inspect` in the session after running those lists. Group IDs select
their leaves. IDs are specific to that input and preparation, not portable job
identifiers. Rectangles use document coordinates in millimetres, x rightwards
and y downwards (the same origin as document inspection, converted from points).
Their union selects only the intersecting geometry: partially intersected paths
are split, retaining their paint and stacking order. Only prepared opaque filled
paths without strokes, clipping, masks or effects support partial-region selection;
unsupported inputs stop explicitly. Both selector fields can be combined; empty
selectors are valid and preserve the Dark behavior. Missing IDs are errors.

The PAL, not the intent handler, renames selected art `LIGHT`, builds the white
`LIGHT NO BLEED` mask, applies the trap ticket and later handles WHP and cleanup.
No additional PAL action types are required. Both `sep.trap` and TrapTask respect
the visible white mask. When that mask is present, nearest rendered ownership is
used instead of nearest-outline colour ties; a mask behind artwork does not
suppress its visible ink. WHP's existing actions convert the mask to 50% K and
move it back/front, excluding covered dark-style WHP while retaining the choked
light-style pass. CLEAN UP deletes the temporary mask.

NE/NB neon is automatically non-trapped by the production sequence; WE has no
neon jobs. This change does not add neon separation support or alter the established
ink epsilon, K threshold or colour conversion.

Revision: 2026-10-21 — Mixed no-trap intent, partial rectangle selection and
visible white-mask trap ownership; uncommitted for lead review.


Mixed validation remains experimental. All twelve local inputs complete with an
explicit empty no-trap intent. A separate shipped-Artwork retrap diagnostic found
no clear omitted eligible whole-object or partial-region bands in this cohort;
it does not supply a convincing nonempty production mask case. Synthetic tests
cover a nonempty selection, partial rectangles, front/back mask ownership and
WHP's covered-area versus choked pass.

The requested customer-to-shipped parity bar is not met. Seven supplied inputs
contain printed process backdrops absent from their shipped artwork, without a
SupaBG tag; the player retains them. Two of the other five satisfy the strict
2-pixel trap/WHP bound at 600 dpi. Small trap/underprint residuals remain in the
other three, alongside source geometry and known colour-conversion differences.
The local report records every plate, stage attribution and unresolved causes;
no backdrop-removal rule or tolerance exception was added.
