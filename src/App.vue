<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { t } from '@/i18n';
import { isPluginRuntime, isTauriRuntime } from '@/sdk/runtime';
import { vmStore } from '@/state/vm-store';
import VmConsole from '@/components/VmConsole.vue';
import VmCreateDialog from '@/components/VmCreateDialog.vue';
import VmSettingsPanel from '@/components/VmSettingsPanel.vue';
import VmSidebar from '@/components/VmSidebar.vue';
import VmToolbar from '@/components/VmToolbar.vue';
import VmWelcomeView from '@/components/VmWelcomeView.vue';

const createVisible = ref(false);
const settingsVisible = ref(false);
const selected = computed(() => vmStore.selected.value);

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
  if (vmStore.state.busy || createVisible.value) {
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
        :selected-id="vmStore.state.selectedId"
        @select="vmStore.select"
        @create="createVisible = true"
        @refresh="refresh"
        @settings="settingsVisible = true"
      />

      <section class="vm-main">
        <VmToolbar
          v-if="selected"
          :vm="selected"
          @settings="settingsVisible = true"
          @delete="requestDelete"
        />
        <VmConsole v-if="selected" :key="selected.id" :vm="selected" />
        <VmWelcomeView v-else @create="createVisible = true" />
      </section>
    </div>

    <VmCreateDialog v-model="createVisible" />
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

.vm-main {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  background: var(--el-bg-color-page);
}
</style>
