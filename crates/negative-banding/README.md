# negative-banding

Rust engine owned by the Photoshop Negative Band Correction repository.
It has no scanner, TIFF, Photoshop, UXP, or network runtime
dependencies. Detection, signed residual refinement, darkness weighting, and
the pure-sine layer/mask renderer live here. epscan maintains an independent
backport; neither repository depends on the other's checkout.

`analyze_samples` accepts normalized, disjoint training and held rows. `analyze`
reads those samples from a packed 16-bit file. Both retain the detected fixed
frequencies/phases and conservatively refine local amplitudes. `sample_rows`
supplies the shared sampling schedule; cancellation is an `AtomicBool`.

`Analysis::correction_region_row` supplies full-precision, refined signal in
source coordinates for crop-safe TIFF export. `linear_corrected_sample` applies
strength and darkness, clamps, and rounds once at the requested bit depth.
`Analysis::render_compact` and its parallel version encode editable pure sine
layers/combined masks and predict Photoshop's quantized composition. Their
0..32768 representation is intentionally confined to the layer output.

The TIFF and Photoshop outputs share the fit but need not be bit-identical:
Photoshop quantizes carriers, masks and intermediate blends, and clips components
sequentially. Direct TIFF export retains full 8/16-bit precision and clips the
summed correction once. Weak residual regions retain their initial amplitudes;
the 10%-to-60% darkness guard remains part of the response model.

The core and plugin use 100% fitted strength by default.
Photoshop's 66% opacity is mask
normalization/headroom, not an extra 0.66 factor for TIFF correction.

Run `cargo test -p negative-banding` from the Photoshop workspace. The crate includes
the original detector's Python golden values, residual under/over-application
tests, layer compositing checks, and TIFF-vs-layer/crop precision checks.

The plugin uses a path dependency within this repository. This private workspace
crate is not published separately. See [provenance](../../UPSTREAM.md) before
porting changes between projects.

License: MIT OR Apache-2.0. The original detector was developed in epscan;
residual refinement and layer rendering were developed in photoshop-banding.
