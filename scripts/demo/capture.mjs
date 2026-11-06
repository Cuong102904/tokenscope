// Capture README screenshots from a running demo dashboard through Chrome's DevTools
// protocol. Usage: node capture.mjs <dashboard url> <devtools port> <out dir>
import fs from 'node:fs';

const [base, port, out] = process.argv.slice(2);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const tab = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, {method: 'PUT'})).json();
const ws = new WebSocket(tab.webSocketDebuggerUrl);
await new Promise(r => ws.onopen = r);
let id = 0;
const pending = new Map(), errors = [];
ws.onmessage = m => {
  const d = JSON.parse(m.data);
  if (d.method === 'Runtime.exceptionThrown') errors.push(d.params.exceptionDetails.exception?.description);
  if (pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
};
const send = (method, params = {}) => new Promise(r => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({id: i, method, params})); });
const ev = async e => (await send('Runtime.evaluate', {expression: e, awaitPromise: true, returnByValue: true})).result?.result?.value;

await send('Page.enable');
await send('Runtime.enable');
await send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 900, deviceScaleFactor: 1, mobile: false});

// [file, url query and tab, colour scheme, setup to run once the tab has loaded]
const SHOTS = [
  ['overview-light', '?days=30#overview', 'light'],
  ['limits-dark', '?days=30#limits', 'dark', `document.querySelector('#limCards td.lc[data-k^="claude|"]').click()`],
  ['cost-light', '?days=30#cost', 'light'],
];
for (const [name, path, scheme, setup] of SHOTS) {
  await send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
  await send('Page.navigate', {url: base + '/' + path});
  await sleep(2500);
  if (setup) { await ev(setup); await sleep(1500); }
  const height = Math.min(await ev(`document.documentElement.scrollHeight`), 1700); // keep long pages readable
  const shot = await send('Page.captureScreenshot', {format: 'png', captureBeyondViewport: true, clip: {x: 0, y: 0, width: 1280, height, scale: 1}});
  fs.writeFileSync(`${out}/${name}.png`, Buffer.from(shot.result.data, 'base64'));
  console.log(`${name}.png ${height}px`);
}
await fetch(`http://127.0.0.1:${port}/json/close/${tab.id}`);
ws.close();
if (errors.length) { console.error('page errors:', errors); process.exit(1); }
