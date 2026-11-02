# Architecture and failure contracts

## Boundaries

`editor` is a native Rust egui/eframe application using wgpu/Direct3D 12
and AccessKit. It is not a browser application. Compatible hardware adapters
are preferred; a compatible CPU/WARP adapter is the explicit last resort.
OpenGL is not required or attempted, including in an RDP session. Windows FXC
is used without downloading extra DXC DLLs. The active adapter and software
fallback are reported in the UI. Image rendering/persistence and
library/export tasks execute away from the GUI event loop.

`capture` owns physical-pixel selection geometry, preferences, generation tokens
and a per-flow atomic admission gate. `platform::selection` owns the native
overlay, mouse capture and focus restoration. Unexpected capture loss cancels
the gesture; ordinary release after mouse-up preserves it. Scoped Escape and
final start admission use competing CAS operations on the same gate. A
cancellation that wins cannot call the recording factory. If start wins, a
concurrent signal sets that generation's unique Stop flag. Old registrations
cannot stop a newly opened capture generation. GUI-frame polling is not the
admission boundary.

`editor::capture_ui` provides a compact launcher, source preflight and movable
recording dock rather than embedding capture controls in the image workspace.
Named WASAPI endpoints are validated before activation; no silent substitution
occurs if a selected endpoint disappears. `media::preview` opens a camera only
after an explicit Preview action, in a separately gated, Job-owned subprocess.
Its latest-frame transport is bounded to 640x360; cancellation confirms process
exit before a waiting recording countdown can proceed.

`media::supervisor` owns a kill-on-close Windows Job Object and the media child.
The child reads a mandatory start gate before device activation or child-process
creation. The supervisor assigns the Job before releasing that gate. Parent
exit before the gate produces EOF; parent exit afterwards closes the Job.
The encoder inherits Job membership. This implements the ownership-before-
activation property without an unsupervised activation window.
Validation, thumbnail and recovery/remux helpers use a separate gated helper
with its own Job Object. Closing the UI does not leave an unsupervised codec
helper alive. Intentionally opened Explorer/video-player windows are not put in
these jobs. A parent-death acceptance case waits for the owned helper tree to
exit before the test's outer cleanup job is released.

`platform::screen` uses Windows Graphics Capture with a free-threaded two-frame
pool. Its callback sends a bounded notification, not a borrowed texture. One
owner thread copies the valid requested rectangle into an owned staging texture,
maps/copies it into CPU memory, unmaps it and closes the WGC frame. No UI lock
or borrowed pool surface crosses into encoding. Resolution changes interrupt
the fixed-canvas recording. A closed-target event interrupts capture. Pending
notifications drain the two-frame pool to the latest frame; a successful null
WinRT frame result is distinguished from a device error. Every executable mode
establishes per-monitor-v2 physical-pixel coordinates. For virtual/RDP outputs
without a physical DXGI color descriptor, WGC uses Float16 surfaces followed by
explicit linear-to-sRGB SDR conversion; values above SDR white are clipped.
Known physical HDR outputs remain unsupported rather than being silently
captured with the 8-bit SDR path.

`platform::camera` uses Media Foundation enumeration, selected-device activation
and a Source Reader. `platform::audio` uses shared-mode WASAPI capture:
eRender plus LOOPBACK for system audio, eCapture for the microphone. Each owns
its COM apartment and all acquire/release operations stay on that thread.
OFF sources never reach device activation. Source enumeration is separate.

## Time and bounded media

One QPC-based origin starts after selected source readiness and encoder
initialization. WGC and WASAPI timestamps use 100 ns units. Camera sample time
has its own epoch: a bounded mapping associates it with the monotonic clock and
rejects backward/excessively divergent time. Arrival latency still requires
hardware calibration; matching units do not prove optical/audio synchronization.

A 30 Hz media-time schedule selects owned screen/camera frames. Missed schedule
ticks increment a counter without changing subsequent timestamps. A retained
screen frame can be repeated; a selected camera that stops advancing interrupts
capture instead of silently freezing the camera overlay.

The encoder receives a timestamp-bearing Matroska stream containing BGRA frames
and selected 48 kHz stereo PCM tracks. It does not use a rawvideo frame-count
clock. Per-source timestamp-aware FFmpeg resampling feeds a normalized audio
mix. With both sources selected, the normalized mix provides headroom rather
than simply adding two full-level tracks.

Visual source queues are latest-value mailboxes. The main compositor holds at
most one reference from each, plus one output canvas. WGC owns two pool textures
and one staging texture. Those buffers are distinct from the pending-frame
count. Native screenshot pixels are bounded by the image-size limit. The PCM
format uses 192,000 bytes per second per source. The queue budget is two seconds per selected
source, at most 768,000 bytes and 512 messages. Queue exhaustion interrupts
rather than growing memory or silently dropping audio. The quiet-loopback
silence policy retains 250 ms of uncommitted time and rejects real audio more
than 20 ms late relative to committed samples. Microphone startup requires a
first packet within one second; subsequent missing delivery has a one-second
watchdog. Timestamped native SILENT packets remain legitimate audio. Device
sample positions and QPC-derived sample positions are checked before converting
only the transport timestamp to milliseconds. Native sources drain pre-origin
samples while waiting at the common start barrier.

Pause/resume boundaries and media admission have one writer-thread owner.
The stdin thread queues a sequenced desired state; it never acknowledges a
pause while a previous pipe write is still in flight. Acknowledgements carry
the exact wall and media sample positions. The UI distinguishes Pausing,
Paused and Resuming, and delayed heartbeats cannot falsely acknowledge them.

Audio producers queue timestamped observations and ordered drain watermarks,
not output packets. The writer validates each native packet once, retains its
original wall-sample range and holds look-ahead until the wall interval is
sealed. Both audio sources and video use the same bounded pause history.
Only active spans reach the encoder; fully paused late packets do not conflict
with silence commitment. Gap/overlap budgets apply to retained active samples.
Pending PCM stays charged to the queue budget until written or discarded.
Prepared output spans are bounded to 100 ms. Stop freezes an immutable media
endpoint before device joins and final draining; it never waits for Resume.
Duration limits measure active media time, not paused wall time.

FFmpeg uses two encoder threads, one complex-filter thread, a two-packet input
queue, no B frames, a finite muxing queue and a one-second mux-interleave bound.
Third-party encoder internals and graphics-driver allocations are not claimed
to have a hard process-memory ceiling.

## Stop and recovery

The UI acknowledges Stop locally and sets an atomic signal. The supervisor sends
one small control message over a pipe that never carries frame payload. The
worker reads it on a separate thread. Heartbeats also run separately from the
media writer. Device workers do not own UI locks.
The global Ctrl+Shift+F9 handler has its own message-loop thread and signals
Stop without going through the editor frame. Parent status merging cannot
replace an acknowledged Stopping state with an old Recording heartbeat: the
Stop flag is read and the heartbeat is merged while holding the same progress
publication mutex used by Stop acknowledgement. A pre-lock snapshot is never
used to publish the next state. Worker exit updates counters, folds Stop and
persists `last-progress.json` within one critical section. Its persistence
snapshot cannot write an older Recording state over an acknowledged Stop.

The normal shutdown path releases devices before finalization. A device that
does not release within the worker's 2.5-second wait causes worker exit. The
external supervisor requests Job termination after a 5-second uncompleted Stop,
20-second startup, or 180-second finalization limit, then checks process exit.
The finalization deadline begins at DevicesStopped, before final audio drain;
that stage also has an explicit Cancel finalization action. Failed device
shutdown aborts the device-owning worker instead of relabeling it Finalizing.
These are enforcement policies, not promises that a starved OS/driver releases
hardware by a hard wall-clock deadline. UI acknowledgement and process/device
exit must be measured independently.
Pause/resume acknowledgement and in-flight media writes also have separate
five-second wall-time watchdogs. A paused or frozen media clock cannot disable
those deadlines. Selected devices continue draining during Pause; their state
is explicit in the dock.

The segmenter writes keyframe-aligned, independently finalized, approximately
two-second MP4 segments. A newline-complete CSV row is not a producer-close
signal: the pinned segmenter can publish it before closing its output handle.
The publisher first acquires a read/write handle that permits other readers but
excludes writers/deletion, retrying only Windows sharing/lock violations for up
to three seconds. Real open/write/trailer errors are not ignored. The CSV is not
a durability journal. Publication then performs:

1. Fully decode the completed segment's video and audio.
2. Flush its file data.
3. Rename it to its committed name on the same volume.
4. Atomically replace the journal after the file is published.

Recovery reconciles the segment completion list with published files and fully
decodes retained segments. Uncommitted tails are not declared playable. Normal
export remuxes the committed segments, checks joined-stream boundaries, flushes
and publishes the final MP4, then publishes library metadata. Originals and
previous completed media are never deleted to recover an interrupted session.
An already-published final MP4 is validated and adopted after a metadata
interruption only if it matches the durable final-publication identity.
Before final rename, the flushed journal records the final SHA-256/byte length,
session ID, a binding to the source selection and committed segment plan, and
the inspected stream shape/timing. Final publication never replaces a competing
destination. Adoption checks the fingerprint, MP4/H.264 dimensions, duration
against the segment timeline (125 ms mux-padding tolerance), and exactly the
expected zero-or-one AAC 48 kHz stereo audio stream. It holds a regular-file
read handle that excludes writes/deletion from validation through completion
metadata publication. An unrelated or older unbound final is preserved and
rejected, not silently adopted; its committed segments remain available.
New recording journals use schema 2 so older readers do not mistake the new
publication protocol for their unbound-final behavior. Legacy schema-1
segments can still be recovered into a new bound publication when no final
file occupies the destination.
This is a local publication/recovery identity, not authentication against a
party able to rewrite both the journal and all media.
Library metadata precedes thumbnail generation, so a failed
derivative does not make the saved video inaccessible. Orphan committed
segments can be reconciled using their validated duration. Remux placement
uses the next segment's start time, not a sum that would remove inter-segment
timestamp gaps. Malformed journals become individual warnings, not a failed
listing of all captures.

## Editor

`model` stores versioned typed operations and reusable styles. History snapshots
contain operation metadata, not copies of the original raster.
`render` applies vector and raster operations in order, preserving the distinction
between connected Fill and selection/global Color Replacement. Cutouts transform
subsequent canvas dimensions. Eraser and transparent fill operate on alpha.
`library` bounds decode dimensions/allocations, validates metadata, and writes
new data to a same-volume temporary file followed by flush and rename.

The render worker coalesces pending requests and cancels superseded active work
at layer/row/tile checkpoints. Gaussian
blur uses bounded strips plus a kernel halo; pixelation uses explicit block
averages. Preview/thumbnail resizing also checks supersession and the shared
30-second cooperative preparation deadline at each output row. Document and
capture metadata plus the encoded thumbnail are staged and flushed off the
UI thread. The authoritative renames occur only after a generation check under
the same short commit gate used to accept a newer generation. There is no
rendering, resizing, encoding, data write or flush under that gate. An edit
superseded before this commit point cannot replace the last published document,
even if the next render fails. Unpublished staging files are cleaned up.
The deadline covers cooperative render/derivative preparation checkpoints, not
a hard wall-clock guarantee for image import/decoding, OS file I/O, or export.
Switching images waits for the
current generation to save so an overwritten mailbox cannot lose another
capture's pending edits. Preview textures are capped at 2048 pixels per side;
flattened export uses the canonical full-size render. Library thumbnail caching
is capped at 24 captures and uses a separate 240 x 150 / 1 MiB decode admission
limit. Both disk reload and completed-render insertion use thumbnail-sized
images. The thumbnail cache cannot accept a preview texture handle, and rejects
oversized pixel inputs before allocating a texture; at most 3,456,000 RGBA texel
bytes are retained by its 24 entries, excluding graphics-backend overhead and
the one current editor preview. The checkerboard iterates only cells intersecting the visible viewport,
including at 800% zoom. Interrupted image imports persist an intent before the
original; Library refresh can adopt a complete original and reconcile missing
metadata while preserving existing edits. Concurrent UI owners and recovery of
an active session are rejected using local exclusive file locks.

## Proof boundaries

`--devices` and `--graphics-diagnostics` do not establish native-window startup.
The separately approved `--gui-smoke-test` launches the actual editor against
a fresh generated-image library and receives its wgpu framebuffer. The exact
main `painter.image` call records the draw/clip rectangles, viewport,
pixels-per-point, texture dimensions, capture identity and saved generation.
This record is reset on every Editor update, so Library or a missing main
image cannot reuse a previous canvas draw. The screenshot request freezes that
geometry and uses a matched request token; a layout/DPI/revision change while
readback is pending fails the check.

The oracle requires the full 640 x 360 fixture on a visible, non-thumbnail-sized
main canvas and checks 432 interior samples across all 48 blue/green tiles,
including four asymmetric corner markers, with an 8/255 per-channel tolerance.
Only framebuffer coordinates derived from the main draw rectangle count;
Recent thumbnails and UI chrome are excluded. Test-only input filtering blocks
fixture editing, imports and page navigation, while preserving screenshot and
OS window-close/resize events. CPU-only negative controls prove that a missing,
uniform or rotated main fixture fails even when a valid Recent thumbnail
satisfies the obsolete whole-frame color counts. The report records this
main-canvas proof, actual adapter and executable hash. No screen/camera/audio
capture API is used, and no passing unit oracle is represented as a real GUI
launch. A new unsigned candidate is not silently launched by packaging.

Unit/editor-oracle tests establish operation, persistence, budget and timing
properties. The generated-media suite establishes actual encoder/decode behavior
for all 16 source combinations without personal device capture. It does not
establish real WASAPI, camera optics, microphone permissions, WGC composition,
mixed-DPI behavior, GUI responsiveness, device unplugging or load-envelope
claims. Those require separate live acceptance and must be reported separately.
An optimized generated-media profile checks time-localized screen, camera,
system-tone and microphone-tone pulses near the beginning, middle and end of
recordings up to ten minutes. Its competing load is one bounded CPU thread
(5 ms work / 5 ms sleep) plus 180 ms simulated video-delivery gaps every five
seconds. It records actual peer CPU time, frame skips and queue/Job memory
counters. This is not a host-wide heavy-load or live-device calibration claim.
