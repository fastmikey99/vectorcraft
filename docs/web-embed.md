# Embedding the web build

A page on the same site can run VectorCraft's web build (`apps/vectorcraft-web`) in an `<iframe>`, hand it a file to open, and store what it saves instead of letting the browser download it. A file manager's "Edit in VectorCraft" (such as a Nextcloud app) is the intended use. The page talks to VectorCraft with `postMessage`. Code: `apps/vectorcraft-web/src/host.rs` (the bridge) and `src/protocol.rs` (its rules, tested natively).

## Turning it on

Load the web build with `?host=parent` in a frame on the **same origin** as the page:

```html
<iframe id="vectorcraft" src="/vectorcraft/index.html?host=parent"></iframe>
```

Without `?host=parent`, or when the page isn't framed, nothing changes: no listener is installed and saves download as usual. A page with an opaque origin (a sandboxed frame without `allow-same-origin`, a `file:` page) gets no bridge either.

What changes in host mode:

- Everything VectorCraft would download goes to the parent as `vectorcraft:save` instead: Save, Save As, Save a Copy and the exports. There is still no save panel, so the parent decides where each file goes.
- The parent is told when unsaved changes appear or go away (`vectorcraft:dirty`). VectorCraft doesn't ask before the page is left; guarding that is the parent's job.

## One document

In host mode the editor works on the file the page hands over, and only that:

- Nothing creates, opens or closes a document: File › New, New from Template, Open, Open Recent and Close are gone, as are the Home screen and its buttons. The command palette and shortcuts don't run them either.
- A file the person drops or picks opens only when it isn't a document (a swatch or graphic style library, presets, a plug-in). A document is turned away with a note in the status bar.
- Each `vectorcraft:open` takes the open document's place. It doesn't ask to save the one it replaces: the page knows from `vectorcraft:dirty` whether it has unsaved changes and decides.
- Data Recovery is off, since restoring copies would add documents.
- `vectorcraft:ready` carries `singleDocument: true`, so a page can tell that the build it loaded works this way.

So `dirty` and every `save` are about that one document.

## Messages

Every message is a plain object with a string `type`. VectorCraft posts only to `window.parent`, with its own origin as the target origin.

| `type` | Direction | Fields | Meaning |
|---|---|---|---|
| `vectorcraft:ready` | to the parent | `version` (string), `singleDocument` (`true`) | The app is running and listening. Send the file now. |
| `vectorcraft:open` | from the parent | `name` (string), `bytes` (`ArrayBuffer` or `Uint8Array`) | Open this file, as if it were picked with File › Open. |
| `vectorcraft:export` | from the parent | `id` (any), `format` (string, optional), `markSaved` (boolean, optional), `selectedOnly` (boolean, optional), `imageOnly` (boolean, optional), `preserveEditing` (boolean, optional, PDF only) | Write the active document (or only the selection) as `format` (PDF without it) and send it back. |
| `vectorcraft:place` | from the parent | `id` (any), `name` (string), `bytes` (`ArrayBuffer` or `Uint8Array`), `at` (`{x, y, width, height}`, optional) | Put this file in place of the selected object, fitted inside `at`. |
| `vectorcraft:placed` | to the parent | `id`, `ids` (numbers) | The place with this `id` is done. |
| `vectorcraft:solidify` | from the parent | `id` (any), `level` (whole number 1–255, optional, 128), `copies` (boolean, optional) | Remove Soft Pixels on the one selected image: every pixel fully opaque or fully clear. |
| `vectorcraft:solidified` | to the parent | `id`, `ids` (numbers), `form` (`"alpha"` or `"mask"`), `level`, `softPixels`, `clearPixels`, `opaquePixels` (numbers) | The solidify with this `id` is done. |
| `vectorcraft:save` | to the parent | `name` (string), `bytes` (`ArrayBuffer`, transferred), `id` (when it answers an export), `bounds` (with `selectedOnly`), `native` (`true` when the bytes are an image's own pixels) | Store this file. |
| `vectorcraft:colorSettings` | from the parent | `id` (any), `profiles` (`[{name?, bytes}]`, optional), `rgb`, `cmyk` (profile names, optional), `intent` (optional), `bpc` (boolean, optional) | Load these ICC profiles and set Edit › Color Settings with them. |
| `vectorcraft:colorSettingsApplied` | to the parent | `id`, `loaded` (names), `rgb`, `cmyk`, `intent`, `bpc`, `profiles` (every name available) | The colour settings with this `id` are in effect. |
| `vectorcraft:prefs` | both ways | `prefs` (string: JSON text) | From the editor: its preferences and workspace changed; store them. From the parent: put these on. |
| `vectorcraft:dirty` | to the parent | `dirty` (boolean) | Unsaved changes appeared (`true`) or went away (`false`). |
| `vectorcraft:failed` | to the parent | `error` (string), `id` (when it answers an export) | Before `ready`: the app couldn't start. After `ready`: the last `vectorcraft:open` was refused, or with an `id` that export was; the app keeps running. |

## Reporter Hybrid contract (2026-10-17)

The new bridge advertises `preserveSource: true`, `printProduction: true` and
`openAcknowledgement: true` alongside `singleDocument: true` in `vectorcraft:ready`.
Hosts must check these capabilities before relying on them; the app version alone
is not a protocol negotiation.

- `vectorcraft:open` accepts `printProduction: true`. It opens all public PDF pages,
  media frames, layers and print settings, bypassing private editing data. Ordinary
  opens retain their prior defaults. `vectorcraft:opened` is sent only after a
  document has actually opened; a failed import sends `vectorcraft:failed`.
- `vectorcraft:export` accepts `output: {conversion: "preserveSource"}` for PDF.
  The bridge uses the existing Separation Roundtrip preset to retain PDF layers
  and avoid downsampling or image smoothing. The output policy retains source
  colour models and values, including named spot inks,
  through the shared writer. Conflicting/unknown policies are refused. A successful
  response carries `sourcePreserved: true`; unsupported export features fail.
- `markSaved` takes effect only after the buffer has been handed to the parent.
  The parent still owns persistence failures and navigation protection.
- A menu save can use remembered PDF options. Reporter requests a fresh export with
  its explicit Hybrid policy before persisting such a save. DF retains its existing
  export defaults. This bridge has no authority to release artwork to CodeWolf.

Production import refuses artwork exceeding safe editable nesting limits before
clipping or opacity-mask information is removed. The host receives
`vectorcraft:failed`, without an `opened` acknowledgement. This does not provide
editing support for those deeply nested files.

Source preservation is a colour-output policy, not proof that arbitrary PDFs are
lossless. Corpus qualification must compare page frames, layers, inks and rendered
coverage. Tests: `operations::tests`, `protocol::tests`, UI single-document and
placement regressions, plus Reporter's editor message tests.

**`vectorcraft:ready`** comes once, when the app has started. `version` is the app's version (`CARGO_PKG_VERSION`). A host that has heard neither `ready` nor `failed` after a reasonable time should assume the app couldn't start. The commonest reason is a browser without WebGPU or WebGL2.

**`vectorcraft:open`** needs both fields:

- `name` is the file's name, such as `Logo.svg`. Its extension decides how the file is read, as for a picked file, so anything File › Open reads can be sent. It also becomes the document's name, which saves are named after.
- `bytes` is the file's contents. The buffer may be transferred.
- A message without a non-empty `name`, or without `bytes` of one of those two types, is refused: VectorCraft answers `vectorcraft:failed` and opens nothing.
- A file that has both but can't be read (an empty or damaged file) is reported inside the app, like any file that fails to open.

**`vectorcraft:save`**: `name` is a bare file name with no folders, the name a download would have had. The parent stores the file and reports any failure to the user itself; nothing is sent back. VectorCraft treats the save as done, as it does a download: after Save or Save As the document counts as saved, and `dirty: false` follows.

**`vectorcraft:export`** lets the page's own button (a "Save to job", say) ask for the file instead of relying on File › Save or Export:

- `format` is any format File › Export writes, by id or extension (`pdf`, `svg`, `png`…); without it, PDF. The file is written with that format's default options.
- The answer is a `vectorcraft:save` carrying the request's `id` (pass a number or string to match answers to requests), named after the document with the format's extension.
- With `markSaved: true` the document counts as saved once written (the page is storing it as the saved file), and `dirty: false` follows. Otherwise exporting leaves the unsaved state as it was, as File › Export does.
- With no document open, or a format the app doesn't write, the answer is `vectorcraft:failed` with the `error` and the same `id`.
- `imageOnly: true` (a raster format; implies `selectedOnly`) accepts only one selected image and answers with its own pixels, `native: true`. Anything else selected (a group, a path, type, several objects, nothing) is refused with `vectorcraft:failed`, saying what is selected: `select one image (a group is selected)`. A page that pays a tracing service per image uses it so it never sends a render of a group or a page.
- `preserveEditing: false` (PDF only) leaves out the native document a PDF otherwise carries for editing it again (File › Save as PDF's Preserve Editing Capabilities): the file is about the size of the art alone, and reopening it reads the PDF as printed. Left out, the PDF keeps it. With another format it is refused.
- With `selectedOnly: true`, only the selection is written, and the `save` carries its `bounds` (`{x, y, width, height}` in document points). One selected image asked for in a raster format (`png`, `jpeg`…) comes as its own pixels, as they are embedded (PNG, JPEG, GIF or WebP as stored, others as PNG), not drawn again at screen resolution: what a tracing or upscaling service needs. `markSaved` can't go with it (a part isn't the document), and with nothing selected the answer is `vectorcraft:failed`.

**`vectorcraft:place`** puts a file into the open document, never opening it as a document of its own, so it works in host mode:

- With one object selected, the file takes that object's place in the stacking order. Without `at` it is fitted onto that object's own frame through its transform: for an image, the file's frame (an SVG's viewBox) goes onto the image's pixel area, rotated, mirrored, scaled or skewed as the image is, so a tracing of the image lies exactly where its pixels did. With `at` it is fitted inside that rect instead (upright). Aspect is kept either way. With nothing selected it is placed fitted inside `at`, which is then required. Several selected objects are refused.
- It is one undo step: Undo brings back what it replaced. What it placed is selected.
- The answer is `vectorcraft:placed` with the same `id` and the placed objects' `ids`, or `vectorcraft:failed` with the `error` and the `id`.

**`vectorcraft:solidify`** removes the soft pixels of the one selected image in the editor, without sending its pixels anywhere (`object.image.solidifyAlpha`). `ready` says `solidify: true`; a page talking to an editor that doesn't say so keeps its own fallback.

- Every alpha at or above `level` (1–255 on the 8-bit scale; 16-bit alpha is cut at `level × 257`) becomes fully opaque, every one below it fully clear. Colour samples are copied as they are (never through premultiplied colour), so soft pixels keep their exact colour values, and the pixel grid doesn't change.
- It works on the image's own stored samples: its alpha channel (a PNG, RGBA or grey + alpha, 8 or 16 bits, keeping its colour type, depth and colour/resolution chunks; a WebP or GIF becomes a PNG), or, when the image has an opacity mask that is one greyscale image (a CMYK image read from a PDF with a soft mask), that mask's image; the CMYK samples are then untouched. `form` says which (`"alpha"` or `"mask"`).
- The result is decoded again and checked before it is kept: every alpha is 0 or full and equals the thresholded source at every pixel, every colour sample is unchanged. If not, nothing changes and the answer is `vectorcraft:failed`.
- With `copies: true` every other image showing the same pixels and transparency at the same printed size gets the same result; copies at other sizes keep theirs. `vectorcraft:selection`'s `copies` counts by pixels and size only: a copy with a different opacity mask (or a mask on one and not the other) is counted there but not changed here, since this image's result isn't its transparency; the reply's `ids` lists what was changed. Either way it is one undo step.
- `ids` are the images changed. They are empty, with no undo step, when no pixel was soft (`softPixels: 0`).
- Refused with `vectorcraft:failed` (the `error` and the `id`): nothing or several objects selected, the selection isn't an image, a linked image, an image with no transparency (`the image has no transparency: nothing to remove`), an opacity mask that isn't one greyscale image, a mask whose image has transparency of its own (opacity, a blend mode, effects or a mask of its own: `the image's opacity mask has its own transparency: release it first`), or an image whose own alpha is not fully opaque that also has a mask (own alpha that is fully opaque everywhere plus a mask is accepted: the mask is the effective transparency and is thresholded). The message's `id` is only the request's: the image is always the selected one. (Underneath, `object.image.solidifyAlpha` takes the selection only when its own `id` parameter is absent; any `id` given, `null` included, that doesn't name an image is refused with `id must name an image`. The bridge always passes the selected image's id.) `level` that isn't a whole number from 1 to 255, or `copies` that isn't a boolean, are refused before anything runs.

Together they make a "trace this image" button: export the selected image with `selectedOnly`, send the pixels to a tracing service, and place its SVG back (with no `at`) while the image is still selected.

**`vectorcraft:prefs`** lets the page keep the editor's preferences and workspace (units, the panel layout, workspaces, recent fonts…) for each person rather than each browser:

- In host mode the editor keeps none in the browser's storage. When they change (checked every two seconds) it sends them as `prefs`, JSON text the page stores as it is, for whoever is signed in.
- Send them back, as they came, after `ready` and before `vectorcraft:open`; the editor puts them on. Text that isn't what the editor gave (not a JSON object, or over 1 MB) is refused with `vectorcraft:failed`.
- Someone with nothing stored starts from the defaults; their first change is sent to the page.

Preferences OK sends a snapshot after the dialog commits, without waiting for the periodic
workspace poll. General Units and grid preferences update display settings without marking
saved artwork dirty or consuming Undo; existing unsaved artwork remains dirty.

**`vectorcraft:dirty`** is sent after the first frame and then whenever the answer changes, so the first one is usually `dirty: false`. `true` means at least one open document has unsaved changes.

## Who is heard

The bridge reads a message only when all of these hold:

1. it comes from `window.parent` (`event.source`), not from another window or frame on the site;
2. its origin (`event.origin`) is exactly the page's own: same scheme, host and port;
3. its `type` is `vectorcraft:open`, `vectorcraft:export`, `vectorcraft:place`, `vectorcraft:solidify`, `vectorcraft:colorSettings`, `vectorcraft:prefs` or `vectorcraft:hello`.

Anything else is ignored without a reply, so a page on another origin can neither open files in VectorCraft nor learn that it is there. Saves and other replies go only to `window.parent`, with the page's own origin as the target, so the browser delivers them to no other site. A same-origin parent can already script the frame directly, so the bridge gives it nothing it couldn't otherwise do; it only spares it reaching into the app's internals.

## Compatibility

Hosts should ignore message types and fields they don't know; VectorCraft does the same. `ready`'s `version` lets a host tell which build it is talking to.

## Example host

A minimal page (error handling left out) that opens a file it fetched and writes saves back next to it:

```js
const frame = document.getElementById('vectorcraft')
window.addEventListener('message', async (event) => {
  if (event.origin !== window.location.origin || event.source !== frame.contentWindow) return
  const message = event.data
  switch (message.type) {
    case 'vectorcraft:ready': {
      const bytes = await (await fetch('/files/Logo.svg')).arrayBuffer()
      frame.contentWindow.postMessage({ type: 'vectorcraft:open', name: 'Logo.svg', bytes }, window.location.origin, [bytes])
      break
    }
    case 'vectorcraft:save':
      await fetch(`/files/${encodeURIComponent(message.name)}`, { method: 'PUT', body: message.bytes })
      break
    case 'vectorcraft:dirty':
      unsaved = message.dirty
      break
    case 'vectorcraft:failed':
      console.error('VectorCraft:', message.error)
      break
  }
})
let unsaved = false
window.addEventListener('beforeunload', (event) => { if (unsaved) event.preventDefault() })
```

### Production Separations (Reporter fork)

The ready message advertises `productionSeparations: true`. An open can include
`separations: {label, inks: [{name, destination: "Indigo"|"Screen", components:
[{name, layer}]}], source_layers: [name]}`. Names are exact, case-sensitive file-link
names supplied by the host's recipe database. This is viewing context, not a recipe
execution request. The host must resolve conflicts rather than infer destinations.

The native **Window → Production Separations** panel keeps process artwork together
as CMYK under Indigo. Explicit named inks take precedence over an Indigo source
layer; unmatched inks remain Unassigned. Component children are offered only for
existing named groups on their configured PDF layer. A flattened legacy plate is
never split by inference. Clicking a row selects existing objects for normal editing;
a multi-ink object is selected whole. Eyes are view state and do not remove artwork
from a save/export. Source RGB and CMYK values and spot names are not converted by
this panel. This is separation inspection, not a calibrated finished-transfer proof.

Context is validated before replacing a file. `opened` is sent after context is
installed. A host can retry `vectorcraft:hello` until it receives `ready`; repeated
ready replies must not reopen the document. Context is supplied on each host open;
it is not embedded as production recipe authority in the PDF. Named object groups
persist in native documents; public PDF import cannot reconstruct group roles that
were flattened or omitted by an exporter.

### Colour settings from the page (Reporter fork)

The ready message advertises `colorSettings: true`. A page whose files are made with particular
ICC profiles (Supacolour's prep worker: U.S. Web Coated (SWOP) v2, Adobe RGB (1998), sRGB) hands
the editor the same profiles and settings, so a sep shows, and is saved, in the colours the worker
made it in. The web build can't read profiles from disk (`color.loadProfile` is native only), and
without this it works in its defaults: sRGB, VectorCraft Generic CMYK, relative colorimetric.

```js
post({ type: 'vectorcraft:colorSettings', id: 'cs1',
  profiles: [{ bytes: swopIcc }, { bytes: adobeRgbIcc }, { bytes: srgbIcc }],
  cmyk: 'U.S. Web Coated (SWOP) v2', rgb: 'Adobe RGB (1998)', intent: 'perceptual', bpc: true })
// then vectorcraft:open
```

- Each profile is registered under `name`, else the description it carries, and then appears
  under that name in the Separations panel's Color Settings menus. A built-in name is refused.
  At most 16 profiles of at most 32 MB each.
- `rgb`, `cmyk`, `intent` (`perceptual`, `relative`, `saturation`, `absolute`) and `bpc` set the
  working spaces as Edit › Color Settings does; a field left out stays as it is. A space must be a
  built-in profile or one registered (by this message or earlier).
- They are applied at the next frame **before** that frame's `vectorcraft:open` is read, so send
  them before the open: the file is then read in them (see below). They last for the page's
  life (every later open), like the person's own Color Settings; the page sends them again after
  a reload. They are not preferences (`vectorcraft:prefs` doesn't carry them) and not an edit:
  no `dirty: true`, no undo step.
- Answered with `vectorcraft:colorSettingsApplied`, or `vectorcraft:failed` with the `id` (an
  unreadable profile, an unknown space, a field of the wrong type, an empty message, more than
  64 MB of profiles together). The profiles and the spaces are checked before anything is
  registered: a refused message registers nothing and leaves the settings unchanged. A profile
  registered again under the name of a working space in use takes its place at once.

**Tagged RGB lands in the RGB working space.** A PDF's `ICCBased` RGB colours used to be read
through their profile into sRGB numbers (and a profile naming the sRGB device model was taken as
DeviceRGB). With an RGB working space other than sRGB those sRGB numbers were then taken as
working-space numbers: a file tagged with the working space itself (the prep worker's Adobe RGB
output, read with Adobe RGB as the working space) came out more saturated each time it was read
(ΔE2000 about 4–7 per round on Supacolour's seps), and an sRGB-tagged customer file more saturated
once (ΔE2000 up to about 10 on saturated colours). Now, with a working space other than sRGB:

- RGB in any profile is converted into the working space, directly (not through sRGB) with the
  colour settings' intent (black-point compensation, which applies to CMYK sources, changes
  nothing between RGB profiles); colours in the working space's own
  profile (the same file, or one giving the same colours) keep their numbers exactly. Solid
  fills, strokes and text, axial, radial and mesh shadings alike.
- On a page with several RGB profiles, or with sRGB-tagged and DeviceRGB colours together, the
  page's walk of its content says which colour is in which space (when it can be matched with
  the reader's paths; otherwise a tagged colour goes through sRGB into the working space and a
  DeviceRGB-looking one keeps its numbers). The walk doesn't follow tiling-pattern cells, soft
  masks or Type 3 glyphs: there, in a file whose only RGB profile is an sRGB one, a
  DeviceRGB-looking colour keeps its numbers (it may be DeviceRGB). Text shown in several RGB
  spaces between two painted paths can't be told apart by glyph either (as for spot inks).
- DeviceRGB numbers are the working space's, and stand.
- Colours the reader can only give as sRGB (Lab, CalRGB, Indexed, mixed DeviceN inks, and any
  colour painted under a transfer function, `TR`/`TR2`) are converted from sRGB into the working
  space instead of being taken as its numbers.
- Images are unchanged: they are sRGB in a document, as before (colours outside sRGB clip).

With sRGB as the working space nothing changes. This holds for the CLI and desktop app too (any
`document.open` or place of a PDF).

**A PSD is read in the profile it embeds.** An 8- or 16-bit RGB Photoshop document's merged image
is converted from its embedded ICC profile (resource 1039) into the sRGB pixels a document's images
are; an sRGB profile (or none) leaves the numbers as they are. Before, the profile was ignored and
the numbers read as sRGB whatever the working space (choosing the working space didn't change
it), so an Adobe RGB PSD came out duller. Colours outside sRGB clip, as for any image. CMYK,
grey, Lab and 32-bit documents read as before.

**Images keep their colour on export.** Converting images to an RGB destination (`output:
{conversion: "destination"}`) uses a 65³ table instead of 17³: an sRGB image written as Adobe RGB
moved by up to 1 ΔE2000 near black, now about 0.6 at most (8-bit rounding).

**What the host sends.** Give the editor the colour settings the next prep will read its save
with: the same `rgb` (and `cmyk`, `intent`, `bpc`). A PDF saved with Preserve Editing (the default)
reopens from the native document it carries, whose RGB numbers are the editor's working-space
numbers, whatever the PDF's tags say; a working space that differs on the way back shifts them.
`vectorcraft:export` accepts `output: {conversion: "preserveSource"}` only; a tagged destination
export isn't available to the page.

### Per-job units (Reporter fork)

The ready message advertises `units: true`. An open can include two optional fields:

- `units`: the opened document's General unit (rulers, positions, sizes, dialog
  distances), such as `"inches"` or `"millimeters"`.
- `strokeUnits`: the Stroke unit (weights, dashes) while that document is open.

Each is a string naming a unit as Preferences › Units names them: the key, label or
suffix in any case, ignoring spaces and punctuation (`inches`, `Inches`, `in`,
`millimeters`, `mm`, `points`, `pt`, `picas`, `centimeters`, `pixels`, `feetInches`,
`meters`, `yards`, `feet`). Anything else (a number, an unknown name) is refused
with `vectorcraft:failed` (`vectorcraft:open: units must name a unit, such as inches
or millimeters`) and nothing opens.

They are display choices for this job, not edits and not preferences:

- `units` is applied to the document the open makes, also a native document saved in
  other units, and also after the Import PDF dialog. The document stays unmodified:
  no `dirty: true`, no undo step. A later save carries the units it shows.
- Neither changes the person's stored preferences, so the `vectorcraft:prefs` the
  editor sends back keep their own Units › General and Stroke (a person who works in
  millimetres is not left in inches by one US job).
- Preferences › OK stores Units › General and Stroke only when the person changed
  them in the dialog: pressing OK during an inches job leaves their stored units as
  they were.
- Every open sets them for its job: an open without `strokeUnits` shows strokes in
  the person's Stroke preference again; one without `units` opens in the file's own
  units or the preference, as before. Choosing a different Stroke unit in
  Preferences ends the page's choice.

### Production preview and floating panels (2026-10-17)

Host configuration starts the ink composite immediately, with visibility stored on
that document rather than in the global proof settings. Show all restores every
plate and component using that same separation-preview mode; it does not return
to ordinary PDF painting, where an opaque top powder plate can hide the artwork.
The recipe-provided ink order is retained within the production panel. These are
view settings only: PDF geometry, stacking, ink names and saved output are unchanged.
Shared docking now supplies opaque panel and tab backgrounds to floating groups.
Regression coverage compares actual composite pixels across load, ink isolation
and Show all, verifies the source document stays unchanged, and inspects the
floating panel's painted background.

### Overprint Preview in the production panel (2026-10-18)

The production panel has its own Overprint Preview checkbox, off by default, so
operators no longer switch to the native Separations Preview panel to toggle it.
Off, each ink covers what is under it (an opaque top powder plate hides the
artwork, as in ordinary viewing); on, overprinting inks are see-through. Plate
eyes and component eyes work either way. The choice is per document view state,
survives the host re-supplying context, and never changes saved artwork.
