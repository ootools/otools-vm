<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { pickPath } from '@/sdk/runtime';
import { VmApi } from '@/services/vm-api';
import { vmStore } from '@/state/vm-store';
import type { GuestOs, NetworkMode, PortForward, SharedDir, VmCreateInput } from '@/types/vm';
import SharedDirEditor from './SharedDirEditor.vue';
import VmDeviceManager, { type DeviceConfig } from './VmDeviceManager.vue';
import VmNetworkManager, { type NetworkConfig } from './VmNetworkManager.vue';

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
  firmware: string;
  firmwarePath: string;
  driverIso: string;
  network: NetworkMode;
  networkIface: string;
  bootOrder: string[];
  usbMedia: string;
  portForwards: PortForward[];
  sharedDirs: SharedDir[];
  clipboard: boolean;
  autoResize: boolean;
  audio: boolean;
  rosetta: boolean;
  storageDir: string;
}>({
  name: '',
  os: 'linux',
  arch: 'arm64',
  cpus: 4,
  memoryMb: 4096,
  diskSizeGb: 40,
  installerPath: '',
  firmware: 'bios',
  firmwarePath: '',
  driverIso: '',
  network: 'nat',
  networkIface: '',
  bootOrder: ['disk', 'cdrom', 'net'],
  usbMedia: '',
  portForwards: [],
  sharedDirs: [],
  clipboard: true,
  autoResize: true,
  audio: true,
  rosetta: false,
  storageDir: '',
});

const osOptions = computed(() => [
  { value: 'windows' as GuestOs, label: t('vm.create.osWindows') },
  { value: 'linux' as GuestOs, label: t('vm.create.osLinux') },
  { value: 'macos' as GuestOs, label: t('vm.create.osMacos') },
  { value: 'other' as GuestOs, label: t('vm.create.osOther') },
]);

// Rosetta 只对 Linux 客户机开放，且宿主必须已经装好。
const rosettaSupported = computed(
  () => Boolean(vmStore.state.engine?.caps.rosetta) && form.os === 'linux',
);
const rosettaInstalled = computed(() => vmStore.state.rosetta === 'installed');
const rosettaHint = computed(() => {
  if (!rosettaInstalled.value) {
    return t('vm.rosetta.notInstalled');
  }
  return t('vm.rosetta.hint');
});

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
  form.firmware = 'bios';
  form.firmwarePath = '';
  form.driverIso = '';
  form.network = 'nat';
  form.networkIface = '';
  form.bootOrder = ['disk', 'cdrom', 'net'];
  form.usbMedia = '';
  form.portForwards = [];
  form.sharedDirs = [];
  form.clipboard = true;
  form.autoResize = true;
  form.audio = true;
  form.rosetta = false;
  form.storageDir = '';
};

watch(visible, (open) => {
  if (open) {
    reset();
    void refreshFreeSpace();
  }
});

// 安装介质常见后缀；宿主对话框按此过滤，减少选错文件。
const IMAGE_EXTENSIONS = ['iso', 'img', 'raw', 'qcow2', 'vmdk', 'vdi', 'vhdx'];

const pickInstaller = async (): Promise<void> => {
  try {
    const picked = await pickPath({
      title: t('vm.create.installer'),
      filters: [{ name: t('vm.action.imageFilter'), extensions: IMAGE_EXTENSIONS }],
    });
    if (picked) {
      form.installerPath = picked;
    }
  } catch {
    // 连宿主对话框都拿不到（例如纯浏览器直开）：提示改用粘贴。
    ElMessage.info(t('vm.action.manualPathPlaceholder'));
  }
};

// 固件 / 驱动 ISO 只对 QEMU 后端有意义（VZ 走 Apple 自己的 EFI）。
const isQemu = computed(() => vmStore.state.engine?.kind === 'qemu');

/**
 * 选择磁盘存放目录。
 *
 * 为什么需要这个：默认目录在用户主目录下，而虚拟机动辄几十 GB——系统盘
 * 常常先满。让用户在创建时就指定数据盘，比事后搬家简单得多。
 *
 * 只搬磁盘：`vm.json`/NVRAM/日志必须留在默认目录，否则虚拟机列表扫不到它。
 */
const pickStorageDir = async (): Promise<void> => {
  try {
    const picked = await pickPath({
      title: t('vm.create.storageDir'),
      directory: true,
    });
    if (picked) {
      form.storageDir = picked;
    }
  } catch {
    ElMessage.info(t('vm.action.manualPathPlaceholder'));
  }
};

/**
 * 存储目录的可用空间。
 *
 * 只在目录有值时才问，且失败静默：查不到空间不该阻止用户创建虚拟机。
 * 用户真正填不下时，native 侧建盘前也会拦一次并给出明确数字。
 */
const freeSpaceGb = ref<number | null>(null);

const refreshFreeSpace = async (): Promise<void> => {
  try {
    const space = await VmApi.getDiskSpace(form.storageDir.trim() || undefined);
    freeSpaceGb.value =
      space.availableBytes === null ? null : Math.floor(space.availableBytes / 1024 ** 3);
  } catch {
    freeSpaceGb.value = null;
  }
};

watch(() => form.storageDir, () => void refreshFreeSpace());

/** 磁盘容量与可用空间的对比结论；null 表示信息不足，不显示提示。 */
const spaceHint = computed(() => {
  if (freeSpaceGb.value === null) {
    return '';
  }
  if (freeSpaceGb.value < form.diskSizeGb) {
    return t('vm.create.storageLow', {
      free: String(freeSpaceGb.value),
      need: String(form.diskSizeGb),
    });
  }
  return t('vm.create.storageFree', { free: String(freeSpaceGb.value) });
});

const pickDriverIso = async (): Promise<void> => {
  try {
    const picked = await pickPath({
      title: t('vm.create.driverIso'),
      filters: [{ name: t('vm.action.imageFilter'), extensions: IMAGE_EXTENSIONS }],
    });
    if (picked) {
      form.driverIso = picked;
    }
  } catch {
    ElMessage.info(t('vm.action.manualPathPlaceholder'));
  }
};

// 设备 / 网络各捆成一个对象模型，与设置面板共用同一批组件。
const deviceConfig = computed<DeviceConfig>({
  get: () => ({
    bootOrder: form.bootOrder,
    usbMedia: form.usbMedia,
  }),
  set: (value) => {
    form.bootOrder = value.bootOrder;
    form.usbMedia = value.usbMedia;
  },
});

const networkConfig = computed<NetworkConfig>({
  get: () => ({
    network: form.network,
    networkIface: form.networkIface,
    portForwards: form.portForwards,
  }),
  set: (value) => {
    form.network = value.network;
    form.networkIface = value.networkIface;
    form.portForwards = value.portForwards;
  },
});

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
    firmware: form.firmware,
    firmwarePath: form.firmwarePath || null,
    driverIso: form.driverIso || null,
    network: form.network,
    networkIface: form.networkIface || null,
    bootOrder: form.bootOrder,
    usbMedia: form.usbMedia || null,
    portForwards: form.portForwards,
    sharedDirs: form.sharedDirs,
    clipboard: form.clipboard,
    autoResize: form.autoResize,
    audio: form.audio,
    rosetta: form.rosetta,
    displayWidth: 1280,
    displayHeight: 800,
    storageDir: form.storageDir.trim() || null,
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

      <el-form-item :label="t('vm.create.storageDir')">
        <div class="vm-form-file">
          <el-input
            v-model="form.storageDir"
            :placeholder="t('vm.create.storageDirPlaceholder')"
          />
          <el-button @click="pickStorageDir">{{ t('vm.action.pickFolder') }}</el-button>
          <el-button v-if="form.storageDir" text @click="form.storageDir = ''">
            {{ t('vm.action.reset') }}
          </el-button>
        </div>
        <p v-if="spaceHint" class="vm-form-hint" :class="{ 'vm-form-warn': freeSpaceGb !== null && freeSpaceGb < form.diskSizeGb }">
          {{ spaceHint }}
        </p>
        <p v-else class="vm-form-hint">{{ t('vm.create.storageDirHint') }}</p>
      </el-form-item>

      <el-form-item :label="t('vm.create.installer')">
        <div class="vm-form-file">
          <el-input v-model="form.installerPath" :placeholder="t('vm.create.installerNone')" />
          <el-button @click="pickInstaller">{{ t('vm.action.pickInstaller') }}</el-button>
          <el-button v-if="form.installerPath" text @click="form.installerPath = ''">
            {{ t('vm.action.remove') }}
          </el-button>
        </div>
        <p class="vm-form-hint">{{ t('vm.create.installerHint') }}</p>
      </el-form-item>

      <el-form-item v-if="isQemu" :label="t('vm.create.firmware')">
        <el-radio-group v-model="form.firmware">
          <el-radio-button value="bios">{{ t('vm.create.firmwareBios') }}</el-radio-button>
          <el-radio-button value="uefi">{{ t('vm.create.firmwareUefi') }}</el-radio-button>
        </el-radio-group>
        <p class="vm-form-hint">{{ t('vm.create.firmwareHint') }}</p>
      </el-form-item>

      <el-form-item v-if="isQemu && form.firmware === 'uefi'" :label="t('vm.create.firmwarePath')">
        <el-input v-model="form.firmwarePath" :placeholder="t('vm.create.firmwarePathPlaceholder')" />
      </el-form-item>

      <el-form-item v-if="isQemu && form.os === 'windows'" :label="t('vm.create.driverIso')">
        <div class="vm-form-file">
          <el-input v-model="form.driverIso" :placeholder="t('vm.create.installerNone')" />
          <el-button @click="pickDriverIso">{{ t('vm.action.pickInstaller') }}</el-button>
          <el-button v-if="form.driverIso" text @click="form.driverIso = ''">
            {{ t('vm.action.remove') }}
          </el-button>
        </div>
        <p class="vm-form-hint">{{ t('vm.create.driverIsoHint') }}</p>
      </el-form-item>

      <el-form-item v-if="rosettaSupported" :label="t('vm.rosetta.label')">
        <div class="vm-form-toggle">
          <el-switch v-model="form.rosetta" :disabled="!rosettaInstalled" />
          <span class="vm-form-hint">{{ rosettaHint }}</span>
        </div>
      </el-form-item>

      <el-form-item v-if="isQemu" :label="t('vm.net.title')">
        <VmNetworkManager v-model="networkConfig" />
      </el-form-item>

      <el-form-item v-if="isQemu" :label="t('vm.device.title')">
        <VmDeviceManager v-model="deviceConfig" />
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

.vm-form-toggle {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
}

.vm-form-toggle .vm-form-hint {
  margin: 0;
}

.vm-form-file {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
}

.vm-form-warn {
  color: var(--el-color-danger);
}
</style>
