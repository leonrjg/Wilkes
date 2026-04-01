# Spec: Semantic Settings & Engine Switching

This document defines the UI and UX for allowing users to switch between embedding engines (Candle vs. Fastembed) and manage their semantic search configuration.

## Overview

Users need a way to choose their preferred inference engine. 
- **Candle (Metal):** Native Rust implementation, uses GPU via Metal. Best for pure-Rust stability but currently limited by framework overhead.
- **Fastembed (ONNX):** Uses ONNX Runtime with CoreML acceleration. Significantly faster (~5-10x) but introduces heavier binary dependencies.

## UI Design: Settings Modal

A new "Settings" icon will be added to the sidebar or top header. Clicking it opens a modal with a "Semantic Search" tab.

### 1. Engine Selection
- **Radios/Segmented Control:** [ Candle ] [ Fastembed (ONNX) ]
- **Description Text:** Dynamic help text explaining the trade-offs (Speed vs. Native overhead).
- **Behavior:** Switching engines filters the "Available Models" list. If the current model is not supported by the new engine, the selection is cleared.

### 2. Model Management
- **Unified List:** Shows models available for the *active* engine.
- **Status Indicators:** 
    - `Cached`: Model files are on disk.
    - `Not Downloaded`: Needs a download before use.
- **Download/Delete:** Inline actions to manage local disk space.

### 3. State Transitions
- **Engine Change:** If an index is already built with Engine A, and the user switches to Engine B, the UI must show a warning: *"The existing index was built with [Engine A]. You must rebuild the index to use [Engine B]."*
- **Active Task Protection:** Disallow engine/model switching while a download or index build is in progress.

## Persistence

The choice of engine will be stored in `settings.json` under the `semantic` object:

```json
{
  "semantic": {
    "engine": "fastembed", // or "candle"
    "model": "BAAI/bge-base-en-v1.5",
    "enabled": true
  }
}
```
