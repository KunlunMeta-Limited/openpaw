#requires -Version 5.1
<#
.SYNOPSIS
  本机编排 OpenPaw engine-host 与 PawOffice Node 服务。
.DESCRIPTION
  与 openpaw-office.sh 保持相同的 build/start/stop/status 语义。
  只使用本机配置；不会复制 .env、打印密钥或删除运行数据。
  Windows 原生运行本轮仅提供入口，验收状态仍以 handoff.md 为准。
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('build', 'start', 'stop', 'status')]
    [string] $Command = 'status',
    [switch] $Force
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$UserHome = [Environment]::GetFolderPath('UserProfile')
if ([string]::IsNullOrWhiteSpace($UserHome)) { $UserHome = $HOME }
$EnvFile = if ($env:OPENPAW_ENV_FILE) { $env:OPENPAW_ENV_FILE } else { Join-Path $UserHome '.openpaw\.env' }
$HostConfig = if ($env:OPENPAW_HOST_CONFIG) { $env:OPENPAW_HOST_CONFIG } else { Join-Path $UserHome '.openpaw\engine-host-office.toml' }
$OfficeSource = if ($env:OPENPAW_OFFICE_SOURCE) { $env:OPENPAW_OFFICE_SOURCE } else { Join-Path (Split-Path $Root -Parent) 'ai-office-react' }
$RunDir = if ($env:OPENPAW_RUN_DIR) { $env:OPENPAW_RUN_DIR } else { Join-Path $env:LOCALAPPDATA 'OpenPaw\office-runtime' }
$HostPort = if ($env:OPENPAW_HOST_PORT) { [int]$env:OPENPAW_HOST_PORT } else { 32885 }
$OfficePort = if ($env:OPENPAW_OFFICE_PORT) { [int]$env:OPENPAW_OFFICE_PORT } else { 8765 }
$HostBinary = if ($env:OPENPAW_HOST_BINARY) { $env:OPENPAW_HOST_BINARY } else { Join-Path $Root 'target\debug\openpaw.exe' }
$Node = if ($env:OPENPAW_NODE) { $env:OPENPAW_NODE } else { 'node.exe' }
$HostPidFile = Join-Path $RunDir 'host.pid'
$OfficePidFile = Join-Path $RunDir 'office.pid'
$HostLog = Join-Path $RunDir 'host.log'
$OfficeLog = Join-Path $RunDir 'office.log'

function Fail([string] $Message) { throw "openpaw-office: $Message" }
function Ensure-RunDir { New-Item -ItemType Directory -Force -Path $RunDir | Out-Null }

function Read-Pid([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $raw = (Get-Content -LiteralPath $Path -Raw).Trim()
    $processId = 0
    if ([int]::TryParse($raw, [ref]$processId) -and $processId -gt 0) { return $processId }
    return $null
}

function Get-ProcessSafe([int] $ProcessId) {
    try { return Get-Process -Id $ProcessId -ErrorAction Stop } catch { return $null }
}

function Get-EnvValue([string] $Path, [string] $Name) {
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $line = Get-Content -LiteralPath $Path | Where-Object { $_ -match ('^\s*' + [regex]::Escape($Name) + '\s*=') } | Select-Object -First 1
    if (-not $line) { return '' }
    return (($line -replace ('^\s*' + [regex]::Escape($Name) + '\s*=\s*'), '').Trim()).Trim([char[]]@('"', "'"))
}

function Get-TomlValue([string] $Path, [string] $Name) {
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $line = Get-Content -LiteralPath $Path | Where-Object { $_ -match ('^\s*' + [regex]::Escape($Name) + '\s*=') } | Select-Object -First 1
    if (-not $line) { return '' }
    return ((($line -replace ('^\s*' + [regex]::Escape($Name) + '\s*=\s*'), '').Trim()).Trim([char[]]@('"', "'"))).ToLowerInvariant()
}

function Validate-Inputs {
    if (-not (Test-Path -LiteralPath $HostConfig -PathType Leaf)) { Fail "Host配置不存在：$HostConfig（设置 OPENPAW_HOST_CONFIG）" }
    if (-not (Test-Path -LiteralPath $EnvFile -PathType Leaf)) { Fail "模型.env不存在：$EnvFile（设置 OPENPAW_ENV_FILE）" }
    if (-not (Test-Path -LiteralPath $OfficeSource -PathType Container)) { Fail "Office源码目录不存在：$OfficeSource（设置 OPENPAW_OFFICE_SOURCE）" }
    if ([string]::IsNullOrWhiteSpace($env:PAWOFFICE_API_KEY)) { Fail '未设置 PAWOFFICE_API_KEY；密钥不能写入脚本或仓库' }
    $provider = Get-TomlValue $HostConfig 'provider'
    if ([string]::IsNullOrWhiteSpace($provider)) { $provider = (Get-EnvValue $EnvFile 'OPENPAW_PROVIDER').ToLowerInvariant() }
    if ([string]::IsNullOrWhiteSpace($provider) -or $provider -eq 'echo') { Fail '拒绝以 Echo 启动 Office；请在.env配置真实Provider' }
}

function Wait-Health([int] $Port, [string] $Label) {
    for ($i = 0; $i -lt 60; $i++) {
        try {
            $response = Invoke-WebRequest -UseBasicParsing -Uri ("http://127.0.0.1:{0}/health" -f $Port) -TimeoutSec 2
            if ($response.StatusCode -eq 200) { Write-Host ("openpaw-office: {0} healthy on 127.0.0.1:{1}" -f $Label, $Port); return }
        } catch { }
        Start-Sleep -Milliseconds 500
    }
    Fail "$Label健康检查失败；日志：$(if ($Label -eq 'Host') { $HostLog } else { $OfficeLog })"
}

function Assert-PortFree([int] $Port, [string] $Label) {
    $connection = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
    if ($connection) { Fail "$Label端口已被占用：$Port" }
}

function Build {
    Ensure-RunDir
    if (-not (Get-Command cargo.exe -ErrorAction SilentlyContinue)) { Fail 'cargo.exe 不在 PATH 中' }
    if (-not (Get-Command npm.cmd -ErrorAction SilentlyContinue)) { Fail 'npm.cmd 不在 PATH 中' }
    Write-Host 'openpaw-office: building OpenPaw engine-host + sidecar'
    Push-Location $Root
    try { & cargo.exe build --locked -p openpaw-cli --features engine-host,sidecar *> (Join-Path $RunDir 'build-openpaw.log'); if ($LASTEXITCODE -ne 0) { Fail "OpenPaw编译失败，日志：$(Join-Path $RunDir 'build-openpaw.log')" } } finally { Pop-Location }
    Write-Host 'openpaw-office: building PawOffice production bundle'
    Push-Location $OfficeSource
    try { & npm.cmd run build:full *> (Join-Path $RunDir 'build-office.log'); if ($LASTEXITCODE -ne 0) { Fail "Office编译失败，日志：$(Join-Path $RunDir 'build-office.log')" } } finally { Pop-Location }
    Write-Host "openpaw-office: build complete; logs are outside the repository: $RunDir"
}

function Start-ServiceProcess([string] $Label, [string] $PidPath, [string] $LogPath, [string] $FilePath, [string[]] $Arguments, [hashtable] $Environment) {
    $old = @{}
    foreach ($key in $Environment.Keys) { $old[$key] = [Environment]::GetEnvironmentVariable($key, 'Process'); [Environment]::SetEnvironmentVariable($key, [string]$Environment[$key], 'Process') }
    try {
        $process = Start-Process -FilePath $FilePath -ArgumentList $Arguments -WorkingDirectory $Root -RedirectStandardOutput $LogPath -RedirectStandardError ($LogPath + '.err') -PassThru
    } finally {
        foreach ($key in $Environment.Keys) { [Environment]::SetEnvironmentVariable($key, $old[$key], 'Process') }
    }
    Set-Content -LiteralPath $PidPath -Value $process.Id -Encoding ASCII
    Write-Host ("openpaw-office: {0} started (PID {1})" -f $Label, $process.Id)
}

function Stop-One([string] $Label, [string] $PidPath, [int] $Timeout, [bool] $AllowForce) {
    $processId = Read-Pid $PidPath
    if (-not $processId) { Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue; Write-Host "openpaw-office: $Label already stopped"; return }
    $process = Get-ProcessSafe $processId
    if (-not $process) { Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue; Write-Host "openpaw-office: $Label already stopped (stale PID removed)"; return }
    try { $process.CloseMainWindow() | Out-Null } catch { }
    try { $process.WaitForExit($Timeout * 1000) } catch { }
    if (-not $process.HasExited) {
        if (-not $AllowForce) { Fail "$Label 未在 ${Timeout}s 内完成优雅停止；未强制终止。确认后使用 stop -Force" }
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        Write-Host "openpaw-office: $Label force-stopped after ${Timeout}s"
    } else { Write-Host "openpaw-office: $Label stopped gracefully" }
    Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
}

function Start-Services {
    Validate-Inputs; Ensure-RunDir
    if (-not (Test-Path -LiteralPath $HostBinary -PathType Leaf)) { Fail "Host二进制不存在，请先执行 build：$HostBinary" }
    if (-not (Test-Path -LiteralPath (Join-Path $OfficeSource 'server\dist\index.js') -PathType Leaf)) { Fail 'Office生产服务不存在，请先执行 build' }
    Assert-PortFree $OfficePort 'Office'; Assert-PortFree $HostPort 'Host'
    Start-ServiceProcess 'Office' $OfficePidFile $OfficeLog $Node @((Join-Path $OfficeSource 'server\dist\index.js')) @{ PAWOFFICE_API_KEY = $env:PAWOFFICE_API_KEY; PAWOFFICE_STATIC_ROOT = (Join-Path $OfficeSource 'dist'); PORT = $OfficePort }
    try { Wait-Health $OfficePort 'Office'; Start-ServiceProcess 'Host' $HostPidFile $HostLog $HostBinary @('--env-file', $EnvFile, 'engine-host', '--config', $HostConfig, '--port', [string]$HostPort) @{ PAWOFFICE_API_KEY = $env:PAWOFFICE_API_KEY }; Wait-Health $HostPort 'Host' } catch { Stop-One 'Host' $HostPidFile 10 $true; Stop-One 'Office' $OfficePidFile 10 $true; throw }
    Write-Host "openpaw-office: all services started; logs: $OfficeLog and $HostLog"
}

function Status {
    Ensure-RunDir
    foreach ($item in @(@{ Label = 'Host'; Pid = $HostPidFile; Port = $HostPort }, @{ Label = 'Office'; Pid = $OfficePidFile; Port = $OfficePort })) {
        $processId = Read-Pid $item.Pid; $state = 'stopped'; $health = 'down'
        if ($processId -and (Get-ProcessSafe $processId)) { $state = "running (PID $processId)" }
        try { $r = Invoke-WebRequest -UseBasicParsing -Uri ("http://127.0.0.1:{0}/health" -f $item.Port) -TimeoutSec 2; if ($r.StatusCode -eq 200) { $health = 'healthy' } } catch { }
        Write-Host ("{0}: {1}, port={2}, health={3}" -f $item.Label, $state, $item.Port, $health)
    }
}

try {
    switch ($Command) {
        'build' { Build }
        'start' { Start-Services }
        'stop' { Ensure-RunDir; $failed = $false; try { Stop-One 'Host' $HostPidFile 60 $Force.IsPresent } catch { Write-Error $_; $failed = $true }; try { Stop-One 'Office' $OfficePidFile 30 $Force.IsPresent } catch { Write-Error $_; $failed = $true }; if ($failed) { exit 1 } }
        'status' { Status }
    }
} catch { Write-Error $_; exit 2 }
