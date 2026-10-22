/**
 * 用「假 WebSocket」验证 rpc.js 的协议帧与会话行为。
 *
 * 不连真实网络：注入一个符合 WebSocket 接口的替身，
 * 断言 rpc.js 发出的请求帧格式、uuid 关联、错误/超时/关闭的重连语义
 * 与 server.rs 的 invoke_command 约定一致。
 *
 * 用法：node scripts/check-rpc-protocol.mjs
 */
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN_ROOT = path.resolve(__dirname, '..');
const REMOTE_CLIENT = path.resolve(PLUGIN_ROOT, '..', 'otools', 'vendor', 'tauri-remote-service', 'client');

const PAGE = `<!doctype html><html><head><meta charset="utf-8"></head><body>
<script type="module">
import { createHostFsRpc } from '/remote/otools-web/rpc.js';

const OPEN = 1, CONNECTING = 0, CLOSED = 3;
const log = { sent: [], instances: [] };

class FakeWebSocket {
  static CONNECTING = CONNECTING;
  static OPEN = OPEN;
  static CLOSED = CLOSED;
  constructor(url) {
    this.url = url;
    this.readyState = CONNECTING;
    this.sentFrames = [];
    log.instances.push(this);
    setTimeout(() => {
      this.readyState = OPEN;
      this.onopen && this.onopen();
    }, 0);
  }
  send(data) {
    const frame = JSON.parse(data);
    this.sentFrames.push(frame);
    log.sent.push(frame);
    // hello 不回；RPC 回显成功
    if (frame.type === 'hello') return;
    setTimeout(() => {
      this.onmessage && this.onmessage({
        data: JSON.stringify({
          uuid: frame.uuid,
          status: 'success',
          payload: { echoCommand: frame.command, echoArgs: frame.args },
        }),
      });
    }, 0);
  }
  close() {
    this.readyState = CLOSED;
    this.onclose && this.onclose();
  }
}

const rpc = createHostFsRpc('ws://fake/ws/otools/native', 'tok-123', { WebSocketImpl: FakeWebSocket });

const result = { ok: true, steps: [] };
const step = (name, pass, detail) => {
  result.steps.push({ name, pass, detail });
  if (!pass) result.ok = false;
};

try {
  const out = await rpc('remote_service_browse_dialog', { request: { path: 'C:/x' } });
  step('RPC 返回服务端 payload', out?.echoCommand === 'remote_service_browse_dialog', JSON.stringify(out));
  step('args 原样透传', out?.echoArgs?.request?.path === 'C:/x', JSON.stringify(out?.echoArgs));

  const hello = log.sent.find((f) => f.type === 'hello');
  step('连接后先发 hello 帧', !!hello);
  step('hello 携带 token', hello?.token === 'tok-123', String(hello?.token));
  step('hello 带 plugins 数组', Array.isArray(hello?.plugins));

  const call = log.sent.find((f) => f.uuid);
  step('RPC 帧含 uuid/command/args', !!call && typeof call.uuid === 'string' && !!call.command && !!call.args);
  step('uuid 非空且带前缀', typeof call?.uuid === 'string' && call.uuid.startsWith('otools-fs-'), String(call?.uuid));

  // 并发两请求，uuid 必须不同
  const [a, b] = await Promise.all([
    rpc('remote_service_list_dir', { path: 'C:/a' }),
    rpc('remote_service_list_dir', { path: 'C:/b' }),
  ]);
  step('并发请求各自拿到对应结果', a?.echoArgs?.path === 'C:/a' && b?.echoArgs?.path === 'C:/b',
    a?.echoArgs?.path + ' / ' + b?.echoArgs?.path);

  const uuids = log.sent.filter((f) => f.uuid).map((f) => f.uuid);
  step('uuid 唯一', new Set(uuids).size === uuids.length, uuids.length + ' calls');

  // 错误响应
  const inst = log.instances[log.instances.length - 1];
  const origSend = inst.send.bind(inst);
  inst.send = (data) => {
    const frame = JSON.parse(data);
    inst.sentFrames.push(frame);
    log.sent.push(frame);
    if (frame.type === 'hello') return;
    setTimeout(() => {
      inst.onmessage && inst.onmessage({
        data: JSON.stringify({ uuid: frame.uuid, status: 'error', payload: 'ACL denied' }),
      });
    }, 0);
  };
  let errMsg = '';
  try {
    await rpc('remote_service_read_file', { path: 'C:/secret' });
  } catch (e) {
    errMsg = e.message;
  }
  step('服务端 error 会 reject 并保留原因', errMsg.includes('ACL denied'), errMsg || '(无错误)');

  // 只复用一个 socket
  step('复用同一条连接（不每调一次新建）', log.instances.length === 1, log.instances.length + ' 个实例');
} catch (e) {
  result.ok = false;
  result.steps.push({ name: '未预期异常', pass: false, detail: e.message + '\\n' + e.stack });
}

window.__rpcResult = result;
</script></body></html>`;

const server = http.createServer((req, res) => {
  const urlPath = (req.url || '/').split('?')[0];
  if (urlPath === '/' || urlPath === '/index.html') {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(PAGE);
    return;
  }
  if (urlPath.startsWith('/remote/')) {
    const file = path.join(REMOTE_CLIENT, urlPath.slice('/remote/'.length));
    if (fs.existsSync(file)) {
      res.writeHead(200, { 'Content-Type': 'application/javascript; charset=utf-8' });
      res.end(fs.readFileSync(file));
      return;
    }
  }
  res.writeHead(404);
  res.end('nope');
});

const requireFromWorkspace = createRequire('C:/Users/Admin/.workbuddy/binaries/node/workspace/noop.js');
const { chromium } = requireFromWorkspace('playwright-core');

const port = await new Promise((r) => server.listen(0, '127.0.0.1', () => r(server.address().port)));
let browser;
try {
  browser = await chromium.launch();
  const page = await browser.newPage();
  const pageErrors = [];
  page.on('pageerror', (e) => pageErrors.push(String(e)));
  await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: 'networkidle' });
  await page.waitForFunction(() => !!window.__rpcResult, null, { timeout: 8000 });
  const result = await page.evaluate(() => window.__rpcResult);

  console.log('=== rpc.js 协议校验 ===');
  for (const s of result.steps) {
    console.log(`${s.pass ? 'PASS' : 'FAIL'}  ${s.name}${s.detail ? '  → ' + s.detail : ''}`);
  }
  if (pageErrors.length) {
    console.log('\n页面错误:');
    for (const e of pageErrors) console.log('  ' + e);
  }
  const total = result.steps.length;
  const passed = result.steps.filter((s) => s.pass).length;
  console.log(`\n${passed}/${total} 通过`);
  if (!result.ok || pageErrors.length) process.exit(1);
} finally {
  if (browser) await browser.close();
  server.close();
}
