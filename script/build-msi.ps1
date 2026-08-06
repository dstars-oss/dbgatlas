#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$ProductVersion,
    [string]$OutputDir,
    [switch]$SkipBuild,
    [switch]$SkipWixExtensionInstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-Windows {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "DbgAtlas MSI build scripts are only supported on Windows."
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

function Get-WorkspacePackageVersion {
    param([Parameter(Mandatory = $true)][string]$CargoToml)

    $inWorkspacePackage = $false
    foreach ($line in Get-Content -LiteralPath $CargoToml) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(.+)\]$') {
            $inWorkspacePackage = ($Matches[1] -eq "workspace.package")
            continue
        }

        if ($inWorkspacePackage -and $trimmed -match '^version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }

    throw "Could not find [workspace.package] version in $CargoToml."
}

function Assert-MsiProductVersion {
    param([Parameter(Mandatory = $true)][string]$Version)

    if ($Version -notmatch '^\d+\.\d+\.\d+$') {
        throw "MSI ProductVersion must be numeric major.minor.patch, got '$Version'. Pass -ProductVersion explicitly if needed."
    }
}

function Resolve-RepoPath {
    param(
        [Parameter(Mandatory = $true)][string]$RepoRoot,
        [Parameter(Mandatory = $true)][string]$Path
    )

    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }

    return [IO.Path]::GetFullPath((Join-Path $RepoRoot $Path))
}

function Assert-ReleasePayload {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir)

    $required = @(
        "dbgatlas.exe",
        "dbgatlas-worker.exe",
        "dbgatlas_dbgeng.dll",
        "dbgatlas_etw.dll",
        "dbgatlas_ida_sdk_930.dll",
        "dbgatlas_ida_sdk_940.dll"
    )

    foreach ($name in $required) {
        $path = Join-Path $ReleaseDir $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Release payload is incomplete: missing $path"
        }
    }
    $legacyIdaAdapter = Join-Path $ReleaseDir "dbgatlas_ida.dll"
    if (Test-Path -LiteralPath $legacyIdaAdapter -PathType Leaf) {
        throw "Release payload contains the retired unversioned IDA adapter: $legacyIdaAdapter"
    }
}

function Ensure-WixUiExtension {
    param([Parameter(Mandatory = $true)][string]$RepoRoot)

    $extensionDir = Join-Path $RepoRoot ".wix\extensions\WixToolset.UI.wixext\7.0.0"
    if (Test-Path -LiteralPath $extensionDir -PathType Container) {
        Write-Host "WiX UI extension is already available: $extensionDir"
        return
    }

    Write-Host "Installing WiX UI extension into the repository WiX cache..."
    Push-Location $RepoRoot
    try {
        Invoke-Checked -FilePath "wix" -Arguments @(
            "extension",
            "add",
            "WixToolset.UI.wixext/7.0.0",
            "--acceptEula",
            "wix7"
        )
    }
    finally {
        Pop-Location
    }
}

Assert-Windows

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path
$releaseDir = Join-Path $repoRoot "target\release"
$installerSource = Join-Path $repoRoot "xtask\installer\DbgAtlas.wxs"

if ([string]::IsNullOrWhiteSpace($ProductVersion)) {
    $ProductVersion = Get-WorkspacePackageVersion -CargoToml (Join-Path $repoRoot "Cargo.toml")
}
Assert-MsiProductVersion -Version $ProductVersion

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $repoRoot "target\installer"
}
else {
    $OutputDir = Resolve-RepoPath -RepoRoot $repoRoot -Path $OutputDir
}

$msiPath = Join-Path $OutputDir "DbgAtlas-$ProductVersion-x64.msi"

if (-not $SkipBuild -and -not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo was not found in PATH."
}
if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw "wix was not found in PATH. Install WiX Toolset v7 or add wix.exe to PATH."
}

Write-Host "DbgAtlas MSI build context:"
Write-Host "  repo root: $repoRoot"
Write-Host "  release dir: $releaseDir"
Write-Host "  installer source: $installerSource"
Write-Host "  product version: $ProductVersion"
Write-Host "  output: $msiPath"
Write-Host "  SkipBuild=$SkipBuild SkipWixExtensionInstall=$SkipWixExtensionInstall"

if (-not $SkipBuild) {
    $buildScript = Join-Path $scriptDir "build-release-install.ps1"
    Write-Host "Building DbgAtlas release payload..."
    Invoke-Checked -FilePath "powershell.exe" -Arguments @(
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        $buildScript,
        "-BuildOnly"
    )
}

Assert-ReleasePayload -ReleaseDir $releaseDir

if (-not $SkipWixExtensionInstall) {
    Ensure-WixUiExtension -RepoRoot $repoRoot
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

Write-Host "Building DbgAtlas MSI..."
Push-Location $repoRoot
try {
    Invoke-Checked -FilePath "wix" -Arguments @(
        "build",
        "-acceptEula",
        "wix7",
        "-ext",
        "WixToolset.UI.wixext",
        "-arch",
        "x64",
        $installerSource,
        "-d",
        "ReleaseDir=$releaseDir",
        "-d",
        "ProductVersion=$ProductVersion",
        "-o",
        $msiPath
    )
}
finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $msiPath -PathType Leaf)) {
    throw "MSI build completed without producing expected file: $msiPath"
}

Write-Host "DbgAtlas MSI is ready: $msiPath"
