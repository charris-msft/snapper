package main

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"
)

type outputRecord struct {
	Monitor int    `json:"monitor"`
	Width   int    `json:"width"`
	Height  int    `json:"height"`
	Format  string `json:"format"`
	Title   string `json:"title,omitempty"`
	PID     uint32 `json:"pid,omitempty"`
	Image   string `json:"image,omitempty"`
	File    string `json:"file,omitempty"`
}

func buildResult(monitorNum, w, h int, imgData []byte, format string, toFile bool) outputRecord {
	rec := outputRecord{
		Monitor: monitorNum,
		Width:   w,
		Height:  h,
		Format:  format,
	}
	ext := "png"
	if format == "jpg" {
		ext = "jpg"
	}
	if toFile {
		ts := time.Now().Format("20060102_150405")
		name := fmt.Sprintf("snapper_%d_%s.%s", monitorNum, ts, ext)
		path := filepath.Join(os.TempDir(), name)
		if err := os.WriteFile(path, imgData, 0644); err != nil {
			fmt.Fprintf(os.Stderr, "Error writing file %s: %v\n", path, err)
			os.Exit(2)
		}
		fmt.Fprintf(os.Stderr, "Wrote %s\n", path)
		rec.File = path
	} else {
		rec.Image = base64.StdEncoding.EncodeToString(imgData)
	}
	return rec
}

func writeJSON(w io.Writer, v interface{}) {
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		fmt.Fprintf(os.Stderr, "Error encoding JSON: %v\n", err)
		os.Exit(2)
	}
}
