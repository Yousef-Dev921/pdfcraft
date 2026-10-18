use emath::TSTransform;
// PdfCraft patch: expose logical Unicode text and direction-aware character bounds.
use epaint::text::CharIndex;

use crate::{Context, Galley, Id};

use super::{CCursorRange, text_cursor_state::is_word_char};

/// AccessKit's `word_starts` uses `u8` indices, so text runs cannot exceed this length.
pub(crate) const MAX_CHARS_PER_TEXT_RUN: usize = 255;

/// Convert a (row, column) layout cursor position to a text run node ID and character index,
/// accounting for rows that are split into multiple text runs.
fn text_run_position(parent_id: Id, row: usize, column: CharIndex) -> accesskit::TextPosition {
    let column = column.0;
    // When column lands exactly on a chunk boundary (e.g., 255), it refers to
    // the end of the previous chunk, not the start of a new one.
    let chunk_index = if column > 0 && column.is_multiple_of(MAX_CHARS_PER_TEXT_RUN) {
        column / MAX_CHARS_PER_TEXT_RUN - 1
    } else {
        column / MAX_CHARS_PER_TEXT_RUN
    };
    let character_index = column - chunk_index * MAX_CHARS_PER_TEXT_RUN;
    accesskit::TextPosition {
        node: parent_id.with(row).with(chunk_index).accesskit_id(),
        character_index,
    }
}

/// Update accesskit with the current text state.
pub fn update_accesskit_for_text_widget(
    ctx: &Context,
    widget_id: Id,
    cursor_range: Option<CCursorRange>,
    role: accesskit::Role,
    global_from_galley: TSTransform,
    galley: &Galley,
) {
    let parent_id = ctx.accesskit_node_builder(widget_id, |builder| {
        let parent_id = widget_id;

        if let Some(cursor_range) = &cursor_range {
            let anchor = galley.layout_from_cursor(cursor_range.secondary);
            let focus = galley.layout_from_cursor(cursor_range.primary);
            builder.set_text_selection(accesskit::TextSelection {
                anchor: text_run_position(parent_id, anchor.row, anchor.column),
                focus: text_run_position(parent_id, focus.row, focus.column),
            });
        }

        builder.set_role(role);

        parent_id
    });

    let Some(parent_id) = parent_id else {
        return;
    };

    let mut prev_row_ended_with_newline = true;

    for (row_index, row) in galley.rows.iter().enumerate() {
        let rtl = row.bidi.as_ref().is_some_and(|b| b.rtl);
        let logical_text = row.text();
        let glyph_count = row.char_count_excluding_newline().0;
        let mut value = String::with_capacity(glyph_count);
        let mut character_lengths = Vec::<u8>::with_capacity(glyph_count);
        let mut character_positions = Vec::<f32>::with_capacity(glyph_count);
        let mut character_widths = Vec::<f32>::with_capacity(glyph_count);
        let mut word_starts = Vec::<usize>::new();
        // For soft-wrapped continuation rows, treat the start as a word
        // boundary so the first word character gets a `word_starts` entry.
        // Paragraph-starting runs (first row or after a newline) get an
        // implicit word start from AccessKit, so they don't need this.
        let mut was_at_word_end = !prev_row_ended_with_newline;

        for (index, chr) in logical_text.chars().enumerate() {
            let is_word_char = is_word_char(chr);
            if is_word_char && was_at_word_end {
                word_starts.push(character_lengths.len());
            }
            was_at_word_end = !is_word_char;
            let old_len = value.len();
            value.push(chr);
            character_lengths.push((value.len() - old_len) as _);
            let edges = row.bidi.as_ref().and_then(|b| b.char_edges.get(index)).copied();
            let (x, width) = if let Some([a, b]) = edges {
                (a.min(b), (b - a).abs())
            } else {
                row.glyphs.get(index).map_or((0.0, 0.0), |g| (g.pos.x, g.advance_width))
            };
            character_positions.push(if rtl { row.size.x - x - width } else { x });
            character_widths.push(width);
        }

        if row.ends_with_newline {
            value.push('\n');
            character_lengths.push(1);
            let x = row.cursor_x(row.char_count_excluding_newline(), false);
            character_positions.push(if rtl { row.size.x - x } else { x });
            character_widths.push(0.0);
        }

        let total_chars = character_lengths.len();

        if total_chars <= MAX_CHARS_PER_TEXT_RUN {
            let run_id = parent_id.with(row_index).with(0usize);
            ctx.register_accesskit_parent(run_id, parent_id);

            ctx.accesskit_node_builder(run_id, |builder| {
                builder.set_role(accesskit::Role::TextRun);
                builder.set_text_direction(if row.bidi.as_ref().is_some_and(|b| b.rtl) {
                    accesskit::TextDirection::RightToLeft
                } else {
                    accesskit::TextDirection::LeftToRight
                });
                // TODO(mwcampbell): Set more node fields for the row
                // once AccessKit adapters expose text formatting info.

                let rect = global_from_galley * row.rect_without_leading_space();
                builder.set_bounds(accesskit::Rect {
                    x0: rect.min.x.into(),
                    y0: rect.min.y.into(),
                    x1: rect.max.x.into(),
                    y1: rect.max.y.into(),
                });
                builder.set_value(value);
                builder.set_character_lengths(character_lengths);

                let pos_offset = if row.bidi.is_some() {
                    0.0
                } else {
                    character_positions.first().copied().unwrap_or(0.0)
                };
                for p in &mut character_positions {
                    *p -= pos_offset;
                }
                builder.set_character_positions(character_positions);
                builder.set_character_widths(character_widths);

                let chunk_word_starts: Vec<u8> = word_starts.iter().map(|&ws| ws as u8).collect();
                builder.set_word_starts(chunk_word_starts);
            });
        } else {
            let num_chunks = total_chars.div_ceil(MAX_CHARS_PER_TEXT_RUN);
            let mut byte_offset = 0usize;

            for chunk_idx in 0..num_chunks {
                let char_start = chunk_idx * MAX_CHARS_PER_TEXT_RUN;
                let char_end = (char_start + MAX_CHARS_PER_TEXT_RUN).min(total_chars);

                let byte_start = byte_offset;
                let chunk_byte_len: usize =
                    character_lengths[char_start..char_end].iter().map(|&l| l as usize).sum();
                let byte_end = byte_start + chunk_byte_len;
                byte_offset = byte_end;

                let run_id = parent_id.with(row_index).with(chunk_idx);
                ctx.register_accesskit_parent(run_id, parent_id);

                ctx.accesskit_node_builder(run_id, |builder| {
                    builder.set_role(accesskit::Role::TextRun);
                    builder.set_text_direction(if row.bidi.as_ref().is_some_and(|b| b.rtl) {
                        accesskit::TextDirection::RightToLeft
                    } else {
                        accesskit::TextDirection::LeftToRight
                    });
                    // TODO(mwcampbell): Set more node fields for the row
                    // once AccessKit adapters expose text formatting info.

                    if chunk_idx > 0 {
                        let prev_id = parent_id.with(row_index).with(chunk_idx - 1);
                        builder.set_previous_on_line(prev_id.accesskit_id());
                    }
                    if chunk_idx + 1 < num_chunks {
                        let next_id = parent_id.with(row_index).with(chunk_idx + 1);
                        builder.set_next_on_line(next_id.accesskit_id());
                    }

                    let row_rect = row.rect_without_leading_space();
                    let along_start = character_positions[char_start..char_end]
                        .iter()
                        .copied()
                        .fold(f32::INFINITY, f32::min);
                    let along_end = character_positions[char_start..char_end]
                        .iter()
                        .zip(&character_widths[char_start..char_end])
                        .map(|(x, w)| x + w)
                        .fold(f32::NEG_INFINITY, f32::max);
                    let (chunk_x0, chunk_x1) = if rtl {
                        (row.pos.x + row.size.x - along_end, row.pos.x + row.size.x - along_start)
                    } else {
                        (row.pos.x + along_start, row.pos.x + along_end)
                    };
                    let chunk_rect = emath::Rect::from_min_max(
                        emath::pos2(chunk_x0, row_rect.min.y),
                        emath::pos2(chunk_x1, row_rect.max.y),
                    );
                    let rect = global_from_galley * chunk_rect;
                    builder.set_bounds(accesskit::Rect {
                        x0: rect.min.x.into(),
                        y0: rect.min.y.into(),
                        x1: rect.max.x.into(),
                        y1: rect.max.y.into(),
                    });
                    builder.set_value(value[byte_start..byte_end].to_owned());
                    builder.set_character_lengths(character_lengths[char_start..char_end].to_vec());

                    let pos_offset = along_start;
                    let chunk_positions: Vec<f32> = character_positions[char_start..char_end]
                        .iter()
                        .map(|&p| p - pos_offset)
                        .collect();
                    builder.set_character_positions(chunk_positions);
                    builder.set_character_widths(character_widths[char_start..char_end].to_vec());

                    let chunk_word_starts: Vec<u8> = word_starts
                        .iter()
                        .filter(|&&ws| ws >= char_start && ws < char_end)
                        .map(|&ws| (ws - char_start) as u8)
                        .collect();
                    builder.set_word_starts(chunk_word_starts);
                });
            }
        }

        prev_row_ended_with_newline = row.ends_with_newline;
    }
}
