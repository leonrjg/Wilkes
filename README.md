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
- Document viewer with highlighted matches
- **Local semantic search**: data is embedded using open-source models, no cloud
  - You can choose from a set of predefined models or any HuggingFace model
- Fully configurable: adjust embedding chunk size and overlap, or just use the default settings
- Cross-platform: works on Windows, Linux, and macOS
- Web version

## Installation
### Desktop
TBD

### Docker
Docker allows you to run software in isolation from your system.

```shell
git clone https://github.com/leonrjg/Wilkes
cd Wilkes
docker compose up
# Now you can visit localhost:2000
```


## Why? | Similar software
The app's purpose is to fulfill my failed Google query for 'search multiple pdfs github'. I tried these at the time:
- [Recoll](https://www.recoll.org/) is complex and has no first-party PDF support
- [Clapgrep](https://github.com/luleyleo/clapgrep) is good but only for Linux
- [Docfetcher](https://docfetcher.sourceforge.io/) (Free) doesn't show highlights
- [Baloo](https://github.com/kde/baloo) is only for Linux
- [Open Notebook](https://github.com/lfnovo/open-notebook) requires setting up Ollama and has no exact search
- [Semantra](https://github.com/freedmand/semantra) has no exact search and is unmaintained
- [Semantic](https://github.com/Bklieger/Semantic) is single-file and unmaintained
- [File-Brain](https://github.com/Hamza5/file-brain) looks pretty good, actually - I found this later :)
- Most others are terminal-based

## Details
<img height="500" alt="Engine selection" src="https://github.com/user-attachments/assets/54335881-c380-4efd-940b-ddf26af7c0f9" />

### Engines
The accuracy of each model is on the eye of the beholder, so the app maximizes model variety by supporting multiple engines:
- Fastembed (Default)
  - Default model: `all-miniLM-L6-v2-onnx` 
- [Sentence Transformers](https://www.sbert.net/) (SBERT) via Python
  - Default model: `e5-small-v2`
  - This has the widest variety of models, but you need to have Python installed. The environment is automatically set up by the app.
- Candle
  - Default model: `all-miniLM-L12-v2`

## Q&A
- What model should I use?
  - You can use those marked as "Recommended", try multiple, or just use the default one. You can check the [MTEB ranking](https://huggingface.co/spaces/mteb/leaderboard) and use any model from that list (through specific engines). Note that the top 10 models of the ranking are too large to run on consumer hardware.
 
## Other images
<img width="1312" height="903" alt="image" src="https://github.com/user-attachments/assets/d2018970-5270-467d-9cc5-49a00ad5fa53" />
<img width="1312" height="903" alt="image" src="https://github.com/user-attachments/assets/ce61fbda-bcdf-41af-acbb-1315073e721a" />


## Roadmap
Since the core functionality is in place, I'll actively focus on bug fixes and UX improvements.

If you have feature requests, feel free to open an issue (or a PR).

### Contributing
Contributions are welcome! Please fork the repository and submit a pull request with your changes.

### License
Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
