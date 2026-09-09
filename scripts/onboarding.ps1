# OpenPaw onboarding — one-shot "can a fresh clone actually run?" gate.
#
# Run from the repository root (the script resolves the repo root relative
# to its own location):
#   powershell -ExecutionPolicy Bypass -File scripts\onboarding.ps1   (5.1+)
#   pwsh ./scripts/onboarding.ps1                                     (7+)
#
# Steps (each reports PASS/FAIL):
#   1. cargo --version                toolchain present
#   2. cargo build -p openpaw-cli     timed debug build
#   3. target\debug\openpaw hello     one piped line -> "Echo: <line>"
#   4. openpaw doctor                 output audit (summary + network-audit report)
#   5. openpaw evolve status          evolution surface loads on a throwaway home
#
# Onboarding success-rate metric: one exit-0 run counts as one successful
# onboarding sample. In CI the rate is the share of matrix runners
# (windows-latest / ubuntu-latest / macos-latest) where this script passes.
#
# Every run is hermetic: OpenPaw state goes to a temp home, never ~/.openpaw.

$ErrorActionPreference = "Stop"

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$opHome = Join-Path ([IO.Path]::GetTempPath()) ("openpaw-onboarding-" + $PID)
$exe = Join-Path $root "target\debug\openpaw.exe"
$buildLog = Join-Path ([IO.Path]::GetTempPath()) ("openpaw-onboarding-build-" + $PID + ".log")

$total = [System.Diagnostics.Stopwatch]::StartNew()
$script:failures = 0

function Step {
    param([string]$Name, [bool]$Ok, [string]$Detail = "")
    if ($Ok) {
        Write-Output ("[{0}] PASS {1}" -f $Name, $Detail)
    } else {
        Write-Output ("[{0}] FAIL {1}" -f $Name, $Detail)
        $script:failures += 1
    }
}

# Native stderr can surface as terminating errors under EAP=Stop on Windows
# PowerShell 5.1; temporarily relax while driving the openpaw binary.
function Invoke-NativeOutput {
    param([scriptblock]$Block)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        return ((& $Block) 2>&1 | Out-String), $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $prev
    }
}

New-Item -ItemType Directory -Force -Path $opHome | Out-Null

try {
    # 1. Toolchain check.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    cargo --version *> $null
    $cargoOk = ($LASTEXITCODE -eq 0)
    $cargoVersion = if ($cargoOk) { (cargo --version) -join " " } else { "cargo not found" }
    $ErrorActionPreference = $prev
    Step "1/5 cargo --version" $cargoOk $cargoVersion

    # 2. Timed debug build of the CLI. Output goes to a log file so the
    #    console only shows the timing (trailing errors on failure).
    $build = [System.Diagnostics.Stopwatch]::StartNew()
    $ErrorActionPreference = "Continue"
    & cargo build -p openpaw-cli *> $buildLog
    $buildOk = ($LASTEXITCODE -eq 0)
    $ErrorActionPreference = $prev
    $build.Stop()
    if (-not $buildOk) { Get-Content $buildLog | Select-Object -Last 20 | Write-Output }
    Step "2/5 cargo build -p openpaw-cli" $buildOk ("{0:N1}s" -f $build.Elapsed.TotalSeconds)

    $binPresent = $buildOk -and (Test-Path $exe)

    # 3. hello — one piped line, echo reply, exit 0.
    $helloDetail = "binary not built"
    $helloOk = $false
    if ($binPresent) {
        $helloOut, $helloCode = Invoke-NativeOutput { ("hello openpaw", "exit") | & $exe hello }
        $helloOk = ($helloCode -eq 0) -and $helloOut.Contains("Echo: hello openpaw")
        if ($helloOk) { $helloDetail = "echo reply received" } else { $helloDetail = $helloOut.Trim() }
    }
    Step "3/5 openpaw hello" $helloOk $helloDetail

    # 4. doctor — exit 0 plus an auditable report: the summary line, at least
    #    one [PASS] check, and the network-audit section. WARN never fails
    #    (the doctor itself exits 0 with warnings).
    $doctorDetail = "binary not built"
    $doctorOk = $false
    if ($binPresent) {
        $doctorOut, $doctorCode = Invoke-NativeOutput { & $exe doctor --home $opHome --workspace $root }
        $doctorOk = ($doctorCode -eq 0) `
            -and $doctorOut.Contains("doctor summary:") `
            -and $doctorOut.Contains("[PASS]") `
            -and $doctorOut.Contains("network call sites")
        if ($doctorOk) { $doctorDetail = "summary + network-audit report audited" } else { $doctorDetail = $doctorOut.Trim() }
    }
    Step "4/5 openpaw doctor" $doctorOk $doctorDetail

    # 5. evolve status — the knowledge/policy surface loads from an empty home.
    $evolveDetail = "binary not built"
    $evolveOk = $false
    if ($binPresent) {
        $evolveOut, $evolveCode = Invoke-NativeOutput { & $exe evolve status --home $opHome }
        $evolveOk = ($evolveCode -eq 0) -and $evolveOut.Contains("unverified knowledge artifacts:")
        if ($evolveOk) { $evolveDetail = "surface loads on empty home" } else { $evolveDetail = $evolveOut.Trim() }
    }
    Step "5/5 openpaw evolve status" $evolveOk $evolveDetail
} finally {
    $total.Stop()
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $opHome
    Remove-Item -Force -ErrorAction SilentlyContinue $buildLog
}

if ($script:failures -eq 0) {
    Write-Output ("onboarding: PASS (5 steps, {0:N1}s)" -f $total.Elapsed.TotalSeconds)
    exit 0
} else {
    Write-Output ("onboarding: FAIL ({0}/5 steps, {1:N1}s)" -f (5 - $script:failures), $total.Elapsed.TotalSeconds)
    exit 1
}
