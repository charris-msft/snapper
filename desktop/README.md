# SteadyCapture

A local Windows screenshot, screen-recording and image-annotation application.
The desktop UI and device supervision are Rust. There is no browser server,
account, telemetry, upload path or background recording service.

**Acceptance testing is in progress.** Compiling, unit tests and generated-media
tests do not establish camera/microphone behavior on untested hardware or
immunity to arbitrary host load. See the acceptance evidence delivered with a
tested build before treating a feature or operating envelope as verified.

## Run

Open `dist\SteadyCapture\SteadyCapture.exe` after packaging. Keep its `tools` and
`licenses` folders beside it. Windows 10 version 2004 or later, SDR display output,
and a compatible Direct3D 12 adapter or Windows WARP software renderer are required. No administrator access is
needed. This is an unsigned local development build; do not bypass an operating
system security warning.

All optional sources start **OFF**. Select a monitor/region, enable only the
sources wanted, then click **Take screenshot** or **Record screen**. A visible
three-second countdown precedes activation. Audio is never opened for a
screenshot. Screen, webcam and audio devices run in a supervised subprocess;
the editor can keep working while recording.

The image editor includes editable arrows, text, callouts, rectangles, ellipses,
lines, original built-in stamps, connected Fill, selection/global Color
Replacement and simultaneous color swap, blur, pixelation, alpha erasing,
horizontal/vertical/jagged cutouts, grayscale, borders, undo/redo, Quick Styles,
Favorites, tags, recent captures, PNG/JPEG export and bitmap clipboard copy.
Blur and pixelation are **not secure redaction**.

Read [the user guide](docs/USER_GUIDE.md) for controls, storage, recovery and
limitations; [the architecture](docs/ARCHITECTURE.md) for resource and lifetime
contracts; and [dependency provenance](docs/DEPENDENCIES.md) for licenses and
upstream references.

## Build and verify

The bootstrap installs only checksum-pinned user-local tools. It does not change
machine PATH, execution policy, TLS settings, device permissions or priorities.

```powershell
.\scripts\Bootstrap.ps1
. .\scripts\Environment.ps1
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
.\scripts\Package.ps1
.\scripts\Verify.ps1
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
`--data-dir <absolute directory>` selects a separate local library.
`--recover <session directory>` validates and exports an interrupted session.

This repository is local-only. No remote or external publishing is configured.
It contains no Snagit implementation, branding or proprietary assets.
