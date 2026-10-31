//! toc layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Automatic table of contents ----

    /// Lay out the automatic TOC: a title plus one paragraph per "entry ⇥ page
    /// number", with the page number placed on a right tab stop.
    ///
    /// Page numbers come from the previous layout pass; the space the TOC itself
    /// occupies shifts them all, and `layout_with` keeps feeding them back until
    /// they stabilize. Entry text and page number are separated by a tab, the
    /// right tab stop sits at the right boundary of the text area, and a dotted
    /// leader is drawn when `TocStyle::dot_leader` is enabled.
    pub(crate) fn place_toc(&mut self, toc: &ofdbox_folio_ir::TocBlock) {
        if self.in_frame {
            self.warn("table of contents inside a table cell; skipped");
            return;
        }
        let Some(entries) = self.toc_entries.clone() else {
            // The first layout pass has no entries; wait for later passes.
            return;
        };
        let entries: Vec<TocEntry> = entries
            .into_iter()
            .filter(|(level, title, _)| *level <= toc.max_level.max(1) && !title.trim().is_empty())
            .collect();
        if entries.is_empty() {
            self.warn("automatic table of contents has no usable headings");
            return;
        }

        if let Some(title) = &toc.title
            && !title.trim().is_empty()
        {
            let mut heading = Paragraph::plain(title.clone(), toc.style.title_style.clone());
            heading.heading_level = None;
            self.place_paragraph(&heading, self.total_pages);
        }

        let content = self.content_width();
        let page_number_style = toc.style.page_number_style.clone();
        let leader = if toc.style.dot_leader {
            TabLeader::Dot
        } else {
            TabLeader::None
        };
        for (level, title, page) in entries {
            let indent = toc
                .style
                .indent_per_level
                .to_mm()
                .max(0.0)
                .mul_add(f64::from(level.saturating_sub(1)), 0.0);
            let mut paragraph = Paragraph::new(ParaStyle {
                left_indent: Twips::from_mm(indent),
                tabs: vec![TabStop::new(
                    Twips::from_mm(content),
                    TabAlign::Right,
                    leader,
                )],
                ..ParaStyle::default()
            });
            paragraph.runs.push(Inline::Text(TextRun::new(
                title,
                toc.style.entry_style.clone(),
            )));
            paragraph
                .runs
                .push(Inline::Text(TextRun::new("\t", page_number_style.clone())));
            // Outline targets are 0-based page ordinals; the TOC records 1-based
            // page numbers.
            paragraph.runs.push(Inline::Text(TextRun::new(
                (page + 1).to_string(),
                page_number_style.clone(),
            )));
            self.place_paragraph(&paragraph, self.total_pages);
        }
    }

    /// Measure the height of a group of blocks on a temporary page (emitting no
    /// elements).
    pub(crate) fn measure_blocks(&mut self, blocks: &[Block], width: f64) -> f64 {
        let saved_page = std::mem::take(&mut self.page);
        let saved = (self.frame, self.cursor, self.in_frame);
        // Measurement must not have side effects: the outline, footnote
        // reservations and float exclusion areas all have to be restored.
        let saved_headings = self.headings.len();
        let saved_notes = self.page_notes.len();
        let saved_inset = self.bottom_inset;
        let saved_exclusions = self.float_exclusions.len();
        let saved_frame_exclusions = std::mem::take(&mut self.frame_exclusions);
        let saved_element_start = self.frame_element_start;
        let saved_float_bottom = self.frame_float_bottom;
        self.in_frame = true;
        self.frame = Frame {
            left: 0.0,
            width,
            top: 0.0,
            bottom: f64::MAX,
        };
        self.cursor = 0.0;
        self.frame_element_start = 0;
        self.frame_float_bottom = 0.0;
        for block in blocks {
            self.place_block(block);
        }
        let height = self.cursor;
        self.page = saved_page;
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
        self.headings.truncate(saved_headings);
        self.page_notes.truncate(saved_notes);
        self.bottom_inset = saved_inset;
        self.float_exclusions.truncate(saved_exclusions);
        self.frame_exclusions = saved_frame_exclusions;
        self.frame_element_start = saved_element_start;
        self.frame_float_bottom = saved_float_bottom;
        height
    }
}
