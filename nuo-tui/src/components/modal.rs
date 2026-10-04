//! Reusable modal page components and helpers.

use nuotc::{Frame, Rect};

use super::super::Theme;
use super::super::design::MODAL_INNER_H_PADDING;
use super::super::primitives::{
    ContentModalSpec, content_modal_area, content_modal_probe, modal_chrome_rows, modal_frame,
    modal_header, render_modal_footer_with_extra,
};
use super::footer::{FooterHint, FooterHintWithBand};
use super::scroll::ScrollBody;

pub(crate) enum ModalPageSize {
    Content(ContentModalSpec),
}

pub(crate) enum ModalHeader<'a> {
    Title(&'a str),
}

impl<'a> ModalHeader<'a> {
    pub(crate) fn title(title: &'a str) -> Self {
        Self::Title(title)
    }
}

pub(crate) struct ModalPage<'a> {
    pub size: ModalPageSize,
    pub header: ModalHeader<'a>,
    pub body: ScrollBody<'a>,
    pub footer_hints: &'a [FooterHint],
    pub extra_footer_hints: &'a [FooterHintWithBand],
}

pub(crate) fn modal_body_width(frame: &Frame, geometry: ContentModalSpec) -> usize {
    let probe = content_modal_probe(frame, geometry);
    (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1)
}

/// Draw a complete centered modal page from declarative parts.
pub(crate) fn draw_modal_page(frame: &mut Frame, page: ModalPage<'_>, theme: &Theme) -> Rect {
    let area = match page.size {
        ModalPageSize::Content(geometry) => {
            let spec = geometry.modal_spec();
            let desired = page.body.lines.len() as u16 + modal_chrome_rows(spec);
            content_modal_area(frame, geometry, desired)
        }
    };
    let f = modal_frame(frame, area, theme, true, true);

    match page.header {
        ModalHeader::Title(title) => {
            modal_header(frame, f.header, title, theme);
        }
    }
    page.body.render(frame, f.body, theme);
    if let Some(footer) = f.footer {
        render_modal_footer_with_extra(
            frame,
            footer,
            page.footer_hints,
            page.extra_footer_hints,
            theme,
        );
    }
    area
}
