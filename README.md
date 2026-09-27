# Negative Band Correction for Photoshop

**[Download Negative Band Correction 0.2 — CCX installer](https://github.com/grahamsz/negative_band_correction/releases/download/0.2/negative-band-correction-0.2.0-all-platforms.ccx)**

**[YouTube Video Demo](https://www.youtube.com/watch?v=mNi-jrepSeo)**

One download includes Windows x64, Intel Mac, and Apple Silicon. Requires Photoshop 25 or newer. Mac builds are currently for testing: they are not Developer ID signed or notarized and may require developer loading and macOS security approval.

A Photoshop UXP panel backed by its own bundled Rust correction engine.
Version 0.2 has one output: **locally fitted waves with density masks and residual
refinement**. Layer pixels contain fitted waves, local amplitude, and image-dependent blend compensation. The editable mask uses estimated debanded density.

One detected component creates one Linear Light pixel layer and one mask.
Independent frequencies or color channels get separate layers inside one Pass
Through group. Layers start at **66% opacity**, with masks normalized so that
this applies the fitted result. Increasing opacity gives about 1.5 times the
fitted correction at 100%, subject to clipping. Keep Fill at 100%.

## Install

1. [Download the CCX installer](https://github.com/grahamsz/negative_band_correction/releases/download/0.2/negative-band-correction-0.2.0-all-platforms.ccx).
2. **Double-click the downloaded .ccx file** and follow the Creative Cloud installation prompts.
3. Open **Plugins > Negative Band Correction** in Photoshop.
4. Open a 16-bit RGB or grayscale negative and select exactly one opaque, full-canvas image layer.
5. Click **Create correction layer**. Save as PSD/PSB to retain editable layers.

The installer includes the native Rust engine. Normal installation does not require
Rust, the SDK, UXP Developer Tool, a helper application, or Developer Mode.
The current unsigned Mac build has the testing limitations noted above.

If you previously loaded a development copy through UXP Developer Tool, unload
that copy before installing the CCX. For development, load
`dist/plugin/manifest.json` with UXP Developer Tool. Restart Photoshop if it
retains a previous native binary after updating.
The operation reads only the selected layer by ID and is one Undo. Correction layers are placed directly above it and clipped to it. Select the original image layer when fitting again. Photoshop's progress dialog can
cancel the operation. The plugin does not automatically remove existing layers.

## Creative Cloud says "Compatible app required"

On a machine with only Photoshop Beta 27.12 installed, the 0.2.0 CCX was rejected
by Creative Cloud and by Adobe's UPIA command-line installer (status `-411`,
no compatible installed product). The manifest requires Photoshop 25 or newer;
Beta exceeds that version, but the installer does not accept this installation.
This failure occurs before the plugin or Rust engine loads.

For Beta development, use UXP Developer Tool: unload any older instance, choose
**Add Plugin**, select `dist/plugin/manifest.json`, and **Load** it into Photoshop
Beta with Developer Mode enabled. This is the unpacked bundle, not the CCX.
For the normal CCX installation route, install and launch a regular Photoshop
release (25 or newer) through Creative Cloud, then retry the package. Installation
and host loading still need verification on that release.

See [Adobe's installer error reference](https://helpx.adobe.com/creative-cloud/apps/troubleshoot/plugin-installation-issues/plugin-installation-errors-using-exman-or-upia.html)
and [developer reports of Beta-only installation failures](https://forums.creativeclouddeveloper.com/t/whats-your-experience-when-distributing-installing-ccx-files-it-looks-to-me-like-the-normal-flow-sometimes-does-not-work/11465).

## Standalone engine and cross-platform builds

The engine lives in `crates/negative-banding` inside this repository. No epscan
checkout is required. epscan retains an independent backport; changes can be
ported explicitly between the projects without coordinating their releases.

GitHub Actions tests and builds Windows x64, macOS Intel, and Apple Silicon.
See [build and CI setup](docs/build-and-ci.md) for the SDK settings,
development artifacts, and optional macOS signing/notarization.
The downloadable CCX includes all three architectures. CI builds and package checks passed on all three; Photoshop host testing on both Mac architectures remains outstanding.

Version 0.2 uses stronger inverse-phase backoff, up to eight refinement steps,
and local held-row guards against newly overcorrected patches. The editable
mask and 66% starting opacity are unchanged.

## Controls

- **Correction strength: 100%** applies the fitted correction.
- **Initial layer opacity: 66%** leaves room to increase correction afterward.
  Changing this starting value changes mask normalization, not the fitted target.
- **Full mask below brightness: 10%**, fading to zero at **60%**, retains darkness
  protection. The residual fit accounts for this gate; it does not replace it.
- **Wave layer amplitude: 50%** makes the wave layer gentler and the density mask brighter. The mask compensates to preserve the fitted correction; higher values darken the mask. Unusually high darkness cutoffs may require a stronger carrier automatically.
- **Detection region** optionally takes `X0,X1,Y0,Y1`, half-open full-resolution
  coordinates. It limits frequency learning, not the corrected area.

Frequencies are learned in pixels; DPI does not impose a 4.3 mm period. Run on
the original negative before inversion, creative curves, resizing, or rotation.
Alt-click the mask to inspect it, or paint it to adjust local applicability.

See [layer and mask equations](docs/compact-pure-waves.md) and
[residual fitting](docs/residual-fitting.md). The output selector, separate-mask
stack, detailed/exact outputs, and old HTTP helper have been removed. Numerical
reference renderers remain only in Rust tests, not the production plugin.

## Precision and verification

Layer pixels and masks use Photoshop's native 16-bit range, 0-32768, with exact
neutral gray. Source capture uses the expanded 0-65535 transfer range. Each run
checks 16 full-width rows of the Photoshop composite against Rust's prediction
before committing. A mismatch rolls back the new layers. This is a sampled
consistency check, not a whole-image guarantee; custom blending gamma can matter.
An unreachable target at the selected opacity also rolls back with instructions
to raise the starting opacity.

The panel shows detected band spacing in pixels and millimetres, calculated as pixels * 25.4 / document PPI. It displays the PPI used; correct the document resolution if scan metadata is wrong. The report-export button and bottom diagnostic box have been removed. A short status near the title still reports progress and errors.

Automated coverage includes mathematical comparisons, native ABI loading,
cancellation, rollback, opacity normalization, and mocked Photoshop workflows.
The selected-layer flow and taller panel still need a run in Photoshop to confirm host behavior. Verification temporarily isolates the selected source and restores original layer visibility, including on failure. Select an opaque, full-canvas image layer; transparent layers, groups and clipped sources are not supported.

## Build and test

Development requires the pinned Rust toolchain, Visual Studio 2022 C++ tools,
Adobe's UXP Hybrid Plugin SDK, Adobe UXP Developer Tool, and Node 18+.

```powershell
cargo test --locked --workspace --release
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
node --test tests/plugin.test.js tests/startup.test.js
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -SdkPath C:\path\to\uxp-hybrid-plugin-sdk-main
```

The build statically links Rust and the C/C++ runtime, runs an addon ABI smoke
test, and uses Adobe's installed packager to create the CCX. Set
`UXP_DEVELOPER_TOOLS` if it is installed outside its default Windows location.
Packaging performs offline validation, not Photoshop-host validation.

The native benchmark reads a TIFF without changing it. It requires numpy and
tifffile and excludes Photoshop reads, writes, and host verification:

```powershell
python scripts/benchmark-native.py target/x86_64-pc-windows-msvc/release/banding.dll path/to/scan.tif
```

Source strips are spooled to a private temporary file; fit/render work is bounded
and parallelized. Normal completion, cancellation, and unload remove the spool.
An abrupt host exit can leave `photoshop-banding-native-*` in the OS temp folder.

The bundled crate owns detection, residual refinement, full-precision TIFF math,
and quantized layer/mask rendering. `src/native.rs` handles Photoshop jobs and
the C ABI; `native/addon.cpp` adapts it to UXP.
See [provenance](UPSTREAM.md) and [CI setup](docs/build-and-ci.md).

