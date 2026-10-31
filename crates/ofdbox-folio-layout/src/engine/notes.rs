//! notes layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Footnotes and endnotes ----

    /// Get (allocating if necessary) the note number, starting at 1.
    pub(crate) fn note_number(&mut self, note: NoteRef) -> usize {
        if let Some(number) = self.note_numbers.get(&(note.kind, note.id)) {
            return *number;
        }
        let number = match note.kind {
            NoteKind::Footnote => {
                self.next_footnote += 1;
                self.next_footnote
            }
            NoteKind::Endnote => {
                self.next_endnote += 1;
                self.next_endnote
            }
        };
        self.note_numbers.insert((note.kind, note.id), number);
        number
    }

    /// Find the note body.
    pub(crate) fn note_blocks(&self, note: NoteRef) -> Option<Vec<Block>> {
        self.document
            .notes
            .iter()
            .find(|item| item.id == note.id && item.kind == note.kind)
            .map(|item| item.blocks.clone())
    }

    /// Collect the note references on a line and reserve page-bottom space for
    /// them.
    pub(crate) fn reserve_notes_in_line(&mut self, pieces: &[Piece], line: &LineSpan) {
        for piece in &pieces[line.start..line.end] {
            let Some(note) = piece.note else {
                continue;
            };
            if self.page_notes.iter().any(|item| item.note == note)
                || self.endnotes.iter().any(|item| item.note == note)
            {
                continue;
            }
            let Some(blocks) = self.note_blocks(note) else {
                self.diagnostic(
                    DiagnosticCode::Unsupported,
                    format!("note {} has no body; skipped", note.id),
                );
                continue;
            };
            let number = self.note_number(note);
            let blocks = with_note_number(note.kind, &blocks, number);
            match note.kind {
                NoteKind::Footnote => {
                    let width = (self.page_frame().width - NOTE_INDENT).max(1.0);
                    let height = self.measure_blocks(&blocks, width) + NOTE_GAP;
                    self.page_notes.push(PendingNote {
                        note,
                        blocks,
                        height,
                    });
                    self.bottom_inset += height;
                    self.refresh_frame_bottom();
                }
                NoteKind::Endnote => self.endnotes.push(PendingNote {
                    note,
                    blocks,
                    height: 0.0,
                }),
            }
        }
    }

    /// Recompute the current frame's bottom boundary after the note area grows.
    ///
    /// When the body has already been pushed past the page bottom by the notes,
    /// `cursor` is not pulled back: that line is already placed, and later lines
    /// will move to the next column / page automatically because
    /// cursor + line height > bottom.
    pub(crate) fn refresh_frame_bottom(&mut self) {
        if self.in_frame {
            return;
        }
        self.frame.bottom = self.column_frame(self.column_index).bottom;
    }

    /// Lay out this page's footnotes at the bottom of the page.
    pub(crate) fn emit_page_notes(&mut self, bottom: f64) {
        let notes = std::mem::take(&mut self.page_notes);
        if notes.is_empty() {
            return;
        }
        let saved = (self.frame, self.cursor, self.in_frame);
        let width = self.page_frame().width;
        let total: f64 = notes
            .iter()
            .map(|note| note.height.max(NOTE_GAP * 2.0))
            .sum();
        let top = (bottom - total).max(0.0);
        self.draw_note_separator(top - NOTE_GAP / 2.0, width);
        self.layout_notes(&notes, top, width);
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
    }

    /// Lay out a list of notes (reused by endnotes); `top` is the top of the note
    /// area.
    pub(crate) fn emit_notes(&mut self, mut notes: Vec<PendingNote>, bottom: f64, separator: bool) {
        if notes.is_empty() {
            return;
        }
        let width = self.page_frame().width;
        let inner = (width - NOTE_INDENT).max(1.0);
        for note in &mut notes {
            if note.height <= 0.0 {
                note.height = self.measure_blocks(&note.blocks, inner) + NOTE_GAP;
            }
        }
        let total: f64 = notes.iter().map(|note| note.height).sum();
        let top = (bottom - total).max(0.0);
        if separator {
            self.draw_note_separator(top - NOTE_GAP / 2.0, width);
        }
        self.layout_notes(&notes, top, width);
    }

    /// The short rule above the note area.
    pub(crate) fn draw_note_separator(&mut self, y: f64, width: f64) {
        let mut path = Path::new();
        path.move_to(Point::new(self.margins.left, y));
        path.line_to(Point::new(self.margins.left + width / 3.0, y));
        self.page.scene.children.push(Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: None,
            stroke: Some(StrokePaint {
                paint: Paint::Color(Rgba::BLACK),
                style: StrokeStyle {
                    width: 0.2,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    dash: Vec::new(),
                    dash_offset: 0.0,
                    ..StrokeStyle::default()
                },
            }),
            rule: FillRule::NonZero,
        }));
    }

    /// Lay out the note bodies downward starting at `top`.
    pub(crate) fn layout_notes(&mut self, notes: &[PendingNote], top: f64, width: f64) {
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: self.margins.left + NOTE_INDENT,
            width: (width - NOTE_INDENT).max(1.0),
            top,
            bottom: f64::MAX,
        };
        self.cursor = top;
        for note in notes {
            for block in &note.blocks {
                self.place_block(block);
            }
            self.cursor += NOTE_GAP;
        }
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
    }

    /// Lay out the endnotes at the end of the document.
    pub(crate) fn flush_endnotes(&mut self) {
        let notes = std::mem::take(&mut self.endnotes);
        if notes.is_empty() {
            return;
        }
        self.new_page();
        self.start_page_body();
        let title = self.options.endnote_title.clone();
        let mut heading = Paragraph::plain(
            title,
            TextStyle {
                size: Twips::from_pt(14.0),
                ..TextStyle::default()
            },
        );
        heading.heading_level = None;
        self.place_paragraph(&heading, self.total_pages);
        self.emit_notes(notes, self.bottom(), false);
        self.finish_page();
    }
}
