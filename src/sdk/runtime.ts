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

/** 宿主与 native 都拿不到选择面板时的标记（此时只能手动填路径）。 */
export const PICK_UNAVAILABLE = 'PICK_UNAVAILABLE';

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
 * 选择文件 / 目录。
 *
 * 顺序上**优先用宿主对话框**：`otools.dialog.open` 是宿主提供的系统原生面板，
 * 各平台都有；而 native 的 AppKit 面板只在 macOS 上存在。
 *
 * 早期实现反过来（先问 native、失败再回落），在 Windows 上每次都会先付一次
 * 必然失败的跨进程调用，更糟的是**宿主对话框若也拿不到，错误会被调用方
 * 那句「也可以直接粘贴绝对路径」吞掉**——用户看到的就是「点了没反应、
 * 只弹一句莫名其妙的话」。
 *
 * 所以这里：宿主对话框可用就直接用；只有在它不可用时才去问 native；
 * 两边都不行时抛出**带原因**的错误，让界面能明确告诉用户该怎么绕过。
 */
export async function pickPath(options: {
  directory?: boolean;
  title: string;
  /** 仅文件选择时生效。 */
  filters?: { name: string; extensions: string[] }[];
}): Promise<string | null> {
  const hostError = hostDialogError();

  if (!hostError) {
    const picked = await openHostDialog({
      directory: options.directory,
      title: options.title,
      filters: options.filters,
    });
    if (picked !== null) {
      return picked;
    }
    // 返回 null 有两种可能：用户点了取消，或者宿主面板本身没弹出来。
    // 后者不该静默——否则用户会觉得按钮坏了。用 native 侧兜底再试一次。
  }

  const command = options.directory ? 'vm_pick_folder' : 'vm_pick_file';
  try {
    const result = await invokeNative<{ path: string | null }>(command, {
      title: options.title,
    });
    if (result && typeof result === 'object' && 'path' in result) {
      return result.path ?? null;
    }
  } catch {
    // native 在本平台没有面板（PICK_DIALOG_UNAVAILABLE），继续往下报错。
  }

  throw new Error(hostError ?? PICK_UNAVAILABLE);
}

/** 宿主没有对话框能力时返回原因，可用时返回 null。 */
function hostDialogError(): string | null {
  const api = readApi();
  if (!api?.dialog?.open) {
    return HOST_DIALOG_UNAVAILABLE;
  }
  return null;
}

export const openHostExternal = (url: string): void => {
  const api = readApi();
  if (api?.shellOpenExternal) {
    void api.shellOpenExternal(url);
  }
};
