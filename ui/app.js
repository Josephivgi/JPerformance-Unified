'use strict';
/* JPerformance Unified window. Reads everything through Tauri commands; nothing is kept in the browser. */

const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

/* ================= Icons ================= */
const I = {
  logo: '<svg viewBox="0 0 24 24"><path d="M3 12h4l2-5 4 10 2-5h6"/></svg>',
  grid: '<path d="M2.5 2.5h4.5v4.5H2.5zM9 2.5h4.5v4.5H9zM2.5 9h4.5v4.5H2.5zM9 9h4.5v4.5H9z"/>',
  alert: '<path d="M8 1.8 14.6 13.5H1.4z"/><path d="M8 6.2v3.3M8 11.6v.1"/>',
  info: '<circle cx="8" cy="8" r="6.3"/><path d="M8 7.2v4M8 4.9v.1"/>',
  crit: '<circle cx="8" cy="8" r="6.3"/><path d="M8 4.6v4.2M8 11.2v.1"/>',
  pc: '<rect x="1.8" y="2.5" width="12.4" height="8.3" rx="1.3"/><path d="M5.5 13.5h5M8 10.8v2.7"/>',
  server: '<rect x="2" y="2.2" width="12" height="4.8" rx="1.2"/><rect x="2" y="9" width="12" height="4.8" rx="1.2"/><path d="M4.6 4.6h.1M4.6 11.4h.1"/>',
  key: '<circle cx="5.2" cy="10.8" r="3"/><path d="m7.4 8.6 6-6M11.2 4.8l1.8 1.8M9.6 6.4l1.4 1.4"/>',
  gear: '<circle cx="8" cy="8" r="2.2"/><path d="M8 1.6v1.8M8 12.6v1.8M1.6 8h1.8M12.6 8h1.8M3.5 3.5l1.3 1.3M11.2 11.2l1.3 1.3M3.5 12.5l1.3-1.3M11.2 4.8l1.3-1.3"/>',
  refresh: '<path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9"/><path d="M13.6 2.2v3.2h-3.2"/>',
  play: '<path d="M5 3.2v9.6L12.6 8z"/>',
  stop: '<rect x="4" y="4" width="8" height="8" rx="1.2"/>',
  trash: '<path d="M2.8 4.3h10.4M6.3 4.3V2.8h3.4v1.5M4.2 4.3l.6 9h6.4l.6-9"/>',
  pause: '<path d="M5.5 3.5v9M10.5 3.5v9"/>',
  search: '<circle cx="7" cy="7" r="4.6"/><path d="m10.5 10.5 3.3 3.3"/>',
  x: '<path d="m4 4 8 8M12 4l-8 8"/>',
  shield: '<path d="M8 1.8 13.2 3.8v4c0 3-2.2 5.3-5.2 6.4-3-1.1-5.2-3.4-5.2-6.4v-4z"/><path d="m5.8 8 1.6 1.6 2.9-3"/>',
  download: '<path d="M8 2.2v8M4.6 7l3.4 3.4L11.4 7M2.6 13.6h10.8"/>',
};
const ic = (n, cls = 'i') => `<svg class="${cls}" viewBox="0 0 16 16">${I[n]}</svg>`;

/* ================= Formatting ================= */
const esc = v => String(v ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const pct = v => v == null || isNaN(v) ? '–' : v < 10 ? v.toFixed(1) + '%' : Math.round(v) + '%';
function bytes(b) {
  const u = ['B', 'KB', 'MB', 'GB', 'TB']; let v = Math.max(0, b || 0), i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return (i === 0 || v >= 100 ? Math.round(v) : v.toFixed(1)) + ' ' + u[i];
}
const rate = b => bytes(b) + '/s';
function ofBytes(a, b) {
  const x = bytes(a), y = bytes(b), ux = x.split(' ')[1];
  return ux === y.split(' ')[1] ? `${x.split(' ')[0]} of ${y}` : `${x} of ${y}`;
}
function dur(s) {
  s = Math.max(0, Math.round(s));
  if (s < 90) return s + ' s';
  if (s < 5400) return Math.round(s / 60) + ' min';
  if (s < 172800) { const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60); return m ? `${h} h ${m} min` : `${h} h`; }
  const d = Math.floor(s / 86400), h = Math.round((s % 86400) / 3600); return h ? `${d} d ${h} h` : `${d} d`;
}
const nowS = () => Math.floor(Date.now() / 1000);
const ago = ts => !ts ? 'never' : nowS() - ts < 10 ? 'just now' : dur(nowS() - ts) + ' ago';
const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const hm = d => String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
function when(ts) {
  if (!ts) return '';
  const d = new Date(ts * 1000), n = new Date();
  if (d.toDateString() === n.toDateString()) return 'Today ' + hm(d);
  const y = new Date(n); y.setDate(n.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return 'Yesterday ' + hm(d);
  if (n - d < 6 * 864e5) return DAYS[d.getDay()] + ' ' + hm(d);
  return d.toLocaleDateString(undefined, { day: 'numeric', month: 'short' }) + ' ' + hm(d);
}
function axisLabel(ts, span) {
  const d = new Date(ts * 1000);
  if (span <= 2 * 86400) return hm(d);
  return DAYS[d.getDay()] + ' ' + d.getDate();
}
function tipTime(ts, bucket) {
  const d = new Date(ts * 1000);
  const base = when(ts);
  return bucket <= 5 ? base + ':' + String(d.getSeconds()).padStart(2, '0') : base;
}

/* ================= State ================= */
const RANGES = [[3600, 'Hour'], [21600, '6 h'], [86400, 'Day'], [604800, 'Week'], [2592000, 'Month']];
const S = {
  page: 'overview', machine: null, tab: 'now', range: 86400, sort: 'cpu', appSel: null,
  histApp: '', pinned: null, filter: 'all', srvKind: 'all', recSel: null, recMinutes: 30,
  asked: new Set(), side: null, update: { state: 'idle' }, theme: 'system',
};
let renderGen = 0;

function go(page, machine, tab) {
  S.page = page; S.machine = machine || null; S.pinned = null; S.recSel = null;
  S.tab = tab || (machine === 'pc' ? 'now' : 'summary');
  S.sort = machine && machine !== 'pc' ? 'mem' : 'cpu';
  render(true);
}

/* ================= Small pieces ================= */
const meterCls = p => p >= 95 ? 'crit' : p >= 85 ? 'warn' : '';
const meter = (p, thin) => `<div class="meter ${thin ? 'thin' : ''} ${meterCls(p)}"><i style="width:${Math.max(1.5, Math.min(100, p || 0))}%"></i></div>`;
const statusText = s => s.failed ? `<span class="status crit">${ic('crit')}${esc(s.label || s.server?.label)}</span>`
  : !s.enabled && s.enabled !== undefined ? '<span class="status">Paused</span>'
  : (s.status || s.server?.status) === 'connecting' ? '<span class="status">Connecting</span>' : '';
const seg = (attr, opts, cur) => `<span class="seg">${opts.map(([k, l]) => `<button data-${attr}="${k}" aria-pressed="${String(cur) === String(k)}">${l}</button>`).join('')}</span>`;
const rangePicker = () => `<span class="seg" role="group" aria-label="Time range">${RANGES.map(([r, l]) => `<button data-range="${r}" aria-pressed="${S.range === r}">${l}</button>`).join('')}</span>`;

function spark(values, max, h = 26) {
  const vals = values.filter(v => v != null);
  if (vals.length < 2) return `<svg class="spark" viewBox="0 0 160 ${h}" aria-hidden="true"></svg>`;
  const w = 160, n = vals.length, m = max || Math.max(...vals) * 1.15 || 1;
  const pts = vals.map((v, i) => [(i / (n - 1)) * w, h - (v / m) * (h - 3) - 1.5]);
  const d = pts.map((p, i) => (i ? 'L' : 'M') + p[0].toFixed(1) + ' ' + p[1].toFixed(1)).join('');
  return `<svg class="spark" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" aria-hidden="true">
    <path d="${d}L${w} ${h}L0 ${h}Z" fill="var(--s1)" opacity=".1"/>
    <path d="${d}" fill="none" stroke="var(--s1)" stroke-width="2" vector-effect="non-scaling-stroke" stroke-linejoin="round" stroke-linecap="round"/>
  </svg>`;
}

function head(title, sub, actions = '', tabs = null) {
  return `<header class="head ${tabs ? '' : 'no-tabs'}">
    <div class="head-text"><h1>${title}</h1><div class="sub">${sub}</div></div>
    <div class="actions">${actions}</div>
    ${tabs ? `<div class="tabs" role="tablist">${tabs.map(([k, l, n]) => `<button class="tab" role="tab" aria-selected="${S.tab === k}" data-tab="${k}">${l}${n != null ? `<span class="n">${n}</span>` : ''}</button>`).join('')}</div>` : ''}
  </header>`;
}

const LEVEL_TEXT = { crit: 'Needs you', warn: 'Worth a look', info: 'For your information' };
const findingRow = f => `<div class="finding ${f.level}">${ic(f.level === 'crit' ? 'crit' : f.level === 'warn' ? 'alert' : 'info', 'i fi')}
  <div><div class="t">${esc(f.title)}</div><div class="d">${esc(f.detail)}</div>
  <div class="m"><span class="kind">${ic(f.machine_id === 'pc' ? 'pc' : 'server')}${esc(f.machine)}</span><span>${LEVEL_TEXT[f.level]}</span>${f.retry ? `<button class="btn small" data-retry="${esc(f.machine_id)}">${ic('refresh')}Retry now</button>` : ''}</div></div>
  <span class="when">${when(f.at)}</span></div>`;

/* ================= Line charts ================= */
// A chart is [ts, value|null] points from `from` to `to`. Gaps longer than 2.5 buckets break the line.
const CHARTS = [];
function lineChart({ id, title, pts, from, to, bucket, max, fmt, h = 120, group, tip }) {
  CHARTS.push({ id, title, pts, from, to, bucket, max, fmt, h, group, tip });
  const vals = pts.map(p => p[1]).filter(v => v != null);
  const cur = vals.length ? vals[vals.length - 1] : null, peak = vals.length ? Math.max(...vals) : null;
  return `<div class="chart" data-chart="${id}">
    <div class="chart-top"><b>${title}</b><span class="v">${vals.length ? `latest ${fmt(cur)} · peak ${fmt(peak)}` : 'No samples in this range'}</span></div>
    <div class="plot"><div class="yax" id="${id}-y" style="height:${h}px"></div><div class="canvas" id="${id}-c" style="height:${h}px"></div><div class="xax" id="${id}-x"></div></div>
  </div>`;
}
function niceMax(v) { const p = Math.pow(10, Math.floor(Math.log10(v || 1))); for (const m of [1, 2, 2.5, 5, 10]) if (m * p >= v * 1.1) return m * p; return 10 * p; }
function chartGeom(c, W) {
  const vals = c.pts.map(p => p[1]).filter(v => v != null);
  const max = c.max || niceMax(vals.length ? Math.max(...vals) : 1);
  const span = Math.max(1, c.to - c.from);
  return { max, x: ts => ((ts - c.from) / span) * W, y: v => c.h - (Math.min(v, max) / max) * c.h };
}
function drawCharts() {
  for (const c of CHARTS) {
    const el = document.getElementById(c.id + '-c'); if (!el) continue;
    const W = el.clientWidth, H = c.h;
    if (!W) continue;
    const { max, x, y } = chartGeom(c, W);
    const gap = Math.max(c.bucket * 2.5, 15);
    // One line with breaks at gaps; each unbroken run gets its own area wash.
    let d = '', area = '', run = '', first = null, prev = null;
    const close = () => { if (first) area += `${run}L${x(prev[0]).toFixed(1)} ${H}L${x(first[0]).toFixed(1)} ${H}Z`; };
    for (const p of c.pts) {
      if (p[1] == null) continue;
      const xy = x(p[0]).toFixed(1) + ' ' + y(p[1]).toFixed(1);
      if (!prev || p[0] - prev[0] > gap) { close(); first = p; run = 'M' + xy; d += 'M' + xy; }
      else { run += 'L' + xy; d += 'L' + xy; }
      prev = p;
    }
    close();
    const ticks = [0, max / 2, max];
    const pinX = S.pinned != null && S.pinned >= c.from && S.pinned <= c.to ? x(S.pinned) : null;
    el.innerHTML = `<svg width="${W}" height="${H}" role="img" aria-label="${esc(c.title)} chart">
      ${ticks.map(t => `<line x1="0" x2="${W}" y1="${y(t)}" y2="${y(t)}" stroke="${t === 0 ? 'var(--axis)' : 'var(--grid)'}" stroke-width="1"/>`).join('')}
      <path d="${area}" fill="var(--s1)" opacity=".1"/>
      <path d="${d}" fill="none" stroke="var(--s1)" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
      <g class="hover" style="display:none"><line y1="0" y2="${H}" stroke="var(--ink-2)" stroke-width="1"/><circle r="4.5" fill="var(--s1)" stroke="var(--surface)" stroke-width="2"/></g>
      ${pinX != null ? `<line x1="${pinX}" x2="${pinX}" y1="0" y2="${H}" stroke="var(--accent)" stroke-width="2"/>` : ''}
    </svg>`;
    document.getElementById(c.id + '-y').innerHTML = ticks.map(t => `<span style="top:${y(t)}px">${c.fmt(t, true)}</span>`).join('');
    const span = c.to - c.from;
    document.getElementById(c.id + '-x').innerHTML = [0, 1, 2, 3, 4].map(k => { const ts = c.from + span * k / 4; return `<span style="left:${x(ts)}px">${axisLabel(ts, span)}</span>`; }).join('');
    const at = e => nearest(c, c.from + ((e.clientX - el.getBoundingClientRect().left) / W) * span);
    el.onmousemove = e => { S.hovering = true; const p = at(e); if (p) hoverAt(c, p[0]); };
    el.onmouseleave = () => { S.hovering = false; clearHover(c.group); };
    el.onclick = e => { const p = at(e); if (p && c.group) { S.pinned = p[0]; render(); } };
  }
}
function nearest(c, ts) {
  let best = null, bd = Infinity;
  for (const p of c.pts) { if (p[1] == null) continue; const dd = Math.abs(p[0] - ts); if (dd < bd) { bd = dd; best = p; } }
  return best;
}
let tipReq = 0;
function hoverAt(c, ts) {
  const peers = c.group ? CHARTS.filter(o => o.group === c.group) : [c];
  for (const o of peers) {
    const el = document.getElementById(o.id + '-c'); if (!el) continue;
    const g = el.querySelector('.hover'); if (!g) continue;
    const p = nearest(o, ts);
    if (!p) { g.style.display = 'none'; continue; }
    const W = el.clientWidth, { x, y } = chartGeom(o, W), px = x(p[0]), py = y(p[1]);
    g.style.display = '';
    g.querySelector('line').setAttribute('x1', px); g.querySelector('line').setAttribute('x2', px);
    g.querySelector('circle').setAttribute('cx', px); g.querySelector('circle').setAttribute('cy', py);
    let tip = el.querySelector('.tip');
    if (o === c) {
      if (!tip) { tip = document.createElement('div'); tip.className = 'tip'; el.appendChild(tip); }
      const extraId = 'tx-' + o.id;
      const html = `<div class="tt">${tipTime(p[0], o.bucket)}</div><div class="tv">${o.fmt(p[1])}</div><div id="${extraId}"></div>${o.group ? '<div class="th">Click to keep this moment open</div>' : ''}`;
      if (tip.dataset.ts !== String(p[0])) {
        tip.dataset.ts = p[0]; tip.innerHTML = html;
        if (o.tip) {
          const req = ++tipReq;
          clearTimeout(hoverAt.t);
          hoverAt.t = setTimeout(async () => {
            const extra = await o.tip(p[0], p[1]).catch(() => '');
            const box = document.getElementById(extraId);
            if (req === tipReq && box) box.innerHTML = extra;
          }, 120);
        }
      }
      const left = px + 14 + 280 > W ? px - 14 - tip.offsetWidth : px + 14;
      tip.style.left = left + 'px'; tip.style.top = Math.max(0, Math.min(o.h - tip.offsetHeight, py - 30)) + 'px';
    } else if (tip) tip.remove();
  }
}
function clearHover(group) {
  for (const o of CHARTS) {
    if (group && o.group !== group) continue;
    const el = document.getElementById(o.id + '-c'); if (!el) continue;
    const g = el.querySelector('.hover'); if (g) g.style.display = 'none';
    const t = el.querySelector('.tip'); if (t) t.remove();
  }
}
const momentCache = new Map();
async function moment(cmd, args) {
  const k = cmd + JSON.stringify(args);
  if (!momentCache.has(k)) momentCache.set(k, invoke(cmd, args));
  if (momentCache.size > 200) momentCache.delete(momentCache.keys().next().value);
  return momentCache.get(k);
}
const tipRows = (list, fmt) => list.slice(0, 3).map(x => `<div class="tr"><span>${esc(x.name)}</span><span>${fmt(x.v)}</span></div>`).join('');

/* ================= Sidebar ================= */
function renderSide() {
  const sd = S.side; if (!sd) return;
  const cur = (p, m) => (S.page === p && (m === undefined || S.machine === m)) ? 'aria-current="page"' : '';
  const servers = sd.ssh ? sd.servers : [];
  const online = servers.filter(s => s.enabled && !s.failed && s.status === 'up').length;
  const watchedN = servers.filter(s => s.enabled).length;
  const pcRow = `<button class="nav machine-nav" ${cur('machine', 'pc')} data-go="machine" data-m="pc">
      <span class="row1"><span class="name">${esc(sd.pc.name)}</span></span>
      <span class="minis"><span class="mini">CPU ${meter(sd.pc.cpu, true)}</span><span class="mini">Mem ${meter(sd.pc.mem_pct, true)}</span></span></button>`;
  const srvRow = s => `<button class="nav machine-nav" ${cur('machine', s.alias)} data-go="machine" data-m="${esc(s.alias)}">
      <span class="row1"><span class="name">${esc(s.alias)}</span></span>
      ${s.failed ? `<span class="why">${ic('crit')}${esc(s.label)} · ${ago(s.checked_at)}</span>`
        : !s.enabled ? '<span class="why" style="color:var(--muted)">Paused</span>'
        : s.cpu == null ? '<span class="why" style="color:var(--muted)">Connecting</span>'
        : `<span class="minis"><span class="mini">CPU ${meter(s.cpu, true)}</span><span class="mini">Mem ${meter(s.mem_pct, true)}</span></span>`}
    </button>`;
  const u = S.update;
  const upd = u.state === 'available' ? `<div class="update">${ic('download')}<span class="grow">Version ${esc(u.version)} is ready</span><button class="btn small" data-dialog="update">What's new</button></div>`
    : u.state === 'downloading' ? `<div class="update">${ic('download')}<span class="grow">Downloading ${u.total ? Math.round(u.done / u.total * 100) + '%' : ''}</span></div>`
    : u.state === 'restarting' ? `<div class="update">${ic('refresh')}<span class="grow">Restarting to finish the update</span></div>` : '';
  const recLine = sd.recorder
    ? `<div class="rec"><div><b>Recording ${watchedN ? `${watchedN + 1} machines` : 'this PC'}</b><span class="muted">Every 5 s, keeps ${sd.retention} days${sd.autostart ? ' · starts with Windows' : ''}</span></div></div>`
    : `<div class="rec"><div><span class="why">${ic('crit')}Not recording</span><span class="muted">The recorder is not running.</span><button class="btn small" data-action="start-recorder" style="margin-top:6px">${ic('play')}Start recording</button></div></div>`;
  const html = `
    <div class="brand"><div class="logo">${I.logo}</div><div><div class="brand-name">JPerformance</div><div class="brand-sub">Your PC and servers over time</div></div></div>
    <nav class="side-scroll" aria-label="Main">
      <button class="nav" ${cur('overview')} data-go="overview">${ic('grid')}Overview</button>
      <button class="nav" ${cur('findings')} data-go="findings">${ic('alert')}Findings${S.attn != null ? `<span class="count ${S.attn ? 'attn' : ''}">${S.attn}</span>` : ''}</button>
      <div class="side-label"><span>This PC</span></div>
      ${pcRow}
      <div class="side-label"><span>Servers</span><span>${sd.ssh ? (watchedN ? `${online} of ${watchedN} online` : 'None picked') : 'Off'}</span></div>
      ${sd.ssh ? servers.map(srvRow).join('') + `<button class="nav" ${cur('keys')} data-go="keys">${ic('key')}SSH keys</button>`
        : `<button class="nav" data-go="settings">${ic('server')}Add servers over SSH</button>`}
    </nav>
    <div class="side-foot">${upd}${recLine}
      <button class="nav" ${cur('settings')} data-go="settings">${ic('gear')}Settings</button>
    </div>`;
  setHTML(document.getElementById('side'), html);
}

/* ================= Overview ================= */
async function pageOverview() {
  const d = await invoke('overview');
  const pc = d.pc, servers = d.servers;
  const attn = d.findings.filter(f => f.level !== 'info');
  S.attn = attn.length;
  const pcLatest = pc.latest?.point;
  const memTotal = pc.latest?.mem_total || 0;
  const disk = pc.disks[0];
  const watchedS = servers.filter(s => s.server.enabled);
  const okS = watchedS.filter(s => !s.failed && s.latest);
  const n = 1 + watchedS.length;
  const pcRow = `<tr data-go="machine" data-m="pc">
      <td><div class="mname"><b>${esc(pc.host.name)}</b><span class="line2"><span class="kind">${ic('pc')}This PC</span></span></div></td>
      <td><div class="cellv"><div class="top"><b class="num">${pct(pcLatest?.cpu)}</b><span>${pc.host.cores} threads</span></div>${spark(pc.cpu_hour.map(p => p[1]), 100)}</div></td>
      <td><div class="cellv"><div class="top"><b class="num">${pct(pcLatest?.mem_pct)}</b><span>${ofBytes(pcLatest?.mem_used, memTotal)}</span></div>${meter(pcLatest?.mem_pct)}</div></td>
      <td><div class="cellv">${disk ? `<div class="top"><b class="num">${Math.round(disk.pct)}%</b><span>${esc(disk.mount)} ${bytes(disk.free)} free</span></div>${meter(disk.pct)}` : '<span class="muted">–</span>'}</div></td>
      <td><div class="cellv"><div class="top"><span>in</span><b class="num">${rate(pc.net_in)}</b></div><div class="top"><span>out</span><b class="num">${rate(pc.net_out)}</b></div></div></td>
      <td><div class="cellv">${pc.top_mem ? `<span class="ell" title="${esc(pc.top_mem.app)}">${esc(pc.top_mem.app)}</span><span class="muted num" style="font-size:11.5px">${bytes(pc.top_mem.mem)} of memory</span>` : '<span class="muted">–</span>'}</div></td></tr>`;
  const srvRow = s => {
    const name = `<td><div class="mname"><b>${esc(s.alias)}</b><span class="line2"><span class="kind">${ic('server')}Server</span>${statusText(s.server.enabled ? s : { enabled: false })}</span>${s.via ? `<span class="line2"><span class="chip">via ${esc(s.via)}</span></span>` : ''}</div></td>`;
    if (!s.server.enabled) return `<tr data-go="machine" data-m="${esc(s.alias)}">${name}<td colspan="4"><span class="muted">Not being watched. Its history is kept.</span></td><td><button class="btn small" data-watch="${esc(s.alias)}" data-on="1">${ic('play')}Resume</button></td></tr>`;
    if (s.failed) return `<tr data-go="machine" data-m="${esc(s.alias)}">${name}<td colspan="4"><div class="offline-cell">${esc(s.hint || s.server.detail)} Last checked ${ago(s.server.checked_at)}.</div></td><td><button class="btn small" data-retry="${esc(s.alias)}">${ic('refresh')}Retry now</button></td></tr>`;
    const p = s.latest?.point;
    if (!p) return `<tr data-go="machine" data-m="${esc(s.alias)}">${name}<td colspan="5"><span class="muted">Logging in. The first readings show up within a few seconds.</span></td></tr>`;
    return `<tr data-go="machine" data-m="${esc(s.alias)}">${name}
      <td><div class="cellv"><div class="top"><b class="num">${pct(p.cpu)}</b><span>${s.server.cores} cores</span></div>${spark(s.cpu_hour.map(q => q[1]), 100)}</div></td>
      <td><div class="cellv"><div class="top"><b class="num">${pct(s.latest.mem_pct)}</b><span>${ofBytes(s.latest.mem_used, p.mem_total)}</span></div>${meter(s.latest.mem_pct)}</div></td>
      <td><div class="cellv">${s.fullest ? `<div class="top"><b class="num">${Math.round(s.fullest.pct)}%</b><span>${esc(s.fullest.mount)} ${bytes(s.fullest.avail)} free</span></div>${meter(s.fullest.pct)}` : '<span class="muted">–</span>'}</div></td>
      <td><div class="cellv"><div class="top"><span>in</span><b class="num">${rate(p.net_rx)}</b></div><div class="top"><span>out</span><b class="num">${rate(p.net_tx)}</b></div></div></td>
      <td><div class="cellv">${s.top ? `<span class="ell" title="${esc(s.top.label)}"><span class="chip">${esc(s.top.kind)}</span> ${esc(s.top.label)}</span><span class="muted num" style="font-size:11.5px">${bytes(s.top.mem)} of memory</span>` : '<span class="muted">–</span>'}</div></td></tr>`;
  };
  // Busiest machine and fullest disk across everything that answers.
  const machines = [{ name: pc.host.name, cpu: pcLatest?.cpu ?? 0, why: pc.top_cpu ? `mostly ${pc.top_cpu.name}` : '' }, ...okS.map(s => ({ name: s.alias, cpu: s.latest.point.cpu, why: s.top ? `${s.top.label} uses the most memory` : '' }))];
  const busiest = machines.reduce((a, b) => b.cpu > a.cpu ? b : a);
  const disks = [...(disk ? [{ pct: disk.pct, where: `${disk.mount} on ${pc.host.name}`, free: disk.free }] : []), ...okS.filter(s => s.fullest).map(s => ({ pct: s.fullest.pct, where: `${s.fullest.mount} on ${s.alias}`, free: s.fullest.avail }))];
  const fullest = disks.length ? disks.reduce((a, b) => b.pct > a.pct ? b : a) : null;
  const down = watchedS.filter(s => s.failed);
  const first = n === 1
    ? `<div><div class="k">Recording</div><div class="v">${d.recorder ? 'On' : 'Off'}</div><div class="d">${d.recorder ? 'Every 5 s, even with this window closed' : 'Start it from the sidebar'}</div></div>`
    : `<div><div class="k">Online</div><div class="v ${down.length ? 'crit' : ''}">${n - down.length} of ${n}</div><div class="d">${down.length ? `${esc(down[0].alias)} is not answering` : 'Everything answers'}</div></div>`;
  return head('Overview', `<span>${n === 1 ? 'This PC' : `This PC and ${n - 1} server${n === 2 ? '' : 's'}`}</span><span>Updated ${hm(new Date())}</span>`, `<button class="btn" data-action="refresh">${ic('refresh')}Refresh</button>`) + `
  <div class="body">
    <div class="summary">
      ${first}
      <div><div class="k">Needs attention</div><div class="v ${attn.some(f => f.level === 'crit') ? 'crit' : ''}">${attn.length}</div><div class="d">${attn[0] ? esc(attn[0].title) : 'Nothing right now'}</div></div>
      <div><div class="k">Busiest right now</div><div class="v" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(busiest.name)}</div><div class="d">${pct(busiest.cpu)} CPU${busiest.why ? ', ' + esc(busiest.why) : ''}</div></div>
      <div><div class="k">Fullest disk</div><div class="v">${fullest ? Math.round(fullest.pct) + '%' : '–'}</div><div class="d">${fullest ? `${esc(fullest.where)}, ${bytes(fullest.free)} free` : ''}</div></div>
    </div>
    <section class="panel fleet" aria-label="Machines">
      <table><colgroup><col style="width:22%"><col style="width:16%"><col style="width:16%"><col style="width:16%"><col style="width:12%"><col style="width:18%"></colgroup><thead><tr><th>Machine</th><th>CPU, last hour</th><th>Memory</th><th>Fullest disk</th><th>Network</th><th>Using the most</th></tr></thead>
      <tbody>${pcRow}${servers.map(srvRow).join('')}</tbody></table>
    </section>
    ${d.ssh ? '' : `<div class="add-servers"><div class="ic">${ic('server')}</div><div><h3>See your servers here too</h3><p>JPerformance can log in to the Linux servers in your SSH config and record them the same way it records this PC. It only reads, and installs nothing on them. This is off until you turn it on.</p></div><button class="btn primary" data-go="settings">${ic('search')}Find my servers</button></div>`}
    <section class="panel">
      <div class="panel-head"><h2>Latest findings</h2><span class="hint">Last 24 hours</span><span class="grow"></span><button class="btn small ghost" data-go="findings">See all ${d.findings.length}</button></div>
      <div class="findings" style="margin-top:8px">${d.findings.length ? d.findings.slice(0, 4).map(findingRow).join('') : '<div class="empty">Nothing found. Everything looks fine.</div>'}</div>
    </section>
  </div>`;
}

/* ================= Findings ================= */
async function pageFindings() {
  const list = await invoke('findings', { range: S.range });
  const machines = [...new Map(list.map(f => [f.machine_id, f.machine])).entries()];
  if (S.filter !== 'all' && !machines.some(([id]) => id === S.filter)) machines.push([S.filter, S.filter === 'pc' ? 'This PC' : S.filter]);
  const shown = list.filter(f => S.filter === 'all' || f.machine_id === S.filter);
  if (S.range === 86400) S.attn = list.filter(f => f.level !== 'info').length;
  return head('Findings', 'Plain explanations of what slowed a machine down, when, and why. Worst first.') + `
  <div class="body">
    <div class="toolbar"><span class="lbl">Machine</span><select class="select" id="ffilter"><option value="all">All machines</option>${machines.map(([id, name]) => `<option value="${esc(id)}" ${S.filter === id ? 'selected' : ''}>${esc(name)}</option>`).join('')}</select>
      <span class="grow"></span>${rangePicker()}</div>
    <section class="panel"><div class="findings">${shown.length ? shown.map(findingRow).join('') : '<div class="empty">Nothing found in this range.</div>'}</div></section>
  </div>`;
}

/* ================= This PC ================= */
const cpuFmt = (v, axis) => axis ? Math.round(v) + '%' : pct(v);
async function pagePC() {
  const t = { now: pcNow, history: pcHistory, apps: pcApps, running: pcRunning, recordings: pcRecordings, insights: pcInsights }[S.tab] || pcNow;
  const [body, now] = await Promise.all([t(), S.pcHead ? Promise.resolve(S.pcHead) : invoke('pc_now')]);
  if (!S.pcHead || S.tab === 'now') S.pcHead = now;
  const h = S.pcHead.host, total = S.pcHead.latest?.mem_total;
  const tabs = [['now', 'Now'], ['history', 'History'], ['apps', 'Apps'], ['running', 'Running', S.pcHead.apps.length], ['recordings', 'Recordings'], ['insights', 'Insights']];
  const sub = `<span>${esc(h.os)}</span><span>${h.cores} threads</span>${total ? `<span>${bytes(total)} memory</span>` : ''}<span>Up ${dur(h.uptime)}</span>`;
  return head(`${esc(h.name)}${S.side?.recorder ? '' : `<span class="status crit">${ic('crit')}Not recording</span>`}`, sub, '', tabs) + `<div class="body">${body}</div>`;
}
async function pcNow() {
  const d = await invoke('pc_now'); S.pcHead = d;
  const p = d.latest?.point, total = d.latest?.mem_total || 0, r = d.recent;
  const tile = (k, v, sub, s, max) => `<div class="tile"><div class="k"><span>${k}</span><span class="muted">10 min</span></div><div class="v num">${v}</div><div class="d">${sub}</div>${spark(s, max, 34)}</div>`;
  const key = S.sort === 'mem' ? 'mem' : S.sort === 'io' ? 'io_rate' : 'cpu';
  const rows = [...d.apps].sort((a, b) => b[key] - a[key]).slice(0, 8);
  const maxMem = Math.max(1, ...d.apps.map(a => a.mem)), maxCpu = Math.max(1, ...d.apps.map(a => a.cpu));
  if (!p) return `<section class="panel"><div class="empty">No readings yet. The recorder takes its first sample within a few seconds.</div></section>`;
  return `
  <div class="tiles">
    ${tile('CPU', pct(p.cpu), `Whole processor, ${d.host.cores} threads`, r.map(x => x.cpu), 100)}
    ${tile('Memory', pct(p.mem_pct), `${ofBytes(p.mem_used, total)} in use`, r.map(x => x.mem_pct), 100)}
    ${tile('Disk and file I/O', rate(p.io_rate), 'All apps reading and writing files', r.map(x => x.io_rate))}
    ${tile('Network', rate(d.net_in + d.net_out), `in ${rate(d.net_in)} · out ${rate(d.net_out)}`, r.map(x => x.net_rate))}
    ${tile('Processes', Math.round(p.procs) || '–', 'Programs and background services', r.map(x => x.procs))}
    ${tile('Apps open', Math.round(p.apps) || d.apps.length, 'Programs with at least one process', r.map(x => x.apps))}
  </div>
  <section class="panel">
    <div class="panel-head"><h2>Using the most right now</h2><span class="grow"></span><span class="lbl muted">Sort by</span>
      ${seg('sort', [['cpu', 'CPU'], ['mem', 'Memory'], ['io', 'I/O']], S.sort)}</div>
    <div class="wrap-x" style="margin-top:10px"><table class="tbl"><thead><tr><th>App</th><th class="r">CPU</th><th class="r">Memory</th><th class="r">Disk and file I/O</th><th class="r">Processes</th></tr></thead><tbody>
    ${rows.map(a => `<tr class="click" data-pcapp="${esc(a.app)}"><td><b style="font-weight:600">${esc(a.app)}</b><span class="sub">${esc(a.explain || a.name)}</span></td>
      <td class="r"><div class="barcell"><b>${pct(a.cpu)}</b>${meter(a.cpu / maxCpu * 100, true)}</div></td>
      <td class="r"><div class="barcell"><b>${bytes(a.mem)}</b>${meter(a.mem / maxMem * 100, true)}</div></td>
      <td class="r num">${rate(a.io_rate)}</td><td class="r num">${a.procs}</td></tr>`).join('')}
    </tbody></table></div>
  </section>`;
}

function heatmap(grid, label, fmtV) {
  const heat = getComputedStyle(document.documentElement).getPropertyValue('--heat').split(',').map(s => s.trim());
  const vals = grid.flat().filter(v => v != null);
  const top = Math.max(5, ...vals);
  const cell = v => v == null ? 'var(--heat-0)' : heat[Math.min(6, Math.floor(v / top * 6.999))];
  const order = [1, 2, 3, 4, 5, 6, 0];
  return `<div class="heat">
      <span></span>${Array.from({ length: 24 }, (_, h) => `<span class="hl">${h % 3 === 0 ? h : ''}</span>`).join('')}
      ${order.map(d => `<span class="dl">${DAYS[d]}</span>` + grid[d].map((v, h) => `<span class="c" style="background:${cell(v)}" title="${DAYS[d]} ${String(h).padStart(2, '0')}:00 · ${v == null ? 'no data' : fmtV(v) + ' ' + label}"></span>`).join('')).join('')}
    </div>
    <div class="heat-key" style="margin-top:10px">0%${heat.map(c => `<i style="background:${c}"></i>`).join('')}${Math.round(top)}%<span style="margin-left:12px;display:inline-flex;gap:4px;align-items:center"><i style="background:var(--heat-0)"></i> no data</span></div>`;
}

async function pcHistory() {
  const d = await invoke('pc_history', { range: S.range, which: S.histApp || null });
  const to = nowS(), from = to - S.range, b = d.bucket;
  S.histApps = d.apps || S.histApps || [];
  const P = d.points;
  const pick = k => P.map(p => [p.ts, p[k]]);
  const tipApps = ts => moment('pc_moment', { ts, bucket: b }).then(m => m.cpu.length ? `<div style="font-size:11px;color:var(--muted)">Most CPU then</div>` + tipRows(m.cpu, pct) : '');
  let pin = '';
  if (S.pinned != null) {
    const pt = P.find(p => p.ts === S.pinned);
    const m = await moment('pc_moment', { ts: S.pinned, bucket: b });
    const list = (arr, f) => arr.length ? arr.map(x => `<div class="tr" style="display:flex;justify-content:space-between;padding:3px 0"><span>${esc(x.name)}</span><span class="num">${f(x.v)}</span></div>`).join('') : '<span class="muted">No app samples at that time.</span>';
    pin = `<section class="panel moment"><div class="panel-head"><h2>At ${tipTime(S.pinned, b)}</h2><span class="grow"></span><button class="btn small ghost" data-unpin>${ic('x')}Close</button></div>
     <div class="panel-body cols-even">${pt && !S.histApp ? `<dl class="kv"><dt>CPU</dt><dd>${pct(pt.cpu)}</dd><dt>Memory</dt><dd>${pct(pt.mem_pct)}, ${bytes(pt.mem_used)}</dd><dt>Disk and file I/O</dt><dd>${rate(pt.io_rate)}</dd><dt>Network</dt><dd>${rate(pt.net_rate)}</dd><dt>Processes</dt><dd>${Math.round(pt.procs)}</dd></dl>` : '<div></div>'}
     <div class="cols-even"><div><div class="muted" style="font-size:12px;margin-bottom:4px">Most CPU then</div>${list(m.cpu, pct)}</div><div><div class="muted" style="font-size:12px;margin-bottom:4px">Most memory then</div>${list(m.mem, bytes)}</div></div></div></section>`;
  }
  const opts = [...new Set([...(S.histApps || []), ...(S.histApp ? [S.histApp] : [])])];
  const charts = S.histApp ? `
    ${lineChart({ id: 'h-cpu', title: `CPU used by ${esc(S.histApp)}`, pts: pick('cpu'), from, to, bucket: b, fmt: cpuFmt, group: 'h' })}
    ${lineChart({ id: 'h-mem', title: 'Memory', pts: pick('mem'), from, to, bucket: b, fmt: bytes, group: 'h' })}
    ${lineChart({ id: 'h-io', title: 'Disk and file I/O', pts: pick('io_rate'), from, to, bucket: b, fmt: rate, group: 'h', h: 90 })}
    ${lineChart({ id: 'h-pr', title: 'Processes', pts: pick('procs'), from, to, bucket: b, fmt: v => Math.round(v), group: 'h', h: 70 })}
    <p class="note">Gaps mean the app was not running. Network is not shown for single apps because Windows does not report it per app.</p>` : `
    ${lineChart({ id: 'h-cpu', title: 'CPU', pts: pick('cpu'), from, to, bucket: b, max: 100, fmt: cpuFmt, group: 'h', tip: tipApps })}
    ${lineChart({ id: 'h-mem', title: 'Memory', pts: pick('mem_pct'), from, to, bucket: b, max: 100, fmt: v => Math.round(v) + '%', group: 'h' })}
    ${lineChart({ id: 'h-io', title: 'Disk and file I/O', pts: pick('io_rate'), from, to, bucket: b, fmt: rate, group: 'h', h: 90 })}
    ${lineChart({ id: 'h-net', title: 'Network', pts: pick('net_rate'), from, to, bucket: b, fmt: rate, group: 'h', h: 90 })}
    ${lineChart({ id: 'h-pr', title: 'Processes running', pts: pick('procs'), from, to, bucket: b, fmt: v => Math.round(v), group: 'h', h: 70 })}`;
  return `
  <div class="toolbar"><span class="lbl">Show</span><select class="select" id="histapp"><option value="">Whole PC</option>${opts.map(a => `<option value="${esc(a)}" ${S.histApp === a ? 'selected' : ''}>${esc(a)}</option>`).join('')}</select><span class="grow"></span>${rangePicker()}</div>
  ${pin}
  <section class="panel"><div class="panel-body charts-stack">
    <p class="note">Hover a chart to see the same moment on every chart${S.histApp ? '' : ' and which apps were behind it'}. Click to keep the moment open.</p>
    ${charts}
  </div></section>
  <section class="panel">
    <div class="panel-head"><h2>When ${S.histApp ? esc(S.histApp) + ' is' : 'this PC is'} busiest</h2><span class="hint">Average CPU for each hour of each weekday, from at least the last week. Darker is busier.</span></div>
    <div class="panel-body">${heatmap(d.heat, 'CPU', pct)}</div>
  </section>`;
}

async function pcApps() {
  const d = await invoke('pc_apps', { range: S.range });
  const key = S.sort === 'mem' ? 'mem_avg' : S.sort === 'io' ? 'io_total' : 'cpu_load';
  const apps = [...d.apps].sort((a, b) => b[key] - a[key]);
  if (!apps.length) return `<div class="toolbar"><span class="grow"></span>${rangePicker()}</div><section class="panel"><div class="empty">No app history in this range yet.</div></section>`;
  if (!S.appSel || !apps.some(a => a.app === S.appSel)) S.appSel = apps[0].app;
  const sel = apps.find(a => a.app === S.appSel);
  const totalLoad = apps.reduce((t, a) => t + a.cpu_load, 0) || 1;
  const totalKey = apps.reduce((t, a) => t + a[key], 0) || 1;
  const max = Math.max(...apps.map(a => a[key])) || 1;
  const fmt = key === 'mem_avg' ? bytes : key === 'io_total' ? bytes : v => pct(v);
  const shown = key === 'cpu_load' ? a => pct(a.cpu_avg) : a => fmt(a[key]);
  const series = await invoke('pc_history', { range: S.range, which: sel.app });
  const to = nowS(), from = to - S.range, b = series.bucket;
  const P = series.points;
  return `
  <div class="toolbar"><span class="lbl">Rank by</span>${seg('sort', [['cpu', 'CPU'], ['mem', 'Memory'], ['io', 'I/O']], S.sort)}<span class="grow"></span>${rangePicker()}</div>
  <div class="cols">
    <section class="panel"><div class="wrap-x"><table class="tbl"><thead><tr><th>App</th><th class="r">${key === 'cpu_load' ? 'Average CPU' : key === 'mem_avg' ? 'Average memory' : 'Read and written'}</th><th class="r">Share</th></tr></thead><tbody>
      ${apps.slice(0, 60).map(a => `<tr class="click ${a.app === S.appSel ? 'sel' : ''}" data-appsel="${esc(a.app)}"><td><b style="font-weight:600">${esc(a.app)}</b><span class="sub">${esc(a.explain || a.name)}</span></td>
      <td class="r"><div class="barcell"><b>${shown(a)}</b>${meter(a[key] / max * 100, true)}</div></td><td class="r num">${Math.round(a[key] / (key === 'cpu_load' ? totalLoad : totalKey) * 100)}%</td></tr>`).join('')}
    </tbody></table></div>
    <p class="note" style="padding:10px 16px 14px">CPU is a share of the whole processor, so 100% means every core is busy. Processes with the same name count as one app. Average CPU is over the minutes the app was running.</p></section>
    <section class="panel"><div class="panel-head"><h2>${esc(sel.app)}</h2><span class="hint">${esc(sel.explain || sel.name)}</span></div><div class="panel-body charts-stack">
      <dl class="kv"><dt>Running</dt><dd>${dur(sel.minutes * 60)} in this range</dd><dt>Peak CPU</dt><dd>${pct(sel.cpu_peak)}</dd><dt>Peak memory</dt><dd>${bytes(sel.mem_peak)}</dd><dt>Read and written</dt><dd>${bytes(sel.io_total)}</dd></dl>
      ${lineChart({ id: 'a-cpu', title: 'CPU', pts: P.map(p => [p.ts, p.cpu]), from, to, bucket: b, fmt: cpuFmt, group: 'a', h: 100 })}
      ${lineChart({ id: 'a-mem', title: 'Memory', pts: P.map(p => [p.ts, p.mem]), from, to, bucket: b, fmt: bytes, group: 'a', h: 100 })}
      <p class="note">Gaps mean the app was not running.</p>
      <div class="toolbar"><button class="btn" data-record="${esc(sel.app)}">${ic('play')}Record this app</button><button class="btn" data-tab="running">See running apps</button></div>
    </div></section>
  </div>`;
}

async function pcRunning() {
  const d = await invoke('pc_now'); S.pcHead = d;
  const key = S.sort === 'mem' ? 'mem' : 'cpu';
  const apps = [...d.apps].sort((a, b) => b[key] - a[key]);
  const action = a => a.protected ? '<span class="muted" style="font-size:12px">Part of Windows</span>'
    : S.asked.has(a.app) ? `<span class="muted" style="font-size:12px;margin-right:8px">Did not close</span><button class="btn small danger" data-force="${esc(a.app)}">Force stop</button>`
    : `<button class="btn small" data-stop="${esc(a.app)}">${ic('stop')}Stop app</button>`;
  return `
  <div class="toolbar"><span class="lbl">Sort by</span>${seg('sort', [['cpu', 'CPU'], ['mem', 'Memory']], key)}<span class="grow"></span><span class="muted" style="font-size:12px">Stopping an app closes it like quitting it. Nothing is uninstalled or deleted.</span></div>
  <section class="panel"><div class="wrap-x"><table class="tbl"><thead><tr><th>App</th><th class="r">Processes</th><th class="r">CPU</th><th class="r">Memory</th><th></th></tr></thead><tbody>
    ${apps.map(a => `<tr><td><b style="font-weight:600">${esc(a.app)}</b><span class="sub">${esc(a.explain || a.name)}</span>${a.titles ? `<span class="titles" title="${esc(a.titles)}">${esc(a.titles)}</span>` : ''}</td>
      <td class="r num">${a.procs}</td><td class="r num">${pct(a.cpu)}</td><td class="r num">${bytes(a.mem)}</td>
      <td class="r">${action(a)}</td></tr>`).join('')}
  </tbody></table></div></section>`;
}

async function pcRecordings() {
  const d = await invoke('recordings');
  const live = d.sessions.filter(s => s.ended == null);
  const saved = d.sessions.filter(s => s.ended != null);
  if (!S.recApp || !d.apps.some(a => a.app === S.recApp)) S.recApp = d.apps[0]?.app || '';
  const liveBlock = s => {
    const series = d.active.find(a => a.id === s.id)?.series || [];
    const to = nowS(), from = Math.max(s.started, to - 600);
    const len = to - s.started;
    return `<section class="panel"><div class="panel-head"><h2>${esc(s.app)}</h2><span class="status">Recording · ${dur(len)}${s.planned_end ? ' of ' + dur(s.planned_end - s.started) : ''}</span><span class="grow"></span><button class="btn" data-recstop="${s.id}">${ic('stop')}Stop</button></div>
    <div class="panel-body recording-live">
      <div class="charts-stack">${lineChart({ id: 'r-cpu-' + s.id, title: 'CPU, every second', pts: series.filter(p => p.ts >= from).map(p => [p.ts, p.cpu]), from, to, bucket: 1, fmt: cpuFmt, h: 110 })}</div>
      ${sessionStats(s)}
    </div></section>`;
  };
  let selBlock = '';
  const sel = saved.find(s => s.id === S.recSel);
  if (sel) {
    const series = await invoke('recording_series', { id: sel.id });
    const from = sel.started, to = sel.ended || nowS();
    const b = Math.max(1, Math.round((to - from) / 600));
    selBlock = `<section class="panel moment"><div class="panel-head"><h2>${esc(sel.app)}, ${when(sel.started)}</h2><span class="grow"></span><button class="btn small ghost" data-recsel="">${ic('x')}Close</button></div>
      <div class="panel-body recording-live"><div class="charts-stack">
        ${lineChart({ id: 'rs-cpu', title: 'CPU', pts: series.map(p => [p.ts, p.cpu]), from, to, bucket: b, fmt: cpuFmt, group: 'rs', h: 100 })}
        ${lineChart({ id: 'rs-mem', title: 'Memory', pts: series.map(p => [p.ts, p.mem]), from, to, bucket: b, fmt: bytes, group: 'rs', h: 90 })}
      </div>${sessionStats(sel)}</div></section>`;
  }
  return `
  <section class="panel"><div class="panel-head"><h2>Start a recording</h2><span class="hint">Records one app every second, even with this window closed.</span></div>
    <div class="panel-body">${d.apps.length ? `<div class="rec-form">
      <label class="field"><span>App</span><select class="select" id="recapp" style="min-width:240px">${d.apps.map(a => `<option value="${esc(a.app)}" ${a.app === S.recApp ? 'selected' : ''}>${esc(a.app)}</option>`).join('')}</select></label>
      <div class="field"><span>For</span>${seg('recmin', [[10, '10 minutes'], [30, '30 minutes'], [0, 'Until I stop it']], S.recMinutes)}</div>
      <button class="btn primary" data-action="rec-start">${ic('play')}Start</button>
    </div>` : '<p class="note">No apps are running that can be recorded.</p>'}</div></section>
  ${live.map(liveBlock).join('')}
  ${selBlock}
  <section class="panel"><div class="panel-head"><h2>Saved recordings</h2></div>
    ${saved.length ? `<div class="wrap-x" style="margin-top:8px"><table class="tbl"><thead><tr><th>App</th><th>Started</th><th>Length</th><th class="r">Avg CPU</th><th class="r">Peak memory</th><th></th></tr></thead><tbody>
      ${saved.map(s => `<tr class="click ${s.id === S.recSel ? 'sel' : ''}" data-recsel="${s.id}"><td><b style="font-weight:600">${esc(s.app)}</b></td><td>${when(s.started)}</td><td>${dur(s.ended - s.started)}</td><td class="r num">${pct(s.cpu_avg)}</td><td class="r num">${bytes(s.mem_peak)}</td><td class="r"><button class="btn small ghost danger" data-recdel="${s.id}">${ic('trash')}Delete</button></td></tr>`).join('')}
    </tbody></table></div>` : '<div class="empty">No saved recordings yet.</div>'}</section>`;
}
const sessionStats = s => `<dl class="kv"><dt>Average CPU</dt><dd>${pct(s.cpu_avg)}</dd><dt>Peak CPU</dt><dd>${pct(s.cpu_peak)}</dd><dt>Average memory</dt><dd>${bytes(s.mem_avg)}</dd><dt>Peak memory</dt><dd>${bytes(s.mem_peak)}</dd><dt>Disk and file I/O</dt><dd>${bytes(s.io_total)} total</dd><dt>Processes</dt><dd>up to ${s.procs_peak}</dd><dt>Samples</dt><dd>${s.samples}</dd></dl>`;

async function pcInsights() {
  const list = (await invoke('findings', { range: S.range })).filter(f => f.machine_id === 'pc');
  return `<div class="toolbar"><span class="grow"></span>${rangePicker()}</div>
  <section class="panel"><div class="findings">${list.length ? list.map(findingRow).join('') : '<div class="empty">Nothing found in this range. This PC looks fine.</div>'}</div></section>`;
}

/* ================= Server ================= */
async function pageServer(alias) {
  const m = await invoke('server_detail', { alias });
  const s = m.server, p = m.latest?.point;
  const pauseBtn = s.enabled ? `<button class="btn" data-watch="${esc(alias)}" data-on="0">${ic('pause')}Pause monitoring</button>` : `<button class="btn" data-watch="${esc(alias)}" data-on="1">${ic('play')}Resume monitoring</button>`;
  const title = `${esc(alias)}${statusText(s.enabled ? m : { enabled: false })}`;
  if (!S.side?.ssh) {
    return head(title, `<span class="mono">${esc(m.address)}</span>`) + `<div class="body"><div class="add-servers"><div class="ic">${ic('server')}</div><div><h3>Servers over SSH are off</h3><p>Its history is kept. Turn servers back on in Settings to see it live.</p></div><button class="btn primary" data-go="settings">Open Settings</button></div></div>`;
  }
  if (m.failed || !p) {
    const to = nowS(), last = p ? p.ts : to;
    const old = await invoke('server_history', { alias, range: 86400 });
    const from = Math.min(last, to) - 86400;
    return head(title, `<span class="mono">${esc(m.address)}</span>${m.via ? `<span>through ${esc(m.via)}</span>` : ''}<span>Last sample ${p ? ago(p.ts) : 'never'}</span>`, pauseBtn) + `
    <div class="body">
      ${m.failed ? `<div class="err">${ic('crit', 'i fi')}<div><div class="t">Cannot log in: ${esc(s.label.toLowerCase())}</div><div>${esc(m.hint)}</div>${s.detail ? `<pre>${esc(s.detail)}</pre>` : ''}
        <div class="toolbar"><button class="btn primary" data-retry="${esc(alias)}">${ic('refresh')}Retry now</button><span class="muted" style="font-size:12px">A failed login is not retried on its own, so a rejected key cannot get you banned by fail2ban.</span></div></div></div>`
        : !s.enabled ? `<div class="empty">Monitoring is paused for this server.</div>` : `<div class="empty">Logging in. The first readings show up within a few seconds.</div>`}
      ${old.points.length ? `<section class="panel"><div class="panel-head"><h2>Before it stopped answering</h2><span class="hint">Its history is kept.</span></div>
        <div class="panel-body charts-stack">${lineChart({ id: 'o-cpu', title: 'CPU, last day', pts: old.points.map(q => [q.ts, q.cpu]), from: to - 86400, to, bucket: old.bucket, max: 100, fmt: cpuFmt, h: 90 })}</div></section>` : ''}
    </div>`;
  }
  const tabs = [['summary', 'Summary'], ['processes', 'Processes', m.groups.length], ['history', 'History'], ['findings', 'Findings', m.findings.length]];
  const sub = `<span class="mono">${esc(m.address)}</span>${m.via ? `<span>through ${esc(m.via)}</span>` : ''}<span>${esc(s.os)}</span><span>${s.cores} cores</span><span>Up ${dur(p.uptime)}</span>`;
  const t = { summary: srvSummary, processes: srvProcesses, history: srvHistory, findings: srvFindings }[S.tab] || srvSummary;
  return head(title, sub, pauseBtn, tabs) + `<div class="body">${await t(m)}</div>`;
}
async function srvSummary(m) {
  const p = m.latest.point, r = m.recent, s = m.server;
  const tile = (k, v, sub, vals, max) => `<div class="tile"><div class="k"><span>${k}</span><span class="muted">10 min</span></div><div class="v num">${v}</div><div class="d">${sub}</div>${spark(vals, max, 34)}</div>`;
  const used = m.latest.mem_used, total = p.mem_total;
  const zfs = Math.min(p.arc, used), progs = Math.max(0, used - zfs), cache = Math.max(0, p.mem_avail - p.mem_free), free = p.mem_free;
  const w = v => (v / total * 100).toFixed(2) + '%';
  const groups = m.groups.filter(g => g.kind !== 'Kernel');
  const g1 = groups[0], g2 = groups[1];
  const memText = `Programs use ${bytes(progs)}.` + (g1 ? ` The biggest is ${esc(g1.label)} with ${bytes(g1.mem)} (${Math.round(g1.mem / total * 100)}% of all memory)${g2 ? `, then ${esc(g2.label)} with ${bytes(g2.mem)}` : ''}.` : '');
  const maxG = Math.max(1, ...groups.map(g => g.mem));
  return `
  <div class="tiles">
    ${tile('CPU', pct(p.cpu), `waiting on disk ${pct(p.iowait)} · taken by host ${pct(p.steal)}`, r.map(x => x.cpu), 100)}
    ${tile('Memory', pct(m.latest.mem_pct), ofBytes(used, total), r.map(x => x.mem_total ? (x.mem_total - x.mem_avail) / x.mem_total * 100 : null), 100)}
    ${tile('Swap', pct(m.latest.swap_pct), p.swap_total ? ofBytes(p.swap_used, p.swap_total) : 'No swap set up', r.map(x => x.swap_used), p.swap_total || null)}
    ${tile('Load', p.load1.toFixed(2), `5 min ${p.load5.toFixed(2)} · 15 min ${p.load15.toFixed(2)}`, r.map(x => x.load1))}
    ${tile('Network', rate(p.net_rx + p.net_tx), `in ${rate(p.net_rx)} · out ${rate(p.net_tx)}`, r.map(x => x.net_rx + x.net_tx))}
    ${tile('Disk I/O', rate(p.disk_read + p.disk_write), `read ${rate(p.disk_read)} · write ${rate(p.disk_write)}`, r.map(x => x.disk_read + x.disk_write))}
  </div>
  <section class="panel"><div class="panel-head"><h2>Where the memory goes</h2><span class="hint">${bytes(total)} total</span></div>
    <div class="panel-body sec">
      <div class="stack" role="img" aria-label="Programs ${bytes(progs)}, ZFS cache ${bytes(zfs)}, file cache ${bytes(cache)}, free ${bytes(free)}"><i style="width:${w(progs)};background:var(--s1)"></i>${zfs > 0 ? `<i style="width:${w(zfs)};background:var(--s2)"></i>` : ''}<i style="width:${w(cache)};background:var(--s3)"></i><i style="flex:1;background:var(--surface-3)"></i></div>
      <div class="legend"><span><i style="background:var(--s1)"></i>Programs<b>${bytes(progs)}</b></span>${zfs > 0 ? `<span><i style="background:var(--s2)"></i>ZFS cache<b>${bytes(zfs)}</b></span>` : ''}<span><i style="background:var(--s3)"></i>File cache, freed when needed<b>${bytes(cache)}</b></span><span><i style="background:var(--surface-3)"></i>Free<b>${bytes(free)}</b></span></div>
      <p style="margin:0;color:var(--ink-2)">${memText}</p>
    </div></section>
  <div class="cols">
    <section class="panel"><div class="panel-head"><h2>Using the most memory</h2><span class="grow"></span><button class="btn small ghost" data-tab="processes">See all ${m.groups.length}</button></div>
      <div class="wrap-x" style="margin-top:8px"><table class="tbl"><thead><tr><th>Name</th><th class="r">CPU</th><th class="r">Memory</th></tr></thead><tbody>
      ${groups.slice(0, 6).map(g => `<tr><td><b style="font-weight:600">${esc(g.label)}</b> <span class="chip">${esc(g.kind)}</span><span class="sub">${esc(g.explain)}</span></td><td class="r num">${pct(g.cpu)}</td><td class="r"><div class="barcell"><b>${bytes(g.mem)}</b>${meter(g.mem / maxG * 100, true)}</div></td></tr>`).join('')}
      </tbody></table></div></section>
    <div class="sec">
      <section class="panel"><div class="panel-head"><h2>Disks</h2></div><div class="panel-body">
        ${m.mounts.map(d => `<div class="disk"><div class="top"><b style="font-weight:600">${esc(d.mount)}</b><span class="num">${Math.round(d.pct)}%</span></div>${meter(d.pct)}<div class="top muted" style="font-size:11.5px"><span class="mono">${esc(d.fs)}</span><span>${bytes(d.avail)} free of ${bytes(d.size)}</span></div></div>`).join('') || '<span class="muted">No disks reported.</span>'}
      </div></section>
      <section class="panel"><div class="panel-head"><h2>About</h2></div><div class="panel-body"><dl class="kv">
        <dt>System</dt><dd>${esc(s.os)}</dd><dt>Kernel</dt><dd class="mono">${esc(s.kernel)}</dd>${s.cpu_model ? `<dt>Processor</dt><dd>${esc(s.cpu_model)}</dd>` : ''}<dt>Login</dt><dd class="mono">${esc(m.address)}</dd>${m.key ? `<dt>Key</dt><dd class="mono">${esc(m.key.split(/[\\/]/).pop())}</dd>` : ''}${s.reboot ? '<dt>Reboot</dt><dd>Pending</dd>' : ''}${s.docker_note ? `<dt>Docker</dt><dd>${esc(s.docker_note)}</dd>` : ''}<dt>Sampled</dt><dd>every 5 s, one SSH login kept open</dd>
      </dl></div></section>
    </div>
  </div>`;
}
const KIND_FILTER = { all: () => true, vm: k => k === 'VM', containers: k => k === 'LXC' || k === 'Docker', services: k => k === 'Service' || k === 'User service' };
async function srvProcesses(m) {
  const key = S.sort === 'cpu' ? 'cpu' : 'mem';
  const list = m.groups.filter(g => KIND_FILTER[S.srvKind](g.kind)).sort((a, b) => b[key] - a[key]);
  const maxG = Math.max(1, ...m.groups.map(g => g.mem));
  return `
  <div class="toolbar"><span class="lbl">Sort by</span>${seg('sort', [['mem', 'Memory'], ['cpu', 'CPU']], key)}<span class="grow"></span><span class="lbl">Show</span>${seg('kind', [['all', 'All'], ['vm', 'VMs'], ['containers', 'Containers'], ['services', 'Services']], S.srvKind)}</div>
  <section class="panel"><div class="wrap-x"><table class="tbl"><thead><tr><th>Name</th><th>What it is</th><th class="r">Processes</th><th class="r">CPU</th><th class="r">Memory</th></tr></thead><tbody>
    ${list.map(g => `<tr><td><b style="font-weight:600">${esc(g.label)}</b>${g.users ? `<span class="sub">${esc(g.users)}</span>` : ''}</td><td class="wrap"><span class="chip">${esc(g.kind)}</span> <span class="ink2">${esc(g.explain)}</span></td><td class="r num">${g.procs}</td><td class="r num">${pct(g.cpu)}</td><td class="r"><div class="barcell"><b>${bytes(g.mem)}</b>${meter(g.mem / maxG * 100, true)}</div></td></tr>`).join('') || '<tr><td colspan="5" class="empty">Nothing of this kind is running.</td></tr>'}
  </tbody></table></div>
  <p class="note" style="padding:10px 16px 14px">Processes are grouped by the VM, container or systemd service that started them. Memory is resident memory, so processes that share memory can add up to more than they really use.</p></section>`;
}
async function srvHistory(m) {
  const alias = m.alias;
  const d = await invoke('server_history', { alias, range: S.range });
  const to = nowS(), from = to - S.range, b = d.bucket, P = d.points;
  const pick = k => P.map(p => [p.ts, p[k]]);
  const tipG = ts => moment('server_moment', { alias, ts, bucket: b }).then(x => x.mem.length ? `<div style="font-size:11px;color:var(--muted)">Most memory then</div>` + tipRows(x.mem, bytes) : '');
  let pin = '';
  if (S.pinned != null) {
    const x = await moment('server_moment', { alias, ts: S.pinned, bucket: b });
    const list = (arr, f) => arr.length ? arr.map(g => `<div class="tr" style="display:flex;justify-content:space-between;padding:3px 0"><span>${esc(g.name)} <span class="chip">${esc(g.kind)}</span></span><span class="num">${f(g.v)}</span></div>`).join('') : '<span class="muted">No samples at that time.</span>';
    pin = `<section class="panel moment"><div class="panel-head"><h2>At ${tipTime(S.pinned, b)}</h2><span class="grow"></span><button class="btn small ghost" data-unpin>${ic('x')}Close</button></div>
      <div class="panel-body cols-even"><div><div class="muted" style="font-size:12px;margin-bottom:4px">Most CPU then</div>${list(x.cpu, pct)}</div><div><div class="muted" style="font-size:12px;margin-bottom:4px">Most memory then</div>${list(x.mem, bytes)}</div></div></section>`;
  }
  return `
  <div class="toolbar"><span class="grow"></span>${rangePicker()}</div>
  ${pin}
  <section class="panel"><div class="panel-body charts-stack">
    <p class="note">Hover a chart to see the same moment on every chart and what was running. Click to keep the moment open.</p>
    ${lineChart({ id: 's-cpu', title: 'CPU', pts: pick('cpu'), from, to, bucket: b, max: 100, fmt: cpuFmt, group: 's', tip: tipG })}
    ${lineChart({ id: 's-mem', title: 'Memory', pts: pick('mem_pct'), from, to, bucket: b, max: 100, fmt: v => Math.round(v) + '%', group: 's' })}
    ${lineChart({ id: 's-load', title: 'Load', pts: pick('load1'), from, to, bucket: b, fmt: v => v.toFixed(2), group: 's', h: 80 })}
    ${lineChart({ id: 's-net', title: 'Network', pts: pick('net'), from, to, bucket: b, fmt: rate, group: 's', h: 80 })}
    ${lineChart({ id: 's-io', title: 'Disk I/O', pts: pick('disk'), from, to, bucket: b, fmt: rate, group: 's', h: 80 })}
  </div></section>`;
}
const srvFindings = async m => `<section class="panel"><div class="findings">${m.findings.length ? m.findings.map(findingRow).join('') : '<div class="empty">Nothing found. This server looks healthy.</div>'}</div></section>`;

/* ================= SSH keys ================= */
async function pageKeys() {
  const d = S.keys || (S.keys = await invoke('ssh_keys'));
  const lvl = l => l === 'Info' ? 'info' : 'warn';
  const checks = [
    { level: d.agent.running ? 'ok' : 'warn', text: d.agent.running ? `ssh-agent is running${d.agent.keys ? ` with ${d.agent.keys} key${d.agent.keys === 1 ? '' : 's'}` : ''}` : 'ssh-agent is not running', detail: d.agent.note },
    ...d.findings.map(f => ({ level: lvl(f.level), text: f.text })),
  ];
  const age = ts => ts ? dur(nowS() - ts).replace(/^(\d+(\.\d)?) days$/, '$1 days') : '–';
  const keyNote = k => k.error ? `<span class="status warn">${ic('alert')}${esc(k.error)}</span>`
    : k.findings.length ? k.findings.map(f => `<span class="status ${f.level === 'Info' ? '' : 'warn'}">${f.level === 'Info' ? '' : ic('alert')}${esc(f.text)}</span>`).join('<br>')
    : '<span class="muted">Fine</span>';
  return head('SSH keys', `<span>${d.keys.length} key${d.keys.length === 1 ? '' : 's'} in ${esc(d.ssh_dir)}</span><span>Only public key files are read</span>`, `<button class="btn" data-action="keys-again">${ic('refresh')}Check again</button>`) + `
  <div class="body">
    <section class="panel"><div class="panel-head"><h2>Checks</h2></div><div class="findings" style="margin-top:6px">
      ${checks.map(c => `<div class="finding ${c.level === 'ok' ? 'info' : c.level}">${c.level === 'ok' ? `<svg class="i fi" viewBox="0 0 16 16">${I.shield}</svg>` : ic(c.level === 'info' ? 'info' : 'alert', 'i fi')}<div><div class="t">${esc(c.text)}</div>${c.detail ? `<div class="d">${esc(c.detail)}</div>` : ''}</div><span></span></div>`).join('')}
    </div></section>
    <section class="panel"><div class="panel-head"><h2>Keys</h2></div>${d.keys.length ? `<div class="wrap-x" style="margin-top:8px"><table class="tbl"><thead><tr><th>Key</th><th>Type</th><th>Fingerprint</th><th>Used by</th><th>Age</th><th>Notes</th></tr></thead><tbody>
      ${d.keys.map(k => `<tr><td><b style="font-weight:600" class="mono">${esc(k.name)}</b>${k.comment ? `<span class="sub">${esc(k.comment)}</span>` : ''}</td><td>${esc(k.kind)}${k.bits ? ' ' + k.bits : ''}</td><td class="mono muted">${esc(k.fingerprint)}</td><td>${k.used_by.length ? k.used_by.map(h => `<span class="chip">${esc(h)}</span>`).join(' ') : '<span class="muted">Not in config</span>'}</td><td>${age(k.created)}</td><td>${keyNote(k)}</td></tr>`).join('')}
    </tbody></table></div>` : '<div class="empty">No public keys found.</div>'}
    <p class="note" style="padding:10px 16px 14px">Logins never prompt and never accept a new or changed host key.</p></section>
  </div>`;
}

/* ================= Settings ================= */
const IMPORT_TEXT = { copied: 'Copied', not_found: 'Not found', failed: 'Could not copy' };
async function pageSettings() {
  const d = await invoke('settings');
  S.theme = d.theme;
  const imp = d.imported;
  const importLine = ['jperformance', 'jserver'].map(k => `${k === 'jperformance' ? 'JPerformance' : 'JServer'}: ${imp[k] ? IMPORT_TEXT[imp[k]] : 'Not checked yet'}`).join(' · ');
  const u = S.update;
  const updText = u.state === 'checking' ? 'Checking GitHub for a new release.'
    : u.state === 'up_to_date' ? 'You have the latest version.'
    : u.state === 'available' ? `Version ${esc(u.version)} is ready to install.`
    : u.state === 'downloading' ? `Downloading the update${u.total ? `, ${Math.round(u.done / u.total * 100)}%` : ''}.`
    : u.state === 'restarting' ? 'Restarting to finish the update.'
    : u.state === 'failed' ? `The update check failed: ${esc(u.message)}`
    : 'Checks GitHub for a new release when it opens and every 30 minutes.';
  return head('Settings', 'Everything here is saved on this PC.') + `
  <div class="body" style="max-width:920px">
    <section class="panel"><div class="panel-head"><h2>Servers over SSH</h2></div>
      <div class="set-list" style="margin-top:4px">
        <div class="set-row"><div><div class="t">Watch servers from my SSH config</div><div class="d">Reads ${esc(d.ssh_config)}, logs in to each host you pick with its key, and records it every 5 seconds. Off by default.</div></div>
          <label class="switch"><input type="checkbox" id="ssh-toggle" ${d.ssh ? 'checked' : ''} aria-label="Watch servers from my SSH config"><span></span></label></div>
      </div>
      ${d.ssh ? `<div class="hosts"><div class="toolbar" style="margin:4px 0 6px"><span class="lbl">${d.hosts.length ? `Found ${d.hosts.length} host${d.hosts.length === 1 ? '' : 's'}. Pick the ones to watch.` : 'No hosts found in your SSH config.'}</span><span class="grow"></span><button class="btn small" data-action="refresh">${ic('search')}Scan again</button></div>
        ${d.hosts.length ? `<table class="tbl"><thead><tr><th style="width:28px"></th><th>Host</th><th>Login</th><th>Reached through</th></tr></thead><tbody>
        ${d.hosts.map(h => `<tr><td><input type="checkbox" class="check" data-host="${esc(h.alias)}" ${h.enabled ? 'checked' : ''} aria-label="Watch ${esc(h.alias)}"></td><td><b style="font-weight:600">${esc(h.alias)}</b></td><td class="mono muted">${esc(h.address)}</td><td>${h.via ? `<span class="chip">${esc(h.via)}</span>` : '<span class="muted">Direct</span>'}</td></tr>`).join('')}
        </tbody></table>` : ''}</div>` : ''}
      <div style="padding:0 16px 16px"><div class="safe">${ic('shield')}<div><b style="color:var(--ink)">What JPerformance does on a server</b><ul>
        <li>Runs read-only commands only: /proc, /sys, df, docker ps, qm list. Nothing is installed or written.</li>
        <li>Never asks for a password and never accepts a new or changed host key.</li>
        <li>Does not retry a failed login on its own, so fail2ban will not ban you.</li></ul></div></div></div>
    </section>
    <section class="panel"><div class="panel-head"><h2>Recording</h2></div><div class="set-list" style="margin-top:4px">
      <div class="set-row"><div><div class="t">Start with Windows</div><div class="d">Keeps recording after a restart. A small helper keeps going when this window is closed.</div></div><label class="switch"><input type="checkbox" id="autostart" ${d.autostart ? 'checked' : ''} aria-label="Start with Windows"><span></span></label></div>
      <div class="set-row"><div><div class="t">Keep history for</div><div class="d">Older samples are removed automatically, for this PC and for servers.</div></div><select class="select" id="retention">${[7, 30, 90, 180, 365].map(n => `<option value="${n}" ${d.retention === n ? 'selected' : ''}>${n} days</option>`).join('')}</select></div>
      <div class="set-row"><div><div class="t">History from JPerformance and JServer</div><div class="d">Copied once on first start. The old apps and their data were not changed. ${importLine}.</div></div><span></span></div>
      <div class="set-row"><div><div class="t">Recorder</div><div class="d">${d.recorder ? 'Running in the background.' : 'Not running, so nothing is being recorded.'}</div></div>${d.recorder ? '<span></span>' : `<button class="btn" data-action="start-recorder">${ic('play')}Start</button>`}</div>
    </div></section>
    <section class="panel"><div class="panel-head"><h2>Appearance</h2></div><div class="set-list" style="margin-top:4px">
      <div class="set-row"><div><div class="t">Theme</div></div>${seg('theme', [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']], S.theme)}</div>
    </div></section>
    <section class="panel"><div class="panel-head"><h2>Updates</h2></div><div class="set-list" style="margin-top:4px">
      <div class="set-row"><div><div class="t">Version ${esc(d.version)}</div><div class="d">${updText} Downloads are checked against the SHA-256 GitHub publishes before they replace the app.</div></div><div class="toolbar">${u.state === 'available' ? `<button class="btn" data-dialog="update">What's new in ${esc(u.version)}</button><button class="btn primary" data-action="install">${ic('download')}Install and restart</button>` : `<button class="btn" data-action="check" ${u.state === 'checking' || u.state === 'downloading' ? 'disabled' : ''}>${ic('refresh')}Check now</button>`}</div></div>
    </div></section>
    <section class="panel"><div class="panel-head"><h2>Data</h2></div><div class="panel-body"><dl class="kv"><dt>Stored in</dt><dd class="mono">${esc(d.data_dir)}</dd><dt>Size</dt><dd>${bytes(d.size)}</dd><dt>Leaves this PC</dt><dd>Never. Only the update check talks to GitHub.</dd></dl></div></section>
  </div>`;
}

/* ================= Dialogs and toasts ================= */
let pendingDialog = null;
function dialog(kind, arg) {
  const d = document.getElementById('dialog');
  pendingDialog = { kind, arg };
  const u = S.update;
  d.innerHTML = kind === 'force' ? `<h3>Force stop ${esc(arg)}?</h3><p>It did not close when asked. Forcing it ends it right away, and any unsaved work in it is lost.</p><div class="row"><button class="btn" data-close>Cancel</button><button class="btn danger" data-confirm>Force stop</button></div>`
    : kind === 'delete' ? `<h3>Delete this recording?</h3><p>Its samples are removed for good. Other history is kept.</p><div class="row"><button class="btn" data-close>Cancel</button><button class="btn danger" data-confirm>Delete recording</button></div>`
    : `<h3>What's new in ${esc(u.version)}</h3><pre class="notes">${esc(u.notes || 'No notes for this release.')}</pre><div class="row"><button class="btn" data-close>Later</button><button class="btn primary" data-confirm>Install and restart</button></div>`;
  document.getElementById('scrim').classList.add('open');
  d.querySelector('[data-confirm]')?.focus();
}
const closeDialog = () => { document.getElementById('scrim').classList.remove('open'); pendingDialog = null; };
async function confirmDialog() {
  const p = pendingDialog; closeDialog(); if (!p) return;
  if (p.kind === 'force') await stopApp(p.arg, true);
  if (p.kind === 'delete') { await act(invoke('delete_recording', { id: p.arg }), 'Recording deleted'); if (S.recSel === p.arg) S.recSel = null; }
  if (p.kind === 'update') await installUpdate();
  render();
}
let toastT;
function toast(msg, bad) {
  const t = document.getElementById('toast');
  t.className = 'toast' + (bad ? ' crit' : ''); t.innerHTML = (bad ? ic('crit') : '') + `<span>${esc(msg)}</span>`; t.hidden = false;
  clearTimeout(toastT); toastT = setTimeout(() => { t.hidden = true; }, bad ? 7000 : 3500);
}
async function act(promise, ok) {
  try { const r = await promise; if (ok) toast(ok); return r; } catch (e) { toast(String(e), true); return undefined; }
}
const STOP_TEXT = {
  closed: a => `${a} closed.`, ended: a => `${a} was stopped.`, not_running: a => `${a} is not running.`,
  denied: a => `Windows did not allow stopping ${a}. It may belong to another user or need administrator rights.`,
  protected: a => `${a} is part of Windows and was not stopped.`,
};
async function stopApp(app, force) {
  const r = await act(invoke('stop_app', { appName: app, force }));
  if (!r) return;
  if (r.result === 'asked') { S.asked.add(app); toast(`Asked ${app} to close. If it does not close, use Force stop.`); setTimeout(() => render(), 3000); }
  else { S.asked.delete(app); toast(STOP_TEXT[r.result](app), r.result === 'denied'); }
}
async function installUpdate() {
  S.update = await act(invoke('install_update')) || S.update;
  pollUpdate();
}

/* ================= Render ================= */
// Writes only when the markup changed, so a quiet refresh does not touch the page.
function setHTML(el, html) { if (el._html !== html) { el._html = html; el.innerHTML = html; return true; } return false; }
async function render(scrollTop) {
  const gen = ++renderGen;
  if (scrollTop) momentCache.clear();
  let html;
  try {
    S.side = await invoke('sidebar');
    if (!S.side.ssh && S.page === 'keys') S.page = 'overview';
    renderSide();
    CHARTS.length = 0;
    html = S.page === 'overview' ? await pageOverview()
      : S.page === 'findings' ? await pageFindings()
      : S.page === 'keys' ? await pageKeys()
      : S.page === 'settings' ? await pageSettings()
      : S.machine === 'pc' ? await pagePC() : await pageServer(S.machine);
  } catch (e) {
    if (gen !== renderGen) return;
    html = head('Something went wrong', '') + `<div class="body"><div class="err">${ic('crit', 'i fi')}<div><div class="t">This page could not load</div><pre>${esc(String(e))}</pre><div class="toolbar"><button class="btn" data-go="overview">Back to Overview</button></div></div></div></div>`;
    CHARTS.length = 0;
  }
  if (gen !== renderGen) return;
  renderSide();
  const main = document.getElementById('main');
  const changed = setHTML(main, html);
  if (scrollTop) main.scrollTop = 0;
  if (changed) requestAnimationFrame(drawCharts);
  if (!render.shown) { render.shown = true; invoke('ready').then(r => { if (r?.smoke) smokeRun(); }).catch(() => {}); }
}
// Live pages refresh every 5 s, unless the user is pointing at a chart, typing, or has a dialog open.
function quietRefresh() {
  if (S.smoke || document.hidden || S.hovering || document.getElementById('scrim').classList.contains('open')) return;
  const f = document.activeElement;
  if (f && (f.tagName === 'SELECT' || f.tagName === 'INPUT') && f.type !== 'checkbox') return;
  render();
}
setInterval(quietRefresh, 5000);

/* ================= Events ================= */
document.addEventListener('click', async e => {
  const t = e.target.closest('button,[data-go],[data-appsel],[data-recsel],[data-pcapp]');
  if (!t) { if (e.target.id === 'scrim') closeDialog(); return; }
  const ds = t.dataset;
  if (ds.retry !== undefined) { e.stopPropagation(); await act(invoke('server_retry', { alias: ds.retry }), `Trying ${ds.retry} again`); return setTimeout(render, 1500); }
  if (ds.watch !== undefined) { e.stopPropagation(); await act(invoke('server_watch', { alias: ds.watch, on: ds.on === '1' })); return render(); }
  if (ds.recdel !== undefined) { e.stopPropagation(); return dialog('delete', Number(ds.recdel)); }
  if (ds.go) return go(ds.go, ds.m);
  if (ds.tab) { S.tab = ds.tab; S.pinned = null; S.sort = S.machine === 'pc' ? 'cpu' : 'mem'; return render(true); }
  if (ds.range) { S.range = Number(ds.range); S.pinned = null; return render(); }
  if (ds.sort) { S.sort = ds.sort; return render(); }
  if (ds.kind) { S.srvKind = ds.kind; return render(); }
  if (ds.recmin !== undefined) { S.recMinutes = Number(ds.recmin); return render(); }
  if (ds.theme) return setTheme(ds.theme, true);
  if (ds.appsel) { S.appSel = ds.appsel; return render(); }
  if (ds.pcapp) { S.tab = 'history'; S.histApp = ds.pcapp; S.pinned = null; return render(true); }
  if (ds.recsel !== undefined) { S.recSel = ds.recsel ? Number(ds.recsel) : null; return render(); }
  if (ds.record) { S.tab = 'recordings'; S.recApp = ds.record; return render(true); }
  if (ds.stop) return stopApp(ds.stop, false).then(() => render());
  if (ds.force) return dialog('force', ds.force);
  if (ds.recstop) { await act(invoke('stop_recording', { id: Number(ds.recstop) }), 'Recording saved'); return render(); }
  if (ds.dialog) return dialog(ds.dialog);
  if (t.hasAttribute('data-close')) return closeDialog();
  if (t.hasAttribute('data-confirm')) return confirmDialog();
  if (t.hasAttribute('data-unpin')) { S.pinned = null; return render(); }
  switch (ds.action) {
    case 'refresh': S.keys = null; momentCache.clear(); return render();
    case 'keys-again': S.keys = null; return render();
    case 'start-recorder': await act(invoke('start_recorder'), 'Recorder started'); return setTimeout(render, 1200);
    case 'rec-start': {
      const app = document.getElementById('recapp')?.value; if (!app) return;
      await act(invoke('start_recording', { appName: app, minutes: S.recMinutes || null }), `Recording ${app}`);
      return render();
    }
    case 'check': S.update = await act(invoke('check_update')) || S.update; pollUpdate(); return render();
    case 'install': return installUpdate();
  }
});
document.addEventListener('change', async e => {
  const t = e.target;
  if (t.id === 'ssh-toggle') { await act(invoke('set_ssh', { on: t.checked }), t.checked ? 'Servers over SSH are on' : 'Servers over SSH are off'); S.keys = null; return render(); }
  if (t.id === 'autostart') { await act(invoke('set_autostart', { on: t.checked })); return render(); }
  if (t.id === 'retention') { await act(invoke('set_retention', { days: Number(t.value) }), `History is kept for ${t.value} days`); t.blur(); return render(); }
  if (t.dataset.host) { await act(invoke('server_watch', { alias: t.dataset.host, on: t.checked })); return render(); }
  if (t.id === 'ffilter') { S.filter = t.value; t.blur(); return render(); }
  if (t.id === 'histapp') { S.histApp = t.value; S.pinned = null; t.blur(); return render(); }
  if (t.id === 'recapp') { S.recApp = t.value; t.blur(); }
});
addEventListener('keydown', e => { if (e.key === 'Escape' && pendingDialog) closeDialog(); });

/* ================= Theme and updates ================= */
const mq = matchMedia('(prefers-color-scheme: dark)');
function applyTheme(v) {
  const root = document.documentElement; root.dataset.theme = v;
  root.classList.toggle('os-dark', v === 'system' && mq.matches);
}
async function setTheme(v, save) {
  S.theme = v; applyTheme(v);
  if (save) await act(invoke('set_theme', { theme: v }));
  const main = document.getElementById('main'); main._html = null;
  render();
}
mq.addEventListener('change', () => { applyTheme(S.theme); document.getElementById('main')._html = null; render(); });
let rt; addEventListener('resize', () => { clearTimeout(rt); rt = setTimeout(drawCharts, 80); });

let updT;
async function pollUpdate() {
  clearTimeout(updT);
  try { S.update = await invoke('update_state'); } catch { /* keep the last state */ }
  renderSide();
  if (['checking', 'downloading', 'restarting'].includes(S.update.state)) updT = setTimeout(pollUpdate, 800);
  else if (S.page === 'settings') render();
}
async function checkUpdates() { try { S.update = await invoke('check_update'); } catch { } pollUpdate(); }

(async function start() {
  try { const st = await invoke('settings'); S.theme = st.theme; } catch { }
  applyTheme(S.theme);
  await render();
  checkUpdates();
  setInterval(checkUpdates, 30 * 60 * 1000);
})();

/* ================= Smoke run (--smoke) ================= */
// Visits every view in the hidden window and reports what rendered, any errors, and clipped text.
async function smokeRun() {
  S.smoke = true;
  const errors = [], out = [];
  addEventListener('error', e => errors.push(String(e.message)));
  addEventListener('unhandledrejection', e => errors.push(String(e.reason)));
  const settle = () => new Promise(r => requestAnimationFrame(() => setTimeout(r, 80)));
  const flat = el => el ? el.innerText.replace(/\s+/g, ' ').trim() : null;
  const visit = async (label, set) => {
    Object.assign(S, { pinned: null, recSel: null }, set);
    try { await render(true); } catch (e) { errors.push(label + ': ' + e); }
    await settle();
    const main = document.getElementById('main');
    const failed = main.querySelector('.head h1')?.textContent === 'Something went wrong';
    const clipped = [...document.querySelectorAll('.summary .v, .summary .d, .tile .v, .tile .d, .head h1, .panel-head h2, .btn, .tab, .nav, .chart-top, .cellv .top')]
      .filter(el => el.scrollWidth > el.clientWidth + 1).map(el => el.className + ': ' + el.textContent.trim().slice(0, 50));
    const r = { label, failed, error: failed ? main.querySelector('pre')?.textContent : null, charts: CHARTS.length, drawn: main.querySelectorAll('.canvas svg path[d^="M"]').length, clipped, text: flat(main).slice(0, 700) };
    // Hover the first chart with data and check the tooltip and the synced crosshairs.
    const c = CHARTS.find(o => o.pts.some(p => p[1] != null));
    if (c) {
      const el = document.getElementById(c.id + '-c'), b = el.getBoundingClientRect();
      el.dispatchEvent(new MouseEvent('mousemove', { clientX: b.left + b.width * 0.9, clientY: b.top + 10, bubbles: true }));
      await new Promise(res => setTimeout(res, 500));
      r.tip = flat(el.querySelector('.tip'));
      r.synced = [...document.querySelectorAll('.canvas .hover')].filter(g => g.style.display !== 'none').length;
      el.dispatchEvent(new MouseEvent('mouseleave'));
      S.hovering = false;
    }
    out.push(r);
  };
  await visit('overview', { page: 'overview', machine: null });
  await visit('findings', { page: 'findings', machine: null, range: 604800 });
  for (const tab of ['now', 'history', 'apps', 'running', 'recordings', 'insights']) await visit('pc ' + tab, { page: 'machine', machine: 'pc', tab, range: 86400 });
  const app = (S.pcHead?.apps || [])[0]?.app;
  if (app) await visit('pc history ' + app, { page: 'machine', machine: 'pc', tab: 'history', histApp: app });
  await visit('pc history pinned', { page: 'machine', machine: 'pc', tab: 'history', histApp: '', range: 21600 });
  const cpu = CHARTS.find(o => o.id === 'h-cpu'), last = cpu && [...cpu.pts].reverse().find(p => p[1] != null);
  if (last) { S.pinned = last[0]; await render(); await settle(); out[out.length - 1].pinned = flat(document.querySelector('.moment'))?.slice(0, 400) || null; }
  await visit('settings', { page: 'settings', machine: null });
  if (S.side?.ssh) {
    await visit('keys', { page: 'keys', machine: null });
    for (const sv of S.side.servers) for (const tab of ['summary', 'processes', 'history', 'findings']) await visit(`server ${sv.alias} ${tab}`, { page: 'machine', machine: sv.alias, tab, range: 86400 });
  }
  await invoke('smoke_report', { report: JSON.stringify({ errors, size: [innerWidth, innerHeight], pages: out }, null, 1) });
}
