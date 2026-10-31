//! blocks layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Blocks ----

    pub(crate) fn place_block(&mut self, block: &Block) {
        // Lay out the header only before the first body block of a page, so that
        // empty pages do not get a header either.
        if !self.page_body && !self.in_frame && !matches!(block, Block::PageBreak) {
            self.start_page_body();
        }
        match block {
            Block::Paragraph(paragraph) => self.place_paragraph(paragraph, self.total_pages),
            Block::Image(image) => self.place_image_block(image),
            Block::PageBreak => self.new_page(),
            Block::Table(table) => self.place_table(table),
            Block::Toc(toc) => self.place_toc(toc),
            Block::Floating(floating) => self.place_floating(floating),
            _ => self.warn("unknown block type; skipped"),
        }
    }

    /// Lay out a group of blocks in order, handling `keep_with_next`: a heading
    /// must not be left alone at the bottom of a page.
    pub(crate) fn place_blocks(&mut self, blocks: &[Block]) {
        let width = self.content_width();
        for (index, block) in blocks.iter().enumerate() {
            self.break_for_keep_with_next(blocks, index, width);
            self.place_block(block);
        }
    }

    /// Break to the next column / page early when the current block carries
    /// `keep_with_next` (possibly forming a chain) and it plus the start of the
    /// next block do not fit on this page.
    pub(crate) fn break_for_keep_with_next(&mut self, blocks: &[Block], index: usize, width: f64) {
        if self.in_frame || !self.page_body {
            return;
        }
        let mut needed = 0.0_f64;
        let mut cursor = index;
        while let Some(block) = blocks.get(cursor) {
            let keep =
                matches!(block, Block::Paragraph(paragraph) if paragraph.style.keep_with_next);
            if !keep {
                break;
            }
            needed += self.measure_blocks(std::slice::from_ref(block), width);
            cursor += 1;
        }
        if cursor == index {
            return;
        }
        let Some(next) = blocks.get(cursor) else {
            return;
        };
        needed += self.block_leading_height(next, width);
        let content = self.bottom() - self.frame.top;
        if needed <= content && self.cursor + needed > self.bottom() + 1e-6 {
            self.new_flow();
        }
    }

    /// Height needed for the "start" of a block: the first line for a paragraph,
    /// the header plus first row for a table, the whole block otherwise.
    pub(crate) fn block_leading_height(&mut self, block: &Block, width: f64) -> f64 {
        match block {
            Block::Paragraph(paragraph) => {
                let laid = self.lay_out_paragraph(paragraph, self.total_pages);
                let Some(line) = laid.lines.first() else {
                    return 0.0;
                };
                let line_box = self.line_box(&laid.style, &laid.pieces, &laid.images, line);
                laid.style.space_before.to_mm().max(0.0) + line_box.height
            }
            Block::Table(table) => {
                let mut head = table.clone();
                let rows = usize::try_from(table.header_rows).unwrap_or(0).max(1);
                head.rows.truncate(rows.min(table.rows.len()));
                self.measure_blocks(&[Block::Table(head)], width)
            }
            _ => self.measure_blocks(std::slice::from_ref(block), width),
        }
    }

    /// The page body starts here: mark it and lay out the header.
    pub(crate) fn start_page_body(&mut self) {
        self.page_body = true;
        self.begin_body();
    }

    /// Lay out the header when the body starts.
    pub(crate) fn begin_body(&mut self) {
        if self.page_started {
            return;
        }
        self.page_started = true;
        self.layout_header();
        self.body_element_start = self.page.scene.children.len();
        self.flush_pending_floats();
        self.place_planned_floats();
    }

    /// Which header/footer set this page uses: different first page → default;
    /// even page → even-page set.
    pub(crate) fn pick_footer(
        set: &HeaderFooterSet,
        first_page: bool,
        page_index: usize,
    ) -> Option<&HeaderFooter> {
        if first_page && set.first.is_some() {
            return set.first.as_ref();
        }
        if page_index % 2 == 1 && set.even.is_some() {
            return set.even.as_ref();
        }
        set.default.as_ref()
    }

    pub(crate) fn layout_header(&mut self) {
        let first_page = self.section_page == 0;
        let Some(header) = Self::pick_footer(&self.headers, first_page, self.section_page) else {
            return;
        };
        let blocks = header.blocks.clone();
        if blocks.is_empty() {
            return;
        }
        let width = self.page_frame().width;
        let saved = (self.frame, self.cursor, self.in_frame);
        let saved_exclusions = std::mem::take(&mut self.frame_exclusions);
        let saved_element_start = self.frame_element_start;
        let saved_float_bottom = self.frame_float_bottom;
        self.in_frame = true;
        self.frame = Frame {
            left: self.margins.left,
            width,
            top: self.header_distance,
            bottom: self.margins.top,
        };
        self.cursor = self.header_distance;
        self.frame_element_start = self.page.scene.children.len();
        self.frame_float_bottom = self.cursor;
        for block in &blocks {
            self.place_block(block);
        }
        if self.cursor > self.margins.top + 0.5 {
            self.warn("header exceeds the top margin and may overlap the body");
        }
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
        self.frame_exclusions = saved_exclusions;
        self.frame_element_start = saved_element_start;
        self.frame_float_bottom = saved_float_bottom;
    }

    /// Lay out the footer and return the top y it occupies (the body bottom edge
    /// when there is no footer).
    pub(crate) fn layout_footer(&mut self) -> f64 {
        let body_bottom = self.page_height - self.margins.bottom;
        let first_page = self.section_page == 0;
        let Some(footer) = Self::pick_footer(&self.footers, first_page, self.section_page) else {
            return body_bottom;
        };
        let blocks = footer.blocks.clone();
        if blocks.is_empty() {
            return body_bottom;
        }
        let width = self.page_frame().width;
        // Measure the height first, then place it right above "bottom edge -
        // footer distance".
        let height = self.measure_blocks(&blocks, width);
        let bottom = (self.page_height - self.footer_distance).max(self.margins.bottom);
        let top = (bottom - height).max(0.0);
        let saved = (self.frame, self.cursor, self.in_frame);
        let saved_exclusions = std::mem::take(&mut self.frame_exclusions);
        let saved_element_start = self.frame_element_start;
        let saved_float_bottom = self.frame_float_bottom;
        self.in_frame = true;
        self.frame = Frame {
            left: self.margins.left,
            width,
            top,
            bottom,
        };
        self.cursor = top;
        self.frame_element_start = self.page.scene.children.len();
        self.frame_float_bottom = self.cursor;
        for block in &blocks {
            self.place_block(block);
        }
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
        self.frame_exclusions = saved_exclusions;
        self.frame_element_start = saved_element_start;
        self.frame_float_bottom = saved_float_bottom;
        top
    }
}
