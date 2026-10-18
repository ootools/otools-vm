# otools-vm

OTools 的桌面虚拟机插件：在 Apple 芯片上以接近原生的速度运行 **Windows / Linux 图形虚拟机**，
支持宿主目录共享、双向剪贴板、窗口缩放自动改变分辨率。

对标 Parallels 的**可用性与体验**（一次点开就能装系统、窗口即屏幕、拖拽即改分辨率），
而不是对标它的 3D 加速 —— 后者依赖厂商自研 GPU 驱动，不在本插件的目标内。

## 引擎

| 宿主 | 引擎 | 说明 |
|---|---|---|
| macOS / Apple 芯片 | Apple Virtualization.framework（`objc2-virtualization`） | 主力路径：硬件虚拟化、virtio-gpu 直通显示、virtio-fs、SPICE 剪贴板 |
| Linux / Windows | QEMU（HVF / KVM / WHPX） | 回落路径：进程管理 + 参数拼装，画面走 QEMU 自己的窗口 |

选 VZ 而不是「统一用 QEMU」，是因为在 Apple 芯片上 VZ 才是能对标 Parallels 的那条路：
QEMU 的 TCG 模拟慢一个数量级，而 QEMU + HVF 又拿不到 VZ 那种可直接嵌进窗口的原生视图。

## 它是怎么把客户机画面放进插件界面的

客户机画面**不是**视频流，也不是 canvas：

1. 前端在页面里留一块空白矩形（控制台区域），把它的 CSS 坐标报给 native；
2. native 在主线程创建 `VZVirtualMachineView`，按坐标换算成 NSView frame（纵向翻转）；
3. 作为 subview 挂到宿主窗口的 contentView 上，盖在 webview 之上。

所以键盘、鼠标、触控板、GPU 全走原生通路，延迟和 UTM 同一量级。
窗口尺寸变化时前端用 `ResizeObserver` 同步矩形，native 只改 frame；
`automaticallyReconfiguresDisplay = true` 让 VZ 把新的显示模式下发给客户机 ——
这就是「拖动窗口自动改分辨率」的全部实现。

宿主窗口拿不到时（例如插件被塞进非常规容器），退化为**独立窗口模式**，至少画面可见。

## 能力矩阵

| 能力 | VZ（macOS） | QEMU（其它宿主） |
|---|---|---|
| 内嵌原生画面 | ✅ | ❌ 走 QEMU 自己的窗口 |
| 宿主目录共享 | ✅ Linux 客户机开箱可用 | ✅ 9p，Linux 客户机 |
| 双向剪贴板 | ✅ 需客户机装 spice-vdagent | ❌ |
| 窗口缩放自适应分辨率 | ✅ | ❌ |
| 音频输出 | ✅ | ✅ |
| 暂停 / 继续 | ✅ | ❌ |
| 克隆虚拟机 | ✅ APFS 写时复制，秒级 | ✅ 整盘复制 |
| 扩容磁盘 | ✅ 只增不减 | ✅ 只增不减 |
| Rosetta（Linux 客户机跑 x86_64） | ✅ | ❌ |
| 快照 | ❌（规划中） | ❌ |

**文件选择**：macOS 用 `NSOpenPanel` 拿真实路径（webview 的 `input[type=file]` 在沙箱里
只给文件名 + 临时拷贝，拿不到可用的宿主路径）。其它宿主没有这个面板，界面会退回
「手动输入绝对路径」；浏览器里则直接给出「需要桌面端」的提示 —— 虚拟机本身依赖原生
虚拟化，浏览器里没有可降级的实现。

**宿主要带 virtualization 权限（必须）**：

`com.apple.security.virtualization` 属于**进程**，插件动态库无法自带，只能由主程序
在签名时申请。宿主 `src-tauri/entitlements.plist` 里必须有这一项，
否则 `validateWithError` 直接拒绝：

```
The process doesn't have the "com.apple.security.virtualization" entitlement.
```

实测 ad-hoc 签名（`signingIdentity: "-"`）即可生效，不需要 provisioning profile。
插件侧会把这条报错翻译成中文提示，而不是把 Apple 原文甩给用户。

**已知限制（写清楚比藏着好）**：

- **Windows 客户机的目录共享**：virtio-fs 在 Windows 上没有可用驱动，v1 只在 Linux 客户机开箱可用。
  后续计划内置 SMB 服务绕开这个限制。
- **Windows 客户机的自适应分辨率**：依赖 virtio-gpu 驱动，未装驱动时分辨率固定。
- **3D 加速**：VZ 只提供 virtio-gpu，无 GPU 直通。需要 DirectX 12 / OpenGL 4 的场景不适用。
- **x86_64 客户机**：Apple 芯片上只能 QEMU 模拟，速度不可用，仅作兼容保留。

## 客户机准备

**Linux**

```bash
# 共享目录（标签见插件设置页）
sudo mount -t virtiofs <tag> /mnt/<tag>

# 剪贴板 + 自适应分辨率
sudo apt install spice-vdagent   # 或 dnf / pacman 对应包
```

**Windows**

装 `spice-guest-tools`（含 vdagent）可启用剪贴板；分辨率自适应还需 virtio-gpu 驱动。

**Linux —— 启用 Rosetta（x86_64 转译）**

先在宿主上装一次 Rosetta：

```bash
softwareupdate --install-rosetta --agree-to-license
```

建机时打开「Rosetta（x86_64 转译）」，然后在客户机内：

```bash
sudo mkdir -p /mnt/rosetta
sudo mount -t virtiofs rosetta /mnt/rosetta
sudo /mnt/rosetta/rosetta update    # 首次注册
# 之后 amd64 二进制可直接执行，例如：
# docker run --platform linux/amd64 hello-world
```

**扩容磁盘之后**，宿主侧只是块设备变大，客户机里还要自己扩：

```bash
sudo growpart /dev/vda 1 && sudo resize2fs /dev/vda1   # Linux
# Windows：磁盘管理 → 右键卷 → 扩展卷
```

**克隆**走 APFS 写时复制，40GB 逻辑盘实测 139ms、几乎不占额外空间；
副本只有真正写入才分配块。

## 数据位置

```
~/.otools/local/vm/
└── vms/<id>/
    ├── vm.json      # 规格（唯一真相）
    ├── disk.raw     # 稀疏 RAW 系统盘
    ├── nvram.bin    # EFI 变量存储
    └── vm.log       # QEMU 后端的输出
```

删掉 `nvram.bin` 等于让客户机忘记引导项，重装系统时才会用到。

## 开发

```bash
pnpm install
pnpm dev                 # 前端，端口 6162

# native（插件形态，产物拷到 lib/）
cd native
cargo build --release
mkdir -p ../lib && cp target/release/libotools_vm_native.dylib ../lib/macOS.dylib

pnpm build               # 前端产物到 dist/
pnpm pack                # 打 .oplg 到 dist-pack/
pnpm verify              # 校验包结构
pnpm smoke               # 原生层冒烟（真动态库 + 真磁盘，18 项断言）
pnpm probe:start         # 真启动验证（签上 virtualization 权限的 C 宿主 + 真客户机）
```

`pnpm smoke` 直接用 ctypes 调 `otools_plugin_invoke`，不经宿主，
覆盖建机 / 克隆（校验写时复制确实生效）/ 扩容 / 拒绝缩容 / 删除。

`pnpm probe:start` 解决 Python 拿不到 virtualization 权限的问题：它编译一个
最小的 Objective-C 宿主、签上权限、dlopen 插件动态库，真起一台虚拟机并驱动
主运行循环直到 `state=running`。**改 `config.rs` 后必须跑这个**——
VZ 的很多错误只在异步重建配置时才暴露（例如一次 SPICE 附件挂错容器导致的
主线程 SIGSEGV，只有跑到 running 才发现）。

宿主按平台挑 `lib/macOS.dylib` / `lib/Windows.dll` / `lib/Linux.so`，
文件名是宿主约定，**不跟随 crate 名**。

## 发布

推 `v*` 标签触发 `.github/workflows/release-plugin.yml`：
构建三平台动态库 + 前端 → 打 `.oplg` → 建 Release → 按 `vars.OTOOLS_PUBLISH_MARKET` 提交市场。

⚠️ `scripts/publish-market.mjs` 校验 `tag === "v" + plugin.json.version`，升版本要同时改
`plugin.json` 与 `package.json`。

## 目录

```
native/src/
  lib.rs              # FFI 入口 + 命令分发
  model.rs            # VmSpec / VmState / EngineCaps
  store.rs            # ~/.otools/local/vm 读写
  engine/mod.rs       # Engine trait + 引擎选择
  engine/vz/          # Virtualization.framework 后端
    config.rs         #   设备组合（含 Rosetta 目录共享）
    view.rs           #   NSView 挂载与坐标换算
    dialog.rs         #   NSOpenPanel
  engine/qemu/        # QEMU 后端
src/
  sdk/runtime.ts      # 插件 ↔ native 桥（独立仓自带的精简版）
  services/           # 命令封装
  state/              # 响应式 store
  components/         # 侧栏 / 工具条 / 控制台 / 创建 / 设置
i18n/                 # 8 种语言
```
