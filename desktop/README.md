# Snapper Desktop

A local Windows screenshot, screen-recording and image-annotation application.
The desktop UI and device supervision are Rust. There is no browser server,
account, telemetry, upload path or background recording service.

**Acceptance testing is in progress.** Compiling, unit tests and generated-media
tests do not establish camera/microphone behavior on untested hardware or
immunity to arbitrary host load. See the acceptance evidence delivered with a
tested build before treating a feature or operating envelope as verified.

## Run

Open `snapper-desktop.exe` in the versioned local `dist\Snapper-<version>-<commit>` bundle after packaging. Keep its `tools` and
`licenses` folders beside it. Windows 10 version 2004 or later, SDR display output,
and a compatible Direct3D 12 adapter or Windows WARP software renderer are required. No administrator access is
needed. This is an unsigned local development build; do not bypass an operating
system security warning.

New installations start with all optional sources **OFF**. Saved choices never
open hardware at launch. Choose Screenshot or Recording in the compact launcher,
select a region visually (or use Full monitor/Window guide), then review sources.
Drag inside a selection to move it, use its handles to resize, and press Enter
or Review sources to continue. **Escape cancels selection, review or countdown**,
including an unfinished drag, without saving a capture or changing the clipboard.
Window guide creates a fixed screen region, not isolated window-content capture.

Preflight identifies the chosen camera, microphone and output route. Camera
preview is a separate, explicit activation in a supervised process. Move/resize
its picture-in-picture placement before capture. Only **Capture** or **Record**
arms the configurable countdown. Screenshots never open audio sources.
Recording uses a small movable control dock; the editor stays out of the way.
Pause/Resume compress the same media timeline for video and both audio sources.
Pausing keeps selected devices open and discards paused content; the dock
states that explicitly. Stop remains available while paused or while a
transition is pending.

The image editor includes editable arrows, text, callouts, rectangles, ellipses,
lines, original built-in stamps, connected Fill, selection/global Color
Replacement and simultaneous color swap, blur, pixelation, alpha erasing,
horizontal/vertical/jagged cutouts, grayscale, borders, undo/redo, Quick Styles,
Favorites, tags, recent captures, PNG/JPEG export and bitmap clipboard copy.
Blur and pixelation are **not secure redaction**.

The focused video editor preserves the source and recovery segments. Trim,
split and remove sections on an explicit 30 fps timeline, update the accurately
re-encoded preview, then scrub frames or play that same MP4 in the Windows
player. Save uses the verified preview bytes; a selected clip renders its own
sub-plan. Progress and cancellation remain available through supervised codec
work. This is not a multitrack editor or an in-app audio playback engine.

Read [the user guide](docs/USER_GUIDE.md) for controls, storage, recovery and
limitations; [the architecture](docs/ARCHITECTURE.md) for resource and lifetime
contracts; and [dependency provenance](docs/DEPENDENCIES.md) for licenses and
upstream references.
The [measurement guide](docs/MEASUREMENT.md) separates consent-gated native
recording metrics, generated A/V acceptance and encoder-availability probes.

## Build and verify

The bootstrap installs only checksum-pinned user-local tools. It does not change
machine PATH, execution policy, TLS settings, device permissions or priorities.

```powershell
.\scripts\Bootstrap.ps1
. .\scripts\Environment.ps1
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
.\scripts\Package.ps1
.\scripts\Verify.ps1 -PackagePath <versioned-package>
```

`--self-test` uses generated video, cursor and camera markers plus generated
440 Hz/880 Hz audio. It does **not** open real microphones, cameras or screen
capture. It tests all 16 source-flag combinations, decoded pixels, audio content,
stream presence, duration, and preservation of a missing-video-frame interval.
The independent editor fixture exercises nine operations through the real
renderer, undo/redo, editable save/reopen, PNG export and immutable-original
checks. Live device and GUI acceptance are separate.

`--fault-test <directory>` injects generated-media-only failures, verifies
playable recovery and confirms helper cleanup after parent death.
`--pause-test <directory>` checks decoded pause/resume duration and A/V markers,
Stop while paused, Stop during a stalled transition, generated-camera preview
release and adjustable camera placement. These tests use no real private inputs.
`--video-edit-test <directory>` checks frame-accurate one-frame cuts, delayed and
gapped audio, exact preview/export identity, cancellation, and immutable source
and recovery segments. Use `scripts\Verify.ps1 -Suite Video` against the package.
Use the optimized packaged candidate for media acceptance; debug codec-helper
startup and hash checking are not a performance baseline.
Run diagnostics through `scripts\Verify.ps1` (`-Suite Profile`, `Encoders` or
`Native` as appropriate). It waits for the GUI-subsystem executable and retains
its output; a bare PowerShell invocation is not a completion check.
`--profile-test <directory> 600` checks an optimized 1080p generated recording
with beginning/middle/end A/V markers over ten minutes. Its modest bounded CPU
peer and injected video gaps do not establish arbitrary-load immunity.

Release packaging requires a committed, clean source tree and records the
commit, executable/runtime/codec hashes and clean-PATH startup proof. Use a
versioned output directory while comparing candidates. The acceptance reports
record their actual executable SHA-256, not just a source commit.
CLI enumeration is not a GUI startup test. After approving the exact local
candidate, run `scripts\Verify-Gui.ps1 -PackagePath <package> -EvidencePath <evidence>
-ApproveWindowTest` to open a nonfocused native editor window with generated
content, verify its rendered framebuffer, and save the actual adapter/hash
report plus a PNG. It does not activate any screen, camera or audio capture.

`--devices` enumerates displays/cameras without opening a capture stream.
`--encoder-diagnostics <directory>` probes named encoders using generated
frames, without changing the production software encoder.
`--native-profile <request.json> <directory> --approve-screen-capture` is an
explicitly approved real-screen measurement, not an unattended setup step.
`--data-dir <absolute directory>` selects a separate local library.
`--recover <session directory>` validates and exports an interrupted session.

This desktop integration is local-only. The parent Snapper repository is public;
no public push, release or upload is implied or authorized by these scripts.
The existing Go `snapper.exe` remains a separate lightweight CLI with unchanged
default capture/JSON behavior. Never run the root `build.ps1` as a desktop build:
it replaces the installed CLI. See [import provenance](docs/IMPORT.md).

This application contains no Snagit implementation, branding or proprietary assets.
