<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { vmStore } from '@/state/vm-store';
import type { VmSummary } from '@/types/vm';

const props = defineProps<{ vm: VmSummary | null }>();
const visible = defineModel<boolean>({ required: true });

const sizeGb = ref(40);
const submitting = ref(false);

const canResize = computed(() => Boolean(vmStore.state.engine?.caps.resizeDisk));
/** 只能往大调；上限取自宿主剩余空间以外的保守值。 */
const minSize = computed(() => props.vm?.diskSizeGb ?? 10);
const maxSize = computed(() => Math.max(minSize.value + 10, 512));

watch(visible, (open) => {
  if (open && props.vm) {
    // 默认给一个「+50%」的合理建议值，用户直接确认即可。
    sizeGb.value = Math.min(maxSize.value, Math.max(minSize.value, Math.ceil(props.vm.diskSizeGb * 1.5)));
  }
});

const submit = async (): Promise<void> => {
  const vm = props.vm;
  if (!vm) {
    return;
  }
  submitting.value = true;
  const done = await vmStore.resizeDisk(vm.id, sizeGb.value);
  submitting.value = false;
  if (!done) {
    ElMessage.error(vmStore.state.lastActionError || t('vm.resize.failed'));
    return;
  }
  visible.value = false;
  ElMessage.success(t('vm.resize.success', { size: sizeGb.value }));
};
</script>

<template>
  <el-dialog v-model="visible" :title="t('vm.resize.title')" width="460px" append-to-body>
    <p class="vm-resize-current">
      {{ t('vm.resize.current', { size: props.vm?.diskSizeGb ?? 0 }) }}
    </p>
    <el-slider v-model="sizeGb" :min="minSize" :max="maxSize" :step="10" show-input />
    <p class="vm-resize-hint">{{ t('vm.resize.hint') }}</p>

    <template #footer>
      <el-button size="small" @click="visible = false">{{ t('vm.action.cancel') }}</el-button>
      <el-button
        size="small"
        type="primary"
        :loading="submitting"
        :disabled="!canResize || sizeGb <= minSize"
        @click="submit"
      >
        {{ t('vm.action.resizeDisk') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.vm-resize-current {
  margin: 0 0 10px;
  font-size: 12px;
  color: var(--el-text-color-regular);
}

.vm-resize-hint {
  margin: 10px 0 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-text-color-secondary);
}
</style>
