use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;
pub const MAX_PIXELS: u64 = 24_000_000;
pub const MAX_DIMENSION: u32 = 16_384;
pub const MAX_LAYERS: usize = 512;
pub const MAX_HISTORY: usize = 80;
pub const MAX_TEXT: usize = 4096;

pub fn validate_size(width: u32, height: u32) -> Result<()> {
    ensure!(
        width > 0 && height > 0,
        "Image dimensions must be positive."
    );
    ensure!(
        width <= MAX_DIMENSION
            && height <= MAX_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "Image exceeds the 24-megapixel / 16,384-pixel dimension budget."
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tool {
    Select,
    Arrow,
    Text,
    Callout,
    Rectangle,
    Ellipse,
    Line,
    Stamp,
    Fill,
    Replace,
    Blur,
    Pixelate,
    Highlight,
    Eraser,
    CutHorizontal,
    CutVertical,
    CutJagged,
}

impl Tool {
    pub const TOOLS: [Self; 17] = [
        Self::Select,
        Self::Arrow,
        Self::Text,
        Self::Callout,
        Self::Rectangle,
        Self::Ellipse,
        Self::Line,
        Self::Stamp,
        Self::Fill,
        Self::Replace,
        Self::Blur,
        Self::Pixelate,
        Self::Highlight,
        Self::Eraser,
        Self::CutHorizontal,
        Self::CutVertical,
        Self::CutJagged,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Arrow => "Arrow",
            Self::Text => "Text",
            Self::Callout => "Callout",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Line => "Line",
            Self::Stamp => "Stamp",
            Self::Fill => "Fill",
            Self::Replace => "Color replacement",
            Self::Blur => "Blur",
            Self::Pixelate => "Pixelate",
            Self::Highlight => "Highlight",
            Self::Eraser => "Eraser",
            Self::CutHorizontal => "Horizontal cutout",
            Self::CutVertical => "Vertical cutout",
            Self::CutJagged => "Jagged cutout",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineStyle {
    Solid,
    Dashed,
    Dotted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndStyle {
    None,
    Arrow,
    Circle,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stamp {
    Check,
    Cross,
    Star,
    Number,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub color: [u8; 4],
    pub fill: [u8; 4],
    pub width: f32,
    pub opacity: f32,
    pub shadow: bool,
    pub line: LineStyle,
    pub start: EndStyle,
    pub end: EndStyle,
    pub start_size: f32,
    pub end_size: f32,
    pub font_size: f32,
    pub filled: bool,
    pub tolerance: u8,
    pub stamp: Stamp,
    pub effect_size: u32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: [38, 170, 164, 255],
            fill: [255, 255, 255, 235],
            width: 5.0,
            opacity: 1.0,
            shadow: false,
            line: LineStyle::Solid,
            start: EndStyle::None,
            end: EndStyle::Arrow,
            start_size: 3.0,
            end_size: 3.0,
            font_size: 28.0,
            filled: false,
            tolerance: 20,
            stamp: Stamp::Check,
            effect_size: 12,
        }
    }
}

impl Style {
    pub fn validate(&self) -> Result<()> {
        for (name, value, min, max) in [
            ("width", self.width, 0.5, 80.0),
            ("opacity", self.opacity, 0.0, 1.0),
            ("font size", self.font_size, 8.0, 240.0),
            ("start size", self.start_size, 0.5, 12.0),
            ("end size", self.end_size, 0.5, 12.0),
        ] {
            ensure!(
                value.is_finite() && (min..=max).contains(&value),
                "Invalid {name}."
            );
        }
        ensure!(
            (2..=128).contains(&self.effect_size),
            "Invalid effect size."
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub tool: Tool,
    pub a: Point,
    pub b: Point,
    pub style: Style,
    pub text: String,
    pub source_color: [u8; 4],
    pub swap_colors: bool,
    #[serde(default)]
    pub path: Vec<Point>,
}

impl Annotation {
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        (
            self.a.x.min(self.b.x),
            self.a.y.min(self.b.y),
            self.a.x.max(self.b.x),
            self.a.y.max(self.b.y),
        )
    }

    pub fn contains(&self, point: Point) -> bool {
        let (x0, y0, x1, y1) = self.bounds();
        let pad = self.style.width.max(8.0);
        point.x >= x0 - pad && point.x <= x1 + pad && point.y >= y0 - pad && point.y <= y1 + pad
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub schema: u32,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<Annotation>,
    pub grayscale: bool,
    pub border: u32,
    pub border_color: [u8; 4],
}

impl Document {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        validate_size(width, height)?;
        Ok(Self {
            schema: SCHEMA,
            width,
            height,
            layers: vec![],
            grayscale: false,
            border: 0,
            border_color: [38, 170, 164, 255],
        })
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == SCHEMA,
            "Unsupported document version {}.",
            self.schema
        );
        validate_size(self.width, self.height)?;
        ensure!(
            self.layers.len() <= MAX_LAYERS,
            "The 512-layer budget was exceeded."
        );
        ensure!(self.border <= 100, "Invalid image border.");
        for layer in &self.layers {
            layer.style.validate()?;
            ensure!(
                layer.text.len() <= MAX_TEXT,
                "Annotation text exceeds 4096 bytes."
            );
            ensure!(
                layer.path.len() <= 2048,
                "An eraser stroke exceeds 2,048 points."
            );
            for point in [layer.a, layer.b].iter().chain(layer.path.iter()) {
                ensure!(
                    point.x.is_finite() && point.y.is_finite(),
                    "Invalid annotation position."
                );
                ensure!(
                    point.x.abs() <= MAX_DIMENSION as f32 * 2.0
                        && point.y.abs() <= MAX_DIMENSION as f32 * 2.0,
                    "Annotation position is outside the editing budget."
                );
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct History {
    undo: Vec<Document>,
    redo: Vec<Document>,
}

impl History {
    pub fn checkpoint(&mut self, current: &Document) {
        if self.undo.last() == Some(current) {
            return;
        }
        if self.undo.len() == MAX_HISTORY {
            self.undo.remove(0);
        }
        self.undo.push(current.clone());
        while self.undo.len() > 1
            && self.undo.iter().map(Self::document_bytes).sum::<usize>() > 12 * 1024 * 1024
        {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    fn document_bytes(document: &Document) -> usize {
        std::mem::size_of::<Document>()
            + document
                .layers
                .iter()
                .map(|a| {
                    std::mem::size_of::<Annotation>()
                        + a.text.len()
                        + a.path.len() * std::mem::size_of::<Point>()
                })
                .sum::<usize>()
    }
    pub fn undo(&mut self, current: &mut Document) -> bool {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(current, previous));
            true
        } else {
            false
        }
    }
    pub fn redo(&mut self, current: &mut Document) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(current, next));
            true
        } else {
            false
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuickStyle {
    pub name: String,
    pub tool: Tool,
    pub style: Style,
    pub favorite: bool,
}

impl QuickStyle {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 80,
            "Style name must be 1-80 bytes."
        );
        if self.tool == Tool::Select {
            bail!("Selection has no drawable style.");
        }
        self.style.validate()
    }
}

pub fn default_styles() -> Vec<QuickStyle> {
    [
        (Tool::Arrow, "Teal arrow", [38, 170, 164, 255]),
        (Tool::Arrow, "Coral arrow", [240, 91, 105, 255]),
        (Tool::Text, "Ink text", [30, 39, 55, 255]),
        (Tool::Rectangle, "Amber box", [230, 161, 39, 255]),
        (Tool::Callout, "Callout", [38, 170, 164, 255]),
        (Tool::Stamp, "Check", [38, 170, 164, 255]),
    ]
    .into_iter()
    .map(|(tool, name, color)| QuickStyle {
        name: name.into(),
        tool,
        style: Style {
            color,
            ..Default::default()
        },
        favorite: true,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_reject_oversized_and_invalid_images() {
        assert!(validate_size(1920, 1080).is_ok());
        assert!(validate_size(6000, 4000).is_ok());
        assert!(validate_size(6001, 4000).is_err());
        assert!(validate_size(0, 10).is_err());
    }
    #[test]
    fn history_is_bounded_and_round_trips() {
        let mut d = Document::new(100, 100).unwrap();
        let mut h = History::default();
        for i in 0..120 {
            h.checkpoint(&d);
            d.border = i % 101;
        }
        assert_eq!(h.undo.len(), MAX_HISTORY);
        let expected = d.clone();
        assert!(h.undo(&mut d));
        assert!(h.redo(&mut d));
        assert_eq!(d, expected);
    }
    #[test]
    fn style_rejects_nonfinite_values() {
        let s = Style {
            width: f32::NAN,
            ..Default::default()
        };
        assert!(s.validate().is_err());
    }
}
