# Snapper

<!-- impeccable:product-schema 1 -->

## Platform

Native Windows. This is a Rust/egui desktop application and a separate Go CLI,
not a web application or an iOS/Android interface.

## Users and purpose

People capture screen content, record explanations, annotate images and find
their recent captures locally. Automation clients use the lightweight Go CLI
to obtain monitor or window screenshots as JSON.

## Confirmed workflow

The desktop capture experience is a compact launcher, a visual drag/resize
region overlay, source review, an explicit Capture or Record action, and a
small movable Pause/Stop dock. The editor stays out of captured content.
Escape cancels the whole active selection or countdown without publishing an
image, changing the clipboard or starting a device. Numeric region controls
supplement visual selection.

## Capabilities and constraints

- Preserve the Go CLI: no arguments capture all monitors and emit JSON.
  Desktop and CLI builds are separate; the desktop binary is
  `snapper-desktop.exe`.
- Independent cursor, webcam, system-audio and microphone choices are required.
  Device selection and saved preferences do not activate hardware. Camera
  preview is a separate explicit action.
- Region coordinates are physical pixels on a selected monitor. Window-guided
  selection means a fixed screen region, not isolated window-content recording.
- Required image tools include arrows, text, callouts, shapes, stamps,
  contiguous fill, separate color replacement/swap, styles/favorites, cutouts,
  blur, pixelation and alpha erasure. Blur and pixelation are not secure redaction.
- Native Windows/RDP operation, per-monitor DPI, bounded media memory,
  supervised device ownership, immutable originals and recoverable publication
  are required. Software/WARP rendering must be identified truthfully.
- Cloud sharing, accounts, OCR/AI redaction, marketplaces, scrolling capture
  and a full video editor are not included by implication.

## Brand commitments

Snapper is the product name. The supplied competitor screenshot defines
capabilities only. Do not copy its logo, capture button, ribbon, palette layout
or proprietary assets. Use original icons and familiar Windows interaction
conventions.

## Evidence and release boundary

Imported source provenance is recorded in `desktop/docs/IMPORT.md`. Source
tests and synthetic media checks are distinct from approved native-window and
real-device acceptance. Do not invent performance measurements or claim
hardware acceleration from language choice. Integration and packages remain
local until publication is explicitly authorized.

## Product principles

Capture is intentional. Cancellation is complete. Source activation is visible.
Saved means durable. Existing automation remains compatible.
