//! tables layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Tables ----

    pub(crate) fn place_table(&mut self, table: &Table) {
        if table.rows.is_empty() || table.columns.is_empty() {
            self.warn("table has no rows or columns; skipped");
            return;
        }
        let nested = self.in_frame;
        let pages_before = self.fixed.pages.len();
        let widths = self.table_widths(table);
        let table_width: f64 = widths.iter().sum();
        let content_width = self.content_width();
        let indent = table.indent.to_mm();
        let offset = match table.alignment {
            Align::Center => (content_width - table_width) / 2.0 + indent,
            Align::Right => content_width - table_width - indent,
            _ => indent,
        };
        let table_left = self.frame.left + offset;
        let header_rows = usize::try_from(table.header_rows)
            .unwrap_or(0)
            .min(table.rows.len());
        let page_bottom = self.bottom();
        let mut index = 0_usize;
        // Continuation position for in-row splitting: `Some` means this row has
        // not been fully laid out yet.
        let mut resume: Option<Vec<Option<CellContinuation>>> = None;
        let mut guard = 0_usize;
        while index < table.rows.len() {
            guard += 1;
            if guard > table.rows.len() * 8 + 16 {
                self.warn("abnormal table pagination count; stopped laying out this table");
                break;
            }
            let snapshot = self.snapshot();
            let top = self.cursor;
            let fragment = self.layout_row(
                table,
                &widths,
                index,
                top,
                page_bottom,
                resume.as_deref(),
                false,
                true,
                table_left,
            );
            let complete = fragment.continuation.iter().all(Option::is_none);
            if complete && fragment.bottom <= page_bottom + 1e-6 {
                // The whole row fits on this page.
                self.cursor = fragment.bottom;
                index += 1;
                resume = None;
                continue;
            }
            if table.rows[index].cant_split && !complete && top > self.page_frame().top + 1e-6 {
                // The row must stay on one page: move it as a whole and retry.
                self.restore(snapshot);
                self.new_flow();
                self.repeat_header_rows(table, &widths, header_rows, page_bottom, table_left);
                let retry = self.layout_row(
                    table,
                    &widths,
                    index,
                    self.cursor,
                    page_bottom,
                    None,
                    true,
                    true,
                    table_left,
                );
                if retry.continuation.iter().any(Option::is_some) {
                    self.diagnostic(
                        DiagnosticCode::Geometry,
                        "row cannot be split and is taller than the page; allowed to overflow",
                    );
                }
                self.cursor = retry.bottom;
                index += 1;
                resume = None;
                continue;
            }
            if !fragment.placed_any {
                // Not a single line was placed: move the whole row to the next
                // page, repeat the header and try again.
                self.restore(snapshot);
                self.new_flow();
                self.repeat_header_rows(table, &widths, header_rows, page_bottom, table_left);
                let retry = self.layout_row(
                    table,
                    &widths,
                    index,
                    self.cursor,
                    page_bottom,
                    None,
                    true,
                    true,
                    table_left,
                );
                if retry.bottom > page_bottom + 1e-6 {
                    self.diagnostic(
                        DiagnosticCode::Geometry,
                        "table row is taller than the available height; split by row or allowed to overflow",
                    );
                }
                self.cursor = retry.bottom;
                index += 1;
                resume = None;
                continue;
            }
            // In-row splitting: keep the part laid out on this page and continue
            // the rest on the next page.
            self.cursor = fragment.bottom;
            self.new_flow();
            self.repeat_header_rows(table, &widths, header_rows, page_bottom, table_left);
            if complete {
                // Only padding / fixed row height overflows; the row itself is
                // already complete.
                index += 1;
                resume = None;
            } else {
                if row_has_floating(&table.rows[index]) && !self.float_row_split_reported {
                    self.diagnostic(
                        DiagnosticCode::Geometry,
                        "table row with floating objects spans pages; continuation is approximate",
                    );
                    self.float_row_split_reported = true;
                }
                self.diagnostic(
                    DiagnosticCode::Geometry,
                    "table row spans pages; continued by row",
                );
                resume = Some(fragment.continuation);
            }
        }
        if nested && self.fixed.pages.len() > pages_before && !self.nested_table_split_reported {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "nested table spans pages; continuation is approximate",
            );
            self.nested_table_split_reported = true;
        }
    }

    /// Repeat the header rows when the table crosses a page.
    pub(crate) fn repeat_header_rows(
        &mut self,
        table: &Table,
        widths: &[f64],
        count: usize,
        limit: f64,
        table_left: f64,
    ) {
        for header in 0..count {
            let fragment = self.layout_row(
                table,
                widths,
                header,
                self.cursor,
                limit,
                None,
                true,
                false,
                table_left,
            );
            self.cursor = fragment.bottom;
        }
    }

    /// Table column widths (millimeters).
    pub(crate) fn table_widths(&self, table: &Table) -> Vec<f64> {
        let total = self.content_width();
        let mut widths = vec![0.0_f64; table.columns.len()];
        let mut fixed = 0.0;
        let mut weights = 0_u32;
        for (index, column) in table.columns.iter().enumerate() {
            match column.width {
                ColumnWidth::Twips(twips) => {
                    let width = twips.to_mm().max(0.0);
                    widths[index] = width;
                    fixed += width;
                }
                ColumnWidth::Weight(weight) => weights += u32::from(weight.max(1)),
                _ => weights += 1,
            }
        }
        if weights > 0 {
            let remaining = (total - fixed).max(0.0);
            for (index, column) in table.columns.iter().enumerate() {
                let weight = match column.width {
                    ColumnWidth::Weight(weight) => u32::from(weight.max(1)),
                    ColumnWidth::Twips(_) => 0,
                    _ => 1,
                };
                if weight > 0 {
                    widths[index] = remaining * f64::from(weight) / f64::from(weights);
                }
            }
        } else if fixed > total && fixed > 0.0 {
            let scale = total / fixed;
            for width in &mut widths {
                *width *= scale;
            }
        }
        widths
    }

    /// Lay out one row (possibly only part of it) and return the bottom occupied
    /// on this page plus the continuation position of each cell.
    ///
    /// `limit` is the available bottom boundary on this page: cell content stops
    /// at the boundary and the remainder is handed to the next page through
    /// `CellContinuation`. `resume` is the continuation position this row left on
    /// the previous page, and `force` means one line must be laid out even if
    /// nothing fits (to prevent an infinite loop).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn layout_row(
        &mut self,
        table: &Table,
        widths: &[f64],
        index: usize,
        top: f64,
        limit: f64,
        resume: Option<&[Option<CellContinuation>]>,
        force: bool,
        allow_floating: bool,
        table_left: f64,
    ) -> RowFragment {
        let row: &Row = &table.rows[index];
        let saved_frame = self.frame;
        let saved_cursor = self.cursor;
        let saved_in_frame = self.in_frame;
        self.in_frame = true;

        let mut x = table_left;
        let mut max_bottom = top;
        let mut placed_any = false;
        let mut continuation = vec![None; row.cells.len()];
        // Column cursor: a merged cell occupies several columns according to its
        // span.
        let mut column = 0_usize;
        for (cell_index, cell) in row.cells.iter().enumerate() {
            let span = usize::try_from(cell.span.max(1)).unwrap_or(1);
            let width: f64 = widths.iter().skip(column).take(span).sum();
            if width <= 0.0 {
                column += span;
                continue;
            }
            let padding = cell.padding.unwrap_or(table.cell_padding);
            let padding_top = padding.top.to_mm().max(0.0);
            let padding_bottom = padding.bottom.to_mm().max(0.0);
            let padding_left = padding.left.to_mm().max(0.0);
            let padding_right = padding.right.to_mm().max(0.0);
            let inner_left = x + padding_left;
            let inner_width = (width - padding_left - padding_right).max(1.0);
            let resume_at = resume.and_then(|items| items.get(cell_index).copied().flatten());
            // A vertically merged continuation cell has no content of its own; it
            // is covered by the starting cell above.
            if cell.v_merge == ofdbox_folio_ir::VMerge::Continue {
                x += width;
                column += span;
                continue;
            }
            let outcome = self.layout_cell(
                cell,
                Frame {
                    left: inner_left,
                    width: inner_width,
                    top: top + padding_top,
                    bottom: limit,
                },
                top + padding_top,
                resume_at,
                force,
                allow_floating,
            );
            placed_any |= outcome.placed_any;
            max_bottom = max_bottom.max(outcome.bottom + padding_bottom);
            continuation[cell_index] = outcome.continuation;
            x += width;
            column += span;
        }

        self.frame = saved_frame;
        self.cursor = saved_cursor;
        self.in_frame = saved_in_frame;

        let complete = continuation.iter().all(Option::is_none);
        let mut height = (max_bottom - top).max(1.0);
        if complete && let Some(fixed) = row.height {
            let fixed = fixed.to_mm().max(1.0);
            height = match row.height_rule {
                ofdbox_folio_ir::RowHeightRule::Auto => height,
                ofdbox_folio_ir::RowHeightRule::AtLeast => height.max(fixed),
                ofdbox_folio_ir::RowHeightRule::Exact => fixed,
                _ => height.max(fixed),
            };
        }
        if placed_any {
            self.draw_row_borders(table, widths, index, top, top + height, table_left);
        }
        RowFragment {
            bottom: top + height,
            continuation,
            placed_any,
        }
    }

    /// Lay out inside a cell's sub-frame, up to `frame.bottom` at most.
    pub(crate) fn layout_cell(
        &mut self,
        cell: &ofdbox_folio_ir::Cell,
        frame: Frame,
        top: f64,
        resume: Option<CellContinuation>,
        force: bool,
        allow_floating: bool,
    ) -> CellOutcome {
        let saved_frame = self.frame;
        let saved_cursor = self.cursor;
        let saved_in_frame = self.in_frame;
        let saved_exclusions = std::mem::take(&mut self.frame_exclusions);
        let saved_element_start = self.frame_element_start;
        let saved_float_bottom = self.frame_float_bottom;
        self.frame = frame;
        self.cursor = top;
        self.in_frame = true;
        self.frame_element_start = self.page.scene.children.len();
        self.frame_float_bottom = top;

        if allow_floating && resume.is_none() {
            for block in &cell.blocks {
                if let Block::Floating(floating) = block {
                    self.place_floating_in_frame(floating);
                }
            }
        }

        let mut block_index = resume.map(|item| item.block).unwrap_or(0);
        let mut line = resume.map(|item| item.line).unwrap_or(0);
        let mut placed_any = false;
        let mut continuation = None;
        while block_index < cell.blocks.len() {
            match &cell.blocks[block_index] {
                Block::Floating(_) => {
                    block_index += 1;
                    line = 0;
                }
                Block::Paragraph(paragraph) => {
                    // Only the first continued block needs `force`; later blocks
                    // are left for the next page when they do not fit.
                    let force_this = force && block_index == resume.map_or(0, |item| item.block);
                    let wrapped = !self.frame_exclusions.is_empty()
                        || resume.is_some_and(|item| item.wrapped);
                    if wrapped {
                        let (next, total, placed) = self.place_paragraph_bounded_wrapped(
                            paragraph,
                            line,
                            frame.bottom,
                            force_this,
                        );
                        placed_any |= placed;
                        if next < total {
                            continuation = Some(CellContinuation {
                                block: block_index,
                                line: next,
                                wrapped: true,
                            });
                            break;
                        }
                        block_index += 1;
                        line = 0;
                    } else {
                        let (next, total) =
                            self.place_paragraph_bounded(paragraph, line, frame.bottom, force_this);
                        placed_any |= next > line;
                        if next < total {
                            continuation = Some(CellContinuation {
                                block: block_index,
                                line: next,
                                wrapped: false,
                            });
                            break;
                        }
                        block_index += 1;
                        line = 0;
                    }
                }
                block => {
                    if self.cursor > frame.bottom + 1e-6 && placed_any {
                        continuation = Some(CellContinuation {
                            block: block_index,
                            line: 0,
                            wrapped: false,
                        });
                        break;
                    }
                    let before = self.cursor;
                    self.place_block(block);
                    placed_any |= self.cursor > before || matches!(block, Block::Paragraph(_));
                    block_index += 1;
                    line = 0;
                }
            }
        }

        let bottom = self.cursor.max(self.frame_float_bottom);
        self.frame = saved_frame;
        self.cursor = saved_cursor;
        self.in_frame = saved_in_frame;
        self.frame_exclusions = saved_exclusions;
        self.frame_element_start = saved_element_start;
        self.frame_float_bottom = saved_float_bottom;
        CellOutcome {
            bottom,
            continuation,
            placed_any,
        }
    }

    /// Draw the border of every cell in a row.
    pub(crate) fn draw_row_borders(
        &mut self,
        table: &Table,
        widths: &[f64],
        index: usize,
        top: f64,
        bottom: f64,
        table_left: f64,
    ) {
        let rows = table.rows.len();
        let columns = widths.len();
        let left = table_left;
        let mut x = left;
        let mut column = 0_usize;
        for cell in &table.rows[index].cells {
            let span = usize::try_from(cell.span.max(1)).unwrap_or(1);
            let width: f64 = widths.iter().skip(column).take(span).sum();
            if width <= 0.0 {
                column += span;
                continue;
            }
            let is_first_row = index == 0;
            let is_last_row = index + 1 == rows;
            let is_first_column = column == 0;
            let is_last_column = column + span >= columns;
            let horizontal = if is_first_row || is_last_row {
                table.borders.outer.or(table.borders.horizontal)
            } else {
                table.borders.horizontal.or(table.borders.outer)
            };
            let vertical = if is_first_column || is_last_column {
                table.borders.outer.or(table.borders.vertical)
            } else {
                table.borders.vertical.or(table.borders.outer)
            };
            if let Some(border) = cell.borders.top.or(horizontal) {
                // The top of a vertically merged continuation cell is covered by
                // the cell above, so no line is drawn there.
                if cell.v_merge != ofdbox_folio_ir::VMerge::Continue {
                    self.line(Point::new(x, top), Point::new(x + width, top), border);
                }
            }
            if let Some(border) = cell.borders.bottom.or(horizontal) {
                self.line(Point::new(x, bottom), Point::new(x + width, bottom), border);
            }
            if let Some(border) = cell.borders.left.or(vertical) {
                self.line(Point::new(x, top), Point::new(x, bottom), border);
            }
            if let Some(border) = cell.borders.right.or(vertical) {
                self.line(
                    Point::new(x + width, top),
                    Point::new(x + width, bottom),
                    border,
                );
            }
            x += width;
            column += span;
        }
    }

    /// Draw one table border line.
    pub(crate) fn line(&mut self, from: Point, to: Point, border: Border) {
        if border.width.to_mm() <= 0.0 {
            return;
        }
        let mut path = Path::new();
        path.move_to(from);
        path.line_to(to);
        self.page.scene.children.push(Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: None,
            stroke: Some(StrokePaint {
                paint: Paint::Color(border.color),
                style: StrokeStyle {
                    width: border.width.to_mm().max(0.05),
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    ..StrokeStyle::default()
                },
            }),
            rule: FillRule::NonZero,
        }));
    }

    /// Space before a paragraph: dropped at the top of a page.
    pub(crate) fn advance_space(&mut self, space: Twips) {
        let space = space.to_mm().max(0.0);
        if space <= 0.0 {
            return;
        }
        if self.cursor <= self.margins.top + 1e-6 {
            return;
        }
        self.cursor += space;
    }

    pub(crate) fn place_paragraph(&mut self, paragraph: &Paragraph, total_pages: Option<usize>) {
        let laid = self.lay_out_paragraph(paragraph, total_pages);
        let LaidParagraph {
            style,
            pieces,
            links,
            images,
            lines,
            inset_left,
        } = laid;

        self.advance_space(style.space_before);

        if paragraph.heading_level.is_some() {
            let title = paragraph_text(paragraph);
            if !title.trim().is_empty() {
                let level = paragraph.heading_level.unwrap_or(1).max(1);
                let page = self.fixed.pages.len();
                self.headings.push((level, title, page, self.cursor));
            }
        }

        let line_count = lines.len();

        // Square floating objects: paragraphs that vertically overlap the object
        // are re-laid out line by line (each line breaks against the width
        // available at that moment).
        if !self.in_frame && line_count > 0 {
            // Probe with a whole-paragraph height estimate: as soon as any part
            // falls in a wrap band, switch to the line-by-line path.
            let first_size = self.max_size(&pieces, &lines[0]);
            let estimate = line_count as f64 * (first_size * 1.2).max(1.0);
            let (probe_left, probe_right) =
                self.floating_insets(self.cursor, self.cursor + estimate);
            if probe_left > 0.0 || probe_right > 0.0 {
                // When the object covers almost the whole text width there is no
                // usable side band; move the paragraph below the object instead
                // of wrapping it one character per line.
                let available = self.content_width() - probe_left - probe_right;
                if available < 8.0 {
                    let mut below = self.cursor;
                    for exclusion in &self.float_exclusions {
                        if exclusion.bottom > below && exclusion.top < self.cursor + estimate {
                            below = below.max(exclusion.bottom);
                        }
                    }
                    self.cursor = below;
                } else {
                    self.place_paragraph_wrapped(&style, &pieces, &links, &images);
                    return;
                }
            }
        }

        // keep_lines_together: if the whole paragraph does not fit, move all of it
        // to the next column / page.
        if !self.in_frame && self.page_body && style.keep_lines_together && line_count > 0 {
            let block = Block::Paragraph(paragraph.clone());
            let total = self.measure_blocks(std::slice::from_ref(&block), self.content_width());
            let content = self.bottom() - self.frame.top;
            if total <= content && self.cursor + total > self.bottom() + 1e-6 {
                self.new_flow();
            }
        }

        let mut index = 0_usize;
        while index < line_count {
            // How many more lines still fit on this page.
            let fit = if self.in_frame {
                line_count - index
            } else {
                self.lines_that_fit(&style, &pieces, &images, &lines, index, self.bottom())
            };
            if fit == 0 && !self.in_frame && self.page_body {
                // Not even one line fits: move to the next column / page and
                // recompute.
                self.new_flow();
                continue;
            }
            let mut take = fit.max(1);
            let at_top = self.cursor <= self.frame.top + 0.01;
            // Widow: only 1 line fits on this page while more content follows,
            // and this is not the top of the page → move the whole paragraph.
            if !self.in_frame && self.page_body && take == 1 && index + 1 < line_count && !at_top {
                self.new_flow();
                continue;
            }
            // Orphan: only 1 line would be left after the page break → place one
            // fewer line here so the next page gets at least two.
            if !self.in_frame && self.page_body && take >= 2 && index + take + 1 == line_count {
                take -= 1;
            }
            for offset in 0..take {
                let line = &lines[index + offset];
                // The line box height and baseline are both driven by the font
                // metrics; images stretch the line box by their height.
                let line_box = self.line_box(&style, &pieces, &images, line);
                let baseline = self.cursor + line_box.ascent;
                let first = index + offset == 0;
                let last = index + offset + 1 == line_count;
                self.emit_line(
                    &style, &pieces, line, first, last, baseline, &links, &images, inset_left,
                );
                self.cursor += line_box.height;
            }
            index += take;
            if index < line_count && !self.in_frame && self.page_body {
                self.new_flow();
            }
        }
        self.cursor += style.space_after.to_mm().max(0.0);
    }

    /// Line-by-line wrapping: re-break each line against the width available at
    /// the current y, going around square floating objects.
    pub(crate) fn place_paragraph_wrapped(
        &mut self,
        style: &ParaStyle,
        pieces: &[Piece],
        links: &[(usize, usize, LinkTarget)],
        images: &[MeasuredImage],
    ) {
        let content = self.content_width();
        let mut from = 0_usize;
        let mut first = true;
        let mut guard = 0_usize;
        while from < pieces.len() {
            guard += 1;
            if guard > pieces.len() * 4 + 32 {
                break;
            }
            // Estimate the line height from the current font size to decide which
            // wrap band this line falls into.
            let estimate = self.max_size(
                pieces,
                &LineSpan {
                    start: from,
                    end: (from + 1).min(pieces.len()),
                },
            ) * 1.2;
            let (left, right) = self.floating_insets(self.cursor, self.cursor + estimate);
            let indent_left = style.left_indent.to_mm().max(0.0) + left;
            let indent_right = style.right_indent.to_mm().max(0.0) + right;
            let width = (content - indent_left - indent_right).max(1.0);
            let first_indent = if first {
                style.first_line_indent.to_mm()
            } else {
                0.0
            };
            let first_width = (width - first_indent).max(1.0);
            let lines = linebreak::break_lines(&pieces[from..], first_width, width);
            let Some(relative) = lines.first() else {
                break;
            };
            let span = LineSpan {
                start: from + relative.start,
                end: from + relative.end,
            };
            // Trailing whitespace produces an empty span; stop instead of
            // re-emitting the same (empty) line forever.
            if span.end <= from {
                break;
            }
            let line_box = self.line_box(style, pieces, images, &span);
            if self.cursor + line_box.height > self.bottom() + 1e-6 && self.page_body {
                self.new_flow();
                continue;
            }
            let baseline = self.cursor + line_box.ascent;
            let last = span.end >= pieces.len();
            self.emit_line(
                style, pieces, &span, first, last, baseline, links, images, left,
            );
            self.cursor += line_box.height;
            from = span.end;
            first = false;
        }
        self.cursor += style.space_after.to_mm().max(0.0);
    }

    /// How many lines, starting from line `start`, still fit entirely in the
    /// current frame.
    pub(crate) fn lines_that_fit(
        &self,
        style: &ParaStyle,
        pieces: &[Piece],
        images: &[MeasuredImage],
        lines: &[LineSpan],
        start: usize,
        bottom: f64,
    ) -> usize {
        let mut y = self.cursor;
        let mut count = 0;
        for line in &lines[start..] {
            let height = self.line_box(style, pieces, images, line).height;
            if y + height > bottom + 1e-6 {
                break;
            }
            y += height;
            count += 1;
        }
        count
    }

    /// Measure a paragraph: style, line breaking pieces and line ranges.
    pub(crate) fn lay_out_paragraph(
        &mut self,
        paragraph: &Paragraph,
        total_pages: Option<usize>,
    ) -> LaidParagraph {
        let style = self.resolve_paragraph_style(&paragraph.style);
        let page_number = self.page_number();
        let (pieces, links, images) = self.measure_paragraph(paragraph, page_number, total_pages);
        let content = self.content_width();
        // Break lines against the full width here; paragraphs that overlap a
        // floating object are re-laid out line by line by
        // `place_paragraph_wrapped`.
        let indent_left = style.left_indent.to_mm().max(0.0);
        let indent_right = style.right_indent.to_mm().max(0.0);
        let width = (content - indent_left - indent_right).max(1.0);
        let first_width = (width - style.first_line_indent.to_mm()).max(1.0);
        let lines = linebreak::break_lines(&pieces, first_width, width);
        LaidParagraph {
            style,
            pieces,
            links,
            images,
            lines,
            inset_left: 0.0,
        }
    }

    /// Lay out a paragraph inside the frame, up to the bottom boundary `limit`;
    /// returns (next line not yet laid out, total line count).
    ///
    /// When `force` is true at least one line is laid out, so that splitting
    /// always makes progress.
    pub(crate) fn place_paragraph_bounded(
        &mut self,
        paragraph: &Paragraph,
        start_line: usize,
        limit: f64,
        force: bool,
    ) -> (usize, usize) {
        let LaidParagraph {
            style,
            pieces,
            links,
            images,
            lines,
            inset_left,
        } = self.lay_out_paragraph(paragraph, self.total_pages);
        let line_count = lines.len();
        if start_line == 0 {
            self.advance_space(style.space_before);
        }
        let mut index = start_line;
        while index < line_count {
            let line = &lines[index];
            // The line box height and baseline are both driven by the font
            // metrics; images stretch the line box by their height.
            let line_box = self.line_box(&style, &pieces, &images, line);
            let height = line_box.height;
            let fits = self.cursor + height <= limit + 1e-6;
            if !fits && !(force && index == start_line) {
                break;
            }
            let baseline = self.cursor + line_box.ascent;
            self.emit_line(
                &style,
                &pieces,
                line,
                index == 0,
                index + 1 == line_count,
                baseline,
                &links,
                &images,
                inset_left,
            );
            self.cursor += height;
            index += 1;
        }
        if index >= line_count {
            self.cursor += style.space_after.to_mm().max(0.0);
        }
        (index, line_count)
    }

    /// Lay out a cell/header paragraph with frame-local floating exclusions.
    ///
    /// Uses a piece index as the continuation position so a paragraph split
    /// across row fragments resumes at the right character.
    pub(crate) fn place_paragraph_bounded_wrapped(
        &mut self,
        paragraph: &Paragraph,
        start_piece: usize,
        limit: f64,
        force: bool,
    ) -> (usize, usize, bool) {
        let LaidParagraph {
            style,
            pieces,
            links,
            images,
            lines: _,
            inset_left: _,
        } = self.lay_out_paragraph(paragraph, self.total_pages);
        let total = pieces.len();
        if start_piece == 0 {
            self.advance_space(style.space_before);
        }
        let mut from = start_piece;
        let mut placed = false;
        while from < total {
            let estimate = self.max_size(
                &pieces,
                &LineSpan {
                    start: from,
                    end: (from + 1).min(total),
                },
            ) * 1.2;
            let (left, right) = self.floating_insets(self.cursor, self.cursor + estimate);
            let indent_left = style.left_indent.to_mm().max(0.0) + left;
            let indent_right = style.right_indent.to_mm().max(0.0) + right;
            let width = (self.frame.width - indent_left - indent_right).max(1.0);
            let first_indent = if from == 0 {
                style.first_line_indent.to_mm()
            } else {
                0.0
            };
            let first_width = (width - first_indent).max(1.0);
            let lines = linebreak::break_lines(&pieces[from..], first_width, width);
            let Some(relative) = lines.first() else {
                break;
            };
            let span = LineSpan {
                start: from + relative.start,
                end: from + relative.end,
            };
            if span.end <= from {
                break;
            }
            let line_box = self.line_box(&style, &pieces, &images, &span);
            let fits = self.cursor + line_box.height <= limit + 1e-6;
            if !fits && !(force && from == start_piece) {
                break;
            }
            let baseline = self.cursor + line_box.ascent;
            self.emit_line(
                &style,
                &pieces,
                &span,
                from == 0,
                span.end >= total,
                baseline,
                &links,
                &images,
                left,
            );
            self.cursor += line_box.height;
            from = span.end;
            placed = true;
        }
        if from >= total {
            self.cursor += style.space_after.to_mm().max(0.0);
        }
        (from, total, placed)
    }

    /// Height of one line: single spacing is computed from the font's
    /// ascent/descent/lineGap, and the multiplier and fixed line spacing adjust
    /// that base.
    pub(crate) fn line_height(&self, style: &ParaStyle, pieces: &[Piece], line: &LineSpan) -> f64 {
        let size = self.max_size(pieces, line);
        let (_, height_em) = self.font_metrics(pieces, line);
        let single = size * height_em;
        let base = match style.line_spacing {
            LineSpacing::Single => single,
            LineSpacing::Multiple(factor) if factor.is_finite() && factor > 0.0 => single * factor,
            LineSpacing::Multiple(_) => single,
            LineSpacing::Exact(value) => value.to_mm().max(0.1),
            LineSpacing::AtLeast(value) => value.to_mm().max(single),
            _ => single,
        };
        if base <= 0.0 { single } else { base }
    }

    /// Vertical metrics of the line's primary font: (ascent, single line height),
    /// in em.
    pub(crate) fn font_metrics(&self, pieces: &[Piece], line: &LineSpan) -> (f64, f64) {
        // An empty line (an empty paragraph or a whitespace-only one) has no
        // pieces in its span; fall back to all pieces of the paragraph so the
        // line still uses the paragraph's font metrics.
        let range = if line.start < line.end {
            &pieces[line.start..line.end]
        } else {
            pieces
        };
        let mut best: Option<(f64, (f64, f64))> = None;
        for piece in range {
            let Some(style) = self.styles.get(piece.style) else {
                continue;
            };
            let size = style.size.to_mm();
            let key = FontKey::new(&style.family.name, style.bold, style.italic);
            let metrics = match self.fonts.get(&key) {
                Some(font) => (font.ascender.max(0.1), font.line_height()),
                // Fall back to empirical values when the font could not be
                // resolved, matching the old behavior.
                None => (0.8, 1.2),
            };
            if best.is_none_or(|(best_size, _)| size > best_size) {
                best = Some((size, metrics));
            }
        }
        best.map(|(_, metrics)| metrics).unwrap_or((0.8, 1.2))
    }

    /// Line box height and baseline offset (relative to the line box top) of one
    /// line.
    pub(crate) fn line_box(
        &self,
        style: &ParaStyle,
        pieces: &[Piece],
        images: &[MeasuredImage],
        line: &LineSpan,
    ) -> LineBox {
        let size = self.max_size(pieces, line);
        let image_height = line_image_height(pieces, line, images);
        let (ascent_em, _) = self.font_metrics(pieces, line);
        let height = self
            .line_height(style, pieces, line)
            .max(image_height + 0.2 * size);
        // Baseline: text is shifted down by the font ascent, and sits on the image
        // bottom when the line contains an image.
        let ascent = (size * ascent_em).max(image_height);
        let ascent = ascent.min((height - 0.1).max(0.0));
        LineBox { height, ascent }
    }

    pub(crate) fn max_size(&self, pieces: &[Piece], line: &LineSpan) -> f64 {
        // Empty lines fall back to all pieces of the paragraph, and a fully
        // empty paragraph falls back to the default text size so it still
        // occupies a normal line.
        let range = if line.start < line.end {
            &pieces[line.start..line.end]
        } else {
            pieces
        };
        let size = range
            .iter()
            .filter_map(|piece| self.styles.get(piece.style))
            .map(|style| style.size.to_mm())
            .fold(0.0_f64, f64::max);
        if size > 0.0 {
            size
        } else {
            TextStyle::default().size.to_mm()
        }
    }

    // ---- Measurement ----

    pub(crate) fn measure_paragraph(
        &mut self,
        paragraph: &Paragraph,
        page_number: usize,
        total_pages: Option<usize>,
    ) -> Measured {
        let mut pieces = Vec::new();
        let mut links = Vec::new();
        let mut images = Vec::new();
        self.measure_inlines(
            &paragraph.runs,
            page_number,
            total_pages,
            &mut pieces,
            &mut links,
            &mut images,
        );
        (pieces, links, images)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn measure_inlines(
        &mut self,
        inlines: &[Inline],
        page_number: usize,
        total_pages: Option<usize>,
        pieces: &mut Vec<Piece>,
        links: &mut Vec<(usize, usize, LinkTarget)>,
        images: &mut Vec<MeasuredImage>,
    ) {
        for inline in inlines {
            match inline {
                Inline::Text(run) => {
                    let style = self.resolve_char_style(&run.style, run.style_id.as_deref());
                    let index = self.intern_style(&style);
                    self.push_text(&run.text, &style, index, pieces);
                }
                Inline::Link(link) => {
                    let start = pieces.len();
                    self.measure_inlines(
                        &link.runs,
                        page_number,
                        total_pages,
                        pieces,
                        links,
                        images,
                    );
                    if pieces.len() > start {
                        links.push((start, pieces.len(), link.target.clone()));
                    }
                }
                Inline::Field(field) => {
                    let style = self.resolve_char_style(&TextStyle::default(), None);
                    let index = self.intern_style(&style);
                    let text = match &field.kind {
                        FieldKind::PageNumber => page_number.to_string(),
                        FieldKind::TotalPages => total_pages
                            .map(|total| total.to_string())
                            .unwrap_or_default(),
                        _ => field.cached_text.clone().unwrap_or_default(),
                    };
                    self.push_text(&text, &style, index, pieces);
                }
                Inline::Note(note) => {
                    // Reference marker: a smaller number laid out as normal text;
                    // page-bottom space for the note area is reserved at
                    // placement time.
                    let mut style = self.resolve_char_style(&TextStyle::default(), None);
                    style.size = Twips(style.size.0 * 7 / 10);
                    let index = self.intern_style(&style);
                    let number = self.note_number(*note);
                    let text = number.to_string();
                    let start = pieces.len();
                    self.push_text(&text, &style, index, pieces);
                    if let Some(first) = pieces.get_mut(start) {
                        first.note = Some(*note);
                    }
                }
                Inline::Image(image) => {
                    let style = self.resolve_char_style(&TextStyle::default(), None);
                    let index = self.intern_style(&style);
                    match self.image_size_of(image.image, image.size) {
                        Some((width, height)) => {
                            images.push(MeasuredImage {
                                asset: image.image,
                                width,
                                height,
                            });
                            pieces.push(Piece::object(width, index, images.len() - 1));
                        }
                        None => {
                            self.diagnostic(
                                DiagnosticCode::Image,
                                "inline image has no resource; skipped",
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn push_text(
        &mut self,
        text: &str,
        style: &TextStyle,
        index: usize,
        pieces: &mut Vec<Piece>,
    ) {
        let key = FontKey::new(&style.family.name, style.bold, style.italic);
        let size = style.size.to_mm();
        for ch in text.chars() {
            if ch == '\n' {
                pieces.push(Piece::text('\n', 0.0, index));
                continue;
            }
            if ch == '\t' {
                // A tab's width can only be determined once the current x is
                // known, so it is recorded as 0 during measurement.
                pieces.push(Piece::text('\t', 0.0, index));
                continue;
            }
            // Use the fallback font's glyph width when the primary font lacks the
            // character; only when both lack it is the character counted as
            // missing.
            let (width, missing) = match self.fonts.get(&key) {
                Some(data) if !data.is_missing(ch) => (data.advance(ch) * size, false),
                _ => match self.fonts.fallback() {
                    Some(fallback) if !fallback.is_missing(ch) => {
                        (fallback.advance(ch) * size, false)
                    }
                    _ => (0.5 * size, true),
                },
            };
            if missing {
                self.missing_chars += 1;
            }
            pieces.push(Piece::text(ch, width, index));
        }
    }

    pub(crate) fn image_size_of(
        &mut self,
        id: ofdbox_folio_ir::AssetId,
        size: Option<(Twips, Twips)>,
    ) -> Option<(f64, f64)> {
        if let Some((width, height)) = size {
            return Some((width.to_mm(), height.to_mm()));
        }
        let asset = self.document.resources.image(id)?;
        let dpi = asset
            .dpi
            .map(|(x, _)| x)
            .unwrap_or(self.options.default_image_dpi);
        let dpi_x = if dpi.is_finite() && dpi > 0.0 {
            dpi
        } else {
            self.options.default_image_dpi
        };
        let dpi_y = asset
            .dpi
            .map(|(_, y)| y)
            .filter(|y| y.is_finite() && *y > 0.0)
            .unwrap_or(dpi_x);
        Some((
            f64::from(asset.pixel_width) / dpi_x * 25.4,
            f64::from(asset.pixel_height) / dpi_y * 25.4,
        ))
    }

    // ---- Placement ----

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_line(
        &mut self,
        style: &ParaStyle,
        pieces: &[Piece],
        line: &LineSpan,
        first: bool,
        last: bool,
        baseline: f64,
        links: &[(usize, usize, LinkTarget)],
        images: &[MeasuredImage],
        inset_left: f64,
    ) {
        if line.is_empty() {
            return;
        }
        let indent_left = style.left_indent.to_mm().max(0.0) + inset_left;
        let first_indent = if first {
            style.first_line_indent.to_mm()
        } else {
            0.0
        };
        let mut x = self.frame.left + indent_left + first_indent;
        let width = self.content_width() - indent_left - style.right_indent.to_mm().max(0.0);
        let natural: f64 = pieces[line.start..line.end]
            .iter()
            .map(|piece| piece.width)
            .sum();
        let mut extra = vec![0.0_f64; line.len()];

        // Justification: the last line is left-aligned; spaces are stretched
        // first, and when there are none the slack is distributed between
        // characters.
        if style.align == Align::Justify && !last && width > natural + 1e-6 {
            let slack = width - natural;
            let spaces: Vec<usize> = (line.start..line.end)
                .filter(|index| pieces[*index].is_space())
                .collect();
            if !spaces.is_empty() {
                let each = slack / spaces.len() as f64;
                for index in spaces {
                    extra[index - line.start] = each;
                }
            } else if line.len() > 1 {
                let each = slack / (line.len() - 1) as f64;
                for slot in extra.iter_mut().take(line.len() - 1) {
                    *slot = each;
                }
            }
        } else {
            match style.align {
                Align::Center => x += (width - natural).max(0.0) / 2.0,
                Align::Right => x += (width - natural).max(0.0),
                _ => {}
            }
        }

        let line_start_item = self.page.items.len() as u32;
        let mut positions = Vec::with_capacity(line.len());
        for (offset, index) in (line.start..line.end).enumerate() {
            positions.push(x);
            let piece = pieces[index];
            if piece.is_tab() {
                // A tab pushes x to the next tab stop and draws a leader when
                // needed.
                x = self.place_tab(style, pieces, line, index, x, baseline);
                continue;
            }
            let advance = piece.width + extra[offset];
            if let Some(object) = piece.object {
                if let Some(image) = images.get(object) {
                    self.place_image(image, x, baseline, piece.width);
                }
            } else {
                self.place_text_piece(piece, x, baseline);
            }
            x += advance;
        }
        let line_end_item = self.page.items.len() as u32;
        // Footnote references on this line must reserve page-bottom space on this
        // page.
        if !self.in_frame {
            self.reserve_notes_in_line(pieces, line);
        }

        // Line box and baseline.
        let line_width = (x - self.frame.left - indent_left).max(0.0);
        let bbox = Rect::new(
            self.frame.left + indent_left,
            baseline - self.max_size(pieces, line),
            line_width,
            self.max_size(pieces, line) * 1.2,
        );
        let mut bbox = bbox;
        for item in &self.page.items[line_start_item as usize..line_end_item as usize] {
            bbox = union(bbox, item.bbox);
        }
        if line_end_item > line_start_item {
            self.page.lines.push(TextLine {
                items: line_start_item..line_end_item,
                bbox,
                baseline,
            });
        }

        // Link areas.
        for (start, end, target) in links {
            let from = (*start).max(line.start);
            let to = (*end).min(line.end);
            if from >= to {
                continue;
            }
            let left = positions[from - line.start];
            let right = positions[(to - 1) - line.start] + pieces[to - 1].width;
            self.page.links.push(LinkArea {
                rect: Rect::new(
                    left,
                    baseline - self.max_size(pieces, line),
                    right - left,
                    self.max_size(pieces, line) * 1.2,
                ),
                target: target.clone(),
            });
        }
    }

    /// Tab placement: push x to the next tab stop and draw a leader when needed.
    ///
    /// The width of the text after the tab stop is already known from
    /// measurement (excluding the tab itself), so right / center / decimal
    /// alignment can all be computed here in one go: first measure the width
    /// from the tab to the end of the line, then decide the start position from
    /// it, and finally fill the remaining gap with the leader.
    pub(crate) fn place_tab(
        &mut self,
        style: &ParaStyle,
        pieces: &[Piece],
        line: &LineSpan,
        index: usize,
        x: f64,
        baseline: f64,
    ) -> f64 {
        // Only align up to the end of this line; trailing whitespace does not
        // participate.
        let mut remaining = 0.0;
        let mut decimal_width = None;
        for piece in &pieces[index + 1..line.end] {
            let piece = *piece;
            if piece.is_tab() {
                break;
            }
            if decimal_width.is_none() && piece.ch == '.' {
                // Width left of the decimal point: move the decimal point itself
                // onto the tab stop.
                decimal_width = Some(remaining);
            }
            remaining += piece.width;
        }

        let left = self.frame.left;
        // Body right edge: neither the tab stop nor the following content may
        // cross it.
        let right = left + self.content_width();
        let default_step = DEFAULT_TAB_STEP;
        let stop = style
            .tabs
            .iter()
            .filter(|stop| stop.position.to_mm() > x - left + 0.01)
            .min_by(|a, b| a.position.cmp(&b.position));
        let (target, align, leader) = match stop {
            Some(stop) => (
                (left + stop.position.to_mm().max(0.0)).min(right),
                stop.align,
                stop.leader,
            ),
            None => {
                // No explicit tab stop: advance by the default step to the next
                // grid position.
                let offset = x - left;
                let next = ((offset / default_step).floor() + 1.0) * default_step;
                ((left + next).min(right), TabAlign::Left, TabLeader::None)
            }
        };

        // Tab stop declared outside the body width: clip to the right edge and
        // record a diagnostic.
        if let Some(stop) = stop
            && stop.position.to_mm() > self.content_width() + 0.01
        {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "tab stop exceeds the body width; clipped to the right edge",
            );
        }
        let mut start = match align {
            TabAlign::Left => target,
            TabAlign::Right => target - remaining,
            TabAlign::Center => target - remaining / 2.0,
            TabAlign::Decimal => match decimal_width {
                Some(width) => target - width,
                None => target - remaining,
            },
            _ => target,
        };
        // The content after the tab stop does not fit in the remaining space: stop
        // right-aligning and place it directly after the preceding text to avoid
        // overlap.
        if start < x {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "content after the tab stop does not fit; placed directly after the preceding text",
            );
            start = x;
        }
        // Still out of bounds (tab stop declared outside the page margins): clip to
        // the body right edge.
        let start = start.min((right - remaining).max(x));

        if start > x + 0.4 {
            let color = self
                .styles
                .get(pieces[index].style)
                .map(|style| style.color)
                .unwrap_or(Rgba::BLACK);
            self.draw_leader(leader, x, start, baseline, color);
        }
        start
    }

    /// Draw the tab leader.
    pub(crate) fn draw_leader(
        &mut self,
        leader: TabLeader,
        from: f64,
        to: f64,
        baseline: f64,
        color: Rgba,
    ) {
        let style = match leader {
            TabLeader::None => return,
            TabLeader::Dot => StrokeStyle {
                width: 0.35,
                cap: LineCap::Round,
                join: LineJoin::Round,
                dash: vec![0.01, 1.1],
                dash_offset: 0.0,
                ..StrokeStyle::default()
            },
            TabLeader::Dash => StrokeStyle {
                width: 0.25,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                dash: vec![1.5, 1.0],
                dash_offset: 0.0,
                ..StrokeStyle::default()
            },
            TabLeader::Line => StrokeStyle {
                width: 0.25,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                dash: Vec::new(),
                dash_offset: 0.0,
                ..StrokeStyle::default()
            },
            _ => return,
        };
        let mut path = Path::new();
        path.move_to(Point::new(from, baseline));
        path.line_to(Point::new(to, baseline));
        self.page.scene.children.push(Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: None,
            stroke: Some(StrokePaint {
                paint: Paint::Color(color),
                style,
            }),
            rule: FillRule::NonZero,
        }));
    }

    pub(crate) fn place_text_piece(&mut self, piece: Piece, x: f64, baseline: f64) {
        let Some(style) = self.styles.get(piece.style).cloned() else {
            return;
        };
        let key = FontKey::new(&style.family.name, style.bold, style.italic);
        // Switch to the fallback font when the primary font lacks this glyph:
        // setting CJK text in an English style is very common in mixed
        // Chinese/English layout.
        let (use_fallback, glyph) = match self.fonts.get(&key) {
            Some(data) if !data.is_missing(piece.ch) => (false, data.glyph(piece.ch)),
            _ => match self.fonts.fallback() {
                Some(fallback) if !fallback.is_missing(piece.ch) => {
                    (true, fallback.glyph(piece.ch))
                }
                // Neither font has this glyph: still write .notdef so the text
                // layer stays complete.
                _ => (false, 0),
            },
        };
        let asset = if use_fallback {
            self.fallback_asset()
        } else {
            self.font_asset(&key)
        };
        let Some(font) = asset else {
            return;
        };
        let size = style.size.to_mm();
        let run = Arc::new(ofdbox_folio_ir::GlyphRun::new(
            piece.ch.to_string(),
            vec![ofdbox_folio_ir::Glyph {
                id: glyph,
                x: 0.0,
                y: 0.0,
                advance: piece.width,
            }],
            vec![ofdbox_folio_ir::Cluster {
                text_start: 0,
                text_end: 1,
                glyph_start: 0,
                glyph_end: 1,
            }],
        ));
        let transform = Matrix::translate(x, baseline);
        let element = TextElement {
            font: Arc::clone(&font),
            font_size: size,
            transform,
            run: Arc::clone(&run),
            fill: Some(Paint::Color(style.color)),
            stroke: None,
        };
        self.page.scene.children.push(Element::Text(element));
        let bbox = text_bbox(&run, size, transform);
        self.page.items.push(TextItem {
            run,
            transform,
            bbox,
            font,
            font_size: size,
        });
    }

    /// Place an inline image: its bottom edge sits on the baseline.
    pub(crate) fn place_image(&mut self, image: &MeasuredImage, x: f64, baseline: f64, width: f64) {
        let Some(stored) = self.register_image(image.asset) else {
            return;
        };
        let height = image.height.max(0.1);
        let width = width.min(image.width).max(0.1);
        let transform = Matrix::scale(
            width / f64::from(stored.pixel_width.max(1)),
            height / f64::from(stored.pixel_height.max(1)),
        )
        .then(Matrix::translate(x, baseline - height));
        self.page.scene.children.push(Element::Image(ImageElement {
            image: stored,
            rect: Rect::default(),
            transform,
            alpha: 1.0,
        }));
    }

    /// Register a Flow image into the Fixed resource table and return the handle
    /// carrying its ID.
    pub(crate) fn register_image(
        &mut self,
        id: ofdbox_folio_ir::AssetId,
    ) -> Option<Arc<ImageAsset>> {
        let (format, encoded, pixel_width, pixel_height, dpi, mask) = {
            let source = self.document.resources.image(id)?;
            (
                source.format,
                Arc::clone(&source.encoded),
                source.pixel_width,
                source.pixel_height,
                source.dpi,
                source.mask.clone(),
            )
        };
        let mut asset = ImageAsset::new(format, encoded, pixel_width, pixel_height);
        if let Some((x, y)) = dpi {
            asset = asset.with_dpi(x, y);
        }
        if let Some(mask) = mask {
            asset = asset.with_mask(mask);
        }
        let id = self.fixed.resources.add_image(asset);
        self.fixed.resources.image(id).cloned()
    }

    pub(crate) fn place_image_block(&mut self, image: &ImageBlock) {
        let Some((mut width, mut height)) = self.image_size_of(image.image, image.size) else {
            self.diagnostic(
                DiagnosticCode::Image,
                "block image has no resource; skipped",
            );
            return;
        };
        let content = self.content_width();
        if width > content {
            let scale = content / width;
            width = content;
            height *= scale;
            self.diagnostic(
                DiagnosticCode::Approximation,
                "image is wider than the text area; scaled proportionally to fit",
            );
        }
        if height > self.bottom() - self.cursor && !self.in_frame && self.page_body {
            self.new_flow();
        }
        let available = self.bottom() - self.cursor;
        if height > available {
            let scale = available / height;
            height = available.max(1.0);
            width *= scale;
            self.diagnostic(
                DiagnosticCode::Approximation,
                "image is taller than the page area; scaled proportionally to fit the current page",
            );
        }
        let Some(stored) = self.register_image(image.image) else {
            return;
        };
        let x = self.frame.left
            + match image.align {
                Align::Center => (content - width) / 2.0,
                Align::Right => content - width,
                _ => 0.0,
            };
        let transform = Matrix::scale(
            width / f64::from(stored.pixel_width.max(1)),
            height / f64::from(stored.pixel_height.max(1)),
        )
        .then(Matrix::translate(x, self.cursor));
        self.page.scene.children.push(Element::Image(ImageElement {
            image: stored,
            rect: Rect::default(),
            transform,
            alpha: 1.0,
        }));
        self.cursor += height;
    }
}
