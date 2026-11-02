use image::{Rgba, RgbaImage};
use snapper_desktop::{
    library::{self, Library},
    model::*,
    render::Renderer,
};

fn original() -> RgbaImage {
    image::load_from_memory(include_bytes!("fixtures/editor-oracle.png"))
        .unwrap()
        .into_rgba8()
}
fn operation(tool: Tool) -> Annotation {
    Annotation {
        tool,
        a: Point { x: 50.0, y: 50.0 },
        b: Point { x: 512.0, y: 384.0 },
        style: Style {
            color: [255, 255, 0, 255],
            tolerance: 0,
            ..Default::default()
        },
        source_color: [255, 0, 0, 255],
        swap_colors: false,
        text: "Independent oracle".into(),
        path: vec![],
    }
}
fn lifecycle(annotation: Annotation, assert: impl FnOnce(&RgbaImage)) {
    let original = original();
    let renderer = Renderer::new().unwrap();
    let mut document = Document::new(512, 384).unwrap();
    let mut history = History::default();
    history.checkpoint(&document);
    document.layers.push(annotation);
    let edited = renderer.render(&original, &document).unwrap();
    assert(&edited);
    history.undo(&mut document);
    assert_eq!(renderer.render(&original, &document).unwrap(), original);
    history.redo(&mut document);
    assert_eq!(renderer.render(&original, &document).unwrap(), edited);
    let root = tempfile::tempdir().unwrap();
    let library = Library::open(root.path().to_owned()).unwrap();
    let capture = library.import_image(&original, "Oracle").unwrap();
    library.save_document(&capture, &document).unwrap();
    let reopened = Library::open(root.path().to_owned()).unwrap();
    let loaded = reopened.load_document(&capture).unwrap();
    let original_path = library.capture_dir(&capture).unwrap().join("original.png");
    let source = library::load_image(&original_path).unwrap();
    assert_eq!(source, original);
    let flattened = renderer.render(&source, &loaded).unwrap();
    assert_eq!(flattened, edited);
    let export = root.path().join("export.png");
    library::export_image(&export, &flattened).unwrap();
    assert_eq!(library::load_image(&export).unwrap(), edited);
}
#[test]
fn connected_fill_leaves_disconnected_red_unchanged() {
    lifecycle(operation(Tool::Fill), |image| {
        assert_eq!(*image.get_pixel(50, 50), Rgba([255, 255, 0, 255]));
        assert_eq!(*image.get_pixel(170, 50), Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(330, 50)[3], 0);
    });
}
#[test]
fn global_replace_reaches_both_red_regions() {
    let mut a = operation(Tool::Replace);
    a.a = Point::default();
    lifecycle(a, |image| {
        assert_eq!(*image.get_pixel(50, 50), Rgba([255, 255, 0, 255]));
        assert_eq!(*image.get_pixel(170, 50), Rgba([255, 255, 0, 255]));
        assert_eq!(*image.get_pixel(170, 170), Rgba([0, 0, 255, 255]));
        assert_eq!(image.get_pixel(330, 50)[3], 0);
    });
}
#[test]
fn replacement_honors_selection_boundary() {
    let mut a = operation(Tool::Replace);
    a.a = Point { x: 30.0, y: 30.0 };
    a.b = Point { x: 120.0, y: 120.0 };
    lifecycle(a, |image| {
        assert_eq!(*image.get_pixel(50, 50), Rgba([255, 255, 0, 255]));
        assert_eq!(*image.get_pixel(170, 50), Rgba([255, 0, 0, 255]));
    });
}
#[test]
fn simultaneous_swap_does_not_collapse_colors() {
    let mut a = operation(Tool::Replace);
    a.a = Point::default();
    a.style.color = [0, 0, 255, 255];
    a.swap_colors = true;
    lifecycle(a, |image| {
        assert_eq!(*image.get_pixel(50, 50), Rgba([0, 0, 255, 255]));
        assert_eq!(*image.get_pixel(170, 50), Rgba([0, 0, 255, 255]));
        assert_eq!(*image.get_pixel(170, 170), Rgba([255, 0, 0, 255]));
    });
}
#[test]
fn eraser_clears_alpha_only_in_brush() {
    let mut a = operation(Tool::Eraser);
    a.a = Point { x: 72.0, y: 72.0 };
    a.b = a.a;
    a.style.width = 24.0;
    lifecycle(a, |image| {
        assert_eq!(*image.get_pixel(72, 72), Rgba([0, 0, 0, 0]));
        assert_eq!(*image.get_pixel(50, 50), Rgba([255, 0, 0, 255]));
    });
}
#[test]
fn horizontal_cutout_closes_exact_strip() {
    let mut a = operation(Tool::CutHorizontal);
    a.a = Point { x: 0.0, y: 120.0 };
    a.b = Point { x: 512.0, y: 152.0 };
    lifecycle(a, |image| {
        assert_eq!(image.dimensions(), (512, 352));
        assert_eq!(*image.get_pixel(50, 138), Rgba([0, 255, 0, 255]));
        assert_eq!(*image.get_pixel(50, 50), Rgba([255, 0, 0, 255]));
    });
}
#[test]
fn vertical_cutout_closes_exact_strip() {
    let mut a = operation(Tool::CutVertical);
    a.a = Point { x: 120.0, y: 0.0 };
    a.b = Point { x: 152.0, y: 384.0 };
    lifecycle(a, |image| {
        assert_eq!(image.dimensions(), (480, 384));
        assert_eq!(*image.get_pixel(138, 50), Rgba([255, 0, 0, 255]));
        assert_eq!(*image.get_pixel(50, 170), Rgba([0, 255, 0, 255]));
    });
}
#[test]
fn blur_is_scoped_and_changes_checkerboard() {
    let mut a = operation(Tool::Blur);
    a.a = Point { x: 320.0, y: 176.0 };
    a.b = Point { x: 416.0, y: 272.0 };
    lifecycle(a, |image| {
        assert_eq!(image.dimensions(), (512, 384));
        assert_ne!(*image.get_pixel(330, 186), *original().get_pixel(330, 186));
        assert_eq!(*image.get_pixel(50, 50), Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(330, 50)[3], 0);
    });
}
#[test]
fn pixelation_is_scoped_and_changes_checkerboard() {
    let mut a = operation(Tool::Pixelate);
    a.a = Point { x: 320.0, y: 176.0 };
    a.b = Point { x: 416.0, y: 272.0 };
    lifecycle(a, |image| {
        assert_ne!(*image.get_pixel(330, 186), *original().get_pixel(330, 186));
        assert_eq!(*image.get_pixel(50, 50), Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(330, 50)[3], 0);
    });
}
