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
      <p>Perform exact or semantic search across multiple PDFs and text files, with highlights.</p>
      <p>This project aims to provide a <strong>plug-and-play</strong>, cross-platform solution for local semantic search.</p>
    </td>
  </tr>
</table>

## Features
- Document viewer with match highlighting
- **Local semantic search**: uses open-source embedding models; no cloud
  - You can choose from a set of predefined models or any HuggingFace model
- Fully configurable: adjust embedding chunk size and overlap, or just use the default settings
- Cross-platform: works on Windows, Linux, and macOS
- Web version

## Installation
### Desktop
- macOS: [wilkes-aarch64-apple-darwin](https://github.com/leonrjg/Wilkes/releases)
- Windows: [wilkes-x86_64-pc-windows-msvc](https://github.com/leonrjg/Wilkes/releases)
- Linux: [wilkes-x86_64-unknown-linux-gnu](https://github.com/leonrjg/Wilkes/releases)

### Docker
Docker allows you to run software in isolation from your system.

```shell
docker run --rm -p 2000:2000 -v wilkes-data:/data ghcr.io/leonrjg/wilkes:latest
# Now you can visit localhost:2000
```

If you want to build locally instead:

```shell
git clone https://github.com/leonrjg/Wilkes
cd Wilkes
docker compose up
# Now you can visit localhost:2000
```

### Demo
You can visit https://demo.wilkes.app to use the app online with your own sample files.
The demo is reset every hour.

## Why? | Similar software
- [Recoll](https://www.recoll.org/) is complex and has no first-party PDF support
- [Clapgrep](https://github.com/luleyleo/clapgrep) is good but only for Linux
- [Docfetcher](https://docfetcher.sourceforge.io/) (Free) doesn't show highlights
- [Baloo](https://github.com/kde/baloo) is only for Linux
- [Open Notebook](https://github.com/lfnovo/open-notebook) requires setting up Ollama and has no exact search
- [Semantra](https://github.com/freedmand/semantra) has no exact search and is unmaintained
- [Semantic](https://github.com/Bklieger/Semantic) is single-file and unmaintained
- [File-Brain](https://github.com/Hamza5/file-brain) looks pretty good, actually - I found this later :)
- Most others are terminal-based

## Engines
<img height="500" alt="Engine selection" src="https://github.com/user-attachments/assets/e61e5260-ff16-49a3-95fe-4fecf4e6ff5a" />

The app supports multiple engines to maximize model availability:
- **Fastembed** (Default)
  - Default model: `all-miniLM-L6-v2-onnx` 
- [**Sentence Transformers**](https://www.sbert.net/) (SBERT) via Python
  - Default model: `e5-small-v2`
  - This has the widest variety of models, but you need to have Python installed. The environment is automatically set up by the app.
- **Candle**
  - Default model: `all-miniLM-L6-v2`

## Q&A
- What model should I use?
  - You can use those marked as "Recommended", try multiple, or just use the default model. You can check the [MTEB ranking](https://huggingface.co/spaces/mteb/leaderboard) and use any model from that list (through specific engines). Note that the top 10 of the ranking are too large to run on consumer hardware.

## External MCP

The desktop app can expose Wilkes' document and literature tools to regular
Claude Code and Codex sessions. In **Settings → Chat → External MCP**, enable
the server, choose its bind address and port, and use the copy buttons for the
client you want to configure. The bind address defaults to `127.0.0.1`; use a
specific LAN address or a wildcard such as `0.0.0.0`/`::` only when remote
clients need access. Bearer authentication is optional and disabled by default.

The endpoint:

- listens on the configured IPv4 or IPv6 address;
- can require a persistent bearer token stored separately from `settings.json`;
- stays available while Wilkes is running, even when its chat pane is closed;
- reports the document and PDF page currently visible in Wilkes through
  `list_context`, even when its chat pane is closed;
- reads only documents inside the current, favorite, or recent Wilkes library
  roots.

The active document reported by `list_context` is informational. External
clients must continue to pass an explicit document path to `get_document_text`,
`get_related_documents`, and `get_file_metadata`.

Binding beyond loopback makes the endpoint reachable from the corresponding
network interfaces. Without token authentication, anyone who can reach the
endpoint can use its tools. Enable the token or restrict access with host
firewall rules where appropriate. When using `0.0.0.0` or `::`, remote clients
must replace the wildcard in the copied endpoint with the Wilkes machine's
reachable address. Disabling authentication keeps an existing token file so it
can be reused if authentication is enabled again.

The in-app chat continues to use its own private, session-scoped MCP server.
Changing the external bind address or port, or rotating its token, does not
affect open Wilkes chat sessions.
 
## Interface
<img width="1312" height="903" alt="image" src="https://github.com/user-attachments/assets/d2018970-5270-467d-9cc5-49a00ad5fa53" />
<img width="1263" height="845" alt="SCR-20260410-sbcv" src="https://github.com/user-attachments/assets/8967b389-ad7b-4c2b-8724-c3cd4959b8be" />
<img width="1263" height="845" alt="SCR-20260410-sjcn" src="https://github.com/user-attachments/assets/bd843c52-45c7-4559-bfa9-8adc0849d9b3" />
<img width="1312" height="903" alt="SCR-20260410-slfi" src="https://github.com/user-attachments/assets/c87bf249-63ba-4b07-b0dd-b6dbdf53257a" />
<img width="1312" height="903" alt="SCR-20260410-smdp" src="https://github.com/user-attachments/assets/62178660-77c3-4708-a817-dd59166934cf" />
<img width="1312" height="903" alt="SCR-20260410-slzb" src="https://github.com/user-attachments/assets/9e71a66c-04cc-4b19-98b5-c491089febc1" />
<img width="1268" height="859" alt="SCR-20260410-sllg" src="https://github.com/user-attachments/assets/9c91e67d-654d-4f1d-aaf0-5a735546b3dc" />


## Roadmap
- Workspaces (virtual folders) rather than 1:1 folder mapping
  - Drag-and-drop files  
- Support for EPUB, MOBI, FB2, XPS, CBZ
- Support for images
- HTML viewer
- Office documents

If you have feature requests, feel free to open an issue (or a PR).

## Changelog
### 0.9.5 - 2026-04-20

#### Added

- Document metadata extraction (DOI, author, date).
- External links (Google Scholar) on viewer.
- Context menu.

## Contributing
<img alt="Coverage" src="https://img.shields.io/badge/coverage-82%25-green" />
Contributions are welcome! Please fork the repository and submit a pull request with your changes.

## License
Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
