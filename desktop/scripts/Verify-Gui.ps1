#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PackagePath,
    [Parameter(Mandatory)][string]$EvidencePath,
    [Parameter(Mandatory)][switch]$ApproveWindowTest
)
$ErrorActionPreference = 'Stop'
if (-not $ApproveWindowTest) { throw 'This check opens a real native window. Explicit approval is required.' }
$Root = (Resolve-Path "$PSScriptRoot\..").Path
$PackagePath = [IO.Path]::GetFullPath($PackagePath, $Root)
$EvidencePath = [IO.Path]::GetFullPath($EvidencePath, $Root)
New-Item -ItemType Directory -Force $EvidencePath | Out-Null
$report = Join-Path $EvidencePath 'gui-startup-check.json'
$info = [Diagnostics.ProcessStartInfo]::new()
$info.FileName = Join-Path $PackagePath 'SteadyCapture.exe'
$info.ArgumentList.Add('--gui-smoke-test')
$info.ArgumentList.Add($report)
$info.WorkingDirectory = $Root
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$info.RedirectStandardOutput = $true
$info.RedirectStandardError = $true
$info.EnvironmentVariables['PATH'] = "$env:SystemRoot\System32;$env:SystemRoot"
$child = [Diagnostics.Process]::Start($info)
$stdout = $child.StandardOutput.ReadToEndAsync()
$stderr = $child.StandardError.ReadToEndAsync()
if (-not $child.WaitForExit(30000)) {
    Stop-Process -Id $child.Id
    throw 'Native window verification exceeded 30 seconds; the test process was stopped.'
}
$stdout.Result | Set-Content (Join-Path $EvidencePath 'gui.stdout.txt') -Encoding utf8
$stderr.Result | Set-Content (Join-Path $EvidencePath 'gui.stderr.txt') -Encoding utf8
if ($child.ExitCode -ne 0) { throw "Native window verification failed: $($stderr.Result)" }
$evidence = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
if ($evidence.passed -ne $true) { throw 'Native framebuffer verification did not pass.' }
if ($evidence.main_canvas.oracle -ne 'main-canvas-tile-pattern-v2' -or
    $evidence.main_canvas.tiles_verified -ne 48 -or
    $evidence.main_canvas.interior_samples_verified -ne 432 -or
    $evidence.main_canvas.recent_and_chrome_excluded -ne $true) {
    throw 'Fixture-specific main-canvas framebuffer proof is missing; thumbnail or chrome colors are not sufficient.'
}
$actual = (Get-FileHash $info.FileName -Algorithm SHA256).Hash
if ($actual -ne $evidence.build.sha256) { throw 'GUI report does not match the tested executable.' }
$child.Dispose()
Write-Output "Actual native window/framebuffer verified: $report"
