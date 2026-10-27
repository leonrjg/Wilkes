// Darkens a light page in the browser pane, the way Dark Reader's dynamic mode
// does it rather than by inverting the page: read the page's stylesheets and
// write each colour again for a dark page -- backgrounds dark, text light,
// borders in between, images untouched. A small port of that idea (Dark
// Reader, MIT), not of Dark Reader: no per-site fixes, no gradients or
// shadows, no frames.
//
// Evaluated by the shell on every page load, then `__wilkesDarkMode(theme)`:
// a theme ({background, text}) darkens the page unless it has a dark theme of
// its own; null puts the page back as its site made it. Installs once per
// page; later evaluations only call it.
(() => {
  if (window.__wilkesDarkMode) return;

  // ── Colours ────────────────────────────────────────────────────────────────

  // An authored colour value, as {r, g, b, a}; `a` is a number or, for an
  // alpha written as an expression (Tailwind's `rgb(255 255 255 /
  // var(--tw-bg-opacity))`), that expression, kept as written.
  let probe = null;
  function parseColor(value) {
    const text = value.trim().toLowerCase();
    if (!text || text === "transparent" || text === "currentcolor" || text === "inherit" || text === "initial" || text === "unset") return null;
    let m = text.match(/^#([0-9a-f]{3,8})$/);
    if (m) {
      let hex = m[1];
      if (hex.length === 3 || hex.length === 4) hex = [...hex].map((c) => c + c).join("");
      if (hex.length !== 6 && hex.length !== 8) return null;
      const n = (i) => parseInt(hex.slice(i, i + 2), 16);
      return { r: n(0), g: n(2), b: n(4), a: hex.length === 8 ? n(6) / 255 : 1 };
    }
    m = text.match(/^rgba?\(\s*([\d.]+%?)[\s,]+([\d.]+%?)[\s,]+([\d.]+%?)\s*(?:[,/]\s*(.+?))?\s*\)$/);
    if (m) {
      const channel = (c) => (c.endsWith("%") ? (parseFloat(c) * 255) / 100 : parseFloat(c));
      return { r: channel(m[1]), g: channel(m[2]), b: channel(m[3]), a: alpha(m[4]) };
    }
    m = text.match(/^hsla?\(\s*([\d.]+)(?:deg)?[\s,]+([\d.]+)%[\s,]+([\d.]+)%\s*(?:[,/]\s*(.+?))?\s*\)$/);
    if (m) {
      return { ...fromHSL({ h: parseFloat(m[1]), s: parseFloat(m[2]) / 100, l: parseFloat(m[3]) / 100 }), a: alpha(m[4]) };
    }
    if (!/^[a-z]+$/.test(text)) return null;
    // A named colour: the canvas knows them all.
    probe ??= document.createElement("canvas").getContext?.("2d") ?? false;
    if (!probe) return null;
    probe.fillStyle = "#010203";
    probe.fillStyle = text;
    return probe.fillStyle === "#010203" ? null : parseColor(probe.fillStyle);
  }

  function alpha(raw) {
    if (raw === undefined) return 1;
    if (/^[\d.]+%$/.test(raw)) return parseFloat(raw) / 100;
    if (/^[\d.]+$/.test(raw)) return parseFloat(raw);
    return raw;
  }

  function toHSL({ r, g, b }) {
    r /= 255; g /= 255; b /= 255;
    const max = Math.max(r, g, b), min = Math.min(r, g, b);
    const l = (max + min) / 2;
    if (max === min) return { h: 0, s: 0, l };
    const d = max - min;
    const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    const h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    return { h: h * 60, s, l };
  }

  function fromHSL({ h, s, l }) {
    const k = (n) => (n + h / 30) % 12;
    const a = s * Math.min(l, 1 - l);
    const f = (n) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
    return { r: Math.round(f(0) * 255), g: Math.round(f(8) * 255), b: Math.round(f(4) * 255) };
  }

  function format({ r, g, b, a }) {
    const rgb = `${Math.round(r)} ${Math.round(g)} ${Math.round(b)}`;
    return a === 1 ? `rgb(${rgb})` : `rgb(${rgb} / ${a})`;
  }

  const scale = (x, inLow, inHigh, outLow, outHigh) => outLow + ((x - inLow) * (outHigh - outLow)) / (inHigh - inLow);

  // The transforms, after Dark Reader's modifyBackgroundColor,
  // modifyForegroundColor and modifyBorderColor: lightness is mapped onto the
  // dark page's range, hue kept; a near-grey takes the theme's own hue, so
  // greys land on the app's background and text rather than a neutral one.
  const MAX_BG_LIGHTNESS = 0.4;
  let poles = null;

  function darkBackground(hsl) {
    const neutral = hsl.s < 0.12;
    const l = hsl.l < 0.5 ? scale(hsl.l, 0, 0.5, poles.bg.l, MAX_BG_LIGHTNESS) : scale(hsl.l, 0.5, 1, MAX_BG_LIGHTNESS, poles.bg.l);
    return neutral ? { h: poles.bg.h, s: poles.bg.s, l } : { ...hsl, l };
  }

  function darkForeground(hsl) {
    if (hsl.l > 0.5) {
      // Already light: kept, so text on a coloured button stays readable.
      return hsl;
    }
    const l = scale(hsl.l, 0, 0.5, poles.text.l, 0.55);
    if (hsl.s < 0.12) return { h: poles.text.h, s: poles.text.s, l };
    // Saturated blues need more light to read on a dark page.
    const isBlue = hsl.h > 200 && hsl.h < 280;
    return { ...hsl, l: isBlue ? Math.max(l, 0.7) : l };
  }

  function darkBorder(hsl) {
    const l = scale(hsl.l, 0, 1, 0.5, 0.2);
    return hsl.s < 0.12 ? { h: poles.bg.h, s: poles.bg.s, l } : { ...hsl, l };
  }

  const ROLES = { background: darkBackground, foreground: darkForeground, border: darkBorder };

  function modify(value, role) {
    const color = parseColor(value);
    if (!color) return null;
    return format({ ...fromHSL(ROLES[role](toHSL(color))), a: color.a });
  }

  // ── Stylesheets ────────────────────────────────────────────────────────────

  const PROPERTIES = {
    "background-color": "background",
    color: "foreground",
    fill: "foreground",
    stroke: "foreground",
    "caret-color": "foreground",
    "text-decoration-color": "foreground",
    "border-top-color": "border",
    "border-right-color": "border",
    "border-bottom-color": "border",
    "border-left-color": "border",
    "outline-color": "border",
    "column-rule-color": "border",
  };
  const VAR_USE = /(?:(hsla?|rgba?)\(\s*)?var\(\s*(--[\w-]+)/g;

  // Cross-origin sheets hide their rules from the page; their text is fetched
  // the way the page itself could fetch it, and read from a sheet of our own.
  const fetched = new Map();

  function rulesOf(sheet) {
    try {
      return sheet.cssRules;
    } catch {
      const href = sheet.href;
      if (!href) return null;
      if (!fetched.has(href)) {
        fetched.set(href, null);
        fetch(href, { credentials: "omit" })
          .then((response) => (response.ok ? response.text() : Promise.reject(response.status)))
          .then((text) => {
            const copy = new CSSStyleSheet();
            copy.replaceSync(text);
            fetched.set(href, copy.cssRules);
            schedule();
          })
          .catch(() => {});
      }
      return fetched.get(href);
    }
  }

  // Every style rule of the page's sheets, with the conditions it sits in.
  function* styleRules(rules, conditions = []) {
    for (const rule of rules ?? []) {
      if (rule.style && rule.selectorText) {
        yield { rule, conditions };
      } else if (rule.cssRules) {
        const condition = rule.conditionText === undefined ? null
          : typeof CSSSupportsRule !== "undefined" && rule instanceof CSSSupportsRule ? `@supports ${rule.conditionText}`
          : typeof CSSContainerRule !== "undefined" && rule instanceof CSSContainerRule ? `@container ${rule.conditionText}`
          : `@media ${rule.conditionText}`;
        if (rule.styleSheet) yield* styleRules(rulesOf(rule.styleSheet), conditions);
        else yield* styleRules(rule.cssRules, condition ? [...conditions, condition] : conditions);
      } else if (rule.styleSheet) {
        yield* styleRules(rulesOf(rule.styleSheet), conditions);
      }
    }
  }

  function pageSheets() {
    return Array.from(document.styleSheets).filter((sheet) => !OWN.has(sheet.ownerNode?.id));
  }

  // Colour variables, after Dark Reader's variable handling: a variable is
  // never darkened in place, because one token can be read as text in one
  // rule and as a background in another. Each role it is read in gets its own
  // derived variable (`--x` → `--wilkes-bg--x`, `--wilkes-text--x`), and each
  // read is pointed at the one for its role. A variable that reads others
  // (`--a: var(--b)`) passes its roles on to them. One read inside
  // `hsl(var(--x))` or `rgb(var(--x))` holds bare channels (shadcn's
  // `--background: 0 0% 100%`), and its derived variables keep that form.
  const ROLE_NAMES = { background: "bg", foreground: "text", border: "border" };
  const derived = (name, role) => `--wilkes-${ROLE_NAMES[role]}${name}`;
  const SHORTHANDS = {
    background: "background",
    border: "border",
    "border-top": "border",
    "border-right": "border",
    "border-bottom": "border",
    "border-left": "border",
    "border-color": "border",
    outline: "border",
  };

  function addTo(map, key, value) {
    if (!map.has(key)) map.set(key, new Set());
    const set = map.get(key);
    if (set.has(value)) return false;
    set.add(value);
    return true;
  }

  // Every colour-bearing read of a variable, and every variable declaration.
  function variables(entries) {
    const uses = new Map();
    const channels = new Map();
    const reads = new Map();
    const declarations = [];
    const noteReads = (value, role, from) => {
      for (const [, wrapper, name] of value.matchAll(VAR_USE)) {
        if (wrapper) channels.set(name, wrapper.startsWith("hsl") ? "hsl" : "rgb");
        if (role) addTo(uses, name, role);
        if (from) addTo(reads, from, name);
      }
    };
    for (const { rule } of entries) {
      const style = rule.style;
      for (let i = 0; i < style.length; i++) {
        const property = style[i];
        const value = style.getPropertyValue(property);
        if (property.startsWith("--")) {
          declarations.push([property, value]);
          if (value.includes("var(")) noteReads(value, null, property);
        } else if (PROPERTIES[property] && value.includes("var(")) {
          noteReads(value, PROPERTIES[property], null);
        }
      }
      for (const [shorthand, role] of Object.entries(SHORTHANDS)) {
        const value = style.getPropertyValue(shorthand);
        if (value.includes("var(")) noteReads(value, role, null);
      }
    }
    for (let changed = true; changed; ) {
      changed = false;
      for (const [from, names] of reads) {
        for (const name of names) {
          for (const role of uses.get(from) ?? []) changed = addTo(uses, name, role) || changed;
          if (channels.has(from) && !channels.has(name)) {
            channels.set(name, channels.get(from));
            changed = true;
          }
        }
      }
    }
    return { uses, channels, declarations };
  }

  // A variable's bare channels as a colour, and a colour back as channels.
  function parseChannels(value, form) {
    const parts = value.trim().split(/[\s,]+/);
    if (parts.length !== 3) return null;
    return parseColor(form === "hsl" ? `hsl(${parts.join(" ")})` : `rgb(${parts.join(" ")})`);
  }

  function formatChannels(color, form) {
    if (form === "rgb") return `${color.r} ${color.g} ${color.b}`;
    const { h, s, l } = toHSL(color);
    return `${+h.toFixed(1)} ${+(s * 100).toFixed(1)}% ${+(l * 100).toFixed(1)}%`;
  }

  function modifyChannels(value, role, form) {
    const color = form && parseChannels(value, form);
    return color ? formatChannels(fromHSL(ROLES[role](toHSL(color))), form) : modify(value, role);
  }

  // Point each read of a variable that has a derived one for `role` at it.
  function rewrite(value, role, emitted) {
    const out = value.replace(/var\(\s*(--[\w-]+)/g, (read, name) =>
      emitted.has(`${role}|${name}`) ? `var(${derived(name, role)}` : read,
    );
    return out === value ? null : out;
  }

  // What a variable's derived one holds for `role`: its colour darkened, or
  // its reads pointed at their own derived variables.
  function derive(name, value, role, current) {
    return modifyChannels(value, role, current.channels.get(name)) ?? (value.includes("var(") ? rewrite(value, role, current.emitted) : null);
  }

  // Which derived variables exist anywhere, so that no read is pointed at one
  // that does not: a chain is followed until nothing more can be derived.
  function emittedVariables({ uses, channels, declarations }) {
    const current = { channels, emitted: new Set() };
    for (let changed = true; changed; ) {
      changed = false;
      for (const [name, value] of declarations) {
        for (const role of uses.get(name) ?? []) {
          const key = `${role}|${name}`;
          if (current.emitted.has(key) || !derive(name, value, role, current)) continue;
          current.emitted.add(key);
          changed = true;
        }
      }
    }
    return current.emitted;
  }

  let current = { uses: new Map(), channels: new Map(), emitted: new Set() };

  function declarationsFor(style) {
    let out = "";
    const emit = (property, value, priority) => {
      out += `${property}: ${value}${priority ? " !important" : ""}; `;
    };
    for (let i = 0; i < style.length; i++) {
      const property = style[i];
      const value = style.getPropertyValue(property);
      if (!value) continue;
      const priority = style.getPropertyPriority(property);
      if (property.startsWith("--")) {
        for (const role of current.uses.get(property) ?? []) {
          if (!current.emitted.has(`${role}|${property}`)) continue;
          const modified = derive(property, value, role, current);
          if (modified) emit(derived(property, role), modified, priority);
        }
        continue;
      }
      const role = PROPERTIES[property];
      if (!role) continue;
      // A colour that only takes its alpha from a variable is still a colour.
      const modified = modify(value, role) ?? (value.includes("var(") ? rewrite(value, role, current.emitted) : null);
      if (modified) emit(property, modified, priority);
    }
    for (const [shorthand, role] of Object.entries(SHORTHANDS)) {
      const value = style.getPropertyValue(shorthand);
      if (!value.includes("var(")) continue;
      const modified = rewrite(value, role, current.emitted);
      if (modified) emit(shorthand, modified, style.getPropertyPriority(shorthand));
    }
    return out;
  }

  function generate() {
    const entries = [];
    for (const sheet of pageSheets()) entries.push(...styleRules(rulesOf(sheet)));
    const found = variables(entries);
    current = { uses: found.uses, channels: found.channels, emitted: emittedVariables(found) };
    let css = "";
    for (const { rule, conditions } of entries) {
      const declarations = declarationsFor(rule.style);
      if (!declarations) continue;
      let block = `${rule.selectorText} { ${declarations}}`;
      for (const condition of [...conditions].reverse()) block = `${condition} { ${block} }`;
      css += block + "\n";
    }
    return css;
  }

  // ── Inline styles ──────────────────────────────────────────────────────────
  // Rewritten in place, with what they said kept to put back. A derived
  // variable added to an element is put back by removing it.

  const inline = new Map();

  function darkenInline(element) {
    if (inline.has(element)) return;
    const style = element.style;
    const changes = [];
    for (let i = 0; i < style.length; i++) {
      const property = style[i];
      const value = style.getPropertyValue(property);
      const priority = style.getPropertyPriority(property);
      if (property.startsWith("--")) {
        for (const role of current.uses.get(property) ?? []) {
          const modified = derive(property, value, role, current);
          if (modified) changes.push([derived(property, role), "", priority, modified]);
        }
        continue;
      }
      const role = PROPERTIES[property];
      if (!role) continue;
      const modified = modify(value, role) ?? (value.includes("var(") ? rewrite(value, role, current.emitted) : null);
      if (modified) changes.push([property, value, priority, modified]);
    }
    if (changes.length === 0) return;
    inline.set(element, changes);
    for (const [property, , priority, modified] of changes) style.setProperty(property, modified, priority);
  }

  function restoreInline() {
    for (const [element, changes] of inline) {
      for (const [property, value, priority] of changes) element.style.setProperty(property, value, priority);
    }
    inline.clear();
  }

  // ── Applying ───────────────────────────────────────────────────────────────

  const DEFAULTS_ID = "wilkes-dark-defaults";
  const OVERRIDES_ID = "wilkes-dark-overrides";
  const OWN = new Set([DEFAULTS_ID, OVERRIDES_ID]);

  function ownStyle(id, first) {
    let style = document.getElementById(id);
    if (!style) {
      style = document.createElement("style");
      style.id = id;
    }
    // Defaults go before the page's styles, so the page's own rules (darkened)
    // win over them; overrides go after every style the page has, <body>'s
    // included, so they win over the page's.
    if (first) {
      const parent = document.head ?? document.documentElement;
      if (parent.firstChild !== style) parent.insertBefore(style, parent.firstChild);
    } else if (document.documentElement.lastChild !== style) {
      document.documentElement.appendChild(style);
    }
    return style;
  }

  let theme = null;
  let timer = 0;
  let observer = null;

  function render() {
    timer = 0;
    if (!theme) return;
    ownStyle(DEFAULTS_ID, true).textContent = `
:root { color-scheme: dark; background-color: ${theme.background}; color: ${theme.text}; }
:where(a:link) { color: ${modify("#0000ee", "foreground")}; }
:where(a:visited) { color: ${modify("#551a8b", "foreground")}; }
:where(input, textarea, select, button) { background-color: ${modify("#ffffff", "background")}; color: ${theme.text}; border-color: ${modify("#767676", "border")}; }`;
    ownStyle(OVERRIDES_ID, false).textContent = generate();
    document.querySelectorAll("[style]").forEach(darkenInline);
    // What was just written is this script's own doing, not the page's.
    observer?.takeRecords();
  }

  function schedule() {
    if (theme && !timer) timer = setTimeout(render, 50);
  }

  function watch() {
    observer = new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        const target = mutation.type === "characterData" ? mutation.target.parentNode : mutation.target;
        if (OWN.has(target?.id)) continue;
        if (target?.nodeName === "STYLE") return schedule();
        for (const node of mutation.addedNodes) {
          if (OWN.has(node.id)) continue;
          if (node.nodeName === "STYLE" || node.nodeName === "LINK") return schedule();
          if (node.nodeType === 1) {
            if (node.hasAttribute("style")) darkenInline(node);
            node.querySelectorAll?.("[style]").forEach(darkenInline);
          }
        }
      }
    });
    observer.observe(document.documentElement, { childList: true, subtree: true, characterData: true });
    document.addEventListener("load", onLoad, true);
  }

  function onLoad(event) {
    if (event.target.nodeName === "LINK") schedule();
  }

  function restore() {
    clearTimeout(timer);
    timer = 0;
    observer?.disconnect();
    observer = null;
    document.removeEventListener("load", onLoad, true);
    for (const id of OWN) document.getElementById(id)?.remove();
    restoreInline();
  }

  // ── A dark theme of its own ────────────────────────────────────────────────
  // The Dark Reader extension's "Detect dark theme" check (`runDarkThemeDetector`
  // in its inject script): judged with this script's styles off.

  const COLOR_SCHEME_META = 'meta[name="color-scheme"]';

  function lightness(color) {
    return (0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b) / 255;
  }

  function computedColor(value) {
    const color = parseColor(value);
    if (!color) return value.includes("rgba(0, 0, 0, 0)") || value === "transparent" ? { r: 0, g: 0, b: 0, a: 0 } : null;
    return { ...color, a: typeof color.a === "number" ? color.a : 1 };
  }

  function hasBuiltInDarkTheme() {
    const rootStyle = getComputedStyle(document.documentElement);
    const root = computedColor(rootStyle.backgroundColor);
    if (!root) return false;
    const body = document.body ? computedColor(getComputedStyle(document.body).backgroundColor) : { r: 0, g: 0, b: 0, a: 0 };
    if (!body) return false;
    if (root.a === 0 && body.a === 0) {
      const text = computedColor(rootStyle.color);
      if (text) return lightness(text) > 0.5;
    }
    const rootLightness = 1 - root.a + root.a * lightness(root);
    return (1 - body.a) * rootLightness + body.a * lightness(body) < 0.5;
  }

  function isDark() {
    const meta = document.querySelector(COLOR_SCHEME_META);
    if (meta) {
      const content = meta.content.toLowerCase();
      if (content === "dark" || content === "only dark") return true;
      if (content === "light" || content === "only light") return false;
    }
    if (
      document.documentElement.classList.contains("dark") ||
      document.body?.classList.contains("dark") ||
      document.documentElement.dataset.theme?.toLowerCase() === "dark"
    ) {
      return true;
    }
    const own = [...OWN].map((id) => document.getElementById(id)?.sheet).filter(Boolean);
    own.forEach((sheet) => (sheet.disabled = true));
    const saved = [...inline].flatMap(([element, original]) =>
      original.map(([property, value, priority, modified]) => {
        element.style.setProperty(property, value, priority);
        return () => element.style.setProperty(property, modified, priority);
      }),
    );
    const dark = hasBuiltInDarkTheme();
    saved.forEach((undo) => undo());
    own.forEach((sheet) => (sheet.disabled = false));
    return dark;
  }

  function canCheck() {
    const body = document.body;
    if (!(body && body.scrollHeight >= 32 && body.clientHeight >= 32 && body.childElementCount > 0)) return false;
    return Array.from(body.children).some((c) => !["SCRIPT", "STYLE", "LINK"].includes(c.tagName));
  }

  let stopDetecting = () => {};

  function detect(onDark) {
    stopDetecting();
    const check = () => {
      stopDetecting();
      if (isDark()) onDark();
    };
    if (canCheck()) return check();
    const watcher = new MutationObserver(() => canCheck() && check());
    watcher.observe(document.documentElement, { childList: true });
    const onReadyState = () => document.readyState === "complete" && check();
    document.addEventListener("readystatechange", onReadyState);
    stopDetecting = () => {
      watcher.disconnect();
      document.removeEventListener("readystatechange", onReadyState);
      stopDetecting = () => {};
    };
  }

  // ── The switch ─────────────────────────────────────────────────────────────

  let applied;
  Object.defineProperty(window, "__wilkesDarkMode", {
    value: (next) => {
      const key = JSON.stringify(next);
      if (key === applied) return;
      applied = key;
      stopDetecting();
      restore();
      theme = next;
      if (!theme) return;
      const bg = parseColor(theme.background);
      const text = parseColor(theme.text);
      if (!bg || !text) return;
      poles = { bg: toHSL(bg), text: toHSL(text) };
      render();
      watch();
      detect(() => {
        theme = null;
        restore();
      });
    },
  });
})();
