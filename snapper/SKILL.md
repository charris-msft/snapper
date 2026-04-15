---
name: snapper
description: "Screen capture tool for AI agents. Snapper is a Windows CLI that takes screenshots and returns them as base64-encoded PNG images in JSON format. Use when you need to see the current state of the screen, analyze UI, read text from displays, or monitor multiple monitor setups. Returns monitor dimensions, image data, and file paths."
license: MIT
compatibility: "Windows only (requires Win32 GDI). Binary available in PATH or buildable from source."
allowed-tools: "shell powershell"
metadata:
  version: "1.0"
  platform: "Windows"
  format: "PNG"
  performance: "~125ms (single), ~188ms (all), ~293ms (stitched)"
---

# Snapper Agent Skill

## Overview

Snapper captures screenshots from Windows displays and returns them as JSON. It's designed for AI agents that need visual context of the current screen state. Whether you're analyzing UI elements, reading text, or monitoring multiple displays, snapper provides fast, structured access to screen content.

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

### Stitch all monitors into one image
```powershell
snapper --stitch
```

**Expected output:**
```json
{"monitor":0,"width":4480,"height":1440,"format":"png","image":"iVBORw0KGgo..."}
```

### Save to file instead of base64
```powershell
snapper -m 1 -o
```

**Expected output:**
```json
{"monitor":1,"width":2560,"height":1440,"format":"png","file":"C:\\Users\\username\\AppData\\Local\\Temp\\snapper_1_20250714_120000.png"}
```

## Command Reference

### Options

| Option | Short | Purpose | Example |
|--------|-------|---------|---------|
| `--monitor N` | `-m N` | Capture specific monitor (1-based) | `snapper -m 2` |
| `--output-file` | `-o` | Write PNG to temp file instead of base64 | `snapper -o` |
| `--stitch` | — | Combine all monitors into one image | `snapper --stitch` |
| `--help` | `-h` | Show help message | `snapper -h` |

### Exit Codes

- **0**: Success
- **1**: Argument/usage error
- **2**: Capture error (e.g., display not found, permission denied)

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

**Multiple monitors response (array):**
```json
[
  {"monitor": 1, "width": 2560, "height": 1440, "format": "png", "image": "..."},
  {"monitor": 2, "width": 1920, "height": 1080, "format": "png", "image": "..."}
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

**Stitched response (monitor field is 0):**
```json
{
  "monitor": 0,
  "width": 4480,
  "height": 1440,
  "format": "png",
  "image": "iVBORw0KGgo..."
}
```

## Common Patterns

### Pattern 1: Screenshot and Analyze

Capture a screenshot and pass the base64 image to vision analysis:

```powershell
$result = snapper -m 1 | ConvertFrom-Json
$imageBase64 = $result.image
# Now use $imageBase64 with Claude's vision API or similar
```

### Pattern 2: Multiple Monitors - Full Context

When you need to see all displays at once, use stitched mode for a unified view:

```powershell
$result = snapper --stitch | ConvertFrom-Json
# $result.width and $result.height give total dimensions
# $result.image contains the full stitched PNG as base64
```

### Pattern 3: Individual Monitors

Check each monitor separately to understand the layout:

```powershell
$monitors = snapper | ConvertFrom-Json
$monitors | ForEach-Object {
  Write-Host "Monitor $($_.monitor): $($_.width)x$($_.height)"
}
```

### Pattern 4: Save Large Screenshots

For large images or when working with external tools, save to file instead of base64:

```powershell
$result = snapper --stitch -o | ConvertFrom-Json
$filePath = $result.file
# Process file at $filePath
```

### Pattern 5: Retry on Failure

If capture fails, retry with appropriate backoff:

```powershell
$maxRetries = 3
$attempt = 0
do {
  $attempt++
  try {
    $result = snapper -m 1 | ConvertFrom-Json
    break
  } catch {
    if ($attempt -eq $maxRetries) {
      Write-Error "Failed to capture screenshot after $maxRetries attempts"
      exit 2
    }
    Start-Sleep -Milliseconds 500
  }
} while ($true)
```

## Error Handling

### Exit Code 1 (Argument Error)

**Cause:** Invalid command-line arguments.

```powershell
# Example: Invalid monitor number
snapper -m 99
# stderr: Monitor 99 not found
# exit code: 1
```

**Resolution:** Check the command syntax and available monitors.

### Exit Code 2 (Capture Error)

**Cause:** Display capture failed (e.g., monitor disconnected, permission denied).

```powershell
# Example: Monitor disconnected between check and capture
snapper -m 2
# stderr: Failed to capture monitor 2
# exit code: 2
```

**Resolution:** Verify displays are connected and try again. Check system permissions.

### Handling JSON Parse Errors

Always validate JSON before using:

```powershell
try {
  $result = snapper -m 1 | ConvertFrom-Json
} catch {
  Write-Error "Failed to parse snapper output: $_"
  exit 1
}
```

## Tips & Best Practices

1. **Check available monitors first:** Run `snapper` with no args to see all monitors and their resolutions before targeting a specific one.

2. **Use `-m` for speed:** Single monitor capture (~125ms) is faster than all monitors (~188ms). If you only need one display, use `-m 1`.

3. **Use `--stitch` for context:** When analyzing UI that spans multiple monitors, use `snapper --stitch` to get a single image showing all displays.

4. **File output for large images:** Use `-o` when the base64 string would be very large (e.g., 4K displays). Files are in temp directory and auto-cleaned by the system.

5. **Decode base64 if needed:** The `image` field is base64-encoded PNG. Most vision APIs accept base64 directly, but you can decode to binary if needed:

   ```powershell
   $imageBytes = [Convert]::FromBase64String($result.image)
   [System.IO.File]::WriteAllBytes("screenshot.png", $imageBytes)
   ```

6. **Monitor dimensions matter:** Check `width` and `height` to understand display scaling and position UI analysis accordingly.

## Platform Requirements

- **OS:** Windows only (uses Win32 GDI API)
- **Architecture:** x64
- **Permissions:** User-level (no admin required for screenshot capture)

## Performance

- Single monitor: ~125ms
- All monitors: ~188ms
- All monitors stitched: ~293ms

These are typical timings; actual performance depends on display resolution and system load.

## Troubleshooting

**"snapper: command not found"**
- Snapper binary is not in PATH. Add the directory containing `snapper.exe` to your PATH, or use the full path when running.

**"Monitor N not found"**
- Invalid monitor number. Use `snapper` with no args to list available monitors.

**"Failed to capture monitor"**
- Display may be disconnected, locked, or experiencing hardware issues. Try again or check system display settings.

**Large base64 strings causing memory issues**
- Use `-o` flag to save to file instead of base64. Process the file path instead of the large string.

---

**Last Updated:** 2025-01-16  
**Compatibility:** snapper 1.0+
