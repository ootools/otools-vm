export type GuestOs = 'windows' | 'linux' | 'macos' | 'other';

export type VmState =
  | 'stopped'
  | 'starting'
  | 'running'
  | 'pausing'
  | 'paused'
  | 'resuming'
  | 'stopping'
  | 'error'
  | 'unavailable';

export interface SharedDir {
  hostPath: string;
  mountTag: string;
  readOnly: boolean;
}

export interface VmSpec {
  id: string;
  name: string;
  os: GuestOs;
  arch: string;
  cpus: number;
  memoryMb: number;
  diskPath: string;
  diskSizeGb: number;
  installerPath: string | null;
  network: 'nat' | 'bridged';
  sharedDirs: SharedDir[];
  clipboard: boolean;
  autoResize: boolean;
  audio: boolean;
  /** 在 Linux 客户机里暴露 Rosetta，用 arm64 宿主直接跑 x86_64 二进制。 */
  rosetta: boolean;
  displayWidth: number;
  displayHeight: number;
  notes: string;
  createdAt: number;
}

export interface VmSummary extends VmSpec {
  state: VmState;
  engine: string;
  lastError: string;
}

export interface EngineCaps {
  nativeView: boolean;
  sharedDirs: boolean;
  sharedDirsGuests: string[];
  clipboard: boolean;
  autoResize: boolean;
  audio: boolean;
  snapshots: boolean;
  /** 能否复制虚拟机（APFS 写时复制，秒级且几乎不占空间）。 */
  clone: boolean;
  /** 能否扩容磁盘（只增不减）。 */
  resizeDisk: boolean;
  rosetta: boolean;
  bridgedNetwork: boolean;
}

/** Rosetta 在宿主上的可用状态。 */
export type RosettaStatus = 'installed' | 'notInstalled' | 'notSupported';

export interface EngineInfo {
  kind: 'vz' | 'qemu' | 'none';
  available: boolean;
  label: string;
  reason: string;
  version: string;
  /** 宿主进程是否具备 macOS 虚拟化权限；false 说明宿主版本过旧。 */
  hostEntitled: boolean;
  caps: EngineCaps;
}

export interface HostInfo {
  os: string;
  arch: string;
  hostname: string;
  recommendedCpus: number;
  recommendedMemoryMb: number;
  totalMemoryMb: number;
}

export interface VmWorkspace {
  engine: EngineInfo;
  vms: VmSummary[];
  host: HostInfo;
}

export interface ViewRect {
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
  /** 页面视口尺寸（window.innerWidth/Height）：原生端据此抵消宿主顶部标题栏 inset。 */
  viewportWidth?: number;
  viewportHeight?: number;
}

export interface VmCreateInput {
  name: string;
  os: GuestOs;
  arch: string;
  cpus: number;
  memoryMb: number;
  diskSizeGb: number;
  installerPath?: string | null;
  sharedDirs: SharedDir[];
  clipboard?: boolean;
  autoResize?: boolean;
  audio?: boolean;
  rosetta?: boolean;
  displayWidth?: number;
  displayHeight?: number;
}

export interface VmUpdateInput {
  id: string;
  name: string;
  cpus: number;
  memoryMb: number;
  sharedDirs: SharedDir[];
  clipboard?: boolean;
  autoResize?: boolean;
  audio?: boolean;
  rosetta?: boolean;
  installerPath?: string | null;
  notes?: string;
}

export interface VmCloneInput {
  id: string;
  name: string;
}

export interface VmResizeDiskInput {
  id: string;
  diskSizeGb: number;
}

/**
 * 存档式快照能力。
 *
 * VZ 是按**配置**判定能否存盘的（设备组合变了结果就可能变），所以这是逐台查的，
 * 不是引擎级开关。
 */
export interface SnapshotSupport {
  /** 当前配置能否存盘。false 时 UI 应禁用「存档」并展示 reason。 */
  supported: boolean;
  /** 不能存盘的原因（VZ 原始文案）。 */
  reason: string;
  /** 是否已有一份可恢复的存档。 */
  hasSnapshot: boolean;
  /** 存档文件大小（字节）。 */
  snapshotBytes: number;
  /** 存档创建时间（Unix 秒）。 */
  snapshotAt: number;
  /**
   * 任务阶段：`idle` / `savingPausing` / `savingWriting` /
   * `restoring` / `restoringResuming`。
   *
   * 存/恢复是异步链（save = pause→落盘；restore = load→resume），命令立即返回，
   * 前端靠轮询这个字段显示进度。
   */
  phase: SnapshotPhase;
  /** 最近一次快照失败原因；成功或未开始时为空。 */
  error: string;
}

export type SnapshotPhase =
  | 'idle'
  | 'savingPausing'
  | 'savingWriting'
  | 'restoring'
  | 'restoringResuming';

/** 是否为「正在存档/恢复」的中间阶段。 */
export function isSnapshotBusy(phase: SnapshotPhase | undefined): boolean {
  return Boolean(phase && phase !== 'idle');
}
