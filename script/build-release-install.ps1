#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Bind = "127.0.0.1:7331",
    [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA "Programs\dbgatlas"),
    [switch]$BuildOnly,
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

function Copy-OptionalRuntimePayload {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir)

    $runtimeNames = @(
        "libgcc_s_seh-1.dll",
        "libstdc++-6.dll",
        "libwinpthread-1.dll"
    )
    $pathDirs = $env:Path -split ";" | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Container) }

    foreach ($name in $runtimeNames) {
        $destination = Join-Path $ReleaseDir $name
        if (Test-Path -LiteralPath $destination -PathType Leaf) {
            continue
        }
        $source = $null
        foreach ($dir in $pathDirs) {
            $candidate = Join-Path $dir $name
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $source = $candidate
                break
            }
        }
        if ($source) {
            Copy-Item -LiteralPath $source -Destination $destination -Force
            Write-Host "Copied optional runtime payload: $name"
        }
    }
}

function Quote-ProcessArgument {
    param([Parameter(Mandatory = $true)][string]$Argument)

    if ($Argument -notmatch '[\s"]') {
        return $Argument
    }

    return '"' + $Argument.Replace('"', '\"') + '"'
}

Assert-Windows

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path
$releaseDir = Join-Path $repoRoot "target\release"
$installScript = Join-Path $scriptDir "install-release.ps1"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo was not found in PATH."
}

if (-not (Test-Path -LiteralPath $installScript -PathType Leaf)) {
    throw "Missing install script: $installScript"
}

Write-Host "DbgAtlas release build context:"
Write-Host "  repo root: $repoRoot"
Write-Host "  release dir: $releaseDir"
Write-Host "  install root: $InstallRoot"
Write-Host "  bind: $Bind"
Write-Host "  BuildOnly=$BuildOnly NoStart=$NoStart NoForce=$NoForce"
Write-Host "Building DbgAtlas release payload..."
Push-Location $repoRoot
try {
    Invoke-Checked -FilePath "cargo" -Arguments @("build", "--workspace", "--release")
}
finally {
    Pop-Location
}

Copy-OptionalRuntimePayload -ReleaseDir $releaseDir
Assert-ReleasePayload -ReleaseDir $releaseDir
Write-Host "Release payload is ready: $releaseDir"

if ($BuildOnly) {
    Write-Host "BuildOnly was set; skipping elevated install."
    exit 0
}

$powershell = (Get-Command powershell.exe -ErrorAction Stop).Source
$installArgs = @(
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    (Quote-ProcessArgument $installScript),
    "-RepoRoot",
    (Quote-ProcessArgument $repoRoot),
    "-InstallRoot",
    (Quote-ProcessArgument $InstallRoot),
    "-Bind",
    $Bind
)

if ($NoStart) {
    $installArgs += "-NoStart"
}

if ($NoForce) {
    $installArgs += "-NoForce"
}

Write-Host "Starting elevated install script..."
$process = Start-Process -FilePath $powershell -ArgumentList $installArgs -Verb RunAs -Wait -PassThru
if ($process.ExitCode -ne 0) {
    throw "Elevated install failed with exit code $($process.ExitCode)."
}

Write-Host "DbgAtlas release install completed."
