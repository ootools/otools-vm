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

/**
 * 是否该**保留原生 console 视图**（即客户机有实时画面可看）。
 *
 * ⚠️ `paused` 故意不算 active：VZ 的 console 视图是宿主窗口级、盖在 webview
 * 之上的原生覆盖层，挂起时客户机图形设备被冻结/挂起，这一层会留下残影甚至
 * 变成整块黑屏，把插件自己的工具栏和别的 TAB 一起盖住。所以挂起后一律摘掉
 * 原生视图，改由 HTML 显示「已挂起」占位；点「继续」回到 running 再挂回来。
 *
 * `stopping` 仍算 active：它只存在极短时间，保留最后一帧让关机过程不闪。
 */
export const isActiveState = (state: VmState): boolean =>
  state === 'starting' ||
  state === 'running' ||
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
