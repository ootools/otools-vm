<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { t } from '@/i18n';
import { isPluginRuntime, isTauriRuntime } from '@/sdk/runtime';
import { MIN_HOST_VERSION, vmStore } from '@/state/vm-store';
import VmCloneDialog from '@/components/VmCloneDialog.vue';
import VmConsole from '@/components/VmConsole.vue';
import VmCreateDialog from '@/components/VmCreateDialog.vue';
import VmResizeDiskDialog from '@/components/VmResizeDiskDialog.vue';
import VmSettingsPanel from '@/components/VmSettingsPanel.vue';
import VmSidebar from '@/components/VmSidebar.vue';
import VmToolbar from '@/components/VmToolbar.vue';
import VmWelcomeView from '@/components/VmWelcomeView.vue';

const createVisible = ref(false);
const settingsVisible = ref(false);
const cloneVisible = ref(false);
const resizeVisible = ref(false);
// 左侧栏是否已收起（给控制台让出更大空间）；收起后工具栏左侧图标可点开切回。
const sidebarCollapsed = ref(false);
const selected = computed(() => vmStore.selected.value);

/**
 * 当前有没有「能展开侧栏的入口」。
 *
 * 展开按钮长在 `VmToolbar` 上，而工具栏只在选中虚拟机时才渲染。所以：
 *   - 欢迎页（没有选中任何机器）→ 工具栏不存在 → 侧栏一旦收起就再也打不开；
 *   - 选中机器后收起、再把那台机器删掉 → `selected` 变 null，工具栏消失，
 *     同样卡死。
 * 这两种情况都必须禁止收起，否则用户只能重开面板。
 */
const canCollapseSidebar = computed(() => Boolean(selected.value));

/** 宿主缺 virtualization 权限（版本过旧）。 */
const needsHostUpgrade = vmStore.needsHostUpgrade;

/**
 * 浏览器里没有 native 库，虚拟机功能不可能工作。
 * 这里只在「加载失败 + 不在任何宿主运行时」时判定为不可用，
 * 避免宿主桥尚未注入的那一瞬间闪一下错误页。
 */
const desktopOnly = computed(
  () => Boolean(vmStore.state.error) && !isPluginRuntime() && !isTauriRuntime(),
);

let pollTimer = 0;

const refresh = async (): Promise<void> => {
  await vmStore.load();
  if (vmStore.state.error && !desktopOnly.value) {
    ElMessage.error(vmStore.state.error);
  }
};

const poll = (): void => {
  if (typeof document !== 'undefined' && document.hidden) {
    return;
  }
  if (vmStore.state.busy || createVisible.value || cloneVisible.value || resizeVisible.value) {
    return;
  }
  // 挂起/继续期间不要插手刷新：store 自己在轮询任务进度，且这两个操作会
  // 连续改变机器状态（running → pausing → paused → …）。外部再并发刷新会让
  // 状态来回抖，控制台跟着 attach/detach 反复挂载，造成遮挡和闪烁。
  if (vmStore.snapshotBusy.value) {
    return;
  }
  void vmStore.refreshStates();
};

const requestDelete = async (): Promise<void> => {
  const vm = selected.value;
  if (!vm) {
    return;
  }
  try {
    await ElMessageBox.confirm(
      t('vm.delete.confirmDetail', { name: vm.name }),
      t('vm.delete.confirm'),
      { type: 'warning', confirmButtonText: t('vm.action.delete'), cancelButtonText: t('vm.action.cancel') },
    );
  } catch {
    return;
  }
  const removed = await vmStore.remove(vm.id);
  if (!removed) {
    ElMessage.error(vmStore.state.lastActionError || t('vm.error.action'));
  }
};

onMounted(async () => {
  await refresh();
  pollTimer = window.setInterval(poll, 2000);
});

/**
 * 兜底：没有选中项时强制展开侧栏。
 *
 * 只靠「禁止收起」不够——用户可能先收起、再删除正在看的那台机器，此时
 * 收起状态已经存在，工具栏又随 `selected` 一起消失，就再也展不开了。
 * 这里在选中项变空的那一刻把侧栏放回来。
 */
watch(selected, (value) => {
  if (!value && sidebarCollapsed.value) {
    sidebarCollapsed.value = false;
  }
});

onBeforeUnmount(() => {
  if (pollTimer) {
    window.clearInterval(pollTimer);
    pollTimer = 0;
  }
});
</script>

<template>
  <el-config-provider size="small">
    <div v-if="desktopOnly" class="vm-blocked">
      <span class="vm-blocked-icon">🖥️</span>
      <p>{{ t('vm.error.desktopOnly') }}</p>
    </div>

    <div v-else class="vm-shell">
      <VmSidebar
        v-if="!sidebarCollapsed"
        :selected-id="vmStore.state.selectedId"
        :can-collapse="canCollapseSidebar"
        @select="vmStore.select"
        @create="createVisible = true"
        @refresh="refresh"
        @settings="settingsVisible = true"
        @toggle-collapse="sidebarCollapsed = true"
      />

      <section class="vm-main">
        <!-- 宿主版本过旧时，一进来就告诉他原因，而不是等点了启动才报错。 -->
        <el-alert
          v-if="needsHostUpgrade"
          class="vm-host-alert"
          type="warning"
          :closable="false"
          show-icon
          :title="t('vm.engine.needsUpgrade')"
          :description="t('vm.error.hostOutdated', { version: MIN_HOST_VERSION })"
        />
        <VmToolbar
          v-if="selected"
          :vm="selected"
          :collapsed="sidebarCollapsed"
          @settings="settingsVisible = true"
          @delete="requestDelete"
          @clone="cloneVisible = true"
          @resize-disk="resizeVisible = true"
          @toggle-sidebar="sidebarCollapsed = false"
        />
        <VmConsole v-if="selected" :key="selected.id" :vm="selected" />
        <VmWelcomeView v-else @create="createVisible = true" />
      </section>
    </div>

    <VmCreateDialog v-model="createVisible" />
    <VmCloneDialog v-model="cloneVisible" :vm="selected" />
    <VmResizeDiskDialog v-model="resizeVisible" :vm="selected" />
    <VmSettingsPanel v-model="settingsVisible" :vm="selected" />
  </el-config-provider>
</template>

<style>
html,
body,
#app {
  height: 100%;
  margin: 0;
  background: var(--el-bg-color);
}

body {
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Microsoft YaHei',
    sans-serif;
  color: var(--el-text-color-primary);
  -webkit-font-smoothing: antialiased;
}
</style>

<style scoped>
.vm-shell {
  display: flex;
  height: 100vh;
  overflow: hidden;
}

.vm-blocked {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  height: 100vh;
  padding: 32px;
  text-align: center;
  color: var(--el-text-color-secondary);
}

.vm-blocked-icon {
  font-size: 32px;
  opacity: 0.6;
}

.vm-blocked p {
  max-width: 420px;
  margin: 0;
  font-size: 13px;
  line-height: 1.7;
}

.vm-host-alert {
  margin: 10px 12px 0;
  flex: none;
}

.vm-main {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  background: var(--el-bg-color-page);
}
</style>
