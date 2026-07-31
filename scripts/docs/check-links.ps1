[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$documentationRoot = (Resolve-Path (Join-Path $repositoryRoot 'docs')).Path
$failures = [System.Collections.Generic.List[string]]::new()
$markdownPattern = [regex]'\[[^\]]*\]\((?<target>[^)]+)\)'
$htmlPattern = [regex]'href="(?<target>[^"]+)"'

function Test-DocumentationTarget {
    param(
        [Parameter(Mandatory)]
        [System.IO.FileInfo] $Source,

        [Parameter(Mandatory)]
        [string] $Target
    )

    $normalized = $Target.Trim().Trim('<', '>')
    $isExternal = $normalized -match '^[a-zA-Z][a-zA-Z0-9+.-]*:'
    if (($normalized.Length -eq 0) -or $normalized.StartsWith('#') -or $isExternal) {
        return
    }

    $pathPart = ($normalized -split '[?#]', 2)[0]
    if ($pathPart.EndsWith('.html', [System.StringComparison]::OrdinalIgnoreCase)) {
        $pathPart = [System.IO.Path]::ChangeExtension($pathPart, '.md')
    }
    if ($pathPart.Length -eq 0) {
        return
    }

    $candidate = [System.IO.Path]::GetFullPath((Join-Path $Source.DirectoryName $pathPart))
    if (-not (Test-Path -LiteralPath $candidate)) {
        $relativeSource = $Source.FullName.Substring($repositoryRoot.Length).TrimStart(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        )
        $failures.Add("$relativeSource -> $Target")
    }
}

$markdownFiles = @(
    Get-Item -LiteralPath (Join-Path $repositoryRoot 'README.md')
    Get-ChildItem -LiteralPath $documentationRoot -Recurse -File -Filter '*.md'
)

$markdownFiles |
    ForEach-Object {
        $source = $_
        $content = Get-Content -Raw -LiteralPath $source.FullName
        foreach ($match in $markdownPattern.Matches($content)) {
            Test-DocumentationTarget -Source $source -Target $match.Groups['target'].Value
        }
        foreach ($match in $htmlPattern.Matches($content)) {
            Test-DocumentationTarget -Source $source -Target $match.Groups['target'].Value
        }
    }

if ($failures.Count -gt 0) {
    $rendered = $failures | Sort-Object -Unique | ForEach-Object { "  - $_" }
    throw "Documentation contains missing local links:`n$($rendered -join "`n")"
}

Write-Output 'Documentation links are valid.'
