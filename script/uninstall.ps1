#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$InstallRoot,
    [string]$DbgAtlasExe
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-Windows {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "DbgAtlas service uninstall scripts are only supported on Windows."
    }
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $FilePath $($Arguments -join ' ')"
    }
}

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Quote-ProcessArgument {
    param([Parameter(Mandatory = $true)][string]$Argument)

    if ($Argument -notmatch '[\s"]') {
        return $Argument
    }

    return '"' + $Argument.Replace('"', '\"') + '"'
}

function Resolve-DbgatlasExe {
    param(
        [Parameter(Mandatory = $true)][string]$RepoRoot,
        [string]$DbgAtlasExe
    )

    if (-not [string]::IsNullOrWhiteSpace($DbgAtlasExe)) {
        return (Resolve-Path -LiteralPath $DbgAtlasExe).Path
    }

    $releaseExe = Join-Path $RepoRoot "target\release\dbgatlas.exe"
    if (Test-Path -LiteralPath $releaseExe -PathType Leaf) {
        return (Resolve-Path -LiteralPath $releaseExe).Path
    }

    throw "Could not find target\release\dbgatlas.exe. Run .\script\build-release-install.ps1 -BuildOnly first, or pass -DbgAtlasExe."
}

Assert-Windows

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path
$dbgatlasExe = Resolve-DbgatlasExe -RepoRoot $repoRoot -DbgAtlasExe $DbgAtlasExe

Write-Host "DbgAtlas uninstall context:"
Write-Host "  repo root: $repoRoot"
Write-Host "  exe: $dbgatlasExe"
if ([string]::IsNullOrWhiteSpace($InstallRoot)) {
    Write-Host "  install root: service default"
}
else {
    Write-Host "  install root: $InstallRoot"
}

if (-not (Test-Administrator)) {
    $powershell = (Get-Command powershell.exe -ErrorAction Stop).Source
    $args = @(
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        (Quote-ProcessArgument $PSCommandPath),
        "-DbgAtlasExe",
        (Quote-ProcessArgument $dbgatlasExe)
    )
    if (-not [string]::IsNullOrWhiteSpace($InstallRoot)) {
        $args += @("-InstallRoot", (Quote-ProcessArgument $InstallRoot))
    }

    Write-Host "Starting elevated uninstall step..."
    $process = Start-Process -FilePath $powershell -ArgumentList $args -Verb RunAs -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "Elevated uninstall failed with exit code $($process.ExitCode)."
    }

    Write-Host "DbgAtlas service and install root were removed."
    exit 0
}

$uninstallArgs = @("service", "uninstall", "--purge")
if (-not [string]::IsNullOrWhiteSpace($InstallRoot)) {
    $uninstallArgs += @("--install-root", $InstallRoot)
}

Invoke-Checked -FilePath $dbgatlasExe -Arguments $uninstallArgs
Write-Host "DbgAtlas service and install root were removed."
