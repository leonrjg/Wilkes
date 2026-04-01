# Adding Candle as an Embedding Backend

## Motivation

fastembed has two problems:
- Small, static model list (~20 models)
- No runtime compatibility guarantee — models in the list can fail with ONNX Runtime errors
- ONNX Runtime on macOS runs on bare CPU cores; it cannot use Metal or the Neural Engine

Candle (HuggingFace's Rust ML framework) replaces fastembed with broader model support, no ONNX dependency, and Metal acceleration on Apple Silicon.

## Scope

A new `CandleEmbedder` struct implementing the existing `Embedder` trait, living alongside `FastEmbedder` in `crates/core/src/embed/`. No changes needed outside that module. fastembed remains as feature-gated dead code during transition.

## What needs implementing

**Model loading**
- Download `config.json`, `tokenizer.json`, and `model.safetensors` from HF Hub (reusing `hf-hub`, already a transitive dep)
- Load weights into candle tensors

**Device selection**
- Use `Device::new_metal(0)` on Apple Silicon (GPU acceleration via Metal)
- Fall back to `Device::Cpu` on non-Metal systems
- Candle on CPU is slower than ORT; Metal is the reason to switch

**Tokenization**
- Use the `tokenizers` crate (HuggingFace's Rust tokenizer) — loads directly from `tokenizer.json`
- Handles BPE, WordPiece, etc. without per-model code

**Inference**
- `candle-transformers` ships a ready-made BERT implementation; use it for encoder-based models
- Forward pass → token embeddings → pooling → L2 normalize

**Pooling strategy**
- Read `1_Pooling/config.json` if present (sentence-transformers format) to determine mean/CLS/max
- For models without this file, fall back to mean pooling as default, with an override table for known exceptions

## Model list

Replace the static fastembed list with a curated set of known-working encoder models. Include the fastembed defaults so existing users don't need to re-download:
- `BAAI/bge-base-en-v1.5` (encoder, currently default)
- `sentence-transformers/all-MiniLM-L6-v2` (encoder, fastembed default)
- `intfloat/multilingual-e5-large-instruct` (560M, encoder)
- `jinaai/jina-embeddings-v5-text-small` (~300M, encoder)
- `jinaai/jina-embeddings-v5-text-nano` (~100M, encoder)

Users could also supply an arbitrary HF model ID; correctness of pooling for unknown models is not guaranteed.

## What stays the same

- `Embedder` trait and all callsites
- Tauri commands
- UI
- Installer / download progress plumbing (only the download target files change)
