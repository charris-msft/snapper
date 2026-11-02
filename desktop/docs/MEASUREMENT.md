# Native recording measurements

Use the exact optimized versioned package, not an older executable or a debug
build. Every report records the running executable's SHA-256 and package
manifest. Generated media, GPU-device construction, `--devices` and renderer
framebuffer tests are not proof of actual Windows Graphics Capture recording.

## Approval boundaries

`--native-profile` is an explicit screen-capture operation. It captures the
specified fixed screen rectangle twice, once without the test CPU peer and
once with a bounded competing load. Obtain approval for the target, duration
and output location before running it. The rectangle contains whatever is
visible there, not isolated window content.

The harness refuses webcam, microphone and system-audio activation. Those need
separately approved live-input and A/V calibration work. It also refuses
unoptimized builds, synthetic requests, fault injection and durations outside
12-120 seconds per pass. Do not treat its approval flag as a substitute for
permission.

## Explicit request

Prepare a media-request JSON for the approved target. This is a schema example,
not a ready-to-run target; replace the monitor placeholder using current
`--devices` output and use the explicitly approved physical-pixel rectangle:

```json
{
  "schema": 1,
  "library_root": "C:\\Temp\\Snapper-profile",
  "session_dir": "C:\\Temp\\Snapper-profile\\sessions\\native-plan",
  "area": {
    "monitor": "<approved monitor ID>",
    "x": 0, "y": 0, "width": 1920, "height": 1080
  },
  "sources": {
    "cursor": false, "webcam": false,
    "system_audio": false, "microphone": false
  },
  "camera_id": null,
  "microphone_id": null,
  "output_id": null,
  "pip": null,
  "video": true,
  "max_seconds": 30,
  "synthetic": false,
  "test_fault": null
}
```

After approval, invoke the packaged executable:

```powershell
.\snapper-desktop.exe --native-profile <request.json> <evidence-directory> --approve-screen-capture
```

Each pass uses a fresh evidence library/session rather than overwriting the
request's storage. No output is uploaded.

## Measurements and interpretation

The report includes a two-second idle CPU baseline, recording CPU for the
parent and complete worker/encoder/helper Job family, aggregate system CPU,
source and output dimensions, counted decoded frames, average output FPS,
skipped/repeated frames, output bytes/bitrate and source backend.

Stop is explicitly requested. Monotonic media-origin timestamps measure Stop
to confirmed device release; wall time measures Stop to Saved and the remaining
finalization interval. Process-family CPU counters remain parent-owned so a
worker heartbeat cannot reset them.

Memory covers the parent process's sampled private-byte peak and the whole
worker Job's peak commitment. Their sum is a conservative upper bound because
the two peaks need not coincide. Driver/kernel GPU allocations are outside
that measure. Parent sampling is every 25 ms.

The competing load is one thread with 5 ms work / 5 ms sleep, with its own CPU
measurement reported separately. It is not host-wide saturation. Root-process
CPU during that case includes the test peer and is labeled accordingly.
The harness measures the actual native media backend through the same
supervisor/worker/encoder as the application, but does not open the editor, so
GUI-rendering overhead is not included.

Results are measurements, not an automatic universal efficiency verdict.
Compare source/output resolution, frame delivery and resource use on the
intended Windows/RDP host. A repeated frame may be legitimate when screen
content is static. A/V synchronization with real inputs is not measured when
audio sources are OFF. Same-build generated A/V tests remain a separate check.

## Encoder availability

```powershell
.\snapper-desktop.exe --encoder-diagnostics <evidence-directory>
```

This bounded check feeds 30 generated 1280x720 frames to the pinned FFmpeg's
libx264, QSV, NVENC and AMF paths. It never opens a screen, camera or audio source.
Named-encoder success does not by itself prove hardware execution, native
capture performance, or segmented-recovery compatibility. Failure diagnostics
are retained. It does not change the production encoder.

The current recording path is explicitly **libx264 software H.264**. A new
encoder or fallback must independently satisfy timestamp, bounded-queue,
segment-finalization, recovery, provenance and local packaging gates before
being selected.
