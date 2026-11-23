// OpenPen page crawler. Evaluated in the page by pen-import through CDP; it
// installs `window.__openpen` with the steps the Rust driver calls in order:
// `settle` / `prescroll` (load lazy content), `freeze` (finish animations),
// `prepare` (turn pseudo-elements into real nodes) and `crawl` (DOM snapshot).
// Everything here is written for OpenPen; it only reads the page, except for
// the reversible marks `prepare` and `crawl` leave behind for the capture.
(() => {
  if (window.__openpen) return true;

  const SKIP = new Set(['SCRIPT', 'STYLE', 'LINK', 'META', 'HEAD', 'NOSCRIPT', 'TEMPLATE',
    'TITLE', 'BASE', 'DATALIST', 'PARAM', 'SOURCE', 'TRACK', 'WBR', 'AREA', 'MAP', 'DIALOG']);
  const SHOT = new Set(['CANVAS', 'VIDEO', 'OBJECT', 'EMBED', 'AUDIO']);
  const NATIVE_INPUTS = new Set(['checkbox', 'radio', 'range', 'color', 'file', 'image']);
  const TEXT_INPUTS = new Set(['text', 'search', 'email', 'url', 'tel', 'password', 'number',
    'date', 'datetime-local', 'month', 'time', 'week', '']);
  const BUTTON_INPUTS = new Set(['submit', 'button', 'reset']);
  const NO_PSEUDO = new Set(['IMG', 'INPUT', 'SELECT', 'TEXTAREA', 'BR', 'VIDEO', 'IFRAME',
    'CANVAS', 'OBJECT', 'EMBED', 'AUDIO', 'HR', 'OPENPEN-PSEUDO']);

  // Computed properties copied into the snapshot. Values equal to the
  // listed default are omitted to keep the snapshot small.
  const STYLE_KEYS = {
    display: '', position: 'static', zIndex: 'auto', float: 'none',
    overflowX: 'visible', overflowY: 'visible', opacity: '1', visibility: 'visible',
    backgroundColor: 'rgba(0, 0, 0, 0)', backgroundImage: 'none', backgroundSize: 'auto',
    backgroundPosition: '0% 0%', backgroundRepeat: 'repeat', backgroundClip: 'border-box',
    borderTopWidth: '0px', borderRightWidth: '0px', borderBottomWidth: '0px', borderLeftWidth: '0px',
    borderTopStyle: 'none', borderRightStyle: 'none', borderBottomStyle: 'none', borderLeftStyle: 'none',
    borderTopColor: '', borderRightColor: '', borderBottomColor: '', borderLeftColor: '',
    borderTopLeftRadius: '0px', borderTopRightRadius: '0px',
    borderBottomRightRadius: '0px', borderBottomLeftRadius: '0px',
    outlineWidth: '0px', outlineStyle: 'none', outlineColor: '', outlineOffset: '0px',
    boxShadow: 'none', filter: 'none', backdropFilter: 'none', mixBlendMode: 'normal',
    clipPath: 'none', maskImage: 'none', webkitMaskImage: 'none',
    paddingTop: '0px', paddingRight: '0px', paddingBottom: '0px', paddingLeft: '0px',
    flexDirection: 'row', flexWrap: 'nowrap', justifyContent: 'normal', alignItems: 'normal',
    rowGap: 'normal', columnGap: 'normal',
    color: '', fontFamily: '', fontSize: '', fontWeight: '400', fontStyle: 'normal',
    lineHeight: 'normal', letterSpacing: 'normal', textAlign: 'start', textTransform: 'none',
    textDecorationLine: 'none', textDecorationColor: '', textShadow: 'none', whiteSpace: 'normal',
    webkitTextFillColor: '', textOverflow: 'clip', direction: 'ltr',
    objectFit: 'fill', objectPosition: '50% 50%',
  };
  const COLOR_KEYS = ['backgroundColor', 'borderTopColor', 'borderRightColor', 'borderBottomColor',
    'borderLeftColor', 'outlineColor', 'color', 'webkitTextFillColor', 'textDecorationColor'];
  const COMPOSITE_KEYS = ['backgroundImage', 'boxShadow', 'textShadow', 'filter', 'backdropFilter'];

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const frame = () => new Promise((r) => requestAnimationFrame(() => r()));

  // ---- color normalization (oklch / lab / color() ... -> rgba) ----------
  let colorCanvas = null;
  const colorCache = new Map();
  function toRgba(c) {
    if (!c || /^rgba?\(/.test(c) || c === 'transparent') return c;
    if (colorCache.has(c)) return colorCache.get(c);
    let out = c;
    try {
      if (!colorCanvas) {
        colorCanvas = document.createElement('canvas');
        colorCanvas.width = colorCanvas.height = 1;
      }
      const g = colorCanvas.getContext('2d', { willReadFrequently: true });
      g.clearRect(0, 0, 1, 1);
      g.fillStyle = '#000';
      g.fillStyle = c;
      g.fillRect(0, 0, 1, 1);
      const d = g.getImageData(0, 0, 1, 1).data;
      out = d[3] === 255 ? `rgb(${d[0]}, ${d[1]}, ${d[2]})`
        : `rgba(${d[0]}, ${d[1]}, ${d[2]}, ${+(d[3] / 255).toFixed(3)})`;
    } catch (_) { /* keep the original */ }
    colorCache.set(c, out);
    return out;
  }
  const COLOR_FN = /\b(?:oklch|oklab|lab|lch|hwb|hsla?|color)\([^()]*\)/g;
  const normalizeColors = (v) => (v && COLOR_FN.test(v) ? v.replace(COLOR_FN, (m) => toRgba(m)) : v);

  // ---- loading helpers ---------------------------------------------------
  function resourceCount() {
    try { return performance.getEntriesByType('resource').length; } catch (_) { return 0; }
  }
  function pendingImages() {
    let n = 0;
    for (const img of document.images) {
      if (!img.complete && img.getAttribute('loading') !== 'lazy') n++;
    }
    return n;
  }
  // Waits until no new resource is fetched for `quietMs` and images are done (bounded).
  async function settle(quietMs, maxMs) {
    const t0 = performance.now();
    let last = resourceCount();
    let since = performance.now();
    while (performance.now() - t0 < maxMs) {
      await sleep(100);
      const n = resourceCount();
      if (n !== last || pendingImages() > 0 || document.readyState !== 'complete') {
        last = n;
        since = performance.now();
      } else if (performance.now() - since >= quietMs) {
        break;
      }
    }
    try {
      await Promise.race([document.fonts.ready, sleep(3000)]);
    } catch (_) { /* ignore */ }
    return Math.round(performance.now() - t0);
  }

  function docHeight() {
    const d = document.documentElement;
    const b = document.body;
    return Math.max(d.scrollHeight, d.offsetHeight, b ? b.scrollHeight : 0, b ? b.offsetHeight : 0);
  }

  // Scrolls through the page so lazy loaders and observers fire, then back to the top.
  async function prescroll(maxMs) {
    for (const img of document.querySelectorAll('img[loading="lazy"]')) img.loading = 'eager';
    for (const f of document.querySelectorAll('iframe[loading="lazy"]')) f.loading = 'eager';
    const t0 = performance.now();
    const step = Math.max(200, Math.floor(window.innerHeight * 0.8));
    let y = 0;
    while (performance.now() - t0 < maxMs) {
      y += step;
      window.scrollTo(0, y);
      await frame();
      await sleep(120);
      if (y >= docHeight() - window.innerHeight) break;
    }
    window.scrollTo(0, docHeight());
    await sleep(200);
    window.scrollTo(0, 0);
    await frame();
    await sleep(100);
    return { height: docHeight(), ms: Math.round(performance.now() - t0) };
  }

  // Jumps finite animations to their end and pauses the infinite ones.
  function freeze() {
    let n = 0;
    try {
      for (const a of document.getAnimations()) {
        try { a.finish(); } catch (_) { try { a.pause(); } catch (_) { /* ignore */ } }
        n++;
      }
    } catch (_) { /* ignore */ }
    for (const v of document.querySelectorAll('video')) { try { v.pause(); } catch (_) { /* ignore */ } }
    window.scrollTo(0, 0);
    return n;
  }

  // ---- pseudo-elements -> real elements ------------------------------------
  function contentText(content) {
    // Concatenates the quoted strings of a computed `content` value.
    let out = '';
    const re = /"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'/g;
    let m;
    let any = false;
    while ((m = re.exec(content))) {
      any = true;
      out += (m[1] ?? m[2]).replace(/\\([0-9a-fA-F]{1,6})\s?/g,
        (_, h) => String.fromCodePoint(parseInt(h, 16))).replace(/\\(.)/g, '$1');
    }
    return any ? out : null;
  }

  let pseudoStyle = null;
  function prepare(scope) {
    const root = scope ? document.querySelector(scope) : document.body;
    if (!root) return 0;
    if (!pseudoStyle) {
      pseudoStyle = document.createElement('style');
      pseudoStyle.textContent =
        '[data-openpen-nb]::before{content:none!important}[data-openpen-na]::after{content:none!important}';
      document.head.appendChild(pseudoStyle);
    }
    const work = [];
    const all = [root, ...root.querySelectorAll('*')];
    for (const el of all) {
      if (NO_PSEUDO.has(el.tagName) || !(el instanceof HTMLElement)) continue;
      for (const which of ['::before', '::after']) {
        const cs = getComputedStyle(el, which);
        const content = cs.content;
        if (!content || content === 'none' || content === 'normal' || cs.display === 'none') continue;
        const text = contentText(content);
        const url = /url\(["']?([^"')]+)["']?\)/.exec(content);
        if (text === null && !url) continue;
        const decl = [];
        for (let i = 0; i < cs.length; i++) {
          const p = cs[i];
          if (p === 'content') continue;
          decl.push(`${p}:${cs.getPropertyValue(p)}`);
        }
        work.push({ el, which, text, url: url ? url[1] : null, css: decl.join(';') });
      }
    }
    // Insert after reading every style, so inserted nodes never affect a read.
    for (const w of work) {
      const span = document.createElement('openpen-pseudo');
      span.setAttribute('data-pseudo', w.which.slice(2));
      span.style.cssText = w.css;
      if (w.url) {
        const img = document.createElement('img');
        img.src = w.url;
        img.style.cssText = 'display:inline;width:auto;height:auto';
        span.appendChild(img);
      }
      if (w.text) span.appendChild(document.createTextNode(w.text));
      if (w.which === '::before') {
        w.el.setAttribute('data-openpen-nb', '');
        w.el.insertBefore(span, w.el.firstChild);
      } else {
        w.el.setAttribute('data-openpen-na', '');
        w.el.appendChild(span);
      }
    }
    return work.length;
  }

  // ---- crawl --------------------------------------------------------------
  function matrixOf(cs) {
    let m = null;
    try {
      const has = (v) => v && v !== 'none';
      if (has(cs.transform) || has(cs.translate) || has(cs.rotate) || has(cs.scale)) {
        m = new DOMMatrix();
        if (has(cs.translate)) {
          const p = cs.translate.split(/\s+/).map((v) => parseFloat(v) || 0);
          m = m.translate(p[0] || 0, p[1] || 0);
        }
        if (has(cs.rotate)) {
          const a = /(-?[\d.]+)(deg|rad|turn|grad)?\s*$/.exec(cs.rotate);
          if (a) {
            let deg = parseFloat(a[1]);
            if (a[2] === 'rad') deg = deg * 180 / Math.PI;
            else if (a[2] === 'turn') deg *= 360;
            else if (a[2] === 'grad') deg *= 0.9;
            m = m.rotate(deg);
          }
        }
        if (has(cs.scale)) {
          const s = cs.scale.split(/\s+/).map(parseFloat);
          m = m.scale(s[0], s.length > 1 ? s[1] : s[0]);
        }
        if (has(cs.transform)) m = m.multiply(new DOMMatrix(cs.transform));
        if (m.isIdentity) m = null;
      }
    } catch (_) { m = null; }
    if (!m) return null;
    const o = (cs.transformOrigin || '').split(/\s+/).map(parseFloat);
    return { m: [m.a, m.b, m.c, m.d, m.e, m.f].map((v) => +v.toFixed(5)), o: [o[0] || 0, o[1] || 0] };
  }

  function componentName(el) {
    try {
      const key = Object.keys(el).find((k) => k.startsWith('__reactFiber$'));
      if (key) {
        let f = el[key];
        for (let i = 0; f && i < 30; i++, f = f.return) {
          if (typeof f.type === 'function' || (f.type && typeof f.type === 'object')) {
            let c = f.child;
            while (c && typeof c.type !== 'string') c = c.child;
            if (!c || c.stateNode !== el) return null;
            const t = f.type;
            const name = t.displayName || t.name || (t.render && (t.render.displayName || t.render.name));
            if (name && /^[A-Z]/.test(name)) return name;
            continue;
          }
          if (typeof f.type === 'string' && f.stateNode !== el) return null;
        }
      }
      const vue = el.__vueParentComponent;
      if (vue && vue.subTree && vue.subTree.el === el && vue.type) {
        return vue.type.name || vue.type.__name || null;
      }
    } catch (_) { /* ignore */ }
    return null;
  }

  function markerText(cs, index) {
    const t = cs.listStyleType;
    switch (t) {
      case 'disc': return '•';
      case 'circle': return '◦';
      case 'square': return '▪';
      case 'decimal': return `${index}.`;
      case 'decimal-leading-zero': return `${index < 10 ? '0' : ''}${index}.`;
      case 'lower-alpha': case 'lower-latin': return `${String.fromCharCode(96 + ((index - 1) % 26) + 1)}.`;
      case 'upper-alpha': case 'upper-latin': return `${String.fromCharCode(64 + ((index - 1) % 26) + 1)}.`;
      case 'lower-roman': return `${roman(index).toLowerCase()}.`;
      case 'upper-roman': return `${roman(index)}.`;
      default: return null;
    }
  }
  function roman(n) {
    const t = [[1000, 'M'], [900, 'CM'], [500, 'D'], [400, 'CD'], [100, 'C'], [90, 'XC'], [50, 'L'],
      [40, 'XL'], [10, 'X'], [9, 'IX'], [5, 'V'], [4, 'IV'], [1, 'I']];
    let s = '';
    for (const [v, r] of t) while (n >= v) { s += r; n -= v; }
    return s;
  }

  let measureCanvas = null;
  function textWidth(text, cs) {
    if (!measureCanvas) measureCanvas = document.createElement('canvas');
    const g = measureCanvas.getContext('2d');
    g.font = `${cs.fontStyle} ${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
    return g.measureText(text).width;
  }

  async function crawl(opts) {
    const t0 = performance.now();
    const maxNodes = opts.maxNodes || 5000;
    const charBudget = { left: 300000 };
    const warnings = [];
    let count = 0;
    let truncated = false;
    const shots = [];
    window.__openpenShots = shots;
    const fontKeys = new Map();
    const fontProbes = [];
    window.__openpenFontProbes = fontProbes;
    const svgFetches = new Map();

    window.scrollTo(0, 0);
    const scope = opts.selector ? document.querySelector(opts.selector) : null;
    if (opts.selector && !scope) return JSON.stringify({ error: `no element matches ${opts.selector}` });

    // Transforms are read first, then disabled while measuring, so every
    // rect below is the untransformed layout box (the converter re-applies them).
    const transforms = new Map();
    const scopeRoot = scope || document.body || document.documentElement;
    for (const el of [scopeRoot, ...scopeRoot.querySelectorAll('*')]) {
      if (el.namespaceURI === 'http://www.w3.org/2000/svg' && el.tagName.toLowerCase() !== 'svg') continue;
      const t = matrixOf(getComputedStyle(el));
      if (t) transforms.set(el, t);
    }
    const freezeStyle = document.createElement('style');
    freezeStyle.textContent = '*,*::before,*::after{transform:none!important;translate:none!important;'
      + 'rotate:none!important;scale:none!important;transition:none!important}';
    document.head.appendChild(freezeStyle);
    await frame();

    const vw = document.documentElement.clientWidth || window.innerWidth;
    const vh = window.innerHeight;
    let region;
    if (scope) {
      const r = scope.getBoundingClientRect();
      region = [r.left + scrollX, r.top + scrollY, r.width, r.height];
    } else if (opts.fullPage) {
      region = [0, 0, vw, docHeight()];
    } else {
      region = [0, 0, vw, vh];
    }
    const regionBottom = region[1] + region[3];

    function styleOf(cs) {
      const s = {};
      for (const k in STYLE_KEYS) {
        let v = cs[k];
        if (v === undefined || v === null || v === '' || v === STYLE_KEYS[k]) continue;
        s[k] = v;
      }
      for (const k of COLOR_KEYS) if (s[k]) s[k] = toRgba(s[k]);
      for (const k of COMPOSITE_KEYS) if (s[k]) s[k] = normalizeColors(s[k]);
      if (s.maskImage === undefined && s.webkitMaskImage) s.maskImage = s.webkitMaskImage;
      delete s.webkitMaskImage;
      return s;
    }

    function rectOf(r, ctx) {
      return [r.left + ctx.ox, r.top + ctx.oy, r.width, r.height].map((v) => +v.toFixed(2));
    }

    function fontIndex(cs) {
      const key = `${cs.fontFamily}|${cs.fontWeight}|${cs.fontStyle}`;
      let i = fontKeys.get(key);
      if (i === undefined) {
        i = fontProbes.length;
        fontKeys.set(key, i);
        fontProbes.push(null);
      }
      return i;
    }

    function textLines(node, ctx) {
      const range = node.ownerDocument.createRange();
      const s = node.data;
      range.selectNodeContents(node);
      const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0 && r.height > 0);
      if (!rects.length) return null;
      if (rects.length === 1 || s.length > 4000 || charBudget.left < s.length) {
        if (rects.length > 1) {
          const u = range.getBoundingClientRect();
          return { lines: [{ t: s, r: rectOf(u, ctx) }], approx: true };
        }
        return { lines: [{ t: s, r: rectOf(rects[0], ctx) }] };
      }
      charBudget.left -= s.length;
      const lines = [];
      let cur = null;
      let i = 0;
      while (i < s.length) {
        const cp = s.codePointAt(i);
        const len = cp > 0xffff ? 2 : 1;
        range.setStart(node, i);
        range.setEnd(node, i + len);
        const cr = Array.from(range.getClientRects()).find((r) => r.height > 0);
        const ch = s.slice(i, i + len);
        if (cr) {
          if (!cur || cr.top >= cur.top + cur.h * 0.6 || (cr.left < cur.left - 1 && cr.top > cur.top + 1)) {
            cur = { t: '', top: cr.top, left: cr.left, right: cr.right, bottom: cr.bottom, h: cr.height };
            lines.push(cur);
          }
          cur.t += ch;
          cur.left = Math.min(cur.left, cr.left);
          cur.right = Math.max(cur.right, cr.right);
          cur.top = Math.min(cur.top, cr.top);
          cur.bottom = Math.max(cur.bottom, cr.bottom);
        } else if (cur) {
          cur.t += ch;
        }
        i += len;
      }
      return {
        lines: lines.map((l) => ({
          t: l.t,
          r: [l.left + ctx.ox, l.top + ctx.oy, l.right - l.left, l.bottom - l.top].map((v) => +v.toFixed(2)),
        })),
      };
    }

    function svgMarkup(el, cs) {
      const clone = el.cloneNode(true);
      const src = [el, ...el.querySelectorAll('*')];
      const dst = [clone, ...clone.querySelectorAll('*')];
      const keys = ['fill', 'fill-opacity', 'fill-rule', 'stroke', 'stroke-width', 'stroke-opacity',
        'stroke-linecap', 'stroke-linejoin', 'stroke-dasharray', 'stroke-dashoffset', 'opacity',
        'stop-color', 'stop-opacity', 'clip-rule', 'font-family', 'font-size', 'font-weight'];
      for (let i = 0; i < src.length && i < dst.length; i++) {
        const c = getComputedStyle(src[i]);
        if (c.display === 'none') { dst[i].setAttribute('display', 'none'); continue; }
        if (c.visibility === 'hidden') dst[i].setAttribute('visibility', 'hidden');
        let style = '';
        for (const k of keys) {
          const v = c.getPropertyValue(k);
          if (v) style += `${k}:${normalizeColors(toRgbaMaybe(v))};`;
        }
        dst[i].setAttribute('style', style);
      }
      clone.setAttribute('color', toRgba(cs.color));
      const r = el.getBoundingClientRect();
      clone.setAttribute('width', `${r.width}`);
      clone.setAttribute('height', `${r.height}`);
      clone.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
      clone.setAttribute('xmlns:xlink', 'http://www.w3.org/1999/xlink');
      // Inline same-document <use> targets; external ones are fetched later.
      const defs = document.createElementNS('http://www.w3.org/2000/svg', 'defs');
      const seen = new Set();
      const external = [];
      for (const use of clone.querySelectorAll('use')) {
        const href = use.getAttribute('href') || use.getAttribute('xlink:href') || '';
        if (href.startsWith('#')) {
          const id = href.slice(1);
          if (seen.has(id) || clone.querySelector(`[id="${CSS.escape(id)}"]`)) continue;
          const target = el.ownerDocument.getElementById(id);
          if (target) {
            seen.add(id);
            defs.appendChild(target.cloneNode(true));
          }
        } else if (href) {
          external.push({ use, href });
        }
      }
      // Gradients / clip paths referenced by url(#id) from elsewhere in the page.
      const markup = new XMLSerializer().serializeToString(clone);
      const refs = markup.match(/url\(["']?#([^"')]+)["']?\)/g) || [];
      for (const ref of refs) {
        const id = /#([^"')]+)/.exec(ref)[1];
        if (seen.has(id) || clone.querySelector(`[id="${CSS.escape(id)}"]`)) continue;
        const target = el.ownerDocument.getElementById(id);
        if (target) {
          seen.add(id);
          defs.appendChild(target.cloneNode(true));
        }
      }
      if (defs.childNodes.length) clone.insertBefore(defs, clone.firstChild);
      return { clone, external };
    }
    function toRgbaMaybe(v) {
      return /^(?:oklch|oklab|lab|lch|hwb|hsla?|color)\(/.test(v) ? toRgba(v) : v;
    }

    async function resolveExternalUses(clone, external) {
      for (const { use, href } of external) {
        try {
          const abs = new URL(href, document.baseURI);
          const id = abs.hash.slice(1);
          abs.hash = '';
          let doc = svgFetches.get(abs.href);
          if (doc === undefined) {
            doc = null;
            try {
              const res = await fetch(abs.href);
              if (res.ok) doc = new DOMParser().parseFromString(await res.text(), 'image/svg+xml');
            } catch (_) { /* cross-origin without CORS */ }
            svgFetches.set(abs.href, doc);
          }
          const target = doc && id ? doc.getElementById(id) : null;
          if (!target) continue;
          let defs = clone.querySelector('defs');
          if (!defs) {
            defs = document.createElementNS('http://www.w3.org/2000/svg', 'defs');
            clone.insertBefore(defs, clone.firstChild);
          }
          if (!clone.querySelector(`[id="${CSS.escape(id)}"]`)) defs.appendChild(document.importNode(target, true));
          use.setAttribute('href', `#${id}`);
          use.removeAttribute('xlink:href');
        } catch (_) { /* ignore */ }
      }
    }

    async function maskSvg(url) {
      let text = svgFetches.get(`mask:${url}`);
      if (text !== undefined) return text;
      text = null;
      try {
        if (url.startsWith('data:image/svg+xml')) {
          const comma = url.indexOf(',');
          const body = url.slice(comma + 1);
          text = url.slice(0, comma).includes(';base64') ? atob(body) : decodeURIComponent(body);
        } else {
          const res = await fetch(url);
          if (res.ok && (res.headers.get('content-type') || '').includes('svg')) text = await res.text();
        }
      } catch (_) { text = null; }
      svgFetches.set(`mask:${url}`, text);
      return text;
    }

    const bgSizes = new Map();
    async function naturalSize(url) {
      if (bgSizes.has(url)) return bgSizes.get(url);
      let size = null;
      try {
        const img = new Image();
        img.src = url;
        await Promise.race([img.decode(), sleep(1500)]);
        if (img.naturalWidth) size = [img.naturalWidth, img.naturalHeight];
      } catch (_) { size = null; }
      bgSizes.set(url, size);
      return size;
    }

    const pendingAsync = [];

    function baseInfo(el, cs, ctx) {
      const n = { t: el.tagName === 'OPENPEN-PSEUDO' ? `::${el.getAttribute('data-pseudo')}` : el.tagName.toLowerCase() };
      const id = el.id;
      if (id && id.length < 60) n.id = id;
      const cls = typeof el.className === 'string' ? el.className.trim().split(/\s+/).filter(Boolean) : [];
      if (cls.length) n.cls = cls.slice(0, 3).join(' ').slice(0, 80);
      const role = el.getAttribute('role');
      if (role) n.role = role;
      const label = el.getAttribute('aria-label') || el.getAttribute('title') || el.getAttribute('alt');
      if (label) n.label = label.slice(0, 80);
      if (el.tagName === 'A' && el.href) n.href = String(el.href).slice(0, 300);
      const comp = componentName(el);
      if (comp) n.comp = comp;
      return n;
    }

    function walkChildren(parentEl, ctx, out) {
      const kids = parentEl.shadowRoot ? parentEl.shadowRoot.childNodes
        : parentEl.tagName === 'SLOT' ? (parentEl.assignedNodes({ flatten: true }).length
          ? parentEl.assignedNodes({ flatten: true }) : parentEl.childNodes)
          : parentEl.childNodes;
      for (const k of kids) {
        if (truncated) return;
        if (k.nodeType === 3) {
          if (!k.data || !k.data.trim()) {
            // Preserved whitespace is still invisible; collapsed one never renders.
            continue;
          }
          if (ctx.hidden) continue;
          const tl = textLines(k, ctx);
          if (tl) {
            const node = { k: 'text', lines: tl.lines };
            if (tl.approx) node.approx = true;
            out.push(node);
          }
        } else if (k.nodeType === 1) {
          const r = walk(k, ctx);
          if (Array.isArray(r)) out.push(...r);
          else if (r) out.push(r);
        }
      }
    }

    function walk(el, ctx) {
      if (SKIP.has(el.tagName)) return null;
      if (el === freezeStyle || el === pseudoStyle) return null;
      const win = el.ownerDocument.defaultView || window;
      const cs = win.getComputedStyle(el);
      if (cs.display === 'none') return null;
      if (cs.display === 'contents') {
        const out = [];
        walkChildren(el, ctx, out);
        return out;
      }
      if (count >= maxNodes) {
        truncated = true;
        return null;
      }
      const isSvgChild = el.namespaceURI === 'http://www.w3.org/2000/svg' && el.tagName.toLowerCase() !== 'svg';
      if (isSvgChild) return null;
      const br = el.getBoundingClientRect();
      const rect = rectOf(br, ctx);
      // Entirely below a viewport-only region: nothing to draw.
      if (rect[1] > regionBottom + 1 && cs.position !== 'fixed') return null;
      // Screen-reader-only content.
      if (rect[2] <= 1 && rect[3] <= 1 && (cs.overflowX !== 'visible' || cs.position === 'absolute')) return null;
      count++;
      const n = baseInfo(el, cs, ctx);
      n.r = rect;
      n.s = styleOf(cs);
      const hidden = cs.visibility !== 'visible';
      if (hidden) n.hidden = true;
      const tf = transforms.get(el);
      if (tf) n.tf = tf;
      const deco = new Set(ctx.deco);
      for (const d of (cs.textDecorationLine || '').split(/\s+/)) if (d === 'underline' || d === 'line-through') deco.add(d);
      if (deco.size) n.deco = [...deco];
      const tag = el.tagName;
      const childCtx = { ox: ctx.ox, oy: ctx.oy, deco, hidden };

      if (tag === 'IMG') {
        n.k = 'img';
        n.src = el.currentSrc || el.src || '';
        n.nat = [el.naturalWidth, el.naturalHeight];
        return n;
      }
      if (el instanceof win.SVGSVGElement || tag.toLowerCase() === 'svg') {
        n.k = 'svg';
        try {
          const { clone, external } = svgMarkup(el, cs);
          if (external.length) {
            pendingAsync.push(resolveExternalUses(clone, external).then(() => {
              n.svg = new XMLSerializer().serializeToString(clone);
            }));
          }
          n.svg = new XMLSerializer().serializeToString(clone);
        } catch (e) {
          n.k = 'shot';
          shots.push(el);
          n.shot = shots.length - 1;
        }
        return n;
      }
      if (tag === 'IFRAME' || tag === 'FRAME') {
        let doc = null;
        try { doc = el.contentDocument; } catch (_) { doc = null; }
        if (doc && doc.body) {
          n.k = 'el';
          const bl = parseFloat(cs.borderLeftWidth) + parseFloat(cs.paddingLeft);
          const bt = parseFloat(cs.borderTopWidth) + parseFloat(cs.paddingTop);
          const inner = { ox: rect[0] + bl, oy: rect[1] + bt, deco: new Set(), hidden };
          const c = [];
          walkChildren(doc.body, inner, c);
          const bs = el.contentWindow.getComputedStyle(doc.body);
          const hs = el.contentWindow.getComputedStyle(doc.documentElement);
          const bg = toRgba(hs.backgroundColor) !== 'rgba(0, 0, 0, 0)' ? hs.backgroundColor : bs.backgroundColor;
          if (!n.s.backgroundColor && bg) n.s.backgroundColor = toRgba(bg);
          n.c = c;
          n.clip = true;
          return n;
        }
        n.k = 'shot';
        shots.push(el);
        n.shot = shots.length - 1;
        return n;
      }
      if (SHOT.has(tag) || (tag === 'INPUT' && NATIVE_INPUTS.has(el.type)) || tag === 'SELECT'
        || tag === 'PROGRESS' || tag === 'METER') {
        n.k = 'shot';
        if (!hidden && rect[2] > 0 && rect[3] > 0) {
          shots.push(el);
          n.shot = shots.length - 1;
        }
        if (tag === 'VIDEO' && el.poster) n.src = el.poster;
        return n;
      }
      if (tag === 'INPUT' && BUTTON_INPUTS.has(el.type)) {
        n.k = 'input';
        n.button = true;
        n.value = el.value || (el.type === 'reset' ? 'Reset' : el.type === 'submit' ? 'Submit' : '');
        n.f = fontIndex(cs);
        if (fontProbes[n.f] === null) fontProbes[n.f] = el;
        return n;
      }
      if ((tag === 'INPUT' && TEXT_INPUTS.has(el.type)) || tag === 'TEXTAREA') {
        n.k = 'input';
        let value = el.value || '';
        let placeholder = false;
        if (!value && el.placeholder) {
          value = el.placeholder;
          placeholder = true;
          n.phColor = toRgba(win.getComputedStyle(el, '::placeholder').color);
        }
        if (el.type === 'password' && !placeholder) value = '•'.repeat(value.length);
        n.value = value;
        if (placeholder) n.placeholder = true;
        n.f = fontIndex(cs);
        if (fontProbes[n.f] === null) fontProbes[n.f] = el;
        return n;
      }
      if (tag === 'BR' || tag === 'WBR') {
        count--;
        return null;
      }

      n.k = 'el';
      if (cs.overflowX !== 'visible' || cs.overflowY !== 'visible' || tag === 'BUTTON') {
        if (cs.overflowX !== 'visible' || cs.overflowY !== 'visible') n.clip = true;
      }
      const mask = n.s.maskImage;
      if (mask && mask !== 'none') {
        const m = /url\("?([^")]+)"?\)/.exec(mask);
        if (m) {
          pendingAsync.push(maskSvg(m[1]).then((svg) => { if (svg) n.mask = svg; }));
        }
      }
      if (n.s.backgroundImage) {
        const urls = [...n.s.backgroundImage.matchAll(/url\("?([^")]+)"?\)/g)].map((m) => m[1]);
        if (urls.length) {
          n.bgNat = {};
          for (const u of urls) {
            pendingAsync.push(naturalSize(u).then((sz) => { if (sz) n.bgNat[u] = sz; }));
          }
        }
      }
      if (cs.display === 'list-item' && cs.listStyleType !== 'none' && cs.listStylePosition === 'outside'
        && !hidden) {
        let index = 1;
        if (el.parentElement && el.parentElement.tagName === 'OL') {
          const start = parseInt(el.parentElement.getAttribute('start') || '1', 10) || 1;
          index = start;
          for (let p = el.previousElementSibling; p; p = p.previousElementSibling) if (p.tagName === 'LI') index++;
        }
        const text = cs.listStyleImage === 'none' ? markerText(cs, index) : null;
        if (text) {
          const w = textWidth(`${text} `, cs);
          n.marker = { t: text, w: +w.toFixed(2) };
        }
      }
      const c = [];
      walkChildren(el, childCtx, c);
      if (c.length) n.c = c;
      if (c.some((k) => k.k === 'text') && n.f === undefined) {
        n.f = fontIndex(cs);
        if (fontProbes[n.f] === null) fontProbes[n.f] = el;
      }
      if (n.marker && n.f === undefined) {
        n.f = fontIndex(cs);
        if (fontProbes[n.f] === null) fontProbes[n.f] = el;
      }
      return n;
    }

    // Page background: the canvas takes the root's, or the body's when the root has none.
    const htmlCs = getComputedStyle(document.documentElement);
    const bodyCs = document.body ? getComputedStyle(document.body) : htmlCs;
    let background = toRgba(htmlCs.backgroundColor);
    let backgroundImage = htmlCs.backgroundImage !== 'none' ? htmlCs.backgroundImage : null;
    let bodyPropagated = false;
    if (background === 'rgba(0, 0, 0, 0)' && !backgroundImage) {
      background = toRgba(bodyCs.backgroundColor);
      if (bodyCs.backgroundImage !== 'none') backgroundImage = normalizeColors(bodyCs.backgroundImage);
      bodyPropagated = true;
    }

    const ctx = { ox: scrollX, oy: scrollY, deco: new Set(), hidden: false };
    let root;
    if (scope) {
      root = walk(scope, ctx);
    } else {
      root = walk(document.body, ctx);
      if (root && !Array.isArray(root) && bodyPropagated) {
        delete root.s.backgroundColor;
        delete root.s.backgroundImage;
      }
    }
    if (Array.isArray(root)) root = { k: 'el', t: 'div', r: region, s: {}, c: root };
    await Promise.all(pendingAsync);

    // Font faces declared by the page (readable sheets directly, others fetched).
    const faces = [];
    const loaded = new Set();
    try {
      for (const f of document.fonts) {
        if (f.status === 'loaded') loaded.add(f.family.replace(/^["']|["']$/g, ''));
      }
    } catch (_) { /* ignore */ }
    const sheetsToFetch = [];
    function collectRules(rules, base) {
      for (const rule of rules) {
        if (rule.type === 5 /* FONT_FACE */) {
          const st = rule.style;
          faces.push({
            family: st.getPropertyValue('font-family').trim().replace(/^["']|["']$/g, ''),
            weight: st.getPropertyValue('font-weight').trim() || '400',
            style: st.getPropertyValue('font-style').trim() || 'normal',
            src: st.getPropertyValue('src'),
            base,
          });
        } else if (rule.cssRules) {
          try { collectRules(rule.cssRules, base); } catch (_) { /* ignore */ }
        }
        if (rule.type === 3 /* IMPORT */ && rule.styleSheet) {
          try { collectRules(rule.styleSheet.cssRules, rule.styleSheet.href || base); } catch (_) {
            if (rule.styleSheet.href) sheetsToFetch.push(rule.styleSheet.href);
          }
        }
      }
    }
    for (const sheet of document.styleSheets) {
      try {
        collectRules(sheet.cssRules, sheet.href || document.baseURI);
      } catch (_) {
        if (sheet.href) sheetsToFetch.push(sheet.href);
      }
    }
    for (const href of sheetsToFetch.slice(0, 10)) {
      try {
        const res = await Promise.race([fetch(href), sleep(4000).then(() => null)]);
        if (!res || !res.ok) continue;
        const css = await res.text();
        for (const block of css.match(/@font-face\s*{[^}]*}/g) || []) {
          const prop = (name) => {
            const m = new RegExp(`${name}\\s*:\\s*([^;}]+)`).exec(block);
            return m ? m[1].trim() : '';
          };
          faces.push({
            family: prop('font-family').replace(/^["']|["']$/g, ''),
            weight: prop('font-weight') || '400',
            style: prop('font-style') || 'normal',
            src: prop('src'),
            base: href,
          });
        }
      } catch (_) { /* ignore */ }
    }
    const fontFaces = [];
    for (const f of faces) {
      if (!loaded.has(f.family)) continue;
      const srcs = [...f.src.matchAll(/url\(\s*["']?([^"')]+)["']?\s*\)(?:\s*format\(\s*["']?([^"')]+)["']?\s*\))?/g)];
      if (!srcs.length) continue;
      const rank = (fmt, url) => {
        const s = `${fmt || ''} ${url}`.toLowerCase();
        if (/truetype|\.ttf\b/.test(s)) return 0;
        if (/opentype|\.otf\b/.test(s)) return 1;
        if (/woff2/.test(s)) return 3;
        if (/woff/.test(s)) return 2;
        return 4;
      };
      srcs.sort((a, b) => rank(a[2], a[1]) - rank(b[2], b[1]));
      let url;
      try { url = new URL(srcs[0][1], f.base).href; } catch (_) { continue; }
      fontFaces.push({ family: f.family, weight: f.weight, style: f.style, url });
    }

    const fonts = [];
    for (const [key, i] of fontKeys) {
      const [family, weight, style] = key.split('|');
      fonts[i] = { family, weight, style };
    }

    freezeStyle.remove();
    await frame();
    return JSON.stringify({
      url: location.href,
      title: document.title || '',
      viewport: [vw, vh],
      docHeight: docHeight(),
      region,
      background,
      backgroundImage,
      root,
      fonts,
      fontFaces,
      shots: shots.length,
      nodes: count,
      truncated,
      warnings,
      ms: Math.round(performance.now() - t0),
    });
  }

  // The page region an import covers (same rules as `crawl`), for the reference screenshot.
  function regionOf(opts) {
    const vw = document.documentElement.clientWidth || window.innerWidth;
    if (opts.selector) {
      const el = document.querySelector(opts.selector);
      if (!el) return JSON.stringify(null);
      const r = el.getBoundingClientRect();
      return JSON.stringify([r.left + scrollX, r.top + scrollY, r.width, r.height]);
    }
    return JSON.stringify(opts.fullPage ? [0, 0, vw, docHeight()] : [0, 0, vw, window.innerHeight]);
  }

  // Rects of the elements captured as screenshots (measured with transforms on).
  function shotRects() {
    return JSON.stringify((window.__openpenShots || []).map((el) => {
      const r = el.getBoundingClientRect();
      return [r.left + scrollX, r.top + scrollY, r.width, r.height];
    }));
  }

  // Re-encodes an image the renderer cannot decode as PNG, through the browser's decoders.
  async function toPng(b64, mime, w, h) {
    const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
    try {
      const img = new Image();
      img.src = url;
      await img.decode();
      const cw = img.naturalWidth || w || 256;
      const ch = img.naturalHeight || h || 256;
      const c = document.createElement('canvas');
      c.width = cw;
      c.height = ch;
      c.getContext('2d').drawImage(img, 0, 0, cw, ch);
      const data = c.toDataURL('image/png');
      return data.slice(data.indexOf(',') + 1);
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  // Fetches a resource from the page (same-origin or CORS-enabled), base64-encoded.
  async function fetchB64(url) {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const buf = new Uint8Array(await res.arrayBuffer());
    let s = '';
    for (let i = 0; i < buf.length; i += 0x8000) s += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
    return JSON.stringify({ b64: btoa(s), type: res.headers.get('content-type') || '' });
  }

  window.__openpen = { settle, prescroll, freeze, prepare, crawl, regionOf, shotRects, toPng, fetchB64, docHeight };
  return true;
})();
