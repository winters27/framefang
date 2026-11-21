/*!
The 5 by 7 pixel font Frame Fang draws its own interface with: button labels,
the size chip, step numbers, dialog text.

It covers printable ASCII (0x20 to 0x7E) and nothing else, on purpose. Text
the user types into an annotation goes through `framefang_core::text` instead,
which rasterises a bundled OFL typeface with `ab_glyph` and so handles accents,
Greek, Cyrillic and symbols. The interface strings are ASCII by construction
and look best on a crisp pixel grid, which is what this gives them. Any other
character draws as `?`, so a stray one still shows up as something.
*/

/// Glyph width in pixels at scale 1.
pub const GLYPH_W: i32 = 5;
/// Glyph height in pixels at scale 1.
pub const GLYPH_H: i32 = 7;

/// First and last characters the font covers.
const FIRST: u32 = 0x20;
const LAST: u32 = 0x7E;
const COUNT: usize = (LAST - FIRST + 1) as usize;

/// The glyphs as pictures, one per character in code order. Each entry is the
/// character itself, then its seven rows top to bottom, each five pixels wide
/// with `#` for ink and `.` for paper. Keeping them as pictures means a glyph
/// can be read and fixed by eye; `decode` turns them into bit rows at compile
/// time.
const ART: [&str; COUNT] = [
    "  ..... ..... ..... ..... ..... ..... .....",
    "! ..#.. ..#.. ..#.. ..#.. ..#.. ..... ..#..",
    "\" .#.#. .#.#. ..... ..... ..... ..... .....",
    "# .#.#. ##### .#.#. .#.#. ##### .#.#. .....",
    "$ ..#.. .#### #.#.. .###. ..#.# ####. ..#..",
    "% ##... ##..# ...#. ..#.. .#... #..## ...##",
    "& .#... #.#.. #.#.. .#... #.#.# #..#. .##.#",
    "' ..#.. ..#.. ..... ..... ..... ..... .....",
    "( ...#. ..#.. .#... .#... .#... ..#.. ...#.",
    ") .#... ..#.. ...#. ...#. ...#. ..#.. .#...",
    "* ..... ..#.. #.#.# .###. #.#.# ..#.. .....",
    "+ ..... ..#.. ..#.. ##### ..#.. ..#.. .....",
    ", ..... ..... ..... ..... ..... ..#.. .#...",
    "- ..... ..... ..... ##### ..... ..... .....",
    ". ..... ..... ..... ..... ..... ..... ..#..",
    "/ ....# ...#. ...#. ..#.. .#... .#... #....",
    "0 .###. #...# #..## #.#.# ##..# #...# .###.",
    "1 ..#.. .##.. ..#.. ..#.. ..#.. ..#.. .###.",
    "2 .###. #...# ....# ...#. ..#.. .#... #####",
    "3 ####. ....# ....# .###. ....# ....# ####.",
    "4 ...#. ..##. .#.#. #..#. ##### ...#. ...#.",
    "5 ##### #.... ####. ....# ....# #...# .###.",
    "6 ..##. .#... #.... ####. #...# #...# .###.",
    "7 ##### ....# ...#. ..#.. .#... .#... .#...",
    "8 .###. #...# #...# .###. #...# #...# .###.",
    "9 .###. #...# #...# .#### ....# ...#. .##..",
    ": ..... ..#.. ..... ..... ..... ..#.. .....",
    "; ..... ..#.. ..... ..... ..... ..#.. .#...",
    "< ...#. ..#.. .#... #.... .#... ..#.. ...#.",
    "= ..... ..... ##### ..... ##### ..... .....",
    "> .#... ..#.. ...#. ....# ...#. ..#.. .#...",
    "? .###. #...# ....# ...#. ..#.. ..... ..#..",
    "@ .###. #...# #.### #.#.# #.### #.... .####",
    "A .###. #...# #...# ##### #...# #...# #...#",
    "B ####. #...# #...# ####. #...# #...# ####.",
    "C .###. #...# #.... #.... #.... #...# .###.",
    "D ###.. #..#. #...# #...# #...# #..#. ###..",
    "E ##### #.... #.... ####. #.... #.... #####",
    "F ##### #.... #.... ####. #.... #.... #....",
    "G .###. #...# #.... #.### #...# #...# .####",
    "H #...# #...# #...# ##### #...# #...# #...#",
    "I .###. ..#.. ..#.. ..#.. ..#.. ..#.. .###.",
    "J ..### ...#. ...#. ...#. ...#. #..#. .##..",
    "K #...# #..#. #.#.. ##... #.#.. #..#. #...#",
    "L #.... #.... #.... #.... #.... #.... #####",
    "M #...# ##.## #.#.# #.#.# #...# #...# #...#",
    "N #...# #...# ##..# #.#.# #..## #...# #...#",
    "O .###. #...# #...# #...# #...# #...# .###.",
    "P ####. #...# #...# ####. #.... #.... #....",
    "Q .###. #...# #...# #...# #.#.# #..#. .##.#",
    "R ####. #...# #...# ####. #.#.. #..#. #...#",
    "S .#### #.... #.... .###. ....# ....# ####.",
    "T ##### ..#.. ..#.. ..#.. ..#.. ..#.. ..#..",
    "U #...# #...# #...# #...# #...# #...# .###.",
    "V #...# #...# #...# #...# #...# .#.#. ..#..",
    "W #...# #...# #...# #.#.# #.#.# #.#.# .#.#.",
    "X #...# #...# .#.#. ..#.. .#.#. #...# #...#",
    "Y #...# #...# #...# .#.#. ..#.. ..#.. ..#..",
    "Z ##### ....# ...#. ..#.. .#... #.... #####",
    "[ .###. .#... .#... .#... .#... .#... .###.",
    "\\ #.... .#... .#... ..#.. ...#. ...#. ....#",
    "] .###. ...#. ...#. ...#. ...#. ...#. .###.",
    "^ ..#.. .#.#. #...# ..... ..... ..... .....",
    "_ ..... ..... ..... ..... ..... ..... #####",
    "` .#... ..#.. ...#. ..... ..... ..... .....",
    "a ..... ..... .###. ....# .#### #...# .####",
    "b #.... #.... ####. #...# #...# #...# ####.",
    "c ..... ..... .###. #...# #.... #...# .###.",
    "d ....# ....# .#### #...# #...# #...# .####",
    "e ..... ..... .###. #...# ##### #.... .###.",
    "f ..##. .#..# .#... ####. .#... .#... .#...",
    "g ..... ..... .#### #...# .#### ....# .###.",
    "h #.... #.... ####. #...# #...# #...# #...#",
    "i ..#.. ..... .##.. ..#.. ..#.. ..#.. .###.",
    "j ...#. ..... ..##. ...#. ...#. #..#. .##..",
    "k #.... #.... #..#. #.#.. ##... #.#.. #..#.",
    "l .##.. ..#.. ..#.. ..#.. ..#.. ..#.. .###.",
    "m ..... ..... ##.#. #.#.# #.#.# #...# #...#",
    "n ..... ..... ####. #...# #...# #...# #...#",
    "o ..... ..... .###. #...# #...# #...# .###.",
    "p ..... ..... ####. #...# ####. #.... #....",
    "q ..... ..... .#### #...# .#### ....# ....#",
    "r ..... ..... #.##. ##..# #.... #.... #....",
    "s ..... ..... .#### #.... .###. ....# ####.",
    "t .#... .#... ####. .#... .#... .#..# ..##.",
    "u ..... ..... #...# #...# #...# #...# .####",
    "v ..... ..... #...# #...# #...# .#.#. ..#..",
    "w ..... ..... #...# #...# #.#.# #.#.# .#.#.",
    "x ..... ..... #...# .#.#. ..#.. .#.#. #...#",
    "y ..... ..... #...# #...# .#### ....# .###.",
    "z ..... ..... ##### ...#. ..#.. .#... #####",
    "{ ...#. ..#.. ..#.. .#... ..#.. ..#.. ...#.",
    "| ..#.. ..#.. ..#.. ..#.. ..#.. ..#.. ..#..",
    "} .#... ..#.. ..#.. ...#. ..#.. ..#.. .#...",
    "~ ..... .#..# #.#.# #..#. ..... ..... .....",
];

/// Every glyph as seven rows of bits, indexed by `char - 0x20`. In each row
/// bit 4 is the leftmost pixel and bit 0 the rightmost, which is how
/// `painter::draw_text` reads them.
pub const FONT_5X7: [[u8; 7]; 95] = decode_all();

/// The table again as a static, so `glyph` can hand out `'static` borrows.
static GLYPHS: [[u8; 7]; COUNT] = FONT_5X7;

/// Turn one picture into bit rows. Byte 0 is the character, byte 1 a space,
/// and each row after that is five pixel bytes plus a separator.
const fn decode(picture: &str) -> [u8; 7] {
    let bytes = picture.as_bytes();
    let mut rows = [0u8; 7];
    let mut row = 0;
    while row < 7 {
        let start = 2 + row * 6;
        let mut col = 0;
        while col < 5 {
            if bytes[start + col] == b'#' {
                rows[row] |= 0b1_0000 >> col;
            }
            col += 1;
        }
        row += 1;
    }
    rows
}

const fn decode_all() -> [[u8; 7]; COUNT] {
    let mut table = [[0u8; 7]; COUNT];
    let mut i = 0;
    while i < COUNT {
        table[i] = decode(ART[i]);
        i += 1;
    }
    table
}

/// The bit rows for `c`, or for `?` when the font does not cover `c`.
pub fn glyph(c: char) -> &'static [u8; 7] {
    let code = u32::from(c);
    let slot = if (FIRST..=LAST).contains(&code) { code } else { u32::from('?') };
    &GLYPHS[(slot - FIRST) as usize]
}

/// How many pixels wide `text` draws at `scale`. Each character takes
/// `GLYPH_W` pixels with a one pixel gap between neighbours (none after the
/// last one), all multiplied by `scale`. Counted in chars, so a non-ASCII
/// character costs one `?` cell, matching what `glyph` draws.
pub fn measure(text: &str, scale: i32) -> i32 {
    let chars = text.chars().count() as i32;
    if chars == 0 {
        return 0;
    }
    (chars * (GLYPH_W + 1) - 1) * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a over every byte of the table, taken from the hand-written bit
    /// table this font was first shipped with. If any pixel of any glyph
    /// moves, this changes.
    const SHIPPED_TABLE_FNV1A: u64 = 0x57f0_8659_8e7a_d3b5;

    fn fnv1a(rows: &[[u8; 7]]) -> u64 {
        rows.iter().flatten().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn decoded_table_matches_the_shipped_pixels() {
        assert_eq!(fnv1a(&FONT_5X7), SHIPPED_TABLE_FNV1A);
    }

    #[test]
    fn sample_glyphs_have_the_shipped_rows() {
        let samples: [(char, [u8; 7]); 6] = [
            (' ', [0; 7]),
            ('!', [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100]),
            ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
            ('A', [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
            ('g', [0b00000, 0b00000, 0b01111, 0b10001, 0b01111, 0b00001, 0b01110]),
            ('~', [0b00000, 0b01001, 0b10101, 0b10010, 0b00000, 0b00000, 0b00000]),
        ];
        for (c, rows) in samples {
            assert_eq!(glyph(c), &rows, "{c:?}");
        }
    }

    #[test]
    fn every_picture_is_well_formed_and_in_code_order() {
        for (i, picture) in ART.iter().enumerate() {
            let bytes = picture.as_bytes();
            assert_eq!(bytes.len(), 2 + 7 * 6 - 1, "entry {i}");
            assert_eq!(u32::from(bytes[0]), FIRST + i as u32, "entry {i} is out of order");
            for (at, &b) in bytes.iter().enumerate().skip(1) {
                let separator = (at - 1) % 6 == 0;
                let ok = if separator { b == b' ' } else { b == b'#' || b == b'.' };
                assert!(ok, "entry {i} has {:?} at byte {at}", b as char);
            }
        }
    }

    #[test]
    fn uncovered_characters_draw_as_a_question_mark() {
        let question = glyph('?');
        for c in ['\u{7F}', '\n', '\u{1F}', '\u{301}', 'Ж', '\u{1F600}'] {
            assert_eq!(glyph(c), question, "{c:?}");
        }
        assert_ne!(glyph('a'), question);
    }

    #[test]
    fn measure_counts_cells_and_inner_gaps() {
        assert_eq!(measure("", 1), 0);
        assert_eq!(measure("", 3), 0);
        assert_eq!(measure("A", 1), 5);
        assert_eq!(measure("AB", 1), 11);
        assert_eq!(measure("ABC", 2), 34);
        // A multi-byte character is still one cell.
        assert_eq!(measure("Ж", 1), measure("?", 1));
    }
}
