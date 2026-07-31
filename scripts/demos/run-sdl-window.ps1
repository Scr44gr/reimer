[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$nativeRoot = Join-Path $workspace 'vendor\sdl3\native\windows-x86_64'
$library = Join-Path $nativeRoot 'SDL3.dll'

if (-not (Test-Path -LiteralPath $library)) {
    throw "The vendored SDL3.dll was not found at '$library'."
}

$env:PATH = "$nativeRoot;$env:PATH"
Push-Location $workspace
try {
    cargo run -p reimer-cli --locked -- run examples/m5_sdl_window.reim
    if ($LASTEXITCODE -ne 0) {
        throw "the SDL window demo failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}
