use std::sync::Arc;

use {
    ahash::AHashMap,
    parking_lot::RwLock,
    rusttype::{point, Scale},
};

use crate::{
    math::{vec2, Vec2},
    mutex::Mutex,
};

use super::texture_atlas::TextureAtlas;

#[derive(Clone, Copy, Debug, Default)]
pub struct GalleyCursor {
    /// character count in whole galley
    pub char_idx: usize,
    /// line number
    pub line: usize,
    /// character count on this line
    pub column: usize,
}

/// A collection of text locked into place.
#[derive(Clone, Debug, Default)]
pub struct Galley {
    /// The full text, including any an all `\n`.
    pub text: String,

    /// Lines of text, from top to bottom.
    /// The number of chars in all lines sum up to text.chars().count().
    /// Note that each paragraph (pieces of text separated with `\n`)
    /// can be split up into multiple lines.
    pub lines: Vec<Line>,

    // Optimization: calculated once and reused.
    pub size: Vec2,
}

/// A typeset piece of text on a single line.
#[derive(Clone, Debug)]
pub struct Line {
    /// The start of each character, probably starting at zero.
    /// The last element is the end of the last character.
    /// This is never empty.
    /// Unit: points.
    ///
    /// `x_offsets.len() + (ends_with_newline as usize) == text.chars().count() + 1`
    pub x_offsets: Vec<f32>,

    /// Top of the line, offset within the Galley.
    /// Unit: points.
    pub y_min: f32,

    /// Bottom of the line, offset within the Galley.
    /// Unit: points.
    pub y_max: f32,

    /// If true, this Line came from a paragraph ending with a `\n`.
    /// The `\n` itself is omitted from `x_offsets`.
    /// A `\n` in the input text always creates a new `Line` below it,
    /// so that text that ends with `\n` has an empty `Line` last.
    /// This also implies that the last `Line` in a `Galley` always has `ends_with_newline == false`.
    pub ends_with_newline: bool,
}

impl Galley {
    pub fn sanity_check(&self) {
        let mut char_count = 0;
        for line in &self.lines {
            line.sanity_check();
            char_count += line.char_count_including_newline();
        }
        assert_eq!(char_count, self.text.chars().count());
        if let Some(last_line) = self.lines.last() {
            debug_assert!(
                !last_line.ends_with_newline,
                "If the text ends with '\\n', there would be an empty Line last.\n\
                Galley: {:#?}",
                self
            );
        }
    }

    /// If given a char index after the first line, the end of the last character is returned instead.
    /// Returns a Vec2 rather than a Pos2 as this is an offset into the galley. *shrug*
    pub fn char_start_pos(&self, char_idx: usize) -> Vec2 {
        let mut char_count = 0;
        for line in &self.lines {
            let line_char_count = line.char_count_including_newline();
            if char_count <= char_idx && char_idx < char_count + line_char_count {
                let line_char_offset = char_idx - char_count;
                return vec2(line.x_offsets[line_char_offset], line.y_min);
            }
            char_count += line_char_count;
        }

        if let Some(last) = self.lines.last() {
            vec2(last.max_x(), last.y_min)
        } else {
            // Empty galley
            vec2(0.0, 0.0)
        }
    }

    /// Character offset at the given position within the galley
    pub fn char_at(&self, pos: Vec2) -> GalleyCursor {
        let mut best_y_dist = f32::INFINITY;
        let mut cursor = GalleyCursor::default();

        let mut char_count = 0;
        for (line_nr, line) in self.lines.iter().enumerate() {
            let y_dist = (line.y_min - pos.y).abs().min((line.y_max - pos.y).abs());
            if y_dist < best_y_dist {
                best_y_dist = y_dist;
                let column = line.char_at(pos.x);
                cursor = GalleyCursor {
                    char_idx: char_count + column,
                    line: line_nr,
                    column,
                }
            }
            char_count += line.char_count_including_newline();
        }
        cursor
    }
}

impl Line {
    pub fn sanity_check(&self) {
        assert!(!self.x_offsets.is_empty());
    }

    /// Excludes the implicit `\n` after the `Line`, if any.
    pub fn char_count_excluding_newline(&self) -> usize {
        assert!(!self.x_offsets.is_empty());
        self.x_offsets.len() - 1
    }

    /// Includes the implicit `\n` after the `Line`, if any.
    pub fn char_count_including_newline(&self) -> usize {
        self.char_count_excluding_newline() + (self.ends_with_newline as usize)
    }

    pub fn min_x(&self) -> f32 {
        *self.x_offsets.first().unwrap()
    }

    pub fn max_x(&self) -> f32 {
        *self.x_offsets.last().unwrap()
    }

    /// Closest char at the desired x coordinate.
    /// Returns something in the range `[0, char_count_excluding_newline()]`
    pub fn char_at(&self, desired_x: f32) -> usize {
        for (i, char_x_bounds) in self.x_offsets.windows(2).enumerate() {
            let char_center_x = 0.5 * (char_x_bounds[0] + char_x_bounds[1]);
            if desired_x < char_center_x {
                return i;
            }
        }
        self.char_count_excluding_newline()
    }
}

// ----------------------------------------------------------------------------

// const REPLACEMENT_CHAR: char = '\u{25A1}'; // □ white square Replaces a missing or unsupported Unicode character.
// const REPLACEMENT_CHAR: char = '\u{FFFD}'; // � REPLACEMENT CHARACTER
const REPLACEMENT_CHAR: char = '?';

#[derive(Clone, Copy, Debug)]
pub struct UvRect {
    /// X/Y offset for nice rendering (unit: points).
    pub offset: Vec2,
    pub size: Vec2,

    /// Top left corner UV in texture.
    pub min: (u16, u16),

    /// Bottom right corner (exclusive).
    pub max: (u16, u16),
}

#[derive(Clone, Copy, Debug)]
pub struct GlyphInfo {
    id: rusttype::GlyphId,

    /// Unit: points.
    pub advance_width: f32,

    /// Texture coordinates. None for space.
    pub uv_rect: Option<UvRect>,
}

/// The interface uses points as the unit for everything.
pub struct Font {
    font: rusttype::Font<'static>,
    /// Maximum character height
    scale_in_pixels: f32,
    pixels_per_point: f32,
    replacement_glyph_info: GlyphInfo,
    glyph_infos: RwLock<AHashMap<char, GlyphInfo>>,
    atlas: Arc<Mutex<TextureAtlas>>,
}

impl Font {
    pub fn new(
        atlas: Arc<Mutex<TextureAtlas>>,
        font_data: &'static [u8],
        scale_in_points: f32,
        pixels_per_point: f32,
    ) -> Font {
        assert!(scale_in_points > 0.0);
        assert!(pixels_per_point > 0.0);

        let font = rusttype::Font::try_from_bytes(font_data).expect("Error constructing Font");
        let scale_in_pixels = pixels_per_point * scale_in_points;

        let replacement_glyph_info = allocate_glyph(
            &mut atlas.lock(),
            REPLACEMENT_CHAR,
            &font,
            scale_in_pixels,
            pixels_per_point,
        )
        .unwrap_or_else(|| {
            panic!(
                "Failed to find replacement character {:?}",
                REPLACEMENT_CHAR
            )
        });

        let font = Font {
            font,
            scale_in_pixels,
            pixels_per_point,
            replacement_glyph_info,
            glyph_infos: Default::default(),
            atlas,
        };

        font.glyph_infos
            .write()
            .insert(REPLACEMENT_CHAR, font.replacement_glyph_info);

        // Preload the printable ASCII characters [32, 126] (which excludes control codes):
        const FIRST_ASCII: usize = 32; // 32 == space
        const LAST_ASCII: usize = 126;
        for c in (FIRST_ASCII..=LAST_ASCII).map(|c| c as u8 as char) {
            font.glyph_info(c);
        }
        font.glyph_info('°');

        font
    }

    pub fn round_to_pixel(&self, point: f32) -> f32 {
        (point * self.pixels_per_point).round() / self.pixels_per_point
    }

    /// Height of one line of text. In points
    /// TODO: rename height ?
    pub fn line_spacing(&self) -> f32 {
        self.scale_in_pixels / self.pixels_per_point
    }
    pub fn height(&self) -> f32 {
        self.scale_in_pixels / self.pixels_per_point
    }

    pub fn uv_rect(&self, c: char) -> Option<UvRect> {
        self.glyph_infos.read().get(&c).and_then(|gi| gi.uv_rect)
    }

    /// `\n` will (intentionally) show up as '?' (`REPLACEMENT_CHAR`)
    fn glyph_info(&self, c: char) -> GlyphInfo {
        {
            if let Some(glyph_info) = self.glyph_infos.read().get(&c) {
                return *glyph_info;
            }
        }

        // Add new character:
        let glyph_info = allocate_glyph(
            &mut self.atlas.lock(),
            c,
            &self.font,
            self.scale_in_pixels,
            self.pixels_per_point,
        );
        // debug_assert!(glyph_info.is_some(), "Failed to find {:?}", c);
        let glyph_info = glyph_info.unwrap_or(self.replacement_glyph_info);
        self.glyph_infos.write().insert(c, glyph_info);
        glyph_info
    }

    /// Typeset the given text onto one line.
    /// Any `\n` will show up as `REPLACEMENT_CHAR` ('?').
    /// Always returns exactly one `Line` in the `Galley`.
    pub fn layout_single_line(&self, text: String) -> Galley {
        let x_offsets = self.layout_single_line_fragment(&text);
        let line = Line {
            x_offsets,
            y_min: 0.0,
            y_max: self.height(),
            ends_with_newline: false,
        };
        let width = line.max_x();
        let size = vec2(width, self.height());
        let galley = Galley {
            text,
            lines: vec![line],
            size,
        };
        galley.sanity_check();
        galley
    }

    pub fn layout_multiline(&self, text: String, max_width_in_points: f32) -> Galley {
        let line_spacing = self.line_spacing();
        let mut cursor_y = 0.0;
        let mut lines = Vec::new();

        let mut paragraph_start = 0;

        while paragraph_start < text.len() {
            let next_newline = text[paragraph_start..].find('\n');
            let paragraph_end = next_newline
                .map(|newline| paragraph_start + newline)
                .unwrap_or_else(|| text.len());

            assert!(paragraph_start <= paragraph_end);
            let paragraph_text = &text[paragraph_start..paragraph_end];
            let mut paragraph_lines =
                self.layout_paragraph_max_width(paragraph_text, max_width_in_points);
            assert!(!paragraph_lines.is_empty());
            paragraph_lines.last_mut().unwrap().ends_with_newline = next_newline.is_some();

            for line in &mut paragraph_lines {
                line.y_min += cursor_y;
                line.y_max += cursor_y;
            }
            cursor_y = paragraph_lines.last().unwrap().y_max;
            cursor_y += line_spacing * 0.4; // Extra spacing between paragraphs. TODO: less hacky

            lines.append(&mut paragraph_lines);

            paragraph_start = paragraph_end + 1;
        }

        if text.is_empty() || text.ends_with('\n') {
            lines.push(Line {
                x_offsets: vec![0.0],
                y_min: cursor_y,
                y_max: cursor_y + line_spacing,
                ends_with_newline: false,
            });
        }

        let mut widest_line = 0.0;
        for line in &lines {
            widest_line = line.max_x().max(widest_line);
        }
        let size = vec2(widest_line, lines.last().unwrap().y_max);

        let galley = Galley { text, lines, size };
        galley.sanity_check();
        galley
    }

    /// Typeset the given text onto one line.
    /// Assumes there are no `\n` in the text.
    /// Return `x_offsets`, one longer than the number of characters in the text.
    fn layout_single_line_fragment(&self, text: &str) -> Vec<f32> {
        let scale_in_pixels = Scale::uniform(self.scale_in_pixels);

        let mut x_offsets = Vec::with_capacity(text.chars().count() + 1);
        x_offsets.push(0.0);

        let mut cursor_x_in_points = 0.0f32;
        let mut last_glyph_id = None;

        for c in text.chars() {
            let glyph = self.glyph_info(c);

            if let Some(last_glyph_id) = last_glyph_id {
                cursor_x_in_points +=
                    self.font
                        .pair_kerning(scale_in_pixels, last_glyph_id, glyph.id)
                        / self.pixels_per_point
            }
            cursor_x_in_points += glyph.advance_width;
            cursor_x_in_points = self.round_to_pixel(cursor_x_in_points);
            last_glyph_id = Some(glyph.id);

            x_offsets.push(cursor_x_in_points);
        }

        x_offsets
    }

    /// A paragraph is text with no line break character in it.
    /// The text will be wrapped by the given `max_width_in_points`.
    fn layout_paragraph_max_width(&self, text: &str, max_width_in_points: f32) -> Vec<Line> {
        if text == "" {
            return vec![Line {
                x_offsets: vec![0.0],
                y_min: 0.0,
                y_max: self.height(),
                ends_with_newline: false,
            }];
        }

        let full_x_offsets = self.layout_single_line_fragment(text);

        let mut line_start_x = full_x_offsets[0];

        {
            #![allow(clippy::float_cmp)]
            assert_eq!(line_start_x, 0.0);
        }

        let mut cursor_y = 0.0;
        let mut line_start_idx = 0;

        // start index of the last space. A candidate for a new line.
        let mut last_space = None;

        let mut out_lines = vec![];

        for (i, (x, chr)) in full_x_offsets.iter().skip(1).zip(text.chars()).enumerate() {
            debug_assert!(chr != '\n');
            let line_width = x - line_start_x;

            if line_width > max_width_in_points {
                if let Some(last_space_idx) = last_space {
                    let include_trailing_space = true;
                    let line = if include_trailing_space {
                        Line {
                            x_offsets: full_x_offsets[line_start_idx..=last_space_idx + 1]
                                .iter()
                                .map(|x| x - line_start_x)
                                .collect(),
                            y_min: cursor_y,
                            y_max: cursor_y + self.height(),
                            ends_with_newline: false,
                        }
                    } else {
                        Line {
                            x_offsets: full_x_offsets[line_start_idx..=last_space_idx]
                                .iter()
                                .map(|x| x - line_start_x)
                                .collect(),
                            y_min: cursor_y,
                            y_max: cursor_y + self.height(),
                            ends_with_newline: false,
                        }
                    };
                    line.sanity_check();
                    out_lines.push(line);

                    line_start_idx = last_space_idx + 1;
                    line_start_x = full_x_offsets[line_start_idx];
                    last_space = None;
                    cursor_y += self.line_spacing();
                    cursor_y = self.round_to_pixel(cursor_y);
                }
            }

            const NON_BREAKING_SPACE: char = '\u{A0}';
            if chr.is_whitespace() && chr != NON_BREAKING_SPACE {
                last_space = Some(i);
            }
        }

        if line_start_idx + 1 < full_x_offsets.len() {
            let line = Line {
                x_offsets: full_x_offsets[line_start_idx..]
                    .iter()
                    .map(|x| x - line_start_x)
                    .collect(),
                y_min: cursor_y,
                y_max: cursor_y + self.height(),
                ends_with_newline: false,
            };
            line.sanity_check();
            out_lines.push(line);
        }

        out_lines
    }
}

fn allocate_glyph(
    atlas: &mut TextureAtlas,
    c: char,
    font: &rusttype::Font<'static>,
    scale_in_pixels: f32,
    pixels_per_point: f32,
) -> Option<GlyphInfo> {
    let glyph = font.glyph(c);
    if glyph.id().0 == 0 {
        return None; // Failed to find a glyph for the character
    }

    let glyph = glyph.scaled(Scale::uniform(scale_in_pixels));
    let glyph = glyph.positioned(point(0.0, 0.0));

    let uv_rect = if let Some(bb) = glyph.pixel_bounding_box() {
        let glyph_width = bb.width() as usize;
        let glyph_height = bb.height() as usize;
        assert!(glyph_width >= 1);
        assert!(glyph_height >= 1);

        let glyph_pos = atlas.allocate((glyph_width, glyph_height));

        let texture = atlas.texture_mut();
        glyph.draw(|x, y, v| {
            if v > 0.0 {
                let px = glyph_pos.0 + x as usize;
                let py = glyph_pos.1 + y as usize;
                texture[(px, py)] = (v * 255.0).round() as u8;
            }
        });

        let offset_y_in_pixels = scale_in_pixels as f32 + bb.min.y as f32 - 4.0 * pixels_per_point; // TODO: use font.v_metrics
        Some(UvRect {
            offset: vec2(
                bb.min.x as f32 / pixels_per_point,
                offset_y_in_pixels / pixels_per_point,
            ),
            size: vec2(glyph_width as f32, glyph_height as f32) / pixels_per_point,
            min: (glyph_pos.0 as u16, glyph_pos.1 as u16),
            max: (
                (glyph_pos.0 + glyph_width) as u16,
                (glyph_pos.1 + glyph_height) as u16,
            ),
        })
    } else {
        // No bounding box. Maybe a space?
        None
    };

    let advance_width_in_points = glyph.unpositioned().h_metrics().advance_width / pixels_per_point;

    Some(GlyphInfo {
        id: glyph.id(),
        advance_width: advance_width_in_points,
        uv_rect,
    })
}

#[test]
fn test_text_layout() {
    let pixels_per_point = 1.0;
    let typeface_data = include_bytes!("../../fonts/ProggyClean.ttf");
    let atlas = TextureAtlas::new(512, 16);
    let atlas = Arc::new(Mutex::new(atlas));
    let font = Font::new(atlas, typeface_data, 13.0, pixels_per_point);

    let galley = font.layout_multiline("".to_owned(), 1024.0);
    assert_eq!(galley.lines.len(), 1);
    assert_eq!(galley.lines[0].ends_with_newline, false);
    assert_eq!(galley.lines[0].x_offsets, vec![0.0]);

    let galley = font.layout_multiline("\n".to_owned(), 1024.0);
    assert_eq!(galley.lines.len(), 2);
    assert_eq!(galley.lines[0].ends_with_newline, true);
    assert_eq!(galley.lines[1].ends_with_newline, false);
    assert_eq!(galley.lines[1].x_offsets, vec![0.0]);

    let galley = font.layout_multiline("\n\n".to_owned(), 1024.0);
    assert_eq!(galley.lines.len(), 3);
    assert_eq!(galley.lines[0].ends_with_newline, true);
    assert_eq!(galley.lines[1].ends_with_newline, true);
    assert_eq!(galley.lines[2].ends_with_newline, false);
    assert_eq!(galley.lines[2].x_offsets, vec![0.0]);

    let galley = font.layout_multiline(" ".to_owned(), 1024.0);
    assert_eq!(galley.lines.len(), 1);
    assert_eq!(galley.lines[0].ends_with_newline, false);

    let galley = font.layout_multiline("One line".to_owned(), 1024.0);
    assert_eq!(galley.lines.len(), 1);
    assert_eq!(galley.lines[0].ends_with_newline, false);

    let galley = font.layout_multiline("First line\n".to_owned(), 1024.0);
    assert_eq!(galley.lines.len(), 2);
    assert_eq!(galley.lines[0].ends_with_newline, true);
    assert_eq!(galley.lines[1].ends_with_newline, false);
    assert_eq!(galley.lines[1].x_offsets, vec![0.0]);

    // Test wrapping:
    let galley = font.layout_multiline("line wrap".to_owned(), 10.0);
    assert_eq!(galley.lines.len(), 2);
    assert_eq!(galley.lines[0].ends_with_newline, false);
    assert_eq!(galley.lines[1].ends_with_newline, false);

    let galley = font.layout_multiline("line\nwrap".to_owned(), 10.0);
    assert_eq!(galley.lines.len(), 2);
    assert_eq!(galley.lines[0].ends_with_newline, true);
    assert_eq!(galley.lines[1].ends_with_newline, false);
}
