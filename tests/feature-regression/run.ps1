param([string[]] $CargoConfig = @())

$ErrorActionPreference = 'Stop'
$cargoPrefix = foreach ($setting in $CargoConfig) { '--config'; $setting }
$checks = @(
    @{ Name = 'Host bootstrap'; Args = @('test', '-p', 'openpaw-engine-host', '--features', 'engine-host', '--test', 'contract', '--locked') }
    @{ Name = 'CLI'; Args = @('test', '-p', 'openpaw-cli', '--locked') }
    @{ Name = 'Engine'; Args = @('test', '-p', 'openpaw-engine', '--locked') }
    @{ Name = 'Store'; Args = @('test', '-p', 'openpaw-store', '--locked') }
    @{ Name = 'Tools'; Args = @('test', '-p', 'openpaw-tools', '--locked') }
    @{ Name = 'Sidecar'; Args = @('test', '-p', 'openpaw-cli', '--features', 'sidecar', '--locked') }
)

foreach ($check in $checks) {
    Write-Host "Running $($check.Name) regression"
    & cargo @cargoPrefix @($check.Args)
    if ($LASTEXITCODE -ne 0) {
        throw "$($check.Name) regression failed with exit code $LASTEXITCODE"
    }
}

Write-Host 'All feature regression categories passed'
