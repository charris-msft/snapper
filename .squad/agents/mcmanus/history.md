# McManus — History

## Core Context

- **Project:** A Windows screenshot capture application with rigorous visual accuracy testing
- **Role:** Tester
- **Joined:** 2026-04-14T17:00:16.968Z

## Learnings

<!-- Append learnings below -->

### 2025-07-17 — CLI Parser & GDI Backend Test Coverage
- **Added `InternalsVisibleTo`** in `src/Snapper.Cli/Properties/AssemblyInfo.cs` so test project can access internal types (`CliParseResult`, `CliOptions`, `ExitCodes`).
- **CliParseResultTests (16 tests):** Covers all happy paths (no args defaults, `-m N`, `--monitor N`, `-o`, `--output-file`, `--stitch`, combined flags), help variants (`-h`, `--help`), and error cases (missing value, zero/negative/non-numeric monitor, stitch+monitor conflict both orderings, unknown args). All return correct result types and exit codes.
- **GdiBitBltBackendTests (5 integration tests):** Captures primary screen verifying positive dimensions, BGRA32 format, non-empty pixel data, dimensions matching `Win32ScreenProvider.GetPrimaryScreen().Bounds`, ScreenIndex 0 success, out-of-range index throws `ArgumentOutOfRangeException`, and pixel data length = W×H×4. Marked with `[Trait("Category", "Integration")]`.
- **All 43 tests pass** (41 succeeded, 2 pre-existing skips for accuracy/encoder stubs).
- **No issues found** in the parser or GDI backend — both behave as documented.

### Team Session: Snapper CLI Implementation (2026-04-14)
- **Verification complete:** All 21 new CLI + GDI tests passing; fenster's implementation verified to spec
- **Session logging:** Team context captured in 5 orchestration logs + session summary
- **Status:** Test phase successful; CLI implementation validated and ready for integration
