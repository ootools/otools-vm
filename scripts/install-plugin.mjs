#!/usr/bin/env node
/**
 * 把 dist-pack 下生成的 .oplg 解包安装到 ~/.otools/plugins/<packid>
 *
 * .oplg 是自定义 zip（deflateRaw），这里手写读取器，不引入依赖。
 * 安装前会把现有安装目录整体备份到 ~/.otools/plugins/.backup/<packid>-<version>-<ts>。
 *
 * 用法：
 *   node scripts/install-plugin.mjs [--file dist-pack/otools-vm-0.5.4.oplg] [--dry-run]
 */
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import crypto from 'node:crypto';
import os from 'node:os';

const ROOT = process.cwd();
const argv = process.argv.slice(2);
const argOf = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};
const DRY = argv.includes('--dry-run');

const manifest = JSON.parse(fs.readFileSync(path.join(ROOT, 'plugin.json'), 'utf8'));
const packid = manifest.packid || manifest.uuid || manifest.id;
const version = manifest.version;
const file = path.resolve(ROOT, argOf('--file', path.join('dist-pack', `${packid}-${version}.oplg`)));

if (!fs.existsSync(file)) {
  console.error(`找不到安装包：${file}`);
  process.exit(1);
}

/** 解析 zip 中央目录，返回 {name, size, compressedSize, method, localOffset} */
function listEntries(buf) {
  // 找 EOCD (0x06054b50)
  let eocd = -1;
  for (let i = buf.length - 22; i >= Math.max(0, buf.length - 66000); i -= 1) {
    if (buf.readUInt32LE(i) === 0x06054b50) { eocd = i; break; }
  }
  if (eocd < 0) throw new Error('不是合法的 zip：找不到 EOCD');

  const total = buf.readUInt16LE(eocd + 10);
  let off = buf.readUInt32LE(eocd + 16);
  const entries = [];
  for (let i = 0; i < total; i += 1) {
    if (buf.readUInt32LE(off) !== 0x02014b50) throw new Error(`中央目录项 ${i} 签名不匹配`);
    const method = buf.readUInt16LE(off + 10);
    const compressedSize = buf.readUInt32LE(off + 20);
    const size = buf.readUInt32LE(off + 24);
    const nameLen = buf.readUInt16LE(off + 28);
    const extraLen = buf.readUInt16LE(off + 30);
    const commentLen = buf.readUInt16LE(off + 32);
    const localOffset = buf.readUInt32LE(off + 42);
    const name = buf.toString('utf8', off + 46, off + 46 + nameLen);
    entries.push({ name, method, size, compressedSize, localOffset });
    off += 46 + nameLen + extraLen + commentLen;
  }
  return entries;
}

function readEntry(buf, entry) {
  const lo = entry.localOffset;
  if (buf.readUInt32LE(lo) !== 0x04034b50) throw new Error(`本地头签名不匹配：${entry.name}`);
  const nameLen = buf.readUInt16LE(lo + 26);
  const extraLen = buf.readUInt16LE(lo + 28);
  const start = lo + 30 + nameLen + extraLen;
  const raw = buf.subarray(start, start + entry.compressedSize);
  if (entry.method === 0) return Buffer.from(raw);
  if (entry.method === 8) return zlib.inflateRawSync(raw);
  throw new Error(`不支持的压缩方式 ${entry.method}：${entry.name}`);
}

const buf = fs.readFileSync(file);
const entries = listEntries(buf).filter((e) => !e.name.endsWith('/'));
console.log(`[1/4] 读取安装包 ${path.basename(file)} -> ${entries.length} 个文件`);

const target = path.join(os.homedir(), '.otools', 'plugins', packid);
console.log(`[2/4] 目标目录 ${target}`);

let backupPath = null;
if (fs.existsSync(target) && fs.readdirSync(target).length > 0) {
  const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
  const backupRoot = path.join(os.homedir(), '.otools', 'plugins', '.backup');
  backupPath = path.join(backupRoot, `${packid}-${version}-${stamp}`);
  if (!DRY) {
    fs.mkdirSync(backupRoot, { recursive: true });
    fs.cpSync(target, backupPath, { recursive: true });
  }
  console.log(`[3/4] 备份旧安装 -> ${backupPath}`);
} else {
  console.log('[3/4] 无旧安装，跳过备份');
}

if (DRY) {
  console.log('[4/4] --dry-run 已跳过写入');
  for (const e of entries) console.log('   ', e.name, e.size);
  process.exit(0);
}

// 只清理旧的受管产物，避免把用户手放的额外文件误删
for (const sub of ['dist', 'lib']) {
  const p = path.join(target, sub);
  if (fs.existsSync(p)) fs.rmSync(p, { recursive: true, force: true });
}
for (const f of ['plugin.json', 'logo.svg', 'SHA256SUMS', 'meta.json']) {
  const p = path.join(target, f);
  if (fs.existsSync(p)) fs.rmSync(p, { force: true });
}

fs.mkdirSync(target, { recursive: true });
for (const e of entries) {
  const dest = path.join(target, e.name);
  if (!dest.startsWith(target)) throw new Error(`非法路径越界：${e.name}`);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, readEntry(buf, e));
}
console.log(`[4/4] 已写入 ${entries.length} 个文件`);

// 校验
const installed = JSON.parse(fs.readFileSync(path.join(target, 'plugin.json'), 'utf8'));
const sha = (p) => (fs.existsSync(p) ? crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') : 'MISSING');
const nativeName = 'Windows.dll';
const srcDll = path.join(ROOT, 'lib', nativeName);
const dstDll = path.join(target, 'lib', nativeName);

console.log('');
console.log('=== 校验 ===');
console.log(`  安装版本      ${installed.version}  (期望 ${version})  ${installed.version === version ? 'OK' : 'MISMATCH'}`);
console.log(`  native 存在   ${fs.existsSync(dstDll) ? 'OK' : 'MISSING'}  ${nativeName}`);
console.log(`  native 一致   ${sha(srcDll) === sha(dstDll) ? 'OK' : 'MISMATCH'}`);
const distFiles = fs.existsSync(path.join(target, 'dist')) ? fs.readdirSync(path.join(target, 'dist')).length : 0;
console.log(`  dist 文件数   ${distFiles}`);
if (backupPath) console.log(`  回滚目录      ${backupPath}`);

if (installed.version !== version || !fs.existsSync(dstDll) || sha(srcDll) !== sha(dstDll)) {
  console.error('\n校验失败');
  process.exit(2);
}
console.log('\n安装成功');
