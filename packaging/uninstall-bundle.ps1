<#
.SYNOPSIS
    Removes the sudokooler Scheduled Task and installed files.
#>

#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    [string]$InstallDir = "C:\Program Files\sudokooler"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$task = Get-ScheduledTask -TaskName "sudokooler" -ErrorAction SilentlyContinue
if ($task) {
    Write-Host "Unregistering Scheduled Task 'sudokooler'..."
    Stop-ScheduledTask -TaskName "sudokooler" -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName "sudokooler" -Confirm:$false
}

$procs = Get-Process -Name "sudokooler" -ErrorAction SilentlyContinue
if ($procs) {
    Write-Host "Stopping running sudokooler process..."
    $procs | Stop-Process -Force
    Start-Sleep -Milliseconds 300
}

if (Test-Path -LiteralPath $InstallDir) {
    Write-Host "Removing '$InstallDir'..."
    Remove-Item -LiteralPath $InstallDir -Recurse -Force
}

Write-Host ""
Write-Host "sudokooler removed. Note: PawnIO was left installed in case other apps (FanControl, OpenRGB, etc.) use it; uninstall PawnIO from Windows Settings -> Apps if no longer needed."
