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
// same rule as Totals::display: '$X' fully priced, '≥ $X' partly priced, 'n/a' nothing priced
const totalText = t => !t.unpriced_models.length ? fmtMoney(t.cost)
  : t.cost > 0 ? '≥ ' + fmtMoney(t.cost) : 'n/a';
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
  // dead accounts (expired and stale) only in the Hạn mức tab; the no-data placeholder stays
  const shown = d.limits_mini.filter(c => c.no_data || !(c.expired && c.stale));
  const hidden = d.limits_mini.length - shown.length;
  renderLimitCards($('ovLimits'), shown, true);
  if (hidden) $('ovLimits').insertAdjacentHTML('beforeend', `<div class="note" style="margin:6px 0 0"><a href="#limits">+${esc(hidden)} thẻ cũ — xem tab Hạn mức</a></div>`);
}

// ---------- limits ----------
const VENDOR_NAME = {claude: 'Claude', codex: 'Codex', opencode: 'OpenCode', agy: 'Antigravity (agy)'};
const vendorName = v => VENDOR_NAME[v] || v;
const SCOPE_BASE = {five_hour: '5 giờ', weekly: 'Tuần', seven_day: 'Tuần', monthly: 'Tháng'};
// "five_hour" -> "5 giờ", "weekly:Gemini" -> "Tuần · Gemini", "x:window_90m" -> "90 phút · x"
function scopeLabel(scope) {
  const ps = String(scope).split(':');
  const i = ps.findIndex(p => SCOPE_BASE[p] || /^window_\d+m$/.test(p));
  if (i < 0) return String(scope);
  const b = SCOPE_BASE[ps[i]] || ps[i].slice(7, -1) + ' phút';
  const rest = ps.filter((_, j) => j !== i).join(':');
  return rest ? b + ' · ' + rest : b;
}
const fmtPct = v => Number(v).toLocaleString('en-US', {maximumFractionDigits: 1}) + '%';
function dur(sec) {
  sec = Math.max(0, Math.round(sec));
  if (sec < 60) return 'dưới 1 phút';
  const d = Math.floor(sec / 86400), h = Math.floor(sec % 86400 / 3600), m = Math.floor(sec % 3600 / 60);
  if (d) return d + ' ngày' + (h ? ' ' + h + ' giờ' : '');
  if (h) return h + ' giờ' + (m ? ' ' + m + ' phút' : '');
  return m + ' phút';
}
const nowSec = () => Date.now() / 1000;
const until = ts => 'còn ' + dur(ts - nowSec());
const ago = ts => nowSec() - ts < 60 ? 'vừa xong' : dur(nowSec() - ts) + ' trước';
// bar level: green < 70, amber < 90, red >= 90 (the number is always shown too)
const level = p => p >= 90 ? 'hi' : p >= 70 ? 'mid' : 'ok';
const limKey = c => [c.vendor, c.account, c.scope].join('|');
function splitLimKey(k) { const [vendor, account, ...rest] = k.split('|'); return {vendor, account: account || '', scope: rest.join('|')}; }

function limitCard(c, mini) {
  if (c.no_data) return `<div class="lcard nodata"><div class="lhead"><span class="sc">${esc(vendorName(c.vendor))}</span></div><div class="lmeta">${esc(c.eta_note)}</div></div>`;
  const sel = !mini && limKey(c) === urlState().lim;
  const head = `<div class="lhead"><span class="sc">${esc(scopeLabel(c.scope))}</span>` +
    `<span class="ac" title="${esc(c.account_label)}">${esc(c.account_label)}</span>` +
    (c.stale ? '<span class="badge" title="Số đo đã cũ, có thể không còn đúng">CŨ</span>' : '') + '</div>';
  let body;
  if (c.expired) {
    // the last number belongs to the previous window: never show it as current
    body = `<div class="lval reset">Đã reset, chờ số mới</div><div class="lmeta">Reset lúc ${esc(fmtTs(c.resets_at))} · ${esc(ago(c.resets_at))}</div>`;
  } else {
    const p = Math.max(0, Math.min(100, Number(c.used_percent))), lv = level(p);
    body = `<div class="lval ${lv}">${esc(fmtPct(c.used_percent))} <span class="lmeta">đã dùng</span></div>` +
      `<div class="bar" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${esc(p)}" aria-label="${esc(fmtPct(p) + ' đã dùng')}"><i class="${lv}" style="width:${esc(p)}%"></i></div>` +
      (c.resets_at != null ? `<div class="lmeta">Reset ${esc(fmtTs(c.resets_at))} · ${esc(until(c.resets_at))}</div>` : '');
  }
  const measured = mini
    ? `<div class="lmeta">đo ${esc(ago(c.measured_at))}</div>`
    : `<div class="lmeta">Đo lúc ${esc(fmtTs(c.measured_at))} (${esc(c.source)}) · ${esc(ago(c.measured_at))}${c.plan_type ? ' · gói ' + esc(c.plan_type) : ''}</div>`;
  const eta = mini || c.expired ? '' : `<div class="lmeta">${c.eta != null
    ? 'Dự kiến chạm 100%: ' + esc(fmtTs(c.eta)) + ' · ' + esc(until(c.eta))
    : 'Không có ETA: ' + esc(c.eta_note)}</div>`;
  const warn = c.warning ? `<div class="lwarn">Cảnh báo: ${esc(c.warning)}</div>` : '';
  return `<div class="lcard${sel ? ' sel' : ''}" role="button" tabindex="0" data-k="${esc(limKey(c))}">${head}${body}${measured}${eta}${warn}</div>`;
}

function renderLimitCards(el, cards, mini) {
  const groups = new Map();
  for (const c of cards) { if (!groups.has(c.vendor)) groups.set(c.vendor, []); groups.get(c.vendor).push(c); }
  el.innerHTML = groups.size ? [...groups].map(([v, cs]) =>
    `<div class="lim-group"><h3>${esc(vendorName(v))}</h3><div class="lim-grid${mini ? ' mini' : ''}">${cs.map(c => limitCard(c, mini)).join('')}</div></div>`).join('')
    : '<div class="empty">Chưa có dữ liệu hạn mức.</div>';
}

let limCards = [];
async function renderLimits(data) {
  const d = data || await getJson('/api/limits');
  limCards = d.cards;
  renderLimitCards($('limCards'), d.cards, false);
  $('tLimEvents').innerHTML = '<thead><tr>' + ['Thời điểm', 'Hãng', 'Tài khoản', 'Phạm vi', 'Reset lúc']
    .map(h => `<th class="l">${esc(h)}</th>`).join('') + '</tr></thead><tbody>' +
    (d.events.length ? d.events.map(e => `<tr><td class="l">${esc(fmtTs(e.ts))}</td><td class="l">${esc(vendorName(e.vendor))}</td>` +
      `<td class="l">${esc(e.account_label)}</td><td class="l">${esc(scopeLabel(e.scope))}</td><td class="l">${esc(e.resets_at != null ? fmtTs(e.resets_at) : '—')}</td></tr>`).join('')
      : '<tr><td colspan="5" class="empty">Không có lần chạm giới hạn nào trong 30 ngày.</td></tr>') + '</tbody>';
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

let limToken = 0, limChartSig = '', limChartKey = '';
async function renderLimitChart() {
  const my = ++limToken;
  const st = urlState();
  const hours = st.lr === '168' ? 168 : 24;
  document.querySelectorAll('#limRange button').forEach(b => b.classList.toggle('on', b.dataset.h === String(hours)));
  if (!st.lim) {
    $('limChartTitle').textContent = 'bấm một thẻ ở trên';
    if (charts.cLimit) { charts.cLimit.destroy(); delete charts.cLimit; }
    limChartSig = '';
    return;
  }
  const k = splitLimKey(st.lim);
  const card = limCards.find(c => limKey(c) === st.lim);
  const title = vendorName(k.vendor) + ' · ' + scopeLabel(k.scope) + ' · ' + (card ? card.account_label : k.account || '—');
  const h = await getJson('/api/limits/history?' + new URLSearchParams({...k, hours}));
  if (my !== limToken) return; // a later click owns the chart now
  $('limChartTitle').textContent = title + (h.points.length ? '' : ' — không có số đo trong khoảng này');
  const now = Date.now(), from = now - hours * 3600e3;
  const c = chartBase(), col = vendorColor(k.vendor);
  // the 30 s auto-refresh: same data and theme -> keep the chart as is (no flicker)
  const view = st.lim + '|' + hours;
  const sig = JSON.stringify([view, h, col, c.mute, c.grid]);
  if (sig === limChartSig && charts.cLimit) return;
  const animate = view !== limChartKey; // animate only a new selection, never a data refresh
  limChartSig = sig; limChartKey = view;
  draw('cLimit', {
    type: 'line',
    data: {datasets: [{label: '% đã dùng', data: h.points.map(([t, v]) => ({x: t * 1000, y: v})),
      borderColor: col, backgroundColor: col, borderWidth: 2, pointRadius: h.points.length < 60 ? 2 : 0, pointHoverRadius: 5, tension: 0}]},
    options: {responsive: true, maintainAspectRatio: false, ...(animate ? {} : {animation: false}), interaction: {mode: 'nearest', axis: 'x', intersect: false},
      scales: {
        x: {type: 'linear', min: from, max: now, ticks: {maxTicksLimit: 6, callback: v => fmtTs(v / 1000)}, grid: {display: false}},
        y: {min: 0, max: 100, ticks: {callback: v => v + '%'}, grid: {color: c.grid}},
      },
      plugins: {legend: {display: false}, tooltip: {callbacks: {title: items => fmtTs(items[0].parsed.x / 1000), label: it => fmtPct(it.parsed.y) + ' đã dùng'}}}},
    plugins: [resetLines(h.resets.map(r => r * 1000), c.mute)],
  });
}

function agyText(r) {
  if (!r) return '';
  switch (r.status) {
    case 'ran': return 'Đã làm mới agy lúc ' + new Date().toLocaleTimeString('vi-VN') + '.';
    case 'throttled': return 'agy vừa chạy gần đây (tối đa 1 lần / 5 phút) — thử lại lúc ' + fmtTs(r.next_allowed) + ' (' + until(r.next_allowed) + ').';
    case 'not_installed': return 'Không tìm thấy lệnh agy trong PATH.';
    default: return 'Lỗi làm mới agy: ' + (r.message || r.status);
  }
}
async function refreshAgy() {
  const b = $('agyRefresh');
  b.disabled = true;
  $('agyMsg').textContent = 'đang chạy agy /usage… (tối đa 30 giây)';
  try {
    const r = await fetch('/api/limits?refresh=agy', {cache: 'no-store', headers: {'X-Agent-Usage': '1'}});
    if (!r.ok) throw new Error('HTTP ' + r.status + ' ' + (await r.text()));
    const d = await r.json();
    $('agyMsg').textContent = agyText(d.refresh);
    if (currentTab() === 'limits') await renderLimits(d);
  } catch (e) {
    $('agyMsg').textContent = 'Lỗi làm mới agy: ' + e.message;
  } finally {
    b.disabled = false;
  }
}
function selectLimit(k) {
  setState({lim: k});
  document.querySelectorAll('#limCards .lcard').forEach(el => el.classList.toggle('sel', el.dataset.k === k));
  renderLimitChart().catch(e => { $('status').textContent = 'lỗi tải dữ liệu: ' + e.message; });
}
function onCardActivate(el, handler) {
  const go = ev => { const card = ev.target.closest('.lcard[data-k]'); if (card) handler(card.dataset.k); };
  el.addEventListener('click', go);
  el.addEventListener('keydown', ev => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); go(ev); } });
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
  const noAnim = charts.cSkCost ? {animation: false} : {}; // data refresh on an already drawn tab: no re-animation
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
      options: {indexAxis: 'y', responsive: true, maintainAspectRatio: false, ...noAnim,
        scales: {x: {beginAtZero: true, grid: {color: c.grid}, ...(extra.x || {})}, y: {grid: {display: false}, ticks: {color: c.ink}, ...(extra.y || {})}},
        plugins: {legend: extra.legend || {display: false}, tooltip: {callbacks: extra.tooltip}}},
      plugins: extra.labels ? [valueLabels(extra.labels)] : []});
  };
  // no rows: hide the chart box and show the "Không có dữ liệu" line that precedes it
  const empty = (wrap, on) => { $(wrap).hidden = !on; $(wrap).previousElementSibling.hidden = !!on; };

  const top = d.by_cost.slice(0, 15);
  empty('wSkCost', top.length);
  if (top.length) hbar('cSkCost', 'wSkCost', top.map(s => s.skill),
    [{label: 'Chi phí', data: top.map(s => s.priced_cost), backgroundColor: vendorColor('claude'), borderRadius: 4, borderSkipped: 'start', maxBarThickness: 18}],
    {x: {ticks: {callback: tickMoney}, grace: '15%'}, labels: (v, i) => costText(top[i].cost, top[i].priced_cost), tooltip: {
      label: it => { const s = top[it.dataIndex]; return costText(s.cost, s.priced_cost) + ' · ' + num(s.messages) + ' tin nhắn'; },
      afterLabel: it => top[it.dataIndex].plugin ? 'plugin: ' + top[it.dataIndex].plugin : ''}});
  else if (charts.cSkCost) { charts.cSkCost.destroy(); delete charts.cSkCost; }

  const pl = d.by_plugin.filter(p => p.priced_cost > 0);
  // plugins whose rows are all unpriced have nothing to draw: list them as n/a instead of dropping them
  const na = d.by_plugin.filter(p => p.priced_cost <= 0 && p.cost == null).map(p => p.plugin);
  $('skPluginNa').hidden = !na.length;
  $('skPluginNa').textContent = na.length ? 'n/a (model chưa có giá): ' + na.join(', ') : '';
  empty('wSkPlugin', pl.length);
  if (pl.length) {
    // plugins past the 7th by cost fold into "Khác" (palette has 8 slots)
    const shown = pl.slice(0, SLOTS - 1), rest = pl.slice(SLOTS - 1);
    const rows = shown.map((p, i) => ({name: p.plugin, cost: p.cost, priced: p.priced_cost, color: css('--s' + (i + 1))}));
    if (rest.length) rows.push({name: OTHER, cost: rest.some(p => p.cost == null) ? null : rest.reduce((a, p) => a + p.priced_cost, 0),
      priced: rest.reduce((a, p) => a + p.priced_cost, 0), color: css('--other')});
    draw('cSkPlugin', {type: 'doughnut',
      data: {labels: rows.map(r => r.name), datasets: [{data: rows.map(r => r.priced), backgroundColor: rows.map(r => r.color), borderColor: c.surface, borderWidth: 2}]},
      options: {responsive: true, maintainAspectRatio: false, cutout: '62%', ...noAnim,
        plugins: {legend: {...legendCfg(c), labels: {...legendCfg(c).labels,
          generateLabels: ch => Chart.overrides.doughnut.plugins.legend.labels.generateLabels(ch).map((l, i) => ({...l, fontColor: c.ink,
            text: rows[i].name + ' ' + costText(rows[i].cost, rows[i].priced)}))}},
          tooltip: {callbacks: {label: it => rows[it.dataIndex].name + ': ' + costText(rows[it.dataIndex].cost, rows[it.dataIndex].priced)}}}},
    });
  } else if (charts.cSkPlugin) { charts.cSkPlugin.destroy(); delete charts.cSkPlugin; }

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
          return n ? vendorName(v) + ': ' + num(n) + (v === 'codex' ? ' lần đọc' : ' lần gọi') + ' · ' + num(s) + ' session' : ''; }}});
  } else if (charts.cSkCount) { charts.cSkCount.destroy(); delete charts.cSkCount; }

  const tools = d.tools; // already top 20 by (vendor, tool); label with vendor when the same tool exists in several
  const dup = new Set(tools.map(t => t.tool).filter((t, i, a) => a.indexOf(t) !== i));
  const tl = t => dup.has(t.tool) ? t.tool + ' (' + vendorName(t.vendor) + ')' : t.tool;
  empty('wSkTools', tools.length);
  if (tools.length) hbar('cSkTools', 'wSkTools', tools.map(tl),
    [{label: 'Số lần gọi', data: tools.map(t => t.count), backgroundColor: tools.map(t => vendorColor(t.vendor)), borderRadius: 4, borderSkipped: 'start', maxBarThickness: 18}],
    {x: {ticks: {precision: 0}, grace: '12%'}, labels: v => num(v), tooltip: {label: it => vendorName(tools[it.dataIndex].vendor) + ': ' + num(it.parsed.x) + ' lần gọi'}});
  else if (charts.cSkTools) { charts.cSkTools.destroy(); delete charts.cSkTools; }
}

// ---------- active sessions + sources ----------
const agoShort = ts => { const d = Math.max(0, Math.round(nowSec() - ts)); return d < 60 ? d + ' giây trước' : dur(d) + ' trước'; };
const th = (h, l) => `<th${l ? ' class="l"' : ''}>${esc(h)}</th>`;
const td = (v, l) => `<td${l ? ' class="l"' : ''}>${esc(v)}</td>`;
async function renderActive() {
  const d = await getJson('/api/active');
  const ss = d.sessions;
  $('actCount').textContent = ss.length + ' phiên · tự làm mới 10 giây';
  $('tActive').innerHTML = '<thead><tr>' + [['Hãng', 1], ['Project', 1], ['Model', 1], ['Cập nhật', 0], ['Chi phí đến giờ', 0], ['Chi phí 1 giờ qua', 0]]
    .map(([h, l]) => th(h, l)).join('') + '</tr></thead><tbody>' +
    (ss.length ? ss.map(s => `<tr title="${esc(s.title || s.session_id)}">` + td(vendorName(s.vendor), 1) +
      `<td class="l"><span title="${esc(s.project)}">${esc(projName(s.project))}</span></td>` + td(s.model || '—', 1) +
      td(agoShort(s.last_write)) + td(costText(s.cost_so_far, s.priced_so_far)) + td(costText(s.cost_last_hour, s.priced_last_hour)) + '</tr>').join('')
      : `<tr><td colspan="6" class="empty">Không có phiên nào hoạt động trong ${esc(d.active_minutes)} phút gần đây</td></tr>`) + '</tbody>';
  $('tSources').innerHTML = '<thead><tr>' + [['Hãng', 1], ['Tìm thấy', 1], ['Số bản ghi', 0], ['Lần chạy cuối', 0], ['Lỗi / chi tiết', 1]]
    .map(([h, l]) => th(h, l)).join('') + '</tr></thead><tbody>' +
    (d.sources.length ? d.sources.map(r => '<tr>' + td(vendorName(r.vendor), 1) + td(r.found ? 'có' : 'không', 1) + td(num(r.records)) +
      td(r.last_run ? fmtTs(r.last_run) + ' · ' + agoShort(r.last_run) : '—') +
      `<td class="l" style="white-space:normal">${r.error ? `<span style="color:var(--bad)">${esc(r.error)}</span>` : esc(r.detail || '—')}</td></tr>`).join('')
      : '<tr><td colspan="5" class="empty">Chưa có lần chạy nào.</td></tr>') + '</tbody>';
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
const RENDER = {overview: renderOverview, limits: () => renderLimits(), cost: renderCost, sessions: renderSessions, skills: renderSkills, active: renderActive};
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
  onCardActivate($('limCards'), selectLimit);
  // overview mini cards open the limits tab with that card's chart
  onCardActivate($('ovLimits'), k => { setState({lim: k}); location.hash = '#limits'; });
  $('limRange').addEventListener('click', ev => {
    const b = ev.target.closest('button[data-h]');
    if (!b) return;
    setState({lr: b.dataset.h === '168' ? '168' : ''});
    renderLimitChart().catch(e => { $('status').textContent = 'lỗi tải dữ liệu: ' + e.message; });
  });
  $('agyRefresh').addEventListener('click', refreshAgy);
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
  // global 30 s refresh; the Đang chạy tab has its own 10 s one (only while it is open and the page visible)
  setInterval(() => { if (currentTab() !== 'active') refresh(); }, 30000);
  setInterval(() => { if (currentTab() === 'active' && !document.hidden) refresh(); }, 10000);
}
init();
