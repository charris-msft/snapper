#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$PackagePath = 'dist\SteadyCapture',
    [string]$EvidencePath = 'artifacts\portable-acceptance',
    [ValidateSet('All','Media','Faults')][string]$Suite = 'All'
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path "$PSScriptRoot\..").Path
$PackagePath = [IO.Path]::GetFullPath($PackagePath, $Root)
$EvidencePath = [IO.Path]::GetFullPath($EvidencePath, $Root)
New-Item -ItemType Directory -Force $EvidencePath | Out-Null
$names = if ($Suite -eq 'All') { @('Media','Faults') } else { @($Suite) }
foreach ($name in $names) {
    $directory = Join-Path $EvidencePath $name.ToLowerInvariant()
    $option = if ($name -eq 'Media') { '--self-test' } else { '--fault-test' }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = Join-Path $PackagePath 'SteadyCapture.exe'
    $info.ArgumentList.Add($option)
    $info.ArgumentList.Add($directory)
    $info.WorkingDirectory = $Root
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.EnvironmentVariables['PATH'] = "$env:SystemRoot\System32;$env:SystemRoot"
    [void]$info.EnvironmentVariables.Remove('STEADYCAPTURE_FFMPEG')
    [void]$info.EnvironmentVariables.Remove('STEADYCAPTURE_FFPROBE')
    $child = [Diagnostics.Process]::Start($info)
    $stdout = $child.StandardOutput.ReadToEndAsync()
    $stderr = $child.StandardError.ReadToEndAsync()
    if (-not $child.WaitForExit(360000)) {
        Stop-Process -Id $child.Id
        throw "$name acceptance exceeded six minutes. Test-owned jobs are terminated on process exit."
    }
    $stdout.Result | Set-Content (Join-Path $EvidencePath "$name.stdout.txt") -Encoding utf8
    $stderr.Result | Set-Content (Join-Path $EvidencePath "$name.stderr.txt") -Encoding utf8
    Write-Output $stdout.Result
    if ($child.ExitCode -ne 0) { throw "$name acceptance failed: $($stderr.Result)" }
    $child.Dispose()
}
Write-Output 'Portable acceptance finished with a clean PATH. These suites do not activate real devices.'
