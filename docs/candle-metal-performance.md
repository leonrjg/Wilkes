# Candle Metal Performance Investigation

Investigation into why embedding 305 chunks from a 200KB text file with `all-MiniLM-L6-v2` (86MB, 6-layer BERT, 384-dim) takes ~40 seconds on an M1 Mac (16GB).

## Environment

- Apple M1, 16GB unified memory
- candle-core 0.9.2
- Metal GPU active, F32 dtype
- `accelerate-src` linked (Apple Accelerate.framework for CPU BLAS)

## Findings

### Metal `forward()` is the bottleneck

Instrumented timing per batch of 32 chunks:

| Phase | Time | Notes |
|---|---|---|
| Tokenize | 0.003–0.009s | Fast, not a factor |
| `model.forward()` (GPU synced) | 2.3–6.3s | **Dominates runtime** |
| Pool + normalize + download | 0.001–0.003s | Negligible after fix |

Forward time scales with seq_len squared (attention), ranging from 2.5s at seq_len=304 to 6.3s at seq_len=512. Total for 305 chunks across 10 batches: **~42 seconds**.

This is ~40x slower than the M1 GPU's theoretical peak (~2.6 TFLOPS FP32). The overhead is internal to candle's Metal backend — likely per-operation command buffer dispatch rather than batched submission.

### CPU + Accelerate is no faster

Forcing `Device::Cpu` with the `accelerate` feature (AMX coprocessor via Accelerate.framework) yielded **~38 seconds** — essentially the same wall time. The bottleneck shifts from Metal dispatch overhead to candle's matmul efficiency for the small matrices in multi-head attention (each head operates on 350x32 dimensions, too small to saturate AMX).

### F16 on Metal causes silent CPU fallbacks

Original configuration used F16 dtype on Metal. Timing showed the forward pass completing in 0.002s (async command submission only) while pool+normalize took 3–6 seconds. Switching to F32 and syncing after forward revealed the true GPU time.

F16 was slower (75s total vs 42s with F32) because candle 0.9 lacks F16 Metal kernels for some operations (likely layer norm, softmax, GELU). Missing kernels silently copy tensors GPU to CPU, run with pure Rust kernels, and copy back — dozens of times per forward pass.

### Broadcast ops in mean pooling lacked Metal kernels

Original mean pooling used `broadcast_as` + element-wise multiply + `sum_keepdim`. These broadcast operations fell back to CPU on Metal, adding seconds of overhead per batch. Replacing the pattern with an equivalent matmul dropped pool time from seconds to ~0.001s.

## Changes made

### 1. F32 dtype on Metal (`candle.rs: select_dtype`)

Switched from F16 to F32 for all devices. Eliminates silent CPU fallbacks from missing F16 Metal kernels.

### 2. Accelerate feature (`core/Cargo.toml`)

Added `candle-core/accelerate` to the `metal` feature set. Any operations that do fall back to CPU now use Apple's BLAS instead of candle's pure Rust kernels.

### 3. Matmul-based mean pooling (`candle.rs: pool`)

Before:
```rust
let mask_expanded = attention_mask.unsqueeze(2)?
    .broadcast_as(token_embeddings.shape())?;
let masked_sum = (token_embeddings * &mask_expanded)?.sum_keepdim(1)?;
let count = attention_mask.sum_keepdim(1)?.unsqueeze(2)?;
masked_sum.broadcast_div(&count)?.squeeze(1)?
```

After:
```rust
let mask_row = attention_mask.unsqueeze(1)?;           // (B, 1, S)
let masked_sum = mask_row.matmul(token_embeddings)?;   // (B, 1, H)
let count = attention_mask.sum_keepdim(1)?.unsqueeze(2)?; // (B, 1, 1)
masked_sum.broadcast_div(&count)?.squeeze(1)?          // (B, H)
```

Mathematically equivalent. The matmul has a guaranteed Metal kernel; the broadcast multiply did not.

### 4. Batch size reduced to 32 (`candle.rs: EMBED_BATCH_SIZE`)

Reduced from 512 to 32 to limit intermediate tensor memory (attention matrices scale with batch x seq_len^2). Minimal throughput impact since per-operation overhead dominates.

## Options for further improvement

| Option | Expected speedup | Effort | Trade-off |
|---|---|---|---|
| Increase `WINDOW_CHARS` (larger chunks) | ~2x | Minimal | Coarser search granularity |
| ONNX Runtime (`ort`) with CoreML backend | ~5–10x | Moderate | New dependency, model export |
| Smaller/distilled model (2-layer) | ~3x | Minimal | Lower embedding quality |

The fundamental constraint is candle 0.9's Metal command dispatch pattern. Until candle batches Metal command buffers, GPU utilization will remain low for multi-layer transformer models. ONNX Runtime with CoreML is the most promising path — it would route inference through Apple's Neural Engine, which is purpose-built for this workload.
