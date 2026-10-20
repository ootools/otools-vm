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

/** NAT 端口转发：把宿主端口映射进客户机。只在 nat 网络下有效。 */
export interface PortForward {
  protocol: string;
  hostPort: number;
  guestPort: number;
  /** 宿主绑定地址，默认 127.0.0.1（只监听回环，不暴露到局域网）。 */
  bindHost: string;
}

/** 网络模式：nat（用户态 NAT）/ bridged（桥接到局域网）/ hostOnly（仅宿主可达）。 */
export type NetworkMode = 'nat' | 'bridged' | 'hostOnly';

/** 宿主上一张可选的网卡。 */
export interface HostInterface {
  /** 传给 QEMU 的标识：Windows 是适配器名，其余是接口名。 */
  id: string;
  /** 展示用名称。 */
  label: string;
  /** 当前 IPv4 地址，空表示没拿到。 */
  ipv4: string;
  /** 是否像一张可桥接的物理网卡。 */
  bridgeable: boolean;
  /** 是否无线；桥接无线网卡有吞吐抖动，UI 提示优先选有线。 */
  wireless: boolean;
}

/** 单个网络模式在当前宿主的可用性。 */
export interface NetworkModeStatus {
  mode: NetworkMode;
  available: boolean;
  /** 不可用时的可操作原因。 */
  reason: string;
  /** 该模式是否需要指定网卡。 */
  needsInterface: boolean;
}

/** 网络后端与宿主原生共享的探测结果。 */
export interface NetworkBackendStatus {
  modes: NetworkModeStatus[];
  smb: { available: boolean; reason: string };
  /** 指定虚拟机视角下宿主在客户机里的地址（NAT 下固定 10.0.2.2）。 */
  hostAddress: string;
}

export interface SharedDir {
  hostPath: string;
  mountTag: string;
  /** 共享协议：9p（默认）/ virtiofs / smb。 */
  protocol: string;
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
  /** 固件类型：bios（默认，SeaBIOS）或 uefi（OVMF）。 */
  firmware: string;
  /** UEFI 固件文件路径；留空按平台常见位置自动探测。 */
  firmwarePath: string | null;
  /** VirtIO 驱动 ISO 路径（virtio-win 等），Windows 安装「加载驱动」用。 */
  driverIso: string | null;
  /** virtiofsd 可执行文件路径；留空按常见安装位置自动探测。仅 virtio-fs 协议需要。 */
  virtiofsdPath: string | null;
  network: NetworkMode;
  /** 桥接 / 仅主机模式用的宿主网卡标识；NAT 下忽略。 */
  networkIface: string | null;
  /** 启动顺序：disk / cdrom / usb / net 的有序列表。 */
  bootOrder: string[];
  /** U 盘介质（RAW 磁盘镜像）路径。 */
  usbMedia: string | null;
  /** NAT 端口转发规则。 */
  portForwards: PortForward[];
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
  /**
   * 该后端能否输出客户机画面（内嵌视图或独立窗口都算）。
   *
   * `false` 表示**任何**画面都拿不到，不只是不能内嵌 —— HCS 就是这种：
   * 它没有 framebuffer 接口。前端据此隐藏控制台入口并说明原因。
   */
  displayOutput: boolean;
}

/** HCS 引擎的可用性详情（仅 Windows 有意义）。 */
export interface HcsStatus {
  supported: boolean;
  available: boolean;
  /** true 表示「缺权限」而不是「缺组件」，两者引导方式不同。 */
  needsElevation: boolean;
  elevated: boolean;
  inHyperVAdmins: boolean;
  reason: string;
  version: string;
}

/**
 * 引擎偏好。
 *
 * `auto` 按平台默认（macOS 用 VZ、其它用 QEMU）。`hcs` 只在 Windows 有意义：
 * 它性能更好但**没有画面输出**，所以必须显式选择。
 */
export type EnginePreference = 'auto' | 'hcs' | 'qemu';

export interface VmSettings {
  enginePreference: EnginePreference;
}

/**
 * QEMU 一键获取的方案。
 *
 * 由 native 端按本机实际可用的途径挑一条（winget / 官方安装器 / Homebrew），
 * 前端只负责展示，不自己判断平台能力——判断逻辑分两处必然漂移。
 */
export interface QemuPlan {
  /** 是否支持一键获取。false 时只能给用户命令让他自己装。 */
  automatic: boolean;
  strategy: QemuInstallStrategy;
  /** 需要用户配合的事（例如「会弹 UAC，请点允许」）。 */
  note: string;
  /** 手工安装时给用户复制走的命令或链接。 */
  command: string;
  /** 为什么选这条路。 */
  reason: string;
  /** 当前平台是否在内置支持范围内（用于区分「不支持」与「装不上」）。 */
  supportedPlatform: boolean;
  /** 我们期望的 QEMU 可执行文件路径。 */
  binaryPath: string;
}

export type QemuInstallStrategy = 'winget' | 'installer' | 'homebrew' | 'manual';

/** QEMU 获取进度。命令立即返回，靠轮询这个结构显示进度。 */
export interface QemuInstallStatus {
  phase: 'idle' | 'downloading' | 'installing' | 'done' | 'failed' | string;
  strategy: QemuInstallStrategy | string;
  /** 0 ~ 1。总量未知时为 0。 */
  progress: number;
  downloaded: number;
  total: number;
  error: string;
  /** 成功后 QEMU 可执行文件的绝对路径。 */
  binary: string;
}

/** 某个目录所在卷的可用空间。 */
export interface DiskSpace {
  path: string;
  /** 取不到时为 null（不应据此判定「空间不足」）。 */
  availableBytes: number | null;
}

/** 是否处于安装中（下载也算）。 */
export function isQemuInstallBusy(status: QemuInstallStatus | null | undefined): boolean {
  return status?.phase === 'downloading' || status?.phase === 'installing';
}


/** Rosetta 在宿主上的可用状态。 */
export type RosettaStatus = 'installed' | 'notInstalled' | 'notSupported';

export interface EngineInfo {
  kind: 'vz' | 'qemu' | 'hcs' | 'none';
  available: boolean;
  label: string;
  reason: string;
  version: string;
  /** 宿主进程是否具备 macOS 虚拟化权限；false 说明宿主版本过旧。 */
  hostEntitled: boolean;
  /** 当前实际生效的加速器（whpx/hvf/kvm/tcg）；未探测到为 null。 */
  accelerator: string | null;
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
  network?: NetworkMode;
  networkIface?: string | null;
  installerPath?: string | null;
  firmware?: string;
  firmwarePath?: string | null;
  driverIso?: string | null;
  virtiofsdPath?: string | null;
  bootOrder?: string[];
  usbMedia?: string | null;
  portForwards?: PortForward[];
  sharedDirs: SharedDir[];
  clipboard?: boolean;
  autoResize?: boolean;
  audio?: boolean;
  rosetta?: boolean;
  displayWidth?: number;
  displayHeight?: number;
  /**
   * 磁盘存放目录（绝对路径）。留空则放在默认数据目录。
   *
   * **只有磁盘会搬**：`vm.json`、NVRAM、日志仍在默认目录，因为虚拟机列表是
   * 靠扫默认目录发现的；把元数据一起搬走会让机器从列表里消失。
   */
  storageDir?: string | null;
}

export interface VmUpdateInput {
  id: string;
  name: string;
  cpus: number;
  memoryMb: number;
  network?: NetworkMode;
  networkIface?: string | null;
  sharedDirs: SharedDir[];
  clipboard?: boolean;
  autoResize?: boolean;
  audio?: boolean;
  rosetta?: boolean;
  installerPath?: string | null;
  firmware?: string;
  firmwarePath?: string | null;
  driverIso?: string | null;
  virtiofsdPath?: string | null;
  bootOrder?: string[];
  usbMedia?: string | null;
  portForwards?: PortForward[];
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
