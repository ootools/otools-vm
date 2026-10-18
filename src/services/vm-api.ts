import { invokeNative } from '@/sdk/runtime';
import type {
  EngineInfo,
  HostInfo,
  VmCreateInput,
  VmSummary,
  VmCloneInput,
  VmResizeDiskInput,
  VmUpdateInput,
  VmWorkspace,
  RosettaStatus,
  ViewRect,
} from '@/types/vm';

/** 命令层薄封装：一个命令一个方法，业务组件不直接拼字符串。 */
export const VmApi = {
  getWorkspace: () => invokeNative<VmWorkspace>('vm_get_workspace'),
  getEngineInfo: () => invokeNative<EngineInfo>('vm_engine_info'),
  getHostInfo: () => invokeNative<HostInfo>('vm_host_info'),

  create: (input: VmCreateInput) => invokeNative<VmSummary>('vm_create', input),
  update: (input: VmUpdateInput) => invokeNative<VmSummary>('vm_update', input),
  remove: (id: string) => invokeNative<{ ok: boolean }>('vm_delete', { id }),
  clone: (input: VmCloneInput) => invokeNative<VmSummary>('vm_clone', input),
  resizeDisk: (input: VmResizeDiskInput) =>
    invokeNative<VmSummary>('vm_resize_disk', input),
  getRosettaStatus: () =>
    invokeNative<{ status: RosettaStatus }>('vm_rosetta_status').then((result) => result.status),

  start: (id: string) => invokeNative<{ ok: boolean }>('vm_start', { id }),
  stop: (id: string) => invokeNative<{ ok: boolean }>('vm_stop', { id }),
  pause: (id: string) => invokeNative<{ ok: boolean }>('vm_pause', { id }),
  resume: (id: string) => invokeNative<{ ok: boolean }>('vm_resume', { id }),

  attachView: (id: string, rect: ViewRect) =>
    invokeNative<{ ok: boolean }>('vm_attach_view', { id, rect }),
  updateViewRect: (id: string, rect: ViewRect) =>
    invokeNative<{ ok: boolean }>('vm_update_view_rect', { id, rect }),
  detachView: (id: string) => invokeNative<{ ok: boolean }>('vm_detach_view', { id }),

  pickFolder: (title: string) =>
    invokeNative<{ path: string | null }>('vm_pick_folder', { title }),
  pickFile: (title: string) =>
    invokeNative<{ path: string | null }>('vm_pick_file', { title }),

  shutdown: () => invokeNative<{ ok: boolean }>('vm_shutdown'),
};
