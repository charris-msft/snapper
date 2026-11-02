package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"go/ast"
	"go/parser"
	"go/token"
	"image"
	"image/color"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"
)

// Compile the production CLI, substituting only the four Win32 boundary
// functions. Default-route tests must never capture the user's real desktop.
func buildFixtureCLI(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	boundaries := map[string]bool{
		"enumerateMonitors": false, "enumerateWindows": false,
		"captureMonitorRaw": false, "captureWindowRaw": false,
	}
	for _, name := range []string{"main.go", "monitor.go", "window.go", "capture.go", "output.go", "stitch.go", "go.mod"} {
		src, err := os.ReadFile(name)
		if err != nil {
			t.Fatal(err)
		}
		if strings.HasSuffix(name, ".go") {
			fset := token.NewFileSet()
			file, err := parser.ParseFile(fset, name, src, 0)
			if err != nil {
				t.Fatal(err)
			}
			var offsets []int
			for _, decl := range file.Decls {
				fn, ok := decl.(*ast.FuncDecl)
				if !ok {
					continue
				}
				if _, exists := boundaries[fn.Name.Name]; exists {
					boundaries[fn.Name.Name] = true
					offsets = append(offsets, fset.Position(fn.Name.Pos()).Offset)
				}
			}
			slices.Sort(offsets)
			slices.Reverse(offsets)
			for _, offset := range offsets {
				src = append(append(append([]byte{}, src[:offset]...), []byte("unusedWin32_")...), src[offset:]...)
			}
		}
		if err := os.WriteFile(filepath.Join(dir, name), src, 0600); err != nil {
			t.Fatal(err)
		}
	}
	for name, replaced := range boundaries {
		if !replaced {
			t.Fatalf("refusing fixture launch: native boundary %s was not replaced", name)
		}
	}
	fixture, err := os.ReadFile(filepath.Join("testdata", "capture_fixture.go"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "capture_fixture.go"), fixture, 0600); err != nil {
		t.Fatal(err)
	}
	binary := filepath.Join(dir, "snapper-contract.exe")
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	cmd := exec.CommandContext(ctx, "go", "build", "-o", binary, ".")
	cmd.Dir = dir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("build fixture CLI: %v\n%s", err, out)
	}
	return binary
}

func runFixture(t *testing.T, binary, scenario string, args ...string) (string, string, int) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, binary, args...)
	for _, env := range os.Environ() {
		key, _, _ := strings.Cut(env, "=")
		if !strings.EqualFold(key, "SNAPPER_FIXTURE_SCENARIO") {
			cmd.Env = append(cmd.Env, env)
		}
	}
	cmd.Env = append(cmd.Env, "SNAPPER_FIXTURE_SCENARIO="+scenario)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	err := cmd.Run()
	if ctx.Err() != nil {
		t.Fatalf("CLI did not complete within 15 seconds: %v", ctx.Err())
	}
	code := 0
	if err != nil {
		exit, ok := err.(*exec.ExitError)
		if !ok {
			t.Fatal(err)
		}
		code = exit.ExitCode()
	}
	return stdout.String(), stderr.String(), code
}

func decodeRecord(t *testing.T, data string) outputRecord {
	t.Helper()
	var record outputRecord
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&record); err != nil {
		t.Fatalf("expected one JSON object: %v\n%s", err, data)
	}
	if !json.Valid([]byte(data)) {
		t.Fatalf("stdout contains content outside JSON: %s", data)
	}
	return record
}

func decodeImageRecord(t *testing.T, record outputRecord) image.Image {
	t.Helper()
	if record.File != "" || record.Image == "" {
		t.Fatalf("expected inline image, got %+v", record)
	}
	data, err := base64.StdEncoding.DecodeString(record.Image)
	if err != nil {
		t.Fatal(err)
	}
	img, format, err := image.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	wantFormat := record.Format
	if wantFormat == "jpg" {
		wantFormat = "jpeg"
	}
	if format != wantFormat || img.Bounds().Dx() != record.Width || img.Bounds().Dy() != record.Height {
		t.Fatalf("encoded image does not match JSON dimensions/format: %s %v, %+v", format, img.Bounds(), record)
	}
	return img
}

func TestCLIContract(t *testing.T) {
	binary := buildFixtureCLI(t)
	t.Run("default-is-all-monitors-JSON-not-GUI", func(t *testing.T) {
		for _, scenario := range []string{"", "one-monitor"} {
			stdout, stderr, code := runFixture(t, binary, scenario)
			if code != 0 || stderr != "" {
				t.Fatalf("exit=%d stderr=%q", code, stderr)
			}
			var records []outputRecord
			if err := json.Unmarshal([]byte(stdout), &records); err != nil {
				t.Fatalf("default must return an array: %v", err)
			}
			want := 2
			if scenario == "one-monitor" {
				want = 1
			}
			if len(records) != want {
				t.Fatalf("got %d monitors, want %d", len(records), want)
			}
			for i, record := range records {
				if record.Monitor != i+1 || record.Format != "png" || record.Title != "" || record.PID != 0 {
					t.Fatalf("default/order changed: %+v", record)
				}
				decodeImageRecord(t, record)
			}
		}
	})
	t.Run("single-monitor-and-format-aliases", func(t *testing.T) {
		for _, args := range [][]string{
			{"--monitor", "2"}, {"-m", "2"},
			{"--monitor", "2", "--format", "jpg", "--quality", "75"},
			{"-m", "2", "-f", "jpg", "-q", "100"},
		} {
			stdout, stderr, code := runFixture(t, binary, "", args...)
			if code != 0 || stderr != "" {
				t.Fatalf("%v: exit=%d stderr=%q", args, code, stderr)
			}
			record := decodeRecord(t, stdout)
			if record.Monitor != 2 || record.Width != 3 || record.Height != 2 {
				t.Fatalf("single monitor contract changed: %+v", record)
			}
			decodeImageRecord(t, record)
		}
	})
	t.Run("stitch-negative-origins-and-transparent-gaps", func(t *testing.T) {
		stdout, stderr, code := runFixture(t, binary, "", "--stitch")
		if code != 0 || stderr != "" {
			t.Fatalf("exit=%d stderr=%q", code, stderr)
		}
		record := decodeRecord(t, stdout)
		if record.Monitor != 0 || record.Width != 7 || record.Height != 4 {
			t.Fatalf("stitch bounds changed: %+v", record)
		}
		img := decodeImageRecord(t, record)
		for _, pixel := range []struct {
			x, y int
			want color.NRGBA
		}{
			{0, 1, color.NRGBA{230, 40, 60, 255}},
			{4, 0, color.NRGBA{20, 90, 210, 255}},
			{0, 0, color.NRGBA{}},
		} {
			if got := color.NRGBAModel.Convert(img.At(pixel.x, pixel.y)); got != pixel.want {
				t.Fatalf("pixel (%d,%d)=%v want=%v", pixel.x, pixel.y, got, pixel.want)
			}
		}
	})
	t.Run("window-first-substring-and-largest-PID", func(t *testing.T) {
		for _, args := range [][]string{{"-w", "dEmO"}, {"--window", "dEmO"}, {"--pid", "17"}} {
			stdout, stderr, code := runFixture(t, binary, "", args...)
			record := decodeRecord(t, stdout)
			wantTitle, wantWidth := "<Demo & Notes>", 3
			if args[0] == "--pid" {
				wantTitle, wantWidth = "Demo main", 6
			}
			if code != 0 || record.Monitor != 0 || record.PID != 17 || record.Title != wantTitle || record.Width != wantWidth {
				t.Fatalf("window contract: exit=%d record=%+v", code, record)
			}
			if !strings.HasPrefix(stderr, "Capturing window: "+wantTitle+" (PID 17, ") || !strings.Contains(stdout, wantTitle) {
				t.Fatalf("diagnostic/HTML escaping changed: stdout=%s stderr=%s", stdout, stderr)
			}
			decodeImageRecord(t, record)
		}
	})
	t.Run("list-windows-no-handle-no-capture", func(t *testing.T) {
		stdout, stderr, code := runFixture(t, binary, "capture-error", "--list-windows", "-f", "jpg", "-q", "100")
		if code != 0 || stderr != "" {
			t.Fatalf("exit=%d stderr=%q", code, stderr)
		}
		want := "[{\"title\":\"<Demo & Notes>\",\"pid\":17,\"x\":-3,\"y\":0,\"width\":3,\"height\":2},{\"title\":\"Demo main\",\"pid\":17,\"x\":0,\"y\":0,\"width\":6,\"height\":4},{\"title\":\"Other\",\"pid\":24,\"x\":4,\"y\":-1,\"width\":2,\"height\":2}]\n"
		if stdout != want {
			t.Fatalf("window JSON changed:\n%s", stdout)
		}
	})
	t.Run("file-output-is-isolated-and-diagnostics-stay-on-stderr", func(t *testing.T) {
		temp := t.TempDir()
		t.Setenv("TEMP", temp)
		t.Setenv("TMP", temp)
		for _, flag := range []string{"-o", "--output-file"} {
			stdout, stderr, code := runFixture(t, binary, "", "-m", "1", "-f", "jpg", flag)
			record := decodeRecord(t, stdout)
			if code != 0 || record.Image != "" || filepath.Dir(record.File) != temp || filepath.Ext(record.File) != ".jpg" {
				t.Fatalf("file contract: exit=%d record=%+v", code, record)
			}
			if stderr != "Wrote "+record.File+"\n" {
				t.Fatalf("file diagnostic changed: %q", stderr)
			}
			data, err := os.ReadFile(record.File)
			if err != nil {
				t.Fatal(err)
			}
			record.File, record.Image = "", base64.StdEncoding.EncodeToString(data)
			decodeImageRecord(t, record)
		}
	})
	t.Run("help-is-stderr-only-and-precedes-route-validation", func(t *testing.T) {
		for _, args := range [][]string{{"-h"}, {"--help", "--monitor", "1", "--stitch"}} {
			stdout, stderr, code := runFixture(t, binary, "capture-error", args...)
			if code != 0 || stdout != "" || !strings.Contains(stderr, "Output is JSON to stdout.") || !strings.Contains(stderr, "Capture every monitor as a separate image.") {
				t.Fatalf("help contract: exit=%d stdout=%s stderr=%s", code, stdout, stderr)
			}
		}
	})
	t.Run("argument-and-capture-errors", func(t *testing.T) {
		cases := []struct {
			args     []string
			scenario string
			code     int
			stderr   string
		}{
			{[]string{"--unknown"}, "", 1, "Error: unknown option \"--unknown\".\n"},
			{[]string{"--monitor"}, "", 1, "Error: --monitor requires a value.\n"},
			{[]string{"-m", "0"}, "", 1, "Error: invalid monitor number \"0\".\n"},
			{[]string{"-m", "3"}, "", 1, "Error: monitor 3 not found (only 2 monitors detected).\n"},
			{[]string{"--window"}, "", 1, "Error: --window requires a title.\n"},
			{[]string{"--pid"}, "", 1, "Error: --pid requires a value.\n"},
			{[]string{"--pid", "x"}, "", 1, "Error: invalid PID \"x\".\n"},
			{[]string{"--format"}, "", 1, "Error: --format requires a value (png or jpg).\n"},
			{[]string{"-f", "jpeg"}, "", 1, "Error: unsupported format \"jpeg\" (use png or jpg).\n"},
			{[]string{"--quality"}, "", 1, "Error: --quality requires a value (1-100).\n"},
			{[]string{"-q", "101"}, "", 1, "Error: invalid quality \"101\" (must be 1-100).\n"},
			{[]string{"-w", "Demo", "--pid", "17"}, "", 1, "Error: --window and --pid are mutually exclusive.\n"},
			{[]string{"-w", "Demo", "-m", "1"}, "", 1, "Error: --window/--pid cannot be combined with --monitor or --stitch.\n"},
			{[]string{"--list-windows", "-o"}, "", 1, "Error: --list-windows is standalone and cannot be combined with other options.\n"},
			{[]string{"--stitch", "-m", "1"}, "", 1, "Error: --stitch cannot be used together with --monitor.\n"},
			{nil, "monitor-error", 2, "Error enumerating monitors: fixture monitor failure\n"},
			{nil, "no-monitors", 2, "Error: no monitors found.\n"},
			{nil, "capture-error", 2, "Error capturing monitor 1: fixture capture failure\n"},
			{[]string{"--list-windows"}, "window-error", 2, "Error listing windows: fixture window failure\n"},
			{[]string{"-w", "absent"}, "", 2, "Error: no window found matching \"absent\"\n"},
			{[]string{"--pid", "99"}, "", 2, "Error: no window found for PID 99\n"},
			{[]string{"--pid", "17"}, "capture-error", 2, "Capturing window: Demo main (PID 17, 6x4)\nError capturing window: fixture capture failure\n"},
		}
		for _, c := range cases {
			stdout, stderr, code := runFixture(t, binary, c.scenario, c.args...)
			if code != c.code || stdout != "" || stderr != c.stderr {
				t.Errorf("%v/%s: exit=%d want=%d stdout=%q stderr=%q want=%q", c.args, c.scenario, code, c.code, stdout, stderr, c.stderr)
			}
		}
	})
}
