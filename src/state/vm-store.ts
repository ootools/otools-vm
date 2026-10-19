import { computed, reactive, ref, watch } from 'vue';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';
import type {
  EngineInfo,
  HostInfo,
  RosettaStatus,
  SnapshotSupport,
  VmCreateInput,
  VmSummary,
  VmUpdateInput,
} from '@/types/vm';

interface VmStoreState {
  loading: boolean;
  busy: boolean;
  error: string;
  engine: EngineInfo | null;
  host: HostInfo | null;
  vms: VmSummary[];
  selectedId: string;
  lastActionError: string;
  /** Rosetta 在宿主上的状态；null 表示还没查过。 */
  rosetta: RosettaStatus | null;
}

const state = reactive<VmStoreState>({
  loading: false,
  busy: false,
  error: '',
  engine: null,
  host: null,
  vms: [],
  selectedId: '',
  lastActionError: '',
  rosetta: null,
});

/**
 * 当前选中虚拟机的存档能力。
 *
 * VZ 是按**配置**判定能否存盘的，所以必须逐台向原生层问，不能只信
 * `engine.caps.snapshots`。切换选中项或状态变化时自动重查。
 */
const snapshotSupport = ref<SnapshotSupport | null>(null);
/**
 * 正在存/恢复。
 *
 * 这两个操作是**异步链**（save = pause→落盘，restore = load→resume），
 * 原生命令立即返回，进度靠轮询 `phase` 得知，因此不能复用同步的 `busy`。
 */
const snapshotBusy = ref(false);

const selected = computed(() => state.vms.find((vm) => vm.id === state.selectedId) ?? null);

const engineReady = computed(() => Boolean(state.engine?.available));

/**
 * 宿主是否缺 macOS 虚拟化权限。
 *
 * 该权限属于进程，插件无法自带；宿主 0.8.3 及更早版本没有它。
 * 这是「宿主版本过旧」而不是「插件配置错误」，所以单独暴露给 UI 做提示。
 */
const needsHostUpgrade = computed(
  () => Boolean(state.engine?.available) && state.engine?.hostEntitled === false,
);

/** 第一个带 virtualization 权限的宿主版本。 */
export const MIN_HOST_VERSION = '0.8.4';

async function load(): Promise<void> {
  state.loading = true;
  try {
    const workspace = await VmApi.getWorkspace();
    // Rosetta 状态只影响一个开关的可用性，查询失败不影响主流程。
    if (workspace.engine.caps.rosetta && state.rosetta === null) {
      state.rosetta = await VmApi.getRosettaStatus().catch(() => null);
    }
    state.engine = workspace.engine;
    state.host = workspace.host;
    state.vms = workspace.vms;
    state.error = '';
    if (state.selectedId && !state.vms.some((vm) => vm.id === state.selectedId)) {
      state.selectedId = '';
    }
  } catch (error) {
    state.error = error instanceof Error ? error.message : String(error);
  } finally {
    state.loading = false;
  }
}

/** 只刷新运行态，避免整表重建导致选中项闪烁。 */
async function refreshStates(): Promise<void> {
  try {
    const workspace = await VmApi.getWorkspace();
    state.engine = workspace.engine;
    state.vms = workspace.vms;
    return;
  } catch {
    // 轮询失败静默，下一次再试。
  }
}

// 切换选中项、或该机器状态变化（启动/停止/暂停）后，快照能力都可能变：
// 状态决定「能否存/恢复」，配置决定「能否存」。统一在这里重查一次。
watch(
  () => {
    const current = state.vms.find((vm) => vm.id === state.selectedId);
    return `${state.selectedId}:${current?.state ?? ''}`;
  },
  () => {
    void refreshSnapshotSupport();
  },
  { immediate: true },
);

function select(id: string): void {
  state.selectedId = id;
  state.lastActionError = '';
}

async function run(action: () => Promise<unknown>): Promise<boolean> {
  state.busy = true;
  state.lastActionError = '';
  try {
    await action();
    await refreshStates();
    return true;
  } catch (error) {
    state.lastActionError = error instanceof Error ? error.message : String(error);
    return false;
  } finally {
    state.busy = false;
  }
}

const start = (id: string) => run(() => VmApi.start(id));
const stop = (id: string) => run(() => VmApi.stop(id));
const pause = (id: string) => run(() => VmApi.pause(id));
const resume = (id: string) => run(() => VmApi.resume(id));

/** 重查当前选中项的快照能力（切选中项、状态变化后调用）。 */
async function refreshSnapshotSupport(): Promise<void> {
  if (!state.selectedId || !state.engine?.caps.snapshots) {
    snapshotSupport.value = null;
    return;
  }
  try {
    snapshotSupport.value = await VmApi.getSnapshotSupport(state.selectedId);
  } catch {
    // 查询失败不该打断主流程：清空即可，UI 会退回「不可用」。
    snapshotSupport.value = null;
  }
}

/** 轮询间隔：存档要写整个 RAM（GB 级），500ms 足够，也不会给原生层压力。 */
const SNAPSHOT_POLL_MS = 500;
/** 安全上限：超过就放弃等待，避免 UI 永远转圈。 */
const SNAPSHOT_TIMEOUT_MS = 5 * 60 * 1000;

/**
 * 等某个快照任务跑完。
 *
 * 原生层的 `save_state`/`restore_state` 只负责**发起**异步链并立即返回，
 * 完成与否要看 `phase` 回到 `idle`。这里统一轮询，成功/失败都返回结果。
 */
async function waitSnapshot(id: string): Promise<{ ok: boolean; error: string }> {
  const startedAt = Date.now();
  while (Date.now() - startedAt < SNAPSHOT_TIMEOUT_MS) {
    let info: SnapshotSupport | null = null;
    try {
      info = await VmApi.getSnapshotSupport(id);
    } catch {
      // 单次查询失败不代表任务失败，继续轮询。
    }
    if (info) {
      snapshotSupport.value = info;
      if (info.phase === 'idle') {
        return { ok: !info.error, error: info.error };
      }
    }
    await new Promise((resolve) => setTimeout(resolve, SNAPSHOT_POLL_MS));
  }
  return { ok: false, error: t('vm.snapshot.timeout') };
}

/** 挂起：把整机（含内存）落到磁盘并停住。关 App / 重启 Mac 都不丢。 */
async function saveState(id: string): Promise<boolean> {
  snapshotBusy.value = true;
  state.lastActionError = '';
  try {
    await VmApi.saveState(id);
    const result = await waitSnapshot(id);
    // 无论成败都要刷新：暂停这一步可能已经发生，状态会变。
    await refreshStates();
    if (!result.ok) {
      state.lastActionError = result.error || t('vm.snapshot.failed');
      return false;
    }
    return true;
  } catch (error) {
    state.lastActionError = error instanceof Error ? error.message : String(error);
    await refreshStates().catch(() => undefined);
    return false;
  } finally {
    snapshotBusy.value = false;
  }
}

/**
 * 继续运行。
 *
 * 两条路径，对用户是同一个「继续」：
 * - 内存里的机器还在（暂停态）→ 直接 resume，秒级；
 * - App 重启过 / 机器已停 → 从存档 restore 再 resume。
 *
 * 判断依据是引擎当前状态：`paused` 且进程里有实例时走 resume；
 * App 重启后 `state()` 会因为有存档而回报 paused，但那时进程里没有实例，
 * 必须走 restore —— 所以这里以 hasSnapshot + 实例是否存在为准：
 * 有实例才能 resume，否则只能 restore。
 */
async function continueRun(id: string): Promise<boolean> {
  snapshotBusy.value = true;
  state.lastActionError = '';
  try {
    if (state.vms.find((vm) => vm.id === id)?.state === 'paused') {
      // 先试直接 resume：内存里还活着时最快。
      // 若进程里其实没有实例（App 刚重启），原生会返回「虚拟机未运行」，
      // 这时自动改走存档恢复。
      try {
        await VmApi.resume(id);
        await refreshStates();
        return true;
      } catch {
        // 落到下面走 restore。
      }
    }
    const restored = await restoreState(id, true);
    return restored;
  } finally {
    snapshotBusy.value = false;
  }
}

/**
 * 从存档还原整机状态，并自动继续运行（App 重启后的路径）。
 *
 * `keepBusy` 供 `continueRun` 内部复用：外层已经置了 busy，
 * 内层不要再抢着清掉，否则按钮 loading 会闪一下。
 */
async function restoreState(id: string, keepBusy = false): Promise<boolean> {
  if (!keepBusy) {
    snapshotBusy.value = true;
  }
  state.lastActionError = '';
  try {
    await VmApi.restoreState(id);
    const result = await waitSnapshot(id);
    await refreshStates();
    if (!result.ok) {
      state.lastActionError = result.error || t('vm.snapshot.failed');
      return false;
    }
    return true;
  } catch (error) {
    state.lastActionError = error instanceof Error ? error.message : String(error);
    await refreshStates().catch(() => undefined);
    return false;
  } finally {
    if (!keepBusy) {
      snapshotBusy.value = false;
    }
  }
}

async function discardState(id: string): Promise<boolean> {
  const done = await run(() => VmApi.discardState(id));
  if (done) {
    await refreshSnapshotSupport();
  }
  return done;
}

async function create(input: VmCreateInput): Promise<VmSummary | null> {
  state.busy = true;
  state.lastActionError = '';
  try {
    const created = await VmApi.create(input);
    await load();
    state.selectedId = created.id;
    return created;
  } catch (error) {
    state.lastActionError = error instanceof Error ? error.message : String(error);
    return null;
  } finally {
    state.busy = false;
  }
}

async function update(input: VmUpdateInput): Promise<boolean> {
  const done = await run(() => VmApi.update(input));
  if (done) {
    await load();
  }
  return done;
}

async function clone(id: string, name: string): Promise<VmSummary | null> {
  state.busy = true;
  state.lastActionError = '';
  try {
    const created = await VmApi.clone({ id, name });
    await load();
    state.selectedId = created.id;
    return created;
  } catch (error) {
    state.lastActionError = error instanceof Error ? error.message : String(error);
    return null;
  } finally {
    state.busy = false;
  }
}

async function resizeDisk(id: string, diskSizeGb: number): Promise<boolean> {
  const done = await run(() => VmApi.resizeDisk({ id, diskSizeGb }));
  if (done) {
    await load();
  }
  return done;
}

async function remove(id: string): Promise<boolean> {
  const done = await run(() => VmApi.remove(id));
  if (done) {
    if (state.selectedId === id) {
      state.selectedId = '';
    }
    await load();
  }
  return done;
}

export const vmStore = {
  state,
  selected,
  engineReady,
  needsHostUpgrade,
  snapshotSupport,
  snapshotBusy,
  load,
  refreshStates,
  refreshSnapshotSupport,
  select,
  start,
  stop,
  pause,
  resume,
  saveState,
  restoreState,
  continueRun,
  discardState,
  create,
  update,
  remove,
  clone,
  resizeDisk,
};
