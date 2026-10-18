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
  rosetta: boolean;
  bridgedNetwork: boolean;
}

export interface EngineInfo {
  kind: 'vz' | 'qemu' | 'none';
  available: boolean;
  label: string;
  reason: string;
  version: string;
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
  installerPath?: string | null;
  notes?: string;
}
