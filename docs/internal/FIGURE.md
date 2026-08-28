# Native PDF image enrichment

## Decision and scope

Wilkes will enrich native raster images embedded in PDFs with:

1. literal text transcribed by the production OCR engine; and
2. a separately generated semantic image description.

Decided 2026-08-27: this feature is phase one of a single-stack roadmap to
three targets — figures, LaTeX formulas, and tables — the remaining
extraction-fidelity gaps for the LLM integrations that read the library, and
for every other consumer of the canonical reading. One prompt-switched
recognition model (PaddleOCR-VL) transcribes all three content types; one
describer (Qwen3-VL through the existing candle generation engine, with
Ollama as the explicit external door) describes them; from phase three, one
small ONNX layout detector routes native vector regions to them. See
"Roadmap: LaTeX, figures, and tables".

PaddleOCR-VL is the selected OCR engine and the only production OCR backend.
OAR-OCR — this document's original selection — is discarded: the OCR
decision record retains why it was chosen and why it was superseded, and
nothing of it remains specified for implementation. If the verification
items in the implementation plan fail, the decision reopens with that
record as context; no fallback engine waits in the wings. This phase uses
only the spotting task and pairs no layout model with it.

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
- Tables, formulas, seals, or chart-specific parsing (tables and formulas
  are roadmap phases 2–3, deferred here — not abandoned).
- Whole-page VLM document-understanding pipelines.
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
            +-- PaddleOCR-VL spotting task
            |      `-- text + image-relative polygons + admission signal
            |
            `-- FigureDescriber
                   `-- prose description of what the figure shows
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

OCR and description remain separate facts. The spotting task transcribes
visible text; it does not provide the semantic description in this design.

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
analysis in this phase. The recognizer should normally return no accepted
text for them,
and the description policy can decline to serialize unhelpful results. Semantic
classification and repeated-artifact suppression are deferred rather than
hidden inside undocumented heuristics.

## OCR integration (PaddleOCR-VL)

### Selected pipeline

Use the `paddleocr_vl` module in the already-pinned `candle-transformers`
0.11: the 0.9B recognizer (NaViT encoder + ERNIE-4.5-0.3B decoder) driving
the end-to-end text-spotting task. No layout model, no OAR crate, no second
ONNX runtime. Wilkes owns the task prompt and the parsing of `<|LOC_nnn|>`
tokens into per-region 4-point quadrilaterals on the normalized 0–999 grid.

Before pinning the final extraction recipe, compare the 1.5 and 1.6
checkpoints on the image evaluation corpus. The shipped checkpoint is the
better measured one, not a runtime choice: Wilkes packages and identifies
one recognizer checkpoint per recipe.

Normalize model output into Wilkes-owned types so engine types and token
formats do not leak across the extraction boundary:

```rust
ImageOcrRegion {
    text: String,
    /// Admission signal derived from token log-probabilities. Explicitly
    /// uncalibrated; thresholded by one tested rule.
    confidence: f32,
    polygon_within_image: Vec<Point>,
    page_polygon: Vec<Point>,
}
```

The image transform maps each spotted polygon — denormalized from the 0–999
grid to input pixels — into MuPDF page coordinates. Precise page polygons
allow exact search to highlight `Knowledge base` rather than the entire page
or image.

### Admission and normalization

- Preserve the model's emission order and region grouping.
- Normalize whitespace without losing punctuation, percentages, units, or
  Unicode characters.
- Keep the raw admission signal in structured metadata.
- Apply one explicit, tested admission threshold before OCR text enters the
  canonical reading.
- Record rejected regions in diagnostics rather than silently discarding them.
- Deduplicate OCR against native text geometrically located inside the same
  image bounds. Some PDFs draw native labels over an image.
- Run OCR whether or not an image-description model is configured.
- Never substitute a second OCR engine after an engine error. A recognition
  failure is a visible partial result, not a trigger for a duplicated
  mechanism.

### Measured, 2026-08-28

Implementation-plan step 1, run. The harness is
`extract::image::paddleocr_vl::evaluate`, the corpus is
`extract::image::corpus::accuracy_corpus`, and both are in the tree; the run
is an ignored test because it needs 1.9 GB of weights per checkpoint.

**The engine works.** Both pinned checkpoints download, verify against their
pinned size and SHA-256, and build in `candle-transformers` 0.11 (about 2.7
seconds to load). `<|LOC|>` output parses to usable quads. The location
vocabulary is contiguous and nothing else shares its id range, which the
parser assumed and which is now checked rather than assumed. The chat framing
matches the checkpoint's own template token for token, and the spotting
task's preprocessing — the 1500-pixel upscale, the 2048-patch envelope, the
Lanczos-then-bicubic resample — matches the model card. The engine decision
does not reopen.

**Accuracy.** On five of eight figures, both checkpoints transcribed every
region exactly, character error 0.000: a clean diagram, the same one at
120x180 where a 12-point label is seven pixels tall, a coloured background,
an inverted dark background, and non-ASCII labels. The figure with no text in
it produced no regions at all — no false positives to admit. Coordinate
accuracy is 0.011 of the image on average and 0.025 at worst, comfortably
inside a label's own footprint.

**1.6 is the shipped checkpoint**, on measurement rather than on being later:
character error 0.182 against 0.196 overall, 0.543 against 0.571 on turned
labels, 0.602 against 0.663 on the sample document's figure, with coordinate
accuracy indistinguishable. The two are *not* distinguished on speed; the
wall-clock numbers differ but the runs were not made on an equally idle
machine, and same architecture at same parameter count gives no reason for
them to.

**Turned labels are a real weakness.** Both checkpoints garble text rotated a
quarter turn — `User interface` came back as `User intiaac expert` — reading
one of five. Recorded, not worked around: this is a property of the weights.

**CPU latency is the finding that costs.** 51 seconds for a 120x180 figure,
around 130 for 240x360, 237 for the sample document's 1559x499 diagram, and
roughly four times that again for a figure large enough to fill the spotting
envelope. The shipped build enables neither `candle-metal` nor
`candle-accelerate`, so this is plain f32 CPU matmul and is what a user gets.
A library of a few hundred figures is an overnight job. Nothing about the
extraction design depends on this — the annotation cache means it is paid
once per image per recipe — but it is what the settings surface has to say
out loud, and it is the strongest argument for the Metal path this repository
does not yet build.

### The acceptance criterion that fails (found 2026-08-28)

> Exact search for `Expert knowledge` finds the sample image and resolves to
> its OCR polygon.

It does not, on the sample. The reason is not recognition — the
transcription of that figure is character-perfect — and it is worth stating
precisely because the numbers above hide it.

Every label in the sample diagram is *set on two lines* inside its own shape:
`User` above `interface`, `Non-` above `expert`, `Expert` above `knowledge`.
The spotting task emits one region per drawn line, correctly. The serializer
joins regions with `; `, so the reading contains `User; interface` — a
semicolon the figure does not draw — and an exact search for the label as a
person reads it finds nothing.

Joining them is not a neighbour rule. The recognizer reads this figure in row
order, across the three circles before down them, so `User` and `interface`
are not adjacent in emission: `Inference` and `Knowledge` come between. It
would take a geometric rule over the whole region set — vertical adjacency
and horizontal overlap, plus the wrap-hyphen join the native-text sanitizer
already does for `Non-` / `expert`. That is layout analysis over the image,
which this phase deliberately does not do.

So this is left open rather than patched, and pinned by
`a_label_split_across_two_drawn_lines_is_two_regions_in_the_reading` so it
cannot regress into being forgotten. It is the one acceptance criterion this
work does not meet, and deciding it is a scope decision, not an
implementation detail.

### Runtime and packaging requirements

No dependency migration is required: `candle-transformers` 0.11 and the
existing hf-hub model lifecycle are already in the tree, and the Rust
toolchain, fastembed, and `ort` pins are untouched. (The migration OAR would
have demanded is recorded in the OCR decision record.)

Wilkes retains ownership of model installation and extraction identity:

- Pin the exact recognizer checkpoint by revision.
- Verify every artifact by size and SHA-256.
- Make offline operation possible after explicit installation.
- Do not let an unversioned or implicit runtime download change extraction.
- Include the candle-transformers version, checkpoint digests, task prompts,
  preprocessing settings, and admission thresholds in the extraction recipe.

PaddleOCR-VL is Apache-2.0 licensed. The redistributed checkpoint must still
receive a model-specific license/provenance inventory before it is packaged.

### How it is turned on (added 2026-08-28)

Enrichment is off by default and is one process-wide analyzer, built from
`settings.image_analysis` and installed for every consumer at once. That is
the same invariant the registry consolidation exists for, one level up: a
per-call-site analyzer is what would let indexing enrich a document and an
MCP read not, and then write both answers into one index under recipes that
disagree.

- `enabled` installs the recognizer and turns transcription on.
- `device` is the recognizer's, defaulting to the engine's own choice.
- `describer_model` names an Ollama tag, or is empty for transcription only.
  The server is `generation.ollama_url`: there is one Ollama endpoint per
  app, and a second field for the same server would be a second answer to
  where it is.

Three consequences are deliberate and are what the settings surface says out
loud:

- Enabled but not installed is an **error**, not a quiet disable. A reading
  that silently omitted the enrichment would be indistinguishable from one
  that found no text in the picture.
- A failed load **detaches** rather than leaving the previous analyzer
  attached. The settings no longer describe it, and continuing to enrich
  under the old recipe is the one outcome that puts two answers into one
  index.
- The analyzer is replaceable while the app runs, where this document first
  said "set once at startup". The invariant that matters is one analyzer at a
  time, not one forever, and a write-once cell would have made turning the
  feature on a restart. Replacement is safe because a reading records the
  recipe that produced it — extraction identity is what keeps the two answers
  apart, and it is already the mechanism that re-reads documents when the
  recipe moves.

Recognition costs roughly a minute a picture on a CPU, which is a fact the
user is told before enabling rather than after.

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
  identical prompting, so one describer prompt serves both the first-class
  and the external path. External models remain whatever the user pulls into
  Ollama; Wilkes does not manage those.

Rejected for first-class support: Gemma 4 (license-inventory friction, dead
audio modality; a reasonable second family later), Moondream (weak
relationship extraction on dense labeled diagrams), SmolVLM2, LFM2-VL,
InternVL, and MiniCPM-V (no candle support — a port or a new runtime would
be a second mechanism; they remain reachable through Ollama), and
PaddleOCR-VL (a narrow parsing model that cannot produce free-form
relationship descriptions; its candidacy is as the OCR engine, not the
describer).

Verification item before pinning the recipe, **resolved 2026-08-27**:
candle 0.11's `qwen3_vl` is dense-safetensors only — there is no
`quantized_qwen3_vl` module beside it, as there is for several text
families. The 2B recipe is therefore ~4.4 GB on disk, the upper end of the
range this item was opened to decide, and there is no quantized path to fall
back to without writing one.

Two further facts about the module, from the same reading, because they are
what the integration costs: it exposes `forward` and no generation helper,
and its `forward` takes the 3D M-RoPE position sections, the deepstack
visual indexes, and the per-image placeholder spans (`continuous_img_pad`)
as caller-supplied arguments. Wilkes owns all of that geometry, alongside
the dynamic-resolution patch flattening the vision tower expects. That is a
larger Wilkes-owned surface than the recognizer's, and none of it is
checkable without the weights.

A describer receives:

- the native image pixels;
- accepted OCR regions and their coordinates;
- a fixed, versioned instruction to describe, in detail, only what is
  visibly present — the elements drawn and the relationships the drawing
  expresses between them.

It does not receive a caption in this phase because caption association is
deferred.

It returns prose. There is no response schema (**amended 2026-08-27**; see
"Why the description is prose and not a schema").

```rust
trait FigureDescriber {
    fn describe(
        &self,
        image: &RgbImage,
        ocr: &[ImageOcrRegion],
    ) -> Result<ImageDescription>;
}
```

Synchronous, where this document first sketched it `async`. Extraction is a
synchronous contract with many callers — indexing, watcher updates,
exact-search fallback, MCP reads, summaries and export all reach it — and
making it async to suit one analyzer would push a runtime requirement onto
every one of them. A describer that needs a server uses a blocking client,
which is what a batch extraction pass wants anyway.

Example output for the sample:

```text
A block diagram of an expert system. A non-expert on the left communicates
through a user interface, which exchanges arrows in both directions with an
inference engine at the centre. The inference engine draws on a knowledge
base below it, and the knowledge base is filled from expert knowledge on the
right. A dashed box encloses the inference engine and the knowledge base and
is labelled as the expert system.
```

### Why the description is prose and not a schema

**Amended 2026-08-27.** The original design returned a `description` string
alongside a list of `{source, relation, target}` triples under a fixed,
validated schema. That is withdrawn. The description is prose, detailed, and
nothing else.

The triples were justified as supporting "validation and future features",
and neither survives contact with what this phase actually does:

- Nothing consumes them. Canonical text takes the prose; the embedder sees
  the canonical text; exact search hits the OCR transcription, not the
  description. A structure no consumer reads is a second representation of
  the same fact, maintained and versioned for nobody.
- They do not validate anything. A triple is as invented as the sentence it
  was extracted from — a describer confident enough to hallucinate an arrow
  will emit a triple for it. Schema conformance proves the reply was shaped
  by something that understood the *format*, which is not the claim that
  matters here.
- They narrowed the field of usable models for no return. Requiring
  schema-conformant JSON demands an instruction-tuned model; asking for a
  paragraph does not. The describer is optional and local-first, so the
  models it can run on are the constraint, and a requirement that costs
  candidates has to earn it.

What replaces the schema as the gate on bytes entering the canonical
reading:

- the reply is normalized and must be non-empty after normalization;
- it is bounded in length, and a describer that runs past the bound is a
  truncated claim rather than a paragraph of invention;
- an empty, refused or unreachable reply is a *failed* description — a
  partial analysis — never an absent one.

Detail is now asked for explicitly, where the schema previously implied a
compact summary. The arrows on a figure are what the description exists to
add: the OCR transcription already carries every label, so a description
that only restates the labels adds nothing the reading did not have.

Relationship extraction as structured data is not abandoned, only unbuilt:
it becomes an explicit feature when something consumes it, and it will want
its own evaluation rather than riding along unmeasured.

### Consequences for the describer selection

The rejection of Moondream above reads "weak relationship extraction on
dense labeled diagrams", and the rejection of PaddleOCR-VL reads "cannot
produce free-form relationship descriptions". Both rationales were written
against the schema. Dropping it widens the field to any model that can
write a paragraph about a picture, which is a materially larger set than the
one that can emit conformant JSON — so the describer selection is reopened
to that extent, and Qwen3-VL remains the selection until something measured
displaces it. PaddleOCR-VL's rejection stands on its own ground regardless:
its task prompts are fixed, and none of them produces prose.

Local analysis remains the default requirement. Any remote describer must be
explicitly configured and visibly disclose that document imagery leaves the
machine. No OCR-adjacent pipeline doubles as an implicit description
fallback.

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
    Unrecorded,
    ImageOcr {
        image_id: String,
        confidence: Option<f32>,
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

Two departures from the sketch above, both as built:

- The confidence is optional. Every byte of the enrichment carries
  provenance, and some of those bytes are the label `Image embedded text:`
  and the separators between regions. Those are Wilkes' own structure, not
  anything a recognizer was confident about, and giving them a number would
  be inventing one.
- `Unrecorded` exists for the coarser per-chunk map the index rebuilds, where
  a chunk's provenance is not resolvable to a single segment. Naming that
  state is the alternative to defaulting it to `Native`, which would claim
  the document said something it did not.

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
page
+ normalized image bbox and transform
+ decoded image pixel SHA-256
+ OCR engine crate/pipeline version
+ recognizer SHA-256
+ OCR preprocessing and admission settings
+ description model revision
+ description prompt version
+ coordinate mapping version
+ technical limits version
+ canonical serialization version
```

Three differences from the first sketch of this key, all as built:

- **The source document's digest is deliberately absent.** The digest of the
  *decoded pixels* names what was analyzed. Two documents that draw the same
  pixels at the same place on the page have the same answer, and keying on
  the file as well would only stop them sharing it — while costing a full
  read of every PDF to compute the key.
- **No detector or dictionary digest.** The selected engine has neither; a
  single checkpoint does detection and recognition in one pass, which is the
  reason it was selected. Keying on artifacts that do not exist would be
  pretending the pipeline is the one that was rejected.
- **The coordinate mapping and the technical limits are in the key.** Both
  are Wilkes' own and neither is inside the engine's settings, so neither
  would otherwise move the recipe. A region that moves is a different
  reading even when the bytes are identical: the same text would resolve to a
  different part of the page.

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

OAR-OCR was originally selected over `ocrs` and Tesseract — a selection
since superseded; see the resolution below — because it provided the
stronger production result contract and model ecosystem:

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

One principle from that record outlives the engine swap: an engine selection
is not a checkpoint selection. The 1.5-versus-1.6 evaluation and
artifact-license inventory remain required before implementation can claim a
pinned, shippable OCR recipe.

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

And what discarding OAR saved, verified 2026-08-27: fastembed 5.13.0 pins
`ort = "=2.0.0-rc.11"` while oar-ocr-core 0.9.2 pins `ort = "=2.0.0-rc.13"`
— two `=` pins on one crate, so a joint build could not resolve at all. The
OAR path required fastembed 6.0.x, the Rust 1.88 → 1.95 toolchain bump, and
re-verification of embedding behavior and every supported platform build.

Candle's module is the recognizer only; PP-DocLayoutV2 is not in candle and
is not needed for this scope. Wilkes would own the spotting-task prompt and
`<|LOC|>` parsing. Whether the candle module drives the 1.5/1.6 checkpoints
and task prompts is an open verification item.
[PaddleOCR-VL v1 paper](https://arxiv.org/pdf/2510.14528),
[1.5 paper](https://arxiv.org/pdf/2601.21957),
[1.6 paper](https://arxiv.org/pdf/2606.03264), and
[candle paddleocr_vl module](https://docs.rs/candle-transformers/0.11.0/candle_transformers/models/paddleocr_vl/index.html).

**Resolution (2026-08-27).** PaddleOCR-VL is selected and OAR is discarded —
as a dependency, an implementation path, and a fallback alike. The roadmap
below decided it: OAR's route to LaTeX and tables adds PP-FormulaNet, table
models, and the dependency migration to reach what one prompt-switched model
provides. What remains is verification, as implementation plan step 1: the
candle module drives the 1.5/1.6 checkpoints and task prompts, `<|LOC|>`
output parses to usable quads, and character error, coordinate accuracy,
admission-rule viability, and CPU latency hold on the planned corpus. If
verification fails, the decision reopens with this record as context; no
OAR specification is kept warm. Nothing is scheduled; this records order,
not timing.

### LaTeX decision analysis

LaTeX fidelity is a committed goal: the roadmap below exercises it in its
second and third phases. It is still a scope change relative to this phase —
formulas sit in the deferred list above — and this is the analysis that
settled how it will be met:

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
"LaTeX decision analysis" — plus that section's serialization,
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

1. **Verify the engine on the pinned runtime**
   - Confirm the candle `paddleocr_vl` module drives the 1.5/1.6
     checkpoints and task prompts, and `<|LOC|>` output parses to usable
     quads.
   - Measure character error, coordinate accuracy, admission-rule
     viability, and CPU latency on the sample corpus; pick the shipped
     checkpoint.
   - If this fails, reopen the engine decision before any later step.

2. **Add structured image types and provenance**
   - Add `ExtractedImage`, `ImageOcrRegion`, image ranges, and analyzer identity.
   - Add native/OCR/description provenance to source-map segments.
   - Extend diagnostics without changing the existing PDF-page locator contract.

3. **Preserve native image blocks**
   - Enable MuPDF image preservation.
   - Retain block position, transform, pixels, and reading anchor.
   - Apply only explicit technical safety limits.
   - Do not add caption, vector grouping, or layout inference.

4. **Integrate PaddleOCR-VL spotting**
   - Add one Wilkes-owned adapter; task prompts and `<|LOC|>` parsing stay
     behind the extraction boundary.
   - Batch native images from a PDF.
   - Map image-relative polygons to page coordinates.
   - Apply the admission rule (one explicit, tested log-probability
     threshold) and native-text deduplication.
   - Install the checkpoint through the Wilkes model lifecycle.

5. **Add the independent description interface**
   - Wire the Qwen3-VL describer through the candle engine, with Ollama as
     the configured external door.
   - Pass image pixels and accepted OCR to the describer.
   - Normalize the reply, bound its length, and reject an empty one.
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
and supported-platform packaging. It selects one PaddleOCR-VL checkpoint for
the shipped recipe; it does not create a runtime choice between engines.

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
- The recognizer and describer run on the already-pinned candle runtime; no
  new inference dependency, toolchain change, or `ort` bump entered.
- PaddleOCR-VL alone owns OCR behavior; no `ocrs`, Tesseract, OAR, or
  runtime fallback is retained.
- No caption association, vector reconstruction, scanned-page layout inference,
  or whole-page VLM parsing has entered this phase implicitly.

The selected first implementation is therefore **MuPDF native image blocks +
PaddleOCR-VL spotting + a Qwen3-VL describer through the candle engine +
canonical-text integration**. More ambitious figure detection remains a
later, explicit feature rather than an accidental expansion of this one.
