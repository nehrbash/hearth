use anyhow::{Context, Result, ensure};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, Wrap, fontdb,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::gpu::Gpu;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    #[default]
    Clip,
    Wrap,
    Ellipsis,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub bounds: [f32; 4],
    pub font_size: f32,
    pub color: [u8; 4],
    pub family: String,
    pub weight: u16,
    pub overflow: Overflow,
    pub clip: Option<[f32; 4]>,
}

impl Label {
    pub fn new(text: impl Into<String>, bounds: [f32; 4], font_size: f32, color: [u8; 4]) -> Self {
        Self {
            text: text.into(),
            bounds,
            font_size,
            color,
            family: "DejaVu Sans".into(),
            weight: 400,
            overflow: Overflow::Clip,
            clip: None,
        }
    }

    pub fn bold(mut self) -> Self {
        self.weight = 700;
        self
    }

    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = family.into();
        self
    }

    pub fn wrap(mut self) -> Self {
        self.overflow = Overflow::Wrap;
        self
    }

    /// Show one line, marking any omitted content with an ellipsis.
    pub fn ellipsis(mut self) -> Self {
        self.overflow = Overflow::Ellipsis;
        self
    }

    pub fn with_clip(mut self, bounds: [f32; 4]) -> Self {
        self.clip = Some(bounds);
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            valid_bounds(self.bounds),
            "Text bounds must be finite and nonnegative in size"
        );
        ensure!(
            self.clip.is_none_or(valid_bounds),
            "Text clipping bounds must be finite and nonnegative in size"
        );
        ensure!(
            self.font_size.is_finite()
                && self.font_size > 0.0
                && (self.font_size * 1.35).is_finite(),
            "Font size must be finite and positive"
        );
        ensure!(
            (1..=1000).contains(&self.weight),
            "Font weight must be between 1 and 1000"
        );
        Ok(())
    }

    fn text_bounds(&self) -> TextBounds {
        let [x, y, width, height] = self.bounds;
        let mut edges = [x, y, x + width, y + height];
        if let Some([clip_x, clip_y, clip_width, clip_height]) = self.clip {
            edges[0] = edges[0].max(clip_x);
            edges[1] = edges[1].max(clip_y);
            edges[2] = edges[2].min(clip_x + clip_width).max(edges[0]);
            edges[3] = edges[3].min(clip_y + clip_height).max(edges[1]);
        }
        let left = edges[0].ceil() as i32;
        let top = edges[1].ceil() as i32;
        TextBounds {
            left,
            top,
            right: (edges[2].floor() as i32).max(left),
            bottom: (edges[3].floor() as i32).max(top),
        }
    }
}

fn valid_bounds([x, y, width, height]: [f32; 4]) -> bool {
    [x, y, width, height, x + width, y + height]
        .iter()
        .all(|value| value.is_finite())
        && width >= 0.0
        && height >= 0.0
}

fn font_system(include_system_fonts: bool) -> FontSystem {
    let mut database = fontdb::Database::new();
    if include_system_fonts {
        database.load_system_fonts();
    }
    for bytes in [
        include_bytes!("../assets/fonts/DejaVuSans.ttf").as_slice(),
        include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf").as_slice(),
        include_bytes!("../assets/fonts/DejaVuSerif.ttf").as_slice(),
    ] {
        database.load_font_data(bytes.to_vec());
    }
    database.set_sans_serif_family("DejaVu Sans");
    database.set_serif_family("DejaVu Serif");
    FontSystem::new_with_locale_and_db("en-US".into(), database)
}

fn shape(font_system: &mut FontSystem, label: &Label) -> Buffer {
    if label.overflow == Overflow::Ellipsis {
        shape_ellipsis(font_system, label)
    } else {
        shape_with_height(font_system, label, Some(label.bounds[3]))
    }
}

fn shape_with_height(font_system: &mut FontSystem, label: &Label, height: Option<f32>) -> Buffer {
    let mut buffer = Buffer::new(
        font_system,
        Metrics::new(label.font_size, label.font_size * 1.35),
    );
    buffer.set_size(font_system, Some(label.bounds[2]), height);
    buffer.set_wrap(
        font_system,
        match label.overflow {
            Overflow::Clip | Overflow::Ellipsis => Wrap::None,
            Overflow::Wrap => Wrap::WordOrGlyph,
        },
    );
    buffer.set_text(
        font_system,
        &label.text,
        &Attrs::new()
            .family(Family::Name(&label.family))
            .weight(Weight(label.weight)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, false);
    buffer
}

fn shape_ellipsis(font_system: &mut FontSystem, label: &Label) -> Buffer {
    let mut candidate = label.clone();
    candidate.overflow = Overflow::Clip;
    if label.bounds[3] < label.font_size * 1.35 {
        candidate.text.clear();
        return shape_with_height(font_system, &candidate, Some(label.bounds[3]));
    }
    let first_line = label.text.split(['\r', '\n']).next().unwrap_or_default();
    candidate.text = first_line.to_owned();
    let original = shape_with_height(font_system, &candidate, Some(label.bounds[3]));
    if first_line.len() == label.text.len() && fits_width(&original, label.bounds[2]) {
        return original;
    }
    candidate.text = "…".into();
    let mut best = shape_with_height(font_system, &candidate, Some(label.bounds[3]));
    if !fits_width(&best, label.bounds[2]) {
        candidate.text.clear();
        return shape_with_height(font_system, &candidate, Some(label.bounds[3]));
    }
    let boundaries: Vec<_> = std::iter::once(0)
        .chain(
            first_line
                .grapheme_indices(true)
                .map(|(index, grapheme)| index + grapheme.len()),
        )
        .collect();
    let mut low = 0;
    let mut high = boundaries.len();
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        candidate.text = format!("{}…", &first_line[..boundaries[middle]]);
        let buffer = shape_with_height(font_system, &candidate, Some(label.bounds[3]));
        if fits_width(&buffer, label.bounds[2]) {
            low = middle;
            best = buffer;
        } else {
            high = middle;
        }
    }
    best
}

fn fits_width(buffer: &Buffer, width: f32) -> bool {
    buffer
        .layout_runs()
        .all(|run| run.line_w.is_finite() && run.line_w <= width)
}

/// Check full fixture-font content against its layout and clipping bounds.
pub fn fully_visible(label: &Label) -> Result<bool> {
    label.validate()?;
    if label.text.len() > 4096
        || (label.overflow == Overflow::Ellipsis && label.text.contains(['\r', '\n']))
        || label.font_size > 512.0
        || label.color[3] == 0
        || label.bounds[2] == 0.0
        || label.bounds[3] == 0.0
    {
        return Ok(false);
    }
    let mut fonts = font_system(false);
    // A bounded buffer exposes only visible runs and would hide overflowing lines.
    let buffer = shape_with_height(&mut fonts, label, None);
    let clip = label.text_bounds();
    let [x, y, _, _] = label.bounds;
    for run in buffer.layout_runs() {
        if ![run.line_top, run.line_height, run.line_w]
            .iter()
            .all(|value| value.is_finite())
            || x < clip.left as f32
            || x + run.line_w > clip.right as f32
            || y + run.line_top < clip.top as f32
            || y + run.line_top + run.line_height > clip.bottom as f32
            || run.glyphs.iter().any(|glyph| glyph.glyph_id == 0)
        {
            return Ok(false);
        }
    }
    let mut swash = SwashCache::new();
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            if x + glyph.x < clip.left as f32 || x + glyph.x + glyph.w > clip.right as f32 {
                return Ok(false);
            }
            let physical = glyph.physical((x, y), 1.0);
            let Some(image) = swash.get_image(&mut fonts, physical.cache_key) else {
                if run
                    .text
                    .get(glyph.start..glyph.end)
                    .is_none_or(|text| !text.chars().all(char::is_whitespace))
                {
                    return Ok(false);
                }
                continue;
            };
            if image.placement.width == 0 || image.placement.height == 0 {
                continue;
            }
            let left = i64::from(physical.x) + i64::from(image.placement.left);
            let top =
                i64::from(physical.y) + run.line_y.round() as i64 - i64::from(image.placement.top);
            if left < i64::from(clip.left)
                || top < i64::from(clip.top)
                || left + i64::from(image.placement.width) > i64::from(clip.right)
                || top + i64::from(image.placement.height) > i64::from(clip.bottom)
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub struct TextSystem {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    renderer: TextRenderer,
    labels: Vec<Label>,
    buffers: Vec<Buffer>,
}

impl TextSystem {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        Self::build(gpu, format, false)
    }

    pub fn with_system_fonts(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        Self::build(gpu, format, true)
    }

    fn build(gpu: &Gpu, format: wgpu::TextureFormat, include_system_fonts: bool) -> Self {
        let cache = Cache::new(&gpu.device);
        let viewport = Viewport::new(&gpu.device, &cache);
        let mut atlas = TextAtlas::new(&gpu.device, &gpu.queue, &cache, format);
        let renderer = TextRenderer::new(
            &mut atlas,
            &gpu.device,
            wgpu::MultisampleState::default(),
            None,
        );
        Self {
            font_system: font_system(include_system_fonts),
            swash: SwashCache::new(),
            atlas,
            viewport,
            renderer,
            labels: Vec::new(),
            buffers: Vec::new(),
        }
    }

    pub fn prepare(&mut self, gpu: &Gpu, size: [u32; 2], labels: &[Label]) -> Result<()> {
        ensure!(size[0] > 0 && size[1] > 0, "Text viewport cannot be empty");
        for label in labels {
            label.validate()?;
        }
        if labels != self.labels {
            self.buffers = labels
                .iter()
                .map(|label| shape(&mut self.font_system, label))
                .collect();
            self.labels = labels.to_vec();
        }
        self.viewport.update(
            &gpu.queue,
            Resolution {
                width: size[0],
                height: size[1],
            },
        );
        self.atlas.trim();
        self.renderer
            .prepare(
                &gpu.device,
                &gpu.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                self.labels
                    .iter()
                    .zip(&self.buffers)
                    .map(|(label, buffer)| TextArea {
                        buffer,
                        left: label.bounds[0],
                        top: label.bounds[1],
                        scale: 1.0,
                        bounds: label.text_bounds(),
                        default_color: Color::rgba(
                            label.color[0],
                            label.color[1],
                            label.color[2],
                            label.color[3],
                        ),
                        custom_glyphs: &[],
                    }),
                &mut self.swash,
            )
            .context("Cannot prepare text atlas")
    }

    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) -> Result<()> {
        self.renderer
            .render(&self.atlas, &self.viewport, pass)
            .context("Cannot render prepared text")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ellipsis_preserves_fitting_text_and_bounds_long_text() {
        let mut fonts = font_system(false);
        let mut label = Label::new("Hearth", [0.0, 0.0, 100.0, 40.0], 20.0, [255; 4]).ellipsis();
        let buffer = shape(&mut fonts, &label);
        assert_eq!(buffer.lines[0].text(), "Hearth");
        label.text = "Hearth workspaces with long names".into();
        let buffer = shape(&mut fonts, &label);
        assert!(buffer.lines[0].text().ends_with('…'));
        assert!(fits_width(&buffer, label.bounds[2]));
        label.bounds[2] = 1.0;
        assert!(shape(&mut fonts, &label).lines[0].text().is_empty());
    }

    #[test]
    fn ellipsis_keeps_grapheme_clusters_and_mixed_scripts() {
        let mut fonts = font_system(false);
        let label = Label::new(
            "e\u{301}".repeat(30),
            [0.0, 0.0, 85.0, 40.0],
            20.0,
            [255; 4],
        )
        .ellipsis();
        let buffer = shape(&mut fonts, &label);
        let text = buffer.lines[0].text();
        assert!(text.ends_with("\u{301}…"));
        assert!(fits_width(&buffer, label.bounds[2]));
        for text in [
            "العربية עברית Hearth Ελληνικά",
            "עברית العربية Hearth Ελληνικά",
        ] {
            let label = Label::new(text, [0.0, 0.0, 140.0, 40.0], 20.0, [255; 4]).ellipsis();
            let buffer = shape(&mut fonts, &label);
            assert!(buffer.lines[0].text().ends_with('…'));
            assert!(fits_width(&buffer, label.bounds[2]));
            assert!(
                buffer
                    .layout_runs()
                    .flat_map(|run| run.glyphs)
                    .all(|glyph| glyph.glyph_id != 0)
            );
        }
    }

    #[test]
    fn ellipsis_marks_omitted_lines_and_visibility_checks_original() -> Result<()> {
        let mut fonts = font_system(false);
        let mut label =
            Label::new("first\nsecond", [0.0, 0.0, 200.0, 100.0], 20.0, [255; 4]).ellipsis();
        assert_eq!(shape(&mut fonts, &label).lines[0].text(), "first…");
        assert!(!fully_visible(&label)?);
        assert_eq!(
            shape(&mut fonts, &label.clone().wrap())
                .layout_runs()
                .count(),
            2
        );
        label.text = "first\n".into();
        assert_eq!(shape(&mut fonts, &label).lines[0].text(), "first…");
        assert!(!fully_visible(&label)?);
        label.text = "first".into();
        assert!(fully_visible(&label)?);
        label.text = "first ".repeat(20);
        assert!(!fully_visible(&label)?);
        Ok(())
    }

    #[test]
    fn fixture_fonts_shape_mixed_scripts_without_missing_glyphs() {
        let mut fonts = font_system(false);
        let label = Label::new(
            "Hearth · Ελληνικά · العربية · עברית",
            [0.0, 0.0, 800.0, 60.0],
            20.0,
            [255; 4],
        );
        let buffer = shape(&mut fonts, &label);
        let glyphs: Vec<_> = buffer.layout_runs().flat_map(|run| run.glyphs).collect();
        assert!(!glyphs.is_empty());
        assert!(glyphs.iter().all(|glyph| glyph.glyph_id != 0));
    }

    #[test]
    fn arabic_falls_back_from_serif_to_sans() {
        let mut fonts = font_system(false);
        let label = Label::new("Hearth العربية", [0.0, 0.0, 500.0, 60.0], 20.0, [255; 4])
            .with_family("DejaVu Serif");
        let buffer = shape(&mut fonts, &label);
        let font_ids: HashSet<_> = buffer
            .layout_runs()
            .flat_map(|run| run.glyphs)
            .map(|glyph| glyph.font_id)
            .collect();
        assert_eq!(font_ids.len(), 2);
    }

    #[test]
    fn wrapping_uses_multiple_lines_and_clip_does_not() {
        let mut fonts = font_system(false);
        let label = Label::new(
            "A long label with several words",
            [0.0, 0.0, 100.0, 300.0],
            20.0,
            [255; 4],
        );
        assert_eq!(shape(&mut fonts, &label).layout_runs().count(), 1);
        assert!(shape(&mut fonts, &label.wrap()).layout_runs().count() > 1);
    }

    #[test]
    fn bounds_intersect_the_parent_clip() {
        let label = Label::new("clipped", [10.0, 20.0, 100.0, 60.0], 20.0, [255; 4])
            .with_clip([40.0, 10.0, 50.0, 50.0]);
        let bounds = label.text_bounds();
        assert_eq!(
            [bounds.left, bounds.top, bounds.right, bounds.bottom],
            [40, 20, 90, 60]
        );
    }

    #[test]
    fn invalid_metrics_are_rejected_before_shaping() {
        let mut label = Label::new("bad", [0.0, 0.0, 100.0, 40.0], 0.0, [255; 4]);
        assert!(label.validate().is_err());
        label.font_size = 20.0;
        label.bounds[2] = -1.0;
        assert!(label.validate().is_err());
        label.bounds[2] = 100.0;
        label.clip = Some([0.0, f32::NAN, 20.0, 20.0]);
        assert!(label.validate().is_err());
    }

    #[test]
    fn full_visibility_accepts_short_text_and_rejects_horizontal_overflow() -> Result<()> {
        let mut label = Label::new("Approve", [10.0, 10.0, 200.0, 40.0], 20.0, [255; 4]);
        assert!(fully_visible(&label)?);
        label.bounds[2] = 20.0;
        assert!(!fully_visible(&label)?);
        Ok(())
    }

    #[test]
    fn full_visibility_measures_lines_beyond_the_visible_buffer() -> Result<()> {
        let mut label = Label::new(
            "first\nsecond\nthird",
            [0.0, 0.0, 200.0, 40.0],
            20.0,
            [255; 4],
        );
        assert!(!fully_visible(&label)?);
        label.bounds[3] = 81.0;
        assert!(fully_visible(&label)?);
        label.text = "a long label that wraps to several lines".into();
        label.bounds[2] = 70.0;
        label.overflow = Overflow::Wrap;
        assert!(!fully_visible(&label)?);
        label.bounds[3] = 400.0;
        assert!(fully_visible(&label)?);
        Ok(())
    }

    #[test]
    fn full_visibility_counts_trailing_empty_lines_and_crlf() -> Result<()> {
        let mut label = Label::new("line\n", [0.0, 0.0, 200.0, 27.0], 20.0, [255; 4]);
        assert!(!fully_visible(&label)?);
        label.bounds[3] = 54.0;
        assert!(fully_visible(&label)?);
        label.text = "line\r\n".into();
        assert!(fully_visible(&label)?);
        label.text = "line\n\n".into();
        assert!(!fully_visible(&label)?);
        label.bounds[3] = 81.0;
        assert!(fully_visible(&label)?);
        Ok(())
    }

    #[test]
    fn full_visibility_requires_unclipped_glyphs_and_known_fonts() -> Result<()> {
        let label = Label::new("Approve", [10.0, 10.0, 200.0, 40.0], 20.0, [255; 4]);
        assert!(!fully_visible(
            &label.clone().with_clip([20.0, 10.0, 200.0, 40.0])
        )?);
        assert!(!fully_visible(
            &label.clone().with_clip([10.0, 10.0, 200.0, 10.0])
        )?);
        let mut missing = label.clone();
        missing.text = "\u{10FFFF}".into();
        assert!(!fully_visible(&missing)?);
        let mut oversized = label;
        oversized.text = "a".repeat(4097);
        assert!(!fully_visible(&oversized)?);
        Ok(())
    }
}
