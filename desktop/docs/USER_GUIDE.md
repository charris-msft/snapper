# Snapper desktop user guide

## Capture

1. In the compact launcher, choose **Screenshot** or **Recording**, then
   **Region**, **Full monitor** or **Window guide** and the monitor.
   **Select area** opens a native overlay. Drag a rectangle, move inside it,
   or resize with its eight handles. The dimensions are physical pixels.
   Enter or **Review sources** continues. Arrow keys move one pixel, Shift
   moves ten, and Ctrl plus an arrow resizes. Precision fields are also
   available in preflight.
2. **Esc to cancel** stays visible throughout selection, adjustment, source
   review and countdown. It cancels the entire operation, including an
   unfinished mouse drag, releases input ownership and returns prior focus.
   Late mouse-up/timer events cannot restart a cancelled selection. Escape is
   registered only for the active flow, not globally while idle. At the exact
   start-admission boundary, an already-admitted Escape becomes Stop for that
   recording instead of being lost.
3. Review the four independent source choices. **Refresh devices** lists
   cameras, microphones and output routes without opening streams. Choose
   the named devices you want. System audio includes the entire mix on the
   chosen output, including other applications. **Preview camera (turns camera
   on)** is a separate explicit action. Drag its placement tile and adjust its
   width; stop the preview or start capture to release preview ownership.
4. Choose **Capture** or **Record** to arm the configurable 0-10 second
   countdown. A camera preview is released before countdown. No recording
   source starts during selection/review/countdown. During recording, a small
   movable dock provides **Pause**, **Resume** and **Stop**. Its elapsed time
   excludes pauses. Selected devices remain on during Pause, but paused screen
   pixels and audio samples are not admitted to the recording.
   Ctrl+Shift+F9 stops from any application without waiting for editor painting.
   Settings can change this shortcut. Conflicts are reported, never stolen.
5. Wait for **Saved**. Stopping, devices released, finalizing and saved are
   different states. A stop request alone does not mean the file is finished.
   **Cancel finalization** ends a blocked export while keeping committed
   segments available in Library for recovery.

Screenshots retain the selected native pixel dimensions. Recording preserves
aspect ratio and fits the region within 1920 x 1080, with even dimensions and a
30 Hz target schedule. Preflight shows both the selected source resolution and
the actual output resolution; scaling is explicit. Encoding currently uses
software H.264/libx264, not an asserted hardware encoder. Camera picture-in-picture
position and width are adjustable before capture.
Camera input is bounded to a negotiated mode at or below 1280 x 720 and 30 fps.
Recordings stop at the selected 1-120 minute active-media limit. They also stop on explicit
device/timing/queue failures or low free space, rather than silently dropping a
selected source. Protected content, secure desktops, unsupported graphics
drivers and known physical HDR displays are not supported capture targets.
Virtual/RDP displays without a physical DXGI color descriptor use Float16 WGC
capture and explicit linear-to-sRGB SDR conversion. Bright values above SDR
white are clipped; this is not an HDR-preserving export. The active path is
displayed while recording. Monitor coordinates are physical pixels, not
DPI-scaled browser/window coordinates.

Window guide uses a window's bounds to select a **fixed screen region**. It does
not isolate window content, follow a moved window or hide overlapping windows.
A selected region must fit one monitor. Snapper does not promise cross-monitor
video. The dock is excluded through Windows display affinity; recording does
not start if that exclusion cannot be established.

## Repeat captures and preferences

The default capture and repeat-region shortcuts are Ctrl+Shift+F8 and
Ctrl+Shift+F7. Repeat returns to source review, not directly into recording.
The last saved region is revalidated against monitor identity, physical
position, resolution and DPI. A changed or disconnected display requires a
fresh selection.

Settings stores up to 20 named capture presets, independent source choices,
device IDs, camera layout, destination, shortcuts and the Recent-strip
preference. Loading a preset or restarting Snapper never activates hardware.
Unreadable settings are reported without overwriting the damaged file.

## Edit images

Import PNG/JPEG, paste an image from the clipboard, or open a screenshot from
Recent/Library. The original is never overwritten by annotation.

Select a tool and drag on the image. Text, callouts and stamps also accept a click
to place a default-sized item. Edit the text and properties on the right.
**Select** selects and moves existing annotations. Delete removes the selected
annotation. Ctrl+Z and Ctrl+Y undo/redo when a text field is not being edited.

**Fill** changes a connected region with a color tolerance. A transparent color
can clear that region. **Color replace** is a separate effect: choose From and
Color, drag a selection or click for the whole image. The swap option exchanges
both original colors simultaneously. **Pick source color** samples a displayed
pixel on the next click.

**Cut H** removes a full-width horizontal strip and closes the gap. **Cut V**
removes a full-height vertical strip. **Jagged cut** leaves a small transparent
torn-style gap. **Eraser** removes alpha along a width/opacity-controlled stroke.
Blur and pixelation change only the selected rectangle. They do not securely
remove secrets. Editable projects intentionally retain originals and history
operations; share a flattened export instead of a project when that distinction
matters. Fully erased pixels in flattened PNG have their RGB cleared as well.

The inspector's saved-style chooser reuses tool properties; **Pin**/**Unpin**
controls Favorites. Styles and Favorites survive restart. The Effects/Properties control
exposes grayscale, an inset border, original dimensions and layer count.
Zoom affects only display size, not exported pixels.

Edits, titles and image tags save locally after successful background preparation.
Newer edits cancel superseded image and preview work at bounded checkpoints.
An obsolete prepared revision is rejected at the save-publication boundary,
even if the newer render fails. Rendering and preview preparation share a
30-second cooperative budget; that is not a hard completion-time promise for
imports, decoding, operating-system disk I/O or exports. Rendering uses one
worker, not a thread per layer. A high-zoom checkerboard is painted only inside
the visible viewport. Recent captures retain small thumbnails, not old
full-sized editor previews.
Wait for the Saving indicator to finish before switching images or closing.
The recent strip shows the newest 24 captures; Library searches all indexed
captures by title or tag.

## Export and playback

**Copy image** copies the canonical flattened RGBA bitmap. **Export image**
writes PNG, or JPEG at quality 95 with transparent pixels composited over white.
JPEG is lossy; it cannot preserve pixel equality with the source.

For videos, **Play original** opens the local MP4 in the configured Windows
player. Titles and tags use **Save title and tags** without rewriting the MP4.

## Focused video editing

Open a recording from Library to inspect its original and saved edit plan.
The edit timeline is explicitly **30 fps**: one frame is 33.333 ms. Time labels
are rounded for display; selections use whole frames and the end is excluded.
Variable-rate source video is normalized onto that grid without changing the
original. This is accurate re-encoding, not a fast keyframe-rounded copy.

Drag the timeline's selection handles or use **Start here** and **End here**.
**Keep selection / trim ends** retains the selected part. **Remove selection**
cuts a middle interval. **Split at playhead** creates a section boundary without
changing the content. Undo/redo and **Restore full original timeline** change
only the edit instructions. Up to 64 ordered kept sections are supported.

**Update edited preview** renders the current plan in a supervised background
job. Until it finishes, the UI labels the edited preview as unavailable or
stale rather than showing the original as if cuts had been applied. The
playhead scrubs decoded frames of the verified edited file. **Play edited
preview** opens that exact local MP4 in the configured Windows player. Existing
player windows retain their old snapshot; open the updated preview to see new
edits. **Play original** remains a separate explicit action.

**Save edited MP4** copies the same verified rendering to a new filename.
**Save selected clip** derives a sub-plan for the chosen edited-timeline range.
Existing destination files, including originals or aliases of them, are never
overwritten. Video and optional audio share one presentation origin; cuts use
exact frame/sample counts and are encoded once into H.264/AAC. Codec priming
and final AAC padding are verified separately from the video-frame timeline.

Progress reports preparation, encoding, verification and final publication.
Cancel stops the owned codec job; a short final publication already in progress
reports that cancellation is too late rather than pretending no file was saved.
The original recording and committed recovery segments remain unchanged.
Preview files are derived, local cache data, limited to three renders per video.
Close old player windows if they hold a cache file open.

There is no multitrack production timeline, caption/AI pipeline or cloud sharing.

Preflight's **After capture** choice routes a screenshot directly to the editor,
bitmap clipboard or a chosen local folder. Video routes to the library view or
a local file. Automatic file routing uses a unique capture name and atomic
publication without overwriting a competing file. Every route retains the
immutable local-library original. A clipboard/export failure is reported
without discarding that original.

## Storage and recovery

The default library is `%LOCALAPPDATA%\Snapper\Library`. Each image has
an immutable `original.png`, a versioned editable document, a thumbnail and
capture metadata. Tags/Quick Styles and nondestructive `video-edits.json` plans
are local JSON. There is no external upload.
The prior application's library is not moved or copied automatically. Use
`--data-dir <absolute directory>` to deliberately open another local library.

Recordings retain independently finalized MP4 segments in a private per-user
session folder. Every published segment is decoded, flushed, renamed and then
added to the atomic journal. Normal completion remuxes those segments into one
ordinary MP4. Segments remain available after export; they consume additional
disk space. The application does not delete old captures to make room.

Interrupted sessions appear in Library. **Recover verified segments** decodes
retained committed segments and builds a new MP4. The uncommitted tail is not
promised to be playable. Failure before the first completed segment can leave
nothing recoverable. A missing, corrupt or inconsistent journal produces an
explicit warning without hiding other captures. Interrupted image imports with
an intact original and saved import intent are adopted on Library refresh;
missing originals are not fabricated. A final MP4 already published before a
metadata failure is adopted only when its saved fingerprint, recording identity,
stream selection, dimensions and duration agree. It is held against writes or
replacement throughout validation and completion.

An unrelated final MP4, or one created by an older build without a saved
publication identity, is left unchanged and recovery reports the mismatch.
Move that final file aside before retrying recovery from the retained segments.
The application never treats an arbitrary playable file at the expected name
as proof that your recording was recovered.

## Limits

- 24 megapixels and 16,384 pixels per image dimension.
- 512 annotation layers; 80 undo checkpoints with a 12 MiB metadata-history
  budget. Raster originals are not copied into every undo state.
- 128 saved styles, 32 tags per capture, 5,000 indexed capture folders.
- One active capture/recording and one library/export job per window.
- A latest-frame mailbox per visual source; two seconds of PCM queue capacity
  per selected audio source (maximum 768,000 queued/staged bytes), a 512-message
  channel cap and at most 512 pending packets per source. A single prepared
  100 ms PCM span adds at most 19,200 bytes. Native acquisition buffers and
  third-party encoder internals are separate allocations.
- 4,096 pause intervals per recording. A pending pause/resume or pipe write
  has a five-second wall-time watchdog; pausing the media clock does not
  disable shutdown monitoring.
- A selected microphone must deliver a real packet within one second and must
  keep delivering packets. Timestamped Windows `SILENT` packets are legitimate;
  missing packets do not turn the microphone into an invented silence track.
  Quiet system-loopback uses a separate 250 ms deferred-silence policy.
- 512 MiB free-space admission and 256 MiB recording-stop thresholds. Final
  export requires room for a second media copy.

These are finite resource policies, not a guarantee of responsiveness under
kernel/driver hangs or arbitrary CPU, GPU, memory or storage exhaustion.
Published performance claims require measured results on the relevant host.
