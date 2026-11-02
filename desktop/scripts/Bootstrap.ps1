[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\Environment.ps1"
New-Item -ItemType Directory -Force $Tools | Out-Null

function Get-VerifiedDownload([string]$Url, [string]$Path, [string]$Sha256) {
    if (-not (Test-Path -LiteralPath $Path)) {
        Invoke-WebRequest -Uri $Url -OutFile "$Path.partial"
        $actual = (Get-FileHash -LiteralPath "$Path.partial" -Algorithm SHA256).Hash
        if ($actual -ne $Sha256) { throw "SHA-256 mismatch for $Url. Download retained, not executed." }
        Move-Item -LiteralPath "$Path.partial" -Destination $Path
    }
    if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $Sha256) {
        throw "SHA-256 mismatch for $Path. Not executed."
    }
}

$rustup = Join-Path $Tools 'rustup-init-1.29.2.exe'
Get-VerifiedDownload `
    'https://static.rust-lang.org/rustup/archive/1.29.2/x86_64-pc-windows-msvc/rustup-init.exe' `
    $rustup '00f139da614dced87d3c3d4121c43eebdac3c1bf8a5ee86b9d80dfd71c8d70fd'
if (-not (Test-Path "$env:CARGO_HOME\bin\rustup.exe")) {
    & $rustup -y --no-modify-path --default-host x86_64-pc-windows-gnullvm `
        --default-toolchain 1.99.1 --profile minimal --component rustfmt,clippy
    if ($LASTEXITCODE -ne 0) { throw "Rust toolchain installation failed ($LASTEXITCODE)." }
}

$llvm = Join-Path $Tools 'llvm-mingw-20261020-ucrt-x86_64.zip'
Get-VerifiedDownload `
    'https://github.com/mstorsjo/llvm-mingw/releases/download/20261020/llvm-mingw-20261020-ucrt-x86_64.zip' `
    $llvm 'fd5462b7f5d93b2ff981fcd3f9c7e427e47f3203c64ea91c27bcc444eebd74cd'
if (-not (Test-Path "$Tools\llvm-mingw-20261020-ucrt-x86_64\bin\clang.exe")) {
    Expand-Archive -LiteralPath $llvm -DestinationPath $Tools
}

$ffmpeg = Join-Path $Tools 'ffmpeg-9.0.2-essentials_build.zip'
Get-VerifiedDownload `
    'https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip' `
    $ffmpeg '60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba'
if (-not (Test-Path $env:STEADYCAPTURE_FFMPEG)) {
    Expand-Archive -LiteralPath $ffmpeg -DestinationPath $Tools
}
& rustc --version
& cargo --version
& $env:STEADYCAPTURE_FFMPEG -hide_banner -version | Select-Object -First 1
Write-Output "User-local build tools ready at $Tools. No machine PATH or security settings changed."
