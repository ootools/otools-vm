<script setup lang="ts">
import { computed } from 'vue';
import { Plus } from '@element-plus/icons-vue';
import { t } from '@/i18n';
import { vmStore } from '@/state/vm-store';

const emit = defineEmits<{ (event: 'create'): void }>();

const engine = computed(() => vmStore.state.engine);
const host = computed(() => vmStore.state.host);

const caps = computed(() => {
  const value = engine.value?.caps;
  if (!value) {
    return [];
  }
  return [
    { label: t('vm.welcome.capShared'), on: value.sharedDirs },
    { label: t('vm.welcome.capClipboard'), on: value.clipboard },
    { label: t('vm.welcome.capResize'), on: value.autoResize },
    { label: t('vm.welcome.capAudio'), on: value.audio },
  ];
});
</script>

<template>
  <div class="vm-welcome">
    <div class="vm-welcome-inner">
      <span class="vm-welcome-badge">OTools VM</span>
      <h2>{{ t('vm.welcome.title') }}</h2>
      <p class="vm-welcome-lead">{{ t('vm.welcome.lead') }}</p>

      <div class="vm-welcome-create">
        <div>
          <h3>{{ t('vm.welcome.createTitle') }}</h3>
          <p>{{ t('vm.welcome.createDesc') }}</p>
        </div>
        <el-button type="primary" :icon="Plus" @click="emit('create')">
          {{ t('vm.action.create') }}
        </el-button>
      </div>

      <div class="vm-welcome-cards">
        <div class="vm-welcome-card">
          <h4>{{ t('vm.welcome.engineTitle') }}</h4>
          <p class="vm-welcome-engine">
            {{ engine?.available ? engine.label : t('vm.engine.unavailable') }}
            <span v-if="engine?.version">· {{ engine.version }}</span>
          </p>
          <p v-if="engine?.reason" class="vm-welcome-reason">{{ engine.reason }}</p>
          <p v-if="host" class="vm-welcome-host">
            {{ host.os }} / {{ host.arch }} · {{ host.totalMemoryMb }} {{ t('vm.unit.mb') }}
          </p>
        </div>

        <div class="vm-welcome-card">
          <h4>{{ t('vm.welcome.capsTitle') }}</h4>
          <ul class="vm-welcome-caps">
            <li v-for="cap in caps" :key="cap.label" :class="{ off: !cap.on }">
              <span class="vm-cap-mark">{{ cap.on ? '✓' : '—' }}</span>
              {{ cap.label }}
            </li>
          </ul>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.vm-welcome {
  flex: 1;
  overflow: auto;
  padding: 28px 24px;
}

.vm-welcome-inner {
  max-width: 640px;
  margin: 0 auto;
}

.vm-welcome-badge {
  display: inline-block;
  padding: 2px 8px;
  border-radius: 999px;
  background: var(--el-color-primary-light-9);
  color: var(--el-color-primary);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.04em;
}

.vm-welcome h2 {
  margin: 12px 0 8px;
  font-size: 22px;
  font-weight: 650;
  color: var(--el-text-color-primary);
}

.vm-welcome-lead {
  margin: 0 0 20px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--el-text-color-regular);
}

.vm-welcome-create {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 16px;
  border-radius: 10px;
  border: 1px solid var(--el-border-color-lighter);
  background: var(--el-fill-color-blank);
}

.vm-welcome-create h3 {
  margin: 0 0 4px;
  font-size: 13px;
  font-weight: 600;
  color: var(--el-text-color-primary);
}

.vm-welcome-create p {
  margin: 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-text-color-secondary);
}

.vm-welcome-cards {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
  margin-top: 12px;
}

.vm-welcome-card {
  padding: 14px;
  border-radius: 10px;
  border: 1px solid var(--el-border-color-lighter);
  background: var(--el-fill-color-blank);
}

.vm-welcome-card h4 {
  margin: 0 0 8px;
  font-size: 12px;
  font-weight: 600;
  color: var(--el-text-color-secondary);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.vm-welcome-engine {
  margin: 0;
  font-size: 13px;
  color: var(--el-text-color-primary);
}

.vm-welcome-reason {
  margin: 6px 0 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-color-warning);
}

.vm-welcome-host {
  margin: 6px 0 0;
  font-size: 11px;
  color: var(--el-text-color-secondary);
}

.vm-welcome-caps {
  margin: 0;
  padding: 0;
  list-style: none;
}

.vm-welcome-caps li {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  line-height: 1.9;
  color: var(--el-text-color-regular);
}

.vm-welcome-caps li.off {
  color: var(--el-text-color-disabled);
}

.vm-cap-mark {
  width: 12px;
  text-align: center;
  color: var(--el-color-success);
}

.vm-welcome-caps li.off .vm-cap-mark {
  color: var(--el-text-color-disabled);
}
</style>
