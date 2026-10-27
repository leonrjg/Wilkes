# Dark Reader (vendored)

`darkreader.js` is the API build of [Dark Reader](https://darkreader.org/)
4.9.133, unchanged, from the `darkreader` npm package
(`https://registry.npmjs.org/darkreader/-/darkreader-4.9.133.tgz`).
MIT licensed, see `LICENSE`.

The browser pane injects it into every page it loads
(`src/browser_pane.rs`, `src/browser_pane_dark.js`). It is compiled into the
desktop binary with `include_str!`, so updating it is replacing this file and
the version above.

sha256 `82619e7a0bcabbea15a91488bc74fd0f9ba8f8f2a1cc67527f78166037cce166`
