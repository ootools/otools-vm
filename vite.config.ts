import { defineConfig } from 'vite';
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

export default defineConfig({
  // 插件在自己的 webview / 独立 origin 下运行，资源必须走相对路径。
  base: './',
  plugins: [vue(), UnoCSS()],
  define: {
    __OTOOLS_PLUGIN_UUID__: JSON.stringify(readPluginIdentity()),
  },
  resolve: {
    alias: {
      '@': path.resolve(pluginRoot, 'src'),
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
