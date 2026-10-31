//! pages layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Pages ----

    pub(crate) fn begin_section(
        &mut self,
        setup: &PageSetup,
        headers: &HeaderFooterSet,
        footers: &HeaderFooterSet,
    ) {
        self.finish_page();
        self.page_width = setup.width.to_mm();
        self.page_height = setup.height.to_mm();
        self.margins = MarginsMm {
            top: setup.margins.top.to_mm(),
            right: setup.margins.right.to_mm(),
            bottom: setup.margins.bottom.to_mm(),
            left: setup.margins.left.to_mm(),
        };
        if self.page_width <= 0.0 || self.page_height <= 0.0 {
            self.page_width = 210.0;
            self.page_height = 297.0;
            self.warn("invalid page size; falling back to A4");
        }
        self.column_count = setup.columns.max(1);
        self.column_gap = setup.column_gap.to_mm().max(0.0);
        self.column_index = 0;
        self.header_distance = setup.header_distance.to_mm().max(0.0);
        self.footer_distance = setup.footer_distance.to_mm().max(0.0);
        self.headers = headers.clone();
        self.footers = footers.clone();
        self.section_page = 0;
        self.in_frame = false;
        self.page_body = false;
        self.page_started = false;
        self.float_exclusions.clear();
        self.placed_float_rects.clear();
        self.float_overlap_reported = false;
        self.frame = self.column_frame(0);
        self.cursor = self.frame.top;
    }

    /// Text area frame.
    /// Full-page text area frame (used by headers and footers, spanning all
    /// columns).
    pub(crate) fn page_frame(&self) -> Frame {
        Frame {
            left: self.margins.left,
            width: (self.page_width - self.margins.left - self.margins.right).max(1.0),
            top: self.margins.top,
            bottom: self.page_height - self.margins.bottom,
        }
    }

    /// Frame of column `index`.
    pub(crate) fn column_frame(&self, index: usize) -> Frame {
        let page = self.page_frame();
        let count = f64::from(self.column_count.max(1));
        let gap = self.column_gap;
        let width = ((page.width - gap * (count - 1.0)) / count).max(1.0);
        Frame {
            left: page.left + (width + gap) * index as f64,
            width,
            top: page.top,
            bottom: page.bottom,
        }
    }

    /// Advance to the next column; returns false when there is none.
    pub(crate) fn next_column(&mut self) -> bool {
        let count = usize::from(self.column_count.max(1));
        if self.column_index + 1 >= count {
            return false;
        }
        self.column_index += 1;
        // Square-wrap exclusion areas belong to the current column container.
        self.float_exclusions.clear();
        self.placed_float_rects.clear();
        self.float_overlap_reported = false;
        self.frame = self.column_frame(self.column_index);
        self.cursor = self.frame.top;
        true
    }

    /// Move the body flow to the next container: first the next column, and on
    /// the last column a new page.
    pub(crate) fn new_flow(&mut self) {
        if self.next_column() {
            return;
        }
        self.new_page();
        self.start_page_body();
    }

    pub(crate) fn content_width(&self) -> f64 {
        self.frame.width.max(1.0)
    }

    pub(crate) fn bottom(&self) -> f64 {
        self.frame.bottom
    }

    pub(crate) fn page_number(&self) -> usize {
        self.fixed.pages.len() + 1
    }

    pub(crate) fn new_page(&mut self) {
        self.finish_page();
        self.page = PageBuffer::new();
        self.column_index = 0;
        self.bottom_inset = 0.0;
        // Square-wrap exclusion areas belong to the page where the floating
        // object was placed; they must not affect later pages.
        self.float_exclusions.clear();
        self.placed_float_rects.clear();
        self.float_overlap_reported = false;
        self.frame = self.column_frame(0);
        self.cursor = self.frame.top;
        self.page_body = false;
        self.page_started = false;
        self.section_page += 1;
        self.front_elements.clear();
        self.body_element_start = 0;
    }

    /// End the current page; pages without body content are not emitted.
    pub(crate) fn finish_page(&mut self) {
        if !self.page_body {
            return;
        }
        if !self.front_elements.is_empty() {
            self.page
                .scene
                .children
                .extend(std::mem::take(&mut self.front_elements));
        }
        let footer_top = self.layout_footer();
        let body_bottom = self.page_height - self.margins.bottom;
        self.emit_page_notes(footer_top.min(body_bottom));
        let size = Size::new(self.page_width, self.page_height);
        let scene = Scene {
            size,
            root: std::mem::take(&mut self.page.scene),
            diagnostics: Vec::new(),
        };
        let index = self.fixed.pages.len();
        let mut page = Page::new(
            PageInfo {
                media_box: Rect::new(0.0, 0.0, size.width, size.height),
                crop_box: Rect::new(0.0, 0.0, size.width, size.height),
                rotation: Rotation::None,
                label: None,
            },
            scene,
        );
        page.text = TextLayer {
            page: index,
            items: std::mem::take(&mut self.page.items),
            lines: std::mem::take(&mut self.page.lines),
        };
        page.links = std::mem::take(&mut self.page.links);
        self.fixed.pages.push(page);
    }

    /// Snapshot the current page, used to roll back a failed placement.
    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            children: self.page.scene.children.len(),
            items: self.page.items.len(),
            lines: self.page.lines.len(),
            links: self.page.links.len(),
            cursor: self.cursor,
        }
    }

    pub(crate) fn restore(&mut self, snapshot: Snapshot) {
        self.page.scene.children.truncate(snapshot.children);
        self.page.items.truncate(snapshot.items);
        self.page.lines.truncate(snapshot.lines);
        self.page.links.truncate(snapshot.links);
        self.cursor = snapshot.cursor;
    }

    pub(crate) fn warn(&mut self, message: impl Into<String>) {
        self.fixed
            .report
            .push(Diagnostic::warning(DiagnosticCode::Geometry, message));
    }

    pub(crate) fn diagnostic(&mut self, code: DiagnosticCode, message: impl Into<String>) {
        self.fixed.report.push(Diagnostic::warning(code, message));
    }
}
