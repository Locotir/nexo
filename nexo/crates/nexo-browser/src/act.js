(op, items) => {
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const FIELD = 'input:not([type=hidden]):not([type=submit]):not([type=button]):not([type=reset]):not([type=image]), select, textarea, [contenteditable=""], [contenteditable="true"]';
  const ANY = 'a, button, input, select, textarea, [role], [onclick], [contenteditable], label, summary';
  const names = (el, fields) => {
    if (!fields) return [norm(el.innerText || el.value), norm(el.getAttribute('aria-label')), norm(el.title)];
    const labels = el.labels ? [...el.labels].map((l) => norm(l.textContent)) : [];
    return [...labels, norm(el.getAttribute('aria-label')), norm(el.placeholder), norm(el.name), norm(el.id), norm(el.title)];
  };
  // A ref (e3), then a CSS selector, then the visible text, label or placeholder of an element.
  const resolve = (target, fields) => {
    if (/^e\d+$/.test(target)) return document.querySelector('[data-nexo-ref="' + target + '"]');
    try {
      const hit = document.querySelector(target);
      if (hit) return hit;
    } catch (e) { /* not a selector, so it is text */ }
    const want = norm(target);
    if (!want) return null;
    const shown = [...document.querySelectorAll(fields ? FIELD : ANY)].filter((el) => el.getClientRects().length > 0);
    return shown.find((el) => names(el, fields).some((n) => n === want))
      || shown.find((el) => names(el, fields).some((n) => n && n.includes(want)))
      || null;
  };
  const fire = (el, type) => el.dispatchEvent(new Event(type, { bubbles: true }));
  const choose = (el, value) => {
    const want = norm(value);
    const opts = [...el.options];
    let idx = opts.findIndex((o) => o.value === value);
    if (idx < 0) idx = opts.findIndex((o) => norm(o.textContent) === want);
    if (idx < 0 && want) idx = opts.findIndex((o) => norm(o.textContent).includes(want));
    if (idx < 0) return { ok: false, error: 'no option ' + JSON.stringify(value) };
    if (el.multiple) opts[idx].selected = true; else el.selectedIndex = idx;
    fire(el, 'input');
    fire(el, 'change');
    return { ok: true };
  };
  const tick = (el, on) => {
    if (el.checked !== on) el.click();
    return { ok: true };
  };
  const setValue = (el, value) => {
    const tag = el.tagName.toLowerCase();
    if (tag === 'select') return choose(el, value);
    if (tag === 'input' && (el.type === 'checkbox' || el.type === 'radio')) {
      return tick(el, /^(true|1|on|yes|checked)$/i.test(value));
    }
    el.focus();
    if (tag === 'input' || tag === 'textarea') {
      const proto = tag === 'input' ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
      Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
    } else if (el.isContentEditable) {
      el.textContent = value;
    } else {
      return { ok: false, error: 'not a field' };
    }
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: value }));
    fire(el, 'change');
    return { ok: true };
  };
  const run = (target, value) => {
    if (op === 'scroll') {
      const dir = norm(target);
      const page = window.innerHeight * 0.8;
      if (dir === 'down') window.scrollBy(0, page);
      else if (dir === 'up') window.scrollBy(0, -page);
      else if (dir === 'top') window.scrollTo(0, 0);
      else if (dir === 'bottom') window.scrollTo(0, document.documentElement.scrollHeight);
      else {
        const el = resolve(target, false);
        if (!el) return { ok: false };
        el.scrollIntoView({ block: 'center', inline: 'nearest' });
      }
      return { ok: true };
    }
    const el = resolve(target, op !== 'mark' && op !== 'exists');
    if (!el) return { ok: false };
    if (op === 'exists') return { ok: true };
    if (op === 'mark') {
      el.setAttribute('data-nexo-click', '1');
      return { ok: true };
    }
    if (op === 'select') {
      if (el.tagName.toLowerCase() !== 'select') return { ok: false, error: 'not a select' };
      return choose(el, value);
    }
    if (op === 'check') {
      if (!(el.tagName.toLowerCase() === 'input' && (el.type === 'checkbox' || el.type === 'radio'))) {
        return { ok: false, error: 'not a checkbox' };
      }
      return tick(el, value === 'true');
    }
    return setValue(el, value);
  };
  return JSON.stringify(items.map(([target, value]) => {
    try {
      return run(target, value);
    } catch (e) {
      return { ok: false, error: String(e) };
    }
  }));
}
