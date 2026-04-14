package main

import (
	"fmt"
	"image"
	"os"
	"strconv"
)

func main() {
	monitor := 0       // 0 = all monitors
	outputFile := false // write to temp file instead of base64
	stitch := false
	showHelp := false

	args := os.Args[1:]
	for i := 0; i < len(args); i++ {
		a := args[i]
		switch a {
		case "-h", "--help":
			showHelp = true
		case "-m", "--monitor":
			i++
			if i >= len(args) {
				fmt.Fprintln(os.Stderr, "Error: --monitor requires a value.")
				os.Exit(1)
			}
			n, err := strconv.Atoi(args[i])
			if err != nil || n < 1 {
				fmt.Fprintf(os.Stderr, "Error: invalid monitor number %q.\n", args[i])
				os.Exit(1)
			}
			monitor = n
		case "-o", "--output-file":
			outputFile = true
		case "--stitch":
			stitch = true
		default:
			fmt.Fprintf(os.Stderr, "Error: unknown option %q.\n", a)
			os.Exit(1)
		}
	}

	if showHelp {
		printHelp()
		os.Exit(0)
	}

	if stitch && monitor > 0 {
		fmt.Fprintln(os.Stderr, "Error: --stitch cannot be used together with --monitor.")
		os.Exit(1)
	}

	monitors, err := enumerateMonitors()
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error enumerating monitors: %v\n", err)
		os.Exit(2)
	}
	if len(monitors) == 0 {
		fmt.Fprintln(os.Stderr, "Error: no monitors found.")
		os.Exit(2)
	}

	if monitor > 0 {
		// Single monitor mode
		if monitor > len(monitors) {
			fmt.Fprintf(os.Stderr, "Error: monitor %d not found (only %d monitors detected).\n", monitor, len(monitors))
			os.Exit(1)
		}
		mon := monitors[monitor-1]
		pngData, err := captureMonitor(mon)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error capturing monitor %d: %v\n", monitor, err)
			os.Exit(2)
		}
		result := buildResult(monitor, mon.Width(), mon.Height(), pngData, outputFile)
		writeJSON(os.Stdout, result)
	} else if stitch {
		// Stitch all monitors — capture raw images in parallel, stitch, encode once
		type rawJob struct {
			index int
			img   *image.NRGBA
			mon   MonitorInfo
			err   error
		}
		ch := make(chan rawJob, len(monitors))
		for i, mon := range monitors {
			go func(idx int, m MonitorInfo) {
				img, err := captureMonitorRaw(m)
				ch <- rawJob{index: idx, img: img, mon: m, err: err}
			}(i, mon)
		}
		captures := make([]rawCapture, len(monitors))
		for range monitors {
			job := <-ch
			if job.err != nil {
				fmt.Fprintf(os.Stderr, "Error capturing monitor %d: %v\n", job.index+1, job.err)
				os.Exit(2)
			}
			captures[job.index] = rawCapture{Mon: job.mon, Image: job.img}
		}
		stitchedPNG, bounds, err := stitchImagesRaw(captures)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error stitching images: %v\n", err)
			os.Exit(2)
		}
		result := buildResult(0, bounds.Width, bounds.Height, stitchedPNG, outputFile)
		writeJSON(os.Stdout, result)
	} else {
		// All monitors, no stitch — capture in parallel
		type captureJob struct {
			index int
			data  []byte
			mon   MonitorInfo
			err   error
		}
		ch := make(chan captureJob, len(monitors))
		for i, mon := range monitors {
			go func(idx int, m MonitorInfo) {
				data, err := captureMonitor(m)
				ch <- captureJob{index: idx, data: data, mon: m, err: err}
			}(i, mon)
		}
		results := make([]outputRecord, len(monitors))
		for range monitors {
			job := <-ch
			if job.err != nil {
				fmt.Fprintf(os.Stderr, "Error capturing monitor %d: %v\n", job.index+1, job.err)
				os.Exit(2)
			}
			results[job.index] = buildResult(job.index+1, job.mon.Width(), job.mon.Height(), job.data, outputFile)
		}
		writeJSON(os.Stdout, results)
	}
}

func printHelp() {
	fmt.Print(`Usage: snapper [options]

Capture screenshots on Windows.

Options:
  -m, --monitor <N>   Monitor number (1-based). Omit to capture all monitors.
  -o, --output-file   Write PNG to temp file(s) instead of base64 to stdout.
      --stitch        Combine all monitors into one image (all-monitor mode only).
  -h, --help          Show this help text and exit.

Exit codes:
  0  Success
  1  Argument error
  2  Capture error
`)
}
