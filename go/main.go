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
	listWindows := false
	windowTitle := ""
	windowPID := 0
	format := "png"
	quality := 85

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
		case "-w", "--window":
			i++
			if i >= len(args) {
				fmt.Fprintln(os.Stderr, "Error: --window requires a title.")
				os.Exit(1)
			}
			windowTitle = args[i]
		case "--pid":
			i++
			if i >= len(args) {
				fmt.Fprintln(os.Stderr, "Error: --pid requires a value.")
				os.Exit(1)
			}
			n, err := strconv.Atoi(args[i])
			if err != nil || n < 1 {
				fmt.Fprintf(os.Stderr, "Error: invalid PID %q.\n", args[i])
				os.Exit(1)
			}
			windowPID = n
		case "--list-windows":
			listWindows = true
		case "-f", "--format":
			i++
			if i >= len(args) {
				fmt.Fprintln(os.Stderr, "Error: --format requires a value (png or jpg).")
				os.Exit(1)
			}
			switch args[i] {
			case "png", "jpg":
				format = args[i]
			default:
				fmt.Fprintf(os.Stderr, "Error: unsupported format %q (use png or jpg).\n", args[i])
				os.Exit(1)
			}
		case "-q", "--quality":
			i++
			if i >= len(args) {
				fmt.Fprintln(os.Stderr, "Error: --quality requires a value (1-100).")
				os.Exit(1)
			}
			n, err := strconv.Atoi(args[i])
			if err != nil || n < 1 || n > 100 {
				fmt.Fprintf(os.Stderr, "Error: invalid quality %q (must be 1-100).\n", args[i])
				os.Exit(1)
			}
			quality = n
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

	// --- Validation ---
	if windowTitle != "" && windowPID > 0 {
		fmt.Fprintln(os.Stderr, "Error: --window and --pid are mutually exclusive.")
		os.Exit(1)
	}
	hasWindow := windowTitle != "" || windowPID > 0
	if hasWindow && (monitor > 0 || stitch) {
		fmt.Fprintln(os.Stderr, "Error: --window/--pid cannot be combined with --monitor or --stitch.")
		os.Exit(1)
	}
	if listWindows && (hasWindow || monitor > 0 || stitch || outputFile) {
		fmt.Fprintln(os.Stderr, "Error: --list-windows is standalone and cannot be combined with other options.")
		os.Exit(1)
	}
	if stitch && monitor > 0 {
		fmt.Fprintln(os.Stderr, "Error: --stitch cannot be used together with --monitor.")
		os.Exit(1)
	}

	// --- List windows ---
	if listWindows {
		windows, err := enumerateWindows()
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error listing windows: %v\n", err)
			os.Exit(2)
		}
		writeJSON(os.Stdout, windows)
		return
	}

	// --- Window capture ---
	if hasWindow {
		var win *WindowInfo
		var err error
		if windowTitle != "" {
			win, err = findWindowByTitle(windowTitle)
		} else {
			win, err = findWindowByPID(uint32(windowPID))
		}
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error: %v\n", err)
			os.Exit(2)
		}
		fmt.Fprintf(os.Stderr, "Capturing window: %s (PID %d, %dx%d)\n", win.Title, win.PID, win.Width, win.Height)
		raw, err := captureWindowRaw(win)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error capturing window: %v\n", err)
			os.Exit(2)
		}
		imgData, err := encodeImage(raw, format, quality)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error encoding image: %v\n", err)
			os.Exit(2)
		}
		result := buildResult(0, raw.Bounds().Dx(), raw.Bounds().Dy(), imgData, format, outputFile)
		result.Title = win.Title
		result.PID = win.PID
		writeJSON(os.Stdout, result)
		return
	}

	// --- Monitor capture ---
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
		// Single monitor
		if monitor > len(monitors) {
			fmt.Fprintf(os.Stderr, "Error: monitor %d not found (only %d monitors detected).\n", monitor, len(monitors))
			os.Exit(1)
		}
		mon := monitors[monitor-1]
		raw, err := captureMonitorRaw(mon)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error capturing monitor %d: %v\n", monitor, err)
			os.Exit(2)
		}
		imgData, err := encodeImage(raw, format, quality)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error encoding image: %v\n", err)
			os.Exit(2)
		}
		result := buildResult(monitor, mon.Width(), mon.Height(), imgData, format, outputFile)
		writeJSON(os.Stdout, result)
	} else if stitch {
		// Stitch all monitors
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
		canvas, bounds, err := stitchImagesRaw(captures)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error stitching images: %v\n", err)
			os.Exit(2)
		}
		imgData, err := encodeImage(canvas, format, quality)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error encoding image: %v\n", err)
			os.Exit(2)
		}
		result := buildResult(0, bounds.Width, bounds.Height, imgData, format, outputFile)
		writeJSON(os.Stdout, result)
	} else {
		// All monitors, no stitch
		type captureJob struct {
			index int
			data  []byte
			mon   MonitorInfo
			err   error
		}
		ch := make(chan captureJob, len(monitors))
		for i, mon := range monitors {
			go func(idx int, m MonitorInfo) {
				raw, err := captureMonitorRaw(m)
				var data []byte
				if err == nil {
					data, err = encodeImage(raw, format, quality)
				}
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
			results[job.index] = buildResult(job.index+1, job.mon.Width(), job.mon.Height(), job.data, format, outputFile)
		}
		writeJSON(os.Stdout, results)
	}
}

func printHelp() {
	const (
		reset  = "\033[0m"
		bold   = "\033[1m"
		dim    = "\033[2m"
		green  = "\033[32m"
		bgreen = "\033[92m"
		cyan   = "\033[36m"
		bcyan  = "\033[96m"
		yellow = "\033[33m"
		white  = "\033[97m"
	)

	fmt.Fprint(os.Stderr, "\n")
	fmt.Fprint(os.Stderr, bgreen+"           __"+reset+"\n")
	fmt.Fprint(os.Stderr, bgreen+"     .-'``  ``'-. "+reset+"\n")
	fmt.Fprint(os.Stderr, bgreen+"    /  "+yellow+"^ "+green+"    "+yellow+"^"+bgreen+"  \\"+reset+"      "+bold+white+"╔══════════════════════════════╗"+reset+"\n")
	fmt.Fprint(os.Stderr, bgreen+"   |  "+yellow+"(o)"+green+"  "+yellow+"(o)"+bgreen+"  |"+reset+"      "+bold+white+"║"+reset+"  "+bold+bcyan+"⚡ S N A P P E R"+reset+"              "+bold+white+"║"+reset+"\n")
	fmt.Fprint(os.Stderr, bgreen+"   |"+green+"    <"+yellow+"__"+green+">    "+bgreen+"|"+reset+"      "+bold+white+"║"+reset+"  "+dim+"Screen capture for AI agents"+reset+"  "+bold+white+"║"+reset+"\n")
	fmt.Fprint(os.Stderr, bgreen+"    \\  "+green+"'-..-'"+bgreen+"  /"+reset+"      "+bold+white+"╚══════════════════════════════╝"+reset+"\n")
	fmt.Fprint(os.Stderr, green+"  ___"+bgreen+"'-......-'"+green+"___"+reset+"\n")
	fmt.Fprint(os.Stderr, green+" /"+bgreen+"  "+green+"/============\\"+bgreen+"  "+green+"\\"+reset+"\n")
	fmt.Fprint(os.Stderr, green+"|"+bgreen+"  "+green+"|"+bcyan+"~~~~~~~~~~~~"+green+"|"+bgreen+"  "+green+"|"+reset+"\n")
	fmt.Fprint(os.Stderr, green+"|"+bgreen+"  "+green+"|"+bcyan+"~~~~~~~~~~~~"+green+"|"+bgreen+"  "+green+"|"+reset+"\n")
	fmt.Fprint(os.Stderr, green+" \\"+bgreen+"  "+green+"\\============/"+bgreen+"  "+green+"/"+reset+"\n")
	fmt.Fprint(os.Stderr, green+"  '-..__    __..-'"+reset+"\n")
	fmt.Fprint(os.Stderr, green+"       |"+bgreen+"|  |"+green+"|"+reset+"\n")
	fmt.Fprint(os.Stderr, green+"       |"+bgreen+"|  |"+green+"|"+reset+"\n")
	fmt.Fprint(os.Stderr, dim+green+"       ^^  ^^"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"USAGE"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+white+"snapper"+reset+" "+dim+"[options]"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"DESCRIPTION"+reset+"\n")
	fmt.Fprint(os.Stderr, "  Capture screenshots of monitors or app windows on Windows.\n")
	fmt.Fprint(os.Stderr, "  Output is JSON to stdout. Diagnostics go to stderr.\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"CAPTURE TARGET"+reset+" "+dim+"(pick one, or omit for all monitors)"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+bcyan+"-w"+reset+", "+bcyan+"--window"+reset+" "+dim+"<title>"+reset+"  Capture a window whose title contains <title>.\n")
	fmt.Fprint(os.Stderr, "                         Case-insensitive substring match.\n")
	fmt.Fprint(os.Stderr, "      "+bcyan+"--pid"+reset+" "+dim+"<N>"+reset+"         Capture the main window of process <N>.\n")
	fmt.Fprint(os.Stderr, "  "+bcyan+"-m"+reset+", "+bcyan+"--monitor"+reset+" "+dim+"<N>"+reset+"    Capture monitor N (1-based).\n")
	fmt.Fprint(os.Stderr, "      "+bcyan+"--stitch"+reset+"         Capture all monitors combined into one image.\n")
	fmt.Fprint(os.Stderr, "      "+bcyan+"--list-windows"+reset+"   List all visible windows as JSON (no capture).\n")
	fmt.Fprint(os.Stderr, "  "+dim+"(no target)"+reset+"           Capture every monitor as a separate image.\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"OUTPUT OPTIONS"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+bcyan+"-f"+reset+", "+bcyan+"--format"+reset+" "+dim+"<fmt>"+reset+"   "+bgreen+"png"+reset+" (default, lossless) or "+bgreen+"jpg"+reset+" (smaller, use for AI).\n")
	fmt.Fprint(os.Stderr, "  "+bcyan+"-q"+reset+", "+bcyan+"--quality"+reset+" "+dim+"<1-100>"+reset+" JPEG quality (default 85). Ignored for png.\n")
	fmt.Fprint(os.Stderr, "  "+bcyan+"-o"+reset+", "+bcyan+"--output-file"+reset+"    Write image to temp file; JSON returns file path.\n")
	fmt.Fprint(os.Stderr, "  "+dim+"(default)"+reset+"             Base64-encode image inline in JSON.\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"GENERAL"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+bcyan+"-h"+reset+", "+bcyan+"--help"+reset+"           Show this help and exit.\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"EXAMPLES"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+dim+"# See what apps are running"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+white+"snapper --list-windows"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")
	fmt.Fprint(os.Stderr, "  "+dim+"# Capture a specific app window as JPEG"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+white+"snapper -w \"Visual Studio Code\" -f jpg"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")
	fmt.Fprint(os.Stderr, "  "+dim+"# Capture a window by its process ID"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+white+"snapper --pid 1234 -f jpg -q 75"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")
	fmt.Fprint(os.Stderr, "  "+dim+"# Capture primary monitor, save to temp file"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+white+"snapper -m 1 -o"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")
	fmt.Fprint(os.Stderr, "  "+dim+"# Capture all monitors stitched into one image"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+white+"snapper --stitch -f jpg -q 80"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"JSON OUTPUT"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+dim+"Window/monitor capture returns:"+reset+"\n")
	fmt.Fprint(os.Stderr, "    {\"monitor\":N, \"width\":W, \"height\":H, \"format\":\"png|jpg\",\n")
	fmt.Fprint(os.Stderr, "     \"title\":\"...\", \"pid\":N, \"image\":\"base64...\" | \"file\":\"path\"}\n")
	fmt.Fprint(os.Stderr, "\n")
	fmt.Fprint(os.Stderr, "  "+dim+"All-monitors (no --stitch) returns a JSON array of the above."+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+dim+"--list-windows returns:"+reset+"\n")
	fmt.Fprint(os.Stderr, "    [{\"title\":\"...\", \"pid\":N, \"x\":X, \"y\":Y, \"width\":W, \"height\":H}, ...]"+reset+"\n")
	fmt.Fprint(os.Stderr, "\n")

	fmt.Fprint(os.Stderr, bold+yellow+"EXIT CODES"+reset+"\n")
	fmt.Fprint(os.Stderr, "  "+bgreen+"0"+reset+"  Success\n")
	fmt.Fprint(os.Stderr, "  "+yellow+"1"+reset+"  Argument error\n")
	fmt.Fprint(os.Stderr, "  "+yellow+"2"+reset+"  Capture error\n")
	fmt.Fprint(os.Stderr, "\n")
}
