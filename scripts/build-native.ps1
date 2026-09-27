param([Parameter(Mandatory=$true)][string]$SdkPath)
$ErrorActionPreference = 'Stop'
$bandingRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$bandingSdk = (Resolve-Path -LiteralPath $SdkPath).Path
if (!(Test-Path -LiteralPath (Join-Path $bandingSdk 'src\api\UxpAddonShared.h'))) {
    throw 'SdkPath must contain src\api\UxpAddonShared.h from the Adobe UXP Hybrid SDK.'
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (!$vs) { throw 'Install Visual Studio C++ build tools.' }
Import-Module (Join-Path $vs 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64' | Out-Null
Push-Location $bandingRoot
$oldFlags = $env:RUSTFLAGS
try {
    # Ship the C/C++ runtime inside the addon as well as the Rust engine.
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --locked --release --lib --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Rust library build failed' }
    $out = Join-Path $bandingRoot 'target\native'
    New-Item -ItemType Directory -Path $out -Force | Out-Null
    $addon = Join-Path $out 'banding-v010.uxpaddon'
    $rustLib = Join-Path $bandingRoot 'target\x86_64-pc-windows-msvc\release\banding.lib'
    & cl.exe /nologo /LD /MT /O2 /EHsc /std:c++17 /W4 /DWIN32_LEAN_AND_MEAN /DNOMINMAX "/I$bandingSdk\src\utilities" "/I$bandingSdk\src\api" "/Fo$out\addon.obj" 'native\addon.cpp' $rustLib /link "/OUT:$addon" /INCREMENTAL:NO /OPT:REF /OPT:ICF /DYNAMICBASE /NXCOMPAT ws2_32.lib userenv.lib bcrypt.lib ntdll.lib advapi32.lib kernel32.lib
    if ($LASTEXITCODE -ne 0) { throw 'Native addon link failed' }
    $smoke=Join-Path $out 'native-smoke.exe'
    & cl.exe /nologo /MT /O2 /EHsc /std:c++17 /W4 /DWIN32_LEAN_AND_MEAN /DNOMINMAX "/I$bandingSdk\src\api" "/Fo$out\smoke.obj" 'native\smoke.cpp' /link "/OUT:$smoke"
    if ($LASTEXITCODE -ne 0) { throw 'Native ABI smoke-test build failed' }
    & $smoke $addon
    if ($LASTEXITCODE -ne 0) { throw 'Native ABI smoke-test failed' }
    $dest=Join-Path $bandingRoot 'plugin\win\x64'
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    Copy-Item -LiteralPath $addon -Destination $dest -Force
    Write-Output "Built $addon (Rust statically linked)."
} finally {
    $env:RUSTFLAGS=$oldFlags
    Pop-Location
}
