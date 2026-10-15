# Images in pattern cells

`links.list` lists page objects. Images in a pattern definition belong to its cell,
so they are addressed by separate engine commands, available to native and web callers:

- `links.patternImages {}` lists resources grouped by shared image bytes. `ids` contains
  their cell node ids; any of those ids can address the resource. The canonical `id` is
  the lowest one. Ids survive replacement and native save/open; importing a PDF again
  creates a new document with its own ids.
- `links.exportPatternImage {id, path?}` returns the stored image without rendering or
  colour conversion. Without `path`, the result contains `name`, `mime`, and `dataBase64`.
  With `path`, its extension must match the stored format. The resource stays embedded.
- `links.replacePatternImage {id, path}` or `{id, name, dataBase64}` accepts a PNG and
  replaces all pattern-cell images sharing that resource, in one undo step. It fully
  decodes the PNG before editing. The new bytes remain shared and embedded.

Each replacement keeps the old unit-square-to-document transform:
`newXf = oldXf * scale(oldWidth/newWidth, oldHeight/newHeight)`. Thus changing pixel
resolution or aspect ratio preserves the physical frame, including skew and reflection.
The PNG's resolution metadata does not resize the artwork. Page nodes, pattern paints,
cell dimensions, clipping paths, opacity and masks are unchanged. An existing node
opacity mask still applies in addition to the new PNG alpha.

The list reports native pixel dimensions, the stored colour space, and whether the
stored file has an alpha channel. It does not reconstruct the original PDF's colour
space declaration or distinguish an original SMask from another source of alpha.
For 8-bit DeviceRGB images with a same-sized 8-bit DeviceGray SMask, synthetic PDF tests
verify exact unassociated RGBA samples, including colour under zero alpha.

`useCount` counts distinct page objects painting the resource. `tileCount` and
`placements` describe candidate tile image frames intersecting paint and clipping
bounds, including partial tiles. Each placement has its page-object `ownerId`, a
unit-square transform, image `bounds`, intersected `clipBounds`, and 1-based `artboards`,
all in document points with y down. Bounds are conservative for nonrectangular clips,
rotated shapes and strokes. This is geometric resource usage, not an opaque-pixel or
occlusion analysis. A tiny overlap at a cell boundary can produce an extra tile placement
without another page-object use. Traversal is bounded and excessive nesting fails explicitly.

Patterns stay editable and repeated images stay shared. Exporting to PDF uses the
existing exporter: it emits clipped instances of the cell art and shares image data;
it does not preserve the source PDF's tiling-pattern resource syntax. Disable image
downsampling when preserving a high-resolution replacement in PDF. VectorCraft's
screen/raster pattern renderer currently caps a rasterized tile at 2048 pixels per side;
native image export and PDF export do not use that preview tile.

Scope: pattern art and pattern paints in node appearances in the page tree, including nested patterns and
ordinary clipping groups. This is not a new Links panel UI or a source-PDF object editor.
Symbol/repeat expansion, text-run paints, opacity-mask-only pattern uses, arbitrary live-effect placement
analysis, linked pattern resources, and exact visible-area enumeration are not added.
Standalone page images sharing the same bytes are not replaced by this pattern command.
Use native save/open to retain editable patterns; a PDF round trip has the exporter's
existing expansion behavior. A host embedding bridge needs request handlers that call these engine commands; the fork's
existing selection-only export/place messages do not address cell nodes. This upstream change
does not add those fork-specific message handlers.

Evidence: `crates/engine/src/tests_patternimages.rs` builds its PDF streams from scratch.
It covers Forms, SMask bytes, multiple tiles and uses, multiple pattern definitions,
replacement at 8× pixel dimensions, affine frames, failure atomicity, undo/redo,
native persistence and PDF output samples.
