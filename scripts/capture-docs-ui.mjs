import { spawn } from 'node:child_process';
import { writeFile } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';

const chrome = process.env.CHROME_BIN ?? 'google-chrome';
const port = Number(process.env.CDP_PORT ?? 9223);
const origin = process.env.ELECTRODE_CAPTURE_ORIGIN ?? 'http://127.0.0.1:8790';
const outDir = new URL('../docs/assets/ui/', import.meta.url);

const shots = [
  {
    path: 'groundstation-dashboard.png',
    url: `${origin}/`,
    width: 1440,
    height: 2200
  },
  {
    path: 'groundstation-dashboard-config.png',
    url: `${origin}/`,
    width: 1440,
    height: 1800,
    prepare:
      "const panel = document.querySelector('details.config-panel'); if (panel) panel.open = true; window.scrollTo(0, 0);"
  },
  {
    path: 'groundstation-sim.png',
    url: `${origin}/?page=sim`,
    width: 1440,
    height: 1800
  },
  {
    path: 'dashboard-tall.png',
    url: `${origin}/?viewer`,
    width: 1440,
    height: 2200
  },
  {
    path: 'dashboard-overview.png',
    url: `${origin}/?viewer`,
    width: 1440,
    height: 1100
  }
];

const browser = spawn(chrome, [
  '--headless=new',
  '--disable-gpu',
  '--no-sandbox',
  `--remote-debugging-port=${port}`,
  'about:blank'
], {
  stdio: ['ignore', 'pipe', 'pipe']
});

browser.stderr.on('data', (chunk) => {
  const text = chunk.toString();
  if (!text.includes('DevTools listening')) {
    process.stderr.write(text);
  }
});

try {
  const pageTarget = await newPageTarget(port);
  const ws = new WebSocket(pageTarget.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  });

  let id = 0;
  const pending = new Map();
  ws.addEventListener('message', (event) => {
    const message = JSON.parse(event.data);
    if (!message.id) return;
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.error) {
      request.reject(new Error(JSON.stringify(message.error)));
    } else {
      request.resolve(message.result);
    }
  });

  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const messageId = ++id;
      pending.set(messageId, { resolve, reject });
      ws.send(JSON.stringify({ id: messageId, method, params }));
    });

  await send('Page.enable');
  await send('Runtime.enable');
  await send('Page.addScriptToEvaluateOnNewDocument', {
    source: "localStorage.setItem('electrode-theme', 'dark'); document.documentElement.dataset.theme = 'dark';"
  });

  for (const shot of shots) {
    await send('Emulation.setDeviceMetricsOverride', {
      width: shot.width,
      height: shot.height,
      deviceScaleFactor: 1,
      mobile: shot.width < 600
    });
    await send('Page.navigate', { url: shot.url });
    await waitForLoad(send);
    await delay(1200);
    if (shot.prepare) {
      await send('Runtime.evaluate', {
        expression: shot.prepare,
        returnByValue: true
      });
      await delay(400);
    }
    const result = await send('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: false,
      fromSurface: true
    });
    await writeFile(new URL(shot.path, outDir), Buffer.from(result.data, 'base64'));
    console.log(`wrote ${shot.path}`);
  }

  ws.close();
} finally {
  browser.kill('SIGTERM');
}

async function newPageTarget(port) {
  const url = `http://127.0.0.1:${port}/json/new?about:blank`;
  for (let attempt = 0; attempt < 50; attempt += 1) {
    try {
      const response = await fetch(url, { method: 'PUT' });
      if (response.ok) return await response.json();
    } catch {
      // Chrome is still starting.
    }
    await delay(100);
  }
  throw new Error(`timed out creating page target at ${url}`);
}

async function waitForLoad(send) {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    const result = await send('Runtime.evaluate', {
      expression: 'document.readyState',
      returnByValue: true
    });
    if (result.result?.value === 'complete') return;
    await delay(100);
  }
}
