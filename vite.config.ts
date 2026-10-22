import { defineConfig, type Plugin } from 'vite';
import vue from '@vitejs/plugin-vue';
import UnoCSS from 'unocss/vite';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const pluginRoot = path.dirname(fileURLToPath(import.meta.url));

/** 构建期注入插件标识：宿主桥还没注入时，前端靠它把命令路由到本插件 native 库。 */
function readPluginIdentity(): string {
  try {
    const manifest = JSON.parse(fs.readFileSync(path.join(pluginRoot, 'plugin.json'), 'utf8')) as {
      uuid?: string;
      packid?: string;
    };
    return String(manifest.uuid || manifest.packid || '').trim();
  } catch {
    return 'otools-vm';
  }
}

/**
 * OTools 插件 SDK 的定位。
 *
 * 宿主能力（对话框 / 原生调用 / 事件）不自己实现，一律走仓库里通用的
 * `otools-plugin-sdk`：它把 `@tauri-apps/*` 的调用改写成走宿主注入的
 * `window.otools` 桥，内嵌 webview / 独立窗口 / 独立 APP 三种形态都能用。
 *
 * SDK 是私有子模块，独立仓没有 `vendor/`，所以按三种常见落位探测：
 *  1. `otools-vm/vendor/otools-plugin-sdk`（CI 里按 token 克隆到这里）
 *  2. 与主仓并列检出时的 `../otools/vendor/...`（本机开发即如此）
 *  3. 被同步进 `otools/otools/plugins/otools-vm` 时的 `../../vendor/...`
 *
 * 都找不到也不报错：构建照样能过，对话框退化成只有 native 面板（Windows/macOS
 * 本来就是 native 优先，实际不受影响）。
 */
const SDK_SRC_CANDIDATES = [
  path.resolve(pluginRoot, 'vendor/otools-plugin-sdk/src'),
  path.resolve(pluginRoot, '../otools/vendor/otools-plugin-sdk/src'),
  path.resolve(pluginRoot, '../../vendor/otools-plugin-sdk/src'),
];
const sdkSrcDir = SDK_SRC_CANDIDATES.find((dir) => fs.existsSync(path.join(dir, 'index.ts')));

/** `@tauri-apps/*` → SDK shim，与主仓 `scripts/otools-plugin-vite-config-sdk.ts` 对齐。 */
const SDK_SHIMS: Record<string, string> = {
  '@tauri-apps/api/core': 'tauri-core-shim.ts',
  '@tauri-apps/api/event': 'tauri-event-shim.ts',
  '@tauri-apps/api/window': 'tauri-window-shim.ts',
  '@tauri-apps/api/webview': 'tauri-webview-shim.ts',
  '@tauri-apps/api/app': 'tauri-app-shim.ts',
  '@tauri-apps/plugin-dialog': 'tauri-plugin-dialog-shim.ts',
  '@tauri-apps/plugin-opener': 'tauri-plugin-opener-shim.ts',
  '@tauri-apps/plugin-process': 'tauri-plugin-process-shim.ts',
  '@tauri-apps/plugin-updater': 'tauri-plugin-updater-shim.ts',
  '@tauri-apps/plugin-notification': 'tauri-plugin-notification-shim.ts',
};

const sdkAlias: Record<string, string> = sdkSrcDir
  ? {
      // 具体子路径必须排在基础 `otools-plugin-sdk` 之前：Vite 按别名顺序取第一个
      // 前缀匹配，基础别名若不后置会把 `otools-plugin-sdk/dialog` 解析成
      // `index.ts/dialog`（把文件名当目录），构建直接失败。
      'otools-plugin-sdk/dialog': path.join(sdkSrcDir, 'dialog.ts'),
      'otools-plugin-sdk/runtime': path.join(sdkSrcDir, 'runtime.ts'),
      'otools-plugin-sdk/native-event-bridge': path.join(sdkSrcDir, 'native-event-bridge.ts'),
      'otools-plugin-sdk/host-file-tree': path.join(sdkSrcDir, 'host-file-tree.ts'),
      'otools-plugin-sdk': path.join(sdkSrcDir, 'index.ts'),
      ...Object.fromEntries(
        Object.entries(SDK_SHIMS).map(([specifier, file]) => [specifier, path.join(sdkSrcDir, file)]),
      ),
    }
  : {};

/**
 * SDK 缺失时的兜底：把 `otools-plugin-sdk` 的几个子模块解析到只做「宿主无面板」
 * 的空实现。
 *
 * 没有这一步，独立仓在没有 vendor 的情况下 `vite build` 会直接失败
 * （`import ... from 'otools-plugin-sdk/xxx'` 找不到模块）。兜底实现返回 `null` /
 * 空值就够了——Windows / macOS 上 native 面板本来就排在前面，永远走不到这里。
 */
function sdkDialogFallback(): Plugin {
  const modules: Record<string, string> = {
    'otools-plugin-sdk/dialog': `export const open = async () => null;
export const save = async () => null;
export const message = async () => {};
export const confirm = async () => false;
export const ask = async () => false;
`,
    'otools-plugin-sdk/host-file-tree': `export const browseHostDir = async () => { throw new Error('host-tree unavailable'); };
export const listHostDir = async () => [];
export const readHostFile = async () => ({});
export const pickFromHostTree = async () => ({ kind: 'cancelled', source: 'host-tree', paths: [], names: [] });
`,
  };
  return {
    name: 'otools-sdk-dialog-fallback',
    resolveId(source) {
      if (sdkSrcDir) {
        return null;
      }
      if (source in modules) {
        return '\0' + source;
      }
      return null;
    },
    load(id) {
      if (!id.startsWith('\0otools-plugin-sdk/')) {
        return null;
      }
      return modules[id.slice(1)] ?? null;
    },
  };
}

export default defineConfig({
  // 插件在自己的 webview / 独立 origin 下运行，资源必须走相对路径。
  base: './',
  plugins: [vue(), UnoCSS(), sdkDialogFallback()],
  define: {
    __OTOOLS_PLUGIN_UUID__: JSON.stringify(readPluginIdentity()),
  },
  resolve: {
    alias: {
      '@': path.resolve(pluginRoot, 'src'),
      ...sdkAlias,
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2020',
    // 插件包会被宿主内嵌加载，不做 hash 分片更利于离线校验。
    rollupOptions: {
      output: {
        entryFileNames: 'otools-vm-[hash].js',
        chunkFileNames: 'otools-vm-[hash].js',
        assetFileNames: 'otools-vm-[hash][extname]',
      },
    },
  },
  server: {
    host: '127.0.0.1',
    port: 6162,
    strictPort: true,
  },
});
