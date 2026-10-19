<script setup lang="ts">
import { computed } from 'vue';
import { ElMessage } from 'element-plus';
import {
  CopyDocument,
  Delete,
  Expand,
  Setting,
  SwitchButton,
  VideoPause,
  VideoPlay,
} from '@element-plus/icons-vue';
import { t } from '@/i18n';
import { vmStore } from '@/state/vm-store';
import type { VmSummary } from '@/types/vm';
import { isBusyState, osIcon, stateTagType } from '@/utils/vm-display';

const props = defineProps<{ vm: VmSummary; collapsed?: boolean }>();
const emit = defineEmits<{
  (event: 'settings'): void;
  (event: 'delete'): void;
  (event: 'clone'): void;
  (event: 'resize-disk'): void;
  (event: 'toggle-sidebar'): void;
}>();

const busy = computed(() => vmStore.state.busy || isBusyState(props.vm.state));
const canStart = computed(() => props.vm.state === 'stopped' || props.vm.state === 'error');
const canStop = computed(
  () => props.vm.state === 'running' || props.vm.state === 'paused' || props.vm.state === 'pausing',
);
// 「暂停」= 挂起：把整机存盘并停住，关 App / 重启 Mac 都不丢。
const canSuspend = computed(
  () =>
    Boolean(vmStore.state.engine?.caps.snapshots) &&
    props.vm.state === 'running' &&
    vmStore.snapshotSupport.value?.supported !== false,
);
// 「继续」= 从挂起继续。两种情形都算：
//   paused  → 实例还在内存里，直接 resume；
//   stopped → App 重启过，从存档 restore 再 resume（需要存在存档文件）。
const canContinue = computed(() => {
  if (!vmStore.state.engine?.caps.snapshots) {
    return false;
  }
  if (vmStore.snapshotSupport.value?.supported === false) {
    return false;
  }
  if (props.vm.state === 'paused') {
    return true;
  }
  return (
    (props.vm.state === 'stopped' || props.vm.state === 'error') &&
    Boolean(vmStore.snapshotSupport.value?.hasSnapshot)
  );
});
// 克隆与扩容都要求磁盘处于一致状态，运行中交给 native 再挡一道。
const canClone = computed(() => canStart.value && Boolean(vmStore.state.engine?.caps.clone));
const canResizeDisk = computed(
  () => canStart.value && Boolean(vmStore.state.engine?.caps.resizeDisk),
);

const snapshot = computed(() => vmStore.snapshotSupport.value);
const snapshotBusy = computed(() => vmStore.snapshotBusy.value);
/** 有存档文件 = 挂起过；用于提示「上次挂起」时间。 */
const snapshotAt = computed(() => snapshot.value?.snapshotAt ?? 0);
const suspendTitle = computed(() => {
  if (!canSuspend.value) {
    return t('vm.snapshot.needRunning');
  }
  if (snapshot.value?.supported === false) {
    return `${t('vm.snapshot.unsupported')}${snapshot.value.reason ? `：${snapshot.value.reason}` : ''}`;
  }
  const at = snapshotAt.value;
  if (at > 0) {
    return `${t('vm.snapshot.saveHint')}（${t('vm.snapshot.lastAt')} ${new Date(at * 1000).toLocaleString()}）`;
  }
  return t('vm.snapshot.saveHint');
});

const run = async (action: () => Promise<boolean>) => {
  await action();
};

/**
 * 挂起 / 继续。
 *
 * 两者都是**异步链**（挂起 = pause→落盘；继续 = load→resume），store 内部会
 * 轮询到任务结束再返回，所以这里只需给用户成功/失败反馈。
 */
const suspend = async () => {
  const done = await vmStore.saveState(props.vm.id);
  if (done) {
    ElMessage.success(t('vm.snapshot.saved'));
  } else {
    ElMessage.error(vmStore.state.lastActionError || t('vm.snapshot.failed'));
  }
};

const continueRun = async () => {
  const done = await vmStore.continueRun(props.vm.id);
  if (done) {
    ElMessage.success(t('vm.snapshot.restored'));
  } else {
    ElMessage.error(vmStore.state.lastActionError || t('vm.snapshot.failed'));
  }
};
</script>

<template>
  <header class="vm-toolbar">
    <div class="vm-toolbar-title">
      <el-button
        v-if="props.collapsed"
        class="vm-toolbar-menu"
        :icon="Expand"
        circle
        size="small"
        :title="t('vm.nav.expand')"
        @click="emit('toggle-sidebar')"
      />
      <span class="vm-toolbar-icon">{{ osIcon(vm.os) }}</span>
      <h2 class="vm-toolbar-name">{{ vm.name }}</h2>
      <span class="vm-toolbar-sub">
        {{ vm.arch }} · {{ vm.cpus }} {{ t('vm.unit.cpu') }} · {{ vm.memoryMb }} {{ t('vm.unit.mb') }}
      </span>
      <el-tag :type="stateTagType(vm.state)" effect="light" size="small">
        {{ t(`vm.state.${vm.state}`) }}
      </el-tag>
    </div>

    <div class="vm-toolbar-actions">
      <el-button
        v-if="canStart"
        type="primary"
        size="small"
        :icon="VideoPlay"
        :loading="busy"
        @click="run(() => vmStore.start(vm.id))"
      >
        {{ t('vm.action.start') }}
      </el-button>
      <!-- 挂起：把整机状态（含内存）写盘并停住；关 App / 重启 Mac 都不丢 -->
      <el-button
        v-if="canSuspend"
        type="warning"
        size="small"
        :icon="VideoPause"
        :loading="snapshotBusy"
        :title="suspendTitle"
        @click="suspend"
      >
        {{ t('vm.snapshot.save') }}
      </el-button>
      <!-- 继续：从挂起状态回到运行（内存里还在就直接跑，App 重启过就从存档载回） -->
      <el-button
        v-if="canContinue"
        type="success"
        size="small"
        :icon="VideoPlay"
        :loading="snapshotBusy"
        :title="t('vm.snapshot.restoreHint')"
        @click="continueRun"
      >
        {{ t('vm.snapshot.restore') }}
      </el-button>
      <el-button
        v-if="canStop"
        size="small"
        :icon="SwitchButton"
        :loading="busy"
        @click="run(() => vmStore.stop(vm.id))"
      >
        {{ t('vm.action.stop') }}
      </el-button>
      <el-button
        size="small"
        :icon="CopyDocument"
        circle
        :disabled="!canClone"
        :title="t('vm.action.clone')"
        @click="emit('clone')"
      />
      <el-button
        size="small"
        :icon="Expand"
        circle
        :disabled="!canResizeDisk"
        :title="t('vm.action.resizeDisk')"
        @click="emit('resize-disk')"
      />
      <el-button size="small" :icon="Setting" circle @click="emit('settings')" />
      <el-button size="small" :icon="Delete" circle @click="emit('delete')" />
    </div>
  </header>
</template>

<style scoped>
.vm-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 6px 14px;
  border-bottom: 1px solid var(--el-border-color-lighter);
  background: var(--el-bg-color);
}

.vm-toolbar-title {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.vm-toolbar-icon {
  font-size: 17px;
  line-height: 1;
}

.vm-toolbar-name {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
  color: var(--el-text-color-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  min-width: 0;
}

.vm-toolbar-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  min-width: 0;
}

.vm-toolbar-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: none;
}
</style>
