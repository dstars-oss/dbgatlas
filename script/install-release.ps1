#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RepoRoot,
    [string]$Bind = "127.0.0.1:7331",
    [switch]$NoStart,
    [switch]$NoForce
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-Windows {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "DbgAtlas service install scripts are only supported on Windows."
    }
}

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Invoke-Dbgatlas {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [switch]$AllowFailure
    )

    & $Exe @Arguments
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0 -and -not $AllowFailure) {
        throw "dbgatlas failed with exit code ${exitCode}: $Exe $($Arguments -join ' ')"
    }
}

function Get-DbgatlasServiceStatus {
    param([Parameter(Mandatory = $true)][string]$Exe)

    $output = & $Exe @("--json", "service", "status") 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Could not query existing service status; install will report the concrete error if this matters."
        return $null
    }

    try {
        $status = (($output | Out-String) | ConvertFrom-Json).status
        return $status
    }
    catch {
        Write-Host "Could not parse existing service status; install will continue."
        return $null
    }
}

function Wait-DbgatlasHealth {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [int]$Attempts = 20,
        [int]$DelayMilliseconds = 500
    )

    $lastOutput = $null
    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        $output = & $Exe @("--json", "service", "health") 2>&1
        if ($LASTEXITCODE -eq 0) {
            $output
            return
        }

        $lastOutput = $output
        if ($attempt -lt $Attempts) {
            Start-Sleep -Milliseconds $DelayMilliseconds
        }
    }

    if ($lastOutput) {
        $lastOutput | ForEach-Object { Write-Host $_ }
    }
    throw "DbgAtlas service did not become healthy after $Attempts attempts."
}

function Assert-ReleasePayload {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir)

    $required = @(
        "dbgatlas.exe",
        "dbgatlas-worker.exe",
        "dbgatlas_dbgeng.dll"
    )

    foreach ($name in $required) {
        $path = Join-Path $ReleaseDir $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Release payload is incomplete: missing $path"
        }
    }
}

Assert-Windows

if (-not (Test-Administrator)) {
    throw "install-release.ps1 must run as Administrator. Use build-release-install.ps1 to build without elevation and elevate only this install step."
}

$repoRootPath = (Resolve-Path -LiteralPath $RepoRoot).Path
$releaseDir = Join-Path $repoRootPath "target\release"
$dbgatlasExe = Join-Path $releaseDir "dbgatlas.exe"

Assert-ReleasePayload -ReleaseDir $releaseDir

$existingStatus = Get-DbgatlasServiceStatus -Exe $dbgatlasExe
if ($NoForce -and $existingStatus -and $existingStatus -ne "not_installed") {
    throw "DbgAtlas service is already installed. Re-run without -NoForce to perform an overwrite install."
}

if ($existingStatus -and $existingStatus -ne "not_installed" -and $existingStatus -ne "stopped") {
    Write-Host "Stopping existing DbgAtlas service..."
    Invoke-Dbgatlas -Exe $dbgatlasExe -Arguments @("service", "stop")
}
else {
    Write-Host "No running DbgAtlas service needs to be stopped."
}

$installArgs = @("service", "install", "--bind", $Bind)
if (-not $NoForce) {
    $installArgs += "--force"
}

Write-Host "Installing DbgAtlas service from release payload..."
Invoke-Dbgatlas -Exe $dbgatlasExe -Arguments $installArgs

if ($NoStart) {
    Write-Host "NoStart was set; leaving service installed but stopped."
    Invoke-Dbgatlas -Exe $dbgatlasExe -Arguments @("--json", "service", "status")
    exit 0
}

Write-Host "Starting DbgAtlas service..."
Invoke-Dbgatlas -Exe $dbgatlasExe -Arguments @("service", "start")

Write-Host "Service status:"
Invoke-Dbgatlas -Exe $dbgatlasExe -Arguments @("--json", "service", "status")

Write-Host "Service health:"
Wait-DbgatlasHealth -Exe $dbgatlasExe
