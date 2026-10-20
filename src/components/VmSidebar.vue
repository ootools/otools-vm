<script setup lang="ts">
import { computed } from 'vue';
import { Fold, Plus, RefreshRight, Setting } from '@element-plus/icons-vue';
import { t } from '@/i18n';
import { vmStore } from '@/state/vm-store';
import { formatGb, osIcon, stateColor } from '@/utils/vm-display';

const props = defineProps<{ selectedId: string; canCollapse?: boolean }>();
const emit = defineEmits<{
  (event: 'select', id: string): void;
  (event: 'create'): void;
  (event: 'refresh'): void;
  (event: 'settings'): void;
  (event: 'toggle-collapse'): void;
}>();

const vms = computed(() => vmStore.state.vms);
const engine = computed(() => vmStore.state.engine);

const stateLabel = (state: string): string => t(`vm.state.${state}`);
</script>

<template>
  <aside class="vm-sidebar">
    <header class="vm-sidebar-head">
      <div class="vm-sidebar-title">
        <h1>{{ t('vm.app.title') }}</h1>
        <p>{{ t('vm.app.subtitle') }}</p>
      </div>
      <div class="vm-sidebar-actions">
        <el-button :icon="RefreshRight" circle :loading="vmStore.state.loading" @click="emit('refresh')" />
        <el-button :icon="Plus" circle type="primary" @click="emit('create')" />
      </div>
    </header>

    <div class="vm-engine-chip">
      <span class="vm-engine-dot" :class="{ ready: engine?.available }" />
      <span class="vm-engine-label">
        {{ engine?.available ? engine.label : t('vm.engine.unavailable') }}
      </span>
      <span v-if="engine?.version" class="vm-engine-version">{{ engine.version }}</span>
    </div>

    <el-scrollbar class="vm-sidebar-scroll">
      <div v-if="!vms.length" class="vm-sidebar-empty">
        <p>{{ t('vm.empty.title') }}</p>
        <span>{{ t('vm.empty.lead') }}</span>
      </div>
      <button
        v-for="vm in vms"
        :key="vm.id"
        type="button"
        class="vm-item"
        :class="{ active: vm.id === props.selectedId }"
        @click="emit('select', vm.id)"
      >
        <span class="vm-item-icon">{{ osIcon(vm.os) }}</span>
        <span class="vm-item-main">
          <span class="vm-item-name">{{ vm.name }}</span>
          <span class="vm-item-meta">
            {{ vm.cpus }} {{ t('vm.unit.cpu') }} · {{ formatGb(vm.diskSizeGb) }}
          </span>
        </span>
        <span class="vm-item-state">
          <span class="vm-item-dot" :style="{ background: stateColor(vm.state) }" />
          <span>{{ stateLabel(vm.state) }}</span>
        </span>
      </button>
    </el-scrollbar>

    <footer class="vm-sidebar-foot">
      <el-button :icon="Setting" text size="small" @click="emit('settings')">
        {{ t('vm.nav.settings') }}
      </el-button>
      <span class="vm-sidebar-count">{{ t('vm.list.count', { count: vms.length }) }}</span>
    </footer>

    <!--
      收起把手的隐藏条件：没有可用的「展开」入口时不允许收起。
      展开按钮在工具栏上，而工具栏只在选中虚拟机时出现——欢迎页收起后
      就再没有入口了，用户只能重开面板。所以这里直接不给出这个把手。
    -->
    <el-button
      v-if="props.canCollapse !== false"
      class="vm-sidebar-collapse"
      :icon="Fold"
      circle
      size="small"
      :title="t('vm.nav.collapse')"
      @click="emit('toggle-collapse')"
    />
  </aside>
</template>

<style scoped>
.vm-sidebar {
  display: flex;
  flex-direction: column;
  width: 264px;
  min-width: 264px;
  height: 100%;
  position: relative;
  border-right: 1px solid var(--el-border-color-lighter);
  background: var(--el-bg-color);
}

.vm-sidebar-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 8px;
  padding: 16px 14px 10px;
}

.vm-sidebar-title h1 {
  margin: 0;
  font-size: 15px;
  font-weight: 600;
  color: var(--el-text-color-primary);
}

.vm-sidebar-title p {
  margin: 2px 0 0;
  font-size: 11px;
  color: var(--el-text-color-secondary);
}

.vm-sidebar-actions {
  display: flex;
  gap: 4px;
}

.vm-engine-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 0 14px 10px;
  padding: 5px 8px;
  border-radius: 6px;
  background: var(--el-fill-color-light);
  font-size: 11px;
  color: var(--el-text-color-secondary);
}

.vm-engine-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--el-color-danger);
}

.vm-engine-dot.ready {
  background: var(--el-color-success);
}

.vm-engine-label {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.vm-engine-version {
  opacity: 0.7;
}

.vm-sidebar-scroll {
  flex: 1;
  min-height: 0;
}

.vm-sidebar-empty {
  padding: 24px 16px;
  text-align: center;
  color: var(--el-text-color-secondary);
}

.vm-sidebar-empty p {
  margin: 0 0 6px;
  font-size: 13px;
  font-weight: 500;
  color: var(--el-text-color-regular);
}

.vm-sidebar-empty span {
  font-size: 11px;
  line-height: 1.5;
}

.vm-item {
  display: flex;
  align-items: center;
  gap: 9px;
  width: calc(100% - 16px);
  margin: 2px 8px;
  padding: 8px 9px;
  border: 1px solid transparent;
  border-radius: 8px;
  background: transparent;
  cursor: pointer;
  text-align: left;
  transition: background 0.15s ease;
}

.vm-item:hover {
  background: var(--el-fill-color-light);
}

.vm-item.active {
  background: var(--el-color-primary-light-9);
  border-color: var(--el-color-primary-light-7);
}

.vm-item-icon {
  font-size: 17px;
  line-height: 1;
}

.vm-item-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.vm-item-name {
  font-size: 13px;
  color: var(--el-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.vm-item-meta {
  font-size: 11px;
  color: var(--el-text-color-secondary);
}

.vm-item-state {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 10px;
  color: var(--el-text-color-secondary);
  white-space: nowrap;
}

.vm-item-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
}

.vm-sidebar-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 6px 10px;
  border-top: 1px solid var(--el-border-color-lighter);
}

.vm-sidebar-count {
  font-size: 11px;
  color: var(--el-text-color-secondary);
}

/* 收缩把手：固定在侧栏内部右缘、垂直居中（不探出到控制台，避免被 VM 原生视图盖住）。 */
.vm-sidebar-collapse {
  position: absolute;
  top: 50%;
  right: 8px;
  transform: translateY(-50%);
  z-index: 5;
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.18);
}
</style>
