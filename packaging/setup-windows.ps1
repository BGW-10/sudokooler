<#
.SYNOPSIS
    Installs PawnIO and signed CPU telemetry modules for sudokooler on Windows.

.DESCRIPTION
    What this script automates:
      1. Checks whether the PawnIO kernel driver is already installed (via the Windows
         Uninstall registry key or C:\Program Files\PawnIO). If missing (and -SkipDriver
         is not set), downloads the latest PawnIO_setup.exe from the official GitHub
         release (namazso/PawnIO.Setup) and runs the installer.
      2. Downloads the latest signed module archive from namazso/PawnIO.Modules on GitHub,
         extracts AMDFamily17.bin, and copies it into C:\Program Files\PawnIO\modules\
         (or -ModulesDir).
      3. Cleans up its temporary download directory on exit.
      4. (Optional, with -RegisterTask) Registers a Windows Task Scheduler logon task
         named "sudokooler" configured to run sudokooler.exe with highest privileges.

.PARAMETER ModulesDir
    Destination directory for signed PawnIO .bin modules.
    Defaults to "C:\Program Files\PawnIO\modules".

.PARAMETER SkipDriver
    Skip downloading and installing the PawnIO kernel driver.

.PARAMETER RegisterTask
    Register an elevated Task Scheduler logon task ("sudokooler") to launch sudokooler.exe
    automatically at Windows sign-in.

.PARAMETER BinaryPath
    Path to sudokooler.exe used when -RegisterTask is specified.
    Defaults to <repo>\target\release\sudokooler.exe.

.EXAMPLE
    .\packaging\setup-windows.ps1

.EXAMPLE
    .\packaging\setup-windows.ps1 -RegisterTask
#>

#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    [string]$ModulesDir = "C:\Program Files\PawnIO\modules",
    [switch]$SkipDriver,
    [switch]$RegisterTask,
    [string]$BinaryPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Test-PawnIoInstalled {
    $uninstallKeys = @(
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO"
    )
    foreach ($key in $uninstallKeys) {
        if (Test-Path -LiteralPath $key) {
            return $true
        }
    }
    return (Test-Path -LiteralPath "C:\Program Files\PawnIO")
}

function Get-GitHubReleaseAsset {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [Parameter(Mandatory)][string]$Pattern
    )
    $headers = @{ "User-Agent" = "sudokooler-setup" }
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
if (-not $BinaryPath) {
    $BinaryPath = Join-Path $repoRoot "target\release\sudokooler.exe"
}

$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("sudokooler-pawnio-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

$prevProgress = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'

try {
    # Step 1: Install PawnIO kernel driver if needed
    if ($SkipDriver) {
        Write-Host "[1/3] Skipping PawnIO driver check (-SkipDriver)."
    } elseif (Test-PawnIoInstalled) {
        Write-Host "[1/3] PawnIO driver already installed."
    } else {
        Write-Host "[1/3] Querying latest namazso/PawnIO.Setup release..."
        $setupAsset = Get-GitHubReleaseAsset -Repo "namazso/PawnIO.Setup" -Pattern "PawnIO_setup*.exe"
        $setupPath = Join-Path $tempDir $setupAsset.Name
        Write-Host "      Downloading $($setupAsset.Name) ($($setupAsset.Tag))..."
        Invoke-WebRequest -Uri $setupAsset.Url -OutFile $setupPath -UseBasicParsing

        Write-Host "      Launching PawnIO installer (complete prompts if shown)..."
        $proc = Start-Process -FilePath $setupPath -Wait -PassThru
        if ($proc.ExitCode -ne 0) {
            throw "PawnIO installer exited with code $($proc.ExitCode)."
        }
        Write-Host "      PawnIO driver installed."
    }

    # Step 2: Download and install signed CPU modules
    Write-Host "[2/3] Querying latest namazso/PawnIO.Modules release..."
    $modulesAsset = Get-GitHubReleaseAsset -Repo "namazso/PawnIO.Modules" -Pattern "release_*.zip"
    $zipPath = Join-Path $tempDir $modulesAsset.Name
    $extractDir = Join-Path $tempDir "modules"

    Write-Host "      Downloading $($modulesAsset.Name) ($($modulesAsset.Tag))..."
    Invoke-WebRequest -Uri $modulesAsset.Url -OutFile $zipPath -UseBasicParsing
    Expand-Archive -LiteralPath $zipPath -DestinationPath $extractDir -Force

    New-Item -ItemType Directory -Path $ModulesDir -Force | Out-Null
    $wantedModules = @("AMDFamily17.bin")
    foreach ($modName in $wantedModules) {
        $found = Get-ChildItem -Path $extractDir -Filter $modName -Recurse -File | Select-Object -First 1
        if (-not $found) {
            throw "Expected module '$modName' was not found in $($modulesAsset.Name)."
        }
        Copy-Item -LiteralPath $found.FullName -Destination (Join-Path $ModulesDir $modName) -Force
        Write-Host "      Installed $modName -> $ModulesDir"
    }

    # Step 3: Optional elevated Task Scheduler logon task
    if ($RegisterTask) {
        $resolvedBin = (Resolve-Path -LiteralPath $BinaryPath -ErrorAction Stop).Path
        Write-Host "[3/3] Registering elevated Scheduled Task 'sudokooler' -> $resolvedBin..."
        $action = New-ScheduledTaskAction -Execute $resolvedBin -WorkingDirectory (Split-Path -Parent $resolvedBin)
        $trigger = New-ScheduledTaskTrigger -AtLogOn
        $principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Highest
        $settings = New-ScheduledTaskSettingsSet `
            -AllowStartIfOnBatteries `
            -DontStopIfGoingOnBatteries `
            -ExecutionTimeLimit (New-TimeSpan -Days 0) `
            -RestartCount 3 `
            -RestartInterval (New-TimeSpan -Minutes 1)

        Register-ScheduledTask -TaskName "sudokooler" -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
        Write-Host "      Scheduled Task 'sudokooler' registered (runs at logon with highest privileges)."
    } else {
        Write-Host "[3/3] Scheduled Task skipped (pass -RegisterTask after 'cargo build --release' to auto-start at logon)."
    }

    Write-Host ""
    Write-Host "Setup complete. Test from an elevated PowerShell prompt:"
    Write-Host "  `"$BinaryPath`" --dry-run --count 3"
}
finally {
    $ProgressPreference = $prevProgress
    if (Test-Path -LiteralPath $tempDir) {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
