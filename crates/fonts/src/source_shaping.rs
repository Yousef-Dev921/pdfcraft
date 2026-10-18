//! Shaping an explicitly selected source/document face, with OpenType features and UAX #9.
use harfrust::{BufferClusterLevel, Direction, ShapeOptions, ShaperData, UnicodeBuffer};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId};
use unicode_bidi::{Level, ParagraphBidiInfo};

#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    pub id: GlyphId,
    pub position: [f64; 2],
    pub text: String,
}
#[derive(Clone, Debug)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    pub advance: f64,
}

impl ShapedRun {
    /// Advance in font-em units, including document spacing only for visible source characters.
    pub fn spaced_advance(&self, char_spacing: f64, word_spacing: f64) -> f64 {
        self.advance
            + self
                .glyphs
                .iter()
                .map(|glyph| {
                    char_spacing * glyph.text.chars().filter(|c| !crate::embedding::default_ignorable(*c)).count() as f64
                        + word_spacing * glyph.text.matches(' ').count() as f64
                })
                .sum::<f64>()
    }
}

/// Complex scripts/marks require shaping instead of independently encoding characters.
pub fn required(text: &str) -> bool {
    text.chars().any(|c| {
        crate::embedding::default_ignorable(c)
            || matches!(u32::from(c), 0x0300..=0x036f | 0x0590..=0x0fff | 0x1780..=0x17ff | 0x200c..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
    })
}

pub fn shape(bytes: &[u8], text: &str) -> Result<ShapedRun, String> {
    Paragraph::new(text)?.shape(bytes, 0..text.len())
}

/// Resolve directional controls once for the complete paragraph, then shape its wrapped rows.
/// A line beginning inside an isolate or override keeps that context without altering its text.
pub struct Paragraph<'a> {
    text: &'a str,
    bidi: ParagraphBidiInfo<'a>,
}

impl<'a> Paragraph<'a> {
    pub fn new(text: &'a str) -> Result<Self, String> {
        if text.len() > 262_144 || text.contains(['\n', '\r']) {
            return Err("shape one bounded text paragraph at a time".into());
        }
        let base = if unicode_bidi::get_base_direction(text) == unicode_bidi::Direction::Rtl { Level::rtl() } else { Level::ltr() };
        Ok(Self { text, bidi: ParagraphBidiInfo::new(text, Some(base)) })
    }

    pub fn text(&self) -> &str {
        self.text
    }

    pub fn rtl(&self) -> bool {
        self.bidi.paragraph_level.is_rtl()
    }

    pub fn shape(&self, bytes: &[u8], range: std::ops::Range<usize>) -> Result<ShapedRun, String> {
        let text = self.text.get(range.clone()).ok_or("shape range must be UTF-8 boundaries inside its paragraph")?;
        let displayed = crate::appearance_text::display_line(text, range.end < self.text.len());
        let line = crate::appearance_text::line_bidi(&self.bidi, range, &displayed).map_err(|_| "invalid bidi range")?;
        shape_resolved(bytes, &displayed, &line)
    }

    /// Hang a final breakable separator only when its advance would otherwise create a blank
    /// wrapped row. Wide final spaces and explicit indentation keep their original advances.
    /// All spacing and the available width are in font-em units, before horizontal scaling.
    pub fn shape_fitted(
        &self,
        bytes: &[u8],
        range: std::ops::Range<usize>,
        max_advance: f64,
        char_spacing: f64,
        word_spacing: f64,
    ) -> Result<ShapedRun, String> {
        if [max_advance, char_spacing, word_spacing].iter().any(|v| !v.is_finite()) || max_advance < 0.0 {
            return Err("shaped source width and spacing must be finite".into());
        }
        let shaped = self.shape(bytes, range.clone())?;
        let logical = self.text.get(range.clone()).ok_or("shape range must be UTF-8 boundaries inside its paragraph")?;
        if range.end == self.text.len()
            && shaped.spaced_advance(char_spacing, word_spacing) > max_advance
            && crate::appearance_text::has_hanging_space(logical)
        {
            let displayed = crate::appearance_text::display_line(logical, true);
            let line = crate::appearance_text::line_bidi(&self.bidi, range, &displayed).map_err(|_| "invalid bidi range")?;
            return shape_resolved(bytes, &displayed, &line);
        }
        Ok(shaped)
    }
}

fn shape_resolved(bytes: &[u8], text: &str, bidi: &ParagraphBidiInfo<'_>) -> Result<ShapedRun, String> {
    if text.len() > 262_144 || text.contains(['\n', '\r']) {
        return Err("shape one bounded text row at a time".into());
    }
    if text.is_empty() {
        return Ok(ShapedRun { glyphs: Vec::new(), advance: 0.0 });
    }
    let face = FontRef::new(bytes).map_err(|e| e.to_string())?;
    if text.chars().any(|c| matches!(u32::from(c), 0x0600..=0x06ff | 0x0750..=0x077f | 0x08a0..=0x0fff | 0x1780..=0x17ff)) && face.gsub().is_err() {
        return Err("the source subset has no contextual substitution tables; select its full font".into());
    }
    let data = ShaperData::new(&face);
    let shaper = data.shaper(&face).build();
    let em = f64::from(shaper.units_per_em().max(1));
    let (levels, runs) = bidi.visual_runs(0..text.len());
    let mut glyphs = Vec::new();
    let mut x = 0.0;
    for run in runs {
        let Some(input) = text.get(run.clone()) else { continue };
        let rtl = levels.get(run.start).is_some_and(Level::is_rtl);
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(input);
        buffer.set_cluster_level(BufferClusterLevel::MonotoneGraphemes);
        buffer.set_direction(if rtl { Direction::RightToLeft } else { Direction::LeftToRight });
        buffer.guess_segment_properties();
        let output = shaper.shape(buffer, ShapeOptions::new());
        let mut starts: Vec<_> = output.glyph_infos().iter().map(|g| g.cluster as usize).collect();
        starts.sort_unstable();
        starts.dedup();
        let mut seen = std::collections::HashSet::new();
        for (g, p) in output.glyph_infos().iter().zip(output.glyph_positions()) {
            let start = g.cluster as usize;
            let end = starts.get(starts.partition_point(|i| *i <= start)).copied().unwrap_or(input.len());
            if g.glyph_id == 0 {
                // Fonts need no cmap entry for a direction/join control. Keep it in ActualText,
                // while omitting an invisible .notdef instead of refusing otherwise valid text.
                if input.get(start..end).is_some_and(|s| !s.is_empty() && s.chars().all(crate::embedding::default_ignorable)) {
                    continue;
                }
                return Err("the full font lacks a required contextual glyph; choose the complete source face".into());
            }
            let logical = if seen.insert(start) { input.get(start..end).unwrap_or_default().to_string() } else { String::new() };
            glyphs.push(ShapedGlyph {
                id: GlyphId::new(g.glyph_id),
                position: [x + f64::from(p.x_offset) / em, f64::from(p.y_offset) / em],
                text: logical,
            });
            x += f64::from(p.x_advance) / em;
        }
    }
    if !x.is_finite() || glyphs.len() > 65_534 {
        return Err("shaped row exceeds its geometry budget".into());
    }
    Ok(ShapedRun { glyphs, advance: x })
}
