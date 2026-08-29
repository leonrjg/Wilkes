# Recognition Engine — Design

Status: implemented (2026-08-29)
Branch: `develop`
Depends on: `OcrEngine`, the recognition worker protocol, `ExtractionRecipe`
identity, the annotation cache, `EmbedderCapabilityManifest` (as precedent)

## 1. Purpose

Recognition today is one engine with one checkpoint. `RecognitionEngine` has a
single variant, [`build_analyzer`](../../../crates/core/src/extract/image/mod.rs)
hardcodes `RecognitionEngine::default()` rather than reading a setting, and
`ImageAnalysisSettings` has no field naming a recognizer at all. The checkpoint
is 1.9 GB of PaddleOCR-VL, which is the reason image analysis reads as an
expert feature rather than a default one.

Embedding solved this problem already. `EmbeddingEngine` is an enum with a
`#[default]` arm on the ONNX engine, each engine names its own
`default_model()`, `list_models` returns a per-engine catalogue, and
`model_capabilities` answers *what choosing a model would mean* so that no
consumer keeps its own table of model facts. The consumer-facing default is
`Fastembed` + `AllMiniLML6V2` — a small ONNX model that runs anywhere — with
larger and more specialized models available behind the same boundary.

This design gives recognition the same shape, and moves the consumer-facing
default onto `granite-docling-258M` under ONNX, with PaddleOCR-VL retained as a
catalogued alternative. It does **not** invent a new boundary: every question
this raises — how models are catalogued, how one is installed, how a choice
enters the recipe, where the weights are loaded — was answered on the embedding
side and is answered the same way here.

### Goals

- `RecognitionEngine` × model id as the addressable unit, mirroring
  `EmbeddingEngine` × `EmbedderModel`.
- A per-engine catalogue with a default, so recognition has a picker rather
  than a constant.
- `granite-docling-258M` under ONNX as the shipped default, with formulas,
  tables and figures in one pass. (Sized at ~318 MB when this was written;
  measurement forced fp32 and 1.26 GB — see §5.7.)
- PaddleOCR-VL kept, catalogued, and selectable — not deprecated.
- Model-specific types (`RecognizerInventory`) stop appearing in API
  signatures, the way `EmbedderCapabilityManifest` already does not.
- Every reading still names, in its recipe, the engine, model and task
  configuration that produced it.

### Non-goals

- Retiring PaddleOCR-VL, or changing its behaviour under its existing recipe.
- An Ollama recognition engine. §9 uses Ollama as a measurement harness that
  ships nothing.
- Re-reading existing libraries. A corpus built under
  `candle+paddleocr-vl-1.6` keeps that identity until the user changes the
  setting (§7).
- Figure *description*, which is a separate fact produced by a separate model
  and already has its own door.

## 2. Invariant

> A reading is produced by exactly one recognizer, and the recipe names which.
> Every model-dependent fact a consumer needs before choosing — what it costs,
> what element kinds it can produce, what its admission threshold is, whether
> its weights are here — is answered by the recognition boundary, not by a
> table the consumer keeps.

The second sentence was the part that was not true when this was written:
`ADMISSION_THRESHOLD` was a module constant inside `paddleocr_vl`,
`recognizer_inventory()` returned a `paddleocr_vl::RecognizerInventory` to
three API surfaces, and `dispatch` routed by engine and then asked
`paddleocr_vl` what a model id means. All three are closed; §4 records what
they were.

`ocr.rs` states the first sentence already and it is preserved without
amendment: *"There is exactly one production engine. A recognition failure is a
partial result, never a second engine's turn."* A second **selectable** engine,
named in the recipe, is consistent with that. A fallback chain is not, and this
design introduces none.

## 3. What is being reused

The embedding boundary, verbatim in shape:

| Embedding | Recognition |
|---|---|
| `EmbeddingEngine` (`#[default] Fastembed`) | `RecognitionEngine` (`#[default] Onnx`) |
| `engine.default_model()` → `"AllMiniLML6V2"` | `engine.default_model()` → `"granite-docling-258M"` |
| `EmbedderModel(String)` | `RecognizerModel(String)` |
| `list_models(engine, data_dir) -> Vec<ModelDescriptor>` | `list_models(engine, model_dir) -> Vec<RecognizerDescriptor>` |
| `model_capabilities(..) -> EmbedderCapabilityManifest` | `recognizer_capabilities(..) -> RecognizerCapabilityManifest` |
| `prepare_embedder` / `load_embedder_local` | `install_recognizer` / `load_recognizer_local` |
| `fetch_model_size(engine, model_id)` | `footprint_bytes` on the catalogue entry |
| worker-only local load; a fault takes the worker, not the app | unchanged — already true |

Three deliberate departures, each with a reason:

1. **No `CustomModel` for recognition.** `supports_custom_models()` is false for
   `Fastembed` because a fastembed model id is an enum name, not a repository.
   Recognition is the same: a model id names a task prompt, a preprocessor and
   a parser that must exist in this build. A hand-typed repository id cannot
   supply those. The catalogue is closed.
2. **`RecognitionEngine` stays in `extract::image::dispatch`.** It does not move
   to `types.rs`. `GenerationSettings` already references
   `crate::generate::GenerationEngine` from its own module; recognition follows
   that precedent rather than `EmbeddingEngine`'s.
3. **The manifest type lives in `types.rs`.** `RecognizerCapabilityManifest`
   and `RecognizerInventory` cross the API boundary, so they belong beside
   `EmbedderCapabilityManifest`, not inside a model module.

## 4. The four divergences, as they were

All four are closed. Kept as the record of what the change was for.

1. **The engine is a constant, not a setting.**
   `extract/image/mod.rs` builds `dispatch::RecognitionEngine::default()` and
   asks `dispatch::shipped_model_id(engine)`. `ImageAnalysisSettings` carries
   `enabled`, `device` and `describer_model` — nothing names the recognizer.

2. **`dispatch` delegates model-id resolution to a model module.**
   `identity`, `admission_threshold` and `installed` all route on the engine and
   then call `paddleocr_vl::checkpoint(model_id)`. With one model family this is
   invisible. With two it means `paddleocr_vl` decides what a granite-docling id
   means.

3. **A model-specific type is in three API signatures.**
   `paddleocr_vl::RecognizerInventory` appears in `crates/desktop/src/lib.rs`,
   `crates/server/src/lib.rs` and `crates/api/src/context.rs`. The struct's
   *fields* are already model-independent — name, repo, revision, license,
   artifacts, footprint. Only its address is wrong.

4. **The admission threshold is a module constant.**
   `ADMISSION_THRESHOLD: f32 = 0.70` sits in `paddleocr_vl` and is baked into
   `identity_of`. It is a per-model operating point wearing the shape of a
   global.

## 5. Design

### 5.1 Engines and models

```rust
pub enum RecognitionEngine {
    /// ONNX Runtime via `ort`, in the recognition worker.
    #[default]
    #[serde(alias = "onnx")]
    Onnx,
    /// candle-transformers, in the recognition worker.
    #[serde(alias = "candle")]
    Candle,
}
```

`Candle` keeps its existing serde alias so settings and recipes that name it
continue to deserialize. `default_model()`:

| engine | default model | alternatives |
|---|---|---|
| `Onnx` | `granite-docling-258M` | — |
| `Candle` | `paddleocr-vl-1.6` | `paddleocr-vl-1.5` |

`supported_engines()` gates each arm on its feature the way
`EmbeddingEngine::supported_engines()` does: `Onnx` behind a new
`recognize-onnx` feature, `Candle` behind `candle`. A build with neither
compiles and reports an empty catalogue; `build_analyzer` on an enabled-but-
uncatalogued recognizer is an error, never a silent disable — the existing rule.

### 5.2 The catalogue

```rust
pub struct RecognizerDescriptor {
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    pub is_default: bool,
    pub is_cached: bool,
    pub footprint_bytes: u64,
    pub admission_threshold: f32,
    pub emits: RegionKinds,
}
```

`list_models(engine, model_dir)` returns these, sorted default-first then
cached-first then by id — the same ordering `embed::engines::dispatch::list_models`
applies, and for the same reason. `recognizer_capabilities(model_dir)` walks
`supported_engines()` and returns a `RecognizerCapabilityManifest { engines,
models }` in `types.rs`.

`footprint_bytes` is static per model, known before installation, as it is
today. That is what lets the terms and the size be disclosed where the download
is offered.

### 5.3 Region kinds — the one type change

`SpottedRegion` is `{ text, confidence, quad }`. That is sufficient for a
spotting response and insufficient for a DocTags one, which distinguishes a
paragraph from a formula's LaTeX, a table's OTSL and a picture's region.
Flattening DocTags into `text` would discard exactly what the model was chosen
for.

```rust
pub enum RegionKind { Text, Formula, Table, Chart, Code }

pub struct SpottedRegion {
    pub kind: RegionKind,
    pub text: String,       // LaTeX for Formula, OTSL for Table, else the reading
    pub confidence: f32,
    pub quad: [Point; 4],
}
```

**`emits` is a property of (model × task configuration), not of the weights.**
PaddleOCR-VL is a document parser — it recognizes tables to HTML, formulas to
LaTeX and charts, each behind its own task prompt. Wilkes drives it with
`SPOTTING_PROMPT = "Spotting:"`, which returns text instances with
quadrilaterals. So the honest catalogue entry is:

| model | task configuration | emits |
|---|---|---|
| `paddleocr-vl-1.6` | `spotting-v2` | `Text` |
| `paddleocr-vl-1.6` | `parsing-v1` | `Text`, `Formula`, `Table`, `Chart` |
| `granite-docling-258M` | `doctags-v1` | `Text`, `Formula`, `Table`, `Chart`, `Code` |

Corrected 2026-08-29, from what was built: this row first read `Picture` and
`Caption` too. A caption is prose and is read as `Text` — a separate kind for
it would be caption *association*, which FIGURE.md defers — and a picture
region is a region with nothing recognized in it, which the reading has no use
for. Neither is a `RegionKind`, so neither may be advertised as one. A
`<picture>` element is what `regions_unroutable` counts: marked out by the
model, and named by nothing in this build.

The difference is one of integration, not capability, and the table must not be
read as "Paddle cannot do tables". `parsing-v1` is
[FIGURE.md](../FIGURE.md)'s 2026-08-29 amendment: PaddleOCR-VL's
`Formula Recognition:`, `Table Recognition:` and `Chart Recognition:` prompts
driven beside `Spotting:`, with a layout detector routing regions to them.
It is a distinct task configuration with its own recipe identity, so a library
read under `spotting-v2` is untouched by its arrival.

The two engines reach the same kinds by different means, and the difference is
worth stating because it is the strongest argument for the ONNX default:
granite-docling self-classifies in one decode, so it needs no router at all,
while `parsing-v1` needs a detection graph and one decode per routed region.
Neither is a fallback for the other; the recipe names which produced a reading.

Because `emits` is keyed on the task configuration, it already lives inside the
identity string via `EXTRACTION_SETTINGS_VERSION`. Nothing new needs to be
recorded.

### 5.4 The admission threshold moves

`ADMISSION_THRESHOLD` leaves `paddleocr_vl` and becomes
`RecognizerDescriptor::admission_threshold`, per model. `paddleocr-vl-1.6`
keeps `0.70`, so its identity string is unchanged and no existing library
re-reads.

granite-docling's threshold is **not** to be guessed, copied from Paddle, or
defaulted. It is a real operating point over a real corpus, and §9 makes
producing it a prerequisite rather than a follow-up. Shipping a default
recognizer with an unswept threshold would be choosing a number and calling it
a measurement.

### 5.5 Inventory and install

`RecognizerInventory` moves from `paddleocr_vl` to `types.rs` unchanged in
shape. `inventory(engine, model_id)` and `install(engine, model_id, model_dir,
progress)` join `dispatch`, and the three API signatures name the moved type.
The `#[cfg(feature = "candle")]` wrappers in `extract/image/mod.rs`
(`recognizer_installed`, `recognizer_inventory`, `install_recognizer`) lose
their gate and take `(engine, model_id)`; the feature gate belongs on the
engine arm inside `dispatch`, which is where `embed::engines::dispatch` puts it.

granite-docling's inventory has three artifacts rather than one, each with the
`.onnx` graph and its `.onnx_data` sidecar. `Checkpoint::artifacts()` already
returns a list, so this is data, not a structural change.

### 5.6 The ONNX runner

One model-independent runner, `extract/image/onnx_vlm.rs`, over three sessions:

| graph | inputs → outputs |
|---|---|
| `embed_tokens` | `input_ids` → `inputs_embeds` |
| `vision_encoder` | `pixel_values`, `pixel_attention_mask` → `image_features` |
| `decoder_model_merged` | `inputs_embeds`, `attention_mask`, `past_key_values.{i}.{key,value}` → `logits`, `present.{i}.{key,value}` |

This is Optimum's export convention, not a granite-docling one; Qwen2-VL-2B and
Florence-2 export the same three graph names with the same KV contract.

**The runner discovers its shape; it does not hardcode it.** Layer count, KV
arity and whether `position_ids` is an input are read from `session.inputs` at
load. This is not speculative generality — granite-docling's decoder takes no
`position_ids` and has 30 KV layers, Qwen2-VL's takes one (mrope) and has 28.
A runner that assumes either is a runner that only ever runs one model, which
is the thing this whole design is trying to stop.

Two constraints:

- **Load by path.** Weights live in `.onnx_data` sidecars, so
  `commit_from_file` is required; `commit_from_memory` cannot resolve them.
- **`ort` is pinned to `=2.0.0-rc.11` by fastembed 5.13.** Bump fastembed to
  ≥ 5.17.4 first, which pins `=2.0.0-rc.13` (ONNX Runtime 1.28), because rc.12
  moved everything in `ort::tensor` to `ort::value` and writing against rc.11
  paths means rewriting them. rc.13 also makes a requested execution provider
  that no prebuilt binary carries a **link-time** error rather than a silent
  CPU fallback — which is the behaviour this codebase wants, but it interacts
  with the existing `fastembed-coreml` feature and must be verified on the
  macOS target before the bump lands.

Per-model, behind a small trait: preprocessing, prompt construction, and the
output parser. Everything above is shared.

### 5.7 granite-docling specifics

Weights: `onnx-community/granite-docling-258M-ONNX`.

| set | vision | embed | decoder | total | measured on the fixture page |
|---|---|---|---|---|---|
| fp32 | 374.0 | 231.2 | 658.1 | **1263.3 MB** | correct; full table; stops cleanly at 338 tokens |
| int8 vision only | 93.9 | 57.8 | 658.1 | 809.8 MB | structure right, characters broken |
| int8 | 93.9 | 57.8 | 166.2 | 317.9 MB | drops words, loops to the cap, never reaches the table |
| fp16 | 187.0 | 115.6 | 329.1 | 631.7 MB | degenerate `!!!!`, NaN log-probabilities |
| q4f16 | 54.8 | 115.6 | 93.4 | 263.8 MB | not run; a WebGPU target |

**Ship fp32. This reverses what this section said before it was run.** The
earlier text picked int8 at 318 MB, reasoning that fp16 is emulated on the CPU
provider. The fp16 reasoning was right — it is worse than emulated, it diverges
into NaN — and the int8 conclusion was wrong.

The damage is attributable, which is why all four rows are here rather than a
verdict. The int8 **vision encoder** corrupts characters while keeping the
layout ("Exper t Sy s t e m s in Pr a c t i c e"); the int8 **decoder** loops
and omits whole elements. int8 also spent *more* wall-clock than fp32, because
looping to the token cap costs more than its faster steps save. There is no
speed argument left to trade quality against.

So this recognizer's size claim is **1.26 GB against PaddleOCR-VL's 1.9 GB** —
a much weaker argument than this spec was originally written on, and the honest
one. What 1.26 GB buys is formulas, tables and figure regions in a single pass
with no layout model in front of it, and that case does not rest on the
footprint at all.

One trap in the file listing survives the change: `embed_tokens_q4` is 231.2 MB,
byte-identical to fp32, because the embedding table is not 4-bit quantized. A
`_q4` suffix does not mean smaller.

Preprocessing, from `preprocessor_config.json` — every value is pinned, none is
a default to be inferred at runtime:

- RGB; resize so the longest edge is 2048; LANCZOS (`resample: 1`).
- Split into 512×512 tiles plus one global 512 thumbnail
  (`do_image_splitting: true`).
- Rescale by 1/255; normalize with mean and std 0.5 on all three channels.
- 64 visual tokens per tile (`image_seq_len: 64`, `scale_factor: 4`).

A full-page 2048-edge image is up to 17 tiles → ~1088 visual tokens. **That
prefill, not the 258M parameters, is what dominates wall-clock on a laptop
CPU.** The tile budget is a deliberate setting with a measured cost, not a
consequence of the longest-edge default, and it belongs in the task
configuration id because changing it changes the reading.

Decoder facts (`config.json`): `LlamaForCausalLM`, hidden 576, 30 layers, 9
heads / 3 KV, head_dim 64, rope_theta 100000, vocab 100352, tied embeddings.
Vision: siglip2-base, hidden 768, 12 layers, patch 16, image 512,
`gelu_pytorch_tanh`, eps 1e-6. `image_token_id` 100270, bos 100264, eos 100257.
`<end_of_utterance>` is added-token id 100352 — one past the declared vocab
size, which the sampler must tolerate rather than treat as out of range.

Parser: DocTags. The structural tokens are in the base vocabulary —
`<doctag>`, `<text>`, `<formula>`, `<otsl>` with `<fcel>`/`<ched>`/`<ecel>`/`<nl>`
cells, `<picture>`, `<chart>`, `<code>`, `<caption>` (94 angle-bracket tokens in
total). Location tokens are **not** in the vocabulary, so coordinates arrive as
ordinary text and the parser must lex them rather than match token ids the way
`parse_spotting` does. Their range is to be established empirically against the
model before the parser is written, not assumed to be `LOC_MAX`'s 0–1000 grid.

`parse_doctags` inherits `parse_spotting`'s discipline exactly: emission order
is reading order and is preserved; an element whose location is absent or
malformed is **dropped, not guessed at**; confidence is the mean probability of
the tokens spelling the element, from the decode's own log-probabilities.

## 6. Settings

`ImageAnalysisSettings` gains two fields:

```rust
#[serde(default)] pub engine: RecognitionEngine,
#[serde(default)] pub model: Option<RecognizerModel>,  // None → engine.default_model()
```

`build_analyzer` reads them instead of calling `RecognitionEngine::default()`.

**The migration hazard is real and must be handled explicitly.** `#[serde(default)]`
on `engine` resolves an existing settings file — written before the field
existed — to `Onnx`. For a user with image analysis enabled and 1.9 GB of
PaddleOCR-VL installed, that silently swaps their recognizer, changes their
recipe, and re-reads and re-embeds every document with a picture in it. That is
the precise outcome `ImageAnalysisSettings`' own doc comment says the design
exists to prevent.

So: a one-time settings migration writes `engine: "candle"`, `model:
"paddleocr-vl-1.6"` into any persisted configuration that predates the field
**and** has `enabled: true`. New installations, and existing ones with image
analysis off, take the `Onnx` default. Absent that migration this change is a
silent re-index for existing users and must not ship.

## 7. Recipe and re-extraction

`identity` remains `engine + model + weights digest + task configuration +
admission threshold`, so switching recognizer changes the recipe and re-reads —
the existing, correct behaviour. Two obligations follow:

- `paddleocr-vl-1.6`'s identity string must be **byte-identical** before and
  after this change. Moving the threshold out of a module constant must not
  perturb it. A test asserts the exact string.
- Switching recognizer in settings must route through the same re-extraction
  path as switching embedder, and must be presented to the user with the same
  weight. It is not a preference; it is a re-read of the corpus.

## 8. Test obligations

- `paddleocr-vl-1.6`'s identity string is unchanged, asserted literally.
- Every catalogued model states a footprint and an admission threshold, and the
  default model of every supported engine is present in its catalogue — the
  direct analogue of the existing
  `every_catalogued_model_names_its_dimension_and_a_hand_added_one_does_not`.
- The runner recovers KV arity and the presence of `position_ids` from a
  session's declared inputs, rather than from a constant.
- `parse_doctags` drops an element with a malformed or absent location and
  preserves emission order, mirroring `parse_spotting`'s tests.
- A settings file predating the `engine` field, with analysis enabled,
  deserializes to `Candle` + `paddleocr-vl-1.6`; one with analysis disabled
  takes the `Onnx` default.
- The inventory names every file the installer writes, for all three
  granite-docling artifacts and their sidecars — the existing
  "installs files the inventory does not name" check, extended.

## 9. Phases

**Phase 0 — measure before building. Done, but not over Ollama.** The plan was
to borrow the existing Ollama door as a harness. In the event the ONNX path was
cheap enough to stand up directly, so the measuring was done against the real
graphs: a Python reference over `onnxruntime` established the tiling, the prompt
expansion, the DocTags shape and the golden output, and the Rust port is checked
against it. That is a better harness than Ollama would have been — it measures
what ships rather than a proxy for it — and it is what produced both the
precision decision in §5.7 and the threshold in §5.4.

The one thing Ollama would still be good for is a wider corpus sweep without
installing weights per machine. The threshold rests on two pages, which is a
band rather than a sweep; widening it is the outstanding work named in §5.4.

**Phase 1 — the boundary, one engine.** Bump fastembed for ort rc.13. Move
`RecognizerInventory` to `types.rs` and the threshold onto a per-model
descriptor. Introduce the catalogue, the manifest and the settings fields with
`Candle` still the only engine and `paddleocr-vl-1.6` still the default. Land
the settings migration. Nothing about any reading changes, and the identity test
proves it.

**Phase 2 — the ONNX engine.** The runner, granite-docling's preprocessor and
DocTags parser, `RegionKind` on `SpottedRegion`, the catalogue entry. `Onnx`
becomes `#[default]`. Existing users stay on Paddle by the Phase 1 migration.

**Phase 3 — the picker.** Surface the manifest where the embedder's picker
already is, with the same re-index warning.

Phases 1 and 2 each leave the tree coherent. Phase 1 shipped alone is a
boundary with one engine behind it, which is a defensible state. Phase 2 shipped
without Phase 1 is a second engine reaching around a boundary that does not yet
exist, which is not.

## 10. What this does not settle

- **Driving PaddleOCR-VL's other task prompts** (table, formula, chart) is
  specified in [FIGURE.md](../FIGURE.md) as `parsing-v1`, not here. §5.3 makes
  it expressible and the two documents must stay agreed on one thing: `emits`
  is keyed on the task configuration, so neither document may state a model's
  kinds without naming the configuration alongside. The `ort` pin is the other
  shared surface — §5.6 moves it to rc.13 and FIGURE.md's routing detector runs
  on whatever it lands at. One version, not two.
- **Which areas of a page reach a recognizer.** Amended 2026-08-29: a PDF's
  embedded rasters are no longer the only ones. Formula and ruled-table areas
  the page *typesets* are marked out from the document's own typography and
  rendered for the recognizer — see FIGURE.md, "Phase 3 — Native vector tables
  and formulas". That is a decision of the PDF extractor and not of this
  document, and it reaches an engine as pixels like anything else. It touches
  this spec at two points and no others: the routing version joins the analyzer
  identity in the extraction recipe, so changing what is routed re-reads the
  library exactly as changing the engine does; and admission gained a second
  native-glyph rule, because a typeset region's bytes go into the reading in
  place of glyphs the page drew and only a formula, a table or a chart is worth
  displacing them for.
- **The tile budget as a user-facing setting.** §5.7 fixes it at the config's
  2048 longest edge for now. It is the dominant cost and it belongs in the task
  configuration id, so exposing it later is a recipe change, not a preference.
- **q4f16 under CoreML.** Rejected for the CPU default on reasoning, not on
  measurement. Revisiting it requires a benchmark, not an argument.
