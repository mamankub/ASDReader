# Editing existing text and source subsets

Click a text region in **Edit a PDF → Edit text and images**. One editor contains its
source rows and mixed styles. Embedded TrueType and OpenType/CFF faces are used in memory;
source font programs, colour, size and baseline anchors stay intact. A textless preview
prevents a second copy underneath. The frame shows the live extent as text grows or shrinks.
Rows a text edit doesn't reach keep their original operators, so their kerning, `TJ` word
gaps and glyph widths stay exactly as they were. Only rows whose characters changed, or that
moved because an earlier row wrapped, are rewritten, and formatting a selection rewrites
only the rows it covers. Changing the whole paragraph's formatting, width or position rewrites
every row.

Add Text also opens existing text when it is clicked; a blank-space click starts a new item.
Both text tools retype an added text item with one click. Draft boxes grow to the page edge
before wrapping, and the saved item uses that width. A single visual source row grows across font/style boundaries;
multi-row paragraphs retain their existing wrap width. Retyping uses a preview with only
that added item's old drawing removed, so deleted draft characters do not show underneath.

A subset font contains only some of the original face's glyphs. A new character cannot be
reconstructed from a glyph the PDF does not contain. **Load font…** imports an embeddable
standalone `.ttf` or `.otf` face. **Installed fonts…** lists every such face installed on this
computer (Windows `Fonts`, macOS `/System/Library/Fonts` and `/Library/Fonts`, Linux `/usr/share/fonts`
and the user's font folders), with a search box, found on request off the UI thread; choosing one loads
it into the page and applies it to the paragraph. The document-font menu selects resources already on the
page. Select **Keep source fonts** to preserve each run's original face. A matching full face
completes its subsets automatically, including new Chinese characters. When the PDF itself embeds
the complete program of a paragraph's font (not a subset) and its licence allows editable
embedding, a character the font's encoding can't show is drawn with that same program. A
character no available face has is refused with an error that names it, and a CJK character
without a asdesign-fonts build says so. Replacement text takes the asdesign-fonts Japanese faces when
they can draw all of it, and the bundled Inter face (`assets/fonts`) for the Latin, Greek and
Cyrillic letters they lack (Uzbek `қизил`, Kazakh `әже`): that face needs no build input, so
these work in every build, the web one included. Choosing another face
is explicit. A font does not need to declare an open licence: its own embedding permissions
decide, and fonts with restricted embedding, variable fonts and the excluded families
(Adobe-authored, Source Han / Noto CJK) are refused or not listed. Each face of a collection
(`.ttc`, such as Windows' `cordia.ttc` with Cordia New and CordiaUPC) is listed and embedded on
its own.
The whole font program is embedded, so a large font makes the PDF larger. Interface fallback fonts
are never exported.

**The installed face a subset was cut from.** When a paragraph's own embedded subset can't show
the text typed (a Thai vowel or tone mark the document never used, say), the desktop app looks for
the full face on this computer by the names inside the embedded program, which survive even when
the PDF calls the font `CIDFont+F1` (PDFsharp does), loads it into the page and applies the edit
with it, so the paragraph keeps its look. Only that face is used, matched by PostScript name (a
bold subset never takes the regular face); without it the error is reported as before. The search
runs in the background from the moment a paragraph editor opens. Automation and the CLI never read
installed fonts on their own: they use `text_font_import` with an explicit file.

**Thai PUA glyphs.** Thai fonts made for software without OpenType shaping (Tahoma, TH
SarabunPSK, Angsana, Cordia…) carry extra glyphs for the marks that move: a tone mark lowered when
no upper vowel is under it (เจ้า, บ้าน), shifted left over ป ฝ ฟ ฬ (ไฟฟ้า, เป็น), lower vowels
dropped under ฎ ฏ, ญ ฐ without their tails. Producers often give those glyphs the Private Use Area
code point in `/ToUnicode` (U+F70B for the lowered mai tho) instead of the Thai character, so the
text read as `เจาของ`. Text extraction, search, copy and the editor read them as the Thai
character (`crates/fonts/src/thai_pua.rs`), and text written back with such a font takes the
variant the standard Thai positioning rules choose, so `เจ้าของ` keeps the lowered mark it had.
Thai and Lao marks drawn with no advance are read right after the letter drawn before them, even
when the producer places them a little past the next letter's start.

**Turned text** (a landscape drawing turned onto a portrait page, as CAD exports are) takes a chosen
document font for the whole paragraph; formatting part of it is still refused.

The full selected program, glyph mappings, widths and Unicode text are embedded in the PDF.
Saving and reopening therefore needs no locally installed font. Editing the same text again
reuses the font resource the page already has, and every resource made from one program
shares a single copy of it. The Type 3 fonts generated for Japanese or Hebrew characters the
page's fonts lack are made anew by each edit; after an edit, those nothing on the page shows any
more leave the page's resources, unless some content that could name them can't be read.
Original subset bytes are
preserved. Optional matching faces from `ASDESIGN_FONTS_DIR` can also complete source subsets;
this works only when that build input actually contains the full face. This change does not
add a font binary or change the release font pin. Fonts shared by releases belong in
`mamankub/asdesign-fonts` under the repository's asset rules.

Rewrapped Chinese and Japanese text, which has no spaces between words, may break between any
two characters. A row never begins with closing punctuation, a closing bracket or quote, a small
kana or the prolonged sound mark, and never ends with an opening bracket or quote or a currency
sign before an amount: the line-start and line-end rules of JIS X 4051, as in Unicode line
breaking (UAX #14). Latin words inside such text are not split. Korean and Thai still break only
at spaces.

Select text within the editor before changing size, colour, font, underline or spacing to
format that selection. Automation supports up to 1024 disjoint UTF-8 byte ranges with the
same properties; invalid or overlapping ranges fail atomically. Untouched source formatting
survives. OpenType shaping uses the selected/source face's substitution and positioning
rules, preserves marks and ligatures, and applies Unicode bidi ordering for a single face.
Each shaped run is one text object: its glyphs are shown in one `TJ` array whose adjustments
place each glyph where shaping put it, and a mark set above or below its letter is raised
with the text rise. Logical text remains available after save/reopen through ToUnicode and ActualText.
Added Arabic text records ActualText per shaped line for copying and searching in typing order.
ActualText already supplies typing order, so the editor preserves it without applying
a second bidi reversal; adjacent visual glyph runs still recover their own reading order.
Only right-to-left ActualText is kept apart this way: left-to-right spans, such as the Latin
ligatures (`fi`, `fl`) Word and Chrome wrap in ActualText mid-word, join their line as before.

A justified paragraph stretches each row but its last to the paragraph's width by widening its
spaces. Word spacing (`Tw`) widens only single-byte code 32, so where the font shows a space with
another code (a composite font, the generated Type 3 fallback) each space is followed by a `TJ`
adjustment instead. Justifying a paragraph with several formats in it is not supported yet: it is
left-aligned.

For headless editing, use `text_font_files` → `text_font_import` → `text_fonts` and pass a
resource to `text_edit.font_resource`. `text_edit.ranges` holds selected range formatting.
`text_edit_layout` accepts resource/ranges too and measures a draft without applying it.
All file inputs respect CLI root confinement; importing a font and applying text are undoable.

On a scanned page, recognised text (Scan & OCR) is an invisible layer over the picture. Edit
text groups it into phrases: words joined by a word space, with a table cell, a tab stop and each
scanned line kept apart. Applying an edit draws the new text visibly in the scan's ink colour (or
the one chosen) and first covers the old words with the paper colour sampled around them. Each
word keeps its position and width, and a longer phrase grows rightwards rather than wrapping. The
cover is one flat colour, and the text uses the layer's Helvetica, not the scanned typeface.
This applies to any invisible text (rendering mode 3), not just an ASDReader OCR layer: editing
invisible text, such as another OCR tool's layer, makes the rewritten paragraph visible over a
painted cover in the sampled paper colour. Edit text also groups invisible text differently: its lines never
merge into multi-line paragraphs, and words split at gaps of 0.6 em or more.

Explicit marked paragraph/heading scopes prevent unrelated tagged regions from merging.
Compatible geometry groups untagged content, including runs split across content streams
or equivalent graphics transforms. Geometry cannot infer author intent in every untagged
PDF. Semantic structure-tree preservation, legacy Type 1/Type 3 inline font preview,
font collections/variable-font instances, mixed-font/style contextual runs and complete rotated
layout fidelity still need broader coverage. Do not interpret the passing real-file checks
as a guarantee for every PDF.

Private acceptance tests are opt-in and keep inputs unmodified. They exercise actual Chinese
keyboard insertion/deletion on all four pages, source-face/baseline checks and identical
in-memory renders before and after saving/reopening. Synthetic tests cover full TrueType
mapping, OpenType/CFF, Arabic marks/ligatures/mixed bidi, repeated edits, disjoint styles,
invalid ranges, font selection and root confinement. CLI acceptance repeats insertion/deletion
and two saves/reopens on four pages; independent PDFium checks extraction and rendering.
No private document, font or screenshot
is included in the repository.

## Paragraph numbers (automation)

A paragraph's number is its position in a list that is regrouped from geometry on every read,
so an edit that merges two paragraphs, or splits one, moves the others. After `text_paragraphs`
the numbers name the paragraphs *that listing showed*: `text_edit`, `text_edit_layout` and
`page_render`'s `hide_text_block` find the paragraph again by its text and box, so editing paragraph 3 then paragraph 1 still edits the
two the listing showed. A paragraph an edit merged into a neighbour (or changed) is refused, and so is the edited paragraph
itself when the edit merged it into one, with
a message to list the page again, instead of rewriting whichever paragraph took its number
(#2516). The result of an edit names the numbers that are gone in `stale_paragraphs`. Listing
a page again renumbers it. The listings kept are capped (256 pages); once the cap drops
them, a page of an affected document is refused until it is listed again, instead of reading its
numbers as positions. Without a listing, a number is a position in the page's current
paragraphs, as before. The inline editor holds the paragraph a switching click asked for the
same way. What is still missing is an identity that survives save and reopen (design: #2508).
