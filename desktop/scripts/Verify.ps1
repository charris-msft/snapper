#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PackagePath,
    [string]$EvidencePath = 'artifacts\portable-acceptance',
    [ValidateSet('All','Media','Faults','Capture','Video','Profile','Encoders','Native')][string]$Suite = 'All',
    [ValidateRange(12,600)][int]$ProfileSeconds = 12,
    [string]$RequestPath,
    [switch]$ApproveScreenCapture
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path "$PSScriptRoot\..").Path
if ($Suite -eq 'Native') {
    if (-not $ApproveScreenCapture -or -not $RequestPath) {
        throw 'Native profiling requires an explicit request JSON and approval for two real-screen captures. No capture was started.'
    }
    $RequestPath = [IO.Path]::GetFullPath($RequestPath, $Root)
}
$PackagePath = [IO.Path]::GetFullPath($PackagePath, $Root)
$EvidencePath = [IO.Path]::GetFullPath($EvidencePath, $Root)
New-Item -ItemType Directory -Force $EvidencePath | Out-Null
$names = if ($Suite -eq 'All') { @('Media','Faults','Capture','Video') } else { @($Suite) }
foreach ($name in $names) {
    $directory = Join-Path $EvidencePath $name.ToLowerInvariant()
    $option = switch ($name) {
        'Media' { '--self-test' } 'Faults' { '--fault-test' } 'Capture' { '--pause-test' }
        'Video' { '--video-edit-test' }
        'Profile' { '--profile-test' } 'Encoders' { '--encoder-diagnostics' } 'Native' { '--native-profile' }
    }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = Join-Path $PackagePath 'snapper-desktop.exe'
    $info.ArgumentList.Add($option)
    if ($name -eq 'Native') { $info.ArgumentList.Add($RequestPath) }
    $info.ArgumentList.Add($directory)
    if ($name -eq 'Profile') { $info.ArgumentList.Add($ProfileSeconds.ToString()) }
    if ($name -eq 'Native') { $info.ArgumentList.Add('--approve-screen-capture') }
    $info.WorkingDirectory = $Root
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.EnvironmentVariables['PATH'] = "$env:SystemRoot\System32;$env:SystemRoot"
    [void]$info.EnvironmentVariables.Remove('STEADYCAPTURE_FFMPEG')
    [void]$info.EnvironmentVariables.Remove('STEADYCAPTURE_FFPROBE')
    [void]$info.EnvironmentVariables.Remove('SNAPPER_FFMPEG')
    [void]$info.EnvironmentVariables.Remove('SNAPPER_FFPROBE')
    $child = [Diagnostics.Process]::Start($info)
    $stdout = $child.StandardOutput.ReadToEndAsync()
    $stderr = $child.StandardError.ReadToEndAsync()
    $timeoutSeconds = if ($name -eq 'Native') { 900 } elseif ($name -eq 'Profile') { $ProfileSeconds + 240 } else { 360 }
    if (-not $child.WaitForExit($timeoutSeconds * 1000)) {
        Stop-Process -Id $child.Id
        throw "$name exceeded $timeoutSeconds seconds. Test-owned jobs are terminated on process exit; completed files remain in $directory. Inspect the evidence before retrying."
    }
    $stdout.Result | Set-Content (Join-Path $EvidencePath "$name.stdout.txt") -Encoding utf8
    $stderr.Result | Set-Content (Join-Path $EvidencePath "$name.stderr.txt") -Encoding utf8
    Write-Output $stdout.Result
    if ($child.ExitCode -ne 0) { throw "$name acceptance failed: $($stderr.Result)" }
    $child.Dispose()
}
Write-Output $(if ($Suite -eq 'Native') {
    'Approved native screen measurements finished with a clean PATH. Private inputs remained OFF; inspect the measurements rather than inferring a universal performance pass.'
} else {
    'Portable acceptance finished with a clean PATH. These suites do not activate screen, camera or audio capture.'
})
