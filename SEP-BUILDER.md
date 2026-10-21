# PAL player — preparation, WE Light, WE Dark, WE Mixed and metallic

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
The output PDF is written atomically only after successful execution and fidelity validation.
Stopped jobs publish no PDF and leave any existing destination untouched. The input
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
  `ExcludeCoveredAreas` subtracts unselected knockout paint **above** the selected art, in
  painting order; overprinting paint and selected objects do not knock selected art out. `ExcludeWhiteObjects`
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
Covered boundaries are removed from ownership; frontmost visible paint wins ties.
The existing epsilon/K predicate is applied to that owner. Complex appearances retain original renderer sampling. The command extends
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
Selection and trap use the same f32 predicate; exactly 0.5% is ink-bearing.
Black eligibility uses K coverage at or above the explicit `blackThreshold`,
including a spot's alternate K. It does not use RGB luminance. CMYK values are
preserved; non-CMYK alternates use the colour library's CMYK conversion.
Excluded objects still take part in nearest-boundary ownership.

PAL name matching is case-sensitive. WHP's `NO WHP` and `NO WHP OR POWDER`
exclusions apply to underprint, not trap tags; `NO POWDER` is not a trap exclusion
either. The standalone command does not execute those underprint actions. Neon names fail explicitly as outside the experiment's scope. Metallic
paints follow the same tag and ink eligibility rules as other spot paints. Neither underprint layer is edited.

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

## Metallic preparation and artist intent

The player supports `RemoveFill` (remove fill layers, preserve strokes) and
`ClipOnShape` (use the topmost selected path as a clip, preserving existing layers,
containers and intervening unselected objects). `MoveObjectsToLayer` and Shaper
copies use the topmost selected layer, or the topmost layer when none is selected,
as specified by ArtPro's active-layer rules (25.07 guide, Layers and Actions sections). The metallic recipe actions run
from the supplied PALs, including Silver underprints, white subtraction, the Light
clip fix, M-1 and Dark renames. Metallic names and inks are accepted by TrapTask;
neon remains unsupported. The existing trap epsilon/K predicate is unchanged.
Clip semantics follow [Esko's public guide, Clip On Shape](https://docs.esko.com/docs/en-us/artproplus/23.03/userguide/pdf/artproplus.pdf).

An optional `intents` parameter supplies the artist's metallic selection:

```json
{
  "intents": {
    "metallic": [
      {
        "objects": {"spot": "Customer Gold"},
        "list": "(2a) Apply Metallic Colour/Set M-2 objects (NZ_US)"
      }
    ]
  }
}
```

`objects` accepts exactly one of `{"ids":[12,15]}`, `{"spot":"ink name"}`,
or `{"rectMm":[x,y,width,height]}`. Rectangle coordinates are document coordinates,
x rightward and y downward, in millimetres; fully enclosed painted bounds qualify.
Spot matching is exact and case-sensitive. Selectors choose atomic objects, not
mixed parent groups; explicit group IDs include their descendants. IDs and bounds
refer to the document **after preparation**, at the intent boundary. Missing,
locked or hidden IDs and empty explicit selections are errors.

Metallic intents execute in array order before the first `(2b)` or `(3)`
folder-qualified list, `Trapping_Supacolour_Standard (NEW)` ticket (including a
bare filename), or `TrapTask`. An explicit `MetallicIntents` entry in `lists` places that
boundary elsewhere; without any boundary it is before the first list. The prior
object selection is restored afterward. Explicit selections bound the 2a list's
attribute selection, so its AddToSelection cannot recolour unrelated objects.
Other PAL actions, including the global cleanup call, retain their literal scope.
Omit `objects` to start that 2a list with an empty selection and let it self-select.
A self-selecting list can also be named directly in `lists`; no intent is required.
The trace records resolved IDs and then every PAL action. No metallic ink choice
is inferred from order text or filenames.

The inspected library has these literal selection behaviours:

- M-1 through M-26 and M-28, including both M-2 variants: AddToSelection of the
  corresponding spot, ignoring case, plus the current artist selection. M-23 is
  an exception: its PAL searches **M-24**, then applies M-23. This is preserved.
- M-27: NewSelection of M-27, ignoring case, when run as a self-selecting list.
  An explicit artist selector (including object marks) takes precedence over the
  opening ink selector, as it does for the other 2a lists.
- PMS 871–876, M-SPOT and M-FLOOD: current selection only; no self-selection.
- No PMS 877 list or PANTONE 877 C selection exists in this library snapshot.
  An explicit PANTONE 877 C spot selector can be supplied to the M-1 list.
- M-28 is selected but never renamed by `M- DARK NAME FIX`. The player does not
  insert a missing rename or otherwise repair the supplied PALs.

Revision: 2026-10-21 — metallic action support and explicit artist selections;
local customer validation is recorded outside the repository.

Metallic validation: all 12 local clean Dark inputs complete; Silver footprints are
within 1 pixel of the shipped plates at 600 dpi. All 37 available metallic
application lists also complete on synthetic Light and Dark art (74 runs).
Process plates are judged by footprint, not tint. Whole-output parity is not
claimed: WHP choke geometry still differs (including one empty underprint), and
TrapTask has small band/owner differences and residual M-1 samples. Intermediate
TrapTask and artwork-only renders isolate those differences before Rasterize and
TRAPPY. A synthetic Dark M-28 retains the PAL's missing DK rename: the ticket's
later DARK rename means no Silver is generated for that case. No exception is
inserted. Customer identifiers, selections and plate tables remain local.

## Combining artist intents

One `intents` object accepts both independently validated keys:

```json
{
  "intents": {
    "metallic": [{"objects": {"spot": "Customer Gold"}, "list": "(2a) Apply Metallic Colour/Set M-2 objects (NZ_US)"}],
    "noTrap": {"objectIds": [19], "rectangles": [{"x": 10, "y": 20, "width": 15, "height": 8}]}
  }
}
```

Metallic lists run at the preparation boundary, before the Mixed ticket; `noTrap`
then replaces the selection immediately before that ticket. Each selector keeps
its own format and meaning: metallic `rectMm` selects fully enclosed objects,
whereas no-trap `rectangles` select intersecting geometry and can split paths.
Neither key is required; unknown keys and invalid payloads are rejected. Synthetic
combined-intent tests cover preparation, metallic selection/paint, no-trap selection
and ticket/TrapTask order with bare and folder-qualified ticket names.


## Persistent separation marks (slice D)

Revision: 2026-10-22 — object marks, public PDF carrier, editor controls and host
messages. Uncommitted for lead review.

Each object has an optional native `sep_mark` attribute:
`{"type":"metallic","list":"(2a) Apply Metallic Colour/Set M-2 objects (NZ_US)"}`
or `{"type":"noTrap"}`. It is metadata, never paint. Native saves retain it.
Marking a group applies to its descendants; marking or clearing one descendant
splits an inherited group mark so its siblings retain their intent. A new mark
replaces the previous kind, rather than combining Metallic and NoTrap on one object.

Commands (CLI, engine, control and MCP):

- `sep.mark.set {"ids":[12],"mark":{"type":"noTrap"}}`
- `sep.mark.set {"mark":{"type":"metallic","list":"(2a) Apply Metallic Colour/Set M-1 objects"}}`
- `sep.mark.clear {"ids":[12]}`
- `sep.mark.list {}` → `{marks:[{id,mark,bounds:{x,y,width,height}|null}],units:"pt"}`

Omit `ids` to use the current selection. Set and Clear are single undo steps;
List is read-only. Bounds use document points, x rightward and y downward. IDs in
summaries are current-session references, not portable selectors. List names are
bounded relative PAL paths; the library resolver still prevents escaping `root`.

PDFs carry marks in private, versioned marked-content tags around the object's
drawing, inside non-isolated Form XObjects. The tag is `VectorCraftSep1_` followed
by hex-encoded JSON; the encoding cannot inject PDF operators. Readers that do not
understand the tag ignore it. It adds no paint, opacity, colour conversion or
printable outline. VectorCraft reads it from public page content, including with
`printProduction:true` and `preserveEditing:false`; the builder never needs the
Reporter to remember IDs. Native editing data, when enabled, also retains the
attribute. One editable object may import as several painted objects, each with
the same mark. Invalid/unknown private tag versions are ignored. External software
that rewrites the content may remove private tags; preservation through such a
rewrite is not claimed.

When `intents` is absent, the builder materializes inherited marks before
preparation and resolves the surviving prepared objects at the metallic boundary.
It groups metallic objects by exact list name in lexical order and runs each 2a
list on that selection. The opening ink selector cannot replace or expand an
explicit artist selection; this also fixes the NewSelection case in M-27. Nested
cleanup and self-selecting list runs retain their literal PAL semantics. NoTrap
marks are resolved immediately before the Mixed ticket and use the established
no-trap path. They require the same one-ticket/immediate-TrapTask sequence as
explicit noTrap intents. Any explicit `intents` object, including `{}`, overrides
all file marks; there is no per-key merge.

Before preparation, both marked and explicit-intent inputs canonicalize path
coordinates to one billionth of a point. PDF page-coordinate inversion otherwise
introduces tiny floating-point differences that curve booleans can amplify into
different underprint edges. This normalization is confined to the builder; it
does not change editor saves or relax the exact pixel comparisons. Separation
metadata also does not trigger the trap engine's raster fallback.

The Attributes panel's **Separation marks** section offers Metallic (M-1–M-28,
both M-2 regional variants, M-SPOT, M-FLOOD and PMS 871–877), No trap (no bleed),
and Clear. Ink/list identifiers are invariant; controls and command labels have
rows in every catalog. Marked objects have coloured bounds outlines in the canvas
only. The supplied library still has no PMS 877 list: that choice records the
intent, but a build reports the missing list rather than substituting an ink.

The same-origin parent bridge accepts:

```json
{"type":"vectorcraft:sep.mark.set","id":"m1","mark":{"type":"metallic","list":"(2a) Apply Metallic Colour/Set M-1 objects"}}
{"type":"vectorcraft:sep.mark.set","id":"m2","mark":{"type":"noTrap"}}
{"type":"vectorcraft:sep.mark.clear","id":"m3"}
{"type":"vectorcraft:sep.mark.list","id":"m4"}
```

Set/Clear target the current selection. Each success replies with
`{type:"vectorcraft:sep.marks",id,marks:[{id,mark,bounds}],units:"pt"}`; failures use
`vectorcraft:failed` with the request ID. Await the reply before exporting.
`vectorcraft:ready` advertises `separationMarks:true`. PDF `vectorcraft:save`
replies (including File > Save) add `marks` and `markUnits:"pt"`, summarized from
the saved file, so selection-only exports describe only their saved objects.
Existing source-colour policy and save acknowledgements are unchanged.

Verification: engine library tests pass (2,709 passed, two ignored), document
tests (242), PDF tests (353 plus two integration tests), web tests (38), and two
headless UI tests. Synthetic coverage includes native persistence, public PDF
reimport without editing data, exact 600 dpi plate equality, set/clear undo,
inherited group marks, authoritative metallic selection, combined Metallic/NoTrap
builder equivalence and explicit-intent precedence. The command sweep and
malformed private-tag import property test pass. Workspace/all-target clippy with
`-D warnings`, changed-file formatting, layering and the wasm check pass. The
local toolchain's wasm standard library was built from its installed Rust sources
for that check. The native window could not start in the sandbox; headless egui
frames and the exported artwork were checked instead.

Local real-file verification used three saved metallic intents (M-1, M-2 NZ_US,
and M-SPOT), with no editing attachment and independent Ghostscript `tiffsep`
renders at 600 dpi. Marked versus unmarked exports are pixel-identical on every
process/spot plate for all three jobs. Against explicit-intent builds from the
original PDFs, two jobs match every plate (nine and ten plates respectively).
The third matches Black and all four spot plates, but fails strict equality on
Cyan, Magenta and Yellow: 2,461, 15,619 and 8,205 pixels respectively differ by
one 8-bit level. Its composite also differs. The remaining cause is the existing
public-PDF gradient resampling/coordinate round trip, also present in an unmarked
save; it is not a metadata paint change. This original-file parity requirement
remains open. All three automatic builds do match equivalent explicit-intent
builds from the saved PDFs, both with marks and from an unmarked-save control,
on every plate and the composite. Customer files, selections and detailed reports stay outside the
repository. The 50k-path performance budgets pass on the final repeat; the first
run had a transient zoom-render budget miss (26 ms versus 16 ms; repeat 10 ms).

## Production refusal boundaries (22 October 2026)

Import and export diagnostics are classified before publication. Unknown or lossy
diagnostics stop the job; outline-text and public-page clipping notes are print
preserving. Skipped images and spot/RGB fallback stop at Import. An unsupported
page transparency blending space stops only when printed transparency uses it;
opaque artwork records an Info diagnostic instead. Export failures and fidelity
warnings stop at Export. Preparation explicitly restores retained page blending
and output intent settings after the requested colour conversion, before validating
the final output.

Ink selection reads CMYK/DeviceN image planes. Conversion/deletion of affected
nonzero image channels currently stops before any paint or swatch is changed.
Provably empty spot channels are removed exactly along with their unused swatches;
unsupported or undecodable image channel selection also stops. Registration `/All`
images are refused because the image storage model cannot retain every-output-colorant
semantics. Marked meshes are never coalesced into one shading.

Underprint checks the full ancestor composition, including cached trap footprints.
Zero opacity contributes nothing; opacity one, Normal blending and proven no-op
effects are accepted. Mask coverage is proved over the clipped content using
geometry and exact decoded image samples. Partial, zero or unknown coverage stops
unless a selected additive contribution is already wholly inside the established
underprint, making its mask irrelevant to the union. That exception never applies
to unselected knockout art or ordinary path operations, and the source mask remains
on the exported artwork. Synthetic tests cover opaque, partial, zero, disabled,
inverted and clipped masks, bitmap coverage and redundant selected contributions.

Trap boundary ownership excludes opaque paint covered by later paint and uses
frontmost ties. Unresolvable compositions use renderer sampling. Bulk paint geometry
has limits of 5 million input elements, 5 million generated topology segments,
15 million work steps and 500 million winding-storage bytes, calibrated at 7.6–9.6
times the measured large-art workload. Cancellation and a ten-minute wall-clock
ceiling are checked inside the topology sweep; exceeding a limit returns a specific
error. Adversarial intersections, memory, cancellation and timeout tests still stop
cleanly. Attribute selection accepts All/Any/None and refuses unknown operators.
The web bridge processes all parent operations in one ordered queue, including
mutation followed by export.

Validation (22 October 2026): engine, PDF, document, pathops and web suites pass
(3,799 tests passed, three ignored); native/all-target and wasm clippy pass with
`-D warnings`. Changed Rust files pass formatting and the diff passes whitespace
checks. No customer fixtures or profiles are checked in.

The October-list real-file rerun completes 44 of 45 regular jobs. The remaining
Import stop identifies actual printed Multiply content in an unsupported ICC-based
page blending space. A controlled RIP comparison confirms that replacing that
space changes the process plates. The formerly rejected ancestor-mask job completes:
its masked white border lies over opaque white backing, and removing both source
masks changes no source colorant pixels at either 600 or 1,200 dpi. The underprint
union is already covered geometrically.

Of the completed jobs, 40 have identical 600 dpi Ghostscript plates against the
base tree. Two metallic jobs retain nonzero image spot samples previously omitted
by ink selection, and two mixed jobs correct covered-paint trap ownership. The
metallic Silver and M-1 plates are slightly further from shipped; WHP is unchanged
in one job and slightly further in the other. One job already lacks shipped WHP
in both baseline and current output. Complete plate deltas, comparison methods and
job identities remain in the local corpus report; production parity is not claimed.
The large performance input completes within the calibrated geometry limits in
92.37 seconds, versus 90.82 seconds for the baseline.

Standalone trapping on shipped Artwork retains its existing colour result across
all eleven dark files: shared-trap mean error 0.0389 percentage points, 99.913% within
2 pp; every ink-bearing job meets mean <1 pp and >99% within 2 pp. The white-only
control has no trap. This does not establish customer-to-shipped preparation-colour
parity: that separate builder comparison remains unchanged at mean 1.5532 pp and
72.061% within 2 pp.
