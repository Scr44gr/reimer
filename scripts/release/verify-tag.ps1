[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $Tag
)

$ErrorActionPreference = 'Stop'

if ($Tag -notmatch '^v(?<version>(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?)$') {
    throw "Release tag '$Tag' must be a SemVer value prefixed with 'v'."
}

$version = $Matches['version']
$metadataJson = & cargo metadata --locked --no-deps --format-version 1
if ($LASTEXITCODE -ne 0) {
    throw 'Cargo metadata failed while validating the release tag.'
}
& git diff --exit-code -- Cargo.lock
if ($LASTEXITCODE -ne 0) {
    throw 'Cargo.lock changed while validating the release tag.'
}

$metadata = $metadataJson | ConvertFrom-Json
$workspaceVersions = @(
    $metadata.packages |
        Where-Object { $_.id -in $metadata.workspace_members } |
        ForEach-Object { $_.version } |
        Sort-Object -Unique
)

if ($workspaceVersions.Count -ne 1 -or $workspaceVersions[0] -ne $version) {
    throw "Tag version '$version' does not match the workspace version(s): $($workspaceVersions -join ', ')."
}

$extensionManifest = Get-Content -Raw -LiteralPath 'editors/vscode/package.json' | ConvertFrom-Json
if ($extensionManifest.version -ne $version) {
    throw "Tag version '$version' does not match the VS Code extension version '$($extensionManifest.version)'."
}

$extensionLockfileText = Get-Content -Raw -LiteralPath 'editors/vscode/package-lock.json'
$lockedVersions = @(
    [regex]::Matches($extensionLockfileText, '"version"\s*:\s*"([^"\r\n]+)"') |
        Select-Object -First 2 |
        ForEach-Object { $_.Groups[1].Value }
)
if ($lockedVersions.Count -ne 2 -or ($lockedVersions | Where-Object { $_ -ne $version })) {
    throw "Tag version '$version' does not match the VS Code lockfile root versions: $($lockedVersions -join ', ')."
}

if ($env:GITHUB_OUTPUT) {
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "version=$version"
}

Write-Output "Release tag $Tag matches version $version."
