# Dependency provenance and API references

The bootstrap uses HTTPS with platform TLS verification and fixed SHA-256 values.
No machine PATH, execution policy, certificate checking, device-consent control
or security setting is changed.

| Component | Pin | Source |
|---|---|---|
| Rust | 1.99.1, x86_64-pc-windows-gnullvm | https://static.rust-lang.org/dist/2026-10-15/ |
| rustup | 1.29.2 | https://static.rust-lang.org/rustup/archive/1.29.2/x86_64-pc-windows-msvc/rustup-init.exe |
| LLVM-MinGW | 20261020 UCRT x86_64 | https://github.com/mstorsjo/llvm-mingw/releases/tag/20261020 |
| FFmpeg/ffprobe | Gyan 9.0.2 essentials | https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip |
| eframe/egui | 0.31.1 | https://docs.rs/eframe/0.31.1/eframe/ |
| wgpu | 24.0.5, explicit Windows dx12+wgsl features | https://docs.rs/wgpu/24.0.5/wgpu/ |
| Windows Rust bindings | 0.58.0 | https://github.com/microsoft/windows-rs |

Direct Rust dependencies are exact-pinned in Cargo.toml; Cargo.lock pins the
resolved graph and checksums. The package includes dependency license metadata
and available top-level license/notice files. Bundled egui default fonts have
their upstream licenses in the corresponding crate's notices.
eframe 0.31.1 does not enable wgpu's DX12 feature by itself when its defaults are
disabled. The Windows dependency explicitly enables it; a regression test
checks the compiled backend set, not just the requested instance configuration.

FFmpeg is a **separate GPLv3 executable**, not linked into the Rust application.
Its upstream GPL license and build README accompany the portable app. The Gyan
build page identifies the release source at
https://github.com/FFmpeg/FFmpeg/commit/946fcce07b and describes its GPLv3/static
build configuration. Further distribution requires honoring the corresponding
source and other license obligations; local packaging is not external
publication authorization. The independently written application source is MIT.

Runtime SHA-256 checks reject a changed encoder or probe executable. Verified
files remain open with read-only sharing for that process lifetime, preventing
replacement after the check.

| Binary | SHA-256 |
|---|---|
| ffmpeg.exe | 3256173f3f8bffd7df12227c68adf68025edb1832273a9530688a7bb1ed8edec |
| ffprobe.exe | f0d36ecbbdd3bcfac3efa078c96c7271c2e68b3810595552ac3b7f17e9a65c52 |

## Verified contracts used in the implementation

- [Windows Graphics Capture](https://learn.microsoft.com/windows/apps/develop/media-authoring-processing/screen-capture):
  compositor-frame ownership, valid content size, QPC frame timestamps and HDR
  considerations.
- [Free-threaded capture pool](https://learn.microsoft.com/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded):
  callbacks without a UI dispatcher.
- [Cursor capture control](https://learn.microsoft.com/uwp/api/windows.graphics.capture.graphicscapturesession.iscursorcaptureenabled):
  independent cursor behavior requires Windows 10 version 2004 or later.
- [WASAPI loopback](https://learn.microsoft.com/windows/win32/coreaudio/loopback-recording):
  shared-mode output-endpoint capture, without requiring Stereo Mix.
- [Audio packet timestamps and flags](https://learn.microsoft.com/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer):
  QPC converted to 100 ns, same-thread acquire/release, silent/discontinuity/time
  error handling.
- [Media Foundation capture](https://learn.microsoft.com/windows/win32/medfound/audio-video-capture-in-media-foundation)
  and [Source Reader](https://learn.microsoft.com/windows/win32/medfound/source-reader):
  video device activation and sample retrieval, not automatic A/V synchronization.
- [Windows Job Objects](https://learn.microsoft.com/windows/win32/procthread/job-objects):
  child process lifetime ownership.
- [egui wgpu setup](https://docs.rs/egui-wgpu/0.31.1/egui_wgpu/struct.WgpuSetupCreateNew.html):
  explicit backend/adapter/surface compatibility configuration.
- [wgpu DX12 compiler options](https://docs.rs/wgpu-types/24.0.0/wgpu_types/enum.Dx12Compiler.html):
  FXC requires no additional compiler DLLs in the application package.
- [FFmpeg segment muxer](https://ffmpeg.org/ffmpeg-formats.html#segment_002c-stream_005fsegment_002c-ssegment):
  segmentation/keyframe alignment and timestamp-reset constraints.
- [FFmpeg audio resampling](https://ffmpeg.org/ffmpeg-resampler.html):
  timestamp compensation and asynchronous resampling.

Public editor descriptions informed behavior only, not implementation or assets:
[Fill](https://www.techsmith.com/learn/tutorials/snagit/fill/),
[Color Replacement](https://www.techsmith.com/learn/tutorials/snagit/effects/#color-replacement),
[Quick Styles/Favorites](https://www.techsmith.com/learn/tutorials/snagit/quick-styles-and-favorites/),
[Cut Out](https://www.techsmith.com/learn/tutorials/snagit/cut-out/),
[Eraser](https://www.techsmith.com/learn/tutorials/snagit/eraser/), and
[Blur](https://www.techsmith.com/learn/tutorials/snagit/blur/).
