[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $Version,

    [Parameter(Mandatory)]
    [string] $TargetHost
)

$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$packageRoot = Join-Path $repositoryRoot 'target\release-package'
$assetRoot = Join-Path $repositoryRoot 'target\release-assets'
$packageName = "reimer-v$Version-$TargetHost"
$stagingRoot = Join-Path $packageRoot $packageName
$isWindowsHost = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)
$executableSuffix = if ($isWindowsHost) { '.exe' } else { '' }

function Remove-GeneratedPath {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $AllowedRoot
    )

    $resolvedPath = [System.IO.Path]::GetFullPath($Path)
    $resolvedRoot = [System.IO.Path]::GetFullPath($AllowedRoot).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    ) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $resolvedPath.StartsWith($resolvedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove generated path outside '$resolvedRoot': '$resolvedPath'."
    }
    if (Test-Path -LiteralPath $resolvedPath) {
        Remove-Item -LiteralPath $resolvedPath -Recurse -Force
    }
}

New-Item -ItemType Directory -Force -Path $packageRoot, $assetRoot | Out-Null
Remove-GeneratedPath -Path $stagingRoot -AllowedRoot $packageRoot
New-Item -ItemType Directory -Force -Path $stagingRoot | Out-Null

foreach ($binaryName in 'reimer', 'reimer-lsp', 'reimer-lint') {
    $source = Join-Path $repositoryRoot "target\release\$binaryName$executableSuffix"
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Release binary is missing: '$source'."
    }
    Copy-Item -LiteralPath $source -Destination $stagingRoot
}

Copy-Item -LiteralPath (Join-Path $repositoryRoot 'std') -Destination $stagingRoot -Recurse
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'README.md') -Destination $stagingRoot
foreach ($licenseName in 'LICENSE-MIT', 'LICENSE-APACHE') {
    $licensePath = Join-Path $repositoryRoot $licenseName
    if (Test-Path -LiteralPath $licensePath -PathType Leaf) {
        Copy-Item -LiteralPath $licensePath -Destination $stagingRoot
    }
}

if ($isWindowsHost) {
    $archive = Join-Path $assetRoot "$packageName.zip"
    Remove-GeneratedPath -Path $archive -AllowedRoot $assetRoot
    Compress-Archive -LiteralPath $stagingRoot -DestinationPath $archive -CompressionLevel Optimal
} else {
    $archive = Join-Path $assetRoot "$packageName.tar.gz"
    Remove-GeneratedPath -Path $archive -AllowedRoot $assetRoot
    & tar -C $packageRoot -czf $archive $packageName
    if ($LASTEXITCODE -ne 0) {
        throw 'tar failed while creating the release archive.'
    }
}

$checksum = Get-FileHash -LiteralPath $archive -Algorithm SHA256
$checksumPath = "$archive.sha256"
Set-Content -LiteralPath $checksumPath -Encoding ascii -NoNewline -Value (
    "$($checksum.Hash.ToLowerInvariant())  $([System.IO.Path]::GetFileName($archive))`n"
)

$archiveRelative = $archive.Substring($repositoryRoot.Length).TrimStart(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
)
$checksumRelative = $checksumPath.Substring($repositoryRoot.Length).TrimStart(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
)
if ($env:GITHUB_OUTPUT) {
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "archive=$archiveRelative"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "checksum=$checksumRelative"
}

Write-Output "Created $archiveRelative"
Write-Output "SHA-256 $($checksum.Hash)"
