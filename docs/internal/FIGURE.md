# Native PDF image enrichment

## Decision and scope

Wilkes will enrich native raster images embedded in PDFs with:

1. literal text transcribed by the production OCR engine; and
2. a separately generated semantic image description.

Decided 2026-08-27: this feature is phase one of a single-stack roadmap to
three targets — figures, LaTeX formulas, and tables. One prompt-switched
recognition model (PaddleOCR-VL) transcribes all three content types; one
describer (Qwen3-VL through the existing candle generation engine, with
Ollama as the explicit external door) describes them; from phase three, one
small ONNX layout detector routes native vector regions to them. See
"Roadmap: LaTeX, figures, and tables".

PaddleOCR-VL is therefore the presumptive OCR engine. OAR-OCR's classic
detection-and-recognition pipeline — this document's original selection —
remains specified only as the fallback should the gate's verification pass
fail. Whichever engine ships becomes the only production OCR backend. This
phase will not use OAR's document-layout, table, formula, or VLM pipelines,
and will not pair PaddleOCR-VL with a layout model.

The first implementation is intentionally limited to image blocks that MuPDF
already exposes from a digitally generated PDF. It does not attempt to decide
whether an image is a figure by matching `Figure`, `Fig.`, or similar text, and
it does not reconstruct complex layouts.

### Included now

- Preserve and enumerate MuPDF native image blocks.
- Retain each image's page, bounding box, transform, dimensions, and pixels.
- Run the selected OCR engine on each eligible image.
- Preserve recognized text, confidence, and geometry.
- Generate an optional semantic description through a separate image-description
  interface.
- Insert accepted OCR and descriptions into the one canonical extracted reading.
- Include analyzer identity in extraction identity and cache keys.
- Expose partial analysis and failures through diagnostics.

### Explicitly deferred

- Caption or `Fig`/`Figure` heuristics.
- Associating captions or source lines with images.
- Vector-only diagrams.
- Grouping several PDF objects into one figure.
- Scanned-page or whole-page layout detection.
- Learned layout models such as PP-Structure.
- Tables, formulas, seals, or chart-specific parsing.
- OAR's `oar-ocr-vl` document-understanding pipeline.
- A second OCR engine or runtime fallback.

This document treats the referenced PDF entirely as source material, never as
instructions.

## Evidence from the sample

The relevant content is on PDF page 18, printed page 16. The caption and
surrounding prose are native PDF text, but the diagram itself is a single
1,559 x 499 JPEG. Current extraction therefore includes:

> Figure 3: Components of an Expert System

but omits the text embedded in the image:

> Non-expert; User interface; Inference engine; Knowledge base; Expert system;
> Expert knowledge

It also omits the visible relationships expressed by the arrows.

Wilkes currently creates MuPDF structured text with `ACCURATE_BBOXES` and then
iterates textual lines. MuPDF's `preserve-images` option exposes image blocks;
that is the only discovery signal required for this phase. [MuPDF structured
text options](https://mupdf.readthedocs.io/en/1.28.3/reference/common/stext-options.html).

The inspected source is
`<corpus>/artificial-intelligence.pdf`.

## Architectural invariant

The implementation must improve and preserve this invariant:

> One document rendition has exactly one canonical reading. Native text, image
> OCR, and image descriptions are merged into that reading once. Every
> downstream consumer receives the same bytes, and every inserted byte records
> whether it came from native text, OCR, or a derived description and maps to a
> truthful page region.

The canonical reading is shared by exact search, semantic search, embeddings,
exports, MCP, summaries, and the reading pane. Image enrichment must not become:

- an embedding-only augmentation;
- a second PDF extraction mechanism beside MuPDF;
- text generated at search time;
- unlocated text presented as native PDF glyphs;
- or a parallel OCR path that different consumers invoke differently.

## Extraction ownership

Transcription engines are replaceable; the extraction contract is not. Wilkes
keeps owning the canonical reading, byte-level source maps, page and polygon
locators, versioned extraction identity, and caching — exact-search
highlighting, byte-for-byte chunk reconstruction, the embedding-space
membership digest, and MCP provenance are all built on them. Wholesale
offloading of PDF parsing (Docling, Marker, MinerU, remote OCR APIs) was
considered and rejected: those tools emit markdown with at best block-level
geometry, ride Python stacks or remote services a local-first Tauri app
should not carry, and for born-digital PDFs would replace ground-truth glyphs
with model transcription — a fidelity regression precisely where LLM
consumers are most sensitive.

The existing sanitize pass already handles column ordering, running-head and
page-number removal, and wrap-hyphen joining deterministically. The remaining
fidelity gaps for LLM consumers are, in order of impact: figures (this
document), native vector tables, formulas, and scanned PDFs. Learned engines
address those inside the owned contract, never in place of it.

## Pipeline

```text
PDF page
   |
   +-- MuPDF native text and word boxes
   |
   +-- MuPDF native image blocks
            |
            +-- OCR engine (presumptively PaddleOCR-VL spotting)
            |      `-- text + image-relative polygons + admission signal
            |
            `-- FigureDescriber
                   `-- semantic description of visible content/relationships
                              |
                              v
                    versioned ImageAnnotation
                              |
                              v
                 canonical ExtractedContent.text
                              |
                    image-aware chunk boundaries
                              |
                              v
                     existing passage embedder
```

OCR and description remain separate facts. OAR transcribes visible text; it
does not provide the semantic description in this design.

## Native image discovery

Enable MuPDF image preservation alongside `ACCURATE_BBOXES`. For every native
image block, retain:

```rust
ExtractedImage {
    id,
    page,
    bbox,
    transform,
    pixel_width,
    pixel_height,
    image_sha256,
    reading_anchor,
}
```

`reading_anchor` records the image block's position relative to the native text
blocks on the page. It controls where enrichment is serialized into canonical
text. It is not inferred from a nearby caption.

The first phase should enumerate image blocks mechanically. Fixed technical
limits are still necessary to prevent pathological work, for example maximum
decoded pixels, maximum crop bytes, and rejection of zero-sized images. Any
minimum-size rule must be versioned, covered by tests, and reported in
diagnostics; it must not quietly act as a semantic figure classifier.

Repeated logos, decorative images, and other non-figures may therefore enter
analysis in this phase. OAR should normally return no accepted text for them,
and the description policy can decline to serialize unhelpful results. Semantic
classification and repeated-artifact suppression are deferred rather than
hidden inside undocumented heuristics.

## OAR-OCR integration

Presumption update 2026-08-27: this section survives as the conditional
fallback specification — see the gate in the OCR decision record. The
admission and normalization rules below bind whichever engine is selected;
only the pipeline and packaging details are OAR-specific.

### Selected pipeline

Use `OAROCRBuilder` with OAR's classic ONNX text detector and recognizer. Do not
instantiate `OARStructureBuilder`, a page-layout pipeline, or `oar-ocr-vl`.

The initial quality baseline should be PP-OCRv6 small on CPU. Before pinning the
final extraction recipe, compare PP-OCRv6 tiny and small on the image evaluation
corpus. The shipped model is the better measured model, not a runtime fallback:
Wilkes must package and identify one selected detector/recognizer pair per
recipe.

OAR supports model paths and in-memory model bytes, batch prediction, and
structured text regions. Its current result preserves recognized text,
confidence, detection/recognition polygons, optional word boxes, and orientation.
Coordinates are mapped back to the original input image when orientation
correction is used. [OAR usage guide](https://github.com/GreatV/oar-ocr/blob/main/docs/usage.md)
and [OAR result type](https://github.com/GreatV/oar-ocr/blob/main/src/oarocr/result.rs).

Normalize OAR output into Wilkes-owned types so OAR types do not leak across
the extraction boundary:

```rust
ImageOcrRegion {
    text: String,
    confidence: f32,
    polygon_within_image: Vec<Point>,
    page_polygon: Vec<Point>,
    word_boxes_within_image: Option<Vec<Polygon>>,
}
```

The image transform maps each OAR polygon from input pixels into MuPDF page
coordinates. Precise page polygons allow exact search to highlight
`Knowledge base` rather than the entire page or image.

### Admission and normalization

- Preserve OAR's reading order and region grouping.
- Normalize whitespace without losing punctuation, percentages, units, or
  Unicode characters.
- Keep raw confidence in structured metadata.
- Apply one explicit, tested confidence threshold before OCR text enters the
  canonical reading.
- Record rejected regions in diagnostics rather than silently discarding them.
- Deduplicate OCR against native text geometrically located inside the same
  image bounds. Some PDFs draw native labels over an image.
- Run OCR whether or not an image-description model is configured.
- Never substitute a second OCR engine after an OAR error. An OAR failure is a
  visible partial result, not a trigger for a duplicated mechanism.

### Runtime and packaging requirements

The OAR selection requires a coordinated dependency migration, not merely a
new Cargo dependency:

- Current OAR upstream requires Rust 1.95, while Wilkes's Docker build uses
  Rust 1.88 in [Dockerfile](Dockerfile:16).
- Wilkes currently pins `ort = 2.0.0-rc.11` in
  [crates/core/Cargo.toml](crates/core/Cargo.toml:51).
- OAR uses a newer `ort`/`ort-sys`. All FastEmbed and OAR dependencies in one
  binary must converge on a single compatible `ort-sys` version.
- Linux, Windows, macOS, Core ML, and the existing MuPDF/Windows CRT build
  constraint must be verified after that convergence.

Verified 2026-08-27: the conflict is exact, not merely awkward. fastembed
5.13.0 pins `ort = "=2.0.0-rc.11"` and oar-ocr-core 0.9.2 pins
`ort = "=2.0.0-rc.13"` — two `=` pins on one crate, so a joint build cannot
resolve today. The convergence path is fastembed 6.0.x (which pins
`=2.0.0-rc.13`) plus the Rust 1.88 → 1.95 toolchain bump: a major fastembed
upgrade whose embedding behavior must be re-verified. This entire migration
exists only on the OAR path; selecting PaddleOCR-VL deletes it.

OAR's default feature can download ONNX Runtime binaries during the build, and
its optional `auto-download` feature can fetch OCR models at runtime. Wilkes
should retain ownership of model installation and extraction identity instead:

- Pin exact detector, recognizer, and dictionary artifacts.
- Verify every artifact by size and SHA-256.
- Make offline operation possible after explicit installation.
- Do not let an unversioned or implicit runtime download change extraction.
- Include OAR crate version, model digests, preprocessing settings, and
  thresholds in the extraction recipe.

OAR is Apache-2.0 licensed. The redistributed model artifacts and dictionaries
must also receive a model-specific license/provenance inventory before they are
packaged. [OAR repository](https://github.com/GreatV/oar-ocr) and [OAR model
registry](https://github.com/GreatV/oar-ocr/blob/main/docs/models.md).

## Image descriptions

The description backend is selected: Qwen3-VL Instruct through the existing
candle generation engine — 2B as the default recipe, 4B as an opt-in recipe
with better document fidelity. Each recipe is a distinct extraction identity,
so switching forces re-extraction. The selection rests on verified facts:

- `candle-transformers` 0.11, the exact version Wilkes pins, ships a
  `qwen3_vl` module, so the describer needs no toolchain or dependency
  migration at all.
- The candle engine already owns the Qwen3 protocol (framing, stop tokens,
  think-preamble stripping); the VL variant extends a family Wilkes ships
  rather than adding a third one.
- All Qwen3-VL sizes are Apache-2.0, which keeps the license/provenance
  inventory clean.
- The same family scales through the existing Ollama engine (2B–235B) with
  identical prompting, so one describer prompt and schema serve both the
  first-class and the external path. External models remain whatever the
  user pulls into Ollama; Wilkes does not manage those.

Rejected for first-class support: Gemma 4 (license-inventory friction, dead
audio modality; a reasonable second family later), Moondream (weak
relationship extraction on dense labeled diagrams), SmolVLM2, LFM2-VL,
InternVL, and MiniCPM-V (no candle support — a port or a new runtime would
be a second mechanism; they remain reachable through Ollama), and
PaddleOCR-VL (a narrow parsing model that cannot produce free-form
relationship descriptions; its candidacy is as the OCR engine, not the
describer).

Verification item before pinning the recipe: whether candle's `qwen3_vl` has
a quantized-GGUF path or is dense-safetensors only. The engine supports both
loading styles, but the answer decides the 2B recipe's footprint (~2 GB vs
~4 GB on disk).

A describer receives:

- the native image pixels;
- accepted OCR regions and their coordinates;
- a fixed instruction to report only visible content and relationships;
- a fixed, versioned response schema.

It does not receive a caption in this phase because caption association is
deferred.

```rust
trait FigureDescriber {
    async fn describe(
        &self,
        image: &RgbImage,
        ocr: &[ImageOcrRegion],
    ) -> Result<ImageDescription>;
}
```

Example structured output for the sample:

```json
{
  "description": "A non-expert communicates through a user interface with an inference engine, which consults a knowledge base populated by expert knowledge.",
  "relationships": [
    {
      "source": "Expert knowledge",
      "relation": "feeds",
      "target": "Knowledge base"
    },
    {
      "source": "User interface",
      "relation": "communicates bidirectionally with",
      "target": "Inference engine"
    }
  ]
}
```

The relationships support validation and future features. Canonical text only
needs a compact natural-language description plus the independently obtained
OCR transcription.

Local analysis remains the default requirement. Any remote describer must be
explicitly configured and visibly disclose that document imagery leaves the
machine. OAR's separate VLM crate is not an implicit description fallback.

## Canonical representation and provenance

Because caption association is out of scope, enrichment is inserted at the
native image block's reading anchor rather than after text that resembles a
caption. For the sample, the canonical reading should contain a block like:

```text
Image embedded text: Non-expert; User interface; Inference engine;
Knowledge base; Expert system; Expert knowledge.

Image description: A non-expert communicates through a user interface with an
inference engine, which consults a knowledge base populated by expert knowledge.
```

The existing native caption and source line remain in their author-provided
positions. The implementation does not move, reinterpret, or duplicate them.

The labels `Image embedded text:` and `Image description:` are deliberate:

- exact-search users can distinguish OCR from authored prose;
- language models do not mistake a description for a quotation;
- exports remain interpretable without private metadata;
- generated claims are not presented as literal source text.

Add structured metadata beside canonical text:

```rust
ExtractedImage {
    id,
    page,
    bbox,
    reading_range,
    image_sha256,
    ocr_regions,
    description,
    analyzer_identity,
}

enum TextProvenance {
    Native,
    ImageOcr {
        image_id: String,
        confidence: f32,
    },
    ImageDescription {
        image_id: String,
        analyzer_id: String,
    },
}
```

`ExtractedContent` owns `images: Vec<ExtractedImage>`. OCR source segments map
to precise page polygons; descriptions map to the complete native image bounds.
Every inserted byte therefore has both a page locator and truthful provenance.

## Chunking

Appending enrichment to canonical text automatically makes the existing
embedder see it. The chunker must nevertheless treat an image-enrichment block
as a structural unit:

- Prefer a boundary immediately before the image block.
- Keep OCR and description together when they fit.
- Prefer a boundary immediately after the block.
- If a large block exceeds the configured size, split at OCR-region or
  paragraph boundaries while retaining the same image locator.
- Do not overlap unrelated body prose across the image boundary.
- Continue satisfying the existing byte-for-byte reconstruction invariant in
  [chunk.rs](crates/core/src/embed/index/chunk.rs:21).

## Determinism, caching, and identity

Image analysis is versioned extraction, not live search-time generation. Cache
annotation JSON by:

```text
source PDF SHA-256
+ page
+ normalized image bbox and transform
+ decoded image pixel SHA-256
+ OCR engine crate/pipeline version
+ detector SHA-256
+ recognizer SHA-256
+ dictionary SHA-256
+ OCR preprocessing and admission settings
+ description model revision
+ description prompt/schema version
+ canonical serialization version
```

Do not cache a second copy of image pixels unless the source format requires it.
Changing any OCR model, description model, prompt, threshold, coordinate
mapping, or serialization changes the extraction recipe and forces
re-extraction and re-embedding.

The application must use one configured extractor registry for indexing,
watcher updates, exact-search fallback, MCP reads, summaries, and export.
Otherwise analyzer configuration and caches can drift between consumers and
violate the canonical-reading invariant.

An OCR or description failure may leave native text and successful stages
available, but the result is explicitly partial. It must never be recorded as
a fully enriched empty annotation.

## Diagnostics

Extend extraction diagnostics with observable image-analysis counts:

```text
native_images_found
native_images_analyzed
native_images_skipped_technical_limit
images_ocr_succeeded
images_ocr_failed
ocr_regions_accepted
ocr_regions_rejected_low_confidence
ocr_regions_deduplicated_against_native_text
images_description_succeeded
images_description_failed
images_description_not_configured
```

Managed corpus export should expose the same diagnostics. For example,
"twenty images found, nineteen OCR successes, one decoder failure, descriptions
not configured" is actionable; silently missing an image is not.

## OCR decision record

OAR-OCR was selected over `ocrs` and Tesseract because it provides the stronger
production result contract and model ecosystem:

- per-region recognition confidence;
- detection and recognition polygons in original-image coordinates;
- optional word boxes and orientation handling;
- batch prediction;
- multiple PP-OCR detector and recognizer sizes;
- broad multilingual model coverage;
- CPU and platform accelerator options.

`ocrs` is easier to compile because its RTen runtime is Rust-native, but its
standard model is Latin-only, its project describes the engine as early
preview, and its public recognized-character type does not expose recognition
confidence. Tesseract would introduce a native system/package dependency and a
second OCR mechanism. Neither remains as a production fallback. This preserves
one owner for OCR behavior and one extraction recipe.

The model checkpoint is not yet selected merely because the engine is. The
PP-OCRv6 tiny-versus-small evaluation and artifact-license inventory remain
required before implementation can claim a pinned, shippable OCR recipe.

### PaddleOCR-VL candidacy (added 2026-08-27)

The record above compared OAR only against `ocrs` and Tesseract, and rejected
OAR's own VLM pipeline. It never evaluated PaddleOCR-VL as the transcription
engine, and that gap matters because it is the only candidate that deletes
the dependency migration:

- `candle-transformers` 0.11 — already pinned — ships a `paddleocr_vl`
  module implementing the 0.9B recognizer (NaViT encoder + ERNIE-4.5-0.3B
  decoder). Models flow through the existing hf-hub lifecycle: no new
  dependencies, no second ONNX runtime, no toolchain bump.
- PaddleOCR-VL 1.5/1.6 added an end-to-end text-spotting task: the model
  itself emits per-region 4-point quadrilaterals as `<|LOC_nnn|>` tokens on
  a normalized 0–999 grid. Within this feature's scope MuPDF already plays
  the layout-detector role (the native image block is the crop), so spotting
  quads map through the image transform to page polygons and the polygon
  acceptance criterion is satisfiable. On a 1,559-px-wide figure the grid
  quantizes to ~1.6 px.
- Apache-2.0, and inherently multilingual — no per-language recognizer and
  dictionary packaging.
- If it replaces OAR entirely, the one-OCR-owner rule is preserved. Using it
  beside OAR would violate that rule and is not on the table.

What OAR still holds:

- Calibrated per-region recognition confidence. The VLM offers only token
  logprobs — usable as an admission signal, but uncalibrated and untested.
- Region-local failure. Autoregressive transcription can drop or repeat
  regions; detection-plus-recognition fails gracefully. Baidu's own v1 paper
  makes this argument, though for full pages; 1.5/1.6 were post-trained to
  harden spotting, and small figure crops are the easy end of that spectrum.
- Footprint and speed: ~15 MiB and tens of milliseconds per image versus
  ~1 GB and seconds per image, on every native image block including logos.
  Cached and offline, so bounded — but real at indexing scale.

Candle's module is the recognizer only; PP-DocLayoutV2 is not in candle and
is not needed for this scope. Wilkes would own the spotting-task prompt and
`<|LOC|>` parsing. Whether the candle module drives the 1.5/1.6 checkpoints
and task prompts is an open verification item.
[PaddleOCR-VL v1 paper](https://arxiv.org/pdf/2510.14528),
[1.5 paper](https://arxiv.org/pdf/2601.21957),
[1.6 paper](https://arxiv.org/pdf/2606.03264), and
[candle paddleocr_vl module](https://docs.rs/candle-transformers/0.11.0/candle_transformers/models/paddleocr_vl/index.html).

**Gate.** The roadmap below commits to LaTeX and tables as goals, which
decides the presumption on paper: OAR's route to the same goals adds
PP-FormulaNet, table models, and the dependency migration to reach what one
prompt-switched model provides. The gate therefore reduces from a bake-off
to a verification pass — the candle module drives the 1.5/1.6 checkpoints
and task prompts, `<|LOC|>` output parses to usable quads, and character
error, coordinate accuracy, admission-rule viability, and CPU latency hold
on the planned corpus. It still precedes implementation plan step 1 (whose
migration is only needed if the verification fails and OAR returns), and it
is still not scheduled; the gate records order, not timing.

### If LaTeX becomes mandatory

Formulas sit in the deferred list, so LaTeX support is a scope change, not a
parameter. The roadmap below exercises this scope change in its second and
third phases; the analysis that led there:

- The presumption flips to PaddleOCR-VL. Its formula recognition is the same
  pinned 0.9B weights behind a `Formula Recognition:` task prompt — no new
  artifacts, licenses, or memory. OAR would need PP-FormulaNet (221–700 MiB)
  or UniMERNet (1.7 GiB) plus tokenizer, a larger license inventory, and
  still the full dependency migration; the classic pipeline's footprint and
  simplicity arguments evaporate.
- OAR's confidence edge does not extend to formulas: PP-FormulaNet's output
  is autoregressive LaTeX too, so formula admission is validity-based on
  either engine (does the LaTeX parse and render), not confidence-based.
- The decisive evaluation question becomes routing: something must decide a
  region is a formula before invoking formula recognition, and neither
  candle nor this scope has a layout model. Options to compare: a small
  classifier stage, running both the OCR and Formula prompts under one
  explicit tested admission rule (~2x compute), or verifying whether 1.6's
  unified parsing self-classifies figure crops.
- Required scope amendments: an `Image embedded formula:` serialization
  label, formula provenance, chunk boundaries that never split inside a
  formula, and formula metrics (ExpRate/CDM-style plus a LaTeX-validity
  admission check) in the evaluation.
- LaTeX chiefly serves exact search, export, and reading fidelity. Text
  embedders handle raw LaTeX poorly, so semantic retrieval of formula
  content still rides on the describer's natural-language rendering.

## Roadmap: LaTeX, figures, and tables

Decided 2026-08-27: the path to all three targets is one stack, sequenced so
each phase reuses the previous one's machinery. Two models Wilkes can
already run — PaddleOCR-VL for recognition of every content type,
prompt-switched, and Qwen3-VL for description — plus one small ONNX layout
detector for routing. No OAR, no toolchain migration, no fastembed upgrade.
The phases are subsequent explicit features; they do not retroactively
expand this document's deferred list.

### Phase 1 — Raster figures

This document, with PaddleOCR-VL as the engine: MuPDF native image blocks,
the spotting task for text and page polygons, the Qwen3-VL describer, and
canonical labeled blocks.

### Phase 2 — Formulas and tables inside figures

Same crops, same weights, different task prompts: `Formula Recognition:`
yields LaTeX, `Table Recognition:` yields a structured table. The new design
work is the routing rule for what a crop contains — options recorded under
"If LaTeX becomes mandatory" — plus that section's serialization,
provenance, and chunking amendments. This picks up equations embedded as
images nearly for free.

### Phase 3 — Native vector tables and formulas

Born-digital PDFs draw most tables and math as vector content, invisible to
the image-block scope. The blocker is region detection, and the routing
problem that recurs through this document has one answer Wilkes can hold
without the OAR migration: PP-DocLayoutV2-class layout detectors
(RT-DETR/PicoDet) are plain ONNX detection graphs — OAR's registry hosts
ONNX conversions — and Wilkes already carries `ort = "=2.0.0-rc.11"` for
FastEmbed. Wilkes uses the model artifacts, not the OAR crate, and owns the
pre- and post-processing. Then: detected region, MuPDF renders the crop (the
binding has pixmap support), the same PaddleOCR-VL task prompts, and a
labeled block at the region's reading anchor.

### Decisions taken now

- **Tables serialize as Markdown.** One canonical table format, chosen once
  and versioned in the extraction recipe. OTSL is compact but hostile to
  embedders and human readers; a Markdown table serves exact search, LLM
  consumers, and embeddings from the same bytes.
- **Recognized regions suppress the native glyphs they displace.** Phase 3
  is the first time recognized content replaces native text — the garbled
  glyph runs inside a table or formula region — rather than standing beside
  it. This is a deliberate extension of the geometric dedup rule already
  specified for native-text-over-image, not a new invariant, with one
  addition: every suppressed byte has a recorded reason in diagnostics, the
  counterpart of every inserted byte carrying truthful provenance.

### Not on the path

- The OAR route to the same goals: classic OCR plus PP-FormulaNet plus table
  models plus a layout pipeline plus the dependency migration — three
  additional model families for what one prompt-switched model provides.
- Whole-page VLM parsing: it replaces ground-truth glyphs with model
  transcription at seconds per page across a library, and Baidu's own papers
  argue against end-to-end layout.

### Verification items

- Phase 1: the candle `paddleocr_vl` module drives the 1.5/1.6 checkpoints
  and task prompts, and `<|LOC|>` output parses to usable quads.
- Phase 3: the chosen layout detector's ONNX opset runs on the pinned
  `ort = "=2.0.0-rc.11"`.

Nothing here is scheduled; the roadmap records order and decisions, not
timing.

## Proposed implementation plan

This is a full extraction feature even though image discovery is deliberately
narrow.

0. **Decide the OCR engine**
   - Run the gate recorded in the OCR decision record. Every OAR-specific
     step below is conditional on its outcome.

1. **Converge the build runtime** *(OAR path only — deleted if PaddleOCR-VL
   is selected)*
   - Upgrade the Rust toolchain to 1.95.
   - Upgrade fastembed to 6.0.x so FastEmbed and OAR converge on
     `ort = "=2.0.0-rc.13"`; re-verify embedding behavior.
   - Verify all supported platform builds.

2. **Add structured image types and provenance**
   - Add `ExtractedImage`, `ImageOcrRegion`, image ranges, and analyzer identity.
   - Add native/OCR/description provenance to source-map segments.
   - Extend diagnostics without changing the existing PDF-page locator contract.

3. **Preserve native image blocks**
   - Enable MuPDF image preservation.
   - Retain block position, transform, pixels, and reading anchor.
   - Apply only explicit technical safety limits.
   - Do not add caption, vector grouping, or layout inference.

4. **Integrate the selected OCR engine** (presumptively PaddleOCR-VL
   spotting; OAR classic only if the verification pass fails)
   - Add one Wilkes-owned adapter; OAR types or `<|LOC|>` parsing stay
     behind the extraction boundary either way.
   - Batch native images from a PDF.
   - Map image-relative polygons to page coordinates.
   - Apply the admission rule (confidence threshold for OAR; an explicit
     tested logprob or validity rule for the VLM) and native-text
     deduplication.
   - Install models through the Wilkes model lifecycle.

5. **Add the independent description interface**
   - Wire the Qwen3-VL describer through the candle engine, with Ollama as
     the configured external door.
   - Pass image pixels and accepted OCR to the describer.
   - Validate a fixed response schema.
   - Report unconfigured and failed descriptions as partial analysis.

6. **Merge annotations into canonical text**
   - Insert at the image block's native reading anchor.
   - Serialize explicit `Image embedded text:` and `Image description:` blocks.
   - Ensure the original text is neither moved nor duplicated.

7. **Add durable caching and extraction identity**
   - Key annotations by source, image pixels, and all analyzer recipe inputs.
   - Consolidate production extraction call sites on one configured registry.
   - Force re-extraction and re-embedding when any recipe input changes.

8. **Make chunking image-aware**
   - Preserve image-enrichment blocks as structural units when possible.
   - Preserve exact canonical-text reconstruction and valid byte ranges.

9. **Verify with a bounded corpus**
   - The supplied expert-system raster diagram.
   - Clean and low-resolution raster diagrams.
   - Colored and dark backgrounds.
   - Rotated labels.
   - Native text over an image for deduplication.
   - Images with no text.
   - Repeated logos, recorded without adding semantic suppression.
   - Low-confidence, decode-failure, OCR-failure, and description-failure cases.
   - Unicode OCR to enforce character-safe string handling.

The evaluation must measure OCR character/word error, missed and false regions,
reading order, coordinate accuracy, CPU latency, peak memory, model footprint,
and supported-platform packaging. It selects one PP-OCRv6 model pair for the
shipped recipe; it does not create a runtime choice between engines.

## Acceptance criteria

The scoped feature is complete only when:

- MuPDF discovers native images without a `Fig`/caption heuristic.
- `get_document_text` includes accepted image OCR and configured descriptions
  exactly once at the image's reading anchor.
- Exact search for `Expert knowledge` finds the sample image and resolves to
  its OCR polygon.
- Semantic search for "Where does expert knowledge enter the system?" retrieves
  the image-enrichment passage when a description is configured.
- The existing embedder receives enriched canonical chunks without a second
  embedding path.
- OCR and description stay together in a self-contained chunk at normal
  settings.
- All chunks reconstruct canonical extracted text byte for byte.
- Every inserted byte has a page locator and explicit provenance.
- Model, prompt, threshold, mapping, or serialization changes alter extraction
  identity and force reindexing.
- Analyzer failures and unconfigured descriptions are visible as partial
  results.
- If OAR is selected, FastEmbed and OAR use one compatible ONNX Runtime
  dependency (fastembed 6.0.x, `ort = "=2.0.0-rc.13"`).
- Exactly one engine — the gate's winner — owns OCR behavior; no `ocrs`,
  Tesseract, second engine, or runtime fallback is retained.
- No caption association, vector reconstruction, scanned-page layout inference,
  or OAR VLM pipeline has entered this phase implicitly.

The selected first implementation is therefore **MuPDF native image blocks +
one gated OCR engine (OAR classic or PaddleOCR-VL spotting) + a Qwen3-VL
describer through the candle engine + canonical-text integration**. More
ambitious figure detection remains a later, explicit feature rather than an
accidental expansion of this one.
