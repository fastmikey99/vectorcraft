# PAL preparation player — slice 1

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
- Page boxes: `SetPageBoxSizeByContent` (FitAll/Both), `SetPageBoxSizeRelative`.
  PAL distances are millimetres, converted to points; output includes all five boxes.
- `CleanObjects`: ExecuteClip, CleanUpGroupsAndClippingMasks, ContourizeText,
  OutlineStrokes. Curve intersections and stroke outlines use VectorCraft's
  existing path operations. Named groups are retained. Images remain clipped
  without resampling.

The first `AddWhiteUnderPrint`, `Choke`, `Spread`, `Shaper`, `Rasterize`,
`SetTrapTags`, or `RemoveTrappingTags` stops execution **before** that action.
Later actions, including ink creation, are not run or anticipated. Unknown actions
and unsupported parameter variants also stop with a reason; they are never skipped.

This is the preparation subset used before separation geometry, not a complete PAL
implementation. Pattern-paint separation selection, mixed process/spot flat paint,
parameterized calls, other processing-step types, and the remaining CleanObjects
options are not implemented. The return value reports any such stop explicitly.

Semantics reference: [Esko ArtPro+ 25.07 public user guide](https://docs.esko.com/docs/en-us/artproplus/25.07/userguide/pdf/artproplus.pdf),
particularly Clean, the action catalogue, and separation selection. Tests generate
small synthetic PDFs and PALs inline; no production artwork or action-library
files are included in the repository.
