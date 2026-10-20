<script setup lang="ts">
import { computed, ref } from 'vue';
import { ArrowUp, ArrowDown, Close, Plus } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';

/** 设备管理：启动顺序、U 盘介质。网络相关（端口转发）在 VmNetworkManager。 */
export interface DeviceConfig {
  bootOrder: string[];
  usbMedia: string;
}

const model = defineModel<DeviceConfig>({ required: true });

/** 与 native 侧 `BOOT_DEVICES` 保持一致。 */
const ALL_BOOT_ITEMS = ['disk', 'cdrom', 'usb', 'net'] as const;

const BOOT_LABEL_KEYS: Record<string, string> = {
  disk: 'vm.device.bootDisk',
  cdrom: 'vm.device.bootCdrom',
  usb: 'vm.device.bootUsb',
  net: 'vm.device.bootNet',
};

const bootLabel = (key: string): string => t(BOOT_LABEL_KEYS[key] ?? key);

const patch = (next: Partial<DeviceConfig>): void => {
  model.value = { ...model.value, ...next };
};

// ---------------- 启动顺序 ----------------

const moveBoot = (index: number, offset: number): void => {
  const list = [...model.value.bootOrder];
  const target = index + offset;
  if (target < 0 || target >= list.length) {
    return;
  }
  [list[index], list[target]] = [list[target], list[index]];
  patch({ bootOrder: list });
};

const removeBoot = (index: number): void => {
  patch({ bootOrder: model.value.bootOrder.filter((_, position) => position !== index) });
};

const remainingBootItems = computed(() =>
  ALL_BOOT_ITEMS.filter((item) => !model.value.bootOrder.includes(item)),
);

const pendingBoot = ref('');
const addBoot = (): void => {
  const value = pendingBoot.value;
  if (!value || model.value.bootOrder.includes(value)) {
    return;
  }
  patch({ bootOrder: [...model.value.bootOrder, value] });
  pendingBoot.value = '';
};

// ---------------- U 盘介质 ----------------

const pickUsbMedia = async (): Promise<void> => {
  try {
    const picked = await VmApi.pickFile(t('vm.device.usbMedia'));
    if (picked?.path) {
      patch({ usbMedia: picked.path });
    }
  } catch {
    // 非 macOS 宿主没有原生面板；输入框本身可编辑，粘贴绝对路径即可。
    ElMessage.info(t('vm.action.manualPathPlaceholder'));
  }
};

</script>

<template>
  <div class="vm-device">
    <div class="vm-device-block">
      <label>{{ t('vm.device.bootOrder') }}</label>
      <div v-if="!model.bootOrder.length" class="vm-device-empty">
        {{ t('vm.device.bootOrderEmpty') }}
      </div>
      <div v-for="(item, index) in model.bootOrder" :key="item" class="vm-device-row">
        <span class="vm-device-index">{{ index + 1 }}</span>
        <span class="vm-device-name">{{ bootLabel(item) }}</span>
        <el-button
          :icon="ArrowUp"
          size="small"
          text
          :disabled="index === 0"
          @click="moveBoot(index, -1)"
        />
        <el-button
          :icon="ArrowDown"
          size="small"
          text
          :disabled="index === model.bootOrder.length - 1"
          @click="moveBoot(index, 1)"
        />
        <el-button :icon="Close" size="small" text @click="removeBoot(index)" />
      </div>
      <div v-if="remainingBootItems.length" class="vm-device-add">
        <el-select v-model="pendingBoot" size="small" :placeholder="t('vm.device.addBootItem')">
          <el-option
            v-for="item in remainingBootItems"
            :key="item"
            :value="item"
            :label="bootLabel(item)"
          />
        </el-select>
        <el-button size="small" type="primary" text :disabled="!pendingBoot" @click="addBoot">
          {{ t('vm.action.add') }}
        </el-button>
      </div>
      <p class="vm-device-hint">{{ t('vm.device.bootOrderHint') }}</p>
    </div>

    <div class="vm-device-block">
      <label>{{ t('vm.device.usbMedia') }}</label>
      <div class="vm-device-file">
        <el-input
          :model-value="model.usbMedia"
          size="small"
          :placeholder="t('vm.device.usbMediaPlaceholder')"
          @update:model-value="(value: string) => patch({ usbMedia: value })"
        />
        <el-button size="small" @click="pickUsbMedia">{{ t('vm.action.pickInstaller') }}</el-button>
        <el-button v-if="model.usbMedia" size="small" text @click="patch({ usbMedia: '' })">
          {{ t('vm.action.remove') }}
        </el-button>
      </div>
      <p class="vm-device-hint">{{ t('vm.device.usbMediaHint') }}</p>
    </div>

  </div>
</template>

<style scoped>
.vm-device {
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 100%;
}

.vm-device-block {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.vm-device-block > label {
  font-size: 12px;
  color: var(--el-text-color-regular);
}

.vm-device-empty {
  font-size: 11px;
  color: var(--el-text-color-secondary);
  padding: 6px 8px;
  border-radius: 6px;
  background: var(--el-fill-color-light);
}

.vm-device-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 6px;
  border-radius: 6px;
  border: 1px solid var(--el-border-color-lighter);
}

.vm-device-index {
  width: 18px;
  height: 18px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  background: var(--el-color-primary-light-8);
  color: var(--el-color-primary);
  font-size: 10px;
  font-weight: 600;
}

.vm-device-name {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  color: var(--el-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.vm-device-add {
  display: flex;
  align-items: center;
  gap: 6px;
}

.vm-device-add .el-select {
  flex: 1;
}

.vm-device-file {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
}

.vm-device-hint {
  margin: 2px 0 0;
  font-size: 11px;
  line-height: 1.5;
  color: var(--el-text-color-secondary);
}
</style>
