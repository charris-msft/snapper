package main

import (
	"errors"
	"image"
	"image/color"
	"os"
)

func enumerateMonitors() ([]MonitorInfo, error) {
	switch os.Getenv("SNAPPER_FIXTURE_SCENARIO") {
	case "monitor-error":
		return nil, errors.New("fixture monitor failure")
	case "no-monitors":
		return nil, nil
	case "one-monitor":
		return []MonitorInfo{{Bounds: RECT{-4, 0, 0, 3}, Index: 1}}, nil
	}
	return []MonitorInfo{
		{Bounds: RECT{-4, 0, 0, 3}, Index: 1},
		{Bounds: RECT{0, -1, 3, 1}, Index: 2},
	}, nil
}

func enumerateWindows() ([]WindowInfo, error) {
	if os.Getenv("SNAPPER_FIXTURE_SCENARIO") == "window-error" {
		return nil, errors.New("fixture window failure")
	}
	return []WindowInfo{
		{Handle: 1, Title: "<Demo & Notes>", PID: 17, X: -3, Y: 0, Width: 3, Height: 2},
		{Handle: 2, Title: "Demo main", PID: 17, X: 0, Y: 0, Width: 6, Height: 4},
		{Handle: 3, Title: "Other", PID: 24, X: 4, Y: -1, Width: 2, Height: 2},
	}, nil
}

func captureMonitorRaw(mon MonitorInfo) (*image.NRGBA, error) {
	if os.Getenv("SNAPPER_FIXTURE_SCENARIO") == "capture-error" && mon.Index == 1 {
		return nil, errors.New("fixture capture failure")
	}
	pixel := color.NRGBA{R: 230, G: 40, B: 60, A: 255}
	if mon.Index == 2 {
		pixel = color.NRGBA{R: 20, G: 90, B: 210, A: 255}
	}
	return fixtureImage(mon.Width(), mon.Height(), pixel), nil
}

func captureWindowRaw(win *WindowInfo) (*image.NRGBA, error) {
	if os.Getenv("SNAPPER_FIXTURE_SCENARIO") == "capture-error" {
		return nil, errors.New("fixture capture failure")
	}
	return fixtureImage(win.Width, win.Height, color.NRGBA{R: 10, G: 150, B: 60, A: 255}), nil
}

func fixtureImage(width, height int, pixel color.NRGBA) *image.NRGBA {
	img := image.NewNRGBA(image.Rect(0, 0, width, height))
	for y := 0; y < height; y++ {
		for x := 0; x < width; x++ {
			img.SetNRGBA(x, y, pixel)
		}
	}
	return img
}
