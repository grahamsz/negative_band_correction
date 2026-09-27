param([string]$SdkPath, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$bandingRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (!$SkipBuild) {
    if (!$SdkPath) {throw 'Provide -SdkPath for the Adobe UXP Hybrid SDK, or -SkipBuild after building.'}
    & (Join-Path $PSScriptRoot 'build-native.ps1') -SdkPath $SdkPath
}
$bandingDist=Join-Path $bandingRoot 'dist'
$bundle=Join-Path $bandingDist 'plugin'
New-Item -ItemType Directory -Path $bundle -Force | Out-Null
foreach ($name in @('manifest.json','index.html','style.css','main.js','workflow.js','native-client.js','compact-stack.js')) {
    Copy-Item -LiteralPath (Join-Path $bandingRoot "plugin\$name") -Destination $bundle -Force
}
foreach ($name in @('win','icons')) {
    $sourceRoot=Join-Path $bandingRoot "plugin\$name"
    foreach ($file in Get-ChildItem -LiteralPath $sourceRoot -Recurse -File) {
        if ($name -eq 'win') {
            $currentAddon=(Get-Content -Raw -LiteralPath (Join-Path $bandingRoot 'plugin\manifest.json') | ConvertFrom-Json).addon.name
            if ($file.Name -ne $currentAddon) {continue}
        }
        $relative=$file.FullName.Substring($sourceRoot.Length).TrimStart('\')
        $destination=Join-Path (Join-Path $bundle $name) $relative
        # Photoshop may hold the add-on open. Identical binaries need no overwrite.
        if ((Test-Path -LiteralPath $destination) -and
            (Get-FileHash -LiteralPath $file.FullName).Hash -eq (Get-FileHash -LiteralPath $destination).Hash) {continue}
        New-Item -ItemType Directory -Path (Split-Path $destination) -Force | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $destination -Force
    }
}
foreach($obsolete in @('client.js','pure-stack.js')) {
    $obsoletePath=Join-Path $bundle $obsolete
    if(Test-Path -LiteralPath $obsoletePath) {Remove-Item -LiteralPath $obsoletePath}
}
foreach ($name in @('LICENSE','THIRD_PARTY_NOTICES.md','UPSTREAM.md')) {
    Copy-Item -LiteralPath (Join-Path $bandingRoot $name) -Destination $bundle -Force
}
Copy-Item -LiteralPath (Join-Path $bandingRoot 'README.md') -Destination $bandingDist -Force
Copy-Item -LiteralPath (Join-Path $bandingRoot 'docs') -Destination $bandingDist -Recurse -Force
$udt=if($env:UXP_DEVELOPER_TOOLS){$env:UXP_DEVELOPER_TOOLS}else{'C:\Program Files\Adobe\Adobe UXP Developer Tools'}
$oldRunAsNode=$env:ELECTRON_RUN_AS_NODE
try {
    $env:ELECTRON_RUN_AS_NODE='1'
    $logs=Join-Path $bandingRoot 'target'
    New-Item -ItemType Directory -Path $logs -Force | Out-Null
    $stdout=Join-Path $logs 'package.stdout.log'
    $stderr=Join-Path $logs 'package.stderr.log'
    $script='"'+(Join-Path $PSScriptRoot 'package-ccx.js')+'"'
    $packagerExe=if($env:UXP_PACKAGING_CORE){$env:UXP_PACKAGING_NODE}else{Join-Path $udt 'Adobe UXP Developer Tools.exe'}
    if(!$packagerExe -or !(Test-Path -LiteralPath $packagerExe)) {throw 'Install UXP Developer Tool, or set UXP_PACKAGING_CORE and UXP_PACKAGING_NODE for Adobe''s npm packager.'}
    $process=Start-Process -FilePath $packagerExe -ArgumentList $script -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    Get-Content -LiteralPath $stdout,$stderr
    if ($process.ExitCode -ne 0) {throw 'Adobe CCX packaging failed'}
    $manifest=Get-Content -Raw -LiteralPath (Join-Path $bundle 'manifest.json') | ConvertFrom-Json
    $versioned=Join-Path $bandingDist ("negative-band-correction-"+$manifest.version+"-win-x64.ccx")
    Copy-Item -LiteralPath (Join-Path $bandingDist ($manifest.id+'_PS.ccx')) -Destination $versioned -Force
    Write-Output "Installer: $versioned"
} finally {$env:ELECTRON_RUN_AS_NODE=$oldRunAsNode}
