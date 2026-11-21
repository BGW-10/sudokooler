<#
.SYNOPSIS
    Builds sudokooler (release) and packages a self-contained Windows bundle + zip.

.DESCRIPTION
    Shared by local developer builds and GitHub Actions (.github/workflows/release.yml):
      1. Runs 'cargo build --release' (unless -SkipBuild is passed).
      2. Fetches the latest PawnIO_setup.exe (namazso/PawnIO.Setup) and signed
         AMDFamily17.bin (namazso/PawnIO.Modules) from GitHub Releases.
      3. Assembles dist\sudokooler-windows-x86_64\ with sudokooler.exe, PawnIO assets,
         offline install.ps1 / uninstall.ps1 scripts, README.md, and THIRD_PARTY_LICENSES.txt.
      4. Compresses the staging folder to dist\sudokooler-windows-x86_64.zip.
#>

[CmdletBinding()]
param(
    [string]$OutDir = "dist",
    [switch]$SkipBuild,
    [string]$BinaryPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-GitHubReleaseAsset {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [Parameter(Mandatory)][string]$Pattern
    )
    $headers = @{ "User-Agent" = "sudokooler-release-bundler" }
    if ($env:GITHUB_TOKEN) {
        $headers["Authorization"] = "Bearer $($env:GITHUB_TOKEN)"
    }
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers $headers
    $asset = $release.assets | Where-Object { $_.name -like $Pattern } | Select-Object -First 1
    if (-not $asset) {
        throw "Could not find asset matching '$Pattern' in latest release of $Repo."
    }
    return [pscustomobject]@{
        Tag  = $release.tag_name
        Name = $asset.name
        Url  = $asset.browser_download_url
    }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not [System.IO.Path]::IsPathRooted($OutDir)) {
    $OutDir = Join-Path $repoRoot $OutDir
}
if (-not $BinaryPath) {
    $BinaryPath = Join-Path $repoRoot "target\release\sudokooler.exe"
}

if (-not $SkipBuild) {
    Write-Host "[1/4] Building sudokooler release binary..."
    Push-Location $repoRoot
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
} else {
    Write-Host "[1/4] Skipping cargo build (-SkipBuild)."
}

if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "Release binary not found at '$BinaryPath'."
}

$bundleName = "sudokooler-windows-x86_64"
$stageDir = Join-Path $OutDir $bundleName
$zipPath = Join-Path $OutDir "$bundleName.zip"
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("sudokooler-bundle-" + [guid]::NewGuid().ToString("N"))

New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

$prevProgress = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'

try {
    if (Test-Path -LiteralPath $stageDir) {
        Remove-Item -LiteralPath $stageDir -Recurse -Force
    }
    if (Test-Path -LiteralPath $zipPath) {
        Remove-Item -LiteralPath $zipPath -Force
    }

    $pawnioStage = Join-Path $stageDir "pawnio"
    $installerStage = Join-Path $stageDir "installer"
    New-Item -ItemType Directory -Path $stageDir -Force | Out-Null
    New-Item -ItemType Directory -Path $pawnioStage -Force | Out-Null
    New-Item -ItemType Directory -Path $installerStage -Force | Out-Null

    Write-Host "[2/4] Downloading latest PawnIO installer and AMDFamily17.bin..."
    $setupAsset = Get-GitHubReleaseAsset -Repo "namazso/PawnIO.Setup" -Pattern "PawnIO_setup*.exe"
    $setupDst = Join-Path $installerStage "PawnIO_setup.exe"
    Invoke-WebRequest -Uri $setupAsset.Url -OutFile $setupDst -UseBasicParsing
    Write-Host "      Fetched $($setupAsset.Name) ($($setupAsset.Tag))"

    $modulesAsset = Get-GitHubReleaseAsset -Repo "namazso/PawnIO.Modules" -Pattern "release_*.zip"
    $modulesZip = Join-Path $tempDir $modulesAsset.Name
    $modulesExtract = Join-Path $tempDir "modules"
    Invoke-WebRequest -Uri $modulesAsset.Url -OutFile $modulesZip -UseBasicParsing
    Expand-Archive -LiteralPath $modulesZip -DestinationPath $modulesExtract -Force

    $amdMod = Get-ChildItem -Path $modulesExtract -Filter "AMDFamily17.bin" -Recurse -File | Select-Object -First 1
    if (-not $amdMod) {
        throw "AMDFamily17.bin not found inside $($modulesAsset.Name)."
    }
    Copy-Item -LiteralPath $amdMod.FullName -Destination (Join-Path $pawnioStage "AMDFamily17.bin") -Force
    Write-Host "      Extracted AMDFamily17.bin ($($modulesAsset.Tag))"

    Write-Host "[3/4] Staging bundle files into '$stageDir'..."
    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $stageDir "sudokooler.exe") -Force
    Copy-Item -LiteralPath (Join-Path $repoRoot "packaging\install-bundle.ps1") -Destination (Join-Path $stageDir "install.ps1") -Force
    Copy-Item -LiteralPath (Join-Path $repoRoot "packaging\uninstall-bundle.ps1") -Destination (Join-Path $stageDir "uninstall.ps1") -Force
    Copy-Item -LiteralPath (Join-Path $repoRoot "README.md") -Destination (Join-Path $stageDir "README.md") -Force

    $licenses = @"
sudokooler Windows Release Bundle - Third-Party Licenses & Sources
====================================================================

1. sudokooler (sudokooler.exe)
   License: MIT
   Source:  https://github.com/BGW-10/sudokooler

2. PawnIO Driver Installer (installer\PawnIO_setup.exe, version $($setupAsset.Tag))
   License: GPL-2.0
   Source:  https://github.com/namazso/PawnIO

3. PawnIO AMDFamily17 Module (pawnio\AMDFamily17.bin, version $($modulesAsset.Tag))
   License: LGPL-2.1-or-later
   Source:  https://github.com/namazso/PawnIO.Modules
"@
    Set-Content -LiteralPath (Join-Path $stageDir "THIRD_PARTY_LICENSES.txt") -Value $licenses -Encoding UTF8

    Write-Host "[4/4] Creating archive '$zipPath'..."
    Compress-Archive -Path $stageDir -DestinationPath $zipPath -Force
    Write-Host "Bundle ready: $zipPath"
}
finally {
    $ProgressPreference = $prevProgress
    if (Test-Path -LiteralPath $tempDir) {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
