#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Bind = "127.0.0.1:7331",
    [string]$InstallRoot,
    [switch]$BuildOnly,
    [switch]$InstallOnly,
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

function Assert-ReleasePayload {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir)

    $required = @(
        "dbgatlas.exe",
        "dbgatlas-worker.exe",
        "dbgatlas_dbgeng.dll",
        "dbgatlas_etw.dll",
        "dbgatlas_ida.dll"
    )

    foreach ($name in $required) {
        $path = Join-Path $ReleaseDir $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Release payload is incomplete: missing $path"
        }
    }
    Write-Host "Required release payload files are present: $($required -join ', ')"
}

function Install-ReleasePayload {
    param(
        [Parameter(Mandatory = $true)][string]$ReleaseDir,
        [string]$InstallRoot,
        [Parameter(Mandatory = $true)][string]$Bind,
        [switch]$NoStart,
        [switch]$NoForce
    )

    $dbgatlasExe = Join-Path $ReleaseDir "dbgatlas.exe"
    $installArgs = @(
        "service",
        "install",
        "--payload-mode",
        "copy",
        "--payload-dir",
        $ReleaseDir,
        "--bind",
        $Bind
    )
    if (-not [string]::IsNullOrWhiteSpace($InstallRoot)) {
        $installArgs += @("--install-root", $InstallRoot)
    }
    if (-not $NoForce) {
        $installArgs += "--force"
    }

    Write-Host "Installing DbgAtlas service from release payload..."
    Invoke-Checked -FilePath $dbgatlasExe -Arguments $installArgs

    if ($NoStart) {
        Write-Host "NoStart was set; leaving service installed but stopped."
        return
    }

    Write-Host "Starting DbgAtlas service..."
    $startArgs = @("service", "start")
    if (-not [string]::IsNullOrWhiteSpace($InstallRoot)) {
        $startArgs += @("--install-root", $InstallRoot)
    }
    Invoke-Checked -FilePath $dbgatlasExe -Arguments $startArgs
}

function Quote-ProcessArgument {
    param([Parameter(Mandatory = $true)][string]$Argument)

    if ($Argument -notmatch '[\s"]') {
        return $Argument
    }

    return '"' + $Argument.Replace('"', '\"') + '"'
}

Assert-Windows

if ($BuildOnly -and $InstallOnly) {
    throw "BuildOnly and InstallOnly cannot be used together."
}

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path
$releaseDir = Join-Path $repoRoot "target\release"

if (-not $InstallOnly -and -not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo was not found in PATH."
}

Write-Host "DbgAtlas release build context:"
Write-Host "  repo root: $repoRoot"
Write-Host "  release dir: $releaseDir"
if ([string]::IsNullOrWhiteSpace($InstallRoot)) {
    Write-Host "  install root: service default"
}
else {
    Write-Host "  install root: $InstallRoot"
}
Write-Host "  bind: $Bind"
Write-Host "  BuildOnly=$BuildOnly InstallOnly=$InstallOnly NoStart=$NoStart NoForce=$NoForce"

if (-not $InstallOnly) {
    Write-Host "Building DbgAtlas release payload..."
    Push-Location $repoRoot
    try {
        Invoke-Checked -FilePath "cargo" -Arguments @("build", "--workspace", "--release")
    }
    finally {
        Pop-Location
    }
}

Assert-ReleasePayload -ReleaseDir $releaseDir
Write-Host "Release payload is ready: $releaseDir"

if ($BuildOnly) {
    Write-Host "BuildOnly was set; skipping elevated install."
    exit 0
}

if (-not (Test-Administrator)) {
    $powershell = (Get-Command powershell.exe -ErrorAction Stop).Source
    $installArgs = @(
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        (Quote-ProcessArgument $PSCommandPath),
        "-InstallOnly",
        "-Bind",
        $Bind
    )
    if (-not [string]::IsNullOrWhiteSpace($InstallRoot)) {
        $installArgs += @("-InstallRoot", (Quote-ProcessArgument $InstallRoot))
    }

    if ($NoStart) {
        $installArgs += "-NoStart"
    }

    if ($NoForce) {
        $installArgs += "-NoForce"
    }

    Write-Host "Starting elevated install step..."
    $process = Start-Process -FilePath $powershell -ArgumentList $installArgs -Verb RunAs -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "Elevated install failed with exit code $($process.ExitCode)."
    }

    Write-Host "DbgAtlas release install completed."
    exit 0
}

$installParameters = @{
    ReleaseDir = $releaseDir
    Bind = $Bind
    NoStart = $NoStart
    NoForce = $NoForce
}
if (-not [string]::IsNullOrWhiteSpace($InstallRoot)) {
    $installParameters.InstallRoot = $InstallRoot
}

Install-ReleasePayload @installParameters

Write-Host "DbgAtlas release install completed."
