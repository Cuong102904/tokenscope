'use strict';
// agent-usage dashboard. Filters live in the query string, the tab in the hash.
const $ = id => document.getElementById(id);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const fmtMoney = v => '$' + Number(v).toLocaleString('en-US', {minimumFractionDigits:2, maximumFractionDigits:2});
const num = v => Number(v).toLocaleString('en-US');
const pct = v => (v * 100).toLocaleString('en-US', {maximumFractionDigits:1}) + '%';
// cost: Option<f64> + priced lower bound -> "$1.00" | "≥ $0.50" | "n/a"
const costText = (cost, priced) => cost != null ? fmtMoney(cost) : (priced > 0 ? '≥ ' + fmtMoney(priced) : 'n/a');
const parts = p => String(p || '').split('/').filter(Boolean);
const base = p => parts(p).slice(-1)[0] || String(p || '') || '(không rõ)';
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
const fmtTs = ts => new Date(ts * 1000).toLocaleString('vi-VN', {year:'2-digit', month:'2-digit', day:'2-digit', hour:'2-digit', minute:'2-digit'});
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
  return {days, vendor: p.get('vendor') || '', model: p.get('model') || '', project: p.get('project') || '', session: p.get('s') || ''};
}
function writeUrl(st) {
  const p = new URLSearchParams();
  for (const k of FILTERS) if (st[k]) p.set(k, st[k]);
  if (st.session) p.set('s', st.session);
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

// ---------- colours (fixed categorical order; colour follows the entity) ----------
const css = n => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
const SLOTS = 8;
const OTHER = 'Khác';
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
function draw(id, cfg) {
  if (charts[id]) charts[id].destroy();
  charts[id] = new Chart($(id), cfg);
}
const legendCfg = c => ({position: 'bottom', labels: {color: c.ink, boxWidth: 10, boxHeight: 10, usePointStyle: true, pointStyle: 'rectRounded'}});

// Stacked bars per day, one dataset per model; models past the 7th by total fold into "Khác".
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
              return ['Tổng: ' + fmtMoney(sum)].concat(d.unpriced.length ? ['Chưa có giá (n/a): ' + d.unpriced.join(', ')] : []);
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
const totalText = t => (t.unpriced_models.length ? '≥ ' : '') + fmtMoney(t.cost);
const totalSub = t => num(t.sessions) + ' session · ' + num(t.messages) + ' tin nhắn';

function setWarn(unpriced) {
  const w = $('warn');
  w.hidden = !unpriced.length;
  w.textContent = unpriced.length ? 'Model chưa có giá (hiện "n/a", không tính vào tổng): ' + unpriced.join(', ') : '';
}

async function getJson(path) {
  const r = await fetch(path, {cache: 'no-store'});
  if (!r.ok) throw new Error(path + ': HTTP ' + r.status + ' ' + (await r.text()));
  return r.json();
}

async function renderOverview() {
  const d = await getJson('/api/overview?' + apiQuery());
  $('ovKpis').innerHTML = [
    kpi('Hôm nay', totalText(d.totals_today), totalSub(d.totals_today)),
    kpi('7 ngày', totalText(d.totals_7d), totalSub(d.totals_7d)),
    kpi('30 ngày', totalText(d.totals_30d), totalSub(d.totals_30d)),
    kpi('Đang chạy', num(d.active_count), 'session'),
  ].join('');
  setWarn(d.totals_30d.unpriced_models);
  drawDaily('cOvDaily', d.daily);
  $('ovLimits').innerHTML = d.limits_mini.length
    ? d.limits_mini.map(l => `<div>${esc(JSON.stringify(l))}</div>`).join('')
    : '<div class="empty">Sắp có — xem tab Hạn mức sau khi tính năng này hoàn tất.</div>';
}

async function renderCost() {
  const d = await getJson('/api/cost?' + apiQuery());
  const t = d.totals, e = d.efficiency;
  $('costKpis').innerHTML = [
    kpi('Tổng chi phí', totalText(t), totalSub(t)),
    kpi('Tỉ lệ cache', pct(e.cache_hit_ratio), 'cache read / (input + cache read)'),
    kpi('Phần subagent', pct(e.subagent_cost_share), 'chi phí subagent / tổng'),
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
    data: {labels: top.map(p => projName(p.project)), datasets: [{label: 'Chi phí', data: top.map(p => p.priced_cost),
      backgroundColor: css('--s1'), borderRadius: 4, borderSkipped: 'start', maxBarThickness: 18}]},
    options: {indexAxis: 'y', responsive: true, maintainAspectRatio: false,
      scales: {x: {beginAtZero: true, ticks: {callback: tickMoney}, grid: {color: c.grid}}, y: {grid: {display: false}, ticks: {color: c.ink}}},
      plugins: {legend: {display: false}, tooltip: {callbacks: {
        title: items => top[items[0].dataIndex].project,
        label: it => { const p = top[it.dataIndex]; return costText(p.cost, p.priced_cost) + ' · ' + num(p.sessions) + ' session'; },
      }}}},
  });

  $('tModels').innerHTML = '<thead><tr>' + ['Model', 'Hãng', 'Chi phí', 'Tin nhắn', 'Input', 'Output', 'Cache read', 'Cache write']
    .map((h, i) => `<th${i === 1 ? ' class="l"' : ''}>${esc(h)}</th>`).join('') + '</tr></thead><tbody>' +
    (d.by_model.length ? d.by_model.map(m => `<tr><td>${esc(m.model)}</td><td class="l">${esc(m.vendor)}</td><td>${esc(m.cost == null ? 'n/a' : fmtMoney(m.cost))}</td>` +
      [m.messages, m.input, m.output, m.cache_read, m.cache_write].map(v => `<td>${esc(num(v))}</td>`).join('') + '</tr>').join('')
      : '<tr><td colspan="8" class="empty">Không có dữ liệu.</td></tr>') + '</tbody>';
}

const SESS_COLS = [
  {key: 'project', label: 'Project', l: true, val: s => projName(s.project), cell: s => `<span title="${esc(s.project)}">${esc(projName(s.project))}</span>`},
  {key: 'vendor', label: 'Hãng', l: true, val: s => s.vendor},
  {key: 'model', label: 'Model', l: true, val: s => s.models[0] || '', cell: s => esc(s.models.join(', '))},
  {key: 'messages', label: 'Tin nhắn', val: s => s.messages, cell: s => esc(num(s.messages))},
  {key: 'subagent', label: 'Subagent', val: s => s.subagent_messages, cell: s => esc(num(s.subagent_messages))},
  {key: 'cost', label: 'Chi phí', val: s => s.priced_cost, cell: s => esc(costText(s.cost, s.priced_cost))},
  {key: 'first', label: 'Đầu', val: s => s.first_ts, cell: s => esc(fmtTs(s.first_ts))},
  {key: 'last', label: 'Cuối', val: s => s.last_ts, cell: s => esc(fmtTs(s.last_ts))},
];
const sessKey = s => s.vendor + '/' + s.session_id;

function renderSessTable() {
  const col = SESS_COLS.find(c => c.key === sort.key) || SESS_COLS[5];
  const rows = [...sessions].sort((a, b) => {
    const x = col.val(a), y = col.val(b);
    return (typeof x === 'number' ? x - y : String(x).localeCompare(String(y))) * sort.dir;
  });
  const sel = urlState().session;
  $('sessCount').textContent = rows.length + ' session';
  $('tSess').innerHTML = '<thead><tr>' + SESS_COLS.map(c =>
    `<th data-k="${esc(c.key)}" class="${c.l ? 'l ' : ''}${c.key === sort.key ? 'sorted' : ''}">${esc(c.label)}${c.key === sort.key ? (sort.dir < 0 ? ' ▾' : ' ▴') : ''}</th>`).join('') +
    '</tr></thead><tbody>' + (rows.length ? rows.map(s =>
      `<tr data-s="${esc(sessKey(s))}"${sessKey(s) === sel ? ' class="sel"' : ''} title="${esc(s.title || s.session_id)}">` +
      SESS_COLS.map(c => `<td${c.l ? ' class="l"' : ''}>${c.cell ? c.cell(s) : esc(c.val(s))}</td>`).join('') + '</tr>').join('')
      : '<tr><td colspan="8" class="empty">Không có session.</td></tr>') + '</tbody>';
}

let tlToken = 0;
async function renderTimeline() {
  const my = ++tlToken;
  const sel = urlState().session;
  if (!sel) { if (charts.cTimeline) { charts.cTimeline.destroy(); delete charts.cTimeline; } return; }
  const i = sel.indexOf('/');
  const vendor = sel.slice(0, i), id = sel.slice(i + 1);
  const s = sessions.find(x => sessKey(x) === sel);
  $('tlTitle').textContent = s ? projName(s.project) + ' · ' + (s.title || id) : id;
  const t = await getJson('/api/session/' + encodeURIComponent(vendor) + '/' + encodeURIComponent(id) + '/timeline');
  if (my !== tlToken) return; // a later click (or refresh) owns the chart now
  $('tlNote').hidden = !t.unpriced_models.length;
  $('tlNote').textContent = t.unpriced_models.length ? 'Đường là cận dưới — model chưa có giá: ' + t.unpriced_models.join(', ') : '';
  const c = chartBase();
  draw('cTimeline', {
    type: 'line',
    data: {datasets: [{label: 'Chi phí tích luỹ', data: t.points.map(([ts, v]) => ({x: ts * 1000, y: v})),
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
  sessions = await getJson('/api/sessions?' + apiQuery({limit: 200}));
  renderSessTable();
  await renderTimeline();
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
  fillSelect($('model'), m.models, st.model);
  buildProjLabels(m.projects.concat(st.project ? [st.project] : []));
  fillSelect($('project'), m.projects, st.project, projName);
  $('project').title = st.project || '';
}

// ---------- router + refresh ----------
const RENDER = {overview: renderOverview, cost: renderCost, sessions: renderSessions};
let busy = false, dirty = false;
function showTab() {
  const tab = currentTab();
  document.querySelectorAll('[data-pane]').forEach(p => p.hidden = p.dataset.pane !== tab);
  document.querySelectorAll('#tabs a').forEach(a => a.classList.toggle('on', a.dataset.tab === tab));
  if (!RENDER[tab]) $('warn').hidden = true;
  return tab;
}
// The tab switch is immediate; data loads are serialised. A request made while
// one is in flight is not dropped: it re-runs once the current one finishes.
async function refresh() {
  showTab();
  if (busy) { dirty = true; return; }
  busy = true;
  const tab = currentTab();
  $('status').textContent = 'đang tải…';
  try {
    await loadMeta();
    if (RENDER[tab]) await RENDER[tab]();
    $('status').textContent = 'cập nhật lúc ' + new Date().toLocaleTimeString('vi-VN');
  } catch (e) {
    $('status').textContent = 'lỗi tải dữ liệu: ' + e.message;
  } finally {
    busy = false;
    if (dirty) { dirty = false; refresh(); }
  }
}

function init() {
  const st = urlState();
  if (![...$('days').options].some(o => o.value === st.days)) st.days = DEFAULT_DAYS;
  $('days').value = st.days;
  writeUrl(st);
  for (const k of FILTERS) $(k).addEventListener('change', () => { setState({[k]: $(k).value}); refresh(); });
  window.addEventListener('hashchange', refresh);
  $('tSess').addEventListener('click', ev => {
    const th = ev.target.closest('th');
    if (th) {
      const k = th.dataset.k;
      sort = sort.key === k ? {key: k, dir: -sort.dir} : {key: k, dir: ['project', 'vendor', 'model'].includes(k) ? 1 : -1};
      renderSessTable();
      return;
    }
    const tr = ev.target.closest('tr[data-s]');
    if (!tr) return;
    setState({session: tr.dataset.s});
    renderSessTable();
    renderTimeline().catch(e => { $('status').textContent = 'lỗi tải dữ liệu: ' + e.message; });
  });
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
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', refresh);
  refresh();
  setInterval(refresh, 30000);
}
init();
