/**
 * headless 实测：QEMU 一键获取与存储目录选择的端到端回归。
 *
 * 依赖 playwright（本机在 managed node workspace 里），运行方式：
 *   node node_modules/vite/bin/vite.js build --emptyOutDir=false
 *   NODE_PATH=<playwright 所在的 node_modules> node scripts/check-ui.cjs
 *
 * 只测「我们新写的代码是否真的连上了 native 命令」，不测 Element Plus 自身。
 * 做法：把 dist 起成静态服务，在 window 上注入一个假宿主（`window.otools`），
 * 记录前端到底调了哪些命令、带了什么参数。
 *
 * 关键断言：
 * 1. 没有 QEMU 时欢迎页出现「一键获取」卡片，并调用 `vm_qemu_plan`；
 * 2. 点「一键获取」调用 `vm_qemu_install`，随后进入轮询；
 * 3. 创建对话框出现「存储位置」，选目录走宿主 dialog，提交时 `storageDir` 真的传下去；
 * 4. 选完目录会问 `vm_disk_space` 并显示可用空间。
 */
const { chromium } = require('playwright');
const http = require('http');
const fs = require('fs');
const path = require('path');

// 脚本在 scripts/ 下，dist 在仓库根。
const ROOT = path.join(__dirname, '..', 'dist');
const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.json': 'application/json',
};

function serve() {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      const url = decodeURIComponent((req.url || '/').split('?')[0]);
      let file = path.join(ROOT, url === '/' ? 'index.html' : url);
      if (!file.startsWith(ROOT) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
        file = path.join(ROOT, 'index.html');
      }
      res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
      fs.createReadStream(file).pipe(res);
    });
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

/** 假宿主：记录所有调用，按命令给出可预测的响应。 */
const HOST_STUB = `
window.__calls = [];
window.__dialogPath = 'D:/VMs/storage';

function engineInfo(available) {
  return {
    kind: 'qemu',
    available: available,
    label: 'QEMU',
    reason: available ? '' : '未找到 qemu-system-*。可以在设置里一键获取，装好后无需重启宿主。',
    version: available ? 'QEMU 10.0.0' : '',
    hostEntitled: true,
    accelerator: available ? 'whpx' : null,
    caps: {
      nativeView: false, sharedDirs: true, sharedDirsGuests: ['linux','windows'],
      clipboard: false, autoResize: false, audio: true, snapshots: false,
      clone: true, resizeDisk: true, rosetta: false, bridgedNetwork: true, displayOutput: true
    }
  };
}

window.otools = {
  getPluginUuid: () => 'otools-vm',
  dialog: {
    open: async (options) => {
      window.__calls.push({ command: '__dialog', payload: options });
      return window.__dialogPath;
    }
  },
  invokeNativePlugin: async (uuid, command, payload) => {
    window.__calls.push({ command: command, payload: payload });
    switch (command) {
      case 'vm_get_workspace':
        return { engine: engineInfo(false), vms: [], host: {
          os: 'Windows 11', arch: 'x86_64', hostname: 'TEST', recommendedCpus: 8,
          recommendedMemoryMb: 16384, totalMemoryMb: 32768 } };
      case 'vm_hcs_status':
        return { supported: true, available: false, needsElevation: true, elevated: false,
          inHyperVAdmins: false, reason: '需要管理员权限', version: '10.0.26100' };
      case 'vm_get_settings':
        return { enginePreference: 'auto' };
      case 'vm_qemu_plan':
        return { automatic: true, strategy: 'winget', note: '安装时可能弹出系统提权确认，请选择「是」。',
          command: 'winget install -e --id SoftwareFreedomConservancy.QEMU',
          reason: '用系统自带的应用安装器安装，带完整性校验，之后可在系统里正常卸载',
          supportedPlatform: true, binaryPath: 'C:/Users/x/.otools/local/vm/qemu/bin/qemu-system-x86_64.exe' };
      case 'vm_qemu_install_status':
        // 只有真的点过安装才报 installing：否则刷新页面就会一直显示进度条。
        return window.__installStarted
          ? { phase: 'installing', strategy: 'winget', progress: 0.42, downloaded: 0, total: 0, error: '', binary: '' }
          : { phase: 'idle', strategy: '', progress: 0, downloaded: 0, total: 0, error: '', binary: '' };
      case 'vm_qemu_install':
        window.__installStarted = true;
        return { phase: 'installing', strategy: 'winget', progress: 0, downloaded: 0, total: 0, error: '', binary: '' };
      case 'vm_disk_space':
        return { path: (payload && payload.path) || 'C:/default', availableBytes: 120 * 1024 * 1024 * 1024 };
      case 'vm_create':
        return { id: 'vm-1', name: (payload && payload.name) || 'x', state: 'stopped', engine: 'qemu', lastError: '',
          os: 'linux', arch: 'x86_64', cpus: 4, memoryMb: 4096, diskSizeGb: 40, diskPath: '', firmware: 'bios',
          firmwarePath: '', driverIso: '', installerPath: '', virtiofsdPath: '', network: 'nat',
          networkIface: '', bootOrder: [], usbMedia: '', portForwards: [], sharedDirs: [],
          clipboard: true, autoResize: true, audio: true, rosetta: false,
          displayWidth: 1280, displayHeight: 800, notes: '', createdAt: 0 };
      case 'vm_start':
        return { ok: true };
      case 'vm_rosetta_status':
        return { status: 'notSupported' };
      case 'vm_network_backend':
        return { modes: [], smb: { available: false, reason: '' }, hostAddress: '10.0.2.2' };
      case 'vm_network_interfaces':
        return [];
      case 'vm_pick_folder':
      case 'vm_pick_file':
        // 忠实模拟 Windows 真机：非 macOS 一律返回 PICK_DIALOG_UNAVAILABLE，
        // 迫使前端改走宿主 dialog。若这里返回 {}，就等于把真机行为测没了。
        throw new Error('PICK_DIALOG_UNAVAILABLE');
      case 'vm_engine_reprobe':
        return engineInfo(true);
      default:
        return {};
    }
  }
};
`;

const failures = [];
function check(name, condition, detail) {
  if (condition) {
    console.log(`  PASS  ${name}`);
  } else {
    failures.push(`${name}${detail ? ` — ${detail}` : ''}`);
    console.log(`  FAIL  ${name}${detail ? ` — ${detail}` : ''}`);
  }
}

(async () => {
  const server = await serve();
  const port = server.address().port;
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  page.on('pageerror', (error) => console.log('  [pageerror]', error.message));
  await page.addInitScript(HOST_STUB);
  await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: 'networkidle' });
  await page.waitForTimeout(900);

  const calls = () => page.evaluate(() => window.__calls);

  console.log('\n[1] 欢迎页：无 QEMU 时给出一键获取');
  const bodyText = await page.locator('body').innerText();
  check('出现「还没有 QEMU」', bodyText.includes('还没有 QEMU'), bodyText.slice(0, 200));
  check('出现「一键获取」按钮', bodyText.includes('一键获取'));
  check('调用了 vm_qemu_plan', (await calls()).some((c) => c.command === 'vm_qemu_plan'));
  // 回归：QEMU 未装时 caps 是全 false 的占位值，不能据此说「该后端不提供画面」
  // ——那是 HCS 的文案，放在这里会让人以为装好 QEMU 也没有画面。
  check(
    '未装 QEMU 时不误报「不提供画面」',
    !bodyText.includes('不提供画面') && !bodyText.includes('没有画面'),
    bodyText.slice(0, 400),
  );

  console.log('\n[2] 点一键获取 → 调 vm_qemu_install 并轮询进度');
  await page.getByRole('button', { name: '一键获取' }).click();
  await page.waitForTimeout(1600);
  const afterInstall = await calls();
  check('调用了 vm_qemu_install', afterInstall.some((c) => c.command === 'vm_qemu_install'));
  const polls = afterInstall.filter((c) => c.command === 'vm_qemu_install_status').length;
  check('开始轮询进度（≥2 次）', polls >= 2, `polls=${polls}`);
  const installingText = await page.locator('body').innerText();
  check('展示安装中状态', installingText.includes('正在安装') || installingText.includes('正在下载'));

  console.log('\n[3] 重新检测 → 调 vm_engine_reprobe');
  // 先刷新，清掉页面内的安装态，让「重新检测」按钮重新出现。
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(800);
  const reprobeButton = page.getByRole('button', { name: '重新检测' });
  check('刷新后按钮已恢复（说明安装态没被固化）', (await reprobeButton.count()) > 0);
  if (await reprobeButton.count()) {
    await reprobeButton.first().click();
    await page.waitForTimeout(900);
    check('调用了 vm_engine_reprobe', (await calls()).some((c) => c.command === 'vm_engine_reprobe'));
  }

  console.log('\n[4] 创建对话框：存储位置可选、可用空间可见、提交时带上 storageDir');
  // 重新加载，避免安装态干扰。
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(800);
  await page.getByRole('button', { name: '新建虚拟机' }).first().click();
  await page.waitForTimeout(600);
  const dialogText = await page.locator('.el-dialog').innerText();
  check('对话框出现「存储位置」', dialogText.includes('存储位置'), dialogText.slice(0, 200));
  check('打开对话框时问了默认目录空间',
    (await calls()).some((c) => c.command === 'vm_disk_space' && !c.payload?.path));

  // 点「选择目录」→ 走宿主 dialog
  const pickButton = page.locator('.el-dialog').getByRole('button', { name: '选择目录' });
  await pickButton.click();
  await page.waitForTimeout(700);
  const pickedCalls = await calls();
  check('选目录走了宿主 dialog 接口', pickedCalls.some((c) => c.command === '__dialog' && c.payload?.directory === true));
  check('选完后又问了这个目录的空间',
    pickedCalls.some((c) => c.command === 'vm_disk_space' && c.payload?.path === 'D:/VMs/storage'));

  const inputValue = await page.locator('.el-dialog input').evaluateAll((nodes) =>
    nodes.map((node) => node.value).filter(Boolean));
  check('存储位置输入框已填入所选目录', inputValue.includes('D:/VMs/storage'), JSON.stringify(inputValue));

  const hintText = await page.locator('.el-dialog').innerText();
  check('显示了可用空间', /可用空间\s*120\s*GB/.test(hintText), hintText.slice(-200));

  // 填名字并提交，验证 storageDir 真的传到 native。
  await page.locator('.el-dialog input').first().fill('测试机');
  const createButton = page.locator('.el-dialog').getByRole('button', { name: '创建' });
  await createButton.last().click();
  await page.waitForTimeout(1200);
  const createCall = (await calls()).find((c) => c.command === 'vm_create');
  check('调用了 vm_create', Boolean(createCall));
  check('vm_create 带上了 storageDir', createCall?.payload?.storageDir === 'D:/VMs/storage',
    JSON.stringify(createCall?.payload?.storageDir));

  await browser.close();
  server.close();

  console.log('\n================ 结果 ================');
  if (failures.length) {
    console.log(`${failures.length} 项失败：`);
    failures.forEach((item) => console.log(' - ' + item));
    process.exitCode = 1;
  } else {
    console.log('全部通过');
  }
})();
