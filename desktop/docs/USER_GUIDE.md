# SteadyCapture user guide

## Capture

1. Choose the monitor on the left. **Region (physical pixels)** accepts a
   monitor-relative left/top/width/height. **Whole monitor** resets it.
2. Independently choose cursor, webcam, system audio and microphone. For webcam,
   **Scan cameras** lists devices without activating them; choose one explicitly.
   The microphone and system-audio choices use Windows' default input/output
   devices. System audio includes other applications playing on that output.
3. Choose **Take screenshot** or **Record screen**. Devices remain closed during
   the visible three-second countdown. Cancel the countdown or press Escape to
   cancel before activation.
4. During recording, the left panel shows active-source choices, elapsed time,
   recorded/skipped frames and a red Stop button. Ctrl+Shift+F9 stops from any
   application without waiting for editor painting. Escape stops recording while
   the application window has keyboard focus. A shortcut-registration conflict
   is reported explicitly; the button and focused Escape still work.
5. Wait for **Saved**. Stopping, devices released, finalizing and saved are
   different states. A stop request alone does not mean the file is finished.
   **Cancel finalization** ends a blocked export while keeping committed
   segments available in Library for recovery.

Screenshots retain the selected native pixel dimensions. Recording preserves
aspect ratio and fits the region within 1920 x 1080, with even dimensions and a
30 Hz target schedule. Webcam picture-in-picture appears at the bottom right.
Camera input is bounded to a negotiated mode at or below 1280 x 720 and 30 fps.
Recordings stop at the selected 1-120 minute limit. They also stop on explicit
device/timing/queue failures or low free space, rather than silently dropping a
selected source. Protected content, secure desktops, unsupported graphics
drivers and known physical HDR displays are not supported capture targets.
Virtual/RDP displays without a physical DXGI color descriptor use Float16 WGC
capture and explicit linear-to-sRGB SDR conversion. Bright values above SDR
white are clipped; this is not an HDR-preserving export. The active path is
displayed while recording. Monitor coordinates are physical pixels, not
DPI-scaled browser/window coordinates.

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

Quick Styles save tool properties for reuse; the star button pins/unpins
Favorites. Styles and Favorites survive restart. The Effects/Properties control
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

For videos, **Play recording** opens the local MP4 in the configured Windows
player. **Save MP4 as** copies it elsewhere. Video trimming and a video-annotation
timeline are not part of this application.

## Storage and recovery

The default library is `%LOCALAPPDATA%\SteadyCapture\Library`. Each image has
an immutable `original.png`, a versioned editable document, a thumbnail and
capture metadata. Tags/Quick Styles are local JSON. There is no external upload.

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
  per selected audio source (maximum 768,000 bytes), plus a 512-message cap.
- A selected microphone must deliver a real packet within one second and must
  keep delivering packets. Timestamped Windows `SILENT` packets are legitimate;
  missing packets do not turn the microphone into an invented silence track.
  Quiet system-loopback uses a separate 250 ms deferred-silence policy.
- 512 MiB free-space admission and 256 MiB recording-stop thresholds. Final
  export requires room for a second media copy.

These are finite resource policies, not a guarantee of responsiveness under
kernel/driver hangs or arbitrary CPU, GPU, memory or storage exhaustion.
Published performance claims require measured results on the relevant host.
