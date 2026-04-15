package main

import (
	"fmt"
	"syscall"
	"unsafe"
)

var (
	user32                = syscall.NewLazyDLL("user32.dll")
	procEnumDisplayMonitors = user32.NewProc("EnumDisplayMonitors")
	procGetMonitorInfoW     = user32.NewProc("GetMonitorInfoW")
)

// RECT matches the Win32 RECT structure.
type RECT struct {
	Left, Top, Right, Bottom int32
}

// MonitorInfo holds the bounds for a single monitor.
type MonitorInfo struct {
	Bounds RECT
	Index  int // 1-based
}

func (m MonitorInfo) Width() int  { return int(m.Bounds.Right - m.Bounds.Left) }
func (m MonitorInfo) Height() int { return int(m.Bounds.Bottom - m.Bounds.Top) }

// MONITORINFOEX matches the Win32 MONITORINFOEXW structure.
type monitorInfoEx struct {
	CbSize    uint32
	RcMonitor RECT
	RcWork    RECT
	DwFlags   uint32
	SzDevice  [32]uint16
}

func enumerateMonitors() ([]MonitorInfo, error) {
	var monitors []MonitorInfo

	cb := syscall.NewCallback(func(hMonitor uintptr, hdcMonitor uintptr, lprcMonitor uintptr, dwData uintptr) uintptr {
		var mi monitorInfoEx
		mi.CbSize = uint32(unsafe.Sizeof(mi))
		r, _, _ := procGetMonitorInfoW.Call(hMonitor, uintptr(unsafe.Pointer(&mi)))
		if r == 0 {
			return 1 // continue enumeration even on failure
		}
		monitors = append(monitors, MonitorInfo{
			Bounds: mi.RcMonitor,
			Index:  len(monitors) + 1,
		})
		return 1 // TRUE to continue
	})

	r, _, err := procEnumDisplayMonitors.Call(0, 0, cb, 0)
	if r == 0 {
		return nil, fmt.Errorf("EnumDisplayMonitors failed: %v", err)
	}
	return monitors, nil
}
