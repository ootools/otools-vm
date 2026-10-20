<script setup lang="ts">
import { computed, ref } from 'vue';
import { Plus, FolderOpened, Close } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';
import { vmStore } from '@/state/vm-store';
import type { SharedDir } from '@/types/vm';

const props = defineProps<{ modelValue: SharedDir[]; disabled?: boolean }>();
const emit = defineEmits<{ (event: 'update:modelValue', value: SharedDir[]): void }>();

const manualMode = ref(false);
const manualPath = ref('');

/** virtio-fs 只有 Linux 宿主能跑（virtiofsd 与 memfd 都只在 Linux 有）。 */
const virtiofsSupported = computed(() => vmStore.state.host?.os === 'linux');

/** 共享协议全集，与 native 侧 SHARED_PROTOCOLS 保持一致。 */
const PROTOCOLS = ['9p', 'virtiofs', 'smb'] as const;

const setProtocol = (index: number, value: string): void => {
  if (!(PROTOCOLS as readonly string[]).includes(value)) {
    return;
  }
  update(
    props.modelValue.map((item, position) => {
      if (position !== index) {
        return item;
      }
      // 换协议时把挂载名收敛到该协议允许的字符集，免得改完协议才发现名字非法。
      const mountTag = value === 'smb' ? toSmbName(item.mountTag) : item.mountTag;
      return { ...item, protocol: value, mountTag };
    }),
  );
};

/** SMB 共享名只允许字母、数字、- _ .；其余字符替换成 -。 */
const toSmbName = (value: string): string =>
  value.replace(/[^a-zA-Z0-9._-]/g, '-').replace(/^-+|-+$/g, '').slice(0, 80) || 'share';

/** 与 native 侧 `SharedDir::derive_tag` 保持同一套规则。 */
const deriveTag = (hostPath: string, index: number): string => {
  const base = hostPath.split(/[/\\]/).filter(Boolean).pop() ?? 'share';
  const cleaned = base
    .replace(/[^a-zA-Z0-9]/g, '-')
    .toLowerCase()
    .replace(/^-+|-+$/g, '');
  return cleaned ? `${cleaned}-${index + 1}` : `share${index + 1}`;
};

/** 只做粗校验：绝对路径才可能是宿主目录，其余交给 native 报错。 */
const looksAbsolute = (value: string): boolean =>
  value.startsWith('/') || /^[a-zA-Z]:[\\/]/.test(value) || value.startsWith('\\\\');

const update = (next: SharedDir[]): void => {
  emit('update:modelValue', next);
};

const pushPath = (hostPath: string): boolean => {
  const trimmed = hostPath.trim();
  if (!trimmed) {
    return false;
  }
  if (props.modelValue.some((item) => item.hostPath === trimmed)) {
    ElMessage.info(trimmed);
    return true;
  }
  const tag = deriveTag(trimmed, props.modelValue.length);
  update([
    ...props.modelValue,
    {
      hostPath: trimmed,
      // SMB 共享名比 9p 标签严（不能带点以外的符号），统一按 SMB 收敛一次，
      // 这样以后切换协议不用再改名字。
      mountTag: toSmbName(tag),
      protocol: '9p',
      readOnly: false,
    },
  ]);
  return true;
};

const addDirectory = async (): Promise<void> => {
  try {
    const picked = await VmApi.pickFolder(t('vm.action.addSharedDir'));
    if (picked?.path) {
      pushPath(picked.path);
    }
    // picked.path === null 表示用户取消，保持现状即可。
  } catch {
    // 非 macOS 宿主没有原生面板：直接切到手动输入，而不是只弹一个错误。
    manualMode.value = true;
  }
};

const commitManual = (): void => {
  const value = manualPath.value.trim();
  if (!value) {
    return;
  }
  if (!looksAbsolute(value)) {
    ElMessage.warning(t('vm.error.pathNotAbsolute'));
    return;
  }
  if (pushPath(value)) {
    manualPath.value = '';
    manualMode.value = false;
  }
};

const cancelManual = (): void => {
  manualPath.value = '';
  manualMode.value = false;
};

const removeAt = (index: number): void => {
  update(props.modelValue.filter((_, position) => position !== index));
};

const toggleReadOnly = (index: number, value: boolean): void => {
  update(
    props.modelValue.map((item, position) =>
      position === index ? { ...item, readOnly: value } : item,
    ),
  );
};
</script>

<template>
  <div class="vm-share">
    <div v-if="!modelValue.length && !manualMode" class="vm-share-empty">
      {{ t('vm.create.sharedDirsHint') }}
    </div>

    <div v-for="(dir, index) in modelValue" :key="dir.hostPath" class="vm-share-row">
      <div class="vm-share-main">
        <el-icon class="vm-share-icon"><FolderOpened /></el-icon>
        <div class="vm-share-path" :title="dir.hostPath">
          <span class="vm-share-name">{{ dir.hostPath }}</span>
          <span class="vm-share-tag">{{ dir.mountTag }}</span>
        </div>
        <el-switch
          :model-value="dir.readOnly"
          size="small"
          :disabled="disabled"
          :active-text="t('vm.settings.readOnly')"
          @update:model-value="(value: string | number | boolean) => toggleReadOnly(index, Boolean(value))"
        />
        <el-button :icon="Close" circle size="small" text :disabled="disabled" @click="removeAt(index)" />
      </div>
      <div class="vm-share-sub">
        <el-select
          :model-value="dir.protocol"
          size="small"
          class="vm-share-protocol"
          :disabled="disabled"
          @update:model-value="(value: string | number | boolean) => setProtocol(index, String(value))"
        >
          <el-option value="9p" :label="t('vm.share.protocol9p')" />
          <el-option value="virtiofs" :label="t('vm.share.protocolVirtiofs')" :disabled="!virtiofsSupported" />
          <el-option value="smb" :label="t('vm.share.protocolSmb')" />
        </el-select>
        <span v-if="dir.protocol === 'virtiofs' && !virtiofsSupported" class="vm-share-warn">
          {{ t('vm.share.virtiofsUnsupported') }}
        </span>
        <span v-else-if="dir.protocol === 'smb'" class="vm-share-hint">
          {{ t('vm.share.smbHint') }}
        </span>
      </div>
    </div>

    <div v-if="manualMode" class="vm-share-manual">
      <el-input
        v-model="manualPath"
        size="small"
        :placeholder="t('vm.action.manualPathPlaceholder')"
        @keyup.enter="commitManual"
      />
      <el-button size="small" type="primary" @click="commitManual">
        {{ t('vm.action.confirm') }}
      </el-button>
      <el-button size="small" text @click="cancelManual">{{ t('vm.action.cancel') }}</el-button>
    </div>

    <div class="vm-share-actions">
      <el-button :icon="Plus" size="small" text type="primary" :disabled="disabled" @click="addDirectory">
        {{ t('vm.action.addSharedDir') }}
      </el-button>
      <el-button v-if="!manualMode" size="small" text :disabled="disabled" @click="manualMode = true">
        {{ t('vm.action.manualPath') }}
      </el-button>
    </div>
  </div>
</template>

<style scoped>
.vm-share {
  display: flex;
  flex-direction: column;
  gap: 6px;
  width: 100%;
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
  flex-direction: column;
  gap: 4px;
  padding: 6px 8px;
  border-radius: 6px;
  border: 1px solid var(--el-border-color-lighter);
}

.vm-share-main {
  display: flex;
  align-items: center;
  gap: 8px;
}

.vm-share-sub {
  display: flex;
  align-items: center;
  gap: 8px;
  padding-left: 22px;
}

.vm-share-protocol {
  width: 190px;
}

.vm-share-warn {
  font-size: 10px;
  line-height: 1.5;
  color: var(--el-color-warning);
}

.vm-share-hint {
  font-size: 10px;
  line-height: 1.5;
  color: var(--el-text-color-secondary);
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

.vm-share-manual {
  display: flex;
  align-items: center;
  gap: 6px;
}

.vm-share-actions {
  display: flex;
  align-items: center;
  gap: 4px;
}
</style>
