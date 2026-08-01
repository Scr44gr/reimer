param(
    [Parameter(Mandatory = $true)]
    [string]$SdlSource
)

$ErrorActionPreference = "Stop"
$packageRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $packageRoot "..\..")).Path
$sourceRoot = (Resolve-Path $SdlSource).Path
$functions = Join-Path $packageRoot "src\raw\functions.reim"
$types = Join-Path $packageRoot "src\raw\types.reim"
$constants = Join-Path $packageRoot "src\raw\constants.reim"
$coverage = Join-Path $packageRoot "coverage.toml"
$workDirectory = Join-Path $workspaceRoot "target\sdl-api-gen"
$translationUnit = Join-Path $workDirectory "sdl_all.c"
$preprocessed = Join-Path $workDirectory "sdl_all.preprocessed.c"
$macroDefinitions = Join-Path $workDirectory "sdl_all.macros.c"
$macroProbe = Join-Path $workDirectory "sdl_macros.c"
$macroExpansions = Join-Path $workDirectory "sdl_macros.expanded.c"
$includeDirectory = Join-Path $sourceRoot "include"

New-Item -ItemType Directory -Force $workDirectory | Out-Null
[System.IO.File]::WriteAllText(
    $translationUnit,
    "#include <SDL3/SDL.h>`r`n",
    [System.Text.UTF8Encoding]::new($false)
)

$compiler = Get-Command cl.exe -ErrorAction SilentlyContinue
$vcvars = $null
if ($null -eq $compiler) {
    $installerDirectory = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer"
    $vswhere = Join-Path $installerDirectory "vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw "MSVC was not found. Install the Visual Studio C++ build tools or run from a Developer PowerShell."
    }
    $installation = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installation)) {
        throw "Visual Studio C++ build tools were not found."
    }
    $vcvars = Join-Path $installation.Trim() "VC\Auxiliary\Build\vcvars64.bat"
    if (-not (Test-Path -LiteralPath $vcvars)) {
        throw "The MSVC x64 environment script was not found at $vcvars"
    }
}

function Invoke-SdlPreprocessor {
    param(
        [Parameter(Mandatory = $true)]
        [string]$InputFile,
        [Parameter(Mandatory = $true)]
        [string]$OutputFile,
        [switch]$PreserveMacros
    )

    $macroArgument = if ($PreserveMacros) { "/d1PP" } else { $null }
    if ($null -ne $compiler) {
        $arguments = @("/nologo", "/E")
        if ($null -ne $macroArgument) {
            $arguments += $macroArgument
        }
        $arguments += "/I$includeDirectory"
        $arguments += $InputFile
        & $compiler.Source @arguments > $OutputFile
    }
    else {
        $macroOption = if ($PreserveMacros) { " /d1PP" } else { "" }
        $command = '/d /c call "' + $vcvars + '" >nul && cl /nologo /E' + $macroOption + ' /I"' + $includeDirectory + '" "' + $InputFile + '" > "' + $OutputFile + '"'
        & cmd.exe $command
    }
    if ($LASTEXITCODE -ne 0) {
        throw "Preprocessing $InputFile failed with exit code $LASTEXITCODE"
    }
}

Invoke-SdlPreprocessor -InputFile $translationUnit -OutputFile $preprocessed
Invoke-SdlPreprocessor -InputFile $translationUnit -OutputFile $macroDefinitions -PreserveMacros

Push-Location $workspaceRoot
try {
    cargo run -q -p sdl-api-gen --bin sdl-api-gen -- macro-probe $macroDefinitions $macroProbe
    if ($LASTEXITCODE -ne 0) {
        throw "Generating the SDL macro probe failed with exit code $LASTEXITCODE"
    }
    Invoke-SdlPreprocessor -InputFile $macroProbe -OutputFile $macroExpansions
    cargo run -q -p sdl-api-gen --bin sdl-api-gen -- $sourceRoot $preprocessed $macroExpansions $functions $types $constants $coverage
    if ($LASTEXITCODE -ne 0) {
        throw "SDL binding generation failed with exit code $LASTEXITCODE"
    }
    cargo run -q -p reimer-cli -- fmt $packageRoot
    if ($LASTEXITCODE -ne 0) {
        throw "Formatting generated bindings failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

Write-Host "Generated SDL raw functions, types, enum and macro constants, and coverage metadata."
