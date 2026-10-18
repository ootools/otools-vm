<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';
import type { VmSummary, ViewRect } from '@/types/vm';
import { isActiveState } from '@/utils/vm-display';

const props = defineProps<{ vm: VmSummary }>();

const stageRef = ref<HTMLElement | null>(null);
const attachError = ref('');
const attachedId = ref('');
let resizeObserver: ResizeObserver | null = null;
let rafHandle = 0;

const active = computed(() => isActiveState(props.vm.state));

/**
 * 控制台区域在页面里的矩形。
 *
 * 单位是 CSS 像素：在 webview 缩放为 1 时与 macOS point 一一对应。
 * 原生视图由宿主窗口承载，坐标原点同为窗口内容区左上角。
 */
const computeRect = (): ViewRect | null => {
  const element = stageRef.value;
  if (!element) {
    return null;
  }
  const bounds = element.getBoundingClientRect();
  if (bounds.width < 8 || bounds.height < 8) {
    return null;
  }
  return {
    x: bounds.left,
    y: bounds.top,
    width: bounds.width,
    height: bounds.height,
    scale: window.devicePixelRatio || 1,
  };
};

const detach = async (id: string): Promise<void> => {
  if (!id) {
    return;
  }
  try {
    await VmApi.detachView(id);
  } catch {
    // 虚拟机可能已经销毁，忽略。
  }
  if (attachedId.value === id) {
    attachedId.value = '';
  }
};

const syncRect = async (): Promise<void> => {
  if (!attachedId.value) {
    return;
  }
  const rect = computeRect();
  if (!rect) {
    return;
  }
  try {
    await VmApi.updateViewRect(attachedId.value, rect);
  } catch {
    // 视图已被销毁时宿主会报错，下一次 attach 会重建。
  }
};

const scheduleSync = (): void => {
  if (rafHandle) {
    return;
  }
  rafHandle = window.requestAnimationFrame(() => {
    rafHandle = 0;
    void syncRect();
  });
};

const attach = async (): Promise<void> => {
  const rect = computeRect();
  if (!rect) {
    return;
  }
  if (attachedId.value === props.vm.id) {
    await syncRect();
    return;
  }
  if (attachedId.value) {
    await detach(attachedId.value);
  }
  try {
    await VmApi.attachView(props.vm.id, rect);
    attachedId.value = props.vm.id;
    attachError.value = '';
  } catch (error) {
    attachError.value = error instanceof Error ? error.message : String(error);
    attachedId.value = '';
  }
};

const reconcile = async (): Promise<void> => {
  if (!props.vm.engine || props.vm.engine === 'none') {
    return;
  }
  if (active.value) {
    await attach();
  } else if (attachedId.value) {
    await detach(attachedId.value);
  }
};

watch(() => props.vm.state, () => void reconcile(), { immediate: true });
watch(
  () => props.vm.id,
  async (_next, previous) => {
    if (previous && previous !== props.vm.id) {
      await detach(previous);
    }
    await reconcile();
  },
);

onMounted(() => {
  if (typeof ResizeObserver !== 'undefined' && stageRef.value) {
    resizeObserver = new ResizeObserver(() => scheduleSync());
    resizeObserver.observe(stageRef.value);
  }
  window.addEventListener('resize', scheduleSync);
  // 挂载时虚拟机可能已经在运行（例如从设置页返回），补一次挂载。
  void reconcile();
});

onBeforeUnmount(async () => {
  resizeObserver?.disconnect();
  resizeObserver = null;
  window.removeEventListener('resize', scheduleSync);
  if (rafHandle) {
    window.cancelAnimationFrame(rafHandle);
    rafHandle = 0;
  }
  if (attachedId.value) {
    await detach(attachedId.value);
  }
});

const mountCommand = computed(() => {
  const first = props.vm.sharedDirs[0];
  if (!first) {
    return '';
  }
  return t('vm.console.mountHint', { tag: first.mountTag });
});

const showEngineFallback = computed(() => props.vm.engine === 'qemu');
const showError = computed(() => Boolean(props.vm.lastError));

const copyMountCommand = async (): Promise<void> => {
  if (!mountCommand.value) {
    return;
  }
  try {
    await navigator.clipboard.writeText(mountCommand.value);
    ElMessage.success(mountCommand.value);
  } catch {
    ElMessage.info(mountCommand.value);
  }
};
</script>

<template>
  <div class="vm-console">
    <div ref="stageRef" class="vm-console-stage" :class="{ live: active }">
      <div v-if="!active" class="vm-console-placeholder">
        <span class="vm-console-placeholder-icon">⏻</span>
        <h3>{{ t('vm.console.waiting') }}</h3>
        <p>{{ t('vm.console.waitingHint') }}</p>
      </div>
    </div>

    <footer class="vm-console-foot">
      <p v-if="showEngineFallback" class="vm-console-note">{{ t('vm.console.engineFallback') }}</p>
      <p v-else-if="attachError" class="vm-console-note error">{{ attachError }}</p>
      <p v-else-if="showError" class="vm-console-note error">{{ vm.lastError }}</p>
      <p v-else-if="mountCommand" class="vm-console-note clickable" @click="copyMountCommand">
        {{ mountCommand }}
      </p>
      <p v-else class="vm-console-note">{{ t('vm.console.agentHint') }}</p>
    </footer>
  </div>
</template>

<style scoped>
.vm-console {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-height: 0;
  padding: 12px;
  gap: 8px;
}

.vm-console-stage {
  position: relative;
  flex: 1;
  min-height: 0;
  border-radius: 8px;
  border: 1px solid var(--el-border-color);
  background: #0b0b0d;
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
}

.vm-console-stage.live {
  border-color: transparent;
}

.vm-console-placeholder {
  text-align: center;
  color: #9aa0a6;
}

.vm-console-placeholder-icon {
  font-size: 26px;
  opacity: 0.5;
}

.vm-console-placeholder h3 {
  margin: 10px 0 4px;
  font-size: 13px;
  font-weight: 500;
  color: #d5d7da;
}

.vm-console-placeholder p {
  margin: 0;
  font-size: 11px;
}

.vm-console-foot {
  flex: none;
}

.vm-console-note {
  margin: 0;
  font-size: 11px;
  line-height: 1.5;
  color: var(--el-text-color-secondary);
  word-break: break-all;
}

.vm-console-note.error {
  color: var(--el-color-danger);
}

.vm-console-note.clickable {
  cursor: pointer;
}

.vm-console-note.clickable:hover {
  color: var(--el-color-primary);
}
</style>
