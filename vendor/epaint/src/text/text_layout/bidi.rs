//! `PdfCraft` patch: UAX #9 layout with source character geometry. Never reorder the editable buffer.
use super::*;
use crate::text::BidiRow;
use unicode_bidi::{BidiClass, BidiInfo, Level, ParagraphInfo};
use unicode_segmentation::UnicodeSegmentation as _;

pub(super) fn required(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            unicode_bidi::bidi_class(c),
            BidiClass::R
                | BidiClass::AL
                | BidiClass::AN
                | BidiClass::LRE
                | BidiClass::RLE
                | BidiClass::LRO
                | BidiClass::RLO
                | BidiClass::PDF
                | BidiClass::RLI
                | BidiClass::LRI
                | BidiClass::FSI
                | BidiClass::PDI
        )
        // Persian digits have bidi class EN, but a shaper guesses the Arabic script's
        // RTL direction without an explicit UAX #9 run. Digit-only fields need this path.
        || matches!(u32::from(c),0x0590..=0x08ff | 0xfb1d..=0xfdff | 0xfe70..=0xfefc)
    })
}

/// Shape one logical line using the paragraph's resolved levels. Font/style boundaries
/// stay attached to their source ranges when directional runs move on screen.
fn row(
    fonts: &mut FontsImpl,
    pixels_per_point: f32,
    job: &LayoutJob,
    paragraph_start: usize,
    source_offsets: &[usize],
    info: &BidiInfo<'_>,
    line: Range<usize>,
) -> Row {
    let text = info.text.get(line.clone()).unwrap_or_default();
    let absolute =
        paragraph_start.saturating_add(line.start)..paragraph_start.saturating_add(line.end);
    let source_start = source_offsets.partition_point(|at| *at < absolute.start);
    let mut paragraph = Paragraph::from_section_index(0);
    let mut buffer = fonts.take_shape_buffer();
    // Normal fields split on LF. Other paragraph separators are processed independently,
    // retaining their source characters rather than passing an invalid range to unicode-bidi.
    for para in &info.paragraphs {
        let part = para.range.start.max(line.start)..para.range.end.min(line.end);
        if part.is_empty() {
            continue;
        }
        // visual_runs clones its whole BidiInfo. Retain the paragraph's already-resolved
        // classes/levels, but give it only this line so narrow, very long comments remain
        // linear in their text size rather than cloning the paragraph once per line.
        let Some(line_text) = info.text.get(part.clone()) else {
            continue;
        };
        let line_info = BidiInfo {
            text: line_text,
            original_classes: info.original_classes.get(part.clone()).unwrap_or_default().to_vec(),
            levels: info.levels.get(part.clone()).unwrap_or_default().to_vec(),
            paragraphs: vec![ParagraphInfo { range: 0..line_text.len(), level: para.level }],
        };
        let Some(line_para) = line_info.paragraphs.first() else {
            continue;
        };
        let (levels, runs) = line_info.visual_runs(line_para, 0..line_text.len());
        for run in runs {
            let rtl = levels.get(run.start).is_some_and(Level::is_rtl);
            let start = paragraph_start.saturating_add(part.start).saturating_add(run.start);
            let end = paragraph_start.saturating_add(part.start).saturating_add(run.end);
            let first_section = job.sections.partition_point(|s| s.byte_range.end.0 <= start);
            let mut sections: Vec<_> = job
                .sections
                .iter()
                .enumerate()
                .skip(first_section)
                .take_while(|(_, s)| s.byte_range.start.0 < end)
                .filter_map(|(index, section)| {
                    let a = start.max(section.byte_range.start.0);
                    let b = end.min(section.byte_range.end.0);
                    (a < b).then(|| {
                        (
                            index,
                            LayoutSection {
                                leading_space: if a == section.byte_range.start.0 {
                                    section.leading_space
                                } else {
                                    0.0
                                },
                                byte_range: ByteIndex(a)..ByteIndex(b),
                                format: section.format.clone(),
                            },
                        )
                    })
                })
                .collect();
            if rtl {
                sections.reverse();
            }
            for (index, section) in sections {
                let mut font = fonts.font(&section.format.font_id.family);
                let mut paragraphs = vec![paragraph];
                buffer = layout_section(
                    &mut font,
                    buffer,
                    pixels_per_point,
                    job,
                    index as u32,
                    &section,
                    &mut paragraphs,
                    Some(if rtl {
                        harfrust::Direction::RightToLeft
                    } else {
                        harfrust::Direction::LeftToRight
                    }),
                    source_offsets.partition_point(|at| *at < section.byte_range.start.0),
                );
                paragraph =
                    paragraphs.pop().unwrap_or_else(|| Paragraph::from_section_index(index as u32));
            }
        }
    }
    fonts.return_shape_buffer(buffer);
    let mut char_edges = vec![[0.0, 0.0]; text.chars().count()];
    let chars: Vec<_> = text.chars().collect();
    let mut clusters = std::collections::BTreeMap::<(usize, usize), (f32, f32, bool)>::new();
    for glyph in &paragraph.glyphs {
        let start = glyph.source_char_start.saturating_sub(source_start).min(chars.len());
        let end = glyph.source_char_end.saturating_sub(source_start).min(chars.len());
        if start >= end {
            continue;
        }
        let entry = clusters.entry((start, end)).or_insert((f32::INFINITY, 0.0, glyph.source_rtl));
        if glyph.advance_width > 0.0 {
            entry.0 = if entry.1 == 0.0 { glyph.pos.x } else { entry.0.min(glyph.pos.x) };
            entry.1 += glyph.advance_width;
        } else if !entry.0.is_finite() {
            entry.0 = glyph.pos.x;
        }
    }
    for ((start, end), (left, width, rtl)) in clusters {
        let letters = chars
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .filter(|c| !is_combining_mark(**c))
            .count()
            .max(1);
        let mut offset = 0;
        for index in start..end {
            let mark = chars.get(index).is_some_and(|c| is_combining_mark(*c));
            let a = offset as f32 / letters as f32;
            if !mark {
                offset += 1;
            }
            let b = offset as f32 / letters as f32;
            let edges = if rtl {
                [left + width * (1.0 - a), left + width * (1.0 - b)]
            } else {
                [left + width * a, left + width * b]
            };
            if let Some(out) = char_edges.get_mut(index) {
                *out = edges;
            }
        }
    }
    let glyph_chars = paragraph
        .glyphs
        .iter()
        .map(|g| {
            g.source_char_start.saturating_sub(source_start).min(chars.len())
                ..g.source_char_end.saturating_sub(source_start).min(chars.len())
        })
        .collect();
    let rtl = info.paragraphs.first().is_some_and(|p| p.level.is_rtl());
    Row {
        bidi: Some(BidiRow { text: text.to_owned(), char_edges, glyph_chars, rtl }),
        section_index_at_start: paragraph.section_index_at_start,
        glyphs: paragraph.glyphs,
        size: vec2(paragraph.cursor_x_px / pixels_per_point, paragraph.empty_paragraph_height),
        visuals: RowVisuals::default(),
    }
}

fn soft_space(c: char) -> bool {
    c.is_whitespace()
        && !matches!(
            c,
            '\u{00a0}'
                | '\u{2007}'
                | '\u{202f}'
                | '\r'
                | '\n'
                | '\u{0085}'
                | '\u{2028}'
                | '\u{2029}'
        )
}

fn visible_content(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(c, '\u{00ad}' | '\u{034f}' | '\u{061c}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}' | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}' | '\u{ffa0}' | '\u{fff0}'..='\u{fff8}' | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e0fff}')
}

/// A soft-break separator remains in logical text for copying and caret indices,
/// but must not consume a new line solely because the preceding word exactly fits.
fn hang_separator(row: &mut Row) {
    let Some(bidi) = row.bidi.as_mut() else {
        return;
    };
    let Some((at, c)) = bidi.text.char_indices().rfind(|(_, c)| visible_content(*c)) else {
        return;
    };
    let start = bidi.text[..at.saturating_add(c.len_utf8())].chars().count();
    if !bidi
        .text
        .chars()
        .skip(start)
        .all(|c| soft_space(c) || (!visible_content(c) && !c.is_whitespace()))
    {
        return;
    }
    let width: f32 =
        bidi.char_edges.iter().skip(start).map(|edges| (edges[1] - edges[0]).abs()).sum();
    if width <= 0.0 {
        return;
    }
    row.size.x = (row.size.x - width).max(0.0);
    let shift = if bidi.rtl { -width } else { 0.0 };
    let edge = if bidi.rtl { 0.0 } else { row.size.x };
    for (index, edges) in bidi.char_edges.iter_mut().enumerate() {
        if index >= start {
            *edges = [edge, edge];
        } else {
            // Pixel-rounded glyph origins can differ from their advances by half
            // a pixel. Keep the hanging separator at the actual reading edge so
            // an arrow cannot step backwards into a negative boundary.
            edges[0] = (edges[0] + shift).clamp(0.0, row.size.x);
            edges[1] = (edges[1] + shift).clamp(0.0, row.size.x);
        }
    }
    for (glyph, source) in row.glyphs.iter_mut().zip(&bidi.glyph_chars) {
        if source.start >= start {
            glyph.pos.x = edge;
            glyph.advance_width = 0.0;
        } else {
            glyph.pos.x += shift;
        }
    }
}

fn wrapped_row(
    fonts: &mut FontsImpl,
    pixels_per_point: f32,
    job: &LayoutJob,
    paragraph_start: usize,
    source_offsets: &[usize],
    info: &BidiInfo<'_>,
    line: Range<usize>,
) -> Row {
    let continued = line.end < info.text.len();
    let mut row = row(fonts, pixels_per_point, job, paragraph_start, source_offsets, info, line);
    if continued || row.size.x > job.effective_wrap_width().max(0.0) {
        hang_separator(&mut row);
    }
    row
}

pub(super) fn layout(fonts: &mut FontsImpl, pixels_per_point: f32, job: Arc<LayoutJob>) -> Galley {
    let source_offsets: Vec<_> =
        job.text.char_indices().map(|(at, _)| at).chain(iter::once(job.text.len())).collect();
    let mut rows = Vec::new();
    let mut offset = 0;
    let mut elided = false;
    let mut intrinsic = Vec2::ZERO;
    let paragraphs: Vec<_> =
        if job.break_on_newline { job.text.split('\n').collect() } else { vec![job.text.as_str()] };
    for (paragraph_index, text) in paragraphs.iter().enumerate() {
        let info = BidiInfo::new(text, None);
        let measured =
            row(fonts, pixels_per_point, &job, offset, &source_offsets, &info, 0..text.len());
        intrinsic.x = intrinsic.x.max(measured.size.x);
        intrinsic.y +=
            measured.glyphs.iter().map(|g| g.line_height).fold(measured.size.y, f32::max);
        let mut start = 0;
        let graphemes: Vec<_> = text.grapheme_indices(true).collect();
        let mut char_index: usize = 0;
        let mut width = 0.0;
        let mut last_break = None;
        let mut has_content = false;
        let mut ranges = Vec::new();
        let max_width = job.effective_wrap_width().max(0.0);
        for (at, grapheme) in &graphemes {
            let count = grapheme.chars().count();
            let end_char = char_index.saturating_add(count);
            let advance: f32 = measured
                .bidi
                .as_ref()
                .and_then(|b| b.char_edges.get(char_index..end_char))
                .unwrap_or_default()
                .iter()
                .map(|e| (e[1] - e[0]).abs())
                .sum();
            let separator = (grapheme.chars().all(soft_space) && has_content)
                || (advance == 0.0 && !grapheme.chars().any(visible_content));
            while !separator && width + advance > max_width && *at > start {
                let end = last_break.filter(|end| *end > start).unwrap_or(*at);
                ranges.push(start..end);
                start = end;
                // Measure the carried word from its source character geometry.
                let before = source_offsets
                    .partition_point(|at| *at < offset.saturating_add(start))
                    .saturating_sub(source_offsets.partition_point(|at| *at < offset));
                width = measured
                    .bidi
                    .as_ref()
                    .and_then(|b| b.char_edges.get(before..char_index))
                    .unwrap_or_default()
                    .iter()
                    .map(|e| (e[1] - e[0]).abs())
                    .sum();
                last_break = None;
                has_content = text.get(start..*at).is_some_and(|s| s.chars().any(visible_content));
            }
            width += advance;
            char_index = end_char;
            has_content |= grapheme.chars().any(visible_content);
            if grapheme
                .chars()
                .all(|c| c.is_whitespace() && !matches!(c, '\u{00a0}' | '\u{2007}' | '\u{202f}'))
                || grapheme.ends_with(['-', '\u{2010}'])
                || grapheme.chars().any(is_cjk)
                || job.wrap.break_anywhere
            {
                last_break = Some(at.saturating_add(grapheme.len()));
            }
        }
        ranges.push(start..text.len());
        let mut pending: std::collections::VecDeque<_> = ranges.into();
        while let Some(mut range) = pending.pop_front() {
            if rows.len() >= job.wrap.max_rows {
                elided = true;
                break;
            }
            let mut shaped = wrapped_row(
                fonts,
                pixels_per_point,
                &job,
                offset,
                &source_offsets,
                &info,
                range.clone(),
            );
            // A line-end contextual form can be wider than the form measured inside the
            // paragraph. Find a fitting grapheme prefix and carry every remaining byte on.
            if shaped.size.x > max_width {
                let boundaries: Vec<_> = text
                    .get(range.clone())
                    .unwrap_or_default()
                    .grapheme_indices(true)
                    .map(|(at, _)| range.start.saturating_add(at))
                    .skip(1)
                    .chain(iter::once(range.end))
                    .collect();
                if boundaries.len() > 1 {
                    let original_end = range.end;
                    let mut low = 1;
                    let mut high = boundaries.len().saturating_sub(1);
                    while low < high {
                        let mid = low.saturating_add(high).saturating_add(1) / 2;
                        let end =
                            boundaries.get(mid.saturating_sub(1)).copied().unwrap_or(range.end);
                        let candidate = wrapped_row(
                            fonts,
                            pixels_per_point,
                            &job,
                            offset,
                            &source_offsets,
                            &info,
                            range.start..end,
                        );
                        if candidate.size.x <= max_width {
                            low = mid;
                        } else {
                            high = mid.saturating_sub(1);
                        }
                    }
                    range.end = boundaries.get(low.saturating_sub(1)).copied().unwrap_or(range.end);
                    shaped = wrapped_row(
                        fonts,
                        pixels_per_point,
                        &job,
                        offset,
                        &source_offsets,
                        &info,
                        range.clone(),
                    );
                    if range.end < original_end {
                        pending.push_front(range.end..original_end);
                    }
                }
            }
            let rtl = shaped.bidi.as_ref().is_some_and(|b| b.rtl);
            let more = !pending.is_empty() || paragraph_index.saturating_add(1) < paragraphs.len();
            if rows.len().saturating_add(1) == job.wrap.max_rows && more {
                elided = true;
                if let Some(overflow) = job.wrap.overflow_character {
                    let at = offset.saturating_add(range.start);
                    if let Some((section_index, section)) = job
                        .sections
                        .iter()
                        .enumerate()
                        .find(|(_, s)| s.byte_range.start.0 <= at && at <= s.byte_range.end.0)
                    {
                        let ellipsis_job =
                            LayoutJob::single_section(overflow.to_string(), section.format.clone());
                        let ellipsis_info = BidiInfo::new(&ellipsis_job.text, None);
                        let ellipsis_offsets = vec![0, ellipsis_job.text.len()];
                        let mut ellipsis = row(
                            fonts,
                            pixels_per_point,
                            &ellipsis_job,
                            0,
                            &ellipsis_offsets,
                            &ellipsis_info,
                            0..ellipsis_job.text.len(),
                        );
                        while shaped.size.x + ellipsis.size.x > max_width && !range.is_empty() {
                            let Some((at, _)) = text
                                .get(range.clone())
                                .and_then(|s| s.grapheme_indices(true).next_back())
                            else {
                                break;
                            };
                            range.end = range.start.saturating_add(at);
                            shaped = row(
                                fonts,
                                pixels_per_point,
                                &job,
                                offset,
                                &source_offsets,
                                &info,
                                range.clone(),
                            );
                        }
                        let shift = if rtl { ellipsis.size.x } else { 0.0 };
                        for glyph in &mut shaped.glyphs {
                            glyph.pos.x += shift;
                        }
                        if let Some(bidi) = shaped.bidi.as_mut() {
                            for edges in &mut bidi.char_edges {
                                edges[0] += shift;
                                edges[1] += shift;
                            }
                            let column = bidi.char_edges.len();
                            let left = if rtl { 0.0 } else { shaped.size.x };
                            bidi.text.push(overflow);
                            bidi.char_edges.push(if rtl {
                                [ellipsis.size.x, 0.0]
                            } else {
                                [left, left + ellipsis.size.x]
                            });
                            for glyph in &mut ellipsis.glyphs {
                                glyph.pos.x += left;
                                glyph.section_index = section_index as u32;
                            }
                            let ranges =
                                ellipsis.glyphs.iter().map(|_| column..column.saturating_add(1));
                            if rtl {
                                bidi.glyph_chars.splice(0..0, ranges);
                            } else {
                                bidi.glyph_chars.extend(ranges);
                            }
                        }
                        shaped.size.x += ellipsis.size.x;
                        if rtl {
                            shaped.glyphs.splice(0..0, ellipsis.glyphs);
                        } else {
                            shaped.glyphs.extend(ellipsis.glyphs);
                        }
                    }
                }
            }
            let x = if job.halign == Align::LEFT && rtl && max_width.is_finite() {
                (max_width - shaped.size.x).max(0.0)
            } else {
                -job.halign.to_factor() * shaped.size.x
            };
            let last = range.end == text.len();
            let newline = last && paragraph_index.saturating_add(1) < paragraphs.len();
            // Empty paragraphs still have the current font's line height.
            if shaped.glyphs.is_empty() {
                let at = offset.saturating_add(range.start);
                if let Some(section) = job
                    .sections
                    .iter()
                    .find(|s| s.byte_range.start.0 <= at && at <= s.byte_range.end.0)
                {
                    let font = fonts.font(&section.format.font_id.family);
                    shaped.size.y = font
                        .styled_metrics(
                            pixels_per_point,
                            section.format.font_id.size,
                            &section.format.coords,
                        )
                        .row_height;
                }
            }
            rows.push(PlacedRow {
                pos: pos2(x, 0.0),
                row: Arc::new(shaped),
                ends_with_newline: newline,
            });
        }
        if elided {
            break;
        }
        offset = offset.saturating_add(text.len()).saturating_add(1);
    }
    galley_from_rows(PointScale::new(pixels_per_point), job, rows, elided, intrinsic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{FontData, FontDefinitions, TextOptions, cursor::CCursor};
    use crate::{FontFamily, FontId};

    fn fonts() -> Option<FontsImpl> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
        let arabic = std::path::PathBuf::from(std::env::var_os("CRAFT_FONTS_DIR")?)
            .join("fonts/noto-sans-arabic/NotoSansArabic.ttf");
        let mut defs = FontDefinitions::empty();
        let latin = std::env::var_os("PDFCRAFT_TEST_LATIN_FONT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("assets/fonts/Inter-Regular.ttf"));
        for (name, path) in [("latin", latin), ("arabic", arabic)] {
            defs.font_data
                .insert(name.into(), Arc::new(FontData::from_owned(std::fs::read(path).ok()?)));
        }
        defs.families.insert(FontFamily::Proportional, vec!["latin".into(), "arabic".into()]);
        Some(FontsImpl::new(TextOptions::default(), defs))
    }

    fn galley(text: &str, width: f32) -> Option<Galley> {
        let mut fonts = fonts()?;
        let job = LayoutJob::simple(text.into(), FontId::proportional(16.0), Color32::WHITE, width);
        Some(super::super::layout(&mut fonts, 1.0, Arc::new(job)))
    }

    #[test]
    fn bidi_keeps_source_text_and_places_words_and_numbers_in_visual_order() {
        let Some(galley) = galley("واحد 123 اثنين", f32::INFINITY) else {
            return;
        };
        assert_eq!(galley.rows[0].text(), "واحد 123 اثنين");
        assert_eq!(galley.end().index.0, "واحد 123 اثنين".chars().count());
        let painted: String =
            galley.rows[0].glyphs.iter().filter(|g| g.advance_width > 0.0).map(|g| g.chr).collect();
        assert_eq!(
            painted,
            format!(
                "{} 123 {}",
                "اثنين".chars().rev().collect::<String>(),
                "واحد".chars().rev().collect::<String>()
            )
        );
    }

    #[test]
    fn bidi_caret_hit_testing_and_arrows_use_source_indices() {
        let Some(galley) = galley("ببب", f32::INFINITY) else {
            return;
        };
        let start = galley.pos_from_cursor(CCursor::new(0)).min;
        let end = galley.pos_from_cursor(galley.end()).min;
        assert!(start.x > end.x);
        let moved = galley.cursor_left_one_character(&CCursor::new(0));
        assert_eq!(moved.index.0, 1);
        assert!(galley.pos_from_cursor(moved).min.x < start.x);
        for index in 0..=3 {
            let point = galley.pos_from_cursor(CCursor::new(index)).center();
            assert_eq!(galley.cursor_from_pos(point.to_vec2()).index.0, index);
        }
    }

    #[test]
    fn bidi_arabic_and_persian_digit_only_fields_keep_numeric_order() {
        let Some(mut fonts) = fonts() else { return };
        for text in ["١٢٣", "۱۲۳", "(١٢٣)", "(۱۲۳)"] {
            let job = LayoutJob::simple(
                text.into(),
                FontId::proportional(16.0),
                Color32::WHITE,
                f32::INFINITY,
            );
            let galley = super::super::layout(&mut fonts, 1.0, Arc::new(job));
            assert_eq!(galley.rows[0].text(), text);
            let painted: String = galley.rows[0]
                .glyphs
                .iter()
                .filter(|g| g.advance_width > 0.0)
                .map(|g| g.chr)
                .collect();
            assert_eq!(painted, text);
        }
    }

    #[test]
    fn bidi_wraps_logical_graphemes_and_preserves_spaces_and_paragraphs() {
        let text = "سَلَام واحد اثنين ثلاثة أربعة\n\nHello عالم 123";
        let Some(galley) = galley(text, 78.0) else {
            return;
        };
        assert!(galley.rows.len() > 3);
        let logical: String = galley
            .rows
            .iter()
            .map(|r| format!("{}{}", r.text(), if r.ends_with_newline { "\n" } else { "" }))
            .collect();
        assert_eq!(logical, text);
        assert_eq!(galley.end().index.0, text.chars().count());
        for row in &galley.rows {
            assert!(!row.text().chars().next().is_some_and(is_combining_mark));
            for edges in &row.bidi.as_ref().unwrap().char_edges {
                assert!(edges.iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn bidi_soft_wrap_hangs_its_separator_without_inserting_a_blank_row() {
        let Some(mut fonts) = fonts() else { return };
        for pixels_per_point in [1.0, 1.25, 2.0] {
            for word in ["سلام", "سَلَامٌ"] {
                let font = FontId::proportional(40.0);
                let natural =
                    LayoutJob::simple(word.into(), font.clone(), Color32::WHITE, f32::INFINITY);
                let width =
                    super::super::layout(&mut fonts, pixels_per_point, Arc::new(natural)).size().x;
                let text = format!("{word} {word}");
                let job = LayoutJob::simple(text.clone(), font, Color32::WHITE, width);
                let galley = super::super::layout(&mut fonts, pixels_per_point, Arc::new(job));
                let rows: Vec<_> = galley.rows.iter().map(|r| r.text()).collect();
                assert_eq!(
                    rows,
                    [format!("{word} "), word.into()],
                    "width {width}, dpi {pixels_per_point}"
                );
                assert_eq!(rows.concat(), text);
                for row in &galley.rows {
                    assert!(row.size.x <= width + 0.01);
                }
                let edges = &galley.rows[0].bidi.as_ref().unwrap().char_edges;
                assert_eq!(
                    edges.last(),
                    Some(&[0.0, 0.0]),
                    "the soft separator keeps a finite source caret boundary"
                );
                assert_eq!(galley.end().index.0, text.chars().count());
                for suffix in [" ", "   "] {
                    let text = format!("{word}{suffix}");
                    let job = LayoutJob::simple(
                        text.clone(),
                        FontId::proportional(40.0),
                        Color32::WHITE,
                        width,
                    );
                    let galley = super::super::layout(&mut fonts, pixels_per_point, Arc::new(job));
                    assert_eq!(galley.rows.len(), 1, "final spaces do not introduce a blank line");
                    assert_eq!(galley.rows[0].text(), text);
                    assert!(galley.rows[0].size.x <= width + 0.01);
                    assert_eq!(galley.end().index.0, text.chars().count());
                    let job = LayoutJob::simple(
                        text,
                        FontId::proportional(40.0),
                        Color32::WHITE,
                        f32::INFINITY,
                    );
                    let wide = super::super::layout(&mut fonts, pixels_per_point, Arc::new(job));
                    assert!(
                        wide.rows[0].size.x > width,
                        "final spaces keep their advances when there is room"
                    );
                }
            }
        }
    }

    #[test]
    fn bidi_hanging_spaces_preserve_indentation_controls_and_logical_indices() {
        let Some(mut fonts) = fonts() else { return };
        let font = FontId::proportional(40.0);
        let natural = LayoutJob::simple("سلام".into(), font.clone(), Color32::WHITE, f32::INFINITY);
        let width = super::super::layout(&mut fonts, 1.0, Arc::new(natural)).size().x;
        for text in ["hello سلام hello", "سلام  سلام", "سلام \u{200f}", "\u{2067}سلام سلام\u{2069}"]
        {
            let job = LayoutJob::simple(text.into(), font.clone(), Color32::WHITE, width);
            let galley = super::super::layout(&mut fonts, 1.0, Arc::new(job));
            assert_eq!(galley.rows.iter().map(|r| r.text()).collect::<String>(), text);
            assert!(
                galley.rows.iter().all(|r| r.text().chars().any(visible_content)),
                "no soft whitespace row for {text:?}"
            );
            for index in 0..=text.chars().count() {
                assert!(galley.pos_from_cursor(CCursor::new(index)).is_finite());
                assert!(
                    galley.cursor_left_one_character(&CCursor::new(index)).index.0
                        <= text.chars().count()
                );
                assert!(
                    galley.cursor_right_one_character(&CCursor::new(index)).index.0
                        <= text.chars().count()
                );
            }
        }
        let text = "\u{200f}  سلام سلام";
        let job = LayoutJob::simple(text.into(), font.clone(), Color32::WHITE, width + 22.5);
        let indented = super::super::layout(&mut fonts, 1.0, Arc::new(job));
        assert_eq!(indented.rows.iter().map(|r| r.text()).collect::<String>(), text);
        assert!(indented.rows[0].text().starts_with("\u{200f}  "));
        assert!(indented.rows[0].size.x > width, "leading indentation keeps its advance");
        let text = "\u{200f}   \n   \nسلام";
        let job = LayoutJob::simple(text.into(), font.clone(), Color32::WHITE, 1000.0);
        let explicit = super::super::layout(&mut fonts, 1.0, Arc::new(job));
        assert_eq!(explicit.rows.len(), 3);
        assert_eq!(explicit.rows[0].text(), "\u{200f}   ");
        assert_eq!(explicit.rows[1].text(), "   ");
        assert!(explicit.rows[..2].iter().all(|r| r.size.x > 0.0));
        assert!(explicit.rows[..2].iter().all(|r| r.ends_with_newline));
        for separator in ['\u{00a0}', '\u{2007}', '\u{202f}'] {
            let text = format!("سلام{separator}سلام");
            let job = LayoutJob::simple(text.clone(), font.clone(), Color32::WHITE, width);
            let galley = super::super::layout(&mut fonts, 1.0, Arc::new(job));
            assert_eq!(galley.rows.iter().map(|r| r.text()).collect::<String>(), text);
            assert!(
                galley
                    .rows
                    .iter()
                    .flat_map(|r| &r.glyphs)
                    .filter(|g| g.chr == separator)
                    .any(|g| g.advance_width > 0.0)
            );
        }
    }

    #[test]
    fn bidi_mixed_selection_can_have_disjoint_visual_spans() {
        let Some(galley) = galley("abc واحد def", f32::INFINITY) else {
            return;
        };
        let row = &galley.rows[0];
        let spans = row.selection_spans(0..5);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(spans.iter().all(|s| s[0] <= s[1]));
    }

    #[test]
    fn bidi_elision_fits_and_places_ellipsis_at_the_reading_end() {
        let Some(mut fonts) = fonts() else { return };
        let mut job = LayoutJob::simple(
            "واحد اثنين ثلاثة أربعة".into(),
            FontId::proportional(16.0),
            Color32::WHITE,
            75.0,
        );
        job.wrap.max_rows = 1;
        let galley = super::super::layout(&mut fonts, 1.0, Arc::new(job));
        assert!(galley.elided);
        assert_eq!(galley.rows.len(), 1);
        let row = &galley.rows[0];
        assert!(row.text().ends_with('…'));
        assert!(row.size.x <= 75.5, "{}", row.size.x);
        let ellipsis = row.glyphs.iter().find(|g| g.chr == '…').unwrap();
        assert!(ellipsis.pos.x < 1.0);
        assert!(row.text().starts_with("واحد"));
    }

    #[test]
    fn bidi_hit_testing_uses_the_character_under_the_pointer_at_a_run_boundary() {
        let Some(galley) = galley("abc واحد def", f32::INFINITY) else {
            return;
        };
        let row = &galley.rows[0];
        let last_arabic = row.bidi.as_ref().unwrap().char_edges[7];
        let point = last_arabic[1] + (last_arabic[0] - last_arabic[1]) * 0.1;
        assert_eq!(row.cursor_at(point), (crate::text::CharIndex(8), false));
    }

    #[test]
    fn bidi_left_arrow_continues_on_the_next_rtl_row() {
        let Some(galley) = galley("واحد اثنين ثلاثة أربعة خمسة", 65.0)
        else {
            return;
        };
        assert!(galley.rows.len() > 1);
        let first_len = galley.rows[0].char_count_excluding_newline().0;
        let next = galley.cursor_left_one_character(&CCursor::new(first_len));
        assert_eq!(next.index.0, first_len);
        assert_eq!(galley.layout_from_cursor(next).row, 1);
        assert!(next.prefer_next_row);
    }

    #[test]
    fn bidi_controls_marks_and_tiny_widths_preserve_all_source_characters() {
        let Some(mut fonts) = fonts() else { return };
        for text in [
            "\u{202E}اب 12\u{202C}c",
            "\u{2067}نص ١٢٣\u{2069} End",
            "بَتُثِ\r\n\nabc",
            "نص\tثان\u{00a0}ثالث",
        ] {
            for width in [0.0, 1.0, 40.0, f32::INFINITY] {
                let job = LayoutJob::simple(
                    text.into(),
                    FontId::proportional(16.0),
                    Color32::WHITE,
                    width,
                );
                let galley = super::super::layout(&mut fonts, 1.0, Arc::new(job));
                let source: String = galley
                    .rows
                    .iter()
                    .map(|r| format!("{}{}", r.text(), if r.ends_with_newline { "\n" } else { "" }))
                    .collect();
                assert_eq!(source, text);
                assert_eq!(galley.end().index.0, text.chars().count());
                assert!(galley.rect.is_finite());
            }
        }
    }

    #[test]
    fn bidi_deterministic_mixed_unicode_inputs_keep_cursor_and_selection_geometry_valid() {
        let Some(mut fonts) = fonts() else { return };
        let alphabet = [
            'ب', 'ا', 'ل', 'م', 'َ', 'ِ', 'ُ', 'a', 'Z', '0', '9', '١', '٢', '(', ')', ' ', '\n',
            '\r', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202e}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{200c}', '\u{200f}',
        ];
        let mut seed = 0x7481_9205_u32;
        for case in 0..64 {
            let mut text = String::from("ب");
            for _ in 0..24 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                if let Some(c) = alphabet.get(seed as usize % alphabet.len()) {
                    text.push(*c);
                }
            }
            for pixels_per_point in [1.0, 1.25, 2.0] {
                let width = [0.0, 10.0, 45.0, 120.0].get(case % 4).copied().unwrap_or(45.0);
                let job = LayoutJob::simple(
                    text.clone(),
                    FontId::proportional(16.0),
                    Color32::WHITE,
                    width,
                );
                let galley = super::super::layout(&mut fonts, pixels_per_point, Arc::new(job));
                let source: String = galley
                    .rows
                    .iter()
                    .map(|r| format!("{}{}", r.text(), if r.ends_with_newline { "\n" } else { "" }))
                    .collect();
                assert_eq!(source, text, "case {case}");
                let count = text.chars().count();
                assert_eq!(galley.end().index.0, count);
                for index in 0..=count {
                    let point = galley.pos_from_cursor(CCursor::new(index));
                    assert!(point.is_finite(), "case {case}, cursor {index}");
                    assert!(galley.cursor_from_pos(point.center().to_vec2()).index.0 <= count);
                    assert!(
                        galley.cursor_left_one_character(&CCursor::new(index)).index.0 <= count
                    );
                    assert!(
                        galley.cursor_right_one_character(&CCursor::new(index)).index.0 <= count
                    );
                }
                for row in &galley.rows {
                    let bidi = row.bidi.as_ref().unwrap();
                    assert!(
                        bidi.glyph_chars
                            .iter()
                            .all(|r| r.start <= r.end && r.end <= bidi.char_edges.len())
                    );
                    assert!(
                        row.selection_spans(0..bidi.char_edges.len())
                            .iter()
                            .flatten()
                            .all(|x| x.is_finite())
                    );
                }
            }
        }
    }

    #[test]
    fn bidi_fractional_dpi_caret_roundtrip_and_contextual_line_edges_fit() {
        let Some(mut fonts) = fonts() else { return };
        for pixels_per_point in [1.0, 1.25, 2.0] {
            let job = LayoutJob::simple(
                "ببب".into(),
                FontId::proportional(16.0),
                Color32::WHITE,
                f32::INFINITY,
            );
            let galley = super::super::layout(&mut fonts, pixels_per_point, Arc::new(job));
            for index in 0..=3 {
                let point = galley.pos_from_cursor(CCursor::new(index)).center();
                assert_eq!(galley.cursor_from_pos(point.to_vec2()).index.0, index);
            }
            for width in [24.0, 38.0, 64.0] {
                let text = "بَ".repeat(100);
                let job = LayoutJob::simple(
                    text.clone(),
                    FontId::proportional(16.0),
                    Color32::WHITE,
                    width,
                );
                let galley = super::super::layout(&mut fonts, pixels_per_point, Arc::new(job));
                assert_eq!(galley.rows.iter().map(|r| r.text()).collect::<String>(), text);
                for row in &galley.rows {
                    assert!(
                        row.size.x <= width + 0.5 || row.text().graphemes(true).count() == 1,
                        "{} > {width}",
                        row.size.x
                    );
                    assert!(!row.text().chars().next().is_some_and(is_combining_mark));
                }
            }
        }
    }

    #[test]
    fn bidi_long_narrow_paragraph_has_bounded_layout_work() {
        let Some(mut fonts) = fonts() else { return };
        let text = "نص طويل 123 ".repeat(1000);
        let job = LayoutJob::simple(text.clone(), FontId::proportional(16.0), Color32::WHITE, 38.0);
        let started = std::time::Instant::now();
        let galley = super::super::layout(&mut fonts, 1.25, Arc::new(job));
        assert_eq!(galley.rows.iter().map(|r| r.text()).collect::<String>(), text);
        assert!(galley.rows.len() > 1000);
        assert!(
            started.elapsed() < core::time::Duration::from_secs(15),
            "long narrow paragraph took {:?}",
            started.elapsed()
        );
    }
}
