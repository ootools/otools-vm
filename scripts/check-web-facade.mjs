/**
 * 验证「纯浏览器 / 远程访问」形态下，插件拿到的 window.otools facade
 * 是否具备宿主文件系统浏览能力（browseHostDir / listHostDir / readHostFile）。
 *
 * 做法：起一个最小静态站，页面里直接 import 真实的
 * tauri-remote-service/client/otools-web/index.js 的 installOtoolsWebRuntime，
 * 然后在页面上下文里探测 facade 的能力面，以及 pickFromHostTree 能否取到数据。
 *
 * 用法：node scripts/check-web-facade.mjs
 */
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN_ROOT = path.resolve(__dirname, '..');
const SDK_PARENT = path.resolve(PLUGIN_ROOT, '..', 'otools');
const REMOTE_CLIENT = path.join(SDK_PARENT, 'vendor', 'tauri-remote-service', 'client');
const SDK_SRC = path.join(SDK_PARENT, 'vendor', 'otools-plugin-sdk', 'src');

const MIME = {
  '.js': 'application/javascript; charset=utf-8',
  '.mjs': 'application/javascript; charset=utf-8',
  '.ts': 'application/javascript; charset=utf-8',
  '.html': 'text/html; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
};

function resolveFile(urlPath) {
  // /remote/*  -> remote client
  // /sdk/*     -> plugin sdk src
  // /          -> 内联 html
  if (urlPath.startsWith('/remote/')) {
    return path.join(REMOTE_CLIENT, urlPath.slice('/remote/'.length));
  }
  if (urlPath.startsWith('/sdk/')) {
    return path.join(SDK_SRC, urlPath.slice('/sdk/'.length));
  }
  return null;
}

const PAGE = `<!doctype html>
<html><head><meta charset="utf-8"><title>facade probe</title></head>
<body><div id="root"></div>
<script type="module">
import { installOtoolsWebRuntime } from '/remote/otools-web/index.js';
import { createHostFsRpc } from '/remote/otools-web/rpc.js';
// 模拟远程页面：有 token/pluginUuid，但没有 Tauri
const facade = installOtoolsWebRuntime({
  pluginUuid: 'otools-vm',
  token: 'dummy',
  appName: 'OTools',
  appVersion: '0.0.0',
  platform: 'windows',
  isDev: true,
  baseUrl: location.origin,
  wsUrl: 'ws://127.0.0.1:1/ws/otools/native',
  // 用真实 rpc.js 生成 invoke，但底层 WS 换成记录器，避免真连
  hostFsInvoke: (() => {
    const calls = [];
    const rec = (command, args) => {
      calls.push({ command, args });
      return Promise.resolve({ currentPath: 'C:/probe', parentPath: null, fileName: null, roots: [], entries: [] });
    };
    rec.__calls = calls;
    window.__sentCommands = calls;
    return rec;
  })(),
});
// 同时验证 rpc.js 模块本身可加载且导出正确
window.__rpcFactory = typeof createHostFsRpc;
window.__probe = {
  hasFacade: !!facade,
  isTauri: !!(window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke),
  methods: Object.keys(facade).sort(),
  hasBrowseHostDir: typeof facade.browseHostDir === 'function',
  hasListHostDir: typeof facade.listHostDir === 'function',
  hasReadHostFile: typeof facade.readHostFile === 'function',
  hasWriteHostFile: typeof facade.writeHostFile === 'function',
  hasHomeHostDir: typeof facade.homeHostDir === 'function',
  hasJoinHostPath: typeof facade.joinHostPath === 'function',
  hasPickFromHostTree: typeof window.__otoolsHostTree === 'function',
  runtime: window.__OToolsEnv && window.__OToolsEnv.runtime,
  runtimeHostFs: typeof facade.runtime?.hostFs?.browseHostDir === 'function',
  // 命令名路由：断言 browseHostDir 打的是 remote_service_browse_dialog
  commandSent: (() => {
    const calls = [];
    try {
      facade.browseHostDir('C:/probe');
      facade.listHostDir('C:/probe');
      facade.readHostFile('C:/probe/a.txt');
    } catch (_) {}
    return window.__sentCommands || calls;
  })(),
  rpcFactoryType: window.__rpcFactory,
};
</script></body></html>`;

const server = http.createServer((req, res) => {
  const urlPath = (req.url || '/').split('?')[0];
  if (urlPath === '/' || urlPath === '/index.html') {
    res.writeHead(200, { 'Content-Type': MIME['.html'] });
    res.end(PAGE);
    return;
  }
  const file = resolveFile(urlPath);
  if (file && fs.existsSync(file) && fs.statSync(file).isFile()) {
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
    res.end(fs.readFileSync(file));
    return;
  }
  res.writeHead(404);
  res.end('not found');
});

const results = [];
const check = (name, pass, detail = '') => {
  results.push({ name, pass, detail });
  console.log(`${pass ? 'PASS' : 'FAIL'}  ${name}${detail ? '  → ' + detail : ''}`);
};

// playwright-core 装在 WorkBuddy 的托管 node 工作区里，这里用 require 解析
const { createRequire } = await import('node:module');
const requireFromWorkspace = createRequire(
  'C:/Users/Admin/.workbuddy/binaries/node/workspace/noop.js',
);
const { chromium } = requireFromWorkspace('playwright-core');

const port = await new Promise((resolve) => {
  server.listen(0, '127.0.0.1', () => resolve(server.address().port));
});

let browser;
try {
  browser = await chromium.launch();
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: 'networkidle' });
  await page.waitForFunction(() => !!window.__probe, null, { timeout: 5000 });
  const probe = await page.evaluate(() => window.__probe);

  console.log('\n=== 探针结果 ===');
  console.log(JSON.stringify(probe, null, 2));
  if (errors.length) console.log('页面错误:', errors);
  console.log('');

  check('web runtime 已安装 facade', probe.hasFacade === true);
  check('环境判定为非 Tauri（纯浏览器）', probe.isTauri === false);
  check('__OToolsEnv.runtime === "web"', probe.runtime === 'web', String(probe.runtime));
  check('facade 有 browseHostDir', probe.hasBrowseHostDir, probe.hasBrowseHostDir ? '' : '缺失 → DOM 树无法取目录数据');
  check('facade 有 listHostDir', probe.hasListHostDir, probe.hasListHostDir ? '' : '缺失');
  check('facade 有 readHostFile', probe.hasReadHostFile, probe.hasReadHostFile ? '' : '缺失');
  check('facade 有 writeHostFile', probe.hasWriteHostFile, probe.hasWriteHostFile ? '' : '缺失');
  check('facade 有 homeHostDir', probe.hasHomeHostDir, probe.hasHomeHostDir ? '' : '缺失');
  check('facade 有 joinHostPath', probe.hasJoinHostPath, probe.hasJoinHostPath ? '' : '缺失');
  check('runtime.hostFs 也挂了 browseHostDir', probe.runtimeHostFs);
  check('rpc.js 可加载并导出 createHostFsRpc', probe.rpcFactoryType === 'function', String(probe.rpcFactoryType));

  const cmds = (probe.commandSent || []).map((c) => c.command);
  console.log('\n实际发出的命令:', cmds.length ? cmds.join(', ') : '(无)');
  check(
    'browseHostDir → remote_service_browse_dialog',
    cmds.includes('remote_service_browse_dialog'),
  );
  check('listHostDir → remote_service_list_dir', cmds.includes('remote_service_list_dir'));
  check('readHostFile → remote_service_read_file', cmds.includes('remote_service_read_file'));

  const browseCall = (probe.commandSent || []).find((c) => c.command === 'remote_service_browse_dialog');
  check(
    'browseHostDir 路径包在 request.path 里',
    browseCall?.args?.request?.path === 'C:/probe',
    JSON.stringify(browseCall?.args),
  );

  console.log('\nfacade 方法列表:');
  console.log('  ' + probe.methods.join(', '));
} finally {
  if (browser) await browser.close();
  server.close();
}

const failed = results.filter((r) => !r.pass);
console.log('\n=== 汇总 ===');
console.log(`${results.length - failed.length}/${results.length} 通过`);
if (failed.length) {
  console.log('未通过：');
  for (const f of failed) console.log(`  - ${f.name}${f.detail ? ' :: ' + f.detail : ''}`);
  process.exit(1);
}
