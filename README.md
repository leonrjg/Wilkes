[img]

## Wilkes
Perform exact or semantic search across multiple PDFs and text files, with highlights.
This project aims to provide a plug-and-play, cross-platform solution for local document search.

### Features
- Document viewer with highlighted matches
- **Local semantic search**: data is embedded using open-source models, no cloud
  - You can choose from a set of predefined models or any HuggingFace model
- Fully configurable: adjust embedding chunk size and overlap, or just use the default settings
- Cross-platform: works on Windows, Linux, macOS, and the web

### Installation
#### Desktop

#### Docker
Docker allows you to run software in isolation from your system.

If you're concerned about security due to the project's recency, this is highly recommended.


### Why? | Similar software
- [Recoll](https://www.recoll.org/) is complex and has no first-party PDF support
- [Clapgrep](https://github.com/luleyleo/clapgrep) is good but only for Linux
- [Docfetcher](https://docfetcher.sourceforge.io/) (Free) doesn't show highlights
- [Baloo](https://github.com/kde/baloo) is only for Linux
- [Open Notebook](https://github.com/lfnovo/open-notebook) requires setting up Ollama and has no exact search
- [Semantra](https://github.com/freedmand/semantra) has no exact search and is unmaintained
- [Semantic](https://github.com/Bklieger/Semantic) is single-file and unmaintained
- Most others are terminal-based

### Technology
This app can be run on desktop or online, thanks to [Tauri](https://tauri.app/).

Because semantic search accuracy is on the eye of the beholder, it maximizes model variety by supporting multiple engines:
- Fastembed (Default)
- [Sentence Transformers](https://www.sbert.net/) (SBERT) through a Python sidecar
  - This has the widest variety of models, but you need to have Python installed.
  - The environment needed to run the models is automatically set up by the app.
- Candle

### Q&A
- What model should I use?
  - The app uses `e5-small-v2` by default. Also, you will see a few models marked as "Recommended" and a brief description of them.
    - You can also consult the [MTEB ranking](https://huggingface.co/spaces/mteb/leaderboard) and use any model from that list (through specific engines).
      - Note: the top 10 models of the ranking are too large to run on consumer hardware.
- 

### Roadmap
Since the core functionality is in place, I'll actively focus on bug fixes and UX improvements.

If you have feature requests, you're welcome to open an issue.

### Contributing
Contributions are welcome! Please fork the repository and submit a pull request with your changes.

### License
Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.