<script setup lang="ts">
import { computed, reactive, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { openHostPath } from '@/sdk/runtime';
import { vmStore } from '@/state/vm-store';
import type { NetworkMode, PortForward, SharedDir, VmSummary, VmUpdateInput } from '@/types/vm';
import SharedDirEditor from './SharedDirEditor.vue';
import VmDeviceManager, { type DeviceConfig } from './VmDeviceManager.vue';
import VmNetworkManager, { type NetworkConfig } from './VmNetworkManager.vue';

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
  rosetta: boolean;
  installerPath: string;
  firmware: string;
  firmwarePath: string;
  driverIso: string;
  virtiofsdPath: string;
  network: NetworkMode;
  networkIface: string;
  bootOrder: string[];
  usbMedia: string;
  portForwards: PortForward[];
  notes: string;
}>({
  name: '',
  cpus: 4,
  memoryMb: 4096,
  sharedDirs: [],
  clipboard: true,
  autoResize: true,
  audio: true,
  rosetta: false,
  installerPath: '',
  firmware: 'bios',
  firmwarePath: '',
  driverIso: '',
  virtiofsdPath: '',
  network: 'nat',
  networkIface: '',
  bootOrder: ['disk', 'cdrom', 'net'],
  usbMedia: '',
  portForwards: [],
  notes: '',
});

const maxCpus = computed(() => Math.max(2, host.value?.recommendedCpus ?? 4));
const maxMemory = computed(() => Math.max(2048, host.value?.recommendedMemoryMb ?? 8192));

/**
 * 运行中只允许调内存（HCS 支持热改，其它后端会明确拒绝）。
 *
 * 这条判断必须和 native 侧 `cmd_update` 的规则一致：那边按「除内存外是否
 * 还有变更」判定，这里据此禁用其余控件。两边不一致会让用户改完一屏才被
 * 告知「请先关机」。
 */
const live = computed(() => {
  const state = props.vm?.state;
  return state === 'running' || state === 'paused';
});
/** 只有 HCS 真的支持热调内存；其它引擎运行中一律只读。 */
const liveMemory = computed(
  () => live.value && vmStore.state.engine?.kind === 'hcs',
);

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
    form.rosetta = vm.rosetta;
    form.installerPath = vm.installerPath ?? '';
    form.firmware = vm.firmware ?? 'bios';
    form.firmwarePath = vm.firmwarePath ?? '';
    form.driverIso = vm.driverIso ?? '';
    form.virtiofsdPath = vm.virtiofsdPath ?? '';
    form.network = vm.network ?? 'nat';
    form.networkIface = vm.networkIface ?? '';
    form.bootOrder = [...vm.bootOrder];
    form.usbMedia = vm.usbMedia ?? '';
    form.portForwards = vm.portForwards.map((rule) => ({ ...rule }));
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
    rosetta: form.rosetta,
    installerPath: form.installerPath || null,
    firmware: form.firmware,
    firmwarePath: form.firmwarePath || null,
    driverIso: form.driverIso || null,
    virtiofsdPath: form.virtiofsdPath || null,
    network: form.network,
    networkIface: form.networkIface || null,
    bootOrder: form.bootOrder,
    usbMedia: form.usbMedia || null,
    portForwards: form.portForwards,
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

// Rosetta 只对 Linux 客户机有意义；宿主装没装决定开关能不能开。
const rosettaSupported = computed(
  () => Boolean(vmStore.state.engine?.caps.rosetta) && props.vm?.os === 'linux',
);
const rosettaInstalled = computed(() => vmStore.state.rosetta === 'installed');
const rosettaHint = computed(() => {
  if (!rosettaSupported.value) {
    return t('vm.rosetta.notSupported');
  }
  if (!rosettaInstalled.value) {
    return t('vm.rosetta.notInstalled');
  }
  return t('vm.rosetta.hint');
});

// 固件 / 驱动 ISO 只对 QEMU 后端有意义。
const isQemu = computed(() => vmStore.state.engine?.kind === 'qemu');

// 共享目录的挂载命令随协议变：9p / virtio-fs / SMB 的客户机侧写法完全不同。
const usesVirtiofs = computed(() => form.sharedDirs.some((dir) => dir.protocol === 'virtiofs'));
const mountHint = computed(() => {
  const first = form.sharedDirs[0];
  if (!first) {
    return '';
  }
  if (first.protocol === 'virtiofs') {
    return t('vm.console.mountHintVirtiofs', { tag: first.mountTag });
  }
  if (first.protocol === 'smb') {
    return t('vm.console.mountHintSmb', { name: first.mountTag, host: smbHostAddress.value });
  }
  return t('vm.console.mountHint', { tag: first.mountTag });
});

// 宿主在客户机里的地址：NAT 下是 QEMU 约定的 10.0.2.2，其余模式取所选网卡地址。
const smbHostAddress = computed(() => {
  if (form.network === 'nat') {
    return '10.0.2.2';
  }
  return form.networkIface || t('vm.net.hostAddressManual');
});

// 设备管理捆成一个对象模型，避免组件为了复用拆成多个 prop。
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

// 网络配置同理：模式 / 网卡 / 端口转发三项同属网络语义。
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

const revealDisk = (): void => {
  if (props.vm?.diskPath) {
    openHostPath(props.vm.diskPath);
  }
};
</script>

<template>
  <el-drawer v-model="visible" :title="t('vm.settings.title')" size="420px" append-to-body>
    <div v-if="vm" class="vm-settings">
      <!--
        运行中锁定除内存以外的所有配置。
        用 `inert` 而不是逐控件写 `:disabled`：表单字段有二十多个且会继续增加，
        逐个加必然漏，漏掉的那个就是「改完才被告知要关机」。
        `inert` 会让整棵子树不可聚焦、不可点击，且不影响视觉排版。
      -->
      <div class="vm-settings-name" :inert="live">
        <section class="vm-settings-section">
          <h4>{{ t('vm.create.name') }}</h4>
          <el-input v-model="form.name" size="small" maxlength="40" />
          <p class="vm-settings-hint vm-settings-path" :title="vm.diskPath" @click="revealDisk">
            {{ vm.diskPath }}
          </p>
        </section>
      </div>

      <section class="vm-settings-section">
        <h4>{{ t('vm.settings.resources') }}</h4>
        <el-alert
          v-if="live"
          class="vm-settings-live"
          :type="liveMemory ? 'info' : 'warning'"
          :closable="false"
          :title="liveMemory ? t('vm.settings.liveMemory') : t('vm.settings.liveReadonly')"
        />
        <div class="vm-settings-field">
          <label>{{ t('vm.create.cpus') }} · {{ form.cpus }}</label>
          <el-slider
            v-model="form.cpus"
            :min="1"
            :max="maxCpus"
            :step="1"
            size="small"
            :disabled="live"
          />
        </div>
        <div class="vm-settings-field">
          <label>{{ t('vm.create.memory') }} · {{ form.memoryMb }} {{ t('vm.unit.mb') }}</label>
          <!--
            运行中时内存仍可调（HCS 热改），但只能调大：调小会让客户机里
            已占用的内存无处安放。min 提到当前值即可，不必额外解释。
          -->
          <el-slider
            v-model="form.memoryMb"
            :min="live ? form.memoryMb : 1024"
            :max="maxMemory"
            :step="512"
            size="small"
            :disabled="live && !liveMemory"
          />
        </div>
      </section>

      <div class="vm-settings-rest" :inert="live">
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
          <div class="vm-settings-toggle">
            <div>
              <span>{{ t('vm.rosetta.label') }}</span>
              <p class="vm-settings-hint">{{ rosettaHint }}</p>
            </div>
            <el-switch
              v-model="form.rosetta"
              size="small"
              :disabled="!rosettaSupported || !rosettaInstalled"
            />
          </div>
        </section>

        <section class="vm-settings-section">
          <h4>{{ t('vm.settings.sharing') }}</h4>
          <SharedDirEditor v-model="form.sharedDirs" />
          <p v-if="form.sharedDirs.length" class="vm-settings-hint vm-settings-code">
            {{ mountHint }}
          </p>
          <div v-if="isQemu && usesVirtiofs" class="vm-settings-field">
            <label>{{ t('vm.settings.virtiofsd') }}</label>
            <el-input
              v-model="form.virtiofsdPath"
              size="small"
              :placeholder="t('vm.settings.virtiofsdPlaceholder')"
            />
            <p class="vm-settings-hint">{{ t('vm.settings.virtiofsdHint') }}</p>
          </div>
        </section>

        <section class="vm-settings-section">
          <h4>{{ t('vm.create.installer') }}</h4>
          <el-input v-model="form.installerPath" size="small" :placeholder="t('vm.create.installerNone')" />
          <p class="vm-settings-hint">{{ t('vm.create.installerHint') }}</p>
        </section>

        <section v-if="isQemu" class="vm-settings-section">
          <h4>{{ t('vm.net.title') }}</h4>
          <VmNetworkManager v-model="networkConfig" :vm-id="vm.id" />
        </section>

        <section v-if="isQemu" class="vm-settings-section">
          <h4>{{ t('vm.device.title') }}</h4>
          <VmDeviceManager v-model="deviceConfig" />
        </section>

        <section v-if="isQemu" class="vm-settings-section">
          <h4>{{ t('vm.settings.firmware') }}</h4>
          <div class="vm-settings-field">
            <el-radio-group v-model="form.firmware" size="small">
              <el-radio-button value="bios">{{ t('vm.create.firmwareBios') }}</el-radio-button>
              <el-radio-button value="uefi">{{ t('vm.create.firmwareUefi') }}</el-radio-button>
            </el-radio-group>
            <p class="vm-settings-hint">{{ t('vm.create.firmwareHint') }}</p>
          </div>
          <div v-if="form.firmware === 'uefi'" class="vm-settings-field">
            <label>{{ t('vm.create.firmwarePath') }}</label>
            <el-input
              v-model="form.firmwarePath"
              size="small"
              :placeholder="t('vm.create.firmwarePathPlaceholder')"
            />
          </div>
          <div class="vm-settings-field">
            <label>{{ t('vm.create.driverIso') }}</label>
            <el-input v-model="form.driverIso" size="small" :placeholder="t('vm.create.installerNone')" />
            <p class="vm-settings-hint">{{ t('vm.create.driverIsoHint') }}</p>
          </div>
        </section>

        <section class="vm-settings-section">
          <h4>{{ t('vm.settings.notes') }}</h4>
          <el-input v-model="form.notes" type="textarea" :rows="3" size="small" />
      </section>
      </div>
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

/* 运行中锁定的区块保持渲染，只是不可交互——不要用 opacity 让它看着像坏了。 */
.vm-settings-name,
.vm-settings-rest {
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
