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
