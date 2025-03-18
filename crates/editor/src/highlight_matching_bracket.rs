use crate::{Editor, RangeToAnchorExt};
use gpui::{Context, HighlightStyle, Window};
use language::CursorShape;
use theme::ActiveTheme;

enum MatchingBracketHighlight {}

impl Editor {
    pub fn refresh_matching_bracket_highlights(
        &mut self,
        window: &Window,
        cx: &mut Context<Editor>,
    ) {
        self.clear_highlights::<MatchingBracketHighlight>(cx);

        let snapshot = self.snapshot(window, cx);
        let buffer_snapshot = snapshot.buffer_snapshot();
        let newest_selection = self.selections.newest::<usize>(&snapshot);
        // Don't highlight brackets if the selection isn't empty
        if !newest_selection.is_empty() {
            return;
        }

        let head = newest_selection.head();
        if head > buffer_snapshot.len() {
            log::error!("bug: cursor offset is out of range while refreshing bracket highlights");
            return;
        }

        let mut tail = head;
        if (self.cursor_shape == CursorShape::Block || self.cursor_shape == CursorShape::Hollow)
            && head < buffer_snapshot.len()
        {
            if let Some(tail_ch) = buffer_snapshot.chars_at(tail).next() {
                tail += tail_ch.len_utf8();
            }
        }

        if let Some((opening_range, closing_range)) =
            buffer_snapshot.innermost_enclosing_bracket_ranges(head..tail, None)
        {
            self.highlight_text::<MatchingBracketHighlight>(
                vec![
                    opening_range.to_anchors(&buffer_snapshot),
                    closing_range.to_anchors(&buffer_snapshot),
                ],
                HighlightStyle {
                    background_color: Some(
                        cx.theme()
                            .colors()
                            .editor_document_highlight_bracket_background,
                    ),
                    ..Default::default()
                },
                cx,
            )
        }
    }
}
