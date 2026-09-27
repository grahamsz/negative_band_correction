# Rust engine provenance

This repository owns its engine in `crates/negative-banding`. It is compiled
directly into the Photoshop addon and has no dependency on an epscan checkout,
Git submodule, or separately published engine package.

The engine was copied from `epscan/crates/negative-banding` version 0.1.0 on
2026-09-26, following the version 0.9 extraction. The Rust source and numerical
reference tests were copied unchanged at that point. Version 0.9.1 changes only
the Photoshop copy's carrier normalization: a lower default amplitude makes
combined masks more visible, with a density-range guard and regression tests.
The detector originated in epscan;
residual refinement and the pure-sine layer renderer originated in
photoshop-banding. Both projects are dual-licensed MIT OR Apache-2.0, and the
engine retains those licenses and source notices.

The earlier vendored detector had SHA-256
`E1967E8D9D2C4853817FC770A35DD095A54263B33458EF6B7EE0AD6C51EED366`.
That identifies the historical detector, not the current engine snapshot.

epscan keeps its own backported copy. The two engines are intentionally
independent: review and port numerical fixes explicitly, along with their
regression tests. Neither repository needs to publish first, and Photoshop CI
does not fetch epscan or use `EPSCAN_REF`.

Version 0.10 bakes the fitted local amplitude into the wave layer and keeps density in an editable mask. This Photoshop-only rendering change is not backported to epscan.

Version 0.11 moves phase-dependent blend compensation into layer pixels and derives the editable mask from estimated debanded density, preserving the 0.10 target. This rendering change is confined to Photoshop.

Version 0.12 changes Photoshop residual acceptance: asymmetric backoff/increase steps, lower confidence threshold for agreed reductions, local inversion guards and step halving. epscan is unchanged.
