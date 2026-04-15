# Snapper

A fast Windows screenshot CLI for AI agents. Captures screens and windows via Win32 GDI and outputs JSON to stdout.

**3.5 MB binary** · **~125ms single monitor** · **Zero dependencies**

## Install

Download `snapper.exe` from [Releases](https://github.com/charris-msft/snapper/releases) or build from source:

```powershell
cd go && go build -o snapper.exe .
```

Or use the build script which also publishes to your tools folder:

```powershell
.\build.ps1
```

## Usage

```
snapper [options]

Capture Target (pick one, or omit for all monitors):
  -w, --window <title>  Capture a window whose title contains <title> (case-insensitive).
      --pid <N>         Capture the main window of process <N>.
  -m, --monitor <N>     Capture monitor N (1-based).
      --stitch          Capture all monitors combined into one image.
      --list-windows    List all visible windows as JSON (no capture).
  (no target)           Capture every monitor as a separate image.

Output Options:
  -f, --format <fmt>    png (default, lossless) or jpg (smaller, recommended for AI).
  -q, --quality <1-100> JPEG quality (default 85). Ignored for png.
  -o, --output-file     Write image to temp file; JSON returns file path.
  (default)             Base64-encode image inline in JSON.

General:
  -h, --help            Show help.
```

### Examples

```powershell
# See what apps are running
snapper --list-windows

# Capture a specific app window as JPEG
snapper -w "Visual Studio Code" -f jpg

# Capture a window by its process ID
snapper --pid 1234 -f jpg -q 75

# Capture primary monitor, save to temp file
snapper -m 1 -o

# Capture all monitors stitched into one image
snapper --stitch -f jpg -q 80
```

## Output Format

JSON to stdout. All diagnostics go to stderr.

**Single monitor (base64):**
```json
{"monitor":1,"width":2560,"height":1440,"format":"png","image":"iVBOR..."}
```

**All monitors:**
```json
[
  {"monitor":1,"width":2560,"height":1440,"format":"png","image":"..."},
  {"monitor":2,"width":1920,"height":1080,"format":"png","image":"..."}
]
```

**Window capture (`-w` or `--pid`):**
```json
{"monitor":0,"width":1200,"height":800,"format":"jpg","title":"My App","pid":12345,"image":"..."}
```

**File output (`-o`):**
```json
{"monitor":1,"width":2560,"height":1440,"format":"png","file":"C:\\Users\\...\\snapper_1_20250714_120000.png"}
```

**Stitched (`--stitch`):**
```json
{"monitor":0,"width":4480,"height":1440,"format":"png","image":"..."}
```

**List windows (`--list-windows`):**
```json
[
  {"title":"Visual Studio Code","pid":1234,"x":0,"y":0,"width":1920,"height":1080},
  {"title":"Windows Terminal","pid":5678,"x":100,"y":100,"width":800,"height":600}
]
```

## JPEG vs PNG

Use `-f jpg` to significantly reduce payload size for AI vision models:

| Format | Typical full-screen size |
|--------|------------------------|
| PNG | ~600-900 KB |
| JPG q85 | ~300-500 KB |
| JPG q50 | ~150-250 KB |

AI vision models see no quality difference at q70+. Use JPEG when token budgets matter.

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Argument error |
| 2 | Capture error |

## Performance

Benchmarked on i9-13900, 2 monitors (2194×1234 + 2560×1440):

| Mode | Time |
|------|------|
| Single monitor | ~125ms |
| All monitors | ~188ms |
| Stitched | ~293ms |

## How It Works

1. `EnumDisplayMonitors` to discover screens
2. `EnumWindows` to discover windows (for `--list-windows` / `-w` / `--pid`)
3. `BitBlt` (SRCCOPY) to capture pixels from each monitor's DC
4. `PrintWindow` with `PW_RENDERFULLCONTENT` for individual window capture
5. BGRA→RGBA pixel swizzle directly on the image buffer
6. PNG or JPEG encode depending on `--format`
7. JSON output to stdout

Multi-monitor captures run in parallel goroutines. Stitching composites raw pixels and encodes only once.

## License

MIT
