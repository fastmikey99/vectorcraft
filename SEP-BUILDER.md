# Separation prototype

Status: experimental, 21 October 2026. This command is an isolated trap experiment,
not a production separation builder or PAL player. No parity score is claimed.

`sep.trap {"blackCutoff": 0.15}` replaces the bottom `Auto Trap-1` layer from exactly
one top-level `Artwork` layer, in one undo step. The cutoff is required: the example
is an experimental value, not an approved production setting.

The command uses the same VectorCraft separation renderer as headless screenshots,
at 600 dpi with antialiasing off. It finds the outside of the complete artwork,
extends the nearest art pixel's process and spot ink samples by 0.2 mm, traces the
pixel edges, then spreads and chokes the clip by 0.01 mm with miter joins (limit 4).
The output is a clip and a process/DeviceN image with a matching opacity mask;
PDF export retains the spot channels and SMask. Trap overprint is off.

Excluded paints are zero-ink white, names beginning `NEON-`, `M-`, or `NO WHP`, and
colours at or below `blackCutoff`. The experimental luma is
`0.2126 R + 0.7152 G + 0.0722 B`, using uncalibrated RGB (`(1-C)(1-K)` etc. for CMYK).
Object-name exclusions inherit through groups. Excluded objects still take part
in nearest-pixel ownership, so eligible colours cannot spread across their edges.

Limits: solid vector paints and path/group/layer/compound nodes only; 64 million
pixels at 600 dpi. An unsupported input fails before changing the document. The
exterior is the complement of the art union, including blank enclosed openings;
ink-to-ink edges never receive a band.
No page trap-ticket `/Properties` are written by this prototype.

The local comparison stopped at the experiment's geometry limit: four of eight
trapped samples were within two pixels of the shipped clip at the example cutoff.
Colour exclusions and remaining edge differences need production clarification. The white-only control produced no trap;
the rich-black control still produced a small band. No cleanup threshold, extra
exclusion or production rule was added to hide those differences. Customer files
and per-file results remain outside the public repository.

Synthetic tests cover exterior geometry, nearest excluded ownership, white and
rich black, name exclusions, spot-plane preservation, PDF DeviceN/SMask export,
replacement, undo and cancellation. There are no customer fixtures.
