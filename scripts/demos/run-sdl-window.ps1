[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$nativeRoot = Join-Path $workspace 'vendor\sdl3\native\windows-x86_64'
$library = Join-Path $nativeRoot 'SDL3.dll'

. (Join-Path $workspace 'scripts\assert-vendored-checksums.ps1')
Assert-VendoredChecksums -PackageRoot (Join-Path $workspace 'vendor\sdl3') `
    -RelativePath @('native/windows-x86_64/SDL3.dll')

$previousPath = $env:PATH
$env:PATH = "$nativeRoot;$previousPath"
Push-Location $workspace
try {
    cargo run -p reimer-cli --locked -- run examples/m5_sdl_window.reim
    if ($LASTEXITCODE -ne 0) {
        throw "the SDL window demo failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
    $env:PATH = $previousPath
}
