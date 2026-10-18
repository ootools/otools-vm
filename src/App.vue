<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { t } from '@/i18n';
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

let pollTimer = 0;

const refresh = async (): Promise<void> => {
  await vmStore.load();
  if (vmStore.state.error) {
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
    <div class="vm-shell">
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

.vm-main {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  background: var(--el-bg-color-page);
}
</style>
