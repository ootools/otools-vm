import type { VmState } from '@/types/vm';

export const stateColor = (state: VmState): string => {
  switch (state) {
    case 'running':
      return '#67c23a';
    case 'starting':
    case 'resuming':
    case 'pausing':
    case 'stopping':
      return '#e6a23c';
    case 'paused':
      return '#409eff';
    case 'error':
      return '#f56c6c';
    case 'unavailable':
      return '#909399';
    default:
      return '#c0c4cc';
  }
};

export const stateTagType = (state: VmState): 'success' | 'warning' | 'danger' | 'info' | 'primary' => {
  switch (state) {
    case 'running':
      return 'success';
    case 'starting':
    case 'resuming':
    case 'pausing':
    case 'stopping':
      return 'warning';
    case 'error':
      return 'danger';
    case 'paused':
      return 'primary';
    default:
      return 'info';
  }
};

export const isBusyState = (state: VmState): boolean =>
  state === 'starting' || state === 'resuming' || state === 'pausing' || state === 'stopping';

export const isActiveState = (state: VmState): boolean =>
  state === 'starting' ||
  state === 'running' ||
  state === 'paused' ||
  state === 'pausing' ||
  state === 'resuming' ||
  state === 'stopping';

export const formatGb = (gb: number): string => `${gb} GB`;

export const formatDate = (seconds: number): string => {
  if (!seconds) {
    return '—';
  }
  const date = new Date(seconds * 1000);
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(
    date.getDate(),
  ).padStart(2, '0')}`;
};

export const osIcon = (os: string): string => {
  switch (os) {
    case 'windows':
      return '🪟';
    case 'linux':
      return '🐧';
    case 'macos':
      return '🍎';
    default:
      return '💿';
  }
};
