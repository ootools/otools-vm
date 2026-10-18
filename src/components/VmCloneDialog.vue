<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { vmStore } from '@/state/vm-store';
import type { VmSummary } from '@/types/vm';

const props = defineProps<{ vm: VmSummary | null }>();
const visible = defineModel<boolean>({ required: true });

const name = ref('');
const submitting = ref(false);

const canClone = computed(() => Boolean(vmStore.state.engine?.caps.clone));

watch(visible, (open) => {
  if (open && props.vm) {
    name.value = `${props.vm.name} 副本`;
  }
});

const submit = async (): Promise<void> => {
  const vm = props.vm;
  if (!vm) {
    return;
  }
  const value = name.value.trim();
  if (!value) {
    ElMessage.warning(t('vm.error.nameRequired'));
    return;
  }
  submitting.value = true;
  const created = await vmStore.clone(vm.id, value);
  submitting.value = false;
  if (!created) {
    ElMessage.error(vmStore.state.lastActionError || t('vm.clone.failed'));
    return;
  }
  visible.value = false;
  ElMessage.success(t('vm.clone.success', { name: created.name }));
};
</script>

<template>
  <el-dialog v-model="visible" :title="t('vm.clone.title')" width="440px" append-to-body>
    <el-form label-position="top" size="small">
      <el-form-item :label="t('vm.clone.nameLabel')">
        <el-input v-model="name" maxlength="40" />
      </el-form-item>
    </el-form>
    <p class="vm-clone-hint">{{ t('vm.clone.hint') }}</p>

    <template #footer>
      <el-button size="small" @click="visible = false">{{ t('vm.action.cancel') }}</el-button>
      <el-button size="small" type="primary" :loading="submitting" :disabled="!canClone" @click="submit">
        {{ t('vm.action.clone') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.vm-clone-hint {
  margin: 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-text-color-secondary);
}
</style>
