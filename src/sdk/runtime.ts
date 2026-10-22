/**
 * 插件 ↔ native 桥接，以及宿主能力（对话框等）的入口。
 *
 * ## 为什么对话框走 `otools-plugin-sdk`
 *
 * 宿主注入的 `window.otools.dialog` 内部是「先试 Tauri 命令、失败回落 webview_fs」，
 * **两条路的异常都被 `try/catch` 吞成 `null`**（见主仓
 * `src/platform/runtime/otools/embedded-runtime.js` 的 `dialogApi`）。
 * 于是「底层桥不可用」和「用户点了取消」在前端拿到的是同一个 `null`，
 * 表现为「点了没反应」——这是最难查的一类问题。
 *
 * 仓库里通用的 `otools-plugin-sdk` 是同一套语义的**唯一受维护实现**，
 * 各插件（disk / aigv / git …）都通过它的 vite alias 把 `@tauri-apps/*`
 * 指到 shim 上。这里沿用同一条路，而不是自造一套调用方式。
 *
 * ## 但面板「一定能弹出来」不能只靠 SDK
 *
 * SDK 的 `open()` 在 shim 失败时同样返回 `null`。所以真正的保证放在
 * **插件自己的 native**：Windows 直接调 Win32 面板、macOS 用 AppKit，
 * 完全不经过任何宿主桥。这里先问 native，native 明确说「本平台没有面板」
 * 时才退回 SDK。这个顺序与「优先宿主」相反，理由就是上面那条：
 * native 的失败是**可区分**的（要么给出路径、要么给出错误、要么明确不可用）。
 */

import { pickDirectory, pickFile } from 'otools-plugin-sdk/dialog';
import { pickFromHostTree } from 'otools-plugin-sdk/host-file-tree';

declare const __OTOOLS_PLUGIN_UUID__: string | undefined;

type OtoolsApi = {
  invokeNative?: (command: string, payload?: unknown) => Promise<unknown>;
  invokeNativePlugin?: (uuid: string, command: string, payload?: unknown) => Promise<unknown>;
  getPluginUuid?: () => string;
  shellOpenPath?: (path: string) => unknown;
  shellOpenExternal?: (url: string) => unknown;
};

type RuntimeWindow = Window & {
  otools?: OtoolsApi;
  utools?: OtoolsApi;
  __TAURI_INTERNALS__?: { invoke: (command: string, args?: unknown) => Promise<unknown> };
  __OToolsEnv?: { pluginUuid?: string };
};

const runtimeWindow = (): RuntimeWindow | undefined =>
  typeof window === 'undefined' ? undefined : (window as RuntimeWindow);

const readApi = (): OtoolsApi | undefined => {
  const target = runtimeWindow();
  if (!target) {
    return undefined;
  }
  const candidates: OtoolsApi[] = [];
  if (target.otools) candidates.push(target.otools);
  if (target.utools) candidates.push(target.utools);
  for (const resolver of [() => target.parent, () => target.top]) {
    try {
      const ancestor = resolver() as RuntimeWindow | null;
      if (ancestor && ancestor !== target) {
        const api = ancestor.otools ?? ancestor.utools;
        if (api) candidates.push(api);
      }
    } catch {
      // 跨域祖先访问会被拒，忽略即可。
    }
  }
  return candidates.find(
    (api) => typeof api.invokeNativePlugin === 'function' || typeof api.invokeNative === 'function',
  ) ?? candidates[0];
};

const readPluginUuid = (api: OtoolsApi | undefined): string => {
  const fromApi = String(api?.getPluginUuid?.() ?? '').trim();
  if (fromApi) {
    // 宿主可能返回 `market:otools-vm` 这类带来源前缀的形式。
    const parts = fromApi.split(':');
    return (parts.length > 1 ? parts[parts.length - 1] : fromApi).trim();
  }
  const fromEnv = String(runtimeWindow()?.__OToolsEnv?.pluginUuid ?? '').trim();
  if (fromEnv) {
    return fromEnv;
  }
  try {
    const params = new URLSearchParams(window.location.search);
    const fromQuery = params.get('plugin') ?? params.get('pluginUuid') ?? '';
    if (fromQuery.trim()) {
      return fromQuery.trim();
    }
  } catch {
    // ignore
  }
  return typeof __OTOOLS_PLUGIN_UUID__ === 'string' ? __OTOOLS_PLUGIN_UUID__ : 'otools-vm';
};

/** 把 `{ok,data}` / `{ok:false,error}` 信封拆开，与宿主桥行为保持一致。 */
const unwrap = (result: unknown): unknown => {
  if (result && typeof result === 'object' && 'ok' in (result as Record<string, unknown>)) {
    const envelope = result as { ok?: boolean; error?: string; data?: unknown };
    if (envelope.ok === false) {
      throw new Error(envelope.error || 'native 调用失败');
    }
    return 'data' in envelope ? envelope.data : result;
  }
  return result;
};

export const isPluginRuntime = (): boolean => {
  const api = readApi();
  return typeof api?.invokeNativePlugin === 'function' || typeof api?.invokeNative === 'function';
};

export const isTauriRuntime = (): boolean =>
  typeof runtimeWindow()?.__TAURI_INTERNALS__?.invoke === 'function';

export async function invokeNative<T = unknown>(command: string, payload?: unknown): Promise<T> {
  const api = readApi();

  if (api && typeof api.invokeNativePlugin === 'function') {
    const uuid = readPluginUuid(api);
    return (await api.invokeNativePlugin(uuid, command, payload ?? null)) as T;
  }

  if (api && typeof api.invokeNative === 'function') {
    return (await api.invokeNative(command, payload ?? null)) as T;
  }

  const internals = runtimeWindow()?.__TAURI_INTERNALS__;
  if (internals?.invoke) {
    const raw = await internals.invoke('native_plugin_invoke', {
      uuid: readPluginUuid(undefined),
      method: command,
      payload: payload ?? null,
    });
    return unwrap(raw) as T;
  }

  throw new Error('当前不在 OTools 宿主内运行，虚拟机功能不可用');
}

export const openHostPath = (path: string): void => {
  const api = readApi();
  if (api?.shellOpenPath) {
    void api.shellOpenPath(path);
  }
};

/** 本平台 native 侧没有系统面板时的标记（仅 Linux）。 */
export const PICK_DIALOG_UNAVAILABLE = 'PICK_DIALOG_UNAVAILABLE';

/** 宿主与 native 都拿不到选择面板时的标记（此时只能手动填路径）。 */
export const PICK_UNAVAILABLE = 'PICK_UNAVAILABLE';

export type PickPathOptions = {
  directory?: boolean;
  title: string;
  /** 仅文件选择时生效。 */
  filters?: { name: string; extensions: string[] }[];
};

/** 选择结果的来源：native 选的是用户本机路径，web 树选的是宿主侧路径。 */
export type PickSource = 'native-local' | 'host-tree';

export type PickPathResult = {
  path: string | null;
  source: PickSource;
};

/**
 * 选择文件 / 目录，返回路径与来源；用户取消时 `path` 为 `null`。
 *
 * 顺序：**先问插件自己的 native**（Windows Win32 / macOS AppKit），
 * 只有 native 明确回答「本平台没有面板」时才退回上层。
 *
 * 反过来（宿主优先）之所以不行：宿主那条路失败时只会给出 `null`，
 * 与「用户取消」无法区分，最终表现就是按钮点了没反应。native 这条路
 * 的每种结局都是可区分的，所以它必须先跑。
 */
export async function pickPathWithSource(options: PickPathOptions): Promise<PickPathResult> {
  const nativeResult = await pickViaNative(options);
  if (nativeResult.kind === 'picked') {
    return { path: nativeResult.path, source: 'native-local' };
  }
  if (nativeResult.kind === 'cancelled') {
    return { path: null, source: 'native-local' };
  }

  // native 在本平台没有面板（Linux / 纯 web / 远程访问）。
  // 桌面 webview 仍走宿主原生对话框；纯浏览器或远程访问时原生框弹不出来，
  // 改用宿主文件系统 DOM 树（选出来的是宿主侧路径）。
  if (isTauriRuntime()) {
    const sdkOptions = { title: options.title, filters: options.filters };
    const picked = options.directory
      ? await pickDirectory(sdkOptions)
      : await pickFile(sdkOptions);
    const path = typeof picked === 'string' && picked.trim() ? picked.trim() : null;
    return { path, source: 'native-local' };
  }

  try {
    const result = await pickFromHostTree({ ...options, directory: Boolean(options.directory) });
    if (result.kind === 'picked' && result.paths[0]) {
      return { path: result.paths[0], source: 'host-tree' };
    }
    return { path: null, source: 'host-tree' };
  } catch {
    return { path: null, source: 'host-tree' };
  }
}

/**
 * 选择文件 / 目录，返回绝对路径；用户取消返回 `null`。
 * 等价于 `pickPathWithSource(...).path`，给不需要来源信息的调用点用。
 */
export async function pickPath(options: PickPathOptions): Promise<string | null> {
  return (await pickPathWithSource(options)).path;
}

type NativePickOutcome =
  | { kind: 'picked'; path: string }
  | { kind: 'cancelled' }
  | { kind: 'unavailable' };

/**
 * 调 native 面板，并把「选到 / 取消 / 本平台没有」三种结局分开。
 *
 * 关键点：**不能把异常统一当成「不可用」**。真正不可用只有
 * `PICK_DIALOG_UNAVAILABLE` 这一个标记；其它异常是真失败，必须往上抛，
 * 否则又回到「静默吞掉」的老问题。
 */
async function pickViaNative(options: PickPathOptions): Promise<NativePickOutcome> {
  const command = options.directory ? 'vm_pick_folder' : 'vm_pick_file';
  const payload: Record<string, unknown> = { title: options.title };
  if (!options.directory && options.filters?.length) {
    payload.filterName = options.filters[0].name;
    payload.extensions = options.filters[0].extensions;
  }

  let result: unknown;
  try {
    result = await invokeNative<{ path: string | null }>(command, payload);
  } catch (error) {
    const message = typeof error === 'string' ? error : (error as Error)?.message ?? '';
    if (message.includes(PICK_DIALOG_UNAVAILABLE)) {
      return { kind: 'unavailable' };
    }
    throw error;
  }

  const path = result && typeof result === 'object' ? (result as { path?: unknown }).path : null;
  if (typeof path === 'string' && path.trim()) {
    return { kind: 'picked', path: path.trim() };
  }
  return { kind: 'cancelled' };
}

export const openHostExternal = (url: string): void => {
  const api = readApi();
  if (api?.shellOpenExternal) {
    void api.shellOpenExternal(url);
  }
};
