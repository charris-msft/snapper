use crate::model::*;
use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use anyhow::{ensure, Context, Result};
use image::{Pixel, Rgba, RgbaImage};
use std::time::{Duration, Instant};
use tiny_skia::{
    Color, FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, StrokeDash, Transform,
};

pub struct Renderer {
    font: FontArc,
}

#[derive(Debug)]
pub struct RenderCancelled;
impl std::fmt::Display for RenderCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Render superseded by a newer edit")
    }
}
impl std::error::Error for RenderCancelled {}
struct RenderControl<'a> {
    cancelled: &'a dyn Fn() -> bool,
    deadline: Instant,
}
impl RenderControl<'_> {
    fn check(&self) -> Result<()> {
        if (self.cancelled)() {
            return Err(RenderCancelled.into());
        }
        ensure!(Instant::now()<self.deadline,"Image render exceeded 30 seconds. Undo the expensive operation or reduce the image/selection size; the original is unchanged.");
        Ok(())
    }
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let definitions = eframe::egui::FontDefinitions::default();
        let data = definitions
            .font_data
            .get("Ubuntu-Light")
            .or_else(|| definitions.font_data.values().next())
            .context("No bundled text font")?;
        let font =
            FontArc::try_from_vec(data.font.to_vec()).context("Loading bundled annotation font")?;
        Ok(Self { font })
    }
    pub fn render(&self, original: &RgbaImage, document: &Document) -> Result<RgbaImage> {
        self.render_cancellable(original, document, &|| false)
    }
    pub fn render_cancellable(
        &self,
        original: &RgbaImage,
        document: &Document,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RgbaImage> {
        let control = RenderControl {
            cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        };
        control.check()?;
        document.validate()?;
        ensure!(
            (original.width(), original.height()) == (document.width, document.height),
            "Original/document dimensions disagree."
        );
        let mut image = original.clone();
        if document.grayscale {
            let width = image.width() as usize;
            for (index, p) in image.pixels_mut().enumerate() {
                if index % width == 0 {
                    control.check()?;
                }
                let gray = (0.2126 * f32::from(p[0])
                    + 0.7152 * f32::from(p[1])
                    + 0.0722 * f32::from(p[2]))
                .round() as u8;
                p[0] = gray;
                p[1] = gray;
                p[2] = gray;
            }
        }
        let mut overlay = Pixmap::new(document.width, document.height)
            .context("Allocating bounded annotation surface")?;
        for layer in &document.layers {
            control.check()?;
            match layer.tool {
                Tool::Fill | Tool::Replace | Tool::Blur | Tool::Pixelate | Tool::Eraser => {
                    composite(&mut image, &mut overlay, &control)?;
                    raster_effect(&mut image, layer, &control)?;
                }
                Tool::CutHorizontal | Tool::CutVertical | Tool::CutJagged => {
                    composite(&mut image, &mut overlay, &control)?;
                    image = cutout(&image, layer, &control)?;
                    overlay = Pixmap::new(image.width(), image.height())
                        .context("Allocating cutout surface")?;
                }
                Tool::Select => {}
                _ => {
                    if layer.style.shadow {
                        self.vector(&mut overlay, layer, true);
                    }
                    self.vector(&mut overlay, layer, false);
                }
            }
        }
        if document.border > 0 {
            let b = document.border as f32;
            if let Some(rect) = Rect::from_xywh(
                b / 2.0,
                b / 2.0,
                image.width() as f32 - b,
                image.height() as f32 - b,
            ) {
                let path = PathBuilder::from_rect(rect);
                let p = paint(document.border_color, 1.0);
                overlay.stroke_path(
                    &path,
                    &p,
                    &Stroke {
                        width: b,
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
        }
        composite(&mut image, &mut overlay, &control)?;
        control.check()?;
        Ok(image)
    }

    fn vector(&self, pixmap: &mut Pixmap, a: &Annotation, shadow: bool) {
        let (x0, y0, x1, y1) = a.bounds();
        let offset = if shadow { 3.0 } else { 0.0 };
        let transform = Transform::from_translate(offset, offset);
        let color = if shadow { [0, 0, 0, 85] } else { a.style.color };
        let fill = if shadow { [0, 0, 0, 70] } else { a.style.fill };
        let p = paint(color, a.style.opacity);
        let fill_p = paint(fill, a.style.opacity);
        let width = a.style.width;
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            dash: match a.style.line {
                LineStyle::Solid => None,
                LineStyle::Dashed => StrokeDash::new(vec![width * 3.0, width * 2.0], 0.0),
                LineStyle::Dotted => StrokeDash::new(vec![width * 0.1, width * 2.0], 0.0),
            },
            ..Default::default()
        };
        match a.tool {
            Tool::Arrow | Tool::Line => {
                let mut b = PathBuilder::new();
                b.move_to(a.a.x, a.a.y);
                b.line_to(a.b.x, a.b.y);
                if let Some(path) = b.finish() {
                    pixmap.stroke_path(&path, &p, &stroke, transform, None);
                }
                if a.tool == Tool::Arrow {
                    draw_end(
                        pixmap,
                        a.a,
                        a.b,
                        a.style.start,
                        width * a.style.start_size,
                        &p,
                        transform,
                    );
                    draw_end(
                        pixmap,
                        a.b,
                        a.a,
                        a.style.end,
                        width * a.style.end_size,
                        &p,
                        transform,
                    );
                }
            }
            Tool::Rectangle | Tool::Ellipse | Tool::Highlight => {
                if let Some(rect) = Rect::from_xywh(x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0))
                {
                    let path = if a.tool == Tool::Ellipse {
                        PathBuilder::from_oval(rect)
                    } else {
                        Some(PathBuilder::from_rect(rect))
                    };
                    let Some(path) = path else {
                        return;
                    };
                    if a.tool == Tool::Highlight {
                        pixmap.fill_path(
                            &path,
                            &paint(color, a.style.opacity * 0.25),
                            FillRule::Winding,
                            transform,
                            None,
                        );
                    } else {
                        if a.style.filled {
                            pixmap.fill_path(&path, &fill_p, FillRule::Winding, transform, None);
                        }
                        pixmap.stroke_path(&path, &p, &stroke, transform, None);
                    }
                }
            }
            Tool::Callout => {
                let mut b = PathBuilder::new();
                let tail = (x1 - x0).min(60.0);
                b.move_to(x0, y0);
                b.line_to(x1, y0);
                b.line_to(x1, y1);
                b.line_to(x0 + tail, y1);
                b.line_to(x0 + tail * 0.3, y1 + tail * 0.35);
                b.line_to(x0 + tail * 0.4, y1);
                b.line_to(x0, y1);
                b.close();
                if let Some(path) = b.finish() {
                    pixmap.fill_path(&path, &fill_p, FillRule::Winding, transform, None);
                    pixmap.stroke_path(&path, &p, &stroke, transform, None);
                }
                self.text(
                    pixmap,
                    &a.text,
                    x0 + 12.0 + offset,
                    y0 + 8.0 + offset,
                    (x1 - x0 - 24.0).max(1.0),
                    (y1 - y0 - 16.0).max(1.0),
                    a.style.font_size,
                    color,
                    a.style.opacity,
                );
            }
            Tool::Text => {
                self.text(
                    pixmap,
                    &a.text,
                    x0 + offset,
                    y0 + offset,
                    (x1 - x0).max(1.0),
                    (y1 - y0).max(1.0),
                    a.style.font_size,
                    color,
                    a.style.opacity,
                );
            }
            Tool::Stamp => {
                let cx = (x0 + x1) / 2.0;
                let cy = (y0 + y1) / 2.0;
                let r = ((x1 - x0).min(y1 - y0) / 2.0).max(6.0);
                let mut b = PathBuilder::new();
                match a.style.stamp {
                    Stamp::Check => {
                        b.move_to(cx - r * 0.7, cy);
                        b.line_to(cx - r * 0.15, cy + r * 0.5);
                        b.line_to(cx + r * 0.75, cy - r * 0.6);
                    }
                    Stamp::Cross => {
                        b.move_to(cx - r * 0.6, cy - r * 0.6);
                        b.line_to(cx + r * 0.6, cy + r * 0.6);
                        b.move_to(cx + r * 0.6, cy - r * 0.6);
                        b.line_to(cx - r * 0.6, cy + r * 0.6);
                    }
                    Stamp::Star => {
                        for i in 0..10 {
                            let angle =
                                i as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
                            let radius = if i % 2 == 0 { r } else { r * 0.42 };
                            let x = cx + radius * angle.cos();
                            let y = cy + radius * angle.sin();
                            if i == 0 {
                                b.move_to(x, y);
                            } else {
                                b.line_to(x, y);
                            }
                        }
                        b.close();
                    }
                    Stamp::Number => {
                        if let Some(path) = Rect::from_xywh(cx - r, cy - r, r * 2.0, r * 2.0)
                            .and_then(PathBuilder::from_oval)
                        {
                            pixmap.fill_path(&path, &p, FillRule::Winding, transform, None);
                        }
                        let number = if a.text.trim().is_empty() {
                            "1"
                        } else {
                            &a.text
                        };
                        self.text(
                            pixmap,
                            number,
                            cx - r * 0.45 + offset,
                            cy - r * 0.65 + offset,
                            r * 1.6,
                            r * 1.8,
                            r * 1.3,
                            [255, 255, 255, 255],
                            a.style.opacity,
                        );
                    }
                }
                if let Some(path) = b.finish() {
                    if a.style.stamp == Stamp::Star {
                        pixmap.fill_path(&path, &p, FillRule::Winding, transform, None);
                    } else {
                        pixmap.stroke_path(
                            &path,
                            &p,
                            &Stroke {
                                width: width.max(r * 0.13),
                                line_cap: LineCap::Round,
                                ..Default::default()
                            },
                            transform,
                            None,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        size: f32,
        color: [u8; 4],
        opacity: f32,
    ) {
        let scaled = self.font.as_scaled(PxScale::from(size));
        let mut pen = point(x, y + scaled.ascent());
        let line_height = scaled.height() + size * 0.18;
        let mut previous = None;
        for ch in text.chars() {
            if ch == '\n' {
                pen.x = x;
                pen.y += line_height;
                previous = None;
                continue;
            }
            let mut glyph = scaled.scaled_glyph(ch);
            if let Some(prev) = previous {
                pen.x += scaled.kern(prev, glyph.id);
            }
            let advance = scaled.h_advance(glyph.id);
            if pen.x + advance > x + width && pen.x > x {
                pen.x = x;
                pen.y += line_height;
            }
            if pen.y - scaled.ascent() > y + height - line_height * 0.7 {
                break;
            }
            glyph.position = pen;
            if let Some(outlined) = self.font.outline_glyph(glyph.clone()) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    if px >= 0
                        && py >= 0
                        && px < pixmap.width() as i32
                        && py < pixmap.height() as i32
                    {
                        let index = (py as usize * pixmap.width() as usize + px as usize) * 4;
                        let data = &mut pixmap.data_mut()[index..index + 4];
                        let alpha = (f32::from(color[3]) * opacity * coverage).round() as u32;
                        for c in 0..3 {
                            data[c] = ((u32::from(color[c]) * alpha
                                + u32::from(data[c]) * (255 - alpha)
                                + 127)
                                / 255) as u8;
                        }
                        data[3] = (alpha + (u32::from(data[3]) * (255 - alpha) + 127) / 255)
                            .min(255) as u8;
                    }
                });
            }
            pen.x += advance;
            previous = Some(glyph.id);
        }
    }
}

fn paint(color: [u8; 4], opacity: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(
        color[0],
        color[1],
        color[2],
        (f32::from(color[3]) * opacity).round() as u8,
    );
    p.anti_alias = true;
    p
}
fn draw_end(
    pixmap: &mut Pixmap,
    tip: Point,
    other: Point,
    end: EndStyle,
    size: f32,
    paint: &Paint<'_>,
    transform: Transform,
) {
    let dx = tip.x - other.x;
    let dy = tip.y - other.y;
    let len = dx.hypot(dy);
    if len < 0.01 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let path: Option<Path> = match end {
        EndStyle::None => None,
        EndStyle::Arrow => {
            let mut b = PathBuilder::new();
            b.move_to(tip.x, tip.y);
            b.line_to(
                tip.x - ux * size - uy * size * 0.48,
                tip.y - uy * size + ux * size * 0.48,
            );
            b.line_to(
                tip.x - ux * size + uy * size * 0.48,
                tip.y - uy * size - ux * size * 0.48,
            );
            b.close();
            b.finish()
        }
        EndStyle::Circle => Rect::from_xywh(tip.x - size / 2.0, tip.y - size / 2.0, size, size)
            .and_then(PathBuilder::from_oval),
        EndStyle::Square => Rect::from_xywh(tip.x - size / 2.0, tip.y - size / 2.0, size, size)
            .map(PathBuilder::from_rect),
    };
    if let Some(path) = path {
        pixmap.fill_path(&path, paint, FillRule::Winding, transform, None);
    }
}

fn composite(
    image: &mut RgbaImage,
    overlay: &mut Pixmap,
    control: &RenderControl<'_>,
) -> Result<()> {
    let width = image.width() as usize;
    for (index, (dst, src)) in image
        .pixels_mut()
        .zip(overlay.data().chunks_exact(4))
        .enumerate()
    {
        if index % width == 0 {
            control.check()?;
        }
        if src[3] == 0 {
            continue;
        }
        let a = u32::from(src[3]);
        let mut straight = [0u8; 4];
        straight[3] = src[3];
        for c in 0..3 {
            straight[c] = ((u32::from(src[c]) * 255 + a / 2) / a).min(255) as u8;
        }
        dst.blend(&Rgba(straight));
    }
    overlay.fill(Color::TRANSPARENT);
    Ok(())
}
fn similar(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    (0..3).all(|c| a[c].abs_diff(b[c]) <= tolerance) && a[3].abs_diff(b[3]) <= tolerance
}
fn replacement(old: [u8; 4], new: [u8; 4], opacity: f32) -> Rgba<u8> {
    let mut out = [0; 4];
    for c in 0..4 {
        out[c] = (f32::from(old[c]) * (1.0 - opacity) + f32::from(new[c]) * opacity).round() as u8;
    }
    Rgba(out)
}
fn raster_effect(image: &mut RgbaImage, a: &Annotation, control: &RenderControl<'_>) -> Result<()> {
    let (x0, y0, x1, y1) = a.bounds();
    let x0 = (x0.max(0.0) as u32).min(image.width());
    let y0 = (y0.max(0.0) as u32).min(image.height());
    let x1 = (x1.max(0.0).ceil() as u32).min(image.width());
    let y1 = (y1.max(0.0).ceil() as u32).min(image.height());
    match a.tool {
        Tool::Eraser => {
            let points = if a.path.is_empty() {
                vec![a.a, a.b]
            } else {
                a.path.clone()
            };
            let radius = a.style.width / 2.0;
            let mut mask = Pixmap::new(image.width(), image.height())
                .context("Allocating bounded eraser mask")?;
            let mut b = PathBuilder::new();
            if let Some(first) = points.first() {
                b.move_to(first.x, first.y);
            }
            for point in points.iter().skip(1) {
                b.line_to(point.x, point.y);
            }
            if points.len() < 2 || points.iter().all(|p| *p == points[0]) {
                if let Some(first) = points.first() {
                    if let Some(path) = Rect::from_xywh(
                        first.x - radius,
                        first.y - radius,
                        radius * 2.0,
                        radius * 2.0,
                    )
                    .and_then(PathBuilder::from_oval)
                    {
                        mask.fill_path(
                            &path,
                            &paint([255, 255, 255, 255], a.style.opacity),
                            FillRule::Winding,
                            Transform::identity(),
                            None,
                        );
                    }
                }
            } else if let Some(path) = b.finish() {
                mask.stroke_path(
                    &path,
                    &paint([255, 255, 255, 255], a.style.opacity),
                    &Stroke {
                        width: a.style.width,
                        line_cap: LineCap::Round,
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
            let width = image.width() as usize;
            for (index, (p, m)) in image
                .pixels_mut()
                .zip(mask.data().chunks_exact(4))
                .enumerate()
            {
                if index % width == 0 {
                    control.check()?;
                }
                p[3] = ((u32::from(p[3]) * (255 - u32::from(m[3])) + 127) / 255) as u8;
                if p[3] == 0 {
                    p[0] = 0;
                    p[1] = 0;
                    p[2] = 0;
                }
            }
        }
        Tool::Fill => {
            let x = a.a.x.max(0.0) as u32;
            let y = a.a.y.max(0.0) as u32;
            if x < image.width() && y < image.height() {
                flood(image, x, y, a, control)?;
            }
        }
        Tool::Replace => {
            for y in y0..y1 {
                control.check()?;
                for x in x0..x1 {
                    let old = image.get_pixel(x, y).0;
                    let new = if similar(old, a.source_color, a.style.tolerance) {
                        Some(a.style.color)
                    } else if a.swap_colors && similar(old, a.style.color, a.style.tolerance) {
                        Some(a.source_color)
                    } else {
                        None
                    };
                    if let Some(new) = new {
                        image.put_pixel(x, y, replacement(old, new, a.style.opacity));
                    }
                }
            }
        }
        Tool::Blur | Tool::Pixelate if x1 > x0 && y1 > y0 => {
            let region = image::imageops::crop_imm(image, x0, y0, x1 - x0, y1 - y0).to_image();
            if a.tool == Tool::Blur {
                let sigma = a.style.effect_size as f32 / 2.0;
                let halo = (sigma * 3.0).ceil() as u32;
                for start in (0..region.height()).step_by(64) {
                    control.check()?;
                    let end = (start + 64).min(region.height());
                    let top = start.saturating_sub(halo);
                    let bottom = (end + halo).min(region.height());
                    let tile =
                        image::imageops::crop_imm(&region, 0, top, region.width(), bottom - top)
                            .to_image();
                    let blurred = image::imageops::blur(&tile, sigma);
                    control.check()?;
                    for y in start..end {
                        for x in 0..region.width() {
                            image.put_pixel(x0 + x, y0 + y, *blurred.get_pixel(x, y - top));
                        }
                    }
                }
            } else {
                let block = a.style.effect_size as usize;
                for by in (0..region.height()).step_by(block) {
                    control.check()?;
                    for bx in (0..region.width()).step_by(block) {
                        let end_y = (by + block as u32).min(region.height());
                        let end_x = (bx + block as u32).min(region.width());
                        let mut sum = [0u64; 4];
                        for y in by..end_y {
                            for x in bx..end_x {
                                let p = region.get_pixel(x, y);
                                for c in 0..4 {
                                    sum[c] += u64::from(p[c]);
                                }
                            }
                        }
                        let count = u64::from((end_x - bx) * (end_y - by));
                        let mean = Rgba(sum.map(|value| (value / count) as u8));
                        for y in by..end_y {
                            for x in bx..end_x {
                                image.put_pixel(x0 + x, y0 + y, mean);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn cutout(image: &RgbaImage, a: &Annotation, control: &RenderControl<'_>) -> Result<RgbaImage> {
    let (x0, y0, x1, y1) = a.bounds();
    let horizontal = a.tool != Tool::CutVertical;
    let length = if horizontal {
        image.height()
    } else {
        image.width()
    };
    let first = (if horizontal { y0 } else { x0 }).max(0.0).floor() as u32;
    let end = ((if horizontal { y1 } else { x1 }).max(0.0).ceil() as u32).min(length);
    ensure!(
        first < end && end - first < length,
        "Cutout must remove part, not all, of the image."
    );
    let gap = if a.tool == Tool::CutJagged {
        20u32.min(end - first)
    } else {
        0
    };
    let new_length = length - (end - first) + gap;
    let (width, height) = if horizontal {
        (image.width(), new_length)
    } else {
        (new_length, image.height())
    };
    validate_size(width, height)?;
    let mut output = RgbaImage::new(width, height);
    for y in 0..height {
        control.check()?;
        for x in 0..width {
            let at = if horizontal { y } else { x };
            if gap > 0 {
                let tooth = ((x % 20) as i32 - 10).unsigned_abs() / 2;
                if at >= first.saturating_sub(tooth) && at < first + gap + tooth {
                    continue;
                }
            } else if at >= first && at < first + gap {
                continue;
            }
            let source = if at < first {
                at
            } else if at >= first + gap {
                at + (end - first) - gap
            } else {
                continue;
            };
            if horizontal {
                output.put_pixel(x, y, *image.get_pixel(x, source));
            } else {
                output.put_pixel(x, y, *image.get_pixel(source, y));
            }
        }
    }
    Ok(output)
}
fn flood(
    image: &mut RgbaImage,
    x: u32,
    y: u32,
    a: &Annotation,
    control: &RenderControl<'_>,
) -> Result<()> {
    let width = image.width();
    let height = image.height();
    let source = image.get_pixel(x, y).0;
    let mut visited = vec![0u64; (width as usize * height as usize).div_ceil(64)];
    let mut seeds = vec![y * width + x];
    let eligible = |image: &RgbaImage, visited: &[u64], x: u32, y: u32| {
        let i = (y * width + x) as usize;
        visited[i / 64] & (1u64 << (i % 64)) == 0
            && similar(image.get_pixel(x, y).0, source, a.style.tolerance)
    };
    while let Some(seed) = seeds.pop() {
        control.check()?;
        let y = seed / width;
        let mut left = seed % width;
        if !eligible(image, &visited, left, y) {
            continue;
        }
        while left > 0 && eligible(image, &visited, left - 1, y) {
            left -= 1;
        }
        let mut above = false;
        let mut below = false;
        let mut x = left;
        while x < width && eligible(image, &visited, x, y) {
            let i = (y * width + x) as usize;
            visited[i / 64] |= 1u64 << (i % 64);
            let old = image.get_pixel(x, y).0;
            image.put_pixel(x, y, replacement(old, a.style.color, a.style.opacity));
            for (ny, active) in [(y.wrapping_sub(1), &mut above), (y + 1, &mut below)] {
                let yes = ny < height && eligible(image, &visited, x, ny);
                if yes && !*active {
                    ensure!(
                        seeds.len() < 262_144,
                        "Fill exceeds its scanline work budget. Use a smaller region."
                    );
                    seeds.push(ny * width + x);
                }
                *active = yes;
            }
            x += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maximum_image_render_can_be_cancelled_before_processing_all_rows() {
        use std::cell::Cell;
        let image = RgbaImage::from_pixel(6000, 4000, Rgba([40, 80, 120, 255]));
        let mut document = Document::new(6000, 4000).unwrap();
        document.grayscale = true;
        let calls = Cell::new(0);
        let result = Renderer::new()
            .unwrap()
            .render_cancellable(&image, &document, &|| {
                calls.set(calls.get() + 1);
                calls.get() > 8
            });
        assert!(result
            .unwrap_err()
            .downcast_ref::<RenderCancelled>()
            .is_some());
        assert_eq!(calls.get(), 9);
        assert_eq!(image.get_pixel(5999, 3999).0, [40, 80, 120, 255]);
    }
    #[test]
    fn raster_operations_check_for_supersession_inside_the_operation() {
        use std::cell::Cell;
        let image = RgbaImage::from_pixel(512, 512, Rgba([255, 255, 255, 255]));
        for tool in [
            Tool::Fill,
            Tool::Replace,
            Tool::Blur,
            Tool::Pixelate,
            Tool::Eraser,
            Tool::CutHorizontal,
        ] {
            let mut document = Document::new(512, 512).unwrap();
            let mut operation = annotation(tool);
            operation.b = Point { x: 510.0, y: 510.0 };
            document.layers.push(operation);
            let calls = Cell::new(0);
            let result = Renderer::new()
                .unwrap()
                .render_cancellable(&image, &document, &|| {
                    calls.set(calls.get() + 1);
                    calls.get() > 520
                });
            assert!(
                result
                    .unwrap_err()
                    .downcast_ref::<RenderCancelled>()
                    .is_some(),
                "{tool:?}"
            );
        }
    }
    fn annotation(tool: Tool) -> Annotation {
        Annotation {
            tool,
            a: Point { x: 1.0, y: 1.0 },
            b: Point { x: 18.0, y: 18.0 },
            style: Style {
                color: [255, 0, 0, 255],
                tolerance: 0,
                ..Default::default()
            },
            text: "Hello".into(),
            source_color: [255, 255, 255, 255],
            swap_colors: false,
            path: vec![],
        }
    }
    #[test]
    fn fill_is_contiguous_and_undo_is_non_destructive() {
        let r = Renderer::new().unwrap();
        let mut original = RgbaImage::from_pixel(20, 20, Rgba([255, 255, 255, 255]));
        for y in 0..20 {
            original.put_pixel(10, y, Rgba([0, 0, 0, 255]));
        }
        let mut d = Document::new(20, 20).unwrap();
        d.layers.push(annotation(Tool::Fill));
        let rendered = r.render(&original, &d).unwrap();
        assert_eq!(rendered.get_pixel(2, 2).0, [255, 0, 0, 255]);
        assert_eq!(rendered.get_pixel(12, 2).0, [255, 255, 255, 255]);
        assert_eq!(original.get_pixel(2, 2).0, [255, 255, 255, 255]);
    }
    #[test]
    fn replacement_is_selection_scoped_and_can_swap() {
        let r = Renderer::new().unwrap();
        let mut image = RgbaImage::from_pixel(20, 20, Rgba([255, 255, 255, 255]));
        image.put_pixel(3, 3, Rgba([255, 0, 0, 255]));
        let mut d = Document::new(20, 20).unwrap();
        let mut a = annotation(Tool::Replace);
        a.swap_colors = true;
        d.layers.push(a);
        let out = r.render(&image, &d).unwrap();
        assert_eq!(out.get_pixel(3, 3).0, [255, 255, 255, 255]);
        assert_eq!(out.get_pixel(4, 4).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255, 255]);
    }
    #[test]
    fn every_vector_tool_changes_pixels() {
        let r = Renderer::new().unwrap();
        let image = RgbaImage::from_pixel(200, 100, Rgba([255, 255, 255, 255]));
        for tool in [
            Tool::Arrow,
            Tool::Text,
            Tool::Callout,
            Tool::Rectangle,
            Tool::Ellipse,
            Tool::Line,
            Tool::Stamp,
            Tool::Highlight,
        ] {
            let mut d = Document::new(200, 100).unwrap();
            let mut a = annotation(tool);
            a.b = Point { x: 160.0, y: 70.0 };
            d.layers.push(a);
            assert_ne!(r.render(&image, &d).unwrap(), image, "{}", tool.label());
        }
    }
    #[test]
    fn cutouts_close_gaps_and_jagged_keeps_alpha_gap() {
        let renderer = Renderer::new().unwrap();
        let mut original = RgbaImage::new(40, 40);
        for (x, y, p) in original.enumerate_pixels_mut() {
            *p = Rgba([x as u8, y as u8, 0, 255]);
        }
        for tool in [Tool::CutHorizontal, Tool::CutVertical, Tool::CutJagged] {
            let mut d = Document::new(40, 40).unwrap();
            let mut a = annotation(tool);
            a.a = Point { x: 10.0, y: 10.0 };
            a.b = Point { x: 20.0, y: 20.0 };
            d.layers.push(a);
            let image = renderer.render(&original, &d).unwrap();
            match tool {
                Tool::CutHorizontal => {
                    assert_eq!(image.dimensions(), (40, 30));
                    assert_eq!(image.get_pixel(0, 10)[1], 20);
                }
                Tool::CutVertical => {
                    assert_eq!(image.dimensions(), (30, 40));
                    assert_eq!(image.get_pixel(10, 0)[0], 20);
                }
                _ => assert_eq!(image.get_pixel(10, 15)[3], 0),
            }
        }
    }
}
