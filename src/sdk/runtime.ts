/**
 * 插件 ↔ native 桥接。
 *
 * 独立仓库没有主仓的 `otools-plugin-sdk`，这里实现等价的最小集合：
 * 1. 宿主注入的 `window.otools.invokeNativePlugin` / `invokeNative`（会自己解包 `{ok,data}`）；
 * 2. 独立 APP / 开发环境下的 `__TAURI_INTERNALS__.invoke`（需要自己解包）；
 * 3. 浏览器直开时抛出可读错误，而不是静默返回 undefined。
 */

declare const __OTOOLS_PLUGIN_UUID__: string | undefined;

type OtoolsApi = {
  invokeNative?: (command: string, payload?: unknown) => Promise<unknown>;
  invokeNativePlugin?: (uuid: string, command: string, payload?: unknown) => Promise<unknown>;
  getPluginUuid?: () => string;
  shellOpenPath?: (path: string) => unknown;
  shellOpenExternal?: (url: string) => unknown;
  dialog?: {
    open?: (options?: {
      directory?: boolean;
      multiple?: boolean;
      title?: string;
      defaultPath?: string;
      filters?: { name: string; extensions: string[] }[];
    }) => Promise<string | string[] | null>;
  };
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

/** 宿主没有提供对话框能力时的标记：调用方据此降级为手动输入。 */
export const HOST_DIALOG_UNAVAILABLE = 'HOST_DIALOG_UNAVAILABLE';

/**
 * 弹出宿主提供的文件 / 目录选择框，返回绝对路径；取消返回 null。
 *
 * **为什么要走宿主而不是插件自己实现**：宿主的 `otools.dialog.open()` 底层是
 * Tauri 的 `plugin:dialog|open`，Windows / macOS / Linux 全平台都有原生面板，
 * 而且拿不到插件命令时会自动回退到 webview_fs 通道，网页形态同样可用。
 * 插件 native 侧只有 macOS 用 AppKit 实现了面板，其它平台本就不该重复造。
 */
export async function openHostDialog(options: {
  directory?: boolean;
  title?: string;
  filters?: { name: string; extensions: string[] }[];
}): Promise<string | null> {
  const api = readApi();
  const dialog = api?.dialog;
  if (!dialog?.open) {
    // 脱离宿主直接打开页面时没有对话框能力，交给调用方降级。
    throw new Error(HOST_DIALOG_UNAVAILABLE);
  }
  const picked = await dialog.open({
    directory: options.directory === true,
    title: options.title,
    filters: options.filters,
  });
  // multiple 没开时正常返回 string；数组取第一个，null 表示用户取消。
  if (Array.isArray(picked)) {
    return picked[0] ?? null;
  }
  return typeof picked === 'string' && picked.trim() ? picked : null;
}

/**
 * 选择文件 / 目录：优先 native（macOS 用 AppKit 面板，与 VZ 窗口模型一致），
 * native 明确表示「本平台没有」时自动改走宿主对话框。
 *
 * 返回 null 表示用户取消或拿不到选择能力，调用方据此保持原值。
 */
export async function pickPath(options: {
  directory?: boolean;
  title: string;
  /** 仅文件选择时生效。 */
  filters?: { name: string; extensions: string[] }[];
}): Promise<string | null> {
  const command = options.directory ? 'vm_pick_folder' : 'vm_pick_file';
  try {
    const result = await invokeNative<{ path: string | null }>(command, {
      title: options.title,
    });
    // **必须区分「键不存在」与「键为 null」**：
    // - `path: null` 是 native 明确告诉我们用户点了取消；
    // - 连 `path` 键都没有，说明这个响应不是选择面板给的（老版本 native、
    //   或走了别的分支），不能当成取消——那会表现为「点了按钮什么都不发生」。
    if (!result || typeof result !== 'object' || !('path' in result)) {
      return openHostDialog({
        directory: options.directory,
        title: options.title,
        filters: options.filters,
      });
    }
    return result.path ?? null;
  } catch {
    // native 在本平台没有面板（PICK_DIALOG_UNAVAILABLE），或面板本身创建失败
    // ——一律改走宿主对话框。
    return openHostDialog({
      directory: options.directory,
      title: options.title,
      filters: options.filters,
    });
  }
}

export const openHostExternal = (url: string): void => {
  const api = readApi();
  if (api?.shellOpenExternal) {
    void api.shellOpenExternal(url);
  }
};
