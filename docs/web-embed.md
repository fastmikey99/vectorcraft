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
| `vectorcraft:save` | to the parent | `name` (string), `bytes` (`ArrayBuffer`, transferred), `id` (when it answers an export), `bounds` (with `selectedOnly`), `native` (`true` when the bytes are an image's own pixels) | Store this file. |
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

Together they make a "trace this image" button: export the selected image with `selectedOnly`, send the pixels to a tracing service, and place its SVG back (with no `at`) while the image is still selected.

**`vectorcraft:prefs`** lets the page keep the editor's preferences and workspace (units, the panel layout, workspaces, recent fonts…) for each person rather than each browser:

- In host mode the editor keeps none in the browser's storage. When they change (checked every two seconds) it sends them as `prefs`, JSON text the page stores as it is, for whoever is signed in.
- Send them back, as they came, after `ready` and before `vectorcraft:open`; the editor puts them on. Text that isn't what the editor gave (not a JSON object, or over 1 MB) is refused with `vectorcraft:failed`.
- Someone with nothing stored starts from the defaults; their first change is sent to the page.

**`vectorcraft:dirty`** is sent after the first frame and then whenever the answer changes, so the first one is usually `dirty: false`. `true` means at least one open document has unsaved changes.

## Who is heard

The bridge reads a message only when all of these hold:

1. it comes from `window.parent` (`event.source`), not from another window or frame on the site;
2. its origin (`event.origin`) is exactly the page's own: same scheme, host and port;
3. its `type` is `vectorcraft:open`, `vectorcraft:export` or `vectorcraft:place`.

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
