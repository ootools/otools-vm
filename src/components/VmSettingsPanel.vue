<script setup lang="ts">
import { computed, reactive, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { openHostPath } from '@/sdk/runtime';
import { vmStore } from '@/state/vm-store';
import type { SharedDir, VmSummary, VmUpdateInput } from '@/types/vm';
import SharedDirEditor from './SharedDirEditor.vue';

const props = defineProps<{ vm: VmSummary | null }>();
const visible = defineModel<boolean>({ required: true });

const host = computed(() => vmStore.state.host);
const saving = computed(() => vmStore.state.busy);

const form = reactive<{
  name: string;
  cpus: number;
  memoryMb: number;
  sharedDirs: SharedDir[];
  clipboard: boolean;
  autoResize: boolean;
  audio: boolean;
  installerPath: string;
  notes: string;
}>({
  name: '',
  cpus: 4,
  memoryMb: 4096,
  sharedDirs: [],
  clipboard: true,
  autoResize: true,
  audio: true,
  installerPath: '',
  notes: '',
});

const maxCpus = computed(() => Math.max(2, host.value?.recommendedCpus ?? 4));
const maxMemory = computed(() => Math.max(2048, host.value?.recommendedMemoryMb ?? 8192));

watch(
  () => [visible.value, props.vm?.id],
  () => {
    const vm = props.vm;
    if (!visible.value || !vm) {
      return;
    }
    form.name = vm.name;
    form.cpus = vm.cpus;
    form.memoryMb = vm.memoryMb;
    form.sharedDirs = vm.sharedDirs.map((dir) => ({ ...dir }));
    form.clipboard = vm.clipboard;
    form.autoResize = vm.autoResize;
    form.audio = vm.audio;
    form.installerPath = vm.installerPath ?? '';
    form.notes = vm.notes;
  },
  { immediate: true },
);

const save = async (): Promise<void> => {
  const vm = props.vm;
  if (!vm) {
    return;
  }
  const input: VmUpdateInput = {
    id: vm.id,
    name: form.name.trim() || vm.name,
    cpus: form.cpus,
    memoryMb: form.memoryMb,
    sharedDirs: form.sharedDirs,
    clipboard: form.clipboard,
    autoResize: form.autoResize,
    audio: form.audio,
    installerPath: form.installerPath || null,
    notes: form.notes,
  };
  const done = await vmStore.update(input);
  if (done) {
    ElMessage.success(t('vm.settings.saved'));
    visible.value = false;
  } else {
    ElMessage.error(vmStore.state.lastActionError || t('vm.error.action'));
  }
};

const revealDisk = (): void => {
  if (props.vm?.diskPath) {
    openHostPath(props.vm.diskPath);
  }
};
</script>

<template>
  <el-drawer v-model="visible" :title="t('vm.settings.title')" size="420px" append-to-body>
    <div v-if="vm" class="vm-settings">
      <section class="vm-settings-section">
        <h4>{{ t('vm.create.name') }}</h4>
        <el-input v-model="form.name" size="small" maxlength="40" />
        <p class="vm-settings-hint vm-settings-path" :title="vm.diskPath" @click="revealDisk">
          {{ vm.diskPath }}
        </p>
      </section>

      <section class="vm-settings-section">
        <h4>{{ t('vm.settings.resources') }}</h4>
        <div class="vm-settings-field">
          <label>{{ t('vm.create.cpus') }} · {{ form.cpus }}</label>
          <el-slider v-model="form.cpus" :min="1" :max="maxCpus" :step="1" size="small" />
        </div>
        <div class="vm-settings-field">
          <label>{{ t('vm.create.memory') }} · {{ form.memoryMb }} {{ t('vm.unit.mb') }}</label>
          <el-slider v-model="form.memoryMb" :min="1024" :max="maxMemory" :step="512" size="small" />
        </div>
      </section>

      <section class="vm-settings-section">
        <h4>{{ t('vm.settings.integration') }}</h4>
        <div class="vm-settings-toggle">
          <div>
            <span>{{ t('vm.settings.clipboard') }}</span>
            <p class="vm-settings-hint">{{ t('vm.settings.clipboardHint') }}</p>
          </div>
          <el-switch v-model="form.clipboard" size="small" />
        </div>
        <div class="vm-settings-toggle">
          <div>
            <span>{{ t('vm.settings.autoResize') }}</span>
            <p class="vm-settings-hint">{{ t('vm.settings.autoResizeHint') }}</p>
          </div>
          <el-switch v-model="form.autoResize" size="small" />
        </div>
        <div class="vm-settings-toggle">
          <div>
            <span>{{ t('vm.settings.audio') }}</span>
          </div>
          <el-switch v-model="form.audio" size="small" />
        </div>
      </section>

      <section class="vm-settings-section">
        <h4>{{ t('vm.settings.sharing') }}</h4>
        <SharedDirEditor v-model="form.sharedDirs" />
        <p v-if="form.sharedDirs.length" class="vm-settings-hint vm-settings-code">
          mount -t virtiofs {{ form.sharedDirs[0].mountTag }} /mnt/{{ form.sharedDirs[0].mountTag }}
        </p>
      </section>

      <section class="vm-settings-section">
        <h4>{{ t('vm.create.installer') }}</h4>
        <el-input v-model="form.installerPath" size="small" :placeholder="t('vm.create.installerNone')" />
        <p class="vm-settings-hint">{{ t('vm.create.installerHint') }}</p>
      </section>

      <section class="vm-settings-section">
        <h4>{{ t('vm.settings.notes') }}</h4>
        <el-input v-model="form.notes" type="textarea" :rows="3" size="small" />
      </section>
    </div>

    <template #footer>
      <el-button size="small" @click="visible = false">{{ t('vm.action.cancel') }}</el-button>
      <el-button size="small" type="primary" :loading="saving" @click="save">
        {{ t('vm.action.save') }}
      </el-button>
    </template>
  </el-drawer>
</template>

<style scoped>
.vm-settings {
  display: flex;
  flex-direction: column;
  gap: 18px;
}

.vm-settings-section h4 {
  margin: 0 0 8px;
  font-size: 12px;
  font-weight: 600;
  color: var(--el-text-color-secondary);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.vm-settings-field {
  margin-bottom: 10px;
}

.vm-settings-field label {
  display: block;
  margin-bottom: 2px;
  font-size: 12px;
  color: var(--el-text-color-regular);
}

.vm-settings-toggle {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  padding: 6px 0;
}

.vm-settings-toggle span {
  font-size: 12px;
  color: var(--el-text-color-primary);
}

.vm-settings-hint {
  margin: 3px 0 0;
  font-size: 11px;
  line-height: 1.5;
  color: var(--el-text-color-secondary);
}

.vm-settings-path {
  cursor: pointer;
  word-break: break-all;
}

.vm-settings-path:hover {
  color: var(--el-color-primary);
}

.vm-settings-code {
  padding: 6px 8px;
  border-radius: 6px;
  background: var(--el-fill-color-light);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  word-break: break-all;
}
</style>
