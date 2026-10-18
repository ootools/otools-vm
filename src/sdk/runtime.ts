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

export const openHostExternal = (url: string): void => {
  const api = readApi();
  if (api?.shellOpenExternal) {
    void api.shellOpenExternal(url);
  }
};
