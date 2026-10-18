# 仓库协作约定

本文件记录 `otools-vm` 仓库的专属工作约定，与主仓 `otools/AGENTS.md` 互补。

## 技术栈

- 前端：Vue 3 + Element Plus + UnoCSS，Vite 构建，产物落 `dist/`。
- 原生：Rust cdylib，入口 `otools_plugin_invoke` / `otools_plugin_free`，产物落 `lib/`。
- 插件标识：`packid` / `uuid` 均为 `otools-vm`，与主仓 `plugins/` 下的插件平级。

## 分层

- 前端按 `services/`（命令封装）、`state/`（响应式 store）、`components/`（视图）、`types/` 分层，
  不要把业务逻辑堆进 `App.vue`。
- native 按 `engine/`（后端）、`model.rs`（领域模型）、`store.rs`（持久化）分层。
  新增能力先看 `Engine` trait 能不能承载，不要为单个后端开洞。

## 双后端纪律

- 任何新能力都要在 `EngineCaps` 里声明，并同时给出 VZ 与 QEMU 的取值；
  前端按 caps 灰掉不支持的开关，而不是让用户点了才报错。
- 不要为了「跨端一致」把 VZ 独有的能力降级成 QEMU 能做的水平。

## 线程与内存

- VZ / AppKit 对象**只能在主线程**创建和访问，统一经 `engine::vz::on_main` 调度。
- 不要在 `on_main` 里等待 VZ 的 completion handler：回调也排在主队列，会自死锁。
  启动/停止一律 fire-and-forget，状态靠轮询 `state()`。
- 实例状态放主线程 `thread_local`，不要试图塞进 `Send` 的全局锁。

## 原生视图

- 客户机画面是 NSView，不在 webview 里。前端只负责报矩形，native 负责挂载与换算。
- 坐标换算是「纵向翻转」：`y = parentHeight - rect.y - rect.height`。
- 虚拟机停止时必须摘掉视图，否则黑框会一直盖在宿主界面上。

## 版本与发布

- 升版本要同时改 `plugin.json` 与 `package.json`，并保证 `tag === v<version>`。
- 动态库不入 git（`.gitignore` 已排除 `lib/` 与 `*.dylib`），由 CI 构建。
- 改发布脚本后跑 `node --test scripts/*.test.mjs`（若有）与 `pnpm verify`。

## 文案

- 用户可见文案全部走 `i18n/*.json`，8 种语言同步更新。
- 技术名词（virtiofs、spice-vdagent、QEMU、ISO）保持原文，不翻译。
- 不要把实现说明写进界面文案。
