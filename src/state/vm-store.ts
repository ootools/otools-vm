import { computed, reactive } from 'vue';
import { VmApi } from '@/services/vm-api';
import type { EngineInfo, HostInfo, VmCreateInput, VmSummary, VmUpdateInput } from '@/types/vm';

interface VmStoreState {
  loading: boolean;
  busy: boolean;
  error: string;
  engine: EngineInfo | null;
  host: HostInfo | null;
  vms: VmSummary[];
  selectedId: string;
  lastActionError: string;
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
});

const selected = computed(() => state.vms.find((vm) => vm.id === state.selectedId) ?? null);

const engineReady = computed(() => Boolean(state.engine?.available));

async function load(): Promise<void> {
  state.loading = true;
  try {
    const workspace = await VmApi.getWorkspace();
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
  } catch {
    // 轮询失败静默，下一次再试。
  }
}

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
  load,
  refreshStates,
  select,
  start,
  stop,
  pause,
  resume,
  create,
  update,
  remove,
};
