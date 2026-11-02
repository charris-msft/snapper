#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
    [string]$OutputPath
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\Environment.ps1"
$Root = (Resolve-Path "$PSScriptRoot\..").Path
if (-not $OutputPath) { $OutputPath = Join-Path $Root 'dist\SteadyCapture' }
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath, $Root)
Push-Location $Root
try {
    $dirty = -not [string]::IsNullOrWhiteSpace((& git status --porcelain | Out-String))
    if ($Configuration -eq 'Release' -and $dirty) {
        throw 'Release packaging requires a committed, clean source tree so its evidence has an exact source identity.'
    }
    $buildArguments = @('build', '--locked', '--quiet')
    if ($Configuration -eq 'Release') { $buildArguments += '--release' }
    & cargo @buildArguments
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed ($LASTEXITCODE)." }
    $Executable = Join-Path $Root "target\$($Configuration.ToLowerInvariant())\steadycapture.exe"
    New-Item -ItemType Directory -Force "$OutputPath\tools","$OutputPath\licenses","$OutputPath\docs" | Out-Null
    Copy-Item -LiteralPath $Executable -Destination "$OutputPath\SteadyCapture.exe" -Force
    Copy-Item -LiteralPath "$Tools\llvm-mingw-20261020-ucrt-x86_64\bin\libunwind.dll" `
        -Destination "$OutputPath\libunwind.dll" -Force
    Copy-Item -LiteralPath $env:STEADYCAPTURE_FFMPEG -Destination "$OutputPath\tools\ffmpeg.exe" -Force
    Copy-Item -LiteralPath $env:STEADYCAPTURE_FFPROBE -Destination "$OutputPath\tools\ffprobe.exe" -Force
    Copy-Item -LiteralPath "$Root\LICENSE","$Root\README.md" -Destination $OutputPath -Force
    Get-ChildItem "$Root\docs" -Filter '*.md' -File | Copy-Item -Destination "$OutputPath\docs" -Force
    Copy-Item -LiteralPath "$Tools\ffmpeg-9.0.2-essentials_build\LICENSE" `
        -Destination "$OutputPath\licenses\FFmpeg-GPLv3.txt" -Force
    Copy-Item -LiteralPath "$Tools\ffmpeg-9.0.2-essentials_build\README.txt" `
        -Destination "$OutputPath\licenses\FFmpeg-build-README.txt" -Force
    Copy-Item -LiteralPath "$Tools\llvm-mingw-20261020-ucrt-x86_64\LICENSE.TXT" `
        -Destination "$OutputPath\licenses\LLVM-MinGW-NOTICE.txt" -Force
    $metadata = (& cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-gnullvm | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot collect dependency license metadata.' }
    $dependencyLicenses = foreach ($package in $metadata.packages) {
        [pscustomobject]@{ name=$package.name; version=$package.version; license=$package.license; repository=$package.repository }
        $directory = Split-Path $package.manifest_path
        $licenses = Get-ChildItem -LiteralPath $directory -File |
            Where-Object Name -Match '^(LICENSE|COPYING|NOTICE)'
        foreach ($license in $licenses) {
            Copy-Item -LiteralPath $license.FullName `
                -Destination "$OutputPath\licenses\$($package.name)-$($package.version)-$($license.Name)" -Force
        }
        if ($package.name -eq 'epaint_default_fonts') {
            Get-ChildItem (Join-Path $directory 'fonts') -Filter '*.txt' -File | ForEach-Object {
                Copy-Item -LiteralPath $_.FullName `
                    -Destination "$OutputPath\licenses\font-$($_.Name)" -Force
            }
        }
    }
    $dependencyLicenses | ConvertTo-Json -Depth 4 | Set-Content "$OutputPath\licenses\dependency-inventory.json" -Encoding utf8
    $probe = [System.Diagnostics.ProcessStartInfo]::new()
    $probe.FileName = "$OutputPath\SteadyCapture.exe"
    $probe.Arguments = '--devices'
    $probe.UseShellExecute = $false
    $probe.CreateNoWindow = $true
    $probe.RedirectStandardOutput = $true
    $probe.RedirectStandardError = $true
    $probe.EnvironmentVariables['PATH'] = "$env:SystemRoot\System32;$env:SystemRoot"
    $child = [System.Diagnostics.Process]::Start($probe)
    $stdout = $child.StandardOutput.ReadToEndAsync()
    $stderr = $child.StandardError.ReadToEndAsync()
    if (-not $child.WaitForExit(15000)) {
        Stop-Process -Id $child.Id
        throw 'Portable startup did not complete within 15 seconds with a clean PATH. Package is not verified.'
    }
    if ($child.ExitCode -ne 0) { throw "Portable startup failed: $($stderr.Result)" }
    $startup = $stdout.Result | ConvertFrom-Json
    [pscustomobject]@{
        clean_path="$env:SystemRoot\System32;$env:SystemRoot"; exit_code=$child.ExitCode
        command='--devices'; result=$startup; stderr=$stderr.Result
    } | ConvertTo-Json -Depth 8 | Set-Content "$OutputPath\startup-check.json" -Encoding utf8
    $child.Dispose()
    $files = foreach ($file in @('SteadyCapture.exe','libunwind.dll','tools\ffmpeg.exe','tools\ffprobe.exe')) {
        $path = Join-Path $OutputPath $file
        [pscustomobject]@{ file=$file; sha256=(Get-FileHash $path -Algorithm SHA256).Hash; bytes=(Get-Item $path).Length }
    }
    [pscustomobject]@{
        version='0.1.0'; configuration=$Configuration; target='x86_64-pc-windows-gnullvm'
        commit=(& git rev-parse HEAD); created_utc=[DateTime]::UtcNow.ToString('o'); files=$files
        clean_path_startup_verified=$true; dirty_source=$dirty
        native_window_gate='Not exercised by --devices. Require Verify-Gui.ps1 evidence for this exact executable.'
    } | ConvertTo-Json -Depth 5 | Set-Content "$OutputPath\build-manifest.json" -Encoding utf8
    Write-Output "Portable native app: $OutputPath\SteadyCapture.exe"
    Write-Output 'All optional sources start OFF. No installer, sign-in or background service.'
    Write-Output 'Actual native-window verification is a separate required, explicitly approved gate.'
} finally {
    Pop-Location
}
