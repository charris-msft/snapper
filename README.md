# Snapper

A fast Windows screenshot CLI for AI agents. Captures screens via Win32 GDI BitBlt and outputs JSON to stdout.

**3.3 MB binary** · **~125ms single monitor** · **Zero dependencies**

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

Options:
  -m, --monitor <N>   Monitor number (1-based). Omit to capture all.
  -o, --output-file   Write PNG to temp file instead of base64.
      --stitch        Combine all monitors into one image.
  -h, --help          Show help.
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

**File output (`-o`):**
```json
{"monitor":1,"width":2560,"height":1440,"format":"png","file":"C:\\Users\\...\\snapper_1_20250714_120000.png"}
```

**Stitched (`--stitch`):**
```json
{"monitor":0,"width":4480,"height":1440,"format":"png","image":"..."}
```

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
2. `BitBlt` (SRCCOPY) to capture pixels from each monitor's DC
3. BGRA→RGBA pixel swizzle directly on the image buffer
4. PNG encode with `BestSpeed` compression
5. JSON output to stdout

Multi-monitor captures run in parallel goroutines. Stitching composites raw pixels and encodes PNG only once.

## License

MIT
