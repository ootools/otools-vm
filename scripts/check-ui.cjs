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
 * 4. 选完目录会问 `vm_disk_space` 并显示可用空间；
 * 5. 遗留的孤儿虚拟机能列出、能一键清理，且**不误走 `vm_stop`**；
 * 6. 没有展开入口时（欢迎页）不给出「收起侧栏」把手；
 * 6b. HCS 不再作为可选引擎出现（它没有画面，而 QEMU 同样有 WHPX 加速）；
 * 7. 选中虚拟机后可收起，并能从工具栏展开；
 * 8. 运行中设置面板只放开内存，其余配置被 inert 锁住；
 * 8b. GL 能力以只读状态展示，不可用时说明缺 virglrenderer；
 * 9. HCS 无画面时给出一键切回 QEMU 的出口，且**先关机再切引擎**。
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

function engineInfo(available, kind) {
  const hcs = kind === 'hcs';
  return {
    kind: kind || 'qemu',
    available: available,
    label: hcs ? 'HCS' : 'QEMU',
    reason: available ? '' : '未找到 qemu-system-*。可以在设置里一键获取，装好后无需重启宿主。',
    version: available ? (hcs ? '10.0.26100' : 'QEMU 10.0.0') : '',
    hostEntitled: true,
    accelerator: available ? 'whpx' : null,
    caps: {
      nativeView: false, sharedDirs: !hcs, sharedDirsGuests: hcs ? [] : ['linux','windows'],
      clipboard: false, autoResize: false, audio: !hcs, snapshots: false,
      clone: !hcs, resizeDisk: true, rosetta: false, bridgedNetwork: !hcs,
      // HCS 没有 framebuffer 接口——这正是「切回 QEMU」那条路径存在的理由。
      // 桩里必须如实反映，否则无画面分支根本走不到。
      displayOutput: !hcs,
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
        // 默认「引擎不可用」——那正是用例 1 要测的场景。需要其它组合的用例
        // 自己预置 __engineAvailable / __engineKind。
        return { engine: engineInfo(window.__engineAvailable === true, window.__engineKind), vms: window.__vms || [], host: {
          os: 'Windows 11', arch: 'x86_64', hostname: 'TEST', recommendedCpus: 8,
          recommendedMemoryMb: 16384, totalMemoryMb: 32768 } };
      case 'vm_hcs_status':
        return { supported: true, available: false, needsElevation: true, elevated: false,
          inHyperVAdmins: false, reason: '需要管理员权限', version: '10.0.26100' };
      case 'vm_get_settings':
        return { enginePreference: 'auto' };
      case 'vm_set_engine_preference':
        // 忠实模拟 native：切换后引擎立刻变。否则「切回 QEMU 就有画面」这条
        // 路径测不出来——前端刷新后仍会拿到旧的 HCS 能力集。
        window.__engineKind = (payload && payload.preference) === 'hcs' ? 'hcs' : 'qemu';
        window.__engineAvailable = true;
        window.__prefCalls = (window.__prefCalls || []).concat([payload && payload.preference]);
        return { enginePreference: (payload && payload.preference) || 'auto' };
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
      case 'vm_stop':
        // 关机后状态要真的变，否则「先关机再切引擎」那条链路会卡在运行中。
        if (window.__vms) {
          window.__vms = window.__vms.map((item) =>
            item.id === (payload && payload.id) ? { ...item, state: 'stopped' } : item,
          );
        }
        return { ok: true };
      case 'vm_start':
        return { ok: true };
      case 'vm_rosetta_status':
        return { status: 'notSupported' };
      case 'vm_gl_support':
        // 默认「不可用」：绝大多数老版本 QEMU 不带 virglrenderer。
        // 需要测可用态的用例自己预置 __glSupported。
        return {
          supported: window.__glSupported === true,
          note: 'GL 加速只解决宿主侧的合成与传输。客户机内还需 virgl 驱动：Linux 自带，Windows 需 virtio-win 且支持有限',
        };
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
      // 孤儿清理：默认没有孤儿，测试里用 addInitScript 预置 window.__orphans
      // 来模拟「上次进程被强杀」。真机上这两个命令只在 HCS 引擎下有内容。
      case 'vm_hcs_orphans':
        return { orphans: window.__orphans || [] };
      case 'vm_hcs_orphans_kill':
        window.__killedOrphans = (payload && payload.ids) || [];
        window.__orphans = [];
        return { killed: window.__killedOrphans, failed: [] };
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

/** 造一台可被选中的虚拟机（只填 UI 需要的字段）。 */
function fakeVm(id, name) {
  return {
    id, name, state: 'stopped', engine: 'qemu', lastError: '',
    os: 'linux', arch: 'x86_64', cpus: 4, memoryMb: 4096, diskSizeGb: 40, diskPath: '',
    firmware: 'bios', firmwarePath: '', driverIso: '', installerPath: '', virtiofsdPath: '',
    network: 'nat', networkIface: '', bootOrder: [], usbMedia: '', portForwards: [],
    sharedDirs: [], clipboard: true, autoResize: true, audio: true, rosetta: false,
    displayWidth: 1280, displayHeight: 800, notes: '', createdAt: 0,
  };
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

  console.log('\n[5] 孤儿清理：遗留机器能列出并一键收掉');
  // 模拟「上次宿主被强杀」：HCS 里还留着两台机器。
  await page.addInitScript(`window.__orphans = ['vm-alpha', 'vm-beta'];`);
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(900);
  const orphanText = await page.locator('body').innerText();
  check('出现孤儿提示卡片', orphanText.includes('遗留'), orphanText.slice(0, 300));
  check('列出了遗留机器 id',
    orphanText.includes('vm-alpha') && orphanText.includes('vm-beta'), orphanText.slice(0, 400));
  // 回归：清理绝不能走 vm_stop —— 它要先 load_spec，孤儿机器的 spec 已被删，会永远失败。
  check('没有误用 vm_stop 清理孤儿',
    !(await calls()).some((c) => c.command === 'vm_stop' && c.payload?.id === 'vm-alpha'));
  const killButton = page.getByRole('button', { name: '全部清理' });
  check('出现「全部清理」按钮', (await killButton.count()) > 0);
  if (await killButton.count()) {
    await killButton.first().click();
    await page.waitForTimeout(900);
    const killCall = (await calls()).find((c) => c.command === 'vm_hcs_orphans_kill');
    check('调用了 vm_hcs_orphans_kill', Boolean(killCall));
    check('把两个 id 都传下去了',
      JSON.stringify(killCall?.payload?.ids) === JSON.stringify(['vm-alpha', 'vm-beta']),
      JSON.stringify(killCall?.payload));
    const afterKill = await page.locator('body').innerText();
    check('清理后卡片消失', !afterKill.includes('vm-alpha'), afterKill.slice(0, 300));
  }

  console.log('\n[6] 侧栏收起：没有展开入口时不允许收起');
  // 欢迎页没有选中虚拟机，工具栏（唯一带「展开侧栏」按钮的地方）不渲染，
  // 所以此时必须不给出收起把手，否则收起后永远打不开。
  await page.addInitScript(`window.__orphans = []; window.__vms = [];`);
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(900);
  const collapseHandle = page.getByTitle('收起侧栏');
  check('欢迎页没有「收起侧栏」把手', (await collapseHandle.count()) === 0);
  // 欢迎页仍在，说明侧栏没被误收起
  check('欢迎页侧栏可见', (await page.locator('.vm-sidebar').count()) > 0);

  console.log('\n[6b] HCS 不再作为可选引擎出现');
  // 即便本机 HCS 完全可用，也不该给用户这个入口：它没有画面、没有共享目录、
  // 没接端口转发，而它唯一的长处（Hyper-V 直接执行 CPU）QEMU 通过 WHPX 也有。
  await page.addInitScript(`window.__vms = []; window.__engineAvailable = true;`);
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(900);
  const welcomeText = await page.locator('body').innerText();
  check('欢迎页没有 HCS 选项', !welcomeText.includes('HCS 引擎'), welcomeText.slice(0, 300));
  check('没有引擎单选项', (await page.locator('.el-radio-group').count()) === 0 || !welcomeText.includes('默认（QEMU）'));

  console.log('\n[7] 选中虚拟机时可以收起，并能从工具栏展开');
  await page.addInitScript(`window.__vms = [${JSON.stringify(fakeVm('vm-solo', '单机'))}];`);
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(900);
  const vmItem = page.locator('.vm-item').first();
  check('列表里出现虚拟机', (await vmItem.count()) > 0);
  if (await vmItem.count()) {
    await vmItem.click();
    await page.waitForTimeout(600);
    const handle = page.getByTitle('收起侧栏');
    check('选中后出现「收起侧栏」把手', (await handle.count()) > 0);
    if (await handle.count()) {
      await handle.first().click();
      await page.waitForTimeout(500);
      check('收起后侧栏消失', (await page.locator('.vm-sidebar').count()) === 0);
      const expandBtn = page.getByTitle('展开侧栏');
      check('工具栏出现「展开侧栏」按钮', (await expandBtn.count()) > 0);
      if (await expandBtn.count()) {
        await expandBtn.first().click();
        await page.waitForTimeout(500);
        check('点展开后侧栏回来', (await page.locator('.vm-sidebar').count()) > 0);
      }
    }
  }

  console.log('\n[8] 运行中设置面板：只放开内存，其余锁定');
  // 用 HCS 引擎 + 运行中的机器：这是唯一允许热调内存的组合。
  const runningVm = { ...fakeVm('vm-live', '运行中'), state: 'running' };
  await page.addInitScript(
    `window.__vms = [${JSON.stringify(runningVm)}]; window.__engineKind = 'hcs'; window.__engineAvailable = true;`,
  );
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(900);
  await page.locator('.vm-item').first().click();
  await page.waitForTimeout(500);
  // 打开设置抽屉。工具栏末尾是「设置」「删除」两个圆形按钮，删除在最后，
  // 所以取倒数第二个——取 last() 会把机器删掉，抽屉里没有 vm 就只剩空壳。
  const settingsBtn = page.locator('.vm-toolbar-actions button').nth(-2);
  await settingsBtn.click();
  await page.waitForTimeout(700);
  const drawer = page.locator('.el-drawer');
  check('设置抽屉已打开', (await drawer.count()) > 0);
  if (await drawer.count()) {
    const drawerText = await drawer.innerText();
    check('提示「只能调整内存」', drawerText.includes('只能调整内存'), drawerText.slice(0, 300));
    // 回归：内存滑块必须仍可用（这是唯一的例外），CPU 必须锁住。
    // 用 inert 而不是逐个 :disabled —— 断言点就落在 inert 容器上。
    const nameLocked = await page.locator('.vm-settings-name').evaluate(
      (node) => node.hasAttribute('inert'),
    );
    const restLocked = await page.locator('.vm-settings-rest').evaluate(
      (node) => node.hasAttribute('inert'),
    );
    check('名字等配置被 inert 锁住', nameLocked === true);
    check('其余配置被 inert 锁住', restLocked === true);
    // 内存滑块在锁区之外，应当还能拖
    const memSlider = page.locator('.vm-settings-section').filter({ hasText: '内存' }).locator('.el-slider').first();
    const memDisabled = await memSlider.evaluate((node) =>
      node.classList.contains('is-disabled'),
    );
    check('内存滑块仍可用', memDisabled === false);
    // GL 能力只读展示：界面上要能看到状态，但不给开关（客户机缺驱动时会花屏）
    const drawerHasGl = drawerText.includes('图形加速');
    check('展示宿主端图形加速能力', drawerHasGl, drawerText.slice(-260));
  }

  console.log('\n[8b] GL 能力不可用时给出可执行说明');
  await page.addInitScript(`window.__glSupported = false;`);
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(900);
  await page.locator('.vm-item').first().click();
  await page.waitForTimeout(500);
  await page.locator('.vm-toolbar-actions button').nth(-2).click();
  await page.waitForTimeout(900);
  const glText = await page.locator('.el-drawer').innerText();
  check('标出不可用', glText.includes('不可用'), glText.slice(-300));
  check('说明原因（缺 virglrenderer）', glText.includes('virglrenderer'), glText.slice(-300));

  console.log('\n[9] HCS 无画面 → 一键切回 QEMU');
  // HCS 拿不到画面是这套后端最大的使用障碍。以前界面只解释原因、不给出口，
  // 引擎偏好藏在别处、切换还要重启插件。这条用例保证「点了真能切过去」。
  //
  // 注意：`addInitScript` 会**累积**（新脚本在旧脚本之后执行），所以这里必须
  // 重新赋值而不是追加列表，否则前面用例的机器会留在 __vms 里，.first()
  // 选中的还是上一组那台。
  const hcsVm = { ...fakeVm('vm-hcs', 'HCS 机器'), state: 'running', engine: 'hcs' };
  await page.addInitScript(
    `window.__vms = [${JSON.stringify(hcsVm)}]; window.__engineKind = 'hcs'; window.__engineAvailable = true; window.__prefCalls = [];`,
  );
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(900);
  await page.locator('.vm-item').filter({ hasText: 'HCS 机器' }).first().click();
  await page.waitForTimeout(700);
  const consoleText = await page.locator('.vm-console').innerText();
  // 机器正在运行时占位区（含 noDisplayHint）不渲染，底栏的 noDisplayFoot 才是
  // 用户实际看到的那句——断言要打在实际渲染的文案上，否则测的是一个不存在的节点。
  check('说明拿不到画面', consoleText.includes('无画面接口'), consoleText.slice(0, 260));
  const switchBtn = page.getByRole('button', { name: '切回 QEMU 查看画面' });
  check('给出「切回 QEMU」的出口', (await switchBtn.count()) > 0);
  if (await switchBtn.count()) {
    await switchBtn.first().click();
    await page.waitForTimeout(1400);
    const afterSwitch = await calls();
    // 关键顺序：必须先关机，再切引擎。反过来的话机器会变成孤儿。
    check('切换前先关了机', afterSwitch.some((c) => c.command === 'vm_stop' && c.payload?.id === 'vm-hcs'));
    const prefCall = afterSwitch.find((c) => c.command === 'vm_set_engine_preference');
    check('调用了 vm_set_engine_preference(qemu)', prefCall?.payload?.preference === 'qemu',
      JSON.stringify(prefCall?.payload));
    const stopIdx = afterSwitch.findIndex((c) => c.command === 'vm_stop' && c.payload?.id === 'vm-hcs');
    const prefIdx = afterSwitch.findIndex((c) => c.command === 'vm_set_engine_preference');
    check('关机发生在切引擎之前', stopIdx >= 0 && prefIdx > stopIdx, `stop=${stopIdx} pref=${prefIdx}`);
  }

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
