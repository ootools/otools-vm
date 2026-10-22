/**
 * 校验 server.rs 注入到远程序页面的那段 JS 是否语法合法。
 *
 * 做法：从 server.rs 里抽出 build_otools_web_runtime_script 的 format! 模板，
 * 按 Rust format! 的规则做最小替换（{{ → {，具名占位符 → 假值），
 * 然后用 `node --check` 验证产物是合法 JS。
 *
 * 这条检查很关键：那段模板里混着 Rust 转义和 JS 模板字面量，
 * 一旦转义写错，远程页面会直接白屏且不会有任何编译期报错。
 *
 * 用法：node scripts/check-injected-script.mjs
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN_ROOT = path.resolve(__dirname, '..');
const SERVER_RS = path.resolve(
  PLUGIN_ROOT, '..', 'otools', 'vendor', 'tauri-remote-service', 'src', 'server.rs',
);

const src = fs.readFileSync(SERVER_RS, 'utf8');
const start = src.indexOf('fn build_otools_web_runtime_script');
if (start < 0) {
  console.error('找不到 build_otools_web_runtime_script');
  process.exit(1);
}
const end = src.indexOf('\nasync fn serve_static_asset', start);
const fnBody = src.slice(start, end);

// 抓 format!( r#"..."# ) 里的模板正文
const tplStart = fnBody.indexOf('r#"');
const tplEnd = fnBody.indexOf('"#,', tplStart);
if (tplStart < 0 || tplEnd < 0) {
  console.error('找不到 format! 的 r#"..."# 模板');
  process.exit(1);
}
const template = fnBody.slice(tplStart + 3, tplEnd);

const results = [];
const check = (name, pass, detail = '') => {
  results.push({ name, pass, detail });
  console.log(`${pass ? 'PASS' : 'FAIL'}  ${name}${detail ? '  → ' + detail : ''}`);
};

// 1) 抽取具名占位符与实参，确认配平
const placeholders = new Set(
  [...template.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).filter((n) => n !== 'wsProtocol'),
);
const argsBlock = fnBody.slice(tplEnd);
const assigned = new Set([...argsBlock.matchAll(/^\s+(\w+) = /gm)].map((m) => m[1]));

const missing = [...placeholders].filter((p) => !assigned.has(p));
check('模板占位符都有对应实参', missing.length === 0, missing.length ? '缺 ' + missing.join(', ') : `${placeholders.size} 个`);

const unused = [...assigned].filter((a) => !placeholders.has(a) && !['app_name', 'app_version'].includes(a));
check('没有多余实参', unused.length === 0, unused.length ? '多余 ' + unused.join(', ') : '');

// 2) 按 format! 规则替换，产出 JS
const FAKE = {
  marker: 'data-tauri-remote-service-otools-web',
  index_route: '/__tauri_remote_service_otools_web__/index.js',
  rpc_route: '/__tauri_remote_service_otools_web__/rpc.js',
  ws_route: '/ws/otools/native',
  app_name: '"OTools"',
  app_version: '"1.0.0"',
  platform: '"windows"',
  is_dev: 'false',
};

let js = template.replace(/\{\{/g, '\u0000L').replace(/\}\}/g, '\u0000R');
js = js.replace(/\{(\w+)\}/g, (m, name) => {
  if (!(name in FAKE)) throw new Error(`没有为占位符 ${name} 准备假值`);
  return FAKE[name];
});
js = js.replace(/\u0000L/g, '{').replace(/\u0000R/g, '}');

check('渲染产物不含未替换的 Rust 转义', !js.includes('{{') && !js.includes('}}'));
check('渲染产物保留 JS 模板字面量 ${...}', js.includes('${wsProtocol}'), '');
check('渲染产物注入了 rpc.js 导入', js.includes('/__tauri_remote_service_otools_web__/rpc.js'));
check('渲染产物调用了 createHostFsRpc', js.includes('createHostFsRpc(wsUrl, token)'));
check('渲染产物把 hostFsInvoke 传给 facade', js.includes('hostFsInvoke,'));

// 3) 语法检查：产物是一整个 <script> 标签，先剥掉标签再当模块检查
const innerJs = js
  .replace(/^\s*<script[^>]*>/i, '')
  .replace(/<\/script>\s*$/i, '');
const tmp = path.join(os.tmpdir(), `otools-injected-${Date.now()}.mjs`);
fs.writeFileSync(tmp, innerJs, 'utf8');
let syntaxOk = true;
let syntaxErr = '';
try {
  execFileSync(process.execPath, ['--check', tmp], { stdio: ['ignore', 'pipe', 'pipe'] });
} catch (e) {
  syntaxOk = false;
  syntaxErr = (e.stderr?.toString() || e.message || '').split('\n').slice(0, 4).join(' | ');
} finally {
  fs.existsSync(tmp) && fs.unlinkSync(tmp);
}
check('渲染产物是合法 JS', syntaxOk, syntaxErr);

const passed = results.filter((r) => r.pass).length;
console.log(`\n${passed}/${results.length} 通过`);
if (passed !== results.length) process.exit(1);
