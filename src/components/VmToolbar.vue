<script setup lang="ts">
import { computed } from 'vue';
import {
  CopyDocument,
  Delete,
  Expand,
  RefreshLeft,
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
const canPause = computed(() => props.vm.state === 'running');
const canResume = computed(() => props.vm.state === 'paused');
// 克隆与扩容都要求磁盘处于一致状态，运行中交给 native 再挡一道。
const canClone = computed(() => canStart.value && Boolean(vmStore.state.engine?.caps.clone));
const canResizeDisk = computed(
  () => canStart.value && Boolean(vmStore.state.engine?.caps.resizeDisk),
);

const run = async (action: () => Promise<boolean>) => {
  await action();
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
      <el-button
        v-if="canPause"
        size="small"
        :icon="VideoPause"
        :loading="busy"
        @click="run(() => vmStore.pause(vm.id))"
      >
        {{ t('vm.action.pause') }}
      </el-button>
      <el-button
        v-if="canResume"
        size="small"
        :icon="RefreshLeft"
        :loading="busy"
        @click="run(() => vmStore.resume(vm.id))"
      >
        {{ t('vm.action.resume') }}
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
