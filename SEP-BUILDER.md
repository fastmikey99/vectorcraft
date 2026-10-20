# Separation prototype

Status: experimental, 21 October 2026. This command is an isolated trap experiment,
not a production separation builder or PAL player. No parity score is claimed.

`sep.trap {"blackThreshold": 0.7}` replaces the bottom `Auto Trap-1` layer from exactly
one top-level `Artwork` layer, in one undo step. The K threshold is required: the example
is the accepted K-coverage rule; the command remains a prototype.

The command uses the same VectorCraft separation renderer as headless screenshots,
at 600 dpi with antialiasing off. Opaque vector fills, stroke outlines and clips
are united with pathops and rasterised once, preventing internal raster seams
from becoming bands while retaining real blank counters. Transparency and live
effects use renderer occupancy with a one-pixel closing fallback. Band distance
is measured from occupied pixel cells, avoiding a half-pixel width shortfall.
Colour samples always come from the original rendered art. The command extends
the nearest art pixel's process and spot ink samples by 0.2 mm, traces the
pixel edges, then spreads and chokes the clip by 0.01 mm with miter joins (limit 4).
The output is a clip and a process/DeviceN image with a matching opacity mask;
PDF export retains the spot channels and SMask. Trap overprint is off.

Eligibility uses the PAL's `SeparationTypeRule Any / NotUsing` epsilon of 0.005:
all process coverages and the spot tint must be below 0.5% to count as no ink.
Black eligibility uses K coverage at or above the explicit `blackThreshold`,
including a spot's alternate K. It does not use RGB luminance. CMYK values are
preserved; non-CMYK alternates use the colour library's CMYK conversion.
Excluded objects still take part in nearest-pixel ownership.

PAL name matching is case-sensitive. WHP's `NO WHP` and `NO WHP OR POWDER`
exclusions apply to underprint, not trap tags; `NO POWDER` is not a trap exclusion
either. This isolated command does not execute those underprint actions. Metallic
and neon names now fail explicitly as outside the experiment's scope instead of
silently producing a partially trapped job. Neither underprint layer is edited.

Limits: solid vector paints and path/group/layer/compound nodes only; 64 million
pixels at 600 dpi. An unsupported input fails before changing the document. The
exterior is the complement of the art union, including blank enclosed openings;
ink-to-ink edges never receive a band.
No page trap-ticket `/Properties` are written by this prototype.

The expanded local edge study supports the accepted K coverage rule, but does
not identify the trap ticket's parameter semantics uniquely. High-K spot
alternate behaviour remains unvalidated.
The no-ink tolerance removes a major false band and K coverage retains trapped
dark chromatic inks. Residual exposed coloured fragments still produce a band
in a shipped no-trap control. No size filter, file exception or additional
production rule was introduced to hide that mismatch. Per-file results and
customer data remain outside the public repository.

Synthetic tests cover vector union, small counters, clips, seam closing,
pixel-cell distance, nearest excluded ownership, white and
rich black, PAL epsilon boundaries, specialty-input rejection, spot alternates, spot-plane preservation, PDF DeviceN/SMask export,
replacement, undo and cancellation. There are no customer fixtures.
