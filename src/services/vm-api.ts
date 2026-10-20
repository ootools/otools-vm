import { invokeNative } from '@/sdk/runtime';
import type {
  EngineInfo,
  EnginePreference,
  HcsStatus,
  HostInfo,
  HostInterface,
  NetworkBackendStatus,
  VmCreateInput,
  VmSummary,
  VmCloneInput,
  VmResizeDiskInput,
  VmUpdateInput,
  VmWorkspace,
  RosettaStatus,
  SnapshotSupport,
  ViewRect,
  VmSettings,
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

  /** 宿主网卡清单：桥接 / 仅主机模式下供用户选择具体网卡。 */
  listNetworkInterfaces: () =>
    invokeNative<HostInterface[]>('vm_network_interfaces'),
  /**
   * 网络模式与宿主原生共享的可用性探测。
   *
   * 传 id 时额外返回该虚拟机视角下宿主在客户机里的地址（NAT 下固定 10.0.2.2），
   * 供 UI 直接拼出客户机侧访问路径。
   */
  getNetworkBackend: (id?: string) =>
    invokeNative<NetworkBackendStatus>('vm_network_backend', id ? { id } : {}),

  /**
   * HCS 引擎状态。非 Windows 平台返回 `supported: false`，调用方不必自己判断平台。
   */
  getHcsStatus: () => invokeNative<HcsStatus>('vm_hcs_status'),

  getSettings: () => invokeNative<VmSettings>('vm_get_settings'),

  /** 切换引擎偏好。**需要重启插件生效**（引擎是进程级单例）。 */
  setEnginePreference: (preference: EnginePreference) =>
    invokeNative<VmSettings>('vm_set_engine_preference', { preference }),

  start: (id: string) => invokeNative<{ ok: boolean }>('vm_start', { id }),
  stop: (id: string) => invokeNative<{ ok: boolean }>('vm_stop', { id }),
  pause: (id: string) => invokeNative<{ ok: boolean }>('vm_pause', { id }),
  resume: (id: string) => invokeNative<{ ok: boolean }>('vm_resume', { id }),

  /** 该虚拟机能否存档（VZ 按配置判定，不是全局开关）。 */
  getSnapshotSupport: (id: string) =>
    invokeNative<SnapshotSupport>('vm_snapshot_support', { id }),
  /** 存档：运行中会先自动暂停，再整机落盘。 */
  saveState: (id: string) => invokeNative<SnapshotSupport>('vm_save_state', { id }),
  /** 恢复：从存档还原整机状态并自动继续运行。 */
  restoreState: (id: string) => invokeNative<SnapshotSupport>('vm_restore_state', { id }),
  discardState: (id: string) => invokeNative<{ ok: boolean }>('vm_discard_state', { id }),

  attachView: (id: string, rect: ViewRect) =>
    invokeNative<{ ok: boolean }>('vm_attach_view', { id, rect }),
  updateViewRect: (id: string, rect: ViewRect) =>
    invokeNative<{ ok: boolean }>('vm_update_view_rect', { id, rect }),
  detachView: (id: string) => invokeNative<{ ok: boolean }>('vm_detach_view', { id }),

  // 路径选择不放这里：它是「native 优先、宿主 dialog 兜底」的跨平台逻辑，
  // 统一走 `@/sdk/runtime` 的 `pickPath()`，避免出现两套调用方式。

  shutdown: () => invokeNative<{ ok: boolean }>('vm_shutdown'),
};
