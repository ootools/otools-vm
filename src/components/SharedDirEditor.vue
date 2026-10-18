<script setup lang="ts">
import { Plus, FolderOpened, Close } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';
import type { SharedDir } from '@/types/vm';

const props = defineProps<{ modelValue: SharedDir[]; disabled?: boolean }>();
const emit = defineEmits<{ (event: 'update:modelValue', value: SharedDir[]): void }>();

/** 与 native 侧 `SharedDir::derive_tag` 保持同一套规则。 */
const deriveTag = (hostPath: string, index: number): string => {
  const base = hostPath.split('/').filter(Boolean).pop() ?? 'share';
  const cleaned = base
    .replace(/[^a-zA-Z0-9]/g, '-')
    .toLowerCase()
    .replace(/^-+|-+$/g, '');
  return cleaned ? `${cleaned}-${index + 1}` : `share${index + 1}`;
};

const update = (next: SharedDir[]): void => {
  emit('update:modelValue', next);
};

const addDirectory = async (): Promise<void> => {
  try {
    const picked = await VmApi.pickFolder(t('vm.action.addSharedDir'));
    if (!picked?.path) {
      return;
    }
    if (props.modelValue.some((item) => item.hostPath === picked.path)) {
      ElMessage.info(picked.path);
      return;
    }
    update([
      ...props.modelValue,
      { hostPath: picked.path, mountTag: deriveTag(picked.path, props.modelValue.length), readOnly: false },
    ]);
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : String(error));
  }
};

const removeAt = (index: number): void => {
  const next = props.modelValue.filter((_, position) => position !== index);
  update(next);
};

const toggleReadOnly = (index: number, value: boolean): void => {
  const next = props.modelValue.map((item, position) =>
    position === index ? { ...item, readOnly: value } : item,
  );
  update(next);
};
</script>

<template>
  <div class="vm-share">
    <div v-if="!modelValue.length" class="vm-share-empty">
      {{ t('vm.create.sharedDirsHint') }}
    </div>
    <div v-for="(dir, index) in modelValue" :key="dir.hostPath" class="vm-share-row">
      <el-icon class="vm-share-icon"><FolderOpened /></el-icon>
      <div class="vm-share-path" :title="dir.hostPath">
        <span class="vm-share-name">{{ dir.hostPath }}</span>
        <span class="vm-share-tag">virtiofs · {{ dir.mountTag }}</span>
      </div>
      <el-switch
        :model-value="dir.readOnly"
        size="small"
        :disabled="disabled"
        :active-text="t('vm.settings.readOnly')"
        @update:model-value="(value: string | number | boolean) => toggleReadOnly(index, Boolean(value))"
      />
      <el-button
        :icon="Close"
        circle
        size="small"
        text
        :disabled="disabled"
        @click="removeAt(index)"
      />
    </div>
    <el-button :icon="Plus" size="small" text type="primary" :disabled="disabled" @click="addDirectory">
      {{ t('vm.action.addSharedDir') }}
    </el-button>
  </div>
</template>

<style scoped>
.vm-share {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.vm-share-empty {
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-text-color-secondary);
  padding: 8px 10px;
  border-radius: 6px;
  background: var(--el-fill-color-light);
}

.vm-share-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 6px;
  border: 1px solid var(--el-border-color-lighter);
}

.vm-share-icon {
  color: var(--el-color-primary);
}

.vm-share-path {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.vm-share-name {
  font-size: 12px;
  color: var(--el-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  direction: rtl;
  text-align: left;
}

.vm-share-tag {
  font-size: 10px;
  color: var(--el-text-color-secondary);
}
</style>
