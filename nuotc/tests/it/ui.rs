use nuotc::ui::{Component, LayoutBox, Lifecycle, PointerPolicy, UiError, UiRuntime};
use nuotc::{Frame, Grid, Rect, Style};

const VIEW: Rect = Rect::new(0, 0, 40, 20);

fn root(ui: &mut UiRuntime<&'static str>) {
    ui.begin(VIEW);
    ui.mount(Component::new("root", None, LayoutBox::Fill))
        .unwrap();
}

fn child(ui: &mut UiRuntime<&'static str>, key: &'static str, rect: Rect) {
    ui.mount(Component::new(key, Some("root"), LayoutBox::Placed(rect)).interactive())
        .unwrap();
}

#[test]
fn ui_reorder_preserves_instance_state_and_unmount_invalidates_handles() {
    let mut ui = UiRuntime::default();
    root(&mut ui);
    child(&mut ui, "a", Rect::new(0, 0, 5, 2));
    child(&mut ui, "b", Rect::new(0, 2, 5, 2));
    ui.commit().unwrap();
    let a = ui.presented().id(&"a").unwrap();
    *ui.state::<String>(a).unwrap() = "draft".into();
    root(&mut ui);
    child(&mut ui, "b", Rect::new(0, 0, 5, 2));
    child(&mut ui, "a", Rect::new(0, 2, 5, 2));
    ui.commit().unwrap();
    assert_eq!(ui.presented().id(&"a"), Some(a));
    assert_eq!(ui.state::<String>(a).unwrap(), "draft");
    root(&mut ui);
    ui.commit().unwrap();
    assert!(ui.state::<String>(a).is_err());
    root(&mut ui);
    child(&mut ui, "a", Rect::new(0, 2, 5, 2));
    ui.commit().unwrap();
    assert_ne!(ui.presented().id(&"a"), Some(a));
    assert_eq!(ui.focus(a), Err(UiError::InvalidFocus));
}

#[test]
fn ui_pending_and_aborted_frames_do_not_publish_geometry_or_lifecycle() {
    let mut ui = UiRuntime::default();
    root(&mut ui);
    child(&mut ui, "a", Rect::new(0, 0, 5, 2));
    ui.commit().unwrap();
    let id = ui.presented().id(&"a").unwrap();
    ui.capture_pointer(id).unwrap();
    root(&mut ui);
    child(&mut ui, "a", Rect::new(10, 0, 5, 2));
    assert_eq!(ui.presented().hit_test(1, 1), Some(&"a"));
    assert_eq!(ui.presented().hit_test(11, 1), None);
    ui.abort();
    assert_eq!(ui.pointer_target(30, 19), Some(&"a"));
    assert_eq!(ui.presented().id(&"a"), Some(id));
}

#[test]
fn ui_modal_restores_focus_and_releases_covered_pointer_capture() {
    let mut ui = UiRuntime::default();
    root(&mut ui);
    child(&mut ui, "editor", Rect::new(0, 15, 40, 5));
    ui.commit().unwrap();
    let editor = ui.presented().id(&"editor").unwrap();
    ui.focus(editor).unwrap();
    ui.capture_pointer(editor).unwrap();
    root(&mut ui);
    child(&mut ui, "editor", Rect::new(0, 15, 40, 5));
    ui.mount(
        Component::new(
            "modal",
            Some("root"),
            LayoutBox::Placed(Rect::new(10, 5, 20, 10)),
        )
        .modal()
        .layer(10),
    )
    .unwrap();
    let mut toast = Component::new("toast", Some("root"), LayoutBox::Fill).layer(20);
    toast.pointer = PointerPolicy::Transparent;
    ui.mount(toast).unwrap();
    let events = ui.commit().unwrap();
    assert!(events.contains(&Lifecycle::CaptureReleased { id: editor }));
    assert_eq!(ui.pointer_target(0, 19), Some(&"modal"));
    assert_eq!(ui.presented().keyboard_path(ui.focused()), vec![&"modal"]);
    assert_eq!(ui.focus(editor), Err(UiError::InvalidFocus));
    root(&mut ui);
    child(&mut ui, "editor", Rect::new(0, 15, 40, 5));
    ui.commit().unwrap();
    assert_eq!(ui.focused(), Some(editor));
}

#[test]
fn ui_clipping_and_viewport_attachments_have_distinct_geometry() {
    let mut ui = UiRuntime::default();
    root(&mut ui);
    child(&mut ui, "parent", Rect::new(5, 5, 10, 5));
    ui.mount(
        Component::new(
            "clipped",
            Some("parent"),
            LayoutBox::Placed(Rect::new(0, 0, 20, 10)),
        )
        .interactive(),
    )
    .unwrap();
    ui.mount(
        Component::new(
            "popup",
            Some("parent"),
            LayoutBox::Viewport(Rect::new(0, 0, 4, 4)),
        )
        .interactive()
        .layer(1),
    )
    .unwrap();
    ui.commit().unwrap();
    assert_eq!(
        ui.presented().layout(&"clipped").unwrap().clip,
        Rect::new(5, 5, 10, 5)
    );
    assert_eq!(ui.presented().hit_test(0, 0), Some(&"popup"));
    assert_eq!(ui.presented().hit_test(4, 4), None);
    assert!(ui.presented().is_descendant(&"popup", &"parent"));
}

#[test]
fn ui_removal_repaints_old_coverage_and_preserves_clean_overlays() {
    let mut ui = UiRuntime::default();
    let mut grid = Grid::new(40, 20);
    let paint = |key: &&str, layout: nuotc::ui::NodeLayout, f: &mut Frame<'_>| {
        let ch = match *key {
            "root" => '.',
            "moving" => 'm',
            _ => 'o',
        };
        for y in layout.bounds.y..layout.bounds.bottom() {
            f.put(
                layout.bounds.x,
                y,
                Style::default(),
                &ch.to_string().repeat(layout.bounds.width as usize),
            );
        }
    };
    root(&mut ui);
    child(&mut ui, "moving", Rect::new(0, 0, 5, 2));
    ui.mount(
        Component::new(
            "overlay",
            Some("root"),
            LayoutBox::Placed(Rect::new(3, 0, 4, 2)),
        )
        .layer(1),
    )
    .unwrap();
    ui.paint(&mut Frame::new(&mut grid), paint).unwrap();
    ui.commit().unwrap();
    root(&mut ui);
    child(&mut ui, "moving", Rect::new(10, 0, 5, 2));
    ui.mount(
        Component::new(
            "overlay",
            Some("root"),
            LayoutBox::Placed(Rect::new(3, 0, 4, 2)),
        )
        .layer(1),
    )
    .unwrap();
    ui.paint(&mut Frame::new(&mut grid), paint).unwrap();
    assert_eq!(grid.get(0, 0).unwrap().symbol.as_str(), ".");
    assert_eq!(grid.get(3, 0).unwrap().symbol.as_str(), "o");
    assert_eq!(grid.get(10, 0).unwrap().symbol.as_str(), "m");
    ui.commit().unwrap();
    root(&mut ui);
    child(&mut ui, "moving", Rect::new(10, 0, 5, 2));
    ui.mount(
        Component::new(
            "overlay",
            Some("root"),
            LayoutBox::Placed(Rect::new(3, 0, 4, 2)),
        )
        .layer(1),
    )
    .unwrap();
    assert!(ui.damage().unwrap().is_empty());
}

#[test]
fn ui_invalid_structure_is_rejected_without_publishing_it() {
    let mut ui = UiRuntime::default();
    root(&mut ui);
    assert_eq!(
        ui.mount(Component::new("root", None, LayoutBox::Fill)),
        Err(UiError::DuplicateKey)
    );
    assert_eq!(
        ui.mount(Component::new("orphan", Some("absent"), LayoutBox::Fill)),
        Err(UiError::MissingParent)
    );
    assert!(ui.presented().is_empty());
}

#[test]
fn ui_painter_cannot_escape_clip_even_through_direct_buffer_access() {
    let mut grid = Grid::new(6, 3);
    Frame::new(&mut grid).paint_clipped(Rect::new(1, 1, 3, 1), |f| {
        for y in 0..3 {
            f.put(0, y, Style::default(), "abcdef");
        }
    });
    assert_eq!(grid.get(0, 1).unwrap().symbol.as_str(), " ");
    assert_eq!(grid.get(1, 1).unwrap().symbol.as_str(), "b");
    assert_eq!(grid.get(4, 1).unwrap().symbol.as_str(), " ");
    assert_eq!(grid.get(1, 0).unwrap().symbol.as_str(), " ");
}
