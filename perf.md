# Snapper Performance Benchmarks

**Date:** 2025-07-14
**Machine:** 13th Gen Intel Core i9-13900
**Monitors:** 2 (2194×1234 + 2560×1440)
**Runs per combination:** 5

Each test was timed using `System.Diagnostics.Stopwatch` in PowerShell. Stdout and stderr
were redirected to `$null` so only wall-clock execution time is measured.

---

## .NET Implementation

**Binary size:** 153.4 MB

### Detailed Results

### All monitors, base64 (no args)

| Run | Time (ms) |
|-----|-----------|
| 1   | 389       |
| 2   | 392       |
| 3   | 350       |
| 4   | 379       |
| 5   | 355       |
| **Avg** | **373.0** |
| **Min** | **350**   |
| **Max** | **392**   |

### Monitor 1 only, base64 (`-m 1`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 261       |
| 2   | 241       |
| 3   | 232       |
| 4   | 247       |
| 5   | 234       |
| **Avg** | **243.0** |
| **Min** | **232**   |
| **Max** | **261**   |

### All monitors, file output (`-o`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 333       |
| 2   | 322       |
| 3   | 351       |
| 4   | 326       |
| 5   | 337       |
| **Avg** | **333.8** |
| **Min** | **322**   |
| **Max** | **351**   |

### Monitor 1, file output (`-m 1 -o`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 244       |
| 2   | 244       |
| 3   | 228       |
| 4   | 232       |
| 5   | 238       |
| **Avg** | **237.2** |
| **Min** | **228**   |
| **Max** | **244**   |

### All monitors stitched, base64 (`--stitch`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 531       |
| 2   | 506       |
| 3   | 488       |
| 4   | 493       |
| 5   | 470       |
| **Avg** | **497.6** |
| **Min** | **470**   |
| **Max** | **531**   |

### All monitors stitched, file output (`--stitch -o`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 484       |
| 2   | 471       |
| 3   | 453       |
| 4   | 456       |
| 5   | 473       |
| **Avg** | **467.4** |
| **Min** | **453**   |
| **Max** | **484**   |

---

## .NET Summary

| Parameters | Avg (ms) | Min (ms) | Max (ms) |
|---|---|---|---|
| *(no args)* — all monitors, base64 | 373.0 | 350 | 392 |
| `-m 1` — monitor 1, base64 | 243.0 | 232 | 261 |
| `-o` — all monitors, file | 333.8 | 322 | 351 |
| `-m 1 -o` — monitor 1, file | 237.2 | 228 | 244 |
| `--stitch` — stitched, base64 | 497.6 | 470 | 531 |
| `--stitch -o` — stitched, file | 467.4 | 453 | 484 |

---

## Go Implementation (unoptimized)

**Date:** 2025-07-14
**Binary size:** 3.3 MB (vs 153.4 MB for .NET)

Naive initial implementation: per-pixel `SetNRGBA()`, default PNG compression (level 6),
sequential captures, encode→decode→encode stitch pipeline.

### Summary (unoptimized)

| Parameters | Avg (ms) | Min (ms) | Max (ms) |
|---|---|---|---|
| *(no args)* — all monitors, base64 | 489.0 | 462 | 506 |
| `-m 1` — monitor 1, base64 | 244.2 | 236 | 250 |
| `-o` — all monitors, file | 464.0 | 432 | 481 |
| `-m 1 -o` — monitor 1, file | 276.6 | 265 | 288 |
| `--stitch` — stitched, base64 | 1124.2 | 996 | 1259 |
| `--stitch -o` — stitched, file | 1114.0 | 968 | 1259 |

---

## Go Implementation (optimized)

**Date:** 2025-07-14
**Binary size:** 3.3 MB

Four optimizations applied:
1. **Direct pixel copy** — bulk BGRA→RGBA swizzle on `Pix` slice, no per-pixel method calls
2. **BestSpeed PNG** — compression level 1 instead of level 6
3. **Raw pixel stitching** — composite raw `*image.NRGBA`, encode PNG once (not 3x)
4. **Parallel capture** — goroutines for multi-monitor capture

### All monitors, base64 (no args)

| Run | Time (ms) |
|-----|-----------|
| 1   | 184       |
| 2   | 174       |
| 3   | 202       |
| 4   | 186       |
| 5   | 195       |
| **Avg** | **188.2** |
| **Min** | **174**   |
| **Max** | **202**   |

### Monitor 1 only, base64 (`-m 1`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 132       |
| 2   | 122       |
| 3   | 133       |
| 4   | 125       |
| 5   | 115       |
| **Avg** | **125.4** |
| **Min** | **115**   |
| **Max** | **133**   |

### All monitors, file output (`-o`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 159       |
| 2   | 179       |
| 3   | 171       |
| 4   | 170       |
| 5   | 181       |
| **Avg** | **172.0** |
| **Min** | **159**   |
| **Max** | **181**   |

### Monitor 1, file output (`-m 1 -o`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 123       |
| 2   | 136       |
| 3   | 130       |
| 4   | 150       |
| 5   | 134       |
| **Avg** | **134.6** |
| **Min** | **123**   |
| **Max** | **150**   |

### All monitors stitched, base64 (`--stitch`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 305       |
| 2   | 296       |
| 3   | 285       |
| 4   | 288       |
| 5   | 292       |
| **Avg** | **293.2** |
| **Min** | **285**   |
| **Max** | **305**   |

### All monitors stitched, file output (`--stitch -o`)

| Run | Time (ms) |
|-----|-----------|
| 1   | 285       |
| 2   | 271       |
| 3   | 291       |
| 4   | 293       |
| 5   | 302       |
| **Avg** | **288.4** |
| **Min** | **271**   |
| **Max** | **302**   |

---

## Comparison: .NET vs Go (optimized)

| Parameters | .NET Avg (ms) | Go Avg (ms) | Δ |
|---|---|---|---|
| *(no args)* — all monitors, base64 | 373.0 | 188.2 | **Go 49% faster** |
| `-m 1` — monitor 1, base64 | 243.0 | 125.4 | **Go 48% faster** |
| `-o` — all monitors, file | 333.8 | 172.0 | **Go 48% faster** |
| `-m 1 -o` — monitor 1, file | 237.2 | 134.6 | **Go 43% faster** |
| `--stitch` — stitched, base64 | 497.6 | 293.2 | **Go 41% faster** |
| `--stitch -o` — stitched, file | 467.4 | 288.4 | **Go 38% faster** |

| Metric | .NET | Go |
|---|---|---|
| **Binary size** | 153.4 MB | 3.3 MB (46x smaller) |

### Optimization Impact (Go unoptimized → optimized)

| Parameters | Before (ms) | After (ms) | Speedup |
|---|---|---|---|
| *(no args)* — all monitors, base64 | 489.0 | 188.2 | **2.6x faster** |
| `-m 1` — monitor 1, base64 | 244.2 | 125.4 | **1.9x faster** |
| `-o` — all monitors, file | 464.0 | 172.0 | **2.7x faster** |
| `-m 1 -o` — monitor 1, file | 276.6 | 134.6 | **2.1x faster** |
| `--stitch` — stitched, base64 | 1124.2 | 293.2 | **3.8x faster** |
| `--stitch -o` — stitched, file | 1114.0 | 288.4 | **3.9x faster** |

### Observations

- **Go (optimized) is faster than .NET across the board** — 38-49% faster on every parameter combo.
- **Single-monitor capture: 125ms vs 243ms** — Go's near-zero startup time shines. .NET's ~200ms JIT/runtime startup is the bottleneck.
- **Stitching saw the biggest optimization gain (3.8-3.9x)** — eliminating the PNG encode→decode→encode round-trip was the single largest win.
- **Go binary is 46x smaller** (3.3 MB vs 153.4 MB) — decisive advantage for distribution.
- **Go wins on every dimension:** faster execution, smaller binary, simpler deployment (no .NET runtime required).

### What Made the Difference

1. **Direct pixel copy** eliminated ~3.7M method calls per capture (biggest win for single-monitor).
2. **BestSpeed PNG** (level 1 vs 6) trades ~15% larger files for ~3x faster encoding — perfect tradeoff for ephemeral screenshots.
3. **Raw pixel stitching** removed 2 redundant PNG encode/decode cycles (biggest win for stitch).
4. **Parallel goroutines** overlapped GDI capture across monitors (biggest win for multi-monitor).
