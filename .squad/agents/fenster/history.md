# Fenster — History

## Core Context

- **Project:** A Windows screenshot capture application with rigorous visual accuracy testing
- **Role:** Core Dev
- **Joined:** 2026-04-14T17:00:16.965Z

## Learnings

<!-- Append learnings below -->

### GDI BitBlt Capture Implementation (2025-07-16)
- Implemented `GdiBitBltBackend` with full P/Invoke-based screen capture via `CreateDCW("DISPLAY")` → `BitBlt` → `GetDIBits`.
- P/Invoke declarations isolated in `NativeMethods.cs` using `LibraryImport` (source-generated marshalling, .NET 7+). Cleaner than `DllImport` and avoids runtime stub generation.
- Used `BitmapInfoHeader.biHeight = -height` (negative) to get top-down row order directly from `GetDIBits`, avoiding a manual row-flip pass.
- All GDI handles (`HDC`, `HBITMAP`) released in `finally` blocks — no `SafeHandle` wrappers needed since handles are short-lived (acquired and released per capture call).
- `ResolveRegion` now properly chains: explicit `Region` → `ScreenIndex` via `IScreenProvider` → primary screen fallback. Constructor accepts `IScreenProvider` for testability (defaults to `Win32ScreenProvider`).
- Output format is BGRA32 (32-bit with alpha channel set by GDI to 0xFF or 0x00 depending on source). Matches `PixelFormat.Bgra32`.
- Created `src/Snapper.Cli/Snapper.Cli.csproj` — console app targeting `net8.0-windows`, refs Snapper.Core + Snapper.Platform, UseWindowsForms enabled.
- Created `src/Snapper.Cli/Program.cs` — placeholder Main that prints "Snapper CLI" and returns 0.
- Created `tests/Snapper.Cli.Tests/Snapper.Cli.Tests.csproj` — xunit test project matching existing test conventions (xunit 2.5.3, NSubstitute 5.1.*, coverlet, MS Test SDK 17.8.0). Refs Snapper.Cli + Snapper.Core.
- Added both projects to `Snapper.slnx` under `/src/` and `/tests/` folders respectively.
- Full solution builds clean: 0 warnings, 0 errors.
- CLI arg parsing: hand-rolled switch loop over `string[] args`, no external packages. Discriminated union via abstract record `CliParseResult` with `Success`/`Error`/`Help` subtypes. Exit codes in `ExitCodes` static class (0=success, 1=arg error, 2=capture error). Validation: `--stitch` + `--monitor` is an error; monitor must be positive int; unknown args rejected.
- Files created: `src/Snapper.Cli/CliOptions.cs` (options record, parse result, parser), `src/Snapper.Cli/HelpText.cs` (formatted help string).

### CLI Orchestration & Output Implementation (2025-07-17)
- `CaptureOrchestrator` coordinates the full capture flow: enumerate screens via `IScreenProvider`, validate monitor index, build `CaptureRequest`(s), run through `CapturePipeline`. Constructor accepts `IScreenProvider` for testability, defaults to `Win32ScreenProvider`.
- Monitor number conversion: CLI uses 1-based (`--monitor 1`), backend uses 0-based (`ScreenIndex = 0`). Conversion happens in `CaptureOrchestrator.RunAsync`.
- `OutputFormatter` writes JSON to stdout via `Utf8JsonWriter` on `Console.OpenStandardOutput()` — no extra text pollutes the stream. All diagnostic messages (file paths, errors) go to stderr.
- Three output modes: (1) base64 JSON (default), (2) file mode (`--output-file`) writes PNG to temp dir with `snapper_{monitor}_{timestamp}.png` naming, (3) stitch mode (`--stitch`) composites all monitors onto a single bitmap using `System.Drawing.Graphics` based on `ScreenInfo.Bounds` positions.
- Stitch canvas sizing: compute bounding rect from all screen bounds (min X/Y to max right/bottom), draw each capture at `(bounds.X - minX, bounds.Y - minY)`. Handles non-zero-origin multi-monitor layouts.
- `Program.Main` changed to `async Task<int>` to support pipeline's async execution. Pattern-matches on `CliParseResult` subtypes. Help text goes to stderr (stdout reserved for JSON).
- Files created: `src/Snapper.Cli/CaptureOrchestrator.cs`, `src/Snapper.Cli/OutputFormatter.cs`. Modified: `src/Snapper.Cli/Program.cs`.
- Full solution builds clean: 0 warnings, 0 errors.

### Team Session: Snapper CLI Implementation (2026-04-14)
- **Orchestration logging:** 5 agent task logs + 1 session log written; team context preserved
- **Testing feedback (mcmanus):** All 21 tests passing; no issues found in parser or GDI backend behavior; full test coverage verified
- **Status:** CLI implementation complete and tested; ready for next phase
