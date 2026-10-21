# PAL player — preparation and WE Light

`sep.runActionList` runs an ordered sequence of ArtPro PAL XML action lists on a
single-page, unrotated PDF. It is an engine command available through the CLI, MCP's command
execution tool, and the control channel.

```sh
vectorcraft-cli run --cmd sep.runActionList --params \
  '{"root":"/path/to/action-library","lists":["Preparation","Recipe"],"input":"/path/to/input.pdf","output":"/path/to/prepared.pdf"}'
```

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
- `Rasterize`, `SetTrapTags`, `RemoveTrappingTags`: empty selections are harmless;
  nonempty selections stop explicitly. Trap processing is a separate slice.

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

Status: experimental, 21 October 2026. This command is an isolated trap experiment,
not a production separation builder or PAL player. No parity score is claimed.

`sep.trap {"blackThreshold": 0.7}` replaces the bottom `Auto Trap-1` layer from exactly
one top-level `Artwork` layer, in one undo step. The K threshold is required: the example
is the accepted K-coverage rule; the command remains a prototype.

The command uses the same VectorCraft separation renderer as headless screenshots,
at 600 dpi with antialiasing off. Opaque vector fills, stroke outlines and clips
are united with pathops and rasterised once, preventing internal raster seams
from becoming bands while retaining real blank counters. Transparency and live
effects use renderer occupancy with a one-pixel closing fallback. Band distance
is measured from occupied pixel cells, avoiding a half-pixel width shortfall.
Colour samples use the nearest vector boundary and its own ink values for
opaque single-fill paths. The existing epsilon/K predicate is applied to that
owner. Complex appearances retain original renderer sampling. The command extends
the chosen process and spot ink values by 0.2 mm, traces the
pixel edges, then spreads and chokes the clip by 0.01 mm with miter joins (limit 4).
The output is a clip and a process/DeviceN image with a matching opacity mask;
PDF export retains the spot channels and SMask. Trap overprint is off.

Eligibility uses the PAL's `SeparationTypeRule Any / NotUsing` epsilon of 0.005:
all process coverages and the spot tint must be below 0.5% to count as no ink.
Black eligibility uses K coverage at or above the explicit `blackThreshold`,
including a spot's alternate K. It does not use RGB luminance. CMYK values are
preserved; non-CMYK alternates use the colour library's CMYK conversion.
Excluded objects still take part in nearest-boundary ownership.

PAL name matching is case-sensitive. WHP's `NO WHP` and `NO WHP OR POWDER`
exclusions apply to underprint, not trap tags; `NO POWDER` is not a trap exclusion
either. This isolated command does not execute those underprint actions. Metallic
and neon names now fail explicitly as outside the experiment's scope instead of
silently producing a partially trapped job. Neither underprint layer is edited.

Limits: solid vector paints and path/group/layer/compound nodes only, plus
opaque unlinked CMYK linear/radial gradients whose stop bounds prove uniform
PAL epsilon/K eligibility. Crossing gradients remain unsupported; accepted
gradients retain nearest rendered colour samples. There are no raster-art
inputs. The grid is limited to 64 million pixels at 600 dpi. An unsupported input fails before changing the document. The
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
