<#
.SYNOPSIS
  对合并后的 OpenPaw engine-host 跑一遍完整联调流程。

.DESCRIPTION
  依次验证：Basic 鉴权 -> 建会话(200) -> prompt_async(204) -> 审批请求与回复(204)
  -> 会话消息(工具终态 + assistant 文本) -> AI Office 动作网关 /actions 抽干。

  请求体一律走临时文件，避免 Windows PowerShell 5.1 把 -d 参数里的引号吃掉
  （那会得到 400 "Failed to parse the request body as JSON"）。

.EXAMPLE
  .\smoke.ps1
  .\smoke.ps1 -Base http://localhost:8790 -Mock scenario
#>
[CmdletBinding()]
param(
  [string]$Base = 'http://localhost:8790',
  [string]$User = 'dev',
  [string]$Pass = 'dev',
  [string]$Gateway = 'http://localhost:8765',
  [string]$Title = 'smoke',
  [string]$Prompt = 'read the readme'
)

$ErrorActionPreference = 'Stop'
$auth = "${User}:${Pass}"
$np = '*'
$tmp = Join-Path $env:TEMP ("openpaw-smoke-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

function Write-Body([string]$name, [string]$json) {
  $p = Join-Path $tmp "$name.json"
  Set-Content -Path $p -Value $json -NoNewline -Encoding ascii
  return $p
}
function Http([string[]]$Arguments) { & curl.exe @Arguments 2>$null }

try {
  Write-Host "== 1) 鉴权：无凭据应 401，且带 WWW-Authenticate" -ForegroundColor Cyan
  $hdr = Http @('-s', '-i', '--noproxy', $np, "$Base/global/health")
  $hdr | Select-String -Pattern '^HTTP/|^www-authenticate' | ForEach-Object { Write-Host "   $_" }

  Write-Host "`n== 2) 带凭据 /global/health" -ForegroundColor Cyan
  Write-Host "   $(Http @('-s','--noproxy',$np,'-u',$auth,"$Base/global/health"))"

  Write-Host "`n== 3) POST /session（期望 200）" -ForegroundColor Cyan
  $b1 = Write-Body 'create' (@{ title = $Title } | ConvertTo-Json -Compress)
  $code = Http @('-s','-o',"$tmp\create.out",'-w','%{http_code}','--noproxy',$np,'-u',$auth,
                 '-H','Content-Type: application/json','--data-binary',"@$b1","$Base/session")
  Write-Host "   status=$code"
  $session = Get-Content "$tmp\create.out" -Raw | ConvertFrom-Json
  $sid = $session.id
  Write-Host "   id=$sid  directory=$($session.directory)"

  Write-Host "`n== 4) POST /session/:id/prompt_async（期望 204）" -ForegroundColor Cyan
  $b2 = Write-Body 'prompt' (@{
    model = @{ providerID = 'mock'; modelID = 'mock-1' }
    parts = @(@{ type = 'text'; text = $Prompt })
  } | ConvertTo-Json -Compress -Depth 6)
  $code = Http @('-s','-o','NUL','-w','%{http_code}','--noproxy',$np,'-u',$auth,
                 '-H','Content-Type: application/json','--data-binary',"@$b2",
                 "$Base/session/$sid/prompt_async")
  Write-Host "   status=$code"

  Write-Host "`n== 5) 审批：轮询 /permission 并回复 once" -ForegroundColor Cyan
  $b3 = Write-Body 'reply' '{"reply":"once"}'
  $replied = 0
  for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 250
    $pending = (Http @('-s','--noproxy',$np,'-u',$auth,"$Base/permission")) | ConvertFrom-Json
    if ($pending.Count -gt 0) {
      foreach ($p in $pending) {
        Write-Host "   请求: tool=$($p.permission) id=$($p.id)"
        $c = Http @('-s','-o','NUL','-w','%{http_code}','--noproxy',$np,'-u',$auth,
                    '-H','Content-Type: application/json','--data-binary',"@$b3",
                    "$Base/permission/$($p.id)/reply")
        Write-Host "   reply status=$c"
        $replied++
      }
    }
    $st = (Http @('-s','--noproxy',$np,'-u',$auth,"$Base/session/status")) | ConvertFrom-Json
    if (-not $st.$sid -or $st.$sid.type -ne 'busy') { break }
  }
  Write-Host "   已回复审批数=$replied"

  Write-Host "`n== 6) GET /session/:id/message" -ForegroundColor Cyan
  $msgs = (Http @('-s','--noproxy',$np,'-u',$auth,"$Base/session/$sid/message")) | ConvertFrom-Json
  foreach ($m in $msgs) {
    foreach ($p in $m.parts) {
      if ($p.type -eq 'text')      { Write-Host "   [$($m.info.role)] text: $($p.text)" }
      elseif ($p.type -eq 'tool')  { Write-Host "   [$($m.info.role)] tool: $($p.tool) -> $($p.state.status)" }
    }
  }

  Write-Host "`n== 7) AI Office 动作网关 /actions（抽干）" -ForegroundColor Cyan
  $actions = (Http @('-s','--noproxy',$np,"$Gateway/actions")) | ConvertFrom-Json
  foreach ($a in $actions.actions) { Write-Host "   $($a | ConvertTo-Json -Compress)" }

  Write-Host "`n会话 id: $sid" -ForegroundColor Green
  Write-Host "SSE 实时流（浏览器会弹 Basic 鉴权，输入 dev/dev）: $Base/event" -ForegroundColor Green
}
finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
