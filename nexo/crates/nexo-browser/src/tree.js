(opts) => {
  // Walks the page once and returns its accessibility-style tree as JSON text. Two filters share the walk:
  // `interactive` keeps the visible actionable elements (the compact snapshot); `all` also keeps headings,
  // landmarks, lists, tables, images, static text and elements that are not visible (marked). Every element that
  // can be pointed at gets a `data-nexo-ref` (`e1`, `e2`, ...): one that already has it keeps it and new ones
  // continue after the highest number in use, so a ref from any snapshot stays valid for click, fill, hover, ...
  // A tree level is one emitted element; transparent wrappers (div, span, label) and wrappers that are left out
  // (the structure around the actionable elements of the interactive filter) add none.
  // Open shadow roots are walked (and <slot>s followed) so web components read like any other element. A ref is
  // not handed out inside a shadow root: the actions that take refs look the element up in the document only.
  // Hidden form fields (<input type=hidden>, which often hold tokens) are never listed. Subtrees marked
  // aria-hidden are not read as text or structure; their visible actionable elements are still listed.
  const interactiveOnly = opts.filter === 'interactive';
  const maxDepth = Math.max(1, opts.depth || 15);
  const NODE_CAP = 20000;
  const SEL = 'a, button, input, select, textarea, [role], [onclick], [contenteditable]';
  const STRUCT = SEL + ', h1, h2, h3, h4, h5, h6, nav, main, aside, article, form, ul, ol, li, table, img, dialog, '
    + 'details, summary, header, footer, fieldset, progress, meter, hr, iframe, video, audio, '
    + 'section[aria-label], section[aria-labelledby], svg[aria-label]';
  const SKIP = new Set(['script', 'style', 'noscript', 'template', 'head', 'meta', 'link', 'title', 'base', 'datalist',
    'option', 'optgroup', 'source', 'track', 'param', 'canvas', 'map', 'area', 'colgroup', 'col']);
  // Leaves take no children of their own; only the actionable ones inside (a link around a button) are kept.
  const LEAF = new Set(['link', 'button', 'heading', 'tab', 'menuitem', 'menuitemcheckbox', 'menuitemradio',
    'treeitem', 'checkbox', 'radio', 'switch', 'textbox', 'searchbox', 'spinbutton', 'slider', 'combobox', 'option',
    'img', 'clickable', 'separator', 'progressbar', 'meter', 'file', 'date', 'time', 'color', 'month', 'week',
    'datetime-local', 'iframe', 'video', 'audio', 'status', 'alert', 'tooltip']);
  // Containers that fold into a single line when nothing structural is inside them.
  const FOLD = new Set(['listitem', 'cell', 'gridcell', 'columnheader', 'rowheader']);
  // Roles named by their own text; every other role is named only by an explicit label.
  const NAMED_BY_TEXT = new Set(['link', 'button', 'heading', 'tab', 'menuitem', 'menuitemcheckbox',
    'menuitemradio', 'treeitem', 'checkbox', 'radio', 'switch', 'option', 'clickable', 'status', 'alert', 'tooltip',
    'listitem', 'cell', 'gridcell', 'columnheader', 'rowheader']);
  // Roles that are not worth a ref of their own (their content or their parent is what a person points at).
  const NOREF = new Set(['listitem', 'row', 'rowgroup', 'cell', 'gridcell', 'columnheader', 'rowheader', 'option',
    'separator']);
  const INPUT_ROLE = { text: 'textbox', email: 'textbox', tel: 'textbox', url: 'textbox', password: 'textbox',
    search: 'searchbox', number: 'spinbutton', checkbox: 'checkbox', radio: 'radio', range: 'slider',
    button: 'button', submit: 'button', reset: 'button', image: 'button' };
  const IMPLICIT = { button: 'button', nav: 'navigation', main: 'main', aside: 'complementary', article: 'article',
    form: 'form', ul: 'list', ol: 'list', li: 'listitem', table: 'table', tr: 'row', thead: 'rowgroup',
    tbody: 'rowgroup', tfoot: 'rowgroup', td: 'cell', img: 'img', dialog: 'dialog', details: 'group',
    summary: 'button', textarea: 'textbox', progress: 'progressbar', meter: 'meter', hr: 'separator',
    fieldset: 'group', output: 'status', iframe: 'iframe', video: 'video', audio: 'audio' };

  const clean = (s, n) => {
    const t = (s || '').replace(/\s+/g, ' ').trim();
    return t.length > n ? t.slice(0, n - 1) + '…' : t;
  };
  const quote = (s) => '"' + s.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"';
  // The text of an element as it shows. innerText does not go into shadow roots or follow a <slot>, so an element
  // that holds either is read child by child through kidsOf (below); every other one by its innerText.
  const rawText = (el) => (el.innerText ? el.innerText : el.textContent) || '';
  const textOf = (el) => {
    if (el.localName === 'slot' || (shadowParts && shadowParts.hosts.some((h) => h === el || el.contains(h)))) {
      let out = '';
      for (const n of kidsOf(el)) {
        if (n.nodeType === 3) out += n.nodeValue;
        else if (n.nodeType === 1 && !SKIP.has(n.localName) && getComputedStyle(n).display !== 'none') {
          out += ' ' + textOf(n) + ' ';
        }
      }
      return out;
    }
    return rawText(el);
  };

  // querySelector that also looks into open shadow roots and at what is slotted into a <slot> below el. The
  // hosts and slots of the page are collected once, and only when the first plain query misses.
  let shadowParts = null;
  const collect = (root, hosts, slots) => {
    for (const e of root.querySelectorAll('*')) {
      if (e.shadowRoot) {
        hosts.push(e);
        collect(e.shadowRoot, hosts, slots);
      }
      if (e.localName === 'slot') slots.push(e);
    }
  };
  const has = (box, sel) => {
    if (box.querySelector(sel)) return true;
    if (!shadowParts) {
      shadowParts = { hosts: [], slots: [] };
      collect(document, shadowParts.hosts, shadowParts.slots);
    }
    const { hosts, slots } = shadowParts;
    if (!hosts.length) return false;
    if (box.shadowRoot && has(box.shadowRoot, sel)) return true;
    for (const h of hosts) {
      if (h !== box && box.contains(h) && has(h.shadowRoot, sel)) return true;
    }
    for (const s of slots) {
      if (!box.contains(s)) continue;
      for (const a of s.assignedNodes({ flatten: true })) {
        if (a.nodeType === 1 && (a.matches(sel) || has(a, sel))) return true;
      }
    }
    return false;
  };
  // The children of a node as they are rendered: the shadow tree of a host (its own children show up where a
  // <slot> is), what is assigned to a <slot>.
  const kidsOf = (el) => {
    if (el.shadowRoot) return el.shadowRoot.childNodes;
    if (el.localName === 'slot') {
      const assigned = el.assignedNodes({ flatten: true });
      if (assigned.length) return assigned;
    }
    return el.childNodes;
  };
  const inShadow = (el) => el.getRootNode() !== document;

  let max = 0;
  for (const el of document.querySelectorAll('[data-nexo-ref]')) {
    const n = parseInt(el.getAttribute('data-nexo-ref').slice(1), 10);
    if (n > max) max = n;
  }
  const nodes = [];
  let capped = false;

  const explicitRole = (el) => {
    const r = (el.getAttribute('role') || '').trim().split(/\s+/)[0].toLowerCase();
    return r === 'presentation' || r === 'none' || r === 'generic' ? '' : r;
  };
  const isInteractive = (el) => {
    if (!el.matches(SEL)) return false;
    if (el.tagName === 'INPUT' && el.type === 'hidden') return false;
    if (el.matches('a, button, input, select, textarea, [onclick]') || explicitRole(el)) return true;
    const ce = el.getAttribute('contenteditable');
    return ce !== null && ce.toLowerCase() !== 'false';
  };
  const roleOf = (el, tag) => {
    const explicit = explicitRole(el);
    if (explicit) return explicit;
    if (tag === 'a') return 'link';
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (tag === 'input') return INPUT_ROLE[el.type] || el.type || 'textbox';
    if (tag === 'select') return el.multiple || el.size > 1 ? 'listbox' : 'combobox';
    if (tag === 'th') return el.getAttribute('scope') === 'row' ? 'rowheader' : 'columnheader';
    if (tag === 'header' || tag === 'footer') {
      if (el.parentElement && el.parentElement.closest('article, aside, main, nav, section')) return '';
      return tag === 'header' ? 'banner' : 'contentinfo';
    }
    if (tag === 'section') return el.hasAttribute('aria-label') || el.hasAttribute('aria-labelledby') ? 'region' : '';
    if (tag === 'svg') return el.hasAttribute('aria-label') ? 'img' : '';
    // An image with an empty alt is decoration.
    if (tag === 'img' && el.getAttribute('alt') === '' && !el.hasAttribute('aria-label')) return '';
    if (IMPLICIT[tag]) return IMPLICIT[tag];
    if (el.getAttribute('contenteditable') !== null && el.isContentEditable) return 'textbox';
    if (el.hasAttribute('onclick')) return 'clickable';
    return '';
  };
  const nameOf = (el, tag, role, leaf) => {
    let name = clean(el.getAttribute('aria-label'), 160);
    const by = el.getAttribute('aria-labelledby');
    if (!name && by) {
      name = clean(by.split(/\s+/).map((id) => (document.getElementById(id) || {}).textContent || '').join(' '), 160);
    }
    if (!name && el.labels && el.labels.length) name = clean(el.labels[0].textContent, 160);
    if (!name && tag === 'img') name = clean(el.getAttribute('alt'), 160);
    if (!name && tag === 'svg') name = clean((el.querySelector('title') || {}).textContent, 160);
    if (!name && tag === 'table') name = clean((el.querySelector('caption') || {}).textContent, 160);
    if (!name && tag === 'fieldset') name = clean((el.querySelector('legend') || {}).textContent, 160);
    const control = tag === 'input' || tag === 'textarea' || tag === 'select';
    if (!name && !control && NAMED_BY_TEXT.has(role) && (leaf || !FOLD.has(role))) {
      name = clean(textOf(el), FOLD.has(role) ? 600 : 160);
      if (!name) {
        const inner = el.querySelector('img[alt], [aria-label], svg title');
        if (inner) name = clean(inner.getAttribute('alt') || inner.getAttribute('aria-label') || inner.textContent, 160);
      }
    }
    if (!name) name = clean(el.placeholder, 160);
    if (!name && tag === 'input' && ['button', 'submit', 'reset'].includes(el.type)) name = clean(el.value, 160);
    return name || clean(el.title, 160) || clean(el.alt, 160);
  };
  const attrsOf = (el, tag, role, name) => {
    const a = [];
    if (role === 'heading') a.push('level=' + (/^h[1-6]$/.test(tag) ? tag[1] : (el.getAttribute('aria-level') || '2')));
    if (tag === 'a' && el.getAttribute('href')) a.push('href=' + quote(clean(el.getAttribute('href'), 120)));
    if (tag === 'input' && el.type === 'password') {
      if (el.value) a.push('value="•••"');
    } else if ((tag === 'input' && !['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'file'].includes(el.type))
      || tag === 'textarea') {
      if (el.value) a.push('value=' + quote(clean(el.value, 80)));
    } else if (tag === 'select' && el.selectedOptions && el.selectedOptions.length) {
      a.push('value=' + quote(clean([...el.selectedOptions].map((o) => o.textContent).join(', '), 80)));
    }
    if ((role === 'textbox' || role === 'searchbox') && el.placeholder && clean(el.placeholder, 60) !== name) {
      a.push('placeholder=' + quote(clean(el.placeholder, 60)));
    }
    if (el.checked === true || el.getAttribute('aria-checked') === 'true') a.push('checked');
    if (el.getAttribute('aria-checked') === 'mixed') a.push('mixed');
    if (el.disabled === true || el.getAttribute('aria-disabled') === 'true') a.push('disabled');
    if (el.required === true || el.getAttribute('aria-required') === 'true') a.push('required');
    if (el.readOnly === true && tag !== 'select') a.push('readonly');
    if (el.getAttribute('aria-expanded') === 'true' || (tag === 'details' && el.open)) a.push('expanded');
    else if (el.getAttribute('aria-expanded') === 'false' || tag === 'details') a.push('collapsed');
    if (el.getAttribute('aria-selected') === 'true') a.push('selected');
    if (el.getAttribute('aria-pressed') === 'true') a.push('pressed');
    const cur = el.getAttribute('aria-current');
    if (cur && cur !== 'false') a.push('current');
    if (el.getAttribute('aria-invalid') === 'true') a.push('invalid');
    return a;
  };
  const isShown = (el, cs) => cs.display !== 'none' && cs.visibility !== 'hidden' && cs.visibility !== 'collapse'
    && (el.getClientRects().length > 0 || cs.display === 'contents');
  const isBlock = (cs) => !cs.display.startsWith('inline') && cs.display !== 'contents';
  const refOf = (el) => {
    let ref = el.getAttribute('data-nexo-ref');
    if (!ref) {
      ref = 'e' + (++max);
      el.setAttribute('data-nexo-ref', ref);
    }
    return ref;
  };
  // Adds a record unless it is below the depth limit (the parent then says there is more) or the page is huge.
  const push = (rec, parent) => {
    if (rec.d >= maxDepth) {
      if (parent) parent.more = true;
      return null;
    }
    if (nodes.length >= NODE_CAP) {
      capped = true;
      return null;
    }
    nodes.push(rec);
    return rec;
  };

  // ctx: 0 = visible so far, 1 = inside a hidden wrapper that is not itself listed (the nodes directly under it
  // carry the "not visible" mark), 2 = inside a node that is already marked (no further marks).
  const wrapCtx = (ctx, shown) => (ctx === 0 && shown ? 0 : ctx === 0 ? 1 : ctx);
  const walkChildren = (el, level, parent, ctx, quiet) => {
    let buf = '';
    const flush = () => {
      const t = clean(buf, 1000);
      buf = '';
      if (t && !quiet && !interactiveOnly) push({ d: level, role: 'text', name: t, hidden: ctx === 1 }, parent);
    };
    for (const c of kidsOf(el)) {
      if (nodes.length >= NODE_CAP) {
        capped = true;
        break;
      }
      if (c.nodeType === 3) {
        buf += c.nodeValue;
        continue;
      }
      if (c.nodeType !== 1) continue;
      const tag = c.tagName.toLowerCase();
      if (SKIP.has(tag) || (tag === 'input' && c.type === 'hidden')) continue;
      if (tag === 'br') {
        buf += ' ';
        continue;
      }
      // Inside aria-hidden only what can be acted on is kept, never text or structure.
      const q = quiet || c.getAttribute('aria-hidden') === 'true';
      const cs = getComputedStyle(c);
      if (interactiveOnly && cs.display === 'none') continue;
      const shown = isShown(c, cs);
      // The text of a label is the name of its control, so it is not repeated as text.
      if (tag === 'label' && c.control) {
        if (has(c, SEL)) {
          flush();
          walkChildren(c, level, parent, wrapCtx(ctx, shown), true);
        }
        continue;
      }
      const interactive = isInteractive(c);
      const role = roleOf(c, tag);
      if (role || interactive) {
        flush();
        emit(c, tag, role || 'clickable', interactive, level, parent, ctx, shown, q);
        continue;
      }
      // A transparent element (div, span, p, ...): it adds no level of its own.
      if (interactiveOnly) {
        if (has(c, SEL)) {
          walkChildren(c, level, parent, wrapCtx(ctx, shown), q);
        }
        continue;
      }
      if (!has(c, STRUCT)) {
        if (q) continue;
        if (ctx === 1 || (ctx === 0 && !shown)) {
          flush();
          const t = clean(c.textContent, 1000);
          if (t) push({ d: level, role: 'text', name: t, hidden: true }, parent);
          continue;
        }
        const block = isBlock(cs);
        if (block) flush();
        buf += ' ' + textOf(c) + ' ';
        if (block) flush();
        continue;
      }
      flush();
      walkChildren(c, level, parent, wrapCtx(ctx, shown), q);
    }
    flush();
  };

  const emit = (el, tag, role, interactive, level, parent, ctx, shown, quiet) => {
    const childCtx = ctx === 0 && shown ? 0 : 2;
    const leaf = LEAF.has(role) || (FOLD.has(role) && !has(el, STRUCT));
    const want = !(quiet && !interactive) && (!interactiveOnly || (interactive && shown));
    let rec = null;
    if (want) {
      const name = nameOf(el, tag, role, leaf);
      rec = push({ d: level, role, name, hidden: ctx === 1 || (ctx === 0 && !shown) }, parent);
      if (rec) {
        const attrs = attrsOf(el, tag, role, name);
        if (attrs.length) rec.attrs = attrs;
        if (interactive || !NOREF.has(role)) {
          if (inShadow(el)) rec.noref = true;
          else rec.ref = refOf(el);
        }
      } else if (level >= maxDepth) {
        return;
      }
    }
    // A level is a line of the tree: wrappers that are left out add none.
    const next = rec ? level + 1 : level;
    if (tag === 'select') {
      if (rec && !interactiveOnly) {
        let n = 0;
        for (const o of el.options) {
          if (n++ >= 50) {
            rec.more = true;
            break;
          }
          push({ d: next, role: 'option', name: clean(o.textContent, 120), attrs: o.selected ? ['selected'] : undefined }, rec);
        }
      }
      return;
    }
    if (leaf) {
      if (has(el, SEL)) walkChildren(el, next, rec || parent, childCtx, true);
      return;
    }
    walkChildren(el, next, rec || parent, childCtx, quiet);
  };

  let root = document.body || document.documentElement;
  if (opts.refId) {
    root = document.querySelector('[data-nexo-ref="' + String(opts.refId).replace(/[^A-Za-z0-9_-]/g, '') + '"]');
    if (!root) return JSON.stringify({ found: false, nodes: [], capped: false });
    const tag = root.tagName.toLowerCase();
    const cs = getComputedStyle(root);
    const interactive = isInteractive(root);
    const role = roleOf(root, tag);
    if (role || interactive) {
      emit(root, tag, role || 'clickable', interactive, 0, null, 0, isShown(root, cs), false);
    } else {
      walkChildren(root, 0, null, isShown(root, cs) ? 0 : 1, false);
    }
  } else if (root) {
    walkChildren(root, 0, null, 0, false);
  }
  return JSON.stringify({ found: true, nodes, capped });
}
