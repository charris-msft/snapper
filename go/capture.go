package main

import (
	"bytes"
	"fmt"
	"image"
	"image/jpeg"
	"image/png"
	"syscall"
	"unsafe"
)

var (
	gdi32                      = syscall.NewLazyDLL("gdi32.dll")
	procGetDC                  = user32.NewProc("GetDC")
	procReleaseDC              = user32.NewProc("ReleaseDC")
	procCreateCompatibleDC     = gdi32.NewProc("CreateCompatibleDC")
	procCreateCompatibleBitmap = gdi32.NewProc("CreateCompatibleBitmap")
	procSelectObject           = gdi32.NewProc("SelectObject")
	procBitBlt                 = gdi32.NewProc("BitBlt")
	procGetDIBits              = gdi32.NewProc("GetDIBits")
	procDeleteDC               = gdi32.NewProc("DeleteDC")
	procDeleteObject           = gdi32.NewProc("DeleteObject")
)

const (
	srccopy      = 0x00CC0020
	biRgb        = 0
	dibRgbColors = 0
)

// BITMAPINFOHEADER matches the Win32 structure.
type bitmapInfoHeader struct {
	BiSize          uint32
	BiWidth         int32
	BiHeight        int32
	BiPlanes        uint16
	BiBitCount      uint16
	BiCompression   uint32
	BiSizeImage     uint32
	BiXPelsPerMeter int32
	BiYPelsPerMeter int32
	BiClrUsed       uint32
	BiClrImportant  uint32
}

// captureMonitorRaw captures a monitor and returns the raw NRGBA image (no PNG encoding).
func captureMonitorRaw(mon MonitorInfo) (*image.NRGBA, error) {
	w := mon.Width()
	h := mon.Height()
	x := int(mon.Bounds.Left)
	y := int(mon.Bounds.Top)

	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return nil, fmt.Errorf("GetDC failed")
	}
	defer procReleaseDC.Call(0, hdcScreen)

	hdcMem, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdcMem == 0 {
		return nil, fmt.Errorf("CreateCompatibleDC failed")
	}
	defer procDeleteDC.Call(hdcMem)

	hBitmap, _, _ := procCreateCompatibleBitmap.Call(hdcScreen, uintptr(w), uintptr(h))
	if hBitmap == 0 {
		return nil, fmt.Errorf("CreateCompatibleBitmap failed")
	}
	defer procDeleteObject.Call(hBitmap)

	procSelectObject.Call(hdcMem, hBitmap)

	r, _, err := procBitBlt.Call(
		hdcMem, 0, 0, uintptr(w), uintptr(h),
		hdcScreen, uintptr(x), uintptr(y),
		srccopy,
	)
	if r == 0 {
		return nil, fmt.Errorf("BitBlt failed: %v", err)
	}

	bmi := bitmapInfoHeader{
		BiSize:        uint32(unsafe.Sizeof(bitmapInfoHeader{})),
		BiWidth:       int32(w),
		BiHeight:      -int32(h), // top-down
		BiPlanes:      1,
		BiBitCount:    32,
		BiCompression: biRgb,
	}

	pixelData := make([]byte, w*4*h)

	r, _, err = procGetDIBits.Call(
		hdcMem, hBitmap,
		0, uintptr(h),
		uintptr(unsafe.Pointer(&pixelData[0])),
		uintptr(unsafe.Pointer(&bmi)),
		dibRgbColors,
	)
	if r == 0 {
		return nil, fmt.Errorf("GetDIBits failed: %v", err)
	}

	// Direct BGRA → RGBA swizzle on the Pix slice (no per-pixel method calls)
	img := image.NewNRGBA(image.Rect(0, 0, w, h))
	pix := img.Pix
	for i := 0; i < len(pixelData); i += 4 {
		pix[i+0] = pixelData[i+2] // R
		pix[i+1] = pixelData[i+1] // G
		pix[i+2] = pixelData[i+0] // B
		pix[i+3] = 255            // A (GDI returns 0; force opaque)
	}

	return img, nil
}

// captureMonitor captures a monitor and returns encoded image bytes.
func captureMonitor(mon MonitorInfo, format string, quality int) ([]byte, error) {
	img, err := captureMonitorRaw(mon)
	if err != nil {
		return nil, err
	}
	return encodeImage(img, format, quality)
}

// encodeImage encodes an image to the given format. For JPEG, quality (1-100) controls compression.
func encodeImage(img image.Image, format string, quality int) ([]byte, error) {
	var buf bytes.Buffer
	switch format {
	case "jpg":
		if err := jpeg.Encode(&buf, img, &jpeg.Options{Quality: quality}); err != nil {
			return nil, fmt.Errorf("JPEG encode failed: %v", err)
		}
	default:
		encoder := &png.Encoder{CompressionLevel: png.BestSpeed}
		if err := encoder.Encode(&buf, img); err != nil {
			return nil, fmt.Errorf("PNG encode failed: %v", err)
		}
	}
	return buf.Bytes(), nil
}
