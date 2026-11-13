(selector) => {
  // The text of the page as a person reads it. With a CSS selector: that element. Without: the main content,
  // the <article>s or <main> (or role=main) that hold a real share of the text, else the densest block of
  // paragraphs, else the whole body. Always plain text, so menus, footers and sidebars stay out when there is a
  // main part; a pick that would drop most of the page is not taken, the body is read instead.
  // innerText is the rendered text (script and style source never in it); SVG has none, so textContent there.
  const text = (el) => ((el.innerText !== undefined ? el.innerText : el.textContent) || '').trim();
  if (selector) {
    let el = null;
    try {
      el = document.querySelector(selector);
    } catch (e) { /* not a valid selector: nothing matches */ }
    return JSON.stringify({ found: !!el, text: el ? text(el) : '', source: 'selector' });
  }
  const body = document.body || document.documentElement;
  const all = text(body);
  const shown = (el) => el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
  const MIN = 200;
  // A pick must hold at least 30% of the page text, and be 200 characters (or half of a very short page).
  const ok = (c) => !!c && c.len >= all.length * 0.3 && (c.len >= MIN || c.len >= all.length * 0.5);
  const cand = (text_, source) => ({ len: text_.length, text: text_, source });
  const best = (list) => {
    const top = list.filter(shown).map((el) => [el, text(el)]).sort((a, b) => b[1].length - a[1].length)[0];
    return top ? cand(top[1], null) : null;
  };
  // Articles that are not inside another article. One that holds most of the article text is the story; several
  // of similar weight (a feed, a list of cards) are all of it, in page order.
  const articles = [...document.querySelectorAll('article')]
    .filter((a) => shown(a) && !(a.parentElement && a.parentElement.closest('article')))
    .map((a) => text(a))
    .filter((t) => t);
  let art = null;
  if (articles.length) {
    const total = articles.reduce((n, t) => n + t.length, 0);
    const top = articles.reduce((a, b) => (b.length > a.length ? b : a));
    art = top.length >= total * 0.6 ? cand(top, 'article') : cand(articles.join('\n\n'), 'articles (' + articles.length + ')');
  }
  const main = best([...document.querySelectorAll('main, [role="main"]')]);
  if (main) main.source = 'main';
  const describe = (el) => el.tagName.toLowerCase() + (el.id ? '#' + el.id : '')
    + (el.classList.length ? '.' + el.classList[0] : '');
  const reply = (c) => JSON.stringify({ found: true, text: c.text, source: c.source });
  if (ok(art) && (!ok(main) || art.len >= main.len * 0.5)) return reply(art);
  if (ok(main)) return reply(main);
  // Densest block: paragraphs score for their parent (and half for its parent), links count against it.
  const score = new Map();
  for (const p of document.querySelectorAll('p, pre, blockquote')) {
    const t = text(p);
    if (t.length < 25 || !shown(p)) continue;
    const s = 1 + (t.split(',').length - 1) + Math.min(Math.floor(t.length / 100), 3);
    const parent = p.parentElement;
    if (!parent || parent === body || parent === document.documentElement) continue;
    score.set(parent, (score.get(parent) || 0) + s);
    const grand = parent.parentElement;
    if (grand && grand !== body && grand !== document.documentElement) score.set(grand, (score.get(grand) || 0) + s / 2);
  }
  let top = null;
  let topScore = 0;
  for (const [el, s] of score) {
    const len = text(el).length || 1;
    const links = [...el.querySelectorAll('a')].reduce((n, a) => n + text(a).length, 0);
    const adjusted = s * (1 - Math.min(links / len, 1));
    if (adjusted > topScore) {
      top = el;
      topScore = adjusted;
    }
  }
  if (top && topScore >= 4 && text(top).length >= MIN && text(top).length >= all.length * 0.3) {
    return reply(cand(text(top), 'densest block ' + describe(top)));
  }
  return JSON.stringify({ found: true, text: all, source: 'body' });
}
