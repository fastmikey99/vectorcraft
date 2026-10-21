# hayro-syntax and hayro-interpret: a content stream limit

Upstream: hayro-syntax 0.7.2 and hayro-interpret 0.7.0 as shipped in craft-libs
(https://github.com/fastmikey99/craft-libs), revision
049a48689e8488148ccd72b5d061baf8bb0810d9, directories `vendor/hayro-syntax` and
`vendor/hayro-interpret` (themselves the PdfCraft snapshot recorded in each `SOURCE.json`),
MIT OR Apache-2.0. The licence texts and assets are retained. Each `SOURCE.json` is kept
verbatim: its local hashes describe the craft-libs files, so the patched files below differ
from them. The `Cargo.lock` files (unused for a path dependency) and cargo's `.cargo-ok` /
`.cargo_vcs_info.json` markers are left out.

Why: real drawings can have a page or form content stream that decodes past 256 MiB (an
Inkscape 22 x 180 in sheet stores a 128 MB form that decodes to 391 MB). Every stream stopped
at `MAX_DECODED_STREAM`, 256 MiB, so the form was cut short, and
`DecodeLimits::with_max_decoded_stream` can only lower that limit.

What changes (each marked `VectorCraft patch` in the source):

- hayro-syntax `filter/mod.rs`: `MAX_DECODED_CONTENT`, 512 MiB, beside the unchanged
  `MAX_DECODED_STREAM`. 512 MiB is hayro-interpret's `MAX_PAGE_CONTENT`, the decoded content
  one page may hold, so a stream that fits the reader also fits the interpreter.
- hayro-syntax `pdf.rs`: `DecodeLimits::max_decoded_content()` (the content limit, or the
  ordinary one once a caller has lowered it) and the crate-private `with_content_limit()`.
- hayro-syntax `object/stream.rs`: `Stream::decoded_content_within(max)`, which decodes with
  the content limit; `decoded_within`, `decoded` and `decoded_image` keep the ordinary limit.
  The crate-private `decoded_raw_within`, used only for a page's contents, takes the content
  limit too.
- hayro-syntax `page.rs`: `Page::content_reader` decodes the page's whole-decoded streams
  within the content limit, shared by all of them (it was `MAX_DECODED_STREAM`), so they never
  hold more than the interpreter's page budget they are charged against.
- hayro-interpret `context.rs`: `decode_content` (forms, tiling patterns, Type 3 glyphs)
  decodes with `decoded_content_within` and the content limit, within what is left of the
  page's budget, as before.

Nothing else moves: object streams, cross-reference streams (also when encrypted or found by
repair), fonts, profiles, functions, shadings, images and `Page::page_stream` decode with
the ordinary 256 MiB limit. VectorCraft's own screen (`crates/pdf/src/streams.rs`) also
refuses, before the reader runs, any stream past 256 MiB except one used as content that
expands at most 32 times, up to the same 512 MiB.

The workspace patches both copies in with `[patch.crates-io]`; hayro-jbig2 and krilla still
come from the same craft-libs revision. Drop the copies and point the patch back at craft-libs
when upstream decodes content with a limit of its own (or lets a caller raise the ordinary
one), then rerun the `streams` tests and a large-content import.
