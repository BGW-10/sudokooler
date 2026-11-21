<#
.SYNOPSIS
    Installs the self-contained sudokooler Windows release bundle offline.

.DESCRIPTION
    Run from an extracted sudokooler-windows-x86_64 release folder as Administrator:
      1. Installs the signed PawnIO kernel driver from bundled installer\PawnIO_setup.exe
         (unless PawnIO is already installed or -SkipDriver is specified).
      2. Installs sudokooler.exe, pawnio\AMDFamily17.bin, README.md, licenses, and
         uninstall.ps1 into C:\Program Files\sudokooler (or -InstallDir).
      3. Registers a silent background Windows Task Scheduler startup task ("sudokooler")
         running as SYSTEM (no terminal window) and starts it immediately (unless
         -NoTask / -NoStart are specified).
#>

#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    [string]$InstallDir = "C:\Program Files\sudokooler",
    [switch]$SkipDriver,
    [switch]$NoTask,
    [switch]$NoStart
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

$bundleRoot = $PSScriptRoot
$exeSrc = Join-Path $bundleRoot "sudokooler.exe"
$modSrc = Join-Path $bundleRoot "pawnio\AMDFamily17.bin"
$driverSrc = Join-Path $bundleRoot "installer\PawnIO_setup.exe"

if (-not (Test-Path -LiteralPath $exeSrc)) {
    throw "Missing '$exeSrc'. Run this script from the extracted release bundle folder."
}
if (-not (Test-Path -LiteralPath $modSrc)) {
    throw "Missing '$modSrc'. Run this script from the extracted release bundle folder."
}

# Step 1: Install bundled PawnIO driver if needed
if ($SkipDriver) {
    Write-Host "[1/3] Skipping PawnIO driver install (-SkipDriver)."
} elseif (Test-PawnIoInstalled) {
    Write-Host "[1/3] PawnIO driver already installed."
} else {
    if (-not (Test-Path -LiteralPath $driverSrc)) {
        throw "Bundled PawnIO installer not found at '$driverSrc'."
    }
    Write-Host "[1/3] Launching bundled PawnIO installer (complete prompts if shown)..."
    $proc = Start-Process -FilePath $driverSrc -Wait -PassThru
    if ($proc.ExitCode -ne 0) {
        throw "PawnIO installer exited with code $($proc.ExitCode)."
    }
    Write-Host "      PawnIO driver installed."
}

# Step 2: Copy sudokooler files to $InstallDir
Write-Host "[2/3] Installing sudokooler into '$InstallDir'..."
Stop-ScheduledTask -TaskName "sudokooler" -ErrorAction SilentlyContinue
Get-Process -Name "sudokooler" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300

$pawnioDstDir = Join-Path $InstallDir "pawnio"
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $pawnioDstDir -Force | Out-Null

$copyMap = @(
    @{ Src = $exeSrc; Dst = (Join-Path $InstallDir "sudokooler.exe") },
    @{ Src = $modSrc; Dst = (Join-Path $pawnioDstDir "AMDFamily17.bin") },
    @{ Src = (Join-Path $bundleRoot "uninstall.ps1"); Dst = (Join-Path $InstallDir "uninstall.ps1") },
    @{ Src = (Join-Path $bundleRoot "README.md"); Dst = (Join-Path $InstallDir "README.md") },
    @{ Src = (Join-Path $bundleRoot "THIRD_PARTY_LICENSES.txt"); Dst = (Join-Path $InstallDir "THIRD_PARTY_LICENSES.txt") }
)

foreach ($item in $copyMap) {
    if ((Test-Path -LiteralPath $item.Src) -and ($item.Src -ne $item.Dst)) {
        Copy-Item -LiteralPath $item.Src -Destination $item.Dst -Force
    }
}

$installedExe = Join-Path $InstallDir "sudokooler.exe"

# Step 3: Register and start silent background Task Scheduler startup task
if ($NoTask) {
    Write-Host "[3/3] Skipping Scheduled Task registration (-NoTask)."
} else {
    Write-Host "[3/3] Registering silent background Scheduled Task 'sudokooler' (SYSTEM at startup)..."
    $action = New-ScheduledTaskAction -Execute $installedExe -WorkingDirectory $InstallDir
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit (New-TimeSpan -Days 0) `
        -RestartCount 3 `
        -RestartInterval (New-TimeSpan -Minutes 1)

    Register-ScheduledTask -TaskName "sudokooler" -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null

    if (-not $NoStart) {
        Start-ScheduledTask -TaskName "sudokooler"
        Write-Host "      Scheduled Task 'sudokooler' registered and started silently in the background."
    } else {
        Write-Host "      Scheduled Task 'sudokooler' registered (-NoStart specified)."
    }
}

Write-Host ""
Write-Host "Installation complete!"
Write-Host "  Verify output: `"$installedExe`" --dry-run --count 3"
Write-Host "  Uninstall:     `"$InstallDir\uninstall.ps1`""
