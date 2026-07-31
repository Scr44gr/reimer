[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Project,

    [ValidateSet('check', 'run', 'build', 'test')]
    [string]$Command = 'run',

    [switch]$Release,

    [switch]$Locked
)

$ErrorActionPreference = 'Stop'

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') {
    throw 'The bundled SDL3 artifact currently targets Windows x64.'
}
if (-not [Environment]::Is64BitProcess) {
    throw 'The bundled SDL3 artifact requires a 64-bit process.'
}

$packageRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$nativeRoot = Join-Path $packageRoot 'native\windows-x86_64'
$library = Join-Path $nativeRoot 'SDL3.lib'
$runtime = Join-Path $nativeRoot 'SDL3.dll'
$projectRoot = (Resolve-Path -LiteralPath $Project).Path

if (-not (Test-Path -LiteralPath $library) -or -not (Test-Path -LiteralPath $runtime)) {
    throw "The vendored SDL3 library is incomplete at '$nativeRoot'."
}

$compiler = Get-Command reimer -ErrorAction Stop
$arguments = @($Command, $projectRoot)
if ($Release) {
    $arguments += '--release'
}
if ($Locked) {
    $arguments += '--locked'
}

$previousPath = $env:PATH
$previousLib = $env:LIB
try {
    $env:PATH = "$nativeRoot;$previousPath"
    $env:LIB = if ([string]::IsNullOrEmpty($previousLib)) {
        $nativeRoot
    } else {
        "$nativeRoot;$previousLib"
    }

    Push-Location -LiteralPath $projectRoot
    try {
        & $compiler.Source @arguments
        if ($LASTEXITCODE -ne 0) {
            throw "The compiler command failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }

    if ($Command -eq 'build') {
        $profile = if ($Release) { 'release' } else { 'debug' }
        $outputDirectory = Join-Path $projectRoot "target\reimer\$profile"
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
        Copy-Item -LiteralPath $runtime -Destination (Join-Path $outputDirectory 'SDL3.dll') -Force
    }
}
finally {
    $env:PATH = $previousPath
    $env:LIB = $previousLib
}
