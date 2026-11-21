# The Text tool's font

`NotoSans-Regular.ttf` is Noto Sans Regular (static, hinted TrueType) from the
Noto Project. `framefang_core::text` embeds it with `include_bytes!` and
ab_glyph rasterizes it, so text annotations come out pixel for pixel the same
on every operating system, with no system fonts involved.

| | |
| --- | --- |
| Source | <https://github.com/notofonts/latin-greek-cyrillic> |
| Version | Noto Sans 2022, `Copyright 2022 The Noto Project Authors` |
| SHA-256 | `478c558ea716033cd60c03438f628dfa75694dcf6b5f6d505a2f05fd2b4f3823` |
| License | SIL Open Font License 1.1: `OFL.txt` here, and `THIRD_PARTY_LICENSES.md` at the repository root |

It covers Latin with its accented letters, Greek, Cyrillic, punctuation,
currency signs and common math symbols. Scripts that Noto ships as separate
families (CJK, Arabic, Hebrew, the Indic scripts) and the symbol and arrow
blocks are left out to keep the binary small. A character the font lacks is
drawn as its placeholder box, so it still shows and takes up room.
