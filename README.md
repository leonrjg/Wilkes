<p align="center">
  <img height="300" alt="SCR-20260407-qnvr" src="https://github.com/user-attachments/assets/dfb5a41a-5d50-4af6-bb7e-554703c102ff" />
  <img height="300" alt="SCR-20260407-qnre" src="https://github.com/user-attachments/assets/682d1cef-a2e4-480b-a5df-a5f596a48b2b" />
</p>

# Wilkes
Perform exact or semantic search across multiple PDFs and text files, with highlights.
This project aims to provide a plug-and-play, cross-platform solution for local document search.

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
- [Recoll](https://www.recoll.org/) is complex and has no first-party PDF support
- [Clapgrep](https://github.com/luleyleo/clapgrep) is good but only for Linux
- [Docfetcher](https://docfetcher.sourceforge.io/) (Free) doesn't show highlights
- [Baloo](https://github.com/kde/baloo) is only for Linux
- [Open Notebook](https://github.com/lfnovo/open-notebook) requires setting up Ollama and has no exact search
- [Semantra](https://github.com/freedmand/semantra) has no exact search and is unmaintained
- [Semantic](https://github.com/Bklieger/Semantic) is single-file and unmaintained
- Most others are terminal-based

## Details
### Engines
Because semantic search accuracy is on the eye of the beholder, it maximizes model variety by supporting multiple engines:
- Fastembed (Default)
- [Sentence Transformers](https://www.sbert.net/) (SBERT) via Python
  - This has the widest variety of models, but you need to have Python installed. The environment is automatically set up by the app.
- Candle

## Q&A
- What model should I use?
  - The app uses `e5-small-v2` by default. You'll also see a few models marked as "Recommended".
    - You can also check the [MTEB ranking](https://huggingface.co/spaces/mteb/leaderboard) and use any model from that list (through specific engines). Note that the top 10 models of the ranking are too large to run on consumer hardware.

## Roadmap
Since the core functionality is in place, I'll actively focus on bug fixes and UX improvements.

If you have feature requests, feel free to open an issue.

### Contributing
Contributions are welcome! Please fork the repository and submit a pull request with your changes.

### License
Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
