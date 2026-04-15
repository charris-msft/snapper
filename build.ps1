# build.ps1 — Build snapper and publish to tools folder
$ErrorActionPreference = 'Stop'

$repoRoot = $PSScriptRoot
$goDir = Join-Path $repoRoot 'go'
$outputExe = Join-Path $repoRoot 'snapper.exe'
$publishDir = 'c:\tools\localbin'
$publishExe = Join-Path $publishDir 'snapper.exe'

Write-Host "Building snapper..." -ForegroundColor Cyan
Push-Location $goDir
try {
    go build -o $outputExe .
    if ($LASTEXITCODE -ne 0) { throw "Go build failed" }
} finally {
    Pop-Location
}

$size = [math]::Round((Get-Item $outputExe).Length / 1MB, 1)
Write-Host "  Built: $outputExe ($size MB)" -ForegroundColor Green

Write-Host "Publishing to $publishDir..." -ForegroundColor Cyan
Copy-Item $outputExe $publishExe -Force
Write-Host "  Published: $publishExe" -ForegroundColor Green

Write-Host "`nDone! snapper.exe is ready." -ForegroundColor Green
