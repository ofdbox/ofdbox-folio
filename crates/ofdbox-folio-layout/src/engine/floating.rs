//! floating layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Floating objects ----

    /// Lay out a floating object.
    ///
    /// During the probe pass this places the object and records a plan; during a
    /// replay pass the object has already been pre-placed at page start, so only
    /// the plan is recorded again.
    pub(crate) fn place_floating(&mut self, floating: &ofdbox_folio_ir::FloatingObject) {
        if self.in_frame {
            self.place_floating_in_frame(floating);
            return;
        }
        let index = self.float_seen;
        self.float_seen += 1;

        if self.float_plan_in.is_some() {
            let planned = self
                .float_plan_in
                .as_ref()
                .and_then(|plans| plans.iter().find(|plan| plan.index == index))
                .cloned();
            if let Some(plan) = planned {
                let (page, x, y, width, height) = self
                    .applied_geometry
                    .get(&index)
                    .copied()
                    .unwrap_or((plan.page, plan.x, plan.y, plan.width, plan.height));
                self.float_plan_out.push(FloatPlan {
                    index,
                    page,
                    x,
                    y,
                    width,
                    height,
                    wrap: floating.wrap,
                    behind_text: floating.behind_text,
                    object: floating.clone(),
                });
                return;
            }
            // A replay pass found an object without a plan (should not happen
            // normally); fall through and place it inline as a fallback.
        }

        let anchor_cursor = self.cursor;
        match self.place_floating_impl(floating, true) {
            FloatPlacement::Placed {
                x,
                y,
                width,
                height,
            } => {
                if y + 1e-6 < anchor_cursor {
                    // The object starts above the current body position, so text
                    // already placed on this page must be re-laid out around it.
                    self.float_replay_needed = true;
                }
                self.float_plan_out.push(FloatPlan {
                    index,
                    page: self.fixed.pages.len(),
                    x,
                    y,
                    width,
                    height,
                    wrap: floating.wrap,
                    behind_text: floating.behind_text,
                    object: floating.clone(),
                });
            }
            FloatPlacement::Deferred => {
                self.float_replay_needed = true;
                self.pending_floats.push(PendingFloat {
                    index,
                    object: floating.clone(),
                });
            }
        }
    }

    /// Place a floating object inside a table cell or header/footer frame.
    pub(crate) fn place_floating_in_frame(&mut self, floating: &ofdbox_folio_ir::FloatingObject) {
        let frame = self.frame;
        let width = floating
            .width
            .map(|width| width.to_mm())
            .unwrap_or(frame.width)
            .clamp(1.0, frame.width);
        let (from_h, from_v) = floating.position_from;
        let origin_x = match from_h {
            ofdbox_folio_ir::PositionFrom::Page => 0.0,
            _ => frame.left,
        };
        let origin_y = match from_v {
            ofdbox_folio_ir::PositionFrom::Page => 0.0,
            ofdbox_folio_ir::PositionFrom::Paragraph => self.cursor,
            _ => frame.top,
        };
        let x = (origin_x + floating.position.x).clamp(
            frame.left,
            (frame.left + frame.width - width).max(frame.left),
        );
        let y = (origin_y + floating.position.y).max(frame.top);
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: x,
            width,
            top: y,
            bottom: f64::MAX,
        };
        self.cursor = y;
        let first_element = self.page.scene.children.len();
        self.place_block(&floating.block);
        let bottom = self.cursor;
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
        if bottom > frame.bottom + 1e-6 {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "floating object inside a table cell/header overflows the cell",
            );
        }
        if floating.behind_text {
            let children = &mut self.page.scene.children;
            let elements: Vec<_> = children.drain(first_element..).collect();
            let insert_at = self.frame_element_start.min(children.len());
            children.splice(insert_at..insert_at, elements);
        }
        match floating.wrap {
            ofdbox_folio_ir::WrapMode::TopBottom => {
                if bottom > self.cursor {
                    self.cursor = bottom;
                }
            }
            ofdbox_folio_ir::WrapMode::Square => {
                self.frame_exclusions.push(FloatExclusion {
                    top: y,
                    bottom,
                    left: x,
                    right: x + width,
                });
            }
            _ => {}
        }
        self.frame_float_bottom = self.frame_float_bottom.max(bottom);
    }

    /// Place a floating object, deferring it to the next page when it does not
    /// fit in the remaining page height.
    pub(crate) fn place_floating_impl(
        &mut self,
        floating: &ofdbox_folio_ir::FloatingObject,
        allow_defer: bool,
    ) -> FloatPlacement {
        let page = self.page_frame();
        let content_height = page.bottom - page.top;
        let snapshot = self.snapshot();
        let width = floating
            .width
            .map(|width| width.to_mm())
            .unwrap_or(page.width)
            .clamp(1.0, page.width);
        // Convert the top-left corner according to the reference base. A
        // paragraph-relative anchor starts at the anchor paragraph's current
        // position, so a floating image does not jump to the top of the page.
        let (from_h, from_v) = floating.position_from;
        let origin_x = match from_h {
            ofdbox_folio_ir::PositionFrom::Page => 0.0,
            ofdbox_folio_ir::PositionFrom::Margin => page.left,
            ofdbox_folio_ir::PositionFrom::Column => self.frame.left,
            ofdbox_folio_ir::PositionFrom::Paragraph => self.frame.left,
            _ => page.left,
        };
        let origin_y = match from_v {
            ofdbox_folio_ir::PositionFrom::Page => 0.0,
            ofdbox_folio_ir::PositionFrom::Margin => page.top,
            ofdbox_folio_ir::PositionFrom::Column => self.frame.top,
            ofdbox_folio_ir::PositionFrom::Paragraph => self.cursor,
            _ => page.top,
        };
        let x = (origin_x + floating.position.x).clamp(0.0, page.left + page.width);
        let y = (origin_y + floating.position.y).max(0.0);
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: x,
            width,
            top: y,
            bottom: f64::MAX,
        };
        self.cursor = y;
        let first_element = self.page.scene.children.len();
        self.place_block(&floating.block);
        let bottom = self.cursor;
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;

        if allow_defer
            && !matches!(from_v, ofdbox_folio_ir::PositionFrom::Page)
            && bottom > page.bottom + 1e-6
            && bottom - y <= content_height + 1e-6
        {
            // Move the object to the next page instead of letting it overflow;
            // the next page places it at the top before its body content.
            self.restore(snapshot);
            return FloatPlacement::Deferred;
        }
        if bottom > page.bottom + 1e-6 {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "floating object does not fit on the page; allowed to overflow",
            );
        }

        if floating.behind_text {
            // `behindDoc="1"`: paint the floating content behind the body text
            // but after the header.
            let children = &mut self.page.scene.children;
            let floating_elements: Vec<_> = children.drain(first_element..).collect();
            let insert_at = self.body_element_start.min(children.len());
            children.splice(insert_at..insert_at, floating_elements);
        }

        match floating.wrap {
            ofdbox_folio_ir::WrapMode::None => {
                // Floating on top; the body is unaffected.
            }
            ofdbox_folio_ir::WrapMode::TopBottom => {
                // The body continues below the object.
                if bottom > self.cursor {
                    self.cursor = bottom;
                }
            }
            ofdbox_folio_ir::WrapMode::Square => {
                // Record an exclusion area: paragraphs that vertically overlap it
                // move to the other side of the object.
                self.float_exclusions.push(FloatExclusion {
                    top: y,
                    bottom,
                    left: x,
                    right: x + width,
                });
            }
            _ => {}
        }
        self.note_float_rect(x, y, width, bottom - y);
        FloatPlacement::Placed {
            x,
            y,
            width,
            height: bottom - y,
        }
    }

    /// Place deferred floating objects at the top of a new page.
    pub(crate) fn flush_pending_floats(&mut self) {
        if self.pending_floats.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_floats);
        let page = self.fixed.pages.len();
        for PendingFloat { index, object } in pending {
            if let FloatPlacement::Placed {
                x,
                y,
                width,
                height,
            } = self.place_floating_impl(&object, false)
            {
                self.float_plan_out.push(FloatPlan {
                    index,
                    page,
                    x,
                    y,
                    width,
                    height,
                    wrap: object.wrap,
                    behind_text: object.behind_text,
                    object,
                });
            }
        }
    }

    /// Pre-place the floating objects planned for the current page.
    pub(crate) fn place_planned_floats(&mut self) {
        let Some(plan_in) = self.float_plan_in.clone() else {
            return;
        };
        let page_index = self.fixed.pages.len();
        for plan in plan_in.iter().filter(|plan| plan.page == page_index) {
            self.place_planned(plan);
        }
    }

    /// Emit pages for planned floating objects whose target page was never
    /// reached by the body flow (for example a deferred object at document end).
    pub(crate) fn flush_unapplied_float_plans(&mut self) {
        let Some(plan_in) = self.float_plan_in.clone() else {
            return;
        };
        let mut plans: Vec<FloatPlan> = plan_in
            .into_iter()
            .filter(|plan| !self.applied_geometry.contains_key(&plan.index))
            .collect();
        plans.sort_by_key(|plan| plan.page);
        let mut started = false;
        for plan in plans {
            if self.fixed.pages.len() > plan.page {
                continue;
            }
            // The current page has already been finished; start a fresh page
            // for this plan (it is the next page at document end).
            self.start_page_after_finish();
            self.start_page_body();
            started = true;
            if !self.applied_geometry.contains_key(&plan.index) {
                self.place_planned(&plan);
            }
        }
        if started {
            self.finish_page();
        }
    }

    /// Begin a new blank page after the current one has already been finished.
    pub(crate) fn start_page_after_finish(&mut self) {
        self.page = PageBuffer::new();
        self.column_index = 0;
        self.bottom_inset = 0.0;
        self.float_exclusions.clear();
        self.placed_float_rects.clear();
        self.float_overlap_reported = false;
        self.front_elements.clear();
        self.body_element_start = 0;
        self.frame = self.column_frame(0);
        self.cursor = self.frame.top;
        self.page_body = false;
        self.page_started = false;
        self.section_page += 1;
    }

    /// Draw one planned floating object and register its wrap area.
    pub(crate) fn place_planned(&mut self, plan: &FloatPlan) {
        let page = self.page_frame();
        let width = plan.width.clamp(1.0, page.width);
        let x = plan.x.clamp(0.0, page.left + page.width);
        let y = plan.y.max(0.0);
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: x,
            width,
            top: y,
            bottom: f64::MAX,
        };
        self.cursor = y;
        let first_element = self.page.scene.children.len();
        self.place_block(&plan.object.block);
        let bottom = self.cursor;
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
        let height = (bottom - y).max(plan.height);

        // z-order: behind objects go before the body text, foreground objects
        // are appended at the end of the page.
        let elements: Vec<Element> = self.page.scene.children.drain(first_element..).collect();
        if plan.behind_text {
            let insert_at = self.body_element_start.min(self.page.scene.children.len());
            self.page
                .scene
                .children
                .splice(insert_at..insert_at, elements);
        } else {
            self.front_elements.extend(elements);
        }

        match plan.wrap {
            ofdbox_folio_ir::WrapMode::TopBottom => {
                if bottom > self.cursor {
                    self.cursor = bottom;
                }
            }
            ofdbox_folio_ir::WrapMode::Square => {
                self.float_exclusions.push(FloatExclusion {
                    top: y,
                    bottom: y + height,
                    left: x,
                    right: x + width,
                });
            }
            _ => {}
        }
        self.applied_geometry
            .insert(plan.index, (self.fixed.pages.len(), x, y, width, height));
        self.note_float_rect(x, y, width, height);
    }

    /// Record a placed floating rectangle and warn once when two objects
    /// overlap in the same page/column container.
    pub(crate) fn note_float_rect(&mut self, x: f64, y: f64, width: f64, height: f64) {
        let overlaps = self
            .placed_float_rects
            .iter()
            .any(|(other_x, other_y, w, h)| {
                x < other_x + w - 1e-6
                    && *other_x < x + width - 1e-6
                    && y < other_y + h - 1e-6
                    && *other_y < y + height - 1e-6
            });
        if overlaps && !self.float_overlap_reported {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "floating objects overlap on the same page/column",
            );
            self.float_overlap_reported = true;
        }
        self.placed_float_rects.push((x, y, width, height));
    }

    /// Floating areas the current paragraph must avoid (square wrapping).
    ///
    /// The whole paragraph yields as one unit: if the object is on the left half
    /// of the page the paragraph shifts right, if it is on the right half the
    /// right margin shrinks.
    pub(crate) fn floating_insets(&self, top: f64, bottom: f64) -> (f64, f64) {
        let (container_left, container_width, exclusions) = if self.in_frame {
            (self.frame.left, self.frame.width, &self.frame_exclusions)
        } else {
            let page = self.page_frame();
            (page.left, page.width, &self.float_exclusions)
        };
        let mut left = 0.0_f64;
        let mut right = 0.0_f64;
        for exclusion in exclusions {
            if exclusion.bottom <= top || exclusion.top >= bottom {
                continue;
            }
            let middle = container_left + container_width / 2.0;
            if exclusion.left + exclusion.right < 2.0 * middle {
                left = left.max(exclusion.right - container_left);
            } else {
                right = right.max(container_left + container_width - exclusion.left);
            }
        }
        (left, right)
    }
}
