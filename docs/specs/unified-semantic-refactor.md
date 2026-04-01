# Specification: Unified Semantic Refactor

This document specifies the structural changes required to support multiple embedding engines (Candle, Fastembed, etc.) in a modular and extensible way.

## 1. Core Type Extensions (`crates/core/src/types.rs`)

To support engine-aware settings, we introduce the `EmbeddingEngine` enum.

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum EmbeddingEngine {
    #[default]
    Candle,
    Fastembed,
}
```

Update `SemanticSettings` to include the engine:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SemanticSettings {
    pub enabled: bool,
    pub engine: EmbeddingEngine, // New field
    pub model: EmbedderModel,
    pub index_path: Option<PathBuf>,
}
```

## 2. Dispatcher Architecture (`crates/core/src/embed/mod.rs`)

We will implement a stateless dispatcher rather than a complex factory to keep the implementation lean and testable.

### `EngineDispatcher`
A new internal struct/module that abstracts engine-specific logic:

- `list_models(engine, data_dir)`: Aggregates results from `candle::list_supported_models` or `fastembed::list_supported_models`.
- `get_installer(engine, model)`: Returns `Arc<dyn EmbedderInstaller>`.
- `fetch_model_size(engine, model_id)`: Dispatches to the appropriate HUB API logic.

### Modularity & Extensibility
To add a new engine (e.g., `LlamaCpp`):
1. Add a variant to `EmbeddingEngine`.
2. Implement the `Embedder` and `EmbedderInstaller` traits in a new file.
3. Add a match arm to the `EngineDispatcher`.

## 3. Index Integrity (`crates/core/src/embed/index.rs`)

The `SemanticIndex` must be "engine-aware" to prevent users from accidentally querying an index built with Fastembed using a Candle embedder (or vice-versa), which could lead to subtle embedding space mismatches.

- **Schema Update**: Add an `engine` key to the `meta` table.
- **Validation**: `SemanticIndex::open` must verify that the `stored_engine` matches the `current_engine`. If they mismatch, it returns an error prompting a rebuild.

## 4. API Layer Decoupling (`crates/api/src/commands/embed.rs`)

The API commands will be refactored to remove all direct imports of `wilkes_core::embed::candle`.

```rust
// Current
pub async fn list_models(data_dir: &Path) -> Vec<ModelDescriptor> {
    wilkes_core::embed::candle::list_supported_models(data_dir)
}

// Refactored
pub async fn list_models(engine: EmbeddingEngine, data_dir: &Path) -> Vec<ModelDescriptor> {
    wilkes_core::embed::dispatch::list_models(engine, data_dir)
}
```

## 5. Desktop Command Refactor (`crates/desktop/src/lib.rs`)

Tauri commands will now resolve the `engine` from the active `Settings` before performing operations.

- `download_model`: Takes `engine` as an argument from the UI.
- `build_index`: Takes `engine` as an argument.
- `restore_semantic_state`: Reads the `engine` from `settings.json` to correctly re-instantiate the `EmbedderInstaller` during startup.

## 6. UI: Settings & Logic (`ui/src/`)

### Settings Modal
A new `SettingsModal.tsx` component will handle global app configuration.
- Tab: "Semantic Search"
- Engine Toggle: Segmented control between "Candle (Metal)" and "Fastembed (ONNX)".
- Impact: Changing the engine updates the global settings state. `SemanticPanel` then filters its model list based on this setting.

### Semantic Panel Refresh
The `SemanticPanel` will be updated to:
1. Display only models supported by the currently selected engine.
2. Show a "Rebuild Required" state if the `engine` in settings differs from the `engine` stored in the `IndexStatus`.
