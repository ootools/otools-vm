<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';
import { vmStore } from '@/state/vm-store';
import type { GuestOs, SharedDir, VmCreateInput } from '@/types/vm';
import SharedDirEditor from './SharedDirEditor.vue';

const visible = defineModel<boolean>({ required: true });

const host = computed(() => vmStore.state.host);
const submitting = ref(false);

const form = reactive<{
  name: string;
  os: GuestOs;
  arch: string;
  cpus: number;
  memoryMb: number;
  diskSizeGb: number;
  installerPath: string;
  sharedDirs: SharedDir[];
  clipboard: boolean;
  autoResize: boolean;
  audio: boolean;
}>({
  name: '',
  os: 'linux',
  arch: 'arm64',
  cpus: 4,
  memoryMb: 4096,
  diskSizeGb: 40,
  installerPath: '',
  sharedDirs: [],
  clipboard: true,
  autoResize: true,
  audio: true,
});

const osOptions = computed(() => [
  { value: 'windows' as GuestOs, label: t('vm.create.osWindows') },
  { value: 'linux' as GuestOs, label: t('vm.create.osLinux') },
  { value: 'macos' as GuestOs, label: t('vm.create.osMacos') },
  { value: 'other' as GuestOs, label: t('vm.create.osOther') },
]);

const maxCpus = computed(() => Math.max(2, host.value?.recommendedCpus ?? 4));
const maxMemory = computed(() => Math.max(2048, host.value?.recommendedMemoryMb ?? 8192));

const reset = (): void => {
  form.name = '';
  form.os = 'linux';
  form.arch = 'arm64';
  form.cpus = Math.min(4, maxCpus.value);
  form.memoryMb = Math.min(4096, maxMemory.value);
  form.diskSizeGb = 40;
  form.installerPath = '';
  form.sharedDirs = [];
  form.clipboard = true;
  form.autoResize = true;
  form.audio = true;
};

watch(visible, (open) => {
  if (open) {
    reset();
  }
});

const pickInstaller = async (): Promise<void> => {
  try {
    const picked = await VmApi.pickFile(t('vm.create.installer'));
    if (picked?.path) {
      form.installerPath = picked.path;
    }
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : String(error));
  }
};

const submit = async (): Promise<void> => {
  if (!form.name.trim()) {
    ElMessage.warning(t('vm.error.nameRequired'));
    return;
  }
  submitting.value = true;
  const input: VmCreateInput = {
    name: form.name.trim(),
    os: form.os,
    arch: form.arch,
    cpus: form.cpus,
    memoryMb: form.memoryMb,
    diskSizeGb: form.diskSizeGb,
    installerPath: form.installerPath || null,
    sharedDirs: form.sharedDirs,
    clipboard: form.clipboard,
    autoResize: form.autoResize,
    audio: form.audio,
    displayWidth: 1280,
    displayHeight: 800,
  };
  const created = await vmStore.create(input);
  submitting.value = false;
  if (!created) {
    ElMessage.error(vmStore.state.lastActionError || t('vm.create.failed'));
    return;
  }
  visible.value = false;
  ElMessage.success(t('vm.create.success'));
  await vmStore.start(created.id);
};
</script>

<template>
  <el-dialog v-model="visible" :title="t('vm.create.title')" width="560px" append-to-body>
    <el-form label-position="top" size="small" class="vm-create-form">
      <el-form-item :label="t('vm.create.name')">
        <el-input v-model="form.name" :placeholder="t('vm.create.namePlaceholder')" maxlength="40" />
      </el-form-item>

      <el-form-item :label="t('vm.create.os')">
        <el-radio-group v-model="form.os">
          <el-radio-button v-for="item in osOptions" :key="item.value" :value="item.value">
            {{ item.label }}
          </el-radio-button>
        </el-radio-group>
      </el-form-item>

      <el-form-item :label="t('vm.create.arch')">
        <el-radio-group v-model="form.arch">
          <el-radio-button value="arm64">arm64</el-radio-button>
          <el-radio-button value="x86_64">x86_64</el-radio-button>
        </el-radio-group>
        <p class="vm-form-hint">{{ t('vm.create.archHint') }}</p>
      </el-form-item>

      <div class="vm-form-grid">
        <el-form-item :label="`${t('vm.create.cpus')} · ${form.cpus}`">
          <el-slider v-model="form.cpus" :min="1" :max="maxCpus" :step="1" show-input />
        </el-form-item>
        <el-form-item :label="`${t('vm.create.memory')} · ${form.memoryMb} ${t('vm.unit.mb')}`">
          <el-slider v-model="form.memoryMb" :min="1024" :max="maxMemory" :step="512" />
        </el-form-item>
      </div>

      <el-form-item :label="`${t('vm.create.disk')} · ${form.diskSizeGb} ${t('vm.unit.gb')}`">
        <el-slider v-model="form.diskSizeGb" :min="10" :max="512" :step="10" />
        <p class="vm-form-hint">{{ t('vm.create.diskHint') }}</p>
      </el-form-item>

      <el-form-item :label="t('vm.create.installer')">
        <div class="vm-form-file">
          <el-input :model-value="form.installerPath" readonly :placeholder="t('vm.create.installerNone')" />
          <el-button @click="pickInstaller">{{ t('vm.action.pickInstaller') }}</el-button>
          <el-button v-if="form.installerPath" text @click="form.installerPath = ''">
            {{ t('vm.action.remove') }}
          </el-button>
        </div>
        <p class="vm-form-hint">{{ t('vm.create.installerHint') }}</p>
      </el-form-item>

      <el-form-item :label="t('vm.create.sharedDirs')">
        <SharedDirEditor v-model="form.sharedDirs" />
      </el-form-item>
    </el-form>

    <template #footer>
      <el-button size="small" @click="visible = false">{{ t('vm.action.cancel') }}</el-button>
      <el-button size="small" type="primary" :loading="submitting" @click="submit">
        {{ t('vm.create.submit') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.vm-create-form {
  max-height: 60vh;
  overflow: auto;
  padding-right: 4px;
}

.vm-form-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.vm-form-hint {
  margin: 4px 0 0;
  font-size: 11px;
  line-height: 1.5;
  color: var(--el-text-color-secondary);
}

.vm-form-file {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
}
</style>
