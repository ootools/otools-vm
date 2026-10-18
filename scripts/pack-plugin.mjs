/**
 * 打包插件为 .oplg（与主仓 `scripts/plugin_pack.py` 的产物格式一致）。
 * ------------------------------------------------------------------
 * `.oplg` 是一个 ZIP，扁平结构：
 *   plugin.json / logo.svg / dist/** / lib/**
 *
 * 本插件是 **native 插件**：`lib/` 里必须至少有一份平台动态库
 * （`macOS.dylib` / `Windows.dll` / `Linux.so`），宿主按当前平台挑对应的那份。
 * 动态库由 CI 用 cargo 构建后放进 `lib/`，不入 git。
 *
 * 用法：
 *   node scripts/pack-plugin.mjs [--out dist-pack] [--version 0.1.0] [--allow-missing-lib]
 */
import { deflateRawSync, crc32 } from 'node:zlib';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const PLATFORM_LIBS = ['macOS.dylib', 'Windows.dll', 'Linux.so'];
const LOGO_MAX_BYTES = 512 * 1024;

function arg(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 && process.argv[index + 1] ? process.argv[index + 1] : fallback;
}

function flag(name) {
  return process.argv.includes(`--${name}`);
}

function walk(dir, base = dir) {
  const out = [];
  for (const name of fs.readdirSync(dir)) {
    const full = path.join(dir, name);
    const stat = fs.lstatSync(full);
    if (stat.isSymbolicLink()) throw new Error(`打包目录不能包含符号链接：${full}`);
    if (stat.isDirectory()) out.push(...walk(full, base));
    else if (stat.isFile()) out.push(path.relative(base, full).split(path.sep).join('/'));
  }
  return out;
}

function dosTime(date) {
  const time =
    ((date.getHours() << 11) | (date.getMinutes() << 5) | (date.getSeconds() / 2)) & 0xffff;
  const day =
    (((date.getFullYear() - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate()) & 0xffff;
  return { time, day };
}

/** 手写 ZIP：不引第三方库，CI 只要有 node 就能打包。 */
function zip(entries) {
  const chunks = [];
  const central = [];
  let offset = 0;
  const now = dosTime(new Date());
  for (const entry of entries) {
    const nameBuf = Buffer.from(entry.name, 'utf8');
    const deflated = deflateRawSync(entry.data, { level: 9 });
    const useDeflate = deflated.length < entry.data.length;
    const payload = useDeflate ? deflated : entry.data;
    const method = useDeflate ? 8 : 0;
    const crc = crc32(entry.data);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6);
    local.writeUInt16LE(method, 8);
    local.writeUInt16LE(now.time, 10);
    local.writeUInt16LE(now.day, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(payload.length, 18);
    local.writeUInt32LE(entry.data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    local.writeUInt16LE(0, 28);
    chunks.push(local, nameBuf, payload);

    const dir = Buffer.alloc(46);
    dir.writeUInt32LE(0x02014b50, 0);
    dir.writeUInt16LE(20, 4);
    dir.writeUInt16LE(20, 6);
    dir.writeUInt16LE(0x0800, 8);
    dir.writeUInt16LE(method, 10);
    dir.writeUInt16LE(now.time, 12);
    dir.writeUInt16LE(now.day, 14);
    dir.writeUInt32LE(crc, 16);
    dir.writeUInt32LE(payload.length, 20);
    dir.writeUInt32LE(entry.data.length, 24);
    dir.writeUInt16LE(nameBuf.length, 28);
    dir.writeUInt32LE(0, 42);
    dir.writeUInt32LE(offset, 42);
    central.push(dir, nameBuf);

    offset += local.length + nameBuf.length + payload.length;
  }

  const centralBuf = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBuf.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...chunks, centralBuf, end]);
}

/** 生产清单：去掉开发期配置（正式包校验会拒收带 devUrl / quickDev 的包）。 */
export function productionManifest(manifest, version = '') {
  const next = version || String(manifest.version || '').trim();
  const entry = manifest.entry;
  if (
    typeof entry !== 'string' ||
    !entry ||
    entry.startsWith('/') ||
    entry.includes('\\') ||
    entry.includes(':')
  ) {
    throw new Error('插件 entry 必须是包内相对路径');
  }
  if (entry.split('/').some((part) => !part || part === '..' || part === '.')) {
    throw new Error(`插件 entry 含非法路径段：${entry}`);
  }
  const result = { ...manifest, version: next };
  delete result.devUrl;
  delete result.quickDev;
  if (result.native && typeof result.native === 'object') {
    result.native = { ...result.native, autoReload: false };
  }
  return result;
}

function main() {
  const source = JSON.parse(fs.readFileSync(path.join(root, 'plugin.json'), 'utf8'));
  const manifest = productionManifest(source, arg('version', source.version));
  const { version, packid } = manifest;

  const outDir = path.join(root, arg('out', 'dist-pack'));
  fs.mkdirSync(outDir, { recursive: true });

  const entries = [
    { name: 'plugin.json', data: Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`, 'utf8') },
  ];

  for (const logo of ['logo.svg', 'logo.png']) {
    const full = path.join(root, logo);
    if (!fs.existsSync(full)) continue;
    const bytes = fs.readFileSync(full);
    if (bytes.length > LOGO_MAX_BYTES) {
      throw new Error(
        `logo ${logo} 超过 ${LOGO_MAX_BYTES} 字节（当前 ${bytes.length}），宿主会回退成 emoji 图标`,
      );
    }
    entries.push({ name: logo, data: bytes });
  }

  for (const name of ['dist', 'lib']) {
    const dir = path.join(root, name);
    if (!fs.existsSync(dir)) continue;
    for (const relative of walk(dir)) {
      entries.push({ name: `${name}/${relative}`, data: fs.readFileSync(path.join(dir, relative)) });
    }
  }

  const entryFile = String(manifest.entry || '').replace(/^\.?\//, '');
  if (!entries.some((item) => item.name === entryFile)) {
    throw new Error(`manifest.entry 指向的文件不在包内：${entryFile}（请先执行 pnpm build）`);
  }
  if (!entries.some((item) => item.name === 'logo.svg')) {
    throw new Error('插件根目录必须提供 logo.svg');
  }

  const libs = PLATFORM_LIBS.filter((name) => entries.some((item) => item.name === `lib/${name}`));
  if (!libs.length && !flag('allow-missing-lib')) {
    throw new Error(
      `lib/ 下没有任何平台动态库（${PLATFORM_LIBS.join(' / ')}）。先构建 native，或加 --allow-missing-lib 仅做前端联调。`,
    );
  }

  const output = path.join(outDir, `${packid}-${version}.oplg`);
  fs.writeFileSync(output, zip(entries));

  fs.writeFileSync(path.join(outDir, 'plugin.json'), entries[0].data);
  fs.copyFileSync(path.join(root, 'logo.svg'), path.join(outDir, 'logo.svg'));

  const releaseFiles = [path.basename(output), 'plugin.json', 'logo.svg'];
  const sums = releaseFiles.map(
    (name) =>
      `${createHash('sha256').update(fs.readFileSync(path.join(outDir, name))).digest('hex')}  ${name}`,
  );
  fs.writeFileSync(path.join(outDir, 'SHA256SUMS'), `${sums.join('\n')}\n`);

  const meta = {
    packid,
    uuid: manifest.uuid,
    version,
    output,
    entries: entries.length,
    totalBytes: entries.reduce((sum, item) => sum + item.data.length, 0),
    nativeLibs: libs,
  };
  fs.writeFileSync(path.join(outDir, 'meta.json'), `${JSON.stringify(meta, null, 2)}\n`, 'utf8');
  process.stdout.write(`${JSON.stringify(meta)}\n`);
}

try {
  main();
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
