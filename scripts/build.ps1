# Same checks as CI, then a release build.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
foreach ($step in @('fmt --check', 'clippy --all-targets -- -D warnings', 'test', 'build --release')) {
    Invoke-Expression "cargo $step"
    if ($LASTEXITCODE -ne 0) { throw "cargo $step failed" }
}
Write-Host 'Built target\release\claudewatch.exe'
