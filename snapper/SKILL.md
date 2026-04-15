---
name: snapper
description: "Screen and window capture tool for AI agents. Snapper is a Windows CLI that takes screenshots of monitors or specific windows and returns them as base64-encoded images (PNG or JPEG) in JSON format. Use when you need to see the current state of the screen, capture a specific application window, analyze UI, read text from displays, or monitor multiple displays. Returns dimensions, image data, window titles, PIDs, and file paths."
license: MIT
compatibility: "Windows only (requires Win32 GDI). Binary available in PATH or buildable from source."
allowed-tools: "shell powershell"
metadata:
  version: "2.0"
  platform: "Windows"
  format: "PNG, JPEG"
  performance: "~125ms (single), ~188ms (all), ~293ms (stitched)"
---

# Snapper Agent Skill

## Overview

Snapper captures screenshots from Windows displays and individual application windows, returning them as JSON. It's designed for AI agents that need visual context of the current screen state. Capture entire monitors, specific windows by title or PID, and choose between PNG (lossless) or JPEG (smaller payload) output.

## Installation

Snapper is a Windows CLI tool. The binary (`snapper.exe`) should be in your PATH. If it's not available:

```powershell
# Build from source
git clone https://github.com/user/snapper.git
cd snapper
.\build.ps1
# Binary will be at .\snapper.exe, add to PATH or run with full path
```

## Quick Start

### Capture single monitor as base64
```powershell
snapper -m 1
```

**Expected output:**
```json
{"monitor":1,"width":2560,"height":1440,"format":"png","image":"iVBORw0KGgoAAAANS..."}
```

### Capture a specific window by title
```powershell
snapper -w "Visual Studio Code"
```

**Expected output:**
```json
{"monitor":0,"width":1920,"height":1080,"format":"png","title":"Visual Studio Code","pid":1234,"image":"iVBORw0KGgo..."}
```

### Capture a window by process ID
```powershell
snapper --pid 1234
```

### List all visible windows
```powershell
snapper --list-windows
```

**Expected output:**
```json
[
  {"title":"Visual Studio Code","pid":1234,"x":0,"y":0,"width":1920,"height":1080},
  {"title":"Windows Terminal","pid":5678,"x":100,"y":100,"width":800,"height":600}
]
```

### Capture all monitors
```powershell
snapper
```

**Expected output:**
```json
[
  {"monitor":1,"width":2560,"height":1440,"format":"png","image":"iVBORw0KGgo..."},
  {"monitor":2,"width":1920,"height":1080,"format":"png","image":"iVBORw0KGgo..."}
]
```

### Use JPEG for smaller output (ideal for AI vision)
```powershell
snapper -m 1 -f jpg -q 75
```

### Stitch all monitors into one image
```powershell
snapper --stitch
```

### Save to file instead of base64
```powershell
snapper -m 1 -o
```

## Command Reference

### Options

| Option | Short | Purpose | Example |
|--------|-------|---------|---------|
| `--monitor N` | `-m N` | Capture specific monitor (1-based) | `snapper -m 2` |
| `--window <title>` | `-w <title>` | Capture window by title (substring match) | `snapper -w "Code"` |
| `--pid N` | — | Capture window by process ID | `snapper --pid 1234` |
| `--list-windows` | — | List all visible windows as JSON | `snapper --list-windows` |
| `--format <fmt>` | `-f <fmt>` | Output format: `png` (default) or `jpg` | `snapper -f jpg` |
| `--quality N` | `-q N` | JPEG quality 1-100 (default 85) | `snapper -f jpg -q 70` |
| `--output-file` | `-o` | Write image to temp file instead of base64 | `snapper -o` |
| `--stitch` | — | Combine all monitors into one image | `snapper --stitch` |
| `--help` | `-h` | Show help message | `snapper -h` |

### Exit Codes

- **0**: Success
- **1**: Argument/usage error
- **2**: Capture error (e.g., display not found, window not found)

### Output Format

All output is JSON written to stdout. Diagnostics and errors go to stderr.

**Single monitor response:**
```json
{
  "monitor": 1,
  "width": 2560,
  "height": 1440,
  "format": "png",
  "image": "iVBORw0KGgo..."
}
```

**Window capture response:**
```json
{
  "monitor": 0,
  "width": 1200,
  "height": 800,
  "format": "jpg",
  "title": "My App - Document.txt",
  "pid": 12345,
  "image": "..."
}
```

**Multiple monitors response (array):**
```json
[
  {"monitor": 1, "width": 2560, "height": 1440, "format": "png", "image": "..."},
  {"monitor": 2, "width": 1920, "height": 1080, "format": "png", "image": "..."}
]
```

**List windows response:**
```json
[
  {"title": "Visual Studio Code", "pid": 1234, "x": 0, "y": 0, "width": 1920, "height": 1080},
  {"title": "Windows Terminal", "pid": 5678, "x": 100, "y": 100, "width": 800, "height": 600}
]
```

**File output response:**
```json
{
  "monitor": 1,
  "width": 2560,
  "height": 1440,
  "format": "png",
  "file": "C:\\Users\\username\\AppData\\Local\\Temp\\snapper_1_20250714_120000.png"
}
```

## Common Patterns

### Pattern 1: See What App Looks Like

Capture a specific application window for visual analysis:

```powershell
$result = snapper -w "Chrome" -f jpg -q 80 | ConvertFrom-Json
# $result.image contains the window screenshot as base64 JPEG
# $result.title has the exact window title
# $result.pid has the process ID
```

### Pattern 2: Discover Running Apps

List all visible windows to find what's running, then capture one:

```powershell
# Step 1: See what's available
$windows = snapper --list-windows | ConvertFrom-Json
$windows | ForEach-Object { Write-Host "$($_.pid)  $($_.width)x$($_.height)  $($_.title)" }

# Step 2: Capture the one you need
$result = snapper -w "Notepad" | ConvertFrom-Json
```

### Pattern 3: Screenshot and Analyze

Capture a screenshot and pass the base64 image to vision analysis:

```powershell
$result = snapper -m 1 | ConvertFrom-Json
$imageBase64 = $result.image
# Now use $imageBase64 with Claude's vision API or similar
```

### Pattern 4: Efficient Capture with JPEG

Use JPEG to reduce payload size (3-5× smaller than PNG). AI vision models see no quality difference at q70+:

```powershell
$result = snapper -m 1 -f jpg -q 75 | ConvertFrom-Json
# Much smaller base64 string, same visual quality for AI analysis
```

### Pattern 5: Multiple Monitors - Full Context

When you need to see all displays at once, use stitched mode:

```powershell
$result = snapper --stitch -f jpg -q 80 | ConvertFrom-Json
# $result.width and $result.height give total dimensions
# $result.image contains the full stitched image as base64
```

### Pattern 6: Save Large Screenshots

For large images or when working with external tools, save to file:

```powershell
$result = snapper --stitch -o | ConvertFrom-Json
$filePath = $result.file
# Process file at $filePath
```

## Tips & Best Practices

1. **Use `-w` to capture specific apps:** `snapper -w "Code"` captures just VS Code, avoiding clutter from other windows.

2. **Use `--list-windows` first:** Discover what's running before capturing. This gives you titles and PIDs to use with `-w` and `--pid`.

3. **Use JPEG for AI vision:** `-f jpg -q 75` gives 3-5× smaller payloads than PNG. AI models see no difference.

4. **Use `-m` for speed:** Single monitor capture (~125ms) is faster than all monitors (~188ms).

5. **Use `--stitch` for spatial context:** When analyzing UI that spans multiple monitors.

6. **File output for large images:** Use `-o` when the base64 string would be very large (e.g., 4K displays).

7. **Window title matching is substring and case-insensitive:** `-w "code"` matches "Visual Studio Code".

## Error Handling

### Exit Code 1 (Argument Error)

**Cause:** Invalid command-line arguments, mutually exclusive flags, invalid format.

```powershell
snapper -w "App" --pid 123   # Error: --window and --pid are mutually exclusive
snapper -w "App" -m 1        # Error: can't combine window and monitor capture
snapper -f bmp               # Error: unsupported format (use png or jpg)
```

### Exit Code 2 (Capture Error)

**Cause:** Window not found, display capture failed, or monitor disconnected.

```powershell
snapper -w "NonExistentApp"  # Error: no window found matching "NonExistentApp"
snapper -m 99                # Error: monitor 99 not found
```

## Platform Requirements

- **OS:** Windows only (uses Win32 GDI API)
- **Architecture:** x64
- **Permissions:** User-level (no admin required)

## Performance

- Single monitor: ~125ms
- All monitors: ~188ms
- All monitors stitched: ~293ms
- Window capture: ~100-200ms

These are typical timings; actual performance depends on display resolution and system load.

## Troubleshooting

**"snapper: command not found"**
- Snapper binary is not in PATH. Add the directory containing `snapper.exe` to your PATH.

**"no window found matching ..."**
- Window may be minimized or closed. Use `snapper --list-windows` to see what's available.

**"Monitor N not found"**
- Invalid monitor number. Use `snapper` with no args to list available monitors.

**PrintWindow fails for some apps**
- Some GPU-rendered or UWP apps may not support PrintWindow. Try capturing the full screen with `-m` instead.

**Large base64 strings causing issues**
- Use `-f jpg` to reduce size, or `-o` to save to file instead of base64.

---

**Last Updated:** 2026-04-15
**Compatibility:** snapper 2.0+
