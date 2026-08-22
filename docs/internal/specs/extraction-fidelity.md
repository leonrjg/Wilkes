# Extraction Fidelity — Design

Status: proposed (2026-08-22)
Branch: `develop`
Depends on: extractor registry, source maps, `ExtractionRecipe` identity, semantic index, PDF search projection

## 1. Purpose

`ExtractedContent.text` is Wilkes's reading of a document. Everything Wilkes
offers is a view over it: literal and semantic search, chunk vectors,
`get_document_text`, grep context lines, preview highlighting, the reading pane,
and the managed corpus export.

Today that text is a transcription of the page's **layout**, not of its
**text**. It contains hyphens that exist only because a line ended, bare page
numbers, running heads, and margin glossary boxes spliced into the middle of
sentences. Wilkes already knows this — `PdfSearchProjection` exists solely to
undo it — but the correction is applied to one consumer and the stored reading
keeps the defect.

This design moves the correction to where the text is produced, so there is one
reading of a document instead of a defective one plus a compensating view of it.
It also completes the declared-outline resolution Wilkes promises: a heading is
anchored to its position, not to the top of the page it appears on.

### Goals

- One canonical `ExtractedContent.text` per rendition, free of layout artifacts,
  with a source map that stays exact.
- Line-wrapped words joined; page furniture removed; marginalia moved out of the
  reading order rather than deleted.
- `OutlineEntry.byte_offset` populated for PDFs, with an observable
  fallback ladder and no guessing.
- `resolve_outline` anchoring on position when position is known.
- The compensating hyphen handling in `PdfSearchProjection` retired.

### Non-goals

- OCR, or any improvement to *which* glyphs are read. This is about the order
  and shape of text already extracted correctly.
- Reflowing tables, figures or equations into linear prose.
- Inferring headings that the document does not declare. A document with no
  outline still has no outline.
- Changing chunking, embedding, or similarity arithmetic. Chunks keep tiling the
  text; only the text underneath them changes.

## 2. Invariant

> A document has exactly one extracted reading. Every consumer — search,
> embeddings, export, display — reads that text and no other. Any normalization
> a consumer would have to perform for itself is a defect in the reading, not a
> feature of the consumer.
>
> The source map remains total and exact: every byte of the reading maps to the
> page position it came from, and every retained byte of the document appears
> somewhere in the reading.

The second half is what separates this from "clean up the text". Removing a page
number is only correct if the map still resolves; relocating a margin box is
only correct if its bytes still exist and still point at the box.

## 3. What the text actually contains

First-hand, from `get_document_text` on a library PDF (IU coursebook,
*DLBCSCT01-01 — Cryptography*, pages 130–132):

- `many web applications and APIs do not properly pro-\ntect sensitive data`
- `to exploit other imple-\nmentation flaws`
- a bare `128` between two paragraphs — the printed page number
- the page opening with a margin glossary box, `Serialization and deseri-\nalization\nSerialization is the proc-\ness of…`, before the sentence it interrupts

Counted across a downstream consumer's model inputs over three coursebooks
(a proxy for the reading itself, since those inputs are chunk text verbatim):

| Artifact | Count | Reach |
|---|---|---|
| Hyphen-broken words | 1,833 (1,095 distinct) | 122 of 128 documents-sections |
| Bare page-number lines | 765 | 122 of 128 |

The three most frequent broken words are `exam-ple` (18), `infor-mation` (15),
`How-ever` (14) — ordinary vocabulary, not domain terms. Semantic search embeds
`exam` and `ple` as separate tokens 18 times in one book; literal search for
`example` finds those occurrences only because the projection rescues it.

## 4. The second-text defect

[`PdfSearchProjection`](../../../crates/core/src/search/pdf_projection.rs)
is documented as a *"Search-only view of extracted PDF text"*. It normalizes
wrap hyphens and whitespace and keeps `spans` mapping every emitted scalar back
to the raw extraction, so `SourceMap` and highlighting still resolve.

It is well built and it is evidence for this design rather than against it: the
projection is Wilkes stating, in code, that the stored reading is not the
reading it wants to match against. But the projection is reachable only from
literal search. Every other consumer gets the raw text:

| Consumer | Reads | Consequence today |
|---|---|---|
| Literal search | projection | correct |
| Semantic index (`chunk_content` → vectors) | raw | embeds `pro` + `tect` |
| `get_document_text` (MCP + reading pane) | raw | callers, including language models, read hyphen-broken prose interleaved with page furniture |
| Grep context lines | raw | artifacts shown to the user |
| Managed corpus export | raw | the same text reaches Underdog's extraction pipeline |

So one document has two texts, one of which is right, and the right one is the
one almost nobody reads. Per the one-owner rule this repository follows, the fix
is not to widen the projection's reach — that spreads a second owner — but to
make the stored reading correct and delete the compensation.

## 5. The outline is resolved to a page

`resolve_outline` ([`crates/api/src/context.rs`](../../../crates/api/src/context.rs))
maps a declared outline entry onto an exported chunk:

```rust
let ordinal = match (entry.page, entry.byte_offset) {
    (Some(page), _) => chunks.iter()
        .find(|chunk| matches!(chunk.outline_origin(),
            SourceOrigin::PdfPage { page: at, .. } if *at >= page))
        .map(OutlineChunk::outline_ordinal),
    (None, Some(offset)) => /* … first chunk containing the offset … */,
    (None, None) => None,
}?;
```

For a PDF this resolves to **the first chunk of the destination page**, because
`flatten_outline` only ever records `page: Some(_)`, `byte_offset: None`. A
heading halfway down page 97 is reported as beginning at the top of page 97.

A consumer that segments a document by the declared outline therefore places
every boundary up to a full page early, and the text between the top of the page
and the actual heading is attributed to the wrong section. That is not the
consumer's bug to fix: `MANAGED_SEMANTIC_CORPUS` §6.1 puts "extraction, source
maps and **declared outline resolution**" on Wilkes's side of the boundary.
Resolving to a page when the document knows the position is Wilkes
under-delivering on a fact it owns.

There is also a latent ordering bug: the `(Some(page), _)` arm matches first, so
once `byte_offset` is populated the page arm would still win. §6c fixes the
precedence with the population.

## 6. Design

### 6a. A sanitation pass inside extraction

The PDF extractor gains a pass between word extraction and
`ExtractedContent` assembly. It consumes `(text, segments)` and returns a new
`(text, segments)` pair in the sanitized coordinate space — the same shape
`PdfSearchProjection::new` already builds, promoted from a search-time view to
the extraction output.

Every transform is expressed as an edit on the segment list, never as a string
operation on the assembled text, so the map cannot drift from the text.

**Class 1 — line-wrap hyphenation.**

A discretionary hyphen at a visual line end is a candidate join. The existing
`is_discretionary_hyphen` and `line_wrap_continuation` predicates identify
candidates; what is new is that a canonical text must **decide**, where the
projection could defer by matching either form.

The rule is **corpus frequency**, decided 2026-08-22:

> Join the two fragments if the joined form occurs elsewhere in the same
> document as an unhyphenated word; otherwise keep the hyphen.

Two passes over the document: collect the unhyphenated word set, then resolve
each candidate against it. Properties that made this the choice over a lexicon:

- No dictionary dependency, and no language assumption.
- Self-calibrating on domain vocabulary — a book that writes `preshared`
  elsewhere joins `pre-\nshared`; one that writes `pre-shared` does not.
- Deterministic given the document, so the rendition hash stays stable.
- Its failure mode is to leave the hyphen, which is exactly today's behaviour.
  A miss is never a regression.

Word-set comparison is case-insensitive and strips trailing punctuation. A
candidate whose joined form appears only hyphenated elsewhere keeps its hyphen
on that evidence alone.

**Class 2 — page furniture.**

Bare page numbers, running heads and running feet. Detected structurally, not
by content: a short text run whose bbox lies outside the body text block, in a
band that repeats at the same vertical position across a majority of pages.
Removed from the reading; its segments are dropped from the map, which is
sound because those bytes leave the text entirely.

Conservative by construction — the band must repeat across pages, so a one-off
short line in the margin of a single page is left alone. A running head equal to
a section title is still furniture and still goes: it is not part of the prose,
and the outline already carries the title.

**Class 3 — marginalia.**

Glossary boxes and side notes whose bboxes sit outside the body column.
Currently emitted in raster order, so they land mid-sentence. They are **moved,
not deleted**: each block is emitted after the last body block of its page,
preserving its segments and therefore its map entries and highlight boxes.

Relocation rather than deletion because they are real authored content — the
IU coursebooks define half their key terms in the margin. Deleting them would
lose definitions; leaving them in place corrupts every sentence they interrupt.

Body-column detection is per page: cluster word bboxes by x-extent, take the
dominant cluster as the body. A page whose clustering is ambiguous (tables,
two-column layouts, figures) is left in raster order — the honest answer, and
one that must be logged per document rather than silently applied.

### 6b. `byte_offset` for PDF outline entries

`flatten_outline` gains a resolution ladder, applied against the sanitized text:

1. **Destination coordinate.** `LinkDestination.kind` is a `DestinationKind`;
   the `XYZ { top, .. }` and `FitH { top }` variants carry a vertical
   coordinate. `extract_page_words` records a per-word bbox, so the first word
   whose bbox begins at or below `top` gives an exact
   `SourceSegment.text_range.start`.

   **Verify before relying on this.** PDF user space is origin-bottom-left with
   y increasing upward; MuPDF page space is top-left, y down. Whether
   `mupdf-0.6` normalizes a destination coordinate into page space is an
   implementation question to answer with a test against a real bookmarked PDF,
   not an assumption. If it does not, the transform is ours to apply, and the
   test is the same test.

2. **Title match on the destination page.** Match the bookmark title against
   that page's text, normalized as `PdfSearchProjection` normalizes — which is
   what that normalization is for. Earliest match wins. `Fit` and `FitB`
   destinations carry no coordinate and start here.

3. **`None`.** A bookmark whose title does not appear on its page — renumbered,
   restyled, or set as an image — gets no offset and keeps today's page
   resolution. Degrade, do not guess.

Which rung answered is recorded per entry and surfaced per document. A document
resolving mostly by rung 3 is a document whose sections are still page-snapped,
and that must be visible here rather than discovered later as a missing concept
in a consumer.

### 6c. `resolve_outline` prefers position

```rust
let ordinal = match (entry.byte_offset, entry.page) { … }
```

`byte_offset` first, `page` as the fallback. Both are still exported, so a
consumer can see which it got. The `(None, None)` drop is unchanged: an entry
that resolves nowhere is not a section.

### 6d. Retire the projection's hyphen handling

With no wrap hyphens in the reading, `WRAP_HYPHEN`, `is_discretionary_hyphen`,
`line_wrap_continuation` and the `wrap_or_hyphen` alternation have nothing to
match. The projection keeps its whitespace normalization and its span map.

Retire against the existing tests, not by assumption:
`literal_passage_ignores_pdf_line_wrap_hyphenation_and_whitespace`,
`genuine_inline_hyphen_is_not_optional` and
`pasted_wrap_hyphen_query_matches_inline_or_wrapped_hyphen` each encode a
behaviour a user relies on. The third is the interesting one — a user pasting a
hyphenated phrase copied out of a PDF viewer must still match — and it may
justify keeping the alternation on the *query* side after the text side is
clean.

## 7. Identity and migration

`EXTRACTOR_RECIPE_VERSION` (`"wilkes-extractors-v1"`,
[`crates/core/src/embed/identity.rs`](../../../crates/core/src/embed/identity.rs))
bumps to `"wilkes-extractors-v2"`. That changes `ExtractionRecipe::id()`, hence
rendition identity, hence `extracted_content_sha256` — every managed document
re-extracts and re-embeds, and no v1 rendition is silently mixed with a v2 one.
This is the intended forcing function, not a cost to be avoided.

All three sanitation classes and the outline anchors ship under that one bump.
They share a coordinate space: anchoring a heading at a byte offset into text
that a later round would re-flow means re-anchoring everything. Classes may land
in separate commits; they may not land in separate recipe versions.

Legacy (non-managed) indexed files re-index on their normal path.

## 8. Contract surface

The managed corpus contract does not change shape. `OutlineEntry.byte_offset`
already exists in
[`docs/internal/specs/fixtures/managed-semantic-corpus-v1.json`](fixtures/managed-semantic-corpus-v1.json)
and is exported as `null` today; it starts carrying values. Add the per-entry
resolution rung (§6b) as a new field, and the per-document marginalia-clustering
outcome (§6a class 3) to the import response diagnostics.

Consumers that already handle `byte_offset: null` keep working unchanged.
`MANAGED_SEMANTIC_CORPUS` §6.1's wording ("extraction, source maps and declared
outline resolution") already covers all of this: the remit widens, the boundary
does not move.

## 9. Judged by

Wilkes-side, on Wilkes's own consumers:

- **`get_document_text` on a library coursebook returns prose.** No hyphen-broken
  words, no bare page numbers, no glossary box interrupting a sentence. This is
  the reading-quality check and it is done by reading it.
- **Hyphen-broken words: 1,833 → 0** across the three measured books, with **zero
  genuine compounds destroyed** — checked against a held-out list of hyphenated
  terms those books use (`role-based`, `cross-border`, `zero-day`,
  `multi-factor`, `denial-of-service`, `pre-shared`).
- **Bare page-number lines: 765 → 0.**
- **Literal search parity with the hyphen handling removed:** every projection
  test that describes a user-visible behaviour still passes, or is consciously
  replaced (§6d).
- **Semantic search improves or holds** on a fixed query set. Embedding
  `example` rather than `exam` + `ple` should not make retrieval worse; if it
  does, that is a finding worth having before the bump ships.
- **Source map totality:** for every rendition, every byte of the reading
  resolves to a page position, and every retained extraction byte appears in the
  reading exactly once. Property test over the fixture corpus.
- **Outline anchor rung distribution** reported per document. A corpus where
  rung 1 answers most entries is the target; a corpus dominated by rung 3 means
  §6b needs another round.
- **Highlighting still lands** on the right words in the preview after
  relocation — the marginalia case is the one that can break it.

## 10. Rejected alternatives

- **Widen `PdfSearchProjection` to every consumer.** Keeps two texts and makes
  the second one load-bearing everywhere. The projection's own doc comment
  ("Search-only view") is the argument against it.
- **Sanitize in the managed corpus export only.** Gives one rendition two texts,
  breaks `extracted_content_sha256` and the rebuild-the-rendition-from-chunks
  invariant `chunk_content` documents, and serves exactly one consumer.
- **Always join wrapped words.** Destroys genuine compounds silently; the three
  measured books use dozens.
- **Lexicon-based joining.** New dependency, a language assumption, and it fails
  on domain vocabulary in both directions — precisely where these documents live.
- **Delete marginalia.** Loses authored definitions. The IU coursebooks put a
  meaningful share of their key terms in the margin.
- **Infer headings from text styling** where the outline is absent or
  unresolvable. A separate question, and one where guessing is worse than the
  honest "this document declares no outline".
