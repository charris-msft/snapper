package main

import (
	"fmt"
	"image"
	"image/draw"
)

type rawCapture struct {
	Mon   MonitorInfo
	Image *image.NRGBA
}

type stitchBounds struct {
	Width  int
	Height int
}

// stitchImagesRaw composites raw NRGBA captures into a single PNG (one encode, no decodes).
func stitchImagesRaw(captures []rawCapture) ([]byte, stitchBounds, error) {
	minX, minY := int(captures[0].Mon.Bounds.Left), int(captures[0].Mon.Bounds.Top)
	maxX, maxY := int(captures[0].Mon.Bounds.Right), int(captures[0].Mon.Bounds.Bottom)
	for _, c := range captures[1:] {
		if int(c.Mon.Bounds.Left) < minX {
			minX = int(c.Mon.Bounds.Left)
		}
		if int(c.Mon.Bounds.Top) < minY {
			minY = int(c.Mon.Bounds.Top)
		}
		if int(c.Mon.Bounds.Right) > maxX {
			maxX = int(c.Mon.Bounds.Right)
		}
		if int(c.Mon.Bounds.Bottom) > maxY {
			maxY = int(c.Mon.Bounds.Bottom)
		}
	}

	totalW := maxX - minX
	totalH := maxY - minY
	canvas := image.NewNRGBA(image.Rect(0, 0, totalW, totalH))

	for _, c := range captures {
		offsetX := int(c.Mon.Bounds.Left) - minX
		offsetY := int(c.Mon.Bounds.Top) - minY
		dst := image.Rect(offsetX, offsetY, offsetX+c.Mon.Width(), offsetY+c.Mon.Height())
		draw.Draw(canvas, dst, c.Image, image.Point{}, draw.Src)
	}

	pngData, err := encodePNG(canvas)
	if err != nil {
		return nil, stitchBounds{}, fmt.Errorf("encode stitched PNG: %v", err)
	}
	return pngData, stitchBounds{Width: totalW, Height: totalH}, nil
}
