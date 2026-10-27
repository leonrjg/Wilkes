// Runs at the start of every page in the browser pane, right after Dark
// Reader's API build. The shell then calls `__wilkesDarkMode(theme)` for the
// page: a theme darkens it with Dark Reader's dynamic mode, unless the page
// turns out to have a dark theme of its own; null leaves it as the site made
// it.
//
// The dark-theme check is the one the Dark Reader extension runs ("Detect dark
// theme", `runDarkThemeDetector` in its inject script, MIT): the API build
// does not include it. Dark Reader is enabled first, so a light page is never
// shown light for longer than it takes to style it, and switched off again if
// the page is dark without it.
(() => {
  const DarkReader = window.DarkReader;
  // The page has no business finding a second Dark Reader in its globals.
  delete window.DarkReader;
  if (!DarkReader) return;
  // Cross-origin stylesheets are read the way the page itself could read them.
  DarkReader.setFetchMethod((url) => window.fetch(url));

  const COLOR_SCHEME_META_SELECTOR = 'meta[name="color-scheme"]';

  const parseColor = (value) => {
    const parts = value.match(/[\d.]+/g);
    if (!parts || parts.length < 3) return null;
    const [r, g, b, a = 1] = parts.map(Number);
    return { r, g, b, a };
  };
  const lightness = ({ r, g, b }) => (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;

  function hasBuiltInDarkTheme() {
    const rootStyle = getComputedStyle(document.documentElement);
    const rootColor = parseColor(rootStyle.backgroundColor);
    if (!rootColor) return false;
    const bodyColor = document.body
      ? parseColor(getComputedStyle(document.body).backgroundColor)
      : { r: 0, g: 0, b: 0, a: 0 };
    if (!bodyColor) return false;
    if (rootColor.a === 0 && bodyColor.a === 0) {
      const rootTextColor = parseColor(rootStyle.color);
      if (rootTextColor) return lightness(rootTextColor) > 0.5;
    }
    const rootLightness = 1 - rootColor.a + rootColor.a * lightness(rootColor);
    const finalLightness = (1 - bodyColor.a) * rootLightness + bodyColor.a * lightness(bodyColor);
    return finalLightness < 0.5;
  }

  function isDark() {
    const meta = document.querySelector(COLOR_SCHEME_META_SELECTOR);
    if (meta) {
      const content = meta.content.toLocaleLowerCase();
      if (content === "dark" || content === "only dark") return true;
      if (content === "light" || content === "only light") return false;
    }
    if (
      document.documentElement.classList.contains("dark") ||
      document.body?.classList.contains("dark") ||
      document.documentElement.dataset.theme?.toLocaleLowerCase() === "dark"
    ) {
      return true;
    }
    // Judge the page as the site made it, with Dark Reader's sheets off.
    const sheets = Array.from(document.styleSheets)
      .filter((s) => s.ownerNode?.classList.contains("darkreader"))
      .concat(
        Array.isArray(document.adoptedStyleSheets)
          ? document.adoptedStyleSheets.filter((s) => s.cssRules?.[0]?.selectorText?.startsWith("#__darkreader"))
          : [],
      );
    sheets.forEach((s) => (s.disabled = true));
    const dark = hasBuiltInDarkTheme();
    sheets.forEach((s) => (s.disabled = false));
    return dark;
  }

  function hasSomeStyle() {
    if (document.querySelector(COLOR_SCHEME_META_SELECTOR)) return true;
    if (document.documentElement.style.backgroundColor || document.body?.style.backgroundColor) return true;
    for (const sheet of document.styleSheets) {
      if (sheet?.ownerNode && !sheet.ownerNode.classList?.contains("darkreader")) return true;
    }
    return false;
  }

  // Whether the page has enough of itself in place to be judged.
  function canCheck() {
    const body = document.body;
    if (!(body && body.scrollHeight >= 32 && body.clientHeight >= 32 && body.childElementCount > 0 && hasSomeStyle())) {
      return false;
    }
    return Array.from(body.children).some((c) => !["SCRIPT", "STYLE", "LINK"].includes(c.tagName));
  }

  let stopDetecting = () => {};

  function detect(onDark) {
    stopDetecting();
    const check = () => {
      stopDetecting();
      if (isDark()) onDark();
    };
    if (canCheck()) {
      check();
      return;
    }
    const observer = new MutationObserver(() => {
      if (canCheck()) check();
    });
    observer.observe(document.documentElement, { childList: true });
    const onReadyState = () => {
      if (document.readyState === "complete") check();
    };
    document.addEventListener("readystatechange", onReadyState);
    stopDetecting = () => {
      observer.disconnect();
      document.removeEventListener("readystatechange", onReadyState);
      stopDetecting = () => {};
    };
  }

  // The shell calls this when the page commits and again when it finishes
  // loading; the second call with the same theme changes nothing.
  let applied;
  Object.defineProperty(window, "__wilkesDarkMode", {
    value: (theme) => {
      const key = JSON.stringify(theme);
      if (key === applied) return;
      applied = key;
      stopDetecting();
      if (!theme) {
        DarkReader.disable();
        return;
      }
      DarkReader.enable({
        darkSchemeBackgroundColor: theme.background,
        darkSchemeTextColor: theme.text,
      });
      detect(() => DarkReader.disable());
    },
  });
})();
