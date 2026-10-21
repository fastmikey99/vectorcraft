# hayro-syntax: a larger ordinary stream limit

Upstream: hayro-syntax 0.7.2 as shipped in craft-libs
(https://github.com/fastmikey99/craft-libs), revision
049a48689e8488148ccd72b5d061baf8bb0810d9, directory `vendor/hayro-syntax` (itself the
PdfCraft snapshot recorded in `SOURCE.json`), MIT OR Apache-2.0. The licence texts are
retained here. `SOURCE.json` is kept verbatim: its local hash for `src/filter/mod.rs`
describes the craft-libs file, not this one.

Only `src/filter/mod.rs` differs: `MAX_DECODED_STREAM` is 1 GiB instead of 256 MiB (one
line, plus a comment pointing here). The rest of the crate is the craft-libs copy unchanged;
its `Cargo.lock` is left out (a path dependency's lock file is not used).

Why: real drawings can have a page or form content stream that decodes past 256 MiB (an
Inkscape 22 x 180 in sheet stores a 128 MB form that decodes to 391 MB). At 256 MiB the
reader stops decoding and the drawing is cut short. `DecodeLimits::with_max_decoded_stream`
can only lower the limit, so the ceiling itself has to move. VectorCraft keeps its own
guard in `crates/pdf/src/streams.rs`, which screens every stream before the reader sees it:
content and form streams may decode up to 1 GiB only when they expand at most 32 times;
fonts, object streams and everything else stay at 256 MiB, and images keep their
declared-size budget.

The workspace patches this copy in with `[patch.crates-io] hayro-syntax = { path =
"vendor/hayro-syntax" }`; hayro-interpret, hayro-jbig2 and krilla still come from the same
craft-libs revision. Drop this copy and point the patch back at craft-libs when upstream
lets a caller raise the ordinary stream limit (or raises it), then rerun the
`streams` tests and a large-content import.
