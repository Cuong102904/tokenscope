'use strict';
// tokenscope dashboard. Filters live in the query string, the tab in the hash.
const $ = id => document.getElementById(id);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const fmtMoney = v => '$' + Number(v).toLocaleString('en-US', {minimumFractionDigits:2, maximumFractionDigits:2});
const num = v => Number(v).toLocaleString('en-US');
const pct = v => (v * 100).toLocaleString('en-US', {maximumFractionDigits:1}) + '%';
// "1 session", "3 sessions"
const plural = (n, w) => num(n) + ' ' + w + (Number(n) === 1 ? '' : 's');
// cost: Option<f64> + priced lower bound -> "$1.00" | "≥ $0.50" | "n/a"
const costText = (cost, priced) => cost != null ? fmtMoney(cost) : (priced > 0 ? '≥ ' + fmtMoney(priced) : 'n/a');
const parts = p => String(p || '').split('/').filter(Boolean);
const base = p => parts(p).slice(-1)[0] || String(p || '') || '(unknown)';
// Display label per project path: the last component, or the shortest
// distinguishing suffix when several known projects share it ("worktrees/x" vs "DEV/x").
let projLabels = new Map();
function buildProjLabels(paths) {
  const m = new Map();
  let todo = [...new Set(paths)], k = 1;
  while (todo.length) {
    const groups = new Map();
    for (const p of todo) {
      const ps = parts(p), key = ps.slice(-k).join('/') || base(p);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(p);
    }
    todo = [];
    for (const [key, g] of groups) {
      // unique, or nothing left to add: settle on this suffix
      if (g.length === 1 || g.every(p => parts(p).length <= k)) g.forEach(p => m.set(p, key));
      else todo.push(...g);
    }
    k++;
  }
  projLabels = m;
}
const projName = p => projLabels.get(p) || base(p);
// "Nov 6, 14:05"; the year is added when it is not the current one
const fmtTs = ts => {
  const d = new Date(ts * 1000);
  const o = {month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23'};
  if (d.getFullYear() !== new Date().getFullYear()) o.year = 'numeric';
  return d.toLocaleString('en-US', o);
};
const fmtClock = () => new Date().toLocaleTimeString('en-GB');
const tickMoney = v => '$' + Number(v).toLocaleString('en-US', {maximumFractionDigits: v < 10 ? 2 : 0});

const TABS = ['overview', 'limits', 'cost', 'sessions', 'skills', 'active'];
const FILTERS = ['days', 'vendor', 'model', 'project'];
const DEFAULT_DAYS = '30';
const charts = {};
let sessions = [];
let sort = {key: 'cost', dir: -1};

// ---------- URL state ----------
function urlState() {
  const p = new URLSearchParams(location.search);
  const days = p.get('days') || DEFAULT_DAYS;
  return {days, vendor: p.get('vendor') || '', model: p.get('model') || '', project: p.get('project') || '', session: p.get('s') || '',
    lim: p.get('lim') || '', lr: p.get('lr') || ''};
}
function writeUrl(st) {
  const p = new URLSearchParams();
  for (const k of FILTERS) if (st[k]) p.set(k, st[k]);
  if (st.session) p.set('s', st.session);
  if (st.lim) p.set('lim', st.lim);
  if (st.lr) p.set('lr', st.lr);
  history.replaceState(null, '', '?' + p + location.hash);
}
function setState(patch) { writeUrl({...urlState(), ...patch}); }
function apiQuery(extra) {
  const st = urlState(), p = new URLSearchParams();
  for (const k of FILTERS) if (st[k]) p.set(k, st[k]);
  for (const [k, v] of Object.entries(extra || {})) p.set(k, v);
  return p.toString();
}
const currentTab = () => { const h = location.hash.slice(1); return TABS.includes(h) ? h : 'overview'; };
const vendorQuery = () => new URLSearchParams(urlState().vendor ? {vendor: urlState().vendor} : {}).toString();
// Filters that mean something on each tab; the others are greyed out there.
const TAB_FILTERS = {limits: ['vendor'], active: ['vendor', 'model', 'project']};
const FILTER_NAME = {days: 'time range', vendor: 'vendor', model: 'model', project: 'project'};
let modelVendors = {};
// models used by `vendor` (all models when no vendor is selected)
const modelsFor = (models, vendor) => vendor ? models.filter(m => m && (modelVendors[m] || []).includes(vendor)) : models;

// ---------- colours (fixed categorical order; colour follows the entity) ----------
const css = n => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
const SLOTS = 8;
const OTHER = 'Other';
const slotOf = new Map();       // model name -> slot 1..8, assigned once per page load
const VENDOR_SLOT = {claude: 2, codex: 3, opencode: 1, agy: 7};
function modelColor(m) {
  if (m === OTHER) return css('--other');
  if (!slotOf.has(m) && slotOf.size < SLOTS) slotOf.set(m, slotOf.size + 1);
  return slotOf.has(m) ? css('--s' + slotOf.get(m)) : css('--other');
}
const vendorColor = v => VENDOR_SLOT[v] ? css('--s' + VENDOR_SLOT[v]) : css('--other');

function chartBase() {
  const mute = css('--mute'), grid = css('--grid'), ink = css('--ink');
  Chart.defaults.color = mute;
  Chart.defaults.borderColor = grid;
  Chart.defaults.font.family = getComputedStyle(document.body).fontFamily;
  return {mute, grid, ink, surface: css('--card')};
}
// Charts never animate and are updated in place. Animations only run on animation
// frames, which hidden, occluded or throttled tabs don't get: a chart created or
// re-created there stayed empty or half drawn until F5, and re-creating on every
// refresh also killed an open tooltip and re-grew the bars.
Chart.defaults.animation = false;
const chartSig = {};
function draw(id, cfg) {
  const old = charts[id];
  const sig = JSON.stringify([cfg.type, cfg.data, cfg.options]); // functions drop out: data + colours only
  if (old && chartSig[id] === sig) return; // nothing changed: leave it (and its tooltip) alone
  chartSig[id] = sig;
  if (old && old.config.type === cfg.type && !(cfg.plugins || []).length) {
    old.data = cfg.data;
    old.options = cfg.options;
    old.update('none'); // synchronous redraw, no animation frames needed
    return;
  }
  // inline plugins close over the old data, so those charts are rebuilt (drawn synchronously)
  if (old) old.destroy();
  charts[id] = new Chart($(id), cfg);
}
function undraw(id) {
  if (charts[id]) { charts[id].destroy(); delete charts[id]; }
  delete chartSig[id];
}
const legendCfg = c => ({position: 'bottom', labels: {color: c.ink, boxWidth: 10, boxHeight: 10, usePointStyle: true, pointStyle: 'rectRounded'}});

// Stacked bars per day, one dataset per model; models past the 7th by total fold into "Other".
function drawDaily(id, daily) {
  const c = chartBase();
  const totals = {};
  daily.forEach(d => Object.entries(d.by_model).forEach(([m, v]) => totals[m] = (totals[m] || 0) + v));
  const ranked = Object.keys(totals).sort((a, b) => totals[b] - totals[a]);
  // keep slot assignment stable: the biggest models get the first free slots
  ranked.forEach(modelColor);
  const shown = ranked.filter(m => slotOf.has(m)).slice(0, SLOTS - 1);
  const rest = ranked.filter(m => !shown.includes(m));
  const series = shown.map(m => ({label: m, data: daily.map(d => d.by_model[m] || 0), color: modelColor(m)}));
  if (rest.length) series.push({label: OTHER, data: daily.map(d => rest.reduce((s, m) => s + (d.by_model[m] || 0), 0)), color: modelColor(OTHER)});
  draw(id, {
    type: 'bar',
    data: {labels: daily.map(d => d.day.slice(5)), datasets: series.map(s => ({
      label: s.label, data: s.data, backgroundColor: s.color, borderColor: c.surface, borderWidth: {top: 1}, borderRadius: 2, maxBarThickness: 28,
    }))},
    options: {
      responsive: true, maintainAspectRatio: false, interaction: {mode: 'index', intersect: false},
      scales: {
        x: {stacked: true, grid: {display: false}, ticks: {maxRotation: 0, autoSkip: true, autoSkipPadding: 8}},
        y: {stacked: true, beginAtZero: true, ticks: {callback: tickMoney}, grid: {color: c.grid}},
      },
      plugins: {
        legend: legendCfg(c),
        tooltip: {
          filter: it => it.parsed.y > 0,
          callbacks: {
            title: items => daily[items[0].dataIndex].day,
            label: it => it.dataset.label + ': ' + fmtMoney(it.parsed.y),
            footer: items => {
              const d = daily[items[0].dataIndex];
              const sum = Object.values(d.by_model).reduce((a, b) => a + b, 0);
              return ['Total: ' + fmtMoney(sum)].concat(d.unpriced.length ? ['No price (n/a): ' + d.unpriced.join(', ')] : []);
            },
          },
        },
      },
    },
  });
}

// ---------- rendering ----------
function kpi(label, value, sub) {
  return `<div class="card kpi"><div class="v">${esc(value)}</div><div class="l">${esc(label)}</div>${sub ? `<div class="s">${esc(sub)}</div>` : ''}</div>`;
}
// same rule as Totals::display: '$X' fully priced, '≥ $X' partly priced, 'n/a' nothing priced
const totalText = t => !t.unpriced_models.length ? fmtMoney(t.cost)
  : t.cost > 0 ? '≥ ' + fmtMoney(t.cost) : 'n/a';
const totalSub = t => plural(t.sessions, 'session') + ' · ' + plural(t.messages, 'message');

function setWarn(unpriced) {
  const w = $('warn');
  w.hidden = !unpriced.length;
  w.textContent = unpriced.length ? 'Models without a price (shown as "n/a", not counted in totals): ' + unpriced.join(', ') : '';
}

// a request that never answers must not leave `busy` set forever (no more refreshes until F5)
const FETCH_TIMEOUT_MS = 15000;
async function getJson(path) {
  const r = await fetch(path, {cache: 'no-store', signal: AbortSignal.timeout(FETCH_TIMEOUT_MS)});
  if (!r.ok) throw new Error(path + ': HTTP ' + r.status + ' ' + (await r.text()));
  return r.json();
}

async function renderOverview() {
  const d = await getJson('/api/overview?' + apiQuery());
  $('ovKpis').innerHTML = [
    kpi('Today', totalText(d.totals_today), totalSub(d.totals_today)),
    kpi('7 days', totalText(d.totals_7d), totalSub(d.totals_7d)),
    kpi('30 days', totalText(d.totals_30d), totalSub(d.totals_30d)),
    kpi('Open sessions', num(d.active_open), num(d.active_working) + ' working'),
  ].join('');
  setWarn(d.totals_30d.unpriced_models);
  drawDaily('cOvDaily', d.daily);
  // accounts with no current reading at all (old logs, other accounts) only in the Limits tab
  const old = new Set(limitAccounts(d.limits_mini.filter(c => !c.no_data)).filter(a => a.cards.every(c => c.stale)).map(a => a.key));
  const shown = d.limits_mini.filter(c => !old.has(c.vendor + '|' + c.account));
  ovCards = shown;
  renderLimitCards($('ovLimits'), shown, true);
  autoRefresh(shown);
  if (old.size) $('ovLimits').insertAdjacentHTML('beforeend', `<div class="note" style="margin:6px 0 0"><a href="#limits">+${esc(old.size)} ${old.size === 1 ? 'account' : 'accounts'} without a current reading — see the Limits tab</a></div>`);
}

// ---------- limits ----------
const VENDOR_NAME = {claude: 'Claude', codex: 'Codex', opencode: 'OpenCode', agy: 'Antigravity (agy)'};
const vendorName = v => VENDOR_NAME[v] || v;
const SCOPE_BASE = {five_hour: '5-hour', weekly: 'Weekly', seven_day: 'Weekly', monthly: 'Monthly'};
// "five_hour" -> "5-hour", "weekly:Gemini" -> "Weekly · Gemini", "x:window_90m" -> "90-min · x"
function scopeLabel(scope) {
  const ps = String(scope).split(':');
  const i = ps.findIndex(p => SCOPE_BASE[p] || /^window_\d+m$/.test(p));
  if (i < 0) return String(scope);
  const b = SCOPE_BASE[ps[i]] || ps[i].slice(7, -1) + '-min';
  const rest = ps.filter((_, j) => j !== i).join(':');
  return rest ? b + ' · ' + rest : b;
}
const fmtPct = v => Number(v).toLocaleString('en-US', {maximumFractionDigits: 1}) + '%';
function dur(sec) {
  sec = Math.max(0, Math.round(sec));
  if (sec < 60) return 'under 1 min';
  const d = Math.floor(sec / 86400), h = Math.floor(sec % 86400 / 3600), m = Math.floor(sec % 3600 / 60);
  if (d) return d + 'd' + (h ? ' ' + h + 'h' : '');
  if (h) return h + 'h' + (m ? ' ' + m + 'm' : '');
  return m + ' min';
}
const nowSec = () => Date.now() / 1000;
const until = ts => 'in ' + dur(ts - nowSec());
const ago = ts => nowSec() - ts < 60 ? 'just now' : dur(nowSec() - ts) + ' ago';
// bar level: green < 70, amber < 90, red >= 90 (the number is always shown too)
const level = p => p >= 90 ? 'hi' : p >= 70 ? 'mid' : 'ok';
const limKey = c => [c.vendor, c.account, c.scope].join('|');
function splitLimKey(k) { const [vendor, account, ...rest] = k.split('|'); return {vendor, account: account || '', scope: rest.join('|')}; }

// ---------- limit table: one row per account, one column per window ----------
const WINDOW_RANK = {five_hour: 0, weekly: 2, seven_day: 2, monthly: 3};
const windowRank = scope => { const b = String(scope).split(':')[0]; return WINDOW_RANK[b] ?? (/^window_\d+m$/.test(b) ? 1 : 4); };
const byWindow = (a, b) => windowRank(a) - windowRank(b) || String(a).localeCompare(String(b));
const pctOf = c => Math.max(0, Math.min(100, Number(c.used_percent)));
// "14:05" when it is less than a day away, else "Nov 11"
const resetShort = ts => ts - nowSec() < 86400
  ? new Date(ts * 1000).toLocaleTimeString('en-GB', {hour: '2-digit', minute: '2-digit'})
  : new Date(ts * 1000).toLocaleDateString('en-US', {month: 'short', day: 'numeric'});
const cap = s => s ? s[0].toUpperCase() + s.slice(1) : s;

// what happens next for a window that has reset (its number belongs to the previous window)
function expiredNext(c) {
  if (c.account === '') return 'Old logs: nothing left to refresh';
  if (toolBusy[c.vendor]) return '<span class="spin" aria-hidden="true"></span>Updating…';
  if (autoTried[limKey(c)] === c.measured_at) return c.vendor === 'codex'
    ? 'Not the signed-in Codex account: waiting for its logs' : 'No newer reading from ' + esc(TOOL_NAME[c.vendor]) + ' yet';
  if (!autoCandidate(c)) return TOOL_NAME[c.vendor] ? 'Click Refresh ' + esc(TOOL_NAME[c.vendor]) : 'Waiting for a new reading';
  if (toolRetryAt[c.vendor]) return 'Updates at ' + esc(fmtTs(toolRetryAt[c.vendor])) + ' · ' + esc(until(toolRetryAt[c.vendor]));
  return 'Waiting for a new reading';
}

function limitCell(c, mini) {
  if (!c) return '<td class="lc none">—</td>';
  const p = pctOf(c), lv = c.expired || c.stale ? 'old' : level(p);
  const tip = [fmtPct(c.used_percent) + ' used · ' + fmtPct(100 - p) + ' left',
    'Measured ' + fmtTs(c.measured_at) + ' (' + c.source + ') · ' + ago(c.measured_at),
    c.resets_at != null ? (c.expired ? 'Reset at ' : 'Resets ') + fmtTs(c.resets_at) : '',
    c.plan_type ? 'Plan ' + c.plan_type : '',
    c.expired ? '' : c.eta != null ? 'Projected to hit 100%: ' + fmtTs(c.eta) : 'No ETA: ' + c.eta_note,
    c.warning ? 'Warning: ' + c.warning : '', mini ? '' : 'Click for the history chart'].filter(Boolean).join('\n');
  let main, sub;
  if (c.expired) {
    main = `${esc(fmtPct(c.used_percent))} <span class="lmeta">before the reset</span>`;
    sub = expiredNext(c);
  } else {
    main = p >= 100 ? 'Limit reached' : esc(fmtPct(100 - p)) + ' left';
    sub = c.resets_at != null ? 'resets ' + esc(resetShort(c.resets_at)) + ' · ' + esc(until(c.resets_at)) : '';
    if (c.stale) sub += (sub ? ' · ' : '') + '<span class="badge">STALE</span>';
  }
  const sel = !mini && limKey(c) === urlState().lim;
  return `<td class="lc${sel ? ' sel' : ''}" data-k="${esc(limKey(c))}" role="button" tabindex="0" title="${esc(tip)}">` +
    `<div class="lv ${lv}">${main}${c.warning ? ' <span class="lwarn" title="' + esc(c.warning) + '">⚠</span>' : ''}</div>` +
    `<div class="bar"><i class="${lv}" style="width:${esc(p)}%"></i></div><div class="lmeta">${sub}</div></td>`;
}

// one line answer to "can I use this account now?"
function accountStatus(cs) {
  const live = cs.filter(c => !c.expired && !c.stale);
  const full = live.filter(c => pctOf(c) >= 100);
  if (full.length) {
    const back = Math.max(...full.map(c => c.resets_at || 0));
    return ['hi', 'Limit reached' + (back ? ' until ' + resetShort(back) : '')];
  }
  if (cs.some(c => c.expired || c.stale) && toolBusy[cs[0].vendor]) return ['', '<span class="spin" aria-hidden="true"></span>Updating…'];
  if (!live.length) return ['mute', 'No current reading · last ' + ago(Math.max(...cs.map(c => c.measured_at)))];
  const near = live.filter(c => pctOf(c) >= 90);
  if (near.length) return ['mid', 'Nearly out · ' + near.map(c => scopeLabel(c.scope) + ' ' + fmtPct(100 - pctOf(c)) + ' left').join(', ')];
  return ['ok', 'Available'];
}

// accounts of the given cards: [{key, cards}], most recently measured first
function limitAccounts(cards) {
  const m = new Map();
  for (const c of cards) { if (!m.has(c.vendor + '|' + c.account)) m.set(c.vendor + '|' + c.account, []); m.get(c.vendor + '|' + c.account).push(c); }
  const newest = cs => Math.max(...cs.map(c => c.measured_at));
  return [...m].map(([key, cs]) => ({key, cards: cs})).sort((a, b) => newest(b.cards) - newest(a.cards));
}

function renderLimitCards(el, cards, mini) {
  const vendors = new Map();
  for (const c of cards) { if (!vendors.has(c.vendor)) vendors.set(c.vendor, []); vendors.get(c.vendor).push(c); }
  el.innerHTML = vendors.size ? [...vendors].map(([v, cs]) => {
    const head = `<div class="lim-group"><h3>${esc(vendorName(v))}</h3>`;
    const nodata = cs.find(c => c.no_data);
    if (nodata) return head + `<div class="lmeta">${esc(nodata.eta_note)}</div></div>`;
    const scopes = [...new Set(cs.map(c => c.scope))].sort(byWindow);
    const rows = limitAccounts(cs).map(({cards: acs}) => {
      const a = acs.reduce((x, y) => (y.measured_at > x.measured_at ? y : x));
      const [cls, status] = accountStatus(acs);
      return `<tr><td class="acc"><div class="an" title="${esc(a.account_label)}">${esc(a.account_label)}</div>` +
        `<div class="lmeta">${a.plan_type ? esc(cap(a.plan_type)) + ' · ' : ''}measured ${esc(ago(a.measured_at))}</div></td>` +
        scopes.map(sc => limitCell(acs.find(c => c.scope === sc), mini)).join('') +
        `<td class="st ${cls}">${status}</td></tr>`;
    }).join('');
    return head + `<div class="scroll"><table class="ltab${mini ? ' mini' : ''}"><thead><tr><th>Account</th>` +
      scopes.map(sc => `<th>${esc(scopeLabel(sc))}</th>`).join('') + '<th>Status</th></tr></thead><tbody>' + rows + '</tbody></table></div></div>';
  }).join('') : '<div class="empty">No rate-limit data yet.</div>';
}

let limCards = [];
async function renderLimits(data) {
  const d = data || await getJson('/api/limits?' + vendorQuery());
  limCards = d.cards;
  renderLimitCards($('limCards'), d.cards, false);
  autoRefresh(d.cards);
  $('tLimEvents').innerHTML = '<thead><tr>' + ['Time', 'Vendor', 'Account', 'Scope', 'Resets at']
    .map(h => `<th class="l">${esc(h)}</th>`).join('') + '</tr></thead><tbody>' +
    (d.events.length ? d.events.map(e => `<tr><td class="l">${esc(fmtTs(e.ts))}</td><td class="l">${esc(vendorName(e.vendor))}</td>` +
      `<td class="l">${esc(e.account_label)}</td><td class="l">${esc(scopeLabel(e.scope))}</td><td class="l">${esc(e.resets_at != null ? fmtTs(e.resets_at) : '—')}</td></tr>`).join('')
      : '<tr><td colspan="5" class="empty">No limit hits in the last 30 days.</td></tr>') + '</tbody>';
  await renderLimitChart();
}

// Chart.js plugin: dashed vertical line + "reset" label at each x (ms)
const resetLines = (xs, color) => ({id: 'resetLines', afterDatasetsDraw(ch) {
  const {ctx, chartArea: a, scales: {x}} = ch;
  ctx.save();
  ctx.strokeStyle = color; ctx.fillStyle = color; ctx.lineWidth = 1; ctx.setLineDash([4, 4]);
  ctx.font = '11px ' + Chart.defaults.font.family;
  for (const v of xs) {
    const px = x.getPixelForValue(v);
    if (px < a.left || px > a.right) continue;
    ctx.beginPath(); ctx.moveTo(px, a.top); ctx.lineTo(px, a.bottom); ctx.stroke();
    ctx.fillText('reset', px + 3, a.top + 10);
  }
  ctx.restore();
}});

let limToken = 0, limChartSig = '';
async function renderLimitChart() {
  const my = ++limToken;
  const st = urlState();
  const hours = st.lr === '168' ? 168 : 24;
  document.querySelectorAll('#limRange button').forEach(b => b.classList.toggle('on', b.dataset.h === String(hours)));
  if (!st.lim) {
    $('limChartTitle').textContent = 'click a card above';
    undraw('cLimit');
    limChartSig = '';
    return;
  }
  const k = splitLimKey(st.lim);
  const card = limCards.find(c => limKey(c) === st.lim);
  const title = vendorName(k.vendor) + ' · ' + scopeLabel(k.scope) + ' · ' + (card ? card.account_label : k.account || '—');
  const h = await getJson('/api/limits/history?' + new URLSearchParams({...k, hours}));
  if (my !== limToken) return; // a later click owns the chart now
  $('limChartTitle').textContent = title + (h.points.length ? '' : ' — no readings in this range');
  const now = Date.now(), from = now - hours * 3600e3;
  const c = chartBase(), col = vendorColor(k.vendor);
  // the 30 s auto-refresh: same data and theme -> keep the chart as is (no flicker)
  const view = st.lim + '|' + hours;
  const sig = JSON.stringify([view, h, col, c.mute, c.grid]);
  if (sig === limChartSig && charts.cLimit) return;
  limChartSig = sig;
  draw('cLimit', {
    type: 'line',
    data: {datasets: [{label: '% used', data: h.points.map(([t, v]) => ({x: t * 1000, y: v})),
      borderColor: col, backgroundColor: col, borderWidth: 2, pointRadius: h.points.length < 60 ? 2 : 0, pointHoverRadius: 5, tension: 0}]},
    options: {responsive: true, maintainAspectRatio: false, interaction: {mode: 'nearest', axis: 'x', intersect: false},
      scales: {
        x: {type: 'linear', min: from, max: now, ticks: {maxTicksLimit: 6, callback: v => fmtTs(v / 1000)}, grid: {display: false}},
        y: {min: 0, max: 100, ticks: {callback: v => v + '%'}, grid: {color: c.grid}},
      },
      plugins: {legend: {display: false}, tooltip: {callbacks: {title: items => fmtTs(items[0].parsed.x / 1000), label: it => fmtPct(it.parsed.y) + ' used'}}}},
    plugins: [resetLines(h.resets.map(r => r * 1000), c.mute)],
  });
}

// On-demand limits: `claude /usage`, `codex app-server` and `agy /usage`.
// A reading whose window has reset or that went stale updates itself: the tool is asked right away, or
// when the server's 5-minute throttle allows, and the card says which.
const TOOL_NAME = {claude: 'Claude', codex: 'Codex', agy: 'agy'};
// Claude and Codex refresh on their own: neither read calls a model. agy's may use quota
// and it reports a reset seconds after each reading, so it would re-run every 5 minutes.
const AUTO_TOOLS = ['claude', 'codex'];
// card key -> measured_at of the reading a successful auto refresh did not replace (another
// account than the logged-in one, say): that reading is not retried again
const autoTried = {};
const toolBusy = {}, toolRetryAt = {}, toolTimer = {};
let ovCards = [];
function refreshText(tool, r) {
  if (!r) return '';
  const n = TOOL_NAME[tool];
  switch (r.status) {
    case 'ran': return 'Refreshed ' + n + ' at ' + fmtClock() + '.';
    case 'throttled': return n + ' was refreshed recently (at most once every 5 minutes). It updates again at ' + fmtTs(r.next_allowed) + ' (' + until(r.next_allowed) + ').';
    case 'not_installed': return 'The ' + tool + ' command was not found on PATH.';
    default: return n + ' refresh failed: ' + (r.message || r.status);
  }
}
// redraw the cards from the last data (busy / retry state shows on them)
function rerenderCards() {
  if (currentTab() === 'limits') renderLimitCards($('limCards'), limCards, false);
  if (currentTab() === 'overview') renderLimitCards($('ovLimits'), ovCards, true);
}
function retryAt(tool, at) {
  clearTimeout(toolTimer[tool]);
  toolRetryAt[tool] = at;
  toolTimer[tool] = setTimeout(() => { toolRetryAt[tool] = null; refresh(); }, Math.max(1000, at * 1000 - Date.now() + 1000));
}
async function refreshTool(tool, auto, cards) {
  if (toolBusy[tool]) return;
  toolBusy[tool] = true;
  $(tool + 'Refresh').disabled = true;
  rerenderCards();
  $('refreshMsg').textContent = (auto ? 'Updating ' + TOOL_NAME[tool] + ' limits' : 'Asking ' + TOOL_NAME[tool] + ' for its limits') + '… (up to 30 seconds)';
  try {
    const r = await fetch('/api/limits?refresh=' + tool + '&' + vendorQuery(), {cache: 'no-store', headers: {'X-Tokenscope': '1'}});
    if (!r.ok) throw new Error('HTTP ' + r.status + ' ' + (await r.text()));
    const res = (await r.json()).refresh;
    $('refreshMsg').textContent = refreshText(tool, res);
    if (res?.status === 'throttled') retryAt(tool, res.next_allowed);
    else if (res?.status === 'ran') {
      clearTimeout(toolTimer[tool]); toolRetryAt[tool] = null;
      for (const c of cards || []) autoTried[limKey(c)] = c.measured_at;
    }
    else retryAt(tool, nowSec() + 300); // not installed or failed: try again later, not on every refresh
  } catch (e) {
    $('refreshMsg').textContent = TOOL_NAME[tool] + ' refresh failed: ' + e.message;
    retryAt(tool, nowSec() + 300);
  } finally {
    toolBusy[tool] = false;
    $(tool + 'Refresh').disabled = false;
    refresh();
  }
}
// ask Claude / Codex for a new reading as soon as one of their shown windows has reset or
// its reading went stale; each reading is tried once (the tool only knows the account that
// is logged in now), and a fresh reading becomes eligible again once it goes stale
// readings without an account (old Codex logs) belong to no one the tool can ask
const autoCandidate = c => (c.expired || c.stale) && c.account !== '' && AUTO_TOOLS.includes(c.vendor) && autoTried[limKey(c)] !== c.measured_at;
function autoRefresh(cards) {
  if (document.hidden) return;
  for (const tool of AUTO_TOOLS) {
    const due = cards.filter(c => c.vendor === tool && autoCandidate(c));
    if (due.length && !toolBusy[tool] && !toolRetryAt[tool]) refreshTool(tool, true, due);
  }
}
function selectLimit(k) {
  setState({lim: k});
  document.querySelectorAll('#limCards [data-k]').forEach(el => el.classList.toggle('sel', el.dataset.k === k));
  renderLimitChart().catch(e => { $('status').textContent = 'Failed to load data: ' + e.message; });
}
function onCardActivate(el, handler) {
  const go = ev => { const card = ev.target.closest('[data-k]'); if (card) handler(card.dataset.k); };
  el.addEventListener('click', go);
  el.addEventListener('keydown', ev => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); go(ev); } });
}

async function renderCost() {
  const d = await getJson('/api/cost?' + apiQuery());
  const t = d.totals, e = d.efficiency;
  $('costKpis').innerHTML = [
    kpi('Total cost', totalText(t), totalSub(t)),
    kpi('Cache hit ratio', pct(e.cache_hit_ratio), 'cache read / (input + cache read)'),
    kpi('Subagent share', pct(e.subagent_cost_share), 'subagent cost / total'),
  ].join('');
  setWarn(t.unpriced_models);
  drawDaily('cCostDaily', d.daily);

  const c = chartBase();
  const vendors = d.by_vendor.filter(v => v.priced_cost > 0);
  draw('cVendor', {
    type: 'doughnut',
    data: {labels: vendors.map(v => v.vendor), datasets: [{
      data: vendors.map(v => v.priced_cost), backgroundColor: vendors.map(v => vendorColor(v.vendor)), borderColor: c.surface, borderWidth: 2,
    }]},
    options: {responsive: true, maintainAspectRatio: false, cutout: '62%',
      plugins: {legend: {...legendCfg(c), labels: {...legendCfg(c).labels,
        generateLabels: ch => Chart.overrides.doughnut.plugins.legend.labels.generateLabels(ch).map((l, i) => ({...l, fontColor: c.ink,
          text: vendors[i].vendor + ' ' + costText(vendors[i].cost, vendors[i].priced_cost)}))}},
        tooltip: {callbacks: {label: it => vendors[it.dataIndex].vendor + ': ' + costText(vendors[it.dataIndex].cost, vendors[it.dataIndex].priced_cost)}}}},
  });

  const top = d.by_project.slice(0, 10);
  draw('cProject', {
    type: 'bar',
    data: {labels: top.map(p => projName(p.project)), datasets: [{label: 'Cost', data: top.map(p => p.priced_cost),
      backgroundColor: css('--s1'), borderRadius: 4, borderSkipped: 'start', maxBarThickness: 18}]},
    options: {indexAxis: 'y', responsive: true, maintainAspectRatio: false,
      scales: {x: {beginAtZero: true, ticks: {callback: tickMoney}, grid: {color: c.grid}}, y: {grid: {display: false}, ticks: {color: c.ink}}},
      plugins: {legend: {display: false}, tooltip: {callbacks: {
        title: items => top[items[0].dataIndex].project,
        label: it => { const p = top[it.dataIndex]; return costText(p.cost, p.priced_cost) + ' · ' + num(p.sessions) + ' session'; },
      }}}},
  });

  $('tModels').innerHTML = '<thead><tr>' + ['Model', 'Vendor', 'Cost', 'Messages', 'Input', 'Output', 'Cache read', 'Cache write']
    .map((h, i) => `<th${i === 1 ? ' class="l"' : ''}>${esc(h)}</th>`).join('') + '</tr></thead><tbody>' +
    (d.by_model.length ? d.by_model.map(m => `<tr><td>${esc(m.model)}</td><td class="l">${esc(m.vendor)}</td><td>${esc(m.cost == null ? 'n/a' : fmtMoney(m.cost))}</td>` +
      [m.messages, m.input, m.output, m.cache_read, m.cache_write].map(v => `<td>${esc(num(v))}</td>`).join('') + '</tr>').join('')
      : '<tr><td colspan="8" class="empty">No data.</td></tr>') + '</tbody>';
}

const SESS_COLS = [
  {key: 'project', label: 'Project', l: true, val: s => projName(s.project), cell: s => `<span title="${esc(s.project)}">${esc(projName(s.project))}</span>`},
  {key: 'vendor', label: 'Vendor', l: true, val: s => s.vendor},
  {key: 'model', label: 'Model', l: true, val: s => s.models[0] || '', cell: s => esc(s.models.join(', '))},
  {key: 'messages', label: 'Messages', val: s => s.messages, cell: s => esc(num(s.messages))},
  {key: 'subagent', label: 'Subagent', val: s => s.subagent_messages, cell: s => esc(num(s.subagent_messages))},
  {key: 'cost', label: 'Cost', val: s => s.priced_cost, cell: s => esc(costText(s.cost, s.priced_cost))},
  {key: 'first', label: 'Started', val: s => s.first_ts, cell: s => esc(fmtTs(s.first_ts))},
  {key: 'last', label: 'Last active', val: s => s.last_ts, cell: s => esc(fmtTs(s.last_ts))},
];
const sessKey = s => s.vendor + '/' + s.session_id;

// The table shows one page; sorting covers every session, not just the visible page.
const SESS_PAGE_SIZE = 50;
const SESS_API_LIMIT = 5000; // server-side cap of /api/sessions
let sessPage = 0, sessQuery = '';
function renderSessTable() {
  const col = SESS_COLS.find(c => c.key === sort.key) || SESS_COLS[5];
  const all = [...sessions].sort((a, b) => {
    const x = col.val(a), y = col.val(b);
    return (typeof x === 'number' ? x - y : String(x).localeCompare(String(y))) * sort.dir;
  });
  const pages = Math.max(1, Math.ceil(all.length / SESS_PAGE_SIZE));
  sessPage = Math.min(Math.max(0, sessPage), pages - 1);
  const from = sessPage * SESS_PAGE_SIZE;
  const rows = all.slice(from, from + SESS_PAGE_SIZE);
  const sel = urlState().session;
  $('sessCount').textContent = (all.length >= SESS_API_LIMIT ? 'first ' : '') + plural(all.length, 'session');
  const pager = $('sessPager');
  pager.hidden = pages < 2;
  pager.innerHTML = pages < 2 ? '' :
    `<button type="button" data-p="prev"${sessPage === 0 ? ' disabled' : ''}>‹ Prev</button>` +
    `<span class="pos">${esc(num(from + 1))}–${esc(num(from + rows.length))} of ${esc(num(all.length))} · page ${esc(sessPage + 1)} / ${esc(pages)}</span>` +
    `<button type="button" data-p="next"${sessPage >= pages - 1 ? ' disabled' : ''}>Next ›</button>`;
  $('tSess').innerHTML = '<thead><tr>' + SESS_COLS.map(c =>
    `<th data-k="${esc(c.key)}" class="${c.l ? 'l ' : ''}${c.key === sort.key ? 'sorted' : ''}">${esc(c.label)}${c.key === sort.key ? (sort.dir < 0 ? ' ▾' : ' ▴') : ''}</th>`).join('') +
    '</tr></thead><tbody>' + (rows.length ? rows.map(s =>
      `<tr data-s="${esc(sessKey(s))}"${sessKey(s) === sel ? ' class="sel"' : ''} title="${esc(s.title || s.session_id)}">` +
      SESS_COLS.map(c => `<td${c.l ? ' class="l"' : ''}>${c.cell ? c.cell(s) : esc(c.val(s))}</td>`).join('') + '</tr>').join('')
      : '<tr><td colspan="8" class="empty">No sessions.</td></tr>') + '</tbody>';
}

let tlToken = 0;
async function renderTimeline() {
  const my = ++tlToken;
  const sel = urlState().session;
  if (!sel) { undraw('cTimeline'); return; }
  const i = sel.indexOf('/');
  const vendor = sel.slice(0, i), id = sel.slice(i + 1);
  const s = sessions.find(x => sessKey(x) === sel);
  $('tlTitle').textContent = s ? projName(s.project) + ' · ' + (s.title || id) : id;
  const t = await getJson('/api/session/' + encodeURIComponent(vendor) + '/' + encodeURIComponent(id) + '/timeline');
  if (my !== tlToken) return; // a later click (or refresh) owns the chart now
  $('tlNote').hidden = !t.unpriced_models.length;
  $('tlNote').textContent = t.unpriced_models.length ? 'This line is a lower bound. Models without a price: ' + t.unpriced_models.join(', ') : '';
  const c = chartBase();
  draw('cTimeline', {
    type: 'line',
    data: {datasets: [{label: 'Cumulative cost', data: t.points.map(([ts, v]) => ({x: ts * 1000, y: v})),
      borderColor: vendorColor(vendor), backgroundColor: vendorColor(vendor), borderWidth: 2, pointRadius: 0, pointHoverRadius: 5, stepped: false, tension: 0}]},
    options: {responsive: true, maintainAspectRatio: false, interaction: {mode: 'nearest', axis: 'x', intersect: false},
      scales: {
        x: {type: 'linear', ticks: {maxTicksLimit: 6, callback: v => fmtTs(v / 1000)}, grid: {display: false}},
        y: {beginAtZero: true, ticks: {callback: tickMoney}, grid: {color: c.grid}},
      },
      plugins: {legend: {display: false}, tooltip: {callbacks: {title: items => fmtTs(items[0].parsed.x / 1000), label: it => fmtMoney(it.parsed.y)}}}},
  });
}

async function renderSessions() {
  const q = apiQuery();
  if (q !== sessQuery) { sessQuery = q; sessPage = 0; } // new filters start on page 1; a timed refresh keeps the page
  sessions = await getJson('/api/sessions?' + apiQuery({limit: SESS_API_LIMIT}));
  renderSessTable();
  await renderTimeline();
}

// ---------- skills & tools ----------
// Horizontal bar charts grow with the row count so labels never collide (30 px per row).
const hHeight = (id, n) => { $(id).style.height = Math.max(120, n * 30 + 50) + 'px'; };
let skSig = '';
async function renderSkills() {
  const d = await getJson('/api/skills?' + apiQuery());
  const c = chartBase();
  // 30 s auto-refresh: same data and theme -> leave the charts alone (no flicker)
  const sig = JSON.stringify([d, c.mute, c.grid, c.ink, css('--s1'), css('--s2'), css('--s3'), css('--other')]);
  if (sig === skSig && charts.cSkCost) return;
  skSig = sig;
  // writes `fmt(value)` just past the end of each bar (single-series charts)
  const valueLabels = fmt => ({id: 'valueLabels', afterDatasetsDraw(ch) {
    const {ctx} = ch;
    ctx.save(); ctx.fillStyle = c.ink; ctx.textBaseline = 'middle'; ctx.font = '11px ' + Chart.defaults.font.family;
    ch.getDatasetMeta(0).data.forEach((bar, i) => {
      const v = ch.data.datasets[0].data[i];
      const t = fmt(v, i), w = ctx.measureText(t).width;
      // keep the text inside the chart area when the bar is (nearly) full width
      const x = bar.x + 4 + w > ch.chartArea.right + 30 ? bar.x - w - 4 : bar.x + 4;
      ctx.fillText(t, x, bar.y);
    });
    ctx.restore();
  }});
  const hbar = (id, wrap, labels, datasets, extra) => {
    hHeight(wrap, labels.length);
    draw(id, {type: 'bar', data: {labels, datasets},
      options: {indexAxis: 'y', responsive: true, maintainAspectRatio: false,
        scales: {x: {beginAtZero: true, grid: {color: c.grid}, ...(extra.x || {})}, y: {grid: {display: false}, ticks: {color: c.ink}, ...(extra.y || {})}},
        plugins: {legend: extra.legend || {display: false}, tooltip: {callbacks: extra.tooltip}}},
      plugins: extra.labels ? [valueLabels(extra.labels)] : []});
  };
  // no rows: hide the chart box and show the "No data" line that precedes it
  const empty = (wrap, on) => { $(wrap).hidden = !on; $(wrap).previousElementSibling.hidden = !!on; };

  const top = d.by_cost.slice(0, 15);
  empty('wSkCost', top.length);
  if (top.length) hbar('cSkCost', 'wSkCost', top.map(s => s.skill),
    [{label: 'Cost', data: top.map(s => s.priced_cost), backgroundColor: vendorColor('claude'), borderRadius: 4, borderSkipped: 'start', maxBarThickness: 18}],
    {x: {ticks: {callback: tickMoney}, grace: '15%'}, labels: (v, i) => costText(top[i].cost, top[i].priced_cost), tooltip: {
      label: it => { const s = top[it.dataIndex]; return costText(s.cost, s.priced_cost) + ' · ' + plural(s.messages, 'message'); },
      afterLabel: it => top[it.dataIndex].plugin ? 'plugin: ' + top[it.dataIndex].plugin : ''}});
  else undraw('cSkCost');

  const pl = d.by_plugin.filter(p => p.priced_cost > 0);
  // plugins whose rows are all unpriced have nothing to draw: list them as n/a instead of dropping them
  const na = d.by_plugin.filter(p => p.priced_cost <= 0 && p.cost == null).map(p => p.plugin);
  $('skPluginNa').hidden = !na.length;
  $('skPluginNa').textContent = na.length ? 'n/a (model has no price): ' + na.join(', ') : '';
  empty('wSkPlugin', pl.length);
  if (pl.length) {
    // plugins past the 7th by cost fold into "Other" (palette has 8 slots)
    const shown = pl.slice(0, SLOTS - 1), rest = pl.slice(SLOTS - 1);
    const rows = shown.map((p, i) => ({name: p.plugin, cost: p.cost, priced: p.priced_cost, color: css('--s' + (i + 1))}));
    if (rest.length) rows.push({name: OTHER, cost: rest.some(p => p.cost == null) ? null : rest.reduce((a, p) => a + p.priced_cost, 0),
      priced: rest.reduce((a, p) => a + p.priced_cost, 0), color: css('--other')});
    draw('cSkPlugin', {type: 'doughnut',
      data: {labels: rows.map(r => r.name), datasets: [{data: rows.map(r => r.priced), backgroundColor: rows.map(r => r.color), borderColor: c.surface, borderWidth: 2}]},
      options: {responsive: true, maintainAspectRatio: false, cutout: '62%',
        plugins: {legend: {...legendCfg(c), labels: {...legendCfg(c).labels,
          generateLabels: ch => Chart.overrides.doughnut.plugins.legend.labels.generateLabels(ch).map((l, i) => ({...l, fontColor: c.ink,
            text: rows[i].name + ' ' + costText(rows[i].cost, rows[i].priced)}))}},
          tooltip: {callbacks: {label: it => rows[it.dataIndex].name + ': ' + costText(rows[it.dataIndex].cost, rows[it.dataIndex].priced)}}}},
    });
  } else undraw('cSkPlugin');

  // counts per skill, one dataset per vendor (colour follows the vendor)
  const tot = {};
  d.invocations.forEach(r => tot[r.skill] = (tot[r.skill] || 0) + r.count);
  const names = Object.keys(tot).sort((a, b) => tot[b] - tot[a] || a.localeCompare(b)).slice(0, 15);
  empty('wSkCount', names.length);
  if (names.length) {
    const vendors = [...new Set(d.invocations.map(r => r.vendor))].sort();
    const cell = (v, n) => d.invocations.filter(r => r.vendor === v && r.skill === n);
    hbar('cSkCount', 'wSkCount', names, vendors.map(v => ({label: vendorName(v), data: names.map(n => cell(v, n).reduce((a, r) => a + r.count, 0)),
      backgroundColor: vendorColor(v), borderRadius: 2, maxBarThickness: 18})),
      {x: {stacked: true, ticks: {precision: 0}}, y: {stacked: true}, legend: legendCfg(c), tooltip: {
        label: it => { const v = vendors[it.datasetIndex], rs = cell(v, names[it.dataIndex]), n = rs.reduce((a, r) => a + r.count, 0), s = rs.reduce((a, r) => a + r.sessions, 0);
          return n ? vendorName(v) + ': ' + plural(n, v === 'codex' ? 'read' : 'call') + ' · ' + plural(s, 'session') : ''; }}});
  } else undraw('cSkCount');

  const tools = d.tools; // already top 20 by (vendor, tool); label with vendor when the same tool exists in several
  const dup = new Set(tools.map(t => t.tool).filter((t, i, a) => a.indexOf(t) !== i));
  const tl = t => dup.has(t.tool) ? t.tool + ' (' + vendorName(t.vendor) + ')' : t.tool;
  empty('wSkTools', tools.length);
  if (tools.length) hbar('cSkTools', 'wSkTools', tools.map(tl),
    [{label: 'Calls', data: tools.map(t => t.count), backgroundColor: tools.map(t => vendorColor(t.vendor)), borderRadius: 4, borderSkipped: 'start', maxBarThickness: 18}],
    {x: {ticks: {precision: 0}, grace: '12%'}, labels: v => num(v), tooltip: {label: it => vendorName(tools[it.dataIndex].vendor) + ': ' + plural(it.parsed.x, 'call')}});
  else undraw('cSkTools');
}

// ---------- active sessions + sources ----------
const agoShort = ts => { const d = Math.max(0, Math.round(nowSec() - ts)); return d < 60 ? d + 's ago' : dur(d) + ' ago'; };
const th = (h, l) => `<th${l ? ' class="l"' : ''}>${esc(h)}</th>`;
const td = (v, l) => `<td${l ? ' class="l"' : ''}>${esc(v)}</td>`;
// "Working", or "Idle · 12 min" in muted text; a session without a visible process is
// listed because its log was just written, so it counts as working
function stateCell(s, activeMinutes) {
  if (s.state === 'idle') {
    const since = s.state_since ? ' · ' + dur(nowSec() - s.state_since) : '';
    return `<td class="l" style="color:var(--mute)" title="Open, waiting for input">Idle${esc(since)}</td>`;
  }
  const why = s.pid ? 'Open and producing output' : `Log written in the last ${activeMinutes} minutes (no open process seen for this agent)`;
  return `<td class="l" style="color:var(--ok)" title="${esc(why)}">Working</td>`;
}
async function renderActive() {
  const d = await getJson('/api/active?' + apiQuery());
  const ss = d.sessions;
  const working = ss.filter(s => s.state === 'working').length;
  $('actCount').textContent = `${num(ss.length)} open · ${num(working)} working · refreshes every 10 s`;
  $('tActive').innerHTML = '<thead><tr>' + [['Vendor', 1], ['Project', 1], ['Model', 1], ['State', 1], ['Updated', 0], ['Cost so far', 0], ['Cost, last hour', 0]]
    .map(([h, l]) => th(h, l)).join('') + '</tr></thead><tbody>' +
    (ss.length ? ss.map(s => `<tr title="${esc(s.title || s.session_id)}">` + td(vendorName(s.vendor), 1) +
      `<td class="l"><span title="${esc(s.project)}">${esc(projName(s.project))}</span></td>` + td(s.model || '—', 1) +
      stateCell(s, d.active_minutes) + td(agoShort(s.last_write)) + td(costText(s.cost_so_far, s.priced_so_far)) + td(costText(s.cost_last_hour, s.priced_last_hour)) + '</tr>').join('')
      : `<tr><td colspan="7" class="empty">No open sessions, and none written in the last ${esc(d.active_minutes)} minutes</td></tr>`) + '</tbody>';
  const when = ts => ts ? fmtTs(ts) + ' · ' + agoShort(ts) : '—';
  $('tSources').innerHTML = '<thead><tr>' + [['Vendor', 1], ['Found', 1], ['Total records', 0], ['New records (last run)', 0], ['Latest limit reading', 0], ['Last run', 0], ['Error / details', 1]]
    .map(([h, l]) => th(h, l)).join('') + '</tr></thead><tbody>' +
    (d.sources.length ? d.sources.map(r => '<tr>' + td(vendorName(r.vendor), 1) + td(r.found ? 'yes' : 'no', 1) + td(num(r.total_records)) +
      td(num(r.records)) + td(when(r.last_limit_at)) + td(when(r.last_run)) +
      `<td class="l" style="white-space:normal">${r.error ? `<span style="color:var(--bad)">${esc(r.error)}</span>` : esc(r.detail || '—')}</td></tr>`).join('')
      : '<tr><td colspan="7" class="empty">No runs yet.</td></tr>') + '</tbody>';
}

// ---------- filters ----------
function fillSelect(sel, values, current, label) {
  const first = sel.options[0];
  sel.replaceChildren(first);
  const all = current && !values.includes(current) ? values.concat([current]) : values;
  for (const v of all) { const o = new Option(label ? label(v) : v, v); o.title = v; sel.add(o); }
  sel.value = current;
}
async function loadMeta() {
  const m = await getJson('/api/meta');
  const st = urlState();
  fillSelect($('vendor'), m.vendors, st.vendor);
  modelVendors = m.model_vendors || {};
  fillSelect($('model'), modelsFor(m.models, st.vendor), st.model);
  buildProjLabels(m.projects.concat(st.project ? [st.project] : []));
  fillSelect($('project'), m.projects, st.project, projName);
  if (!$('project').disabled) $('project').title = st.project || '';
}

// ---------- router + refresh ----------
const RENDER = {overview: renderOverview, limits: () => renderLimits(), cost: renderCost, sessions: renderSessions, skills: renderSkills, active: renderActive};
let busy = false, dirty = false;
function showTab() {
  const tab = currentTab();
  document.querySelectorAll('[data-pane]').forEach(p => p.hidden = p.dataset.pane !== tab);
  document.querySelectorAll('#tabs a').forEach(a => a.classList.toggle('on', a.dataset.tab === tab));
  if (!RENDER[tab]) $('warn').hidden = true;
  const used = TAB_FILTERS[tab] || FILTERS;
  for (const k of FILTERS) {
    const off = !used.includes(k);
    $(k).disabled = off;
    $(k).title = off ? `The ${FILTER_NAME[k]} filter does not apply to this tab` : (k === 'project' ? urlState().project : '');
  }
  return tab;
}
// The tab switch is immediate; data loads are serialised. A request made while
// one is in flight is not dropped: it re-runs once the current one finishes.
async function refresh() {
  showTab();
  if (busy) { dirty = true; return; }
  busy = true;
  const tab = currentTab();
  $('status').textContent = 'Loading…';
  try {
    await loadMeta();
    if (RENDER[tab]) await RENDER[tab]();
    $('status').textContent = 'Updated at ' + fmtClock();
  } catch (e) {
    $('status').textContent = 'Failed to load data: ' + e.message;
  } finally {
    busy = false;
    if (dirty) { dirty = false; refresh(); }
  }
}

// ---------- theme: Auto (follows the OS) -> Light -> Dark ----------
const THEME_KEY = 'tokenscope-theme';
const THEMES = ['auto', 'light', 'dark'];
// inline SVG icons: unicode symbols like ◐ are missing from many system fonts
const THEME_ICON = {
  auto: '<circle cx="8" cy="8" r="6.25" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M8 1.75a6.25 6.25 0 0 1 0 12.5z" fill="currentColor"/>',
  light: '<circle cx="8" cy="8" r="3" fill="currentColor"/><path d="M8 1v2M8 13v2M1 8h2M13 8h2M3.05 3.05l1.4 1.4M11.55 11.55l1.4 1.4M3.05 12.95l1.4-1.4M11.55 4.45l1.4-1.4" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>',
  dark: '<path d="M13.5 10.1A6 6 0 0 1 5.9 2.5a6 6 0 1 0 7.6 7.6z" fill="currentColor"/>',
};
const THEME_NAME = {auto: 'Auto', light: 'Light', dark: 'Dark'};
function storedTheme() {
  try { const t = localStorage.getItem(THEME_KEY); return THEMES.includes(t) ? t : 'auto'; } catch (e) { return 'auto'; }
}
let theme = 'auto';
function applyTheme(t) {
  theme = t;
  if (t === 'auto') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
  const b = $('theme');
  b.innerHTML = `<svg viewBox="0 0 16 16" aria-hidden="true">${THEME_ICON[t]}</svg><span>${THEME_NAME[t]}</span>`;
  b.title = 'Theme: ' + (t === 'auto' ? 'Auto (follows your system)' : THEME_NAME[t]) + '. Click to switch.';
  b.setAttribute('aria-label', b.title);
}
function cycleTheme() {
  const t = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length];
  try { localStorage.setItem(THEME_KEY, t); } catch (e) { /* private mode: the choice lasts for this page only */ }
  applyTheme(t);
  refresh(); // charts read the colour tokens when drawn
}

function init() {
  applyTheme(storedTheme());
  $('theme').addEventListener('click', cycleTheme);
  const st = urlState();
  if (![...$('days').options].some(o => o.value === st.days)) st.days = DEFAULT_DAYS;
  $('days').value = st.days;
  writeUrl(st);
  for (const k of FILTERS) $(k).addEventListener('change', () => {
    const patch = {[k]: $(k).value};
    // a model the new vendor never used would only ever give empty results
    if (k === 'vendor' && !modelsFor([urlState().model], patch.vendor).length) patch.model = '';
    setState(patch);
    refresh();
  });
  window.addEventListener('hashchange', refresh);
  $('tSess').addEventListener('click', ev => {
    const th = ev.target.closest('th');
    if (th) {
      const k = th.dataset.k;
      sort = sort.key === k ? {key: k, dir: -sort.dir} : {key: k, dir: ['project', 'vendor', 'model'].includes(k) ? 1 : -1};
      sessPage = 0;
      renderSessTable();
      return;
    }
    const tr = ev.target.closest('tr[data-s]');
    if (!tr) return;
    setState({session: tr.dataset.s});
    renderSessTable();
    renderTimeline().catch(e => { $('status').textContent = 'Failed to load data: ' + e.message; });
  });
  $('sessPager').addEventListener('click', ev => {
    const b = ev.target.closest('button[data-p]');
    if (!b || b.disabled) return;
    sessPage += b.dataset.p === 'next' ? 1 : -1;
    renderSessTable();
  });
  onCardActivate($('limCards'), selectLimit);
  // overview mini cards open the limits tab with that card's chart
  onCardActivate($('ovLimits'), k => { setState({lim: k}); location.hash = '#limits'; });
  $('limRange').addEventListener('click', ev => {
    const b = ev.target.closest('button[data-h]');
    if (!b) return;
    setState({lr: b.dataset.h === '168' ? '168' : ''});
    renderLimitChart().catch(e => { $('status').textContent = 'Failed to load data: ' + e.message; });
  });
  $('claudeRefresh').addEventListener('click', () => refreshTool('claude'));
  $('codexRefresh').addEventListener('click', () => refreshTool('codex'));
  $('agyRefresh').addEventListener('click', () => refreshTool('agy'));
  // collapsible cards, remembered per browser
  let collapsed = new Set();
  try { collapsed = new Set(JSON.parse(localStorage.getItem('au-collapsed') || '[]')); } catch (e) { /* storage unavailable */ }
  document.querySelectorAll('section[data-sec]').forEach(sec => {
    if (collapsed.has(sec.dataset.sec)) sec.classList.add('collapsed');
    sec.querySelector('h2').addEventListener('click', () => {
      sec.classList.toggle('collapsed');
      sec.classList.contains('collapsed') ? collapsed.add(sec.dataset.sec) : collapsed.delete(sec.dataset.sec);
      try { localStorage.setItem('au-collapsed', JSON.stringify([...collapsed])); } catch (e) { /* ignore */ }
    });
  });
  // dark/light switch: redraw charts with the other theme's colours
  // an OS light/dark switch only matters in Auto mode
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { if (theme === 'auto') refresh(); });
  refresh();
  // global 30 s refresh; the Active tab has its own 10 s one (only while it is open and the page visible)
  // timers only while the page is visible; coming back to the tab refreshes at once
  setInterval(() => { if (!document.hidden && currentTab() !== 'active') refresh(); }, 30000);
  setInterval(() => { if (!document.hidden && currentTab() === 'active') refresh(); }, 10000);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
}
init();
