package main

import (
	"fmt"
	"image"
	"strings"
	"syscall"
	"unsafe"
)

var (
	procEnumWindows              = user32.NewProc("EnumWindows")
	procGetWindowTextW           = user32.NewProc("GetWindowTextW")
	procGetWindowTextLengthW     = user32.NewProc("GetWindowTextLengthW")
	procIsWindowVisible          = user32.NewProc("IsWindowVisible")
	procGetWindowRect            = user32.NewProc("GetWindowRect")
	procGetWindowThreadProcessId = user32.NewProc("GetWindowThreadProcessId")
	procPrintWindow              = user32.NewProc("PrintWindow")

	dwmapi                      = syscall.NewLazyDLL("dwmapi.dll")
	procDwmGetWindowAttribute   = dwmapi.NewProc("DwmGetWindowAttribute")
)

const (
	dwmwaExtendedFrameBounds = 9
	pwRenderFullContent      = 0x00000002
)

// WindowInfo describes a visible top-level window.
type WindowInfo struct {
	Handle uintptr `json:"-"`
	Title  string  `json:"title"`
	PID    uint32  `json:"pid"`
	X      int     `json:"x"`
	Y      int     `json:"y"`
	Width  int     `json:"width"`
	Height int     `json:"height"`
}

// enumerateWindows returns all visible top-level windows with titles.
func enumerateWindows() ([]WindowInfo, error) {
	var windows []WindowInfo

	cb := syscall.NewCallback(func(hwnd uintptr, lParam uintptr) uintptr {
		visible, _, _ := procIsWindowVisible.Call(hwnd)
		if visible == 0 {
			return 1
		}

		length, _, _ := procGetWindowTextLengthW.Call(hwnd)
		if length == 0 {
			return 1
		}
		buf := make([]uint16, length+1)
		procGetWindowTextW.Call(hwnd, uintptr(unsafe.Pointer(&buf[0])), uintptr(length+1))
		title := syscall.UTF16ToString(buf)
		if title == "" {
			return 1
		}

		var pid uint32
		procGetWindowThreadProcessId.Call(hwnd, uintptr(unsafe.Pointer(&pid)))

		// Use DWM extended frame bounds (excludes invisible shadow borders on Win10/11).
		var rect RECT
		hr, _, _ := procDwmGetWindowAttribute.Call(
			hwnd,
			uintptr(dwmwaExtendedFrameBounds),
			uintptr(unsafe.Pointer(&rect)),
			uintptr(unsafe.Sizeof(rect)),
		)
		if hr != 0 {
			procGetWindowRect.Call(hwnd, uintptr(unsafe.Pointer(&rect)))
		}

		w := int(rect.Right - rect.Left)
		h := int(rect.Bottom - rect.Top)
		if w <= 0 || h <= 0 {
			return 1
		}

		windows = append(windows, WindowInfo{
			Handle: hwnd,
			Title:  title,
			PID:    pid,
			X:      int(rect.Left),
			Y:      int(rect.Top),
			Width:  w,
			Height: h,
		})
		return 1
	})

	r, _, err := procEnumWindows.Call(cb, 0)
	if r == 0 {
		return nil, fmt.Errorf("EnumWindows failed: %v", err)
	}
	return windows, nil
}

// findWindowByTitle returns the first window whose title contains the query (case-insensitive).
func findWindowByTitle(title string) (*WindowInfo, error) {
	windows, err := enumerateWindows()
	if err != nil {
		return nil, err
	}
	q := strings.ToLower(title)
	for i := range windows {
		if strings.Contains(strings.ToLower(windows[i].Title), q) {
			return &windows[i], nil
		}
	}
	return nil, fmt.Errorf("no window found matching %q", title)
}

// findWindowByPID returns the largest visible window belonging to the given process.
func findWindowByPID(pid uint32) (*WindowInfo, error) {
	windows, err := enumerateWindows()
	if err != nil {
		return nil, err
	}
	var best *WindowInfo
	bestArea := 0
	for i := range windows {
		if windows[i].PID == pid {
			area := windows[i].Width * windows[i].Height
			if area > bestArea {
				best = &windows[i]
				bestArea = area
			}
		}
	}
	if best == nil {
		return nil, fmt.Errorf("no window found for PID %d", pid)
	}
	return best, nil
}

// captureWindowRaw uses PrintWindow to capture a window's content.
func captureWindowRaw(win *WindowInfo) (*image.NRGBA, error) {
	hwnd := win.Handle

	// GetWindowRect for full window dimensions (PrintWindow renders the whole window).
	var fullRect RECT
	procGetWindowRect.Call(hwnd, uintptr(unsafe.Pointer(&fullRect)))
	w := int(fullRect.Right - fullRect.Left)
	h := int(fullRect.Bottom - fullRect.Top)
	if w <= 0 || h <= 0 {
		return nil, fmt.Errorf("window has zero size")
	}

	hdcWindow, _, _ := procGetDC.Call(hwnd)
	if hdcWindow == 0 {
		return nil, fmt.Errorf("GetDC failed for window")
	}
	defer procReleaseDC.Call(hwnd, hdcWindow)

	hdcMem, _, _ := procCreateCompatibleDC.Call(hdcWindow)
	if hdcMem == 0 {
		return nil, fmt.Errorf("CreateCompatibleDC failed")
	}
	defer procDeleteDC.Call(hdcMem)

	hBitmap, _, _ := procCreateCompatibleBitmap.Call(hdcWindow, uintptr(w), uintptr(h))
	if hBitmap == 0 {
		return nil, fmt.Errorf("CreateCompatibleBitmap failed")
	}
	defer procDeleteObject.Call(hBitmap)

	procSelectObject.Call(hdcMem, hBitmap)

	r, _, err := procPrintWindow.Call(hwnd, hdcMem, pwRenderFullContent)
	if r == 0 {
		return nil, fmt.Errorf("PrintWindow failed: %v", err)
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

	// Crop to the visible bounds (exclude DWM shadow borders).
	visX := win.X - int(fullRect.Left)
	visY := win.Y - int(fullRect.Top)
	visW := win.Width
	visH := win.Height

	// Clamp to bitmap bounds.
	if visX < 0 {
		visW += visX
		visX = 0
	}
	if visY < 0 {
		visH += visY
		visY = 0
	}
	if visX+visW > w {
		visW = w - visX
	}
	if visY+visH > h {
		visH = h - visY
	}

	img := image.NewNRGBA(image.Rect(0, 0, visW, visH))
	for row := 0; row < visH; row++ {
		srcOff := ((visY + row) * w * 4) + (visX * 4)
		dstOff := row * visW * 4
		for col := 0; col < visW; col++ {
			si := srcOff + col*4
			di := dstOff + col*4
			img.Pix[di+0] = pixelData[si+2] // R
			img.Pix[di+1] = pixelData[si+1] // G
			img.Pix[di+2] = pixelData[si+0] // B
			img.Pix[di+3] = 255             // A
		}
	}

	return img, nil
}
