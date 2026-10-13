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
| `vectorcraft:export` | from the parent | `id` (any), `format` (string, optional), `markSaved` (boolean, optional) | Write the active document as `format` (PDF without it) and send it back. |
| `vectorcraft:save` | to the parent | `name` (string), `bytes` (`ArrayBuffer`, transferred), `id` (when it answers an export) | Store this file. |
| `vectorcraft:dirty` | to the parent | `dirty` (boolean) | Unsaved changes appeared (`true`) or went away (`false`). |
| `vectorcraft:failed` | to the parent | `error` (string), `id` (when it answers an export) | Before `ready`: the app couldn't start. After `ready`: the last `vectorcraft:open` was refused, or with an `id` that export was; the app keeps running. |

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

**`vectorcraft:dirty`** is sent after the first frame and then whenever the answer changes, so the first one is usually `dirty: false`. `true` means at least one open document has unsaved changes.

## Who is heard

The bridge reads a message only when all of these hold:

1. it comes from `window.parent` (`event.source`), not from another window or frame on the site;
2. its origin (`event.origin`) is exactly the page's own: same scheme, host and port;
3. its `type` is `vectorcraft:open` or `vectorcraft:export`.

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
