//! The page `nexo --wire` keeps beside its recordings: the run's timeline, each request split into its parts by
//! size, and links to the raw files. One self-contained file (no network), that reloads itself while live.

/// The report for the run in `dir`, its timeline given as JSON.
pub(crate) fn page(dir: &str, timeline_json: &str) -> String {
    // the data rides in a script element: `</` would end it early
    let data = timeline_json.replace("</", "<\\/");
    let dir = html_escape(dir);
    TEMPLATE.replace("{{DIR}}", &dir).replace("{{DATA}}", &data)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>nexo wire</title>
<style>
:root{--bg:#0f1115;--panel:#161922;--line:#262b36;--fg:#e6e8ee;--dim:#8a91a3;--accent:#6aa8ff;--ok:#4cc38a;--warn:#e3b341;--err:#f06a6a;--bar:#3a6fd8;--bar2:#2b3a55;--sys:#9b7cf0;--tools:#3fb6b0;--mono:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
@media (prefers-color-scheme: light){:root{--bg:#f7f8fa;--panel:#fff;--line:#e3e6ec;--fg:#1b1f27;--dim:#667085;--accent:#2457c5;--bar:#2f6fe4;--bar2:#dbe5f7;--sys:#7b5ad6;--tools:#178f8a}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:13px/1.45 system-ui,-apple-system,Segoe UI,sans-serif}
header{position:sticky;top:0;z-index:2;display:flex;flex-wrap:wrap;gap:12px;align-items:center;padding:10px 16px;background:var(--panel);border-bottom:1px solid var(--line)}
header h1{font-size:14px;margin:0;font-weight:650;letter-spacing:.2px}
header .dir{color:var(--dim);font-family:var(--mono);font-size:11px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:40vw}
.stats{display:flex;gap:14px;color:var(--dim)}
.stats b{color:var(--fg);font-weight:600}
.spacer{flex:1}
input[type=search]{background:var(--bg);color:var(--fg);border:1px solid var(--line);border-radius:6px;padding:5px 8px;width:220px}
button{background:var(--bg);color:var(--fg);border:1px solid var(--line);border-radius:6px;padding:5px 10px;cursor:pointer}
button.live{border-color:var(--ok);color:var(--ok)}
main{display:grid;grid-template-columns:minmax(380px,44%) 1fr;height:calc(100vh - 50px)}
#list{overflow:auto;border-right:1px solid var(--line)}
#detail{overflow:auto;padding:16px 20px}
.row{display:grid;grid-template-columns:44px 84px 1fr auto;gap:8px;align-items:center;padding:6px 12px;border-bottom:1px solid var(--line);cursor:pointer}
.row:hover{background:var(--panel)}
.row.sel{background:var(--panel);box-shadow:inset 3px 0 0 var(--accent)}
.row .n{font-family:var(--mono);color:var(--dim)}
.row .t{font-family:var(--mono);color:var(--dim);font-size:11px}
.row .m{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.row .r{font-family:var(--mono);font-size:12px;text-align:right;white-space:nowrap}
.note{display:grid;grid-template-columns:44px 84px 1fr;gap:8px;padding:3px 12px;border-bottom:1px solid var(--line);color:var(--dim);font-size:12px}
.note .k{font-family:var(--mono);font-size:11px;text-transform:uppercase;letter-spacing:.4px;margin-right:6px}
.note.warn{color:var(--warn)} .note.error{color:var(--err)}
.pill{display:inline-block;font-family:var(--mono);font-size:11px;padding:0 6px;border-radius:9px;border:1px solid currentColor}
.ok{color:var(--ok)} .bad{color:var(--err)} .run{color:var(--warn)}
.mini{height:4px;background:var(--bar2);border-radius:2px;margin-top:3px;overflow:hidden}
.mini i{display:block;height:100%;background:var(--bar)}
h2{font-size:15px;margin:0 0 4px}
.sub{color:var(--dim);margin-bottom:14px;font-family:var(--mono);font-size:11px;word-break:break-all}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:8px;margin-bottom:16px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:8px 10px}
.card .l{color:var(--dim);font-size:11px}
.card .v{font-family:var(--mono);font-size:14px;margin-top:2px}
.stack{display:flex;height:14px;border-radius:4px;overflow:hidden;background:var(--bar2);margin:6px 0 4px}
.stack span{display:block;height:100%}
.legend{display:flex;gap:14px;color:var(--dim);font-size:11px;margin-bottom:16px}
.legend i{display:inline-block;width:9px;height:9px;border-radius:2px;margin-right:5px;vertical-align:-1px}
.links{display:flex;gap:10px;margin-bottom:16px;flex-wrap:wrap}
.links a{color:var(--accent);text-decoration:none;font-family:var(--mono);font-size:12px;border:1px solid var(--line);border-radius:6px;padding:4px 8px}
table{width:100%;border-collapse:collapse;font-size:12px}
th{text-align:left;color:var(--dim);font-weight:500;padding:4px 6px;border-bottom:1px solid var(--line);cursor:pointer;user-select:none;white-space:nowrap}
td{padding:4px 6px;border-bottom:1px solid var(--line);vertical-align:top}
td.num{font-family:var(--mono);text-align:right;white-space:nowrap}
td.role{font-family:var(--mono);white-space:nowrap}
td.kinds{color:var(--dim);font-family:var(--mono);font-size:11px}
td.prev{color:var(--dim);max-width:0;width:100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
tr.top td{background:color-mix(in srgb,var(--err) 12%,transparent)}
.bar{height:8px;background:var(--bar2);border-radius:2px;min-width:80px}
.bar i{display:block;height:100%;background:var(--bar);border-radius:2px}
pre{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:10px;overflow:auto;font:12px/1.4 var(--mono);white-space:pre-wrap;word-break:break-word}
.empty{color:var(--dim);padding:40px;text-align:center}
@media (max-width:900px){main{grid-template-columns:1fr;height:auto}#list{max-height:45vh;border-right:0;border-bottom:1px solid var(--line)}input[type=search]{width:100%}header .dir{max-width:100%}}
</style>
</head>
<body>
<header>
  <h1>nexo wire</h1>
  <span class="dir" title="{{DIR}}">{{DIR}}</span>
  <span class="stats" id="stats"></span>
  <span class="spacer"></span>
  <input type="search" id="q" placeholder="filter: model, status, warn, text…">
  <button id="live" class="live" title="reload every 2 s">● live</button>
</header>
<main>
  <div id="list"></div>
  <div id="detail"><div class="empty">Pick a request on the left.</div></div>
</main>
<script id="data" type="application/json">{{DATA}}</script>
<script>
(function(){
  const store = {
    get(k){ try { return sessionStorage.getItem('wire.'+k); } catch(e){ return null; } },
    set(k,v){ try { sessionStorage.setItem('wire.'+k, v); } catch(e){} }
  };
  let items = [];
  try { items = JSON.parse(document.getElementById('data').textContent); } catch(e){}
  const ex = items.filter(i => i.type === 'exchange');
  const fmt = n => n == null ? '–' : n >= 1e6 ? (n/1e6).toFixed(2)+'M' : n >= 1e4 ? (n/1e3).toFixed(1)+'k' : String(n);
  const esc = s => String(s == null ? '' : s).replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
  const pad = n => String(n).padStart(4,'0');
  const inTok = u => u && (u.input_tokens ?? u.prompt_tokens ?? u.promptTokenCount);
  const cacheTok = u => u && ((u.cache_read_input_tokens||0) + (u.cache_creation_input_tokens||0) + ((u.prompt_tokens_details||{}).cached_tokens||0) + ((u.input_tokens_details||{}).cached_tokens||0));
  const outTok = u => u && (u.output_tokens ?? u.completion_tokens ?? u.candidatesTokenCount);

  // header numbers
  const errors = ex.filter(e => e.error || (e.status && e.status >= 400)).length;
  const sumIn = ex.reduce((a,e) => a + (inTok(e.usage)||0) + (cacheTok(e.usage)||0), 0);
  const sumOut = ex.reduce((a,e) => a + (outTok(e.usage)||0), 0);
  const warns = items.filter(i => i.type === 'note' && (i.kind === 'warn' || i.kind === 'error')).length;
  document.getElementById('stats').innerHTML =
    `<span><b>${ex.length}</b> request${ex.length===1?'':'s'}</span><span><b class="${errors?'bad':''}">${errors}</b> failed</span>` +
    `<span><b class="${warns?'run':''}">${warns}</b> warnings</span><span>in <b>${fmt(sumIn)}</b></span><span>out <b>${fmt(sumOut)}</b></span>`;

  const maxEst = Math.max(1, ...ex.map(e => e.estimated_tokens||0));
  let sel = parseInt(store.get('sel')||'0',10) || (ex.length ? ex[ex.length-1].n : 0);
  let sortBy = store.get('sort') || 'index';
  const q = document.getElementById('q');
  q.value = store.get('q') || '';

  function status(e){
    if (!e.done) return '<span class="pill run">…</span>';
    if (e.error && !e.status) return '<span class="pill bad">fail</span>';
    return `<span class="pill ${e.status>=400||e.error?'bad':'ok'}">${e.status||'?'}</span>`;
  }
  function noteText(i){
    const d = i.data || {};
    switch(i.kind){
      case 'view': return `<span class="k">view</span>${esc(d.policy)} sends ${fmt(d.view_tokens)} of ${fmt(d.history_tokens)} history · ${d.messages_in_view}/${d.messages_total} messages`+(d.ceiling?` · ceiling ${fmt(d.ceiling)}`:'')+(d.failed?` · <b class="bad">FAILED: ${esc(d.failed)}</b>`:'')+(d.note?` · ${esc(d.note)}`:'');
      case 'fit': return `<span class="k">fit</span>window ${fmt(d.window)} · request ${fmt(d.used)} + reply ${fmt(d.reply)}`+(d.tries?` · curator refit ×${d.tries}${d.shrink?` (−${fmt(d.shrink)})`:''}`:'')+(d.over?` · <b class="bad">over by ${fmt(d.over)}</b>`:'');
      case 'refused': return `<span class="k">refused</span>${esc(d.message)}`;
      default: return `<span class="k">${esc(i.kind)}</span>${esc(d.text || JSON.stringify(d))}`;
    }
  }
  function matches(i, f){
    if (!f) return true;
    return JSON.stringify(i).toLowerCase().includes(f);
  }
  function renderList(){
    const f = q.value.trim().toLowerCase();
    const html = items.filter(i => matches(i, f)).map(i => {
      if (i.type === 'note') {
        const lvl = i.kind === 'warn' || i.kind === 'error' ? i.kind : (i.data && i.data.failed) || (i.data && i.data.over) || i.kind === 'refused' ? 'error' : '';
        return `<div class="note ${lvl}"><span></span><span class="t">${esc(i.at)}</span><span>${noteText(i)}</span></div>`;
      }
      const u = i.usage || {};
      const real = inTok(u) != null ? (inTok(u)||0) + (cacheTok(u)||0) : null;
      return `<div class="row${i.n===sel?' sel':''}" data-n="${i.n}">
        <span class="n">#${i.n}</span><span class="t">${esc(i.at)}</span>
        <span class="m">${esc(i.model)} <span class="t">${esc(i.protocol)}</span>
          <div class="mini"><i style="width:${(100*(i.estimated_tokens||0)/maxEst).toFixed(1)}%"></i></div></span>
        <span class="r">${status(i)} ~${fmt(i.estimated_tokens)}${real!=null?` / ${fmt(real)}`:''} · ${i.total_ms!=null?(i.total_ms/1000).toFixed(1)+'s':'…'}</span>
      </div>`;
    }).join('');
    const list = document.getElementById('list');
    list.innerHTML = html || '<div class="empty">No requests yet. They appear here as nexo sends them.</div>';
    list.querySelectorAll('.row').forEach(r => r.onclick = () => { sel = +r.dataset.n; store.set('sel', sel); renderList(); renderDetail(); });
  }
  function renderDetail(){
    const e = ex.find(x => x.n === sel);
    const d = document.getElementById('detail');
    if (!e) { d.innerHTML = '<div class="empty">Pick a request on the left.</div>'; return; }
    const u = e.usage || {};
    const msgBytes = (e.parts||[]).reduce((a,p) => a + p.bytes, 0);
    const total = Math.max(1, e.system_bytes + e.tools_bytes + msgBytes);
    const pct = b => (100*b/total).toFixed(2);
    const parts = (e.parts||[]).slice();
    const top = parts.reduce((m,p) => p.bytes > (m?m.bytes:-1) ? p : m, null);
    if (sortBy === 'size') parts.sort((a,b) => b.bytes - a.bytes);
    const maxB = Math.max(1, ...parts.map(p => p.bytes));
    const card = (l,v) => `<div class="card"><div class="l">${l}</div><div class="v">${v}</div></div>`;
    d.innerHTML = `
      <h2>#${e.n} · ${esc(e.model)} ${status(e)}</h2>
      <div class="sub">${esc(e.at)} · POST ${esc(e.url)}</div>
      <div class="grid">
        ${card('estimated (chars/4)', '~'+fmt(e.estimated_tokens))}
        ${card('provider input', fmt(inTok(u)) + (cacheTok(u)?` <span class="t">+${fmt(cacheTok(u))} cache</span>`:''))}
        ${card('provider output', fmt(outTok(u)))}
        ${card('reply room asked', fmt(e.max_tokens))}
        ${card('messages', (e.parts||[]).length + (e.parts_hidden?` (+${e.parts_hidden})`:''))}
        ${card('images', e.images)}
        ${card('first byte', e.first_byte_ms!=null?(e.first_byte_ms/1000).toFixed(2)+'s':'–')}
        ${card('total', e.total_ms!=null?(e.total_ms/1000).toFixed(2)+'s':'running')}
        ${card('stop', esc(e.stop||'–'))}
      </div>
      <div class="stack">
        <span style="width:${pct(e.system_bytes)}%;background:var(--sys)" title="system"></span>
        <span style="width:${pct(e.tools_bytes)}%;background:var(--tools)" title="tools"></span>
        <span style="width:${pct(msgBytes)}%;background:var(--bar)" title="messages"></span>
      </div>
      <div class="legend"><span><i style="background:var(--sys)"></i>system ~${fmt(Math.round(e.system_bytes/4))}</span>
        <span><i style="background:var(--tools)"></i>${e.tools} tools ~${fmt(Math.round(e.tools_bytes/4))}</span>
        <span><i style="background:var(--bar)"></i>messages ~${fmt(Math.round(msgBytes/4))}</span></div>
      ${e.error?`<pre class="bad">${esc(e.error)}</pre>`:''}
      <div class="links">
        <a href="${pad(e.n)}-request.json" target="_blank">request.json</a>
        <a href="${pad(e.n)}-response.sse" target="_blank">response.sse</a>
        <a href="${pad(e.n)}-reply.txt" target="_blank">reply.txt</a>
      </div>
      <table><thead><tr>
        <th data-s="index">#</th><th>role</th><th>blocks</th><th data-s="size">~tokens ${sortBy==='size'?'▾':''}</th><th></th><th>starts with</th>
      </tr></thead><tbody>
      ${parts.map(p => `<tr class="${top && p.index===top.index && parts.length>1?'top':''}">
        <td class="num">${p.index}</td><td class="role">${esc(p.role)}</td>
        <td class="kinds">${esc(p.kinds.join(' '))}${p.images?` 🖼${p.images}`:''}</td>
        <td class="num">${fmt(p.tokens)}</td>
        <td><div class="bar"><i style="width:${(100*p.bytes/maxB).toFixed(1)}%"></i></div></td>
        <td class="prev" title="${esc(p.preview)}">${esc(p.preview)}</td></tr>`).join('')}
      </tbody></table>`;
    d.querySelectorAll('th[data-s]').forEach(th => th.onclick = () => { sortBy = th.dataset.s; store.set('sort', sortBy); renderDetail(); });
  }
  q.oninput = () => { store.set('q', q.value); renderList(); };
  // live: reload in place, keeping the pick, the sort, the filter and the scroll
  const btn = document.getElementById('live');
  let live = store.get('live') !== '0';
  const paint = () => { btn.textContent = live ? '● live' : '❚❚ paused'; btn.className = live ? 'live' : ''; };
  btn.onclick = () => { live = !live; store.set('live', live ? '1' : '0'); paint(); };
  paint();
  const list = document.getElementById('list'), det = document.getElementById('detail');
  renderList(); renderDetail();
  const follow = store.get('follow') !== '0';
  list.scrollTop = +(store.get('ls')||0); det.scrollTop = +(store.get('ds')||0);
  if (follow && !store.get('ls')) list.scrollTop = list.scrollHeight;
  setInterval(() => {
    if (!live || document.activeElement === q) return;
    store.set('ls', list.scrollTop); store.set('ds', det.scrollTop);
    location.reload();
  }, 2000);
})();
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    #[test]
    fn the_data_cannot_close_its_script_and_the_folder_is_escaped() {
        let p = super::page("/tmp/<x>", r#"[{"text":"</script><b>"}]"#);
        assert!(!p.contains("</script><b>"));
        assert!(p.contains("&lt;x&gt;"));
        assert!(p.contains(r#"<\/script><b>"#));
    }
}
