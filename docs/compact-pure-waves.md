# Compensated waves with a clean density mask

Version 0.11 preserves the version 0.10 fitted correction target, while moving
phase-dependent clipping compensation out of the editable mask and into layer
pixels. The layer is no longer a pure sinusoid: it includes local amplitude and
image-dependent compensation. Detection and residual fitting are unchanged.

## Mask

First estimate band-free density as original * (1 - sum of refined band signals),
before applying strength or darkness protection. This is an estimate, not a
promise that all residual banding has been removed. The density mask uses this
estimate rather than the banded original.

A conservative mask floor provides enough headroom for both signs of the wave.
It uses estimated density and global peak-amplitude bounds, not the instantaneous
sine phase. This avoids the alternating mask stripes previously required by
Linear Light clipping. The floor may make some regions brighter than their
simple density weight; it does not change the fitted correction.

## Layer and opacity

For each component, calculate the same target change as version 0.10. Given the
new mask M and Photoshop's actual initial opacity P, solve the layer pixel L:

```text
L = quantize(0.5 + (target - base) / (2 * P * M))
output = quantize(base + P * M * (clamp(base + 2*L - 1) - base))
```

Zero-weight pixels are neutral. The renderer checks that rounding and clipping
still reproduce the target within one native 16-bit level per component. If the
target is unreachable it fails explicitly rather than silently undercorrecting.

Initial opacity remains 66% (usually 168/255 internally). Raising it to 100%
provides approximately 1.52 times the fitted change. Keep Fill at 100%. Painting
the mask scales the already fitted correction pixels and preserves their local
amplitude structure. Protected pixels may have neutral layer pixels, so painting
white cannot invent correction there.

Tests cover a synthetic flat-density negative with a known band (mask variation
at most two native levels), target preservation, 66%-to-100% opacity headroom,
RGB, clipped dark regions, parallel rendering and the native ABI. Photoshop's
sampled composite verification remains enabled. Image structure belongs in the
mask, and imperfect band estimation can still leave residual periodic structure.