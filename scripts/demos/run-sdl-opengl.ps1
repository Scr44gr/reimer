[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$release = '3.4.12'
$archiveHash = '326e9f5ae2cbab478c03a9e2b22a560e5a9358b5f5eed8e61f5a7c8333750cf1'
$archiveUrl = "https://github.com/libsdl-org/SDL/releases/download/release-$release/SDL3-$release-win32-x64.zip"
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$nativeRoot = Join-Path $workspace "target\native\sdl-$release"
$archive = Join-Path $nativeRoot "SDL3-$release-win32-x64.zip"
$library = Join-Path $nativeRoot 'SDL3.dll'

if (-not (Test-Path -LiteralPath $library)) {
    New-Item -ItemType Directory -Path $nativeRoot -Force | Out-Null
    if (-not (Test-Path -LiteralPath $archive)) {
        Invoke-WebRequest -Uri $archiveUrl -OutFile $archive
    }

    $actualHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $archiveHash) {
        throw "SDL archive checksum mismatch: expected $archiveHash, found $actualHash"
    }
    Expand-Archive -LiteralPath $archive -DestinationPath $nativeRoot -Force
}

if (-not (Test-Path -LiteralPath $library)) {
    throw "SDL3.dll was not found after extracting the verified release archive"
}

$env:PATH = "$nativeRoot;$env:PATH"
Push-Location $workspace
try {
    cargo run -p reimer-cli --locked -- run examples/m5_sdl_opengl.reim
    if ($LASTEXITCODE -ne 0) {
        throw "the SDL3/OpenGL demo failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}
