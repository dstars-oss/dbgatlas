param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [string]$IdaInstallDir = "C:\Program Files\IDA Professional 9.3",
    [string]$McpUrl = "http://127.0.0.1:7331/mcp",
    [string]$Token = $env:DBGATLAS_TOKEN,
    [string]$VsDevCmd,
    [switch]$RebuildIdb
)

$ErrorActionPreference = "Stop"

function Assert-True {
    param(
        [bool]$Condition,
        [string]$Message
    )
    if (-not $Condition) {
        throw $Message
    }
}

function Invoke-McpTool {
    param(
        [string]$Name,
        [hashtable]$Arguments,
        [switch]$AllowError
    )
    $payload = @{
        jsonrpc = "2.0"
        id = 1
        method = "tools/call"
        params = @{
            name = $Name
            arguments = $Arguments
        }
    } | ConvertTo-Json -Depth 32
    $response = Invoke-RestMethod `
        -Uri $McpUrl `
        -Method Post `
        -ContentType "application/json" `
        -Headers @{ Authorization = "Bearer $Token" } `
        -Body $payload
    $result = $response.result
    $text = [string]$result.content[0].text
    $value = $text | ConvertFrom-Json
    if ($result.isError -and -not $AllowError) {
        throw "$Name failed: $text"
    }
    [pscustomobject]@{
        IsError = [bool]$result.isError
        Value = $value
        Text = $text
    }
}

function Get-FunctionAddress {
    param(
        [object]$LookupResult,
        [string]$Name
    )
    foreach ($item in @($LookupResult.Value.result.items)) {
        if ($item.query -eq $Name -and $item.found) {
            return [UInt64]$item.function.address
        }
    }
    throw "Function was not found: $Name"
}

function Resolve-VsDevCmd {
    param([string]$Requested)

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        if (Test-Path -LiteralPath $Requested -PathType Leaf) {
            return (Resolve-Path -LiteralPath $Requested).Path
        }
        throw "VsDevCmd.bat was not found: $Requested"
    }

    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($env:VSINSTALLDIR)) {
        $candidates += Join-Path $env:VSINSTALLDIR "Common7\Tools\VsDevCmd.bat"
    }

    $vswhereCandidates = @()
    if (-not [string]::IsNullOrWhiteSpace(${env:ProgramFiles(x86)})) {
        $vswhereCandidates += Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    }
    if (-not [string]::IsNullOrWhiteSpace($env:ProgramFiles)) {
        $vswhereCandidates += Join-Path $env:ProgramFiles "Microsoft Visual Studio\Installer\vswhere.exe"
    }
    foreach ($vswhere in $vswhereCandidates) {
        if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
            continue
        }
        $installPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($installPath)) {
            $candidates += Join-Path $installPath "Common7\Tools\VsDevCmd.bat"
        }
    }

    $candidates += @(
        "C:\Program Files\Microsoft Visual Studio\18\Community\Common7\Tools\VsDevCmd.bat",
        "C:\Program Files\Microsoft Visual Studio\18\Professional\Common7\Tools\VsDevCmd.bat",
        "C:\Program Files\Microsoft Visual Studio\18\Enterprise\Common7\Tools\VsDevCmd.bat",
        "C:\Program Files\Microsoft Visual Studio\18\BuildTools\Common7\Tools\VsDevCmd.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\VsDevCmd.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\Professional\Common7\Tools\VsDevCmd.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\Common7\Tools\VsDevCmd.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat"
    )

    foreach ($candidate in $candidates | Select-Object -Unique) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    throw "VsDevCmd.bat was not found. Pass -VsDevCmd or install Visual Studio C++ Build Tools."
}

if ([string]::IsNullOrWhiteSpace($Token)) {
    throw "DBGATLAS_TOKEN is required, or pass -Token."
}

Write-Host "DbgAtlas reverse E2E context:"
Write-Host "  repo root: $RepoRoot"
Write-Host "  MCP URL: $McpUrl"
Write-Host "  IDA install dir: $IdaInstallDir"
Write-Host "  rebuild IDB: $RebuildIdb"

$workRoot = Join-Path $RepoRoot "temp\reverse-e2e"
$workspaceRoot = Join-Path $workRoot "workspace-root"
$runsRoot = Join-Path $workRoot "runs"
New-Item -ItemType Directory -Force -Path $workRoot, $workspaceRoot, $runsRoot | Out-Null
Write-Host "  work root: $workRoot"
Write-Host "  workspace root: $workspaceRoot"

$fixtureC = Join-Path $workRoot "dbgatlas_reverse_fixture.c"
$fixtureExe = Join-Path $workRoot "dbgatlas_reverse_fixture.exe"
$fixtureObj = Join-Path $workRoot "dbgatlas_reverse_fixture.obj"
$fixturePdb = Join-Path $workRoot "dbgatlas_reverse_fixture.pdb"
$buildCmd = Join-Path $workRoot "build_fixture.cmd"
$idaScript = Join-Path $workRoot "create_idb.py"
$idaLog = Join-Path $workRoot "ida-create.log"
$baseIdb = Join-Path $workRoot "dbgatlas_reverse_fixture.i64"

$fixtureSource = @'
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct FixtureRecord {
    uint32_t magic;
    uint32_t flags;
    const char *label;
    int values[4];
} FixtureRecord;

__declspec(dllexport) volatile int g_fixture_counter = 7;
__declspec(dllexport) const char g_fixture_banner[] = "DbgAtlas reverse fixture ready";

static FixtureRecord g_record = {
    0x44424741u,
    0x1020u,
    "record-alpha",
    { 3, 5, 8, 13 },
};

__declspec(noinline) int helper_mix(int seed, int value) {
    int mixed = seed ^ (value * 33);
    mixed += (int)strlen(g_record.label);
    return mixed + g_record.values[value & 3];
}

__declspec(noinline) int fixture_score(const FixtureRecord *rec, int salt) {
    int total = (int)(rec->magic ^ rec->flags) + salt;
    for (int i = 0; i < 4; ++i) {
        total = helper_mix(total, rec->values[i]);
    }
    return total;
}

__declspec(dllexport) int fixture_entry(int argc, const char **argv) {
    int salt = argc > 1 ? argv[1][0] : 0x42;
    int score = fixture_score(&g_record, salt);
    g_fixture_counter += score & 0xff;
    printf("%s: score=%d counter=%d\n", g_fixture_banner, score, g_fixture_counter);
    return score == 0x12345678;
}

int main(int argc, const char **argv) {
    return fixture_entry(argc, argv);
}
'@
Set-Content -LiteralPath $fixtureC -Value $fixtureSource -Encoding ASCII

$vsDevCmd = Resolve-VsDevCmd -Requested $VsDevCmd
Write-Host "Using VsDevCmd: $vsDevCmd"
$buildScript = @"
@echo off
setlocal
call "$vsDevCmd" -arch=x64 -host_arch=x64 >NUL
if errorlevel 1 exit /b %errorlevel%
cl /nologo /Zi /Od /W4 /D_CRT_SECURE_NO_WARNINGS /Fo"$fixtureObj" /Fd"$fixturePdb" /Fe"$fixtureExe" "$fixtureC" /link /DEBUG /PDB:"$fixturePdb" /INCREMENTAL:NO
exit /b %errorlevel%
"@
Set-Content -LiteralPath $buildCmd -Value $buildScript -Encoding ASCII
Write-Host "Building reverse fixture: $fixtureExe"
& cmd.exe /c $buildCmd
if ($LASTEXITCODE -ne 0) {
    throw "Fixture build failed with exit code $LASTEXITCODE"
}
Write-Host "Fixture build output:"
Write-Host "  exe: $fixtureExe"
Write-Host "  pdb: $fixturePdb"

$idaCreateScript = @'
import ida_auto
import ida_loader
import ida_nalt
import ida_pro
import idaapi

ida_auto.auto_wait()
print(f"[dbgatlas-fixture] input={ida_nalt.get_input_file_path()}")
print(f"[dbgatlas-fixture] imagebase=0x{idaapi.get_imagebase():x}")
ida_loader.save_database(None, ida_loader.DBFL_KILL)
ida_pro.qexit(0)
'@
Set-Content -LiteralPath $idaScript -Value $idaCreateScript -Encoding ASCII

$idaExe = Join-Path $IdaInstallDir "idat.exe"
if (-not (Test-Path -LiteralPath $idaExe)) {
    throw "idat.exe was not found: $idaExe"
}
if ($RebuildIdb -or -not (Test-Path -LiteralPath $baseIdb)) {
    Write-Host "Creating base IDB: $baseIdb"
    if ($RebuildIdb -and (Test-Path -LiteralPath $baseIdb)) {
        Remove-Item -LiteralPath $baseIdb -Force
    }
    & $idaExe "-A" "-L$idaLog" "-S$idaScript" "-o$baseIdb" $fixtureExe
    if ($LASTEXITCODE -ne 0) {
        throw "IDA database creation failed with exit code $LASTEXITCODE"
    }
} else {
    Write-Host "Reusing cached base IDB: $baseIdb"
}

$stamp = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
# baseIdb 是可复用基线；每次 MCP 测试复制成 timestamped work IDB，
# 避免 rename/comment/save 等写操作污染下一轮测试。
$workIdb = Join-Path $runsRoot "dbgatlas_reverse_fixture.mcp-work.$stamp.i64"
Copy-Item -LiteralPath $baseIdb -Destination $workIdb -Force
Write-Host "Working IDB: $workIdb"

$session = $null
try {
    $open = Invoke-McpTool "reverse.session.open" @{
        project_root = $workspaceRoot
        database_path = $workIdb
        ida_install_dir = $IdaInstallDir
    }
    $session = $open.Value.session_id
    Write-Host "Opened reverse session: $($session.id)"

    $lookup = Invoke-McpTool "reverse.lookup_funcs" @{
        session_id = $session
        queries = @("fixture_entry", "helper_mix", "fixture_score")
    }
    $entry = Get-FunctionAddress $lookup "fixture_entry"
    $helper = Get-FunctionAddress $lookup "helper_mix"
    Write-Host "Resolved fixture_entry=0x$("{0:x}" -f $entry), helper_mix=0x$("{0:x}" -f $helper)"

    $queryFuncs = Invoke-McpTool "reverse.query_funcs" @{
        session_id = $session
        filter = "fixture"
        sort_by = "name"
        offset = 0
        count = 20
    }
    $queryNames = @($queryFuncs.Value.result.items | ForEach-Object { $_.name })
    Assert-True ($queryNames -contains "fixture_entry") "query_funcs did not return fixture_entry"
    Assert-True ($queryNames -contains "fixture_score") "query_funcs did not return fixture_score"

    $spaced = Invoke-McpTool "reverse.find_bytes" @{
        session_id = $session
        patterns = @("44 62 67 41 74 6C 61 73")
        offset = 0
        limit = 10
    }
    Assert-True ([int]$spaced.Value.result.count -ge 1) "spaced find_bytes pattern did not match"

    $compact = Invoke-McpTool "reverse.find_bytes" @{
        session_id = $session
        patterns = @("44626741746c6173")
        offset = 0
        limit = 10
    }
    Assert-True ([int]$compact.Value.result.count -ge 1) "compact find_bytes pattern did not match"

    $invalid = Invoke-McpTool -Name "reverse.find_bytes" -Arguments @{
        session_id = $session
        patterns = @("123")
        offset = 0
        limit = 1
    } -AllowError
    Assert-True $invalid.IsError "invalid find_bytes pattern should be a tool error"
    Assert-True ($invalid.Text -like "*odd number*") "invalid find_bytes error was not descriptive"
    Assert-True (-not [string]::IsNullOrWhiteSpace([string]$invalid.Value.operation_id.id)) "invalid find_bytes error did not include operation_id"

    Invoke-McpTool "reverse.set_comments" @{
        session_id = $session
        items = @(@{ addr = $entry; text = "DbgAtlas script MCP comment" })
    } | Out-Null
    $disasm = Invoke-McpTool "reverse.disasm" @{
        session_id = $session
        addr = $entry
    }
    $disasmText = ($disasm.Value.result.instructions | ForEach-Object { $_.text }) -join "`n"
    Assert-True ($disasmText -like "*DbgAtlas script MCP comment*") "comment readback failed"

    Invoke-McpTool "reverse.rename" @{
        session_id = $session
        items = @(@{ kind = "function"; addr = $helper; new_name = "script_mcp_helper_mix" })
    } | Out-Null
    $renamed = Invoke-McpTool "reverse.lookup_funcs" @{
        session_id = $session
        queries = @("script_mcp_helper_mix")
    }
    Assert-True ([bool]$renamed.Value.result.items[0].found) "rename readback failed"

    $save = Invoke-McpTool "reverse.idb_save" @{
        session_id = $session
    }
    Assert-True ([bool]$save.Value.result.ok) "idb_save failed"
    Write-Host "Saved work IDB."
}
finally {
    if ($null -ne $session) {
        Write-Host "Closing reverse session: $($session.id)"
        Invoke-McpTool "reverse.session.close" @{ session_id = $session } -AllowError | Out-Null
    }
}

Write-Host "Reverse MCP E2E passed: $workIdb"
