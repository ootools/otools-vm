<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { Close, Connection, Warning } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';
import type { HostInterface, NetworkBackendStatus, NetworkMode, PortForward } from '@/types/vm';

/**
 * 网络管理：网络模式、宿主网卡、端口转发。
 *
 * 端口转发从设备管理迁到这里——它本质是网络配置，且**只在 NAT 模式下有意义**
 * （桥接 / 仅主机模式的客户机本来就有独立地址，不需要宿主代转）。
 */
export interface NetworkConfig {
  network: NetworkMode;
  /** 桥接 / 仅主机用的宿主网卡标识；NAT 下为空。 */
  networkIface: string;
  portForwards: PortForward[];
}

const model = defineModel<NetworkConfig>({ required: true });

const props = defineProps<{ vmId?: string }>();

const interfaces = ref<HostInterface[]>([]);
const backend = ref<NetworkBackendStatus | null>(null);
const loading = ref(false);

/** 当前模式的能力探测结果；拿不到时按「可用」处理，交由启动时的校验兜底。 */
const modeStatus = (mode: NetworkMode) =>
  backend.value?.modes.find((item) => item.mode === mode);

const modeAvailable = (mode: NetworkMode): boolean => {
  const status = modeStatus(mode);
  return status ? status.available : true;
};

const modeReason = (mode: NetworkMode): string => modeStatus(mode)?.reason ?? '';

const needsInterface = computed(
  () => modeStatus(model.value.network)?.needsInterface ?? model.value.network !== 'nat',
);

const isUserNet = computed(() => model.value.network === 'nat');

const smbAvailable = computed(() => backend.value?.smb.available ?? true);
const smbReason = computed(() => backend.value?.smb.reason ?? '');

/** 宿主在客户机里的地址：NAT 下是固定的 10.0.2.2，其余模式取所选网卡的 IPv4。 */
const hostAddress = computed(() => {
  if (isUserNet.value) {
    return '10.0.2.2';
  }
  const picked = interfaces.value.find((item) => item.id === model.value.networkIface);
  return picked?.ipv4 ?? '';
});

const load = async (): Promise<void> => {
  loading.value = true;
  try {
    const [list, probe] = await Promise.all([
      VmApi.listNetworkInterfaces(),
      VmApi.getNetworkBackend(props.vmId),
    ]);
    interfaces.value = list;
    backend.value = probe;
  } catch {
    // 探测失败不阻塞配置：用户仍可手动填网卡标识，配置由 native 校验兜底。
    interfaces.value = [];
    backend.value = null;
  } finally {
    loading.value = false;
  }
};

onMounted(load);
watch(() => props.vmId, load);

const patch = (next: Partial<NetworkConfig>): void => {
  model.value = { ...model.value, ...next };
};

const setNetwork = (value: NetworkMode): void => {
  // 切回 NAT 时清掉网卡选择：留着会让下次切到桥接时拿到一个可能已失效的网卡名。
  patch({ network: value, networkIface: value === 'nat' ? '' : model.value.networkIface });
};

// ---------------- 端口转发（仅 NAT） ----------------

const draft = ref({ protocol: 'tcp', hostPort: '', guestPort: '' });

const parsePort = (raw: string): number | null => {
  const value = Number(raw.trim());
  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    return null;
  }
  return value;
};

const addForward = (): void => {
  const hostPort = parsePort(draft.value.hostPort);
  const guestPort = parsePort(draft.value.guestPort);
  if (hostPort === null || guestPort === null) {
    ElMessage.warning(t('vm.device.portRange'));
    return;
  }
  const exists = model.value.portForwards.some(
    (item) => item.protocol === draft.value.protocol && item.hostPort === hostPort,
  );
  if (exists) {
    ElMessage.warning(t('vm.device.portDuplicate'));
    return;
  }
  const rule: PortForward = {
    protocol: draft.value.protocol,
    hostPort,
    guestPort,
    bindHost: '127.0.0.1',
  };
  patch({ portForwards: [...model.value.portForwards, rule] });
  draft.value = { protocol: 'tcp', hostPort: '', guestPort: '' };
};

const removeForward = (index: number): void => {
  patch({
    portForwards: model.value.portForwards.filter((_, position) => position !== index),
  });
};
</script>

<template>
  <div class="vm-net">
    <div class="vm-net-block">
      <label>{{ t('vm.net.mode') }}</label>
      <el-radio-group
        :model-value="model.network"
        size="small"
        @update:model-value="(value: string | number | boolean) => setNetwork(String(value) as NetworkMode)"
      >
        <el-radio-button value="nat">{{ t('vm.net.modeNat') }}</el-radio-button>
        <el-radio-button value="bridged" :disabled="!modeAvailable('bridged')">
          {{ t('vm.net.modeBridged') }}
        </el-radio-button>
        <el-radio-button value="hostOnly" :disabled="!modeAvailable('hostOnly')">
          {{ t('vm.net.modeHostOnly') }}
        </el-radio-button>
      </el-radio-group>

      <p class="vm-net-hint">
        {{
          model.network === 'nat'
            ? t('vm.net.modeNatHint')
            : model.network === 'bridged'
              ? t('vm.net.modeBridgedHint')
              : t('vm.net.modeHostOnlyHint')
        }}
      </p>

      <p v-if="!modeAvailable(model.network) && modeReason(model.network)" class="vm-net-warn">
        <el-icon><Warning /></el-icon>
        <span>{{ modeReason(model.network) }}</span>
      </p>
    </div>

    <div v-if="needsInterface" class="vm-net-block">
      <label>{{ t('vm.net.interface') }}</label>
      <el-select
        :model-value="model.networkIface"
        size="small"
        filterable
        allow-create
        default-first-option
        class="vm-net-iface"
        :placeholder="t('vm.net.interfacePlaceholder')"
        @update:model-value="(value: string) => patch({ networkIface: value })"
      >
        <el-option
          v-for="item in interfaces"
          :key="item.id"
          :value="item.id"
          :label="item.label"
          :disabled="!item.bridgeable"
        >
          <span class="vm-net-opt-name">{{ item.label }}</span>
          <span class="vm-net-opt-ip">{{ item.ipv4 }}</span>
        </el-option>
      </el-select>
      <p v-if="!interfaces.length && !loading" class="vm-net-hint">
        {{ t('vm.net.interfaceEmpty') }}
      </p>
      <p
        v-else-if="interfaces.find((item) => item.id === model.networkIface)?.wireless"
        class="vm-net-warn"
      >
        <el-icon><Warning /></el-icon>
        <span>{{ t('vm.net.interfaceWireless') }}</span>
      </p>
      <p v-else class="vm-net-hint">{{ t('vm.net.interfaceHint') }}</p>
    </div>

    <div class="vm-net-block">
      <label>{{ t('vm.device.portForwards') }}</label>
      <p v-if="!isUserNet" class="vm-net-hint">{{ t('vm.net.portForwardNatOnly') }}</p>
      <template v-else>
        <div v-if="!model.portForwards.length" class="vm-net-empty">
          {{ t('vm.device.portForwardsEmpty') }}
        </div>
        <div
          v-for="(rule, index) in model.portForwards"
          :key="`${rule.protocol}-${rule.hostPort}`"
          class="vm-net-row"
        >
          <span class="vm-net-mono">
            {{ rule.protocol }} {{ rule.bindHost }}:{{ rule.hostPort }} → :{{ rule.guestPort }}
          </span>
          <el-button :icon="Close" size="small" text @click="removeForward(index)" />
        </div>
        <div class="vm-net-fwd">
          <el-select v-model="draft.protocol" size="small" class="vm-net-fwd-proto">
            <el-option value="tcp" label="TCP" />
            <el-option value="udp" label="UDP" />
          </el-select>
          <el-input
            v-model="draft.hostPort"
            size="small"
            class="vm-net-fwd-port"
            :placeholder="t('vm.device.portHost')"
          />
          <span class="vm-net-arrow">→</span>
          <el-input
            v-model="draft.guestPort"
            size="small"
            class="vm-net-fwd-port"
            :placeholder="t('vm.device.portGuest')"
          />
          <el-button size="small" type="primary" text @click="addForward">
            {{ t('vm.action.add') }}
          </el-button>
        </div>
        <p class="vm-net-hint">{{ t('vm.device.portForwardsHint') }}</p>
      </template>
    </div>

    <div class="vm-net-block">
      <label class="vm-net-label-inline">
        <el-icon><Connection /></el-icon>
        <span>{{ t('vm.net.hostShare') }}</span>
        <el-button
          size="small"
          text
          :loading="loading"
          class="vm-net-refresh"
          @click="load"
        >
          {{ t('vm.action.refresh') }}
        </el-button>
      </label>
      <p v-if="!smbAvailable" class="vm-net-warn">
        <el-icon><Warning /></el-icon>
        <span>{{ smbReason }}</span>
      </p>
      <template v-else>
        <p class="vm-net-hint">{{ t('vm.net.hostShareHint') }}</p>
        <div v-if="hostAddress" class="vm-net-path">
          <span class="vm-net-mono">\\{{ hostAddress }}\&lt;{{ t('vm.share.smbName') }}&gt;</span>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.vm-net {
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 100%;
}

.vm-net-block {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.vm-net-block > label {
  font-size: 12px;
  color: var(--el-text-color-regular);
}

.vm-net-label-inline {
  display: flex;
  align-items: center;
  gap: 4px;
}

.vm-net-refresh {
  margin-left: auto;
}

.vm-net-empty {
  font-size: 11px;
  color: var(--el-text-color-secondary);
  padding: 6px 8px;
  border-radius: 6px;
  background: var(--el-fill-color-light);
}

.vm-net-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 6px;
  border-radius: 6px;
  border: 1px solid var(--el-border-color-lighter);
}

.vm-net-mono {
  flex: 1;
  min-width: 0;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  color: var(--el-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.vm-net-fwd {
  display: flex;
  align-items: center;
  gap: 6px;
}

.vm-net-fwd-proto {
  width: 76px;
  flex: none;
}

.vm-net-fwd-port {
  width: 84px;
  flex: none;
}

.vm-net-arrow {
  flex: none;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}

.vm-net-iface {
  width: 100%;
}

.vm-net-opt-name {
  margin-right: 8px;
}

.vm-net-opt-ip {
  color: var(--el-text-color-secondary);
  font-size: 11px;
}

.vm-net-path {
  padding: 6px 8px;
  border-radius: 6px;
  background: var(--el-fill-color-light);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  color: var(--el-text-color-primary);
}

.vm-net-hint {
  margin: 2px 0 0;
  font-size: 11px;
  line-height: 1.5;
  color: var(--el-text-color-secondary);
}

.vm-net-warn {
  display: flex;
  align-items: flex-start;
  gap: 4px;
  margin: 2px 0 0;
  font-size: 11px;
  line-height: 1.5;
  color: var(--el-color-warning);
}
</style>
