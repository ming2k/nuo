use super::*;

#[test]
fn oauth_pending_records_selectable_regions_in_layout_map() {
    use crate::shell::event_loop::transcript::extract_selection_text;
    use nuo_view::model::layout::MODAL_DOC_MSG_IDX;
    use nuo_view::model::selection::{SelectionDrag, SelectionState};

    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let mut layout_map = nuo_view::model::layout::LayoutMap::new();
    let mut scroll = 0;
    let url = "https://github.com/login/oauth/authorize?client_id=Iv1.test12345678&scope=user%3Aemail&state=xyz987";

    terminal.draw(|f| {
        draw_oauth_pending(
            "GitHub OAuth",
            "Please authorize in browser",
            url,
            "1234-5678",
            None,
            0,
            f,
            &theme,
            &mut scroll,
            None,
            &SelectionState::None,
            &mut layout_map,
        );
    });

    // Verify layout map recorded MODAL_DOC_MSG_IDX regions
    let doc_region = layout_map
        .first_region_for_message(MODAL_DOC_MSG_IDX)
        .expect("OAuth pending must push modal doc regions to layout_map");

    // Test drag selecting inside the OAuth modal
    let url_cursor = layout_map
        .cursor_at(doc_region.rect.x, doc_region.rect.y)
        .expect("must resolve cursor on oauth text");
    assert_eq!(url_cursor.message_idx, MODAL_DOC_MSG_IDX);

    let mut selection = SelectionState::None;
    let mut drag = SelectionDrag::default();
    drag.begin_range(&mut selection, url_cursor);

    // Drag to the end of the line
    drag.update_from_point(
        &mut selection,
        &layout_map,
        doc_region.rect.x + doc_region.rect.width - 1,
        doc_region.rect.y,
    );
    drag.finish(&mut selection);

    assert!(selection.is_active());
    let extracted = extract_selection_text(&selection, &[], "", &layout_map, None);
    assert!(extracted.is_some());
}
