$ErrorActionPreference = 'Stop'
$Tools = if ($env:SNAPPER_TOOLCHAIN_ROOT) { $env:SNAPPER_TOOLCHAIN_ROOT } else {
    # Reuse the already approved, checksum-pinned user-local toolchain.
    Join-Path $env:LOCALAPPDATA 'SteadyCapture\Toolchain'
}
$env:CARGO_HOME = Join-Path $Tools 'cargo'
$env:RUSTUP_HOME = Join-Path $Tools 'rustup'
$env:PATH = "$env:CARGO_HOME\bin;$Tools\llvm-mingw-20261020-ucrt-x86_64\bin;$env:PATH"
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER = 'x86_64-w64-mingw32-clang.exe'
$env:CC = 'x86_64-w64-mingw32-clang.exe'
$env:CXX = 'x86_64-w64-mingw32-clang++.exe'
$env:STEADYCAPTURE_FFMPEG = Join-Path $Tools 'ffmpeg-9.0.2-essentials_build\bin\ffmpeg.exe'
$env:STEADYCAPTURE_FFPROBE = Join-Path $Tools 'ffmpeg-9.0.2-essentials_build\bin\ffprobe.exe'
$env:SNAPPER_FFMPEG = $env:STEADYCAPTURE_FFMPEG
$env:SNAPPER_FFPROBE = $env:STEADYCAPTURE_FFPROBE
