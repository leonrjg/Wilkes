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
record as context; no fallback engine waits in the wings.

**Amended 2026-08-29 — the full task-prompt set is in scope.** As first
written, this phase drove only `Spotting:` and paired no layout model with
it, and formulas, tables and charts sat in the deferred list as roadmap
phases 2–3. That split no longer holds: PaddleOCR-VL is a document parser,
not a text spotter, and reading one of its six task prompts is a property of
this integration rather than a limit of the weights. `Formula Recognition:`
(LaTeX), `Table Recognition:` and `Chart Recognition:` are now required scope,
which makes region routing required with them (§"Routing", amended). What
remains deferred to phase 3 is *native vector* region discovery, not the
prompts. The acceptance audit dated 2026-08-28 stands as a record of what was
true then; the criteria this amendment adds are marked open.

The first implementation is intentionally limited to image blocks that MuPDF
already exposes from a digitally generated PDF. It does not attempt to decide
whether an image is a figure by matching `Figure`, `Fig.`, or similar text, and
it does not reconstruct complex layouts.

### Included now

- Preserve and enumerate MuPDF native image blocks.
- Retain each image's page, bounding box, transform, dimensions, and pixels.
- Run the selected OCR engine on each eligible image.
- Preserve recognized text, confidence, and geometry.
- Route each region to a content kind with a layout detector, and recognize
  formulas as LaTeX, tables as Markdown, and charts as data (amended
  2026-08-29).
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
- Scanned-page or whole-page layout detection. Region detection *inside an
  image crop* is in scope as of 2026-08-29, and so is marking out formula and
  ruled-table areas of a page from the page's own typography. Handing a whole
  rendered page to a recognizer and taking its reading of the prose is not,
  and is not planned: it would make the recognizer a second PDF extraction
  mechanism, which the architectural invariant forbids.
- ~~Native vector tables and formulas — content MuPDF draws rather than
  embeds.~~ **In scope as of 2026-08-29**; see "Phase 3 — Native vector tables
  and formulas" for what was built and what within it is still deferred.
  Unruled tables and inline mathematics remain out of scope, and the reasons
  are in the module rather than here.
- Seal recognition. The sixth task prompt exists; nothing in the corpus
  exercises it, and a prompt nobody measured is not scope.
- Whole-page VLM document-understanding pipelines.
- A second OCR engine or runtime fallback.

Struck from this list on 2026-08-29, having moved into scope: caption or
`Fig`/`Figure` heuristics remain deferred, but *tables, formulas and
chart parsing* and *a learned layout model for routing* do not. See
"Decision and scope".

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
the end-to-end text-spotting task. No OAR crate. Wilkes owns the task prompt
and the parsing of `<|LOC_nnn|>` tokens into per-region 4-point
quadrilaterals on the normalized 0–999 grid.

Amended 2026-08-29: "no layout model, no second ONNX runtime" no longer
holds — see "Task prompts and routing" below. One small ONNX detection graph
now runs beside the recognizer, on the `ort` this repository already carries.

Before pinning the final extraction recipe, compare the 1.5 and 1.6
checkpoints on the image evaluation corpus. The shipped checkpoint is the
better measured one, not a runtime choice: Wilkes packages and identifies
one recognizer checkpoint per recipe.

Normalize model output into Wilkes-owned types so engine types and token
formats do not leak across the extraction boundary:

```rust
ImageOcrRegion {
    /// Amended 2026-08-29: which task prompt produced `text`, and therefore
    /// how `text` is to be read. `Text` is the reading; `Formula` is LaTeX;
    /// `Table` and `Chart` are Markdown tables.
    kind: RegionKind,
    text: String,
    /// Admission signal derived from token log-probabilities. Explicitly
    /// uncalibrated; thresholded by one tested rule. Meaningful for `Text`;
    /// carried but not thresholded for the other kinds, which are admitted
    /// on validity — see "Per-kind admission".
    confidence: f32,
    polygon_within_image: Vec<Point>,
    page_polygon: Vec<Point>,
}
```

The image transform maps each spotted polygon — denormalized from the 0–999
grid to input pixels — into MuPDF page coordinates. Precise page polygons
allow exact search to highlight `Knowledge base` rather than the entire page
or image.

### Task prompts and routing (amended 2026-08-29)

PaddleOCR-VL 1.6 was post-trained on six task prompts. Wilkes drives four of
them; the strings are the model's, exactly, and are not paraphrased:

| Prompt | Produces | Wilkes' canonical form |
| --- | --- | --- |
| `Spotting:` | text instances with `<|LOC_nnn|>` quads | reading text, per-region polygons |
| `Formula Recognition:` | LaTeX | LaTeX, verbatim |
| `Table Recognition:` | table structure | Markdown table |
| `Chart Recognition:` | chart contents | Markdown table |
| `OCR:` | plain text, no geometry | *not driven* — `Spotting:` is `OCR:` plus the coordinates, and running both would be two answers to one question |
| `Seal Recognition:` | seal text | *not driven* — deferred, unmeasured |

`Spotting:` remains the only prompt that returns geometry. The other three
return content for the crop they are given and nothing about where it sits,
which is the whole of the routing problem: **something must decide what a
region contains before a prompt can be chosen for it.**

#### Routing is a layout detector, decided once

"LaTeX decision analysis" left three options open. One is now closed by
evidence and one by design:

- **Self-classification is ruled out.** The 1.6 model card documents six
  task-specific prompts and no unified parsing prompt; the official pipeline
  routes with PP-DocLayoutV2 before the VLM ever sees a region. The
  hypothesis that 1.6 might self-classify a figure crop was worth checking
  and did not survive the check.
- **Running every prompt and arbitrating is rejected.** It costs 4× the
  decode per crop, and it needs an arbitration rule — is this "valid" LaTeX
  a formula or a bar chart the formula prompt hallucinated over? — that
  exists only until phase 3 lands a detector and makes it dead code. Adding a
  second routing mechanism with a known expiry date is the thing this
  codebase's rules exist to prevent.
- **The layout detector moves from phase 3 to now.** The roadmap already
  committed to a PP-DocLayoutV2-class ONNX detection graph
  (RT-DETR/PicoDet) for native vector regions. It is the same routing
  question, and one router answering it twice is the structural fix; two
  routers, one temporary, is the ad-hoc one.

The cost of this decision is honest and should not be understated: it breaks
"no new artifacts" for the prompt work. It adds one detection graph — tens of
megabytes against the recognizer's 1.9 GB — and its digest joins the
extraction identity, which the original design explicitly did not have to
carry ("No detector or dictionary digest", under "Determinism, caching, and
identity"). That paragraph is amended by this one: the pipeline now has a
detector, so the recipe names it. What it buys is that phases 2 and 3 share a
mechanism instead of each having their own.

The detector runs on the `ort` already in the tree. Wilkes uses the model
artifacts and owns the pre- and post-processing, as the roadmap specified —
not the OAR crate.

#### Per-kind admission

Confidence-thresholding is a text rule and does not transfer. Each kind is
admitted by what makes *that* kind wrong:

- **Text** keeps the existing rule: mean token probability against the
  engine's tested threshold.
- **Formula** is admitted on validity: the LaTeX must parse. An
  autoregressive decoder that truncates mid-expression produces high-
  confidence invalid LaTeX, so confidence is the wrong question. A formula
  that does not parse is a rejected region with a recorded reason, never a
  string inserted and hoped over.
- **Table** is admitted on structure: the parse must yield a rectangular
  table of at least two rows and two columns, every row the same width. A
  ragged table is a failed recognition wearing the shape of a result.
- **Chart** is admitted as a table, by the same rule, and is labeled
  distinctly in the reading — a chart transcribed to rows is a
  *reconstruction*, not a quotation, and must not be presented as one.

Every rejection is recorded in diagnostics with its kind and its reason, as
low-confidence text regions already are.

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

**Accuracy.** On the shipped checkpoint, over eight figures and forty emitted
regions: character error 0.111, four regions missed, two wrong. Five figures
were transcribed exactly, character error 0.000 — a clean diagram, the same
one at 120x180 where a 12-point label is seven pixels tall, a coloured
background, an inverted dark background, and non-ASCII labels. The figure with
no text in it produced no regions at all. The sample document's own diagram
came back with all twelve of its drawn lines exact. Coordinate accuracy is
0.012 of the image on average and 0.025 at worst, comfortably inside a label's
own footprint.

Every missed and every wrong region came from one figure.

**Turned labels are a real weakness.** Both checkpoints garble text rotated a
quarter turn — `User interface` came back as `User intiaac expert` — reading
one of five. Recorded, not worked around: this is a property of the weights,
and it is the whole of this corpus's error.

**The checkpoint choice is not load-bearing**, which is the honest result of
comparing them. On seven of eight figures 1.5 and 1.6 are indistinguishable:
the same five perfect transcriptions, the same empty answer on the textless
figure, the same twelve exact lines on the sample, and coordinate accuracy
equal to a thousandth. On the eighth both fail, and their character error
differs by 0.03 — a difference between two garbled strings, not a basis for a
recipe. 1.6 ships as the later post-training of the same weights with nothing
measured against it. They are not distinguished on speed either; wall-clock
differed between runs, but the runs were not made on an equally idle machine
and the checkpoints are the same architecture at the same parameter count.

**The admission threshold is 0.70**, measured rather than chosen. Across the
forty regions it is the point where both errors are zero:

```text
threshold   correct in   wrong in   correct lost
     0.60           38          1              0
     0.70           38          0              0
     0.80           37          0              1
     0.90           34          0              4
```

0.60, where this sat before it was measured, admitted a garbled region. Above
0.70 the rule starts discarding transcriptions that were right. Forty
observations from one corpus is a small basis and the wrong regions all came
from the turned-label figure, so this is a real operating point and not a
calibration.

**CPU latency is the finding that costs.** 33 seconds for a 120x180 figure,
around 95 for 240x360, 239 for the sample document's 1559x499 diagram, and
several times that again for a figure large enough to fill the spotting
envelope. The shipped build enables neither `candle-metal` nor
`candle-accelerate`, so this is plain f32 CPU matmul and is what a user gets.
A library of a few hundred figures is an overnight job. Nothing about the
extraction design depends on this — the annotation cache means it is paid once
per image per recipe — but it is what the settings surface has to say out
loud, and it is the strongest argument for the Metal path this repository does
not yet build.

**One caution about the measurement itself.** Two of this evaluation's early
results were defects in the corpus, not in the model, and both looked exactly
like character error until they were traced: a label laid out past the right
edge of the page, and a ground-truth list written from this document's prose
summary of the sample figure rather than from the twelve lines the figure
draws. The corpus now asserts that its labels fit. The remaining character
error on the sample is reading-order disagreement between two defensible
orders for a figure whose elements sit side by side, not misread characters —
which is why region exactness is reported beside it.

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

PaddleOCR-VL is Apache-2.0 licensed.

**The inventory exists, 2026-08-28.** The checkpoint carries its SPDX
identifier, the statement it is published under, and the works it is derived
from — a NaViT-style vision encoder and an ERNIE-4.5-0.3B decoder, both
Apache-2.0. Naming only the repository the files are fetched from would have
been an inventory of the download rather than of the model.

Two things make it an inventory rather than a claim. It is built from the same
artifact list the install walks, so a fourth file cannot reach a user's disk
without appearing in what they are told they are downloading, and a test holds
that. And it is rendered where the download is offered — the licence, the
size, the pinned revision, the components, and every file with the digest it
is verified against — so it is readable before the 1.9 GB arrives, which is
the only time it is of use to whoever has to decide.

Wilkes fetches these files at the user's request rather than shipping them
inside the application. That is what the disclosure point follows from: there
is no bundle to put a NOTICE in, and the moment of redistribution is the
moment the user asks for one.

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

Recognition is slow on a CPU, which is a fact the user is told before enabling
rather than after — and the figure they are told is the measured one. This
section first said "roughly a minute a picture", which was written before the
measurement and was wrong by a factor of four at the size that matters: about
half a minute for a small diagram, four minutes for a full-width one, and
several times that for a figure large enough to fill the spotting envelope.

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

Amended 2026-08-29: one label per recognized kind, on the same reasoning.
A reader and an LLM must be able to tell a transcription from a
reconstruction without consulting metadata:

| Kind | Label | Body |
| --- | --- | --- |
| Text | `Image embedded text:` | the reading, regions separated as today |
| Formula | `Image embedded formula:` | LaTeX, verbatim, no fencing added |
| Table | `Image embedded table:` | a Markdown table |
| Chart | `Image transcribed chart:` | a Markdown table |

`Image transcribed chart:` deliberately does not say *embedded*. The other
three labels name content that is present in the image and was read; a chart
rendered as rows is Wilkes' reconstruction of what the picture depicts, and
the label is the only place a consumer learns that. This is the same
distinction the `Image description:` label already draws, applied to a case
that sits between quotation and description.

Tables serialize as Markdown, as decided under "Roadmap → Decisions taken
now" — one canonical table format, versioned in the recipe. The
`Table Recognition:` prompt's own output format is converted to it; whatever
the model emits is an engine token format and does not cross the extraction
boundary, exactly as `<|LOC_nnn|>` does not.

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
        /// Amended 2026-08-29. Which prompt produced these bytes. A formula
        /// and a paragraph are both recognized content and are not the same
        /// claim about the document, so provenance names the kind rather
        /// than leaving a consumer to infer it from the label.
        kind: RegionKind,
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
- **Never split inside a formula or a table** (amended 2026-08-29). Half a
  LaTeX expression is not a shorter expression, it is an invalid one, and
  half a Markdown table is not a smaller table. A formula or table that
  exceeds the configured chunk size is its own oversized chunk; the
  alternative is a chunk that reconstructs to bytes no consumer can parse.
  This is a stronger rule than the region-boundary preference above, and it
  overrides it.
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
+ task-prompt set and per-kind admission rules   (amended 2026-08-29)
+ layout detector SHA-256 and its settings        (amended 2026-08-29)
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

  *Superseded 2026-08-29.* There is now a detector — the routing graph — and
  the key carries its SHA-256 and its post-processing settings. The reasoning
  above was never "detectors do not belong in the key"; it was that keying on
  an artifact this pipeline did not have would be pretending. It has one now,
  so it names it. There is still no dictionary.
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
regions_routed_by_kind{text,formula,table,chart}
regions_marked_not_text
regions_unroutable
formulas_accepted
formulas_rejected_invalid_latex
tables_accepted
tables_rejected_malformed
charts_accepted
charts_rejected_malformed
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
second and third phases. This is the analysis that settled how it will be
met.

**Resolved 2026-08-29.** The sentence that stood here — "it is still a scope
change relative to this phase; formulas sit in the deferred list above" — no
longer holds. Formulas, tables and charts are required scope; the routing
question this analysis identified as decisive is answered under "Task prompts
and routing"; the scope amendments it demanded (a serialization label,
provenance, chunk boundaries, metrics) are made in their own sections rather
than left as a list of things somebody should do. The analysis below is
retained because its reasoning is what produced those answers:

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
  → **Answered 2026-08-29:** self-classification does not exist in 1.6; the
  multi-prompt rule is rejected as a mechanism with a built-in expiry; the
  phase-3 layout detector moves forward and routes both cases.
- Required scope amendments: an `Image embedded formula:` serialization
  label, formula provenance, chunk boundaries that never split inside a
  formula, and formula metrics (ExpRate/CDM-style plus a LaTeX-validity
  admission check) in the evaluation.
  → **Made 2026-08-29**, under "Canonical representation and provenance",
  "Chunking", and "Verification items" respectively.
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

### Phase 2 — Formulas, tables and charts inside figures

**Folded into scope 2026-08-29; no longer a later phase.** Same crops, same
weights, different task prompts: `Formula Recognition:` yields LaTeX,
`Table Recognition:` and `Chart Recognition:` yield Markdown tables. The
routing rule is decided (a layout detector, brought forward from phase 3) and
the serialization, provenance, chunking and metric amendments are made in
their own sections.

The phrase this section used to end on — "nearly for free" — was wrong, and
the correction is the substance of the amendment. The prompts are free; the
*routing* is not, and it was the whole cost all along. Pricing it honestly is
what moved the detector forward instead of leaving a cheap-looking phase that
could not be built.

### Phase 3 — Native vector tables and formulas

Born-digital PDFs draw most tables and math as vector content, invisible to
the image-block scope.

**Built 2026-08-29, and not with a layout model.** The trigger was a report
that granite-docling was "not OCR'ing LaTeX": a reading of a cryptography text
returned `ci = ai ⊕bi` with and without the recognizer, byte for byte. It was
not a recognizer failure. The formula is glyphs the page draws, so no image
block exists there, so nothing was ever dispatched — the head of the path was
missing while everything downstream of it worked.

What closed it is cheaper than the detector this section planned, because the
document already declares the thing a detector would have to infer:

- **Formulas are found by font.** A typesetter that sets mathematics switches
  to a math font — `CMMI`/`CMSY`/`CMEX` under TeX, `LatinModernMath` or
  `STIXTwoMath` under unicode-math, `Cambria Math` under Word. That switch is
  a fact in the file. A *line* most of whose glyphs come from one is a display
  formula; the threshold separates roughly 0.9 on the reported equation from
  0.08 on the sentence above it. Fonts are read with a MuPDF `NativeDevice`,
  since the safe structured-text API exposes a character's box and size but
  not its face.

  *Corrected 2026-08-29, from the reported document.* The first list knew only
  TeX's and unicode-math's names, and the document that prompted the whole
  feature sets its mathematics in `DBAMWK+Formula` — a publisher's own face,
  named for its job, matched by nothing in the list. The reading was unchanged
  and every count read zero, which is indistinguishable from a document with
  no mathematics in it. The rule now also matches a family whose name contains
  `math`, `formula` or `equation`, and the gap it closed is the reason for the
  face report below.
- **A line is only read again if flattening destroyed something.** The
  measurement that settled this: on the reported document, 103 lines pass the
  font test and only 31 carry a subscript, superscript or stacked term. The
  other 72 are inline fragments — `mod n`, `p −1`, `12P = 8P + 4P` — that
  flatten to *themselves*: there is nothing in them to recover, and reading
  each one costs a recognizer call. A length floor was tried first and is the
  wrong question in the wrong units; it kept
  `ETAOINSRHDLUCMFYWGPBVKXQJZ` — a cipher alphabet set in the same face,
  twenty-six glyphs, nothing to repair and a transcription that could only
  damage it — and dropped `c = me` at four glyphs. Structure is detected as
  the spread of the line's glyph sizes and baselines, which is the damage
  itself rather than a proxy for it.
- **Tables are found by their rules.** `FZ_STEXT_COLLECT_VECTORS` hands over
  the thin wide rectangles the page filled; three of them sharing a column,
  with text between, is the booktabs shape.
- **The region is rendered and goes through the existing pipeline.** MuPDF
  draws the area at four to eight times page scale into a white-padded crop,
  and it enters `analyze` as a `DiscoveredImage` like any embedded figure —
  same recognizer, same per-kind admission, same serialization, same source
  map, same chunk rule.

The detector this section specified is therefore not built and is no longer
planned for this purpose. It remains the answer for what the typography cannot
declare: an unruled table, and a document that sets its mathematics in a text
font. Both are out of scope and stated as such in the module.

**A document that yields nothing says why.** The faces a document draws with,
and which of them were read as mathematics, are reported once per document —
at info when *none* was, because that is the case a reader needs to see. Before
this, a document whose mathematics is set in an unrecognized face produced no
formulas and looked exactly like a document with no mathematics in it; finding
the difference took a throwaway probe against the file. The face names are the
evidence and they are only in the PDF, so the log is where they belong.

**The crop is padded in pixels, not in points.** A recognizer's tiler takes
the canvas's pixel dimensions and rounds them up to whole tiles, so a canvas a
hair under the aspect bound is charged for a second row of them. Measured on
this document's regions: 1409x353 tiles as 4x2 — nine tiles with the thumbnail,
576 visual tokens — where 1409x352 tiles as 4x1, five tiles and 320 tokens, for
the same picture. The first version padded the page rectangle and rounded to
pixels afterwards, which landed on the wrong side of that boundary every time
and paid ~80% more prefill per region than it needed. Padding the pixel
rectangle, with `floor` on the derived edge so the ratio meets or passes the
bound, lands on the right side by construction.

That coupling is real and cannot be asserted from inside the renderer, because
the rounding happens on the other side of the module boundary. One test spans
it — `the_render_pads_a_sliver_with_paper_and_not_with_the_rest_of_the_page`
runs the produced canvas through `granite_docling::tile_grid` and requires one
row — and it is the only place the two modules meet.

**Known, and not fixed here: embedded figures are squashed.** `prepare_tiles`
resizes an image onto the tile grid without padding it first, so any image
whose aspect is not exactly `cols:rows` is distorted on the way in — the
worked example's 1559x499 figure arrives at 2048x1024, a 1.56x vertical
stretch. That is the same defect as the one above on the other side of the
boundary, and fixing it properly means moving the fit into `prepare_tiles`,
which owns the grid, and correcting recognized coordinates back through the
pad. It changes what every embedded image looks like to the model, so it needs
`doctags-v1` to move and the library to re-read. Not done.

**Cost is bounded and the bound is reported.** Recognition is tens of seconds
a region, so inline mathematics is deliberately unreachable — the unit is the
line, and an inline formula shares its line with prose — and a per-document
budget caps how many regions one reading may spend. What the budget drops is
counted and logged, because a bounded run that reports nothing dropped reads
exactly like a document that had nothing more to find.

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

  **As built, the extension is a replacement rather than an addition, and
  deliberately so.** The native-glyph dedup asks "does the page already draw
  this?" and refuses the transcription when it does. For a typeset region the
  answer is always yes — that is what the region *is* — so the check is not
  tightened for it, it is removed: `RegionOrigin::Typeset` skips it outright,
  and the recognizer is the designated owner of those bytes. Two rules
  competing for the same bytes was the thing to avoid; one rule per origin is
  what replaced it. Ownership is settled in exactly one place,
  `sanitize::supersede_typeset_regions`, and only where the recognizer's
  answer was admitted — a formula whose LaTeX does not close, a ragged table,
  or a failed recognition leaves the page's own glyphs untouched. The failure
  mode of a wrongly marked-out region is a wasted recognizer call, never a
  paragraph replaced by nothing.

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
- ~~Phase 3: the chosen layout detector's ONNX opset runs on the pinned
  `ort`.~~ Withdrawn 2026-08-29: phase 3 shipped without a detector, so there
  is no opset to verify. What replaces it as this phase's open measurement is
  the math-font share threshold, which is set from the reported document's two
  sides and not from a sweep — see the typeset-routing items below.

Added 2026-08-29, for typeset routing:

- **The math-font share threshold is a band, not a sweep.** It is set from
  two measurements on one document: a display equation at roughly 0.9 and the
  sentence above it at 0.08. That the signal separates display mathematics
  from prose is not in doubt; where exactly the line sits between them is, and
  widening it to the evaluation corpus is the outstanding work. The failure
  mode of a threshold set too low is cost — a prose line marked out, read, and
  refused by admission — and of one set too high is a formula left as the
  glyph run it is today. Neither corrupts a reading.
- **The font list is a list, and a list is not a rule.** `math`, `formula`,
  `equation` and the TeX families cover what has been seen; a publisher who
  names its math face for its course code is not covered, and nothing in the
  file would give it away. The face report is the mitigation and not the fix:
  it makes the miss visible, and adding a name is then a one-line change made
  on evidence. What would replace the list is a signal that does not depend on
  naming at all — the face that is neither the body face nor a weight of it —
  and that is not built.
- **The per-document region budget has not been calibrated against a
  mathematics textbook.** The number bounds a wait, and what a reader will
  wait for is the measurement that has not been taken.
- **Table detection by rules has not been run against an unruled table,
  because it cannot find one.** What is unverified is the false-positive rate
  on framed figures and boxed asides, which the rule stack can look like.
- **The recognizer has not been measured on a rendered formula crop.** The
  path is proven end to end against a stub; what granite-docling actually
  returns for a white-padded display equation at four to eight times page
  scale is the gating measurement, and it needs the weights.

Added 2026-08-29, for the prompt work:

- The candle `paddleocr_vl` module accepts `Formula Recognition:`,
  `Table Recognition:` and `Chart Recognition:` against the shipped
  checkpoint, and each returns parseable output for a known-good crop. This
  is the first item and it gates the rest: the module was verified for
  spotting only, and a prompt the module cannot drive is a decision to
  reopen, not a bug to work around.
- The layout detector's opset runs on the `ort` in the tree, and its
  labels map onto `RegionKind` without a residual class that has nowhere to
  go.
- Formula metrics on the evaluation corpus: ExpRate/CDM-style accuracy plus
  the LaTeX-validity admission check, reported beside the existing character
  error and coordinate accuracy.
- Table metrics: TEDS, or a stated reason for a different measure.
- End to end, a page whose only equation is a raster image resolves that
  equation's LaTeX through exact search.

**On the `ort` pin.** This document's acceptance list contains "no new
inference dependency, toolchain change, or `ort` bump entered", and that was
true of the phase it audited. Both halves are now superseded: the detector is
a second inference path, and
[recognition-engine.md](specs/recognition-engine.md) moves the pin to
`=2.0.0-rc.13` for reasons of its own. The two changes must land in one
`ort` version, not two — whichever ships first sets it, and the other is
written against it.

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

**All of that list is measured, 2026-08-28.** The first run reported five of
the nine and the section above was written from it, which is why its caution
about the measurement is the shape it is. The harness now reports the rest:

- **Word error** beside the character rate, from the same edit distance over a
  different unit. They fail differently — one misread letter costs one
  character and a whole word — so a transcription that is nearly right and one
  that dropped a label are told apart rather than averaged together.
- **Reading order**, as the fraction of region pairs emitted in the order the
  figure draws them. This is the one that was genuinely missing rather than
  merely unreported: a figure whose every label is transcribed perfectly still
  reads wrongly if the labels arrive in an order the drawing does not support,
  and character error cannot see that. It is also what the sample's residual
  error actually is, which the section above could only say in prose. Two
  regions reading the same string count as one position, so a repeated label —
  `Expert`, twice, on the sample — disagrees with nothing.
- **Model footprint**, from the pinned artifact sizes rather than from
  whatever is on the machine that ran it.
- **Peak resident set** of the process, where the platform reports one, and
  absent where it does not rather than a zero that would read as measured.
- **Supported-platform packaging**, to the extent a run can establish it: the
  target, the realized device, the dtype and the compiled inference backends,
  recorded on every result. A latency figure without those beside it is not
  attributable to anything, and the shipped build compiles neither
  `candle-metal` nor `candle-accelerate` — which is what makes the CPU numbers
  the numbers a user gets.

The correctness and missed-region rules are unchanged, deliberately: the
numbers recorded above were measured under them, and moving the definition
without re-running the corpus would have quietly restated an old measurement
as a new one.

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
  *(Superseded 2026-08-29 — the routing detector is a second inference path
  and the pin moves. See "Verification items → On the `ort` pin".)*
- PaddleOCR-VL alone owns OCR behavior; no `ocrs`, Tesseract, OAR, or
  runtime fallback is retained. *(Still holds, and note what it says: no
  fallback. A second **selectable** recognizer, named in the recipe, is a
  different thing — see [recognition-engine.md](specs/recognition-engine.md).)*
- No caption association, vector reconstruction, scanned-page layout inference,
  or whole-page VLM parsing has entered this phase implicitly.

Added 2026-08-29 for the prompt work, and **none of these is met yet**:

- Every one of the four driven task prompts produces canonical output under
  its own label, and a fifth kind cannot appear without a label.
- A formula that does not parse as LaTeX is rejected with a recorded reason
  and does not reach the reading.
- A table region reaches the reading as a Markdown table, or is rejected;
  the engine's own table format never crosses the extraction boundary.
- A chart is labeled as a transcription, not as embedded content.
- No chunk boundary falls inside a formula or a table, and byte-for-byte
  reconstruction still holds across an oversized one.
- The task-prompt set and the detector digest are both in the extraction
  identity, and changing either forces re-extraction.
- Routing is one mechanism. There is no second path that decides what a
  region contains.

### Where the list stands, 2026-08-28

Every criterion above except one is met and pinned by a test, so that the
answer to "is this still true" is a suite run rather than a reading. The
describer's own criteria are excluded here: that work is separate and this
audit did not touch it.

| Criterion | Held by |
| --- | --- |
| Discovery without a caption heuristic | `a_native_image_block_is_found_with_its_placement_and_pixels`, `enrichment_lands_at_the_image_block_rather_than_near_a_caption` |
| Enrichment once, at the reading anchor | `a_diagram_with_labels_is_transcribed_into_the_reading`, `a_page_scoped_read_carries_that_pages_enrichment_and_no_others` |
| Exact search resolves to the OCR polygon | `exact_search_finds_a_transcribed_label_at_its_own_polygon` — **the mechanism, not the sample**; see below |
| Semantic search reaches the enrichment | `semantic_search_retrieves_the_enrichment_for_a_question_only_the_picture_answers` |
| One embedding path | `the_enrichment_reaches_the_embedder_as_one_passage` |
| A self-contained enrichment chunk | `an_image_block_is_a_chunk_of_its_own` |
| Byte-for-byte reconstruction | `chunks_around_an_image_block_still_reconstruct_the_reading` |
| A locator and provenance on every inserted byte | `every_inserted_byte_has_a_locator_and_truthful_provenance`, `every_piece_carries_provenance_and_a_region` |
| Recipe changes force re-extraction | `every_input_that_changes_the_bytes_changes_the_recipe`, `a_different_recipe_never_reads_the_previous_recipes_answer` |
| Failures visible as partial results | `a_recognition_failure_is_partial_rather_than_empty`, `without_an_analyzer_the_reading_is_unchanged_and_the_image_is_counted` |
| No new inference dependency or `ort` bump | The whole feature added `image`, `url` and `base64` and nothing else; `ort` is still `=2.0.0-rc.11` and there is no toolchain file to bump |
| One OCR owner | No `ocrs`, Tesseract or OAR crate is in any manifest, and a recognition failure has no second engine to fall to |

The exception is the third row, and it is the one recorded under "The
acceptance criterion that fails". The polygon mechanism works and is pinned;
what does not hold is the criterion as written against the sample, whose
labels are each set on two drawn lines. That is a scope decision about layout
analysis, not an implementation gap, and it stays open with
`a_label_split_across_two_drawn_lines_is_two_regions_in_the_reading` holding
the behaviour so it cannot regress into being forgotten.

The selected first implementation is therefore **MuPDF native image blocks +
PaddleOCR-VL spotting + a Qwen3-VL describer through the candle engine +
canonical-text integration**. More ambitious figure detection remains a
later, explicit feature rather than an accidental expansion of this one.

### Status after the 2026-08-29 amendment

The audit above is a record of 2026-08-28 and is not restated as current.
Against the amended scope, this document now describes:

| | |
| --- | --- |
| **Completed** | Everything in the table above: native image discovery, `Spotting:`, admission, canonical integration, provenance, chunking, identity, diagnostics. Twelve of thirteen criteria, each held by a named test. |
| **Partial** | The polygon criterion, unchanged — the mechanism holds, the sample does not, and `a_label_split_across_two_drawn_lines_is_two_regions_in_the_reading` keeps it from being forgotten. |
| **Specified, not built** | The three added task prompts, the routing detector, per-kind admission, the three new labels, `RegionKind` on regions and provenance, the formula and table chunk rule, the identity additions, and the eight criteria added above. |

Nothing in the third row has been implemented. This amendment moves it out of
the roadmap and into scope; it does not move it into the build. The first
verification item — that the candle module drives the three prompts at all —
gates every other item in that row and has not been run.

### Status after the recognition-engine change, 2026-08-29

The row above is superseded, and not by the work it described.
[recognition-engine.md](specs/recognition-engine.md) made the recognizer an
engine × model choice and shipped granite-docling under ONNX as the default,
and granite-docling *self-classifies*: one decode returns DocTags in which a
formula is a formula, a table is a table and a chart is a chart, with no router
in front of it. So the kinds arrived through the ONNX engine rather than
through `Formula Recognition:` and a detection graph, and everything this
amendment specified *downstream of routing* was built for them:

| | |
| --- | --- |
| **Completed** | `RegionKind` on `ImageOcrRegion` and on `TextProvenance::ImageOcr`; per-kind admission (text on the threshold, formulas on LaTeX validity, tables and charts on being a rectangular Markdown table of at least 2×2); one label per kind, with the exhaustive match that stops a kind reaching the reading unlabelled; the formula-and-table chunk rule; `ADMISSION_RULES_VERSION` in the analyzer identity; the per-kind and unroutable diagnostics; and the recognizer reporting what it could not route rather than dropping it — amended 2026-08-29 to separate a region it *named* as carrying no text, a `<picture>`, from one this build could not name at all, since a document parser answers the first once per figure and the two together are unreadable as one number. |
| **Partial** | The polygon criterion, unchanged from 2026-08-28. |
| **Not built** | `parsing-v1`: PaddleOCR-VL's `Formula Recognition:`, `Table Recognition:` and `Chart Recognition:` prompts, and the layout detector that routes regions to them. |

Six of the eight criteria added by the amendment now hold, each pinned by a
test:

| Criterion | Held by |
| --- | --- |
| Every kind under its own label; no fifth kind without one | `each_kind_is_written_under_its_own_label`, `every_kind_has_a_distinct_label`, `every_recognized_kind_reaches_the_reading_under_its_own_label` |
| An unparseable formula is rejected with a reason | `a_formula_is_admitted_on_whether_its_latex_closes`, `a_formula_that_does_not_parse_is_refused_with_its_reason` |
| A table is Markdown or is rejected; no engine format crosses | `a_table_is_admitted_on_being_rectangular`, `a_ragged_table_converts_and_is_refused_by_admission`, `a_table_that_is_not_rectangular_is_refused_on_structure` |
| A chart is labelled a transcription | `a_chart_is_labelled_a_transcription_and_a_table_is_not` |
| No boundary inside a formula or table; reconstruction holds | `a_table_larger_than_a_chunk_is_not_cut_in_half`, `chunks_still_reconstruct_across_an_oversized_table`, `a_table_that_fits_is_chunked_as_it_always_was` |
| Routing is one mechanism | `placement_carries_the_kind_and_does_not_assign_one` — the kind is the recognizer's answer, carried through placement and admission, and assigned nowhere else |

The two that do not are the two that name `parsing-v1`'s parts. "The task-prompt
set and the detector digest are both in the extraction identity" holds for its
first half — the task configuration is in every engine's identity string, which
is what makes `spotting-v2` and `doctags-v1` different recipes — and has no
second half to hold: there is no detector, because the shipped engine needs
none. And "every one of the four driven task prompts produces canonical output"
is not a statement about this build, which drives one prompt per engine.

**What `parsing-v1` still is, and why it did not land here.** It is not made
redundant by granite-docling: it is the Candle engine's second task
configuration, and a user who chooses PaddleOCR-VL gets `Spotting:` and prose
only. Building it needs an ONNX detection graph pinned by repository, revision,
file list and SHA-256 — digests that can only come from the artifact, never from
reasoning about it — and the verification item that gates the whole of it, that
the candle module drives the three prompts against the shipped checkpoint at
all, needs 1.9 GB of weights and has still not been run. Writing the prompts and
the detector against neither would be shipping an unmeasured second routing
path, which is the thing the amendment's own reasoning rejects. It stays named
here, unbuilt, with its gate unchanged.
