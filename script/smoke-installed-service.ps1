#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA "Programs\dbgatlas"),
    [string]$Endpoint,
    [string]$Token,
    [string]$DbgAtlasExe,
    [int]$TimeoutSec = 15
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-DbgAtlasExe {
    if ($DbgAtlasExe) {
        return (Resolve-Path -LiteralPath $DbgAtlasExe).Path
    }

    $installed = Join-Path $InstallRoot "bin\dbgatlas.exe"
    if (Test-Path -LiteralPath $installed -PathType Leaf) {
        return (Resolve-Path -LiteralPath $installed).Path
    }

    $fromPath = Get-Command dbgatlas.exe -ErrorAction SilentlyContinue
    if ($fromPath) {
        return $fromPath.Source
    }

    throw "Could not find dbgatlas.exe. Pass -DbgAtlasExe or install the service payload first."
}

function Resolve-Endpoint {
    if ($Endpoint) {
        return $Endpoint
    }

    $configPath = Join-Path $InstallRoot "etc\runtime.toml"
    if (Test-Path -LiteralPath $configPath -PathType Leaf) {
        $config = Get-Content -LiteralPath $configPath -Raw
        $match = [regex]::Match($config, '(?m)^\s*bind\s*=\s*"([^"]+)"')
        if ($match.Success) {
            return $match.Groups[1].Value
        }
    }

    return "127.0.0.1:7331"
}

function Resolve-Token {
    if ($Token) {
        return $Token
    }

    if ($env:DBGATLAS_TOKEN) {
        return $env:DBGATLAS_TOKEN
    }

    $tokenPath = Join-Path $InstallRoot "etc\token"
    if (Test-Path -LiteralPath $tokenPath -PathType Leaf) {
        return (Get-Content -LiteralPath $tokenPath -Raw).Trim()
    }

    throw "Could not resolve service bearer token. Set DBGATLAS_TOKEN, pass -Token, or install the service first."
}

function Invoke-JsonCli {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $output = & $Exe @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $Exe $($Arguments -join ' ')`n$output"
    }
    return ($output | Out-String | ConvertFrom-Json)
}

function Invoke-Mcp {
    param(
        [Parameter(Mandatory = $true)][string]$EndpointValue,
        [Parameter(Mandatory = $true)][string]$BearerToken,
        [Parameter(Mandatory = $true)][object]$Body
    )

    $headers = @{ Authorization = "Bearer $BearerToken" }
    $json = $Body | ConvertTo-Json -Depth 20 -Compress
    return Invoke-RestMethod `
        -Uri "http://$EndpointValue/mcp" `
        -Method Post `
        -Headers $headers `
        -ContentType "application/json" `
        -Body $json `
        -TimeoutSec $TimeoutSec
}

function Get-JsonProperty {
    param(
        [Parameter(Mandatory = $true)]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }
    return $property.Value
}

$exe = Resolve-DbgAtlasExe
$endpointValue = Resolve-Endpoint
$bearerToken = Resolve-Token

Write-Host "DbgAtlas installed-service smoke:"
Write-Host "  exe: $exe"
Write-Host "  endpoint: http://$endpointValue"
Write-Host "  install root: $InstallRoot"

$health = Invoke-JsonCli -Exe $exe -Arguments @(
    "--json",
    "service",
    "health",
    "--endpoint",
    $endpointValue,
    "--token",
    $bearerToken
)
$healthError = Get-JsonProperty -Object $health -Name "error"
if ($null -ne $healthError) {
    throw "service.health returned JSON-RPC error: $($healthError.message)"
}
if ($health.result.status -ne "ok") {
    throw "service.health did not report ok: $($health | ConvertTo-Json -Depth 20)"
}

$info = Invoke-JsonCli -Exe $exe -Arguments @(
    "--json",
    "service",
    "info",
    "--endpoint",
    $endpointValue,
    "--token",
    $bearerToken
)
$infoError = Get-JsonProperty -Object $info -Name "error"
if ($null -ne $infoError) {
    throw "service.info returned JSON-RPC error: $($infoError.message)"
}

$initialize = Invoke-Mcp -EndpointValue $endpointValue -BearerToken $bearerToken -Body @{
    jsonrpc = "2.0"
    id = 1
    method = "initialize"
    params = @{
        protocolVersion = "2024-11-05"
        capabilities = @{}
        clientInfo = @{
            name = "dbgatlas-smoke"
            version = "0.1.0"
        }
    }
}
$toolsCapability = $initialize.result.capabilities.PSObject.Properties["tools"]
if ($null -eq $toolsCapability) {
    throw "MCP initialize did not return tools capability."
}

$tools = Invoke-Mcp -EndpointValue $endpointValue -BearerToken $bearerToken -Body @{
    jsonrpc = "2.0"
    id = 2
    method = "tools/list"
    params = @{}
}
$toolNames = @($tools.result.tools | ForEach-Object { $_.name })
if ($toolNames -notcontains "service.health") {
    throw "MCP tools/list did not include service.health."
}

$mcpHealth = Invoke-Mcp -EndpointValue $endpointValue -BearerToken $bearerToken -Body @{
    jsonrpc = "2.0"
    id = 3
    method = "tools/call"
    params = @{
        name = "service.health"
        arguments = @{}
    }
}
$mcpHealthResult = Get-JsonProperty -Object $mcpHealth -Name "result"
if ($mcpHealthResult.isError) {
    throw "MCP service.health tool returned isError=true."
}

Write-Host "Installed-service smoke passed."
