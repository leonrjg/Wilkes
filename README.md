<p align="center">
  <img style="width:48%" alt="Light mode" src="https://github.com/user-attachments/assets/8b84516f-5384-47d2-bb5e-ce2dd78e5b18" />
  <img style="width:48%" alt="Dark mode" src="https://github.com/user-attachments/assets/6c03a671-1a2a-42c5-b5ab-0cd6a66030bf" />
</p>

<table align="center">
  <tr>
    <td valign="top">
      <img height="200" alt="Wilkes" src="https://github.com/user-attachments/assets/d64c15a4-4aad-4cc6-ba20-d243dbb0c21f" />
    </td>
    <td valign="top">
      <h1>Wilkes</h1>
      <p>Read, organize and search your document library, with exact and local semantic search.</p>
      <p>Work with PDFs, ebooks and text files in separate workspaces on desktop or the web.</p>
    </td>
  </tr>
</table>

**Development version: 1.0.0-rc.1.** See [GitHub Releases](https://github.com/leonrjg/Wilkes/releases) for published builds and [the changelog](CHANGELOG.md) for candidate highlights and breaking changes.

## Features

- **Search your library:** exact search with regular expressions and case-sensitive matching, local semantic search, or combined results with evidence labels and passage highlights.
- **Read documents:** PDF and ebook readers, Markdown and HTML views, text and code viewers, multiple tabs, saved reading positions, bookmarks, notes and highlights.
- **Organize your research:** separate workspaces with multiple library folders, tags, smart collections, search history, related passages, topic clouds and a citation graph.
- **Recognize scanned content:** optional OCR with layout detection and separate readers for page regions, formulas and tables.
- **Find literature:** search OpenAlex and Semantic Scholar, add custom providers, sync learning catalogues, download available documents and work with Zotero.
- **Use local models:** run embeddings locally and generate summaries, cited search answers and topic labels with Candle or a configured Ollama server. Text documents also support library-grounded inline completions.
- **Connect an agent:** desktop chat can use the active document and attached files. HTTP and MCP interfaces expose document, search and research tools to other applications.
- **Choose desktop or web:** desktop packages for macOS, Windows and Linux, plus a web server distributed through Docker.

## Installation

### Desktop

Download the package for your platform from [GitHub Releases](https://github.com/leonrjg/Wilkes/releases).

| Platform | Supported architecture | Packages |
| --- | --- | --- |
| macOS | Apple Silicon (ARM64) | DMG |
| Windows | x86-64 | EXE installer, MSI, portable ZIP |
| Linux | x86-64 | DEB, AppImage |

Signing status is recorded in each release. Candidate builds appear as prereleases.

### Docker

The server image supports Linux amd64 and arm64. To run the published stable image:

```sh
docker run --rm -p 127.0.0.1:2000:2000 -v wilkes-data:/data ghcr.io/leonrjg/wilkes:latest
```

Open [localhost:2000](http://localhost:2000). The named volume keeps application data between runs. For a published candidate, replace `latest` with its full version tag, such as `1.0.0-rc.1`.

To build from the checked-out source:

```sh
git clone https://github.com/leonrjg/Wilkes
cd Wilkes
docker compose up --build
```

## First run

1. On desktop, add a folder to the current workspace or drop it onto the window. In the web app, upload documents into the workspace.
2. Start with exact search. Searching text already present in documents does not require an embedding or generation model. Scanned pages need OCR before their text can be searched.
3. For semantic search, open **Settings → Semantic → Models**, choose an engine and model, download it if needed, and select **Build semantic index** for the current library folder. Adding or switching folders does not start a build automatically.
4. Once indexing finishes, use semantic or combined search. Repeat the index build for additional library folders you want to search semantically.
5. If you need scanned text, formulas or tables, configure **Settings → Extraction → OCR** before indexing. Install the required models, enable OCR, and rebuild affected documents. Track progress, failures and interrupted work under **Settings → Workers**.

Workspaces keep their library folders, indexes, research data and saved sessions separate. Use the workspace picker to create or switch workspaces.

## Document formats

Wilkes reads PDF, EPUB, MOBI, AZW3, FB2, CBZ and CBT documents, along with configured text formats such as plain text, Markdown, HTML, source code, CSV and JSON. Supported paged document formats are always enabled; **Settings → File extensions** controls text formats.

AZW3 layout is approximate and internal Kindle links are unavailable. Comic archives and image-only pages require OCR for text search. Markdown and HTML have rendered and source views.

## Models and optional dependencies

Exact search and document reading work without model downloads. Models are downloaded when you choose to install them; their download sizes are shown in settings.

| Feature | What you need |
| --- | --- |
| Semantic search with Fastembed (default) | A downloaded embedding model; no separate Python installation |
| Semantic search with Candle | A downloaded supported embedding model; no separate Python installation |
| Semantic search with Sentence Transformers (SBERT) | Python installed; Wilkes sets up its worker environment |
| OCR | A configured reader; download any required recognizer and layout models |
| Generation with Candle | A downloaded supported generation model |
| Generation with Ollama | A running, reachable Ollama server with the chosen model installed |
| Desktop chat | A separately installed and configured supported agent CLI; tool access depends on the backend |
| Literature, catalogues and Zotero | The selected service or local application, plus credentials where required |

Fastembed and Candle default to MiniLM-L6; SBERT defaults to `intfloat/e5-small-v2`. Choose a listed model first; custom model support depends on the engine and architecture.

OCR is off by default. **Use general reader** also defaults to off, and **PP-FormulaNet_plus-S** is the default formula reader when no formula model is saved. Existing explicit reader choices are preserved. Settings show the effective readers and any missing setup.

Embedding models and built-in Candle generation run locally. Literature services, a configured Ollama endpoint, and chat backends connect to the services you choose.

## HTTP and MCP

On desktop, **Settings → Servers** controls the HTTP API and external MCP listener. The standalone web server also provides MCP at `/mcp`. Clients can search documents, read passages and manage research data through these interfaces.

Desktop chat supports Claude Code, Codex and Nanocoder backends. Wilkes currently attaches its MCP tools to Claude Code and Codex; Nanocoder chat does not receive those tools through ACP.

## Interface

<img width="1312" height="903" alt="image" src="https://github.com/user-attachments/assets/d2018970-5270-467d-9cc5-49a00ad5fa53" />
<img width="1263" height="845" alt="SCR-20260410-sbcv" src="https://github.com/user-attachments/assets/8967b389-ad7b-4c2b-8724-c3cd4959b8be" />
<img width="1263" height="845" alt="SCR-20260410-sjcn" src="https://github.com/user-attachments/assets/bd843c52-45c7-4559-bfa9-8adc0849d9b3" />
<img width="1312" height="903" alt="SCR-20260410-slfi" src="https://github.com/user-attachments/assets/c87bf249-63ba-4b07-b0dd-b6dbdf53257a" />
<img width="1312" height="903" alt="SCR-20260410-smdp" src="https://github.com/user-attachments/assets/62178660-77c3-4708-a817-dd59166934cf" />
<img width="1312" height="903" alt="SCR-20260410-slzb" src="https://github.com/user-attachments/assets/9e71a66c-04cc-4b19-98b5-c491089febc1" />
<img width="1268" height="859" alt="SCR-20260410-sllg" src="https://github.com/user-attachments/assets/9c91e67d-654d-4f1d-aaf0-5a735546b3dc" />

If you have feature requests, feel free to open an issue (or a PR).

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for release history, candidate notes and breaking changes.

## Contributing

Contributions are welcome! Please fork the repository and submit a pull request with your changes.

For release builds, supported artifacts, signing configuration and candidate tags, see [Releasing](docs/releasing.md).

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
