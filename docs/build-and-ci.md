# Windows and macOS builds

The plugin owns its Rust library in `crates/negative-banding`. Clone this
repository alone; builds and releases do not depend on an epscan checkout.
Source repository: https://github.com/grahamsz/negative_band_correction.

## GitHub Actions setup

`.github/workflows/build.yml` tests Windows x64, macOS Apple Silicon, and macOS
Intel on every push and pull request, without needing the Adobe SDK. It tests
the plugin transport and bundled engine together as one Cargo workspace.

Configure these on the Photoshop plugin repository:

| Setting | Type | Purpose |
| --- | --- | --- |
| `UXP_SDK_URL` | Secret | Private HTTPS download URL for your authorized Adobe UXP Hybrid Plugin SDK ZIP. Keep the SDK out of source control and artifacts. |
| `UXP_SDK_SHA256` | Variable | SHA-256 of that exact ZIP; mismatches stop the build. |
| `UXP_NATIVE_BUILDS` | Variable | Set to `true` to build native artifacts on pushes. Otherwise use **Run workflow** with native builds enabled. |

An expiring download URL must be refreshed when it expires. Obtain the SDK from
the Adobe Developer Console and host it privately under your Adobe license.
The workflow does not upload or redistribute the SDK.

Native jobs build and ABI-smoke-test each architecture separately. The final
job combines Windows x64, Mac Intel, and Mac Apple Silicon into one
`negative-band-correction-<version>-all-platforms.ccx`, using Adobe's packager.
Download the `negative-band-correction-all-platforms-ccx` Actions artifact and
extract the CCX to install it. Packaging checks that all three native binaries
are present and unchanged. Platform development bundles remain available too.

Unsigned Mac builds are for local testing; production Mac distribution requires
the signing option below. CI does not open Photoshop or prove host behavior.
Tag pushes also create a GitHub prerelease and attach the combined CCX.
For example, tag `0.1` corresponds to plugin version `0.1.0`.
Pull requests never receive SDK or signing secrets.

## macOS signing for distribution

Default macOS builds use ad-hoc signatures for development. Adobe requires
Developer ID signing and notarization of the addons for distribution. Enable
**sign_macos** when manually running the workflow, and configure these secrets:

- `APPLE_CERTIFICATE_P12`: base64-encoded Developer ID Application certificate,
  including its private key.
- `APPLE_CERTIFICATE_PASSWORD`: password protecting that P12.
- `APPLE_SIGNING_IDENTITY`: full Developer ID Application signing identity.
- `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`: notarization credentials.

`scripts/sign-mac.sh` imports the certificate into a temporary keychain, signs
the addon with a timestamp and hardened runtime, submits it to Apple's notary
service, requires an Accepted response, and removes the temporary keychain.
Keep the resulting binary unchanged when packaging. Bare Mach-O libraries cannot
carry a stapled ticket; notarization records their signature for online checks.
No certificate or Apple credentials are included in build artifacts.

## Local build

Windows:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-native.ps1 -SdkPath C:\path\to\uxp-hybrid-plugin-sdk-main
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -SkipBuild
```

macOS (run on the matching architecture for the ABI smoke test):

```sh
bash scripts/build-native-mac.sh /path/to/uxp-hybrid-plugin-sdk-main
python3 scripts/bundle.py --platform mac/arm64  # use mac/x64 on Intel
```

Both builds statically link the bundled Rust crate into the addon; users install
no extra Rust runtime or companion server. The macOS deployment target is 12.0.
The Windows build has been exercised locally. macOS linking, signing, and host
loading must be confirmed by the first configured workflow/Photoshop run.

Sources: [Adobe hybrid builds](https://developer.adobe.com/uxp/guides/how-to/hybrid-plugins/build),
[Adobe packaging](https://developer.adobe.com/uxp/guides/how-to/distribution/package/),
[GitHub runner architectures](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

## Packaging without the desktop UXP Developer Tool

Adobe's published `@adobe/uxp-devtools-core@1.2.0` can package the Windows CCX
with Node when the desktop tool is absent. This is a build-time dependency only.
Install it in a separate build-tools directory with npm and `--ignore-scripts`.
Set `UXP_PACKAGING_CORE` to that directory's
`node_modules/@adobe/uxp-devtools-core`, and `UXP_PACKAGING_NODE` to `node.exe`,
then run `scripts/package.ps1 -SkipBuild`. The script uses Adobe's package and
manifest/icon validation implementation; it does not replace Photoshop testing.
This route was used for 0.10.0. Users need neither developer tool nor Node.