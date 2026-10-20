<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { Plus } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { VmApi } from '@/services/vm-api';
import { vmStore } from '@/state/vm-store';
import {
  isQemuInstallBusy,
  type EnginePreference,
  type HcsStatus,
  type QemuInstallStatus,
  type QemuPlan,
  type VmSettings,
} from '@/types/vm';

const emit = defineEmits<{ (event: 'create'): void }>();

const engine = computed(() => vmStore.state.engine);
const host = computed(() => vmStore.state.host);

const softwareAccel = computed(
  () => engine.value?.kind === 'qemu' && engine.value?.accelerator === 'tcg',
);

const caps = computed(() => {
  const value = engine.value?.caps;
  if (!value) {
    return [];
  }
  return [
    { label: t('vm.welcome.capShared'), on: value.sharedDirs },
    { label: t('vm.welcome.capClipboard'), on: value.clipboard },
    { label: t('vm.welcome.capResize'), on: value.autoResize },
    { label: t('vm.welcome.capAudio'), on: value.audio },
  ];
});

/**
 * 当前后端完全没有画面输出能力时的说明。
 *
 * 只在 HCS 出现（它没有 framebuffer 接口）。必须提前讲清楚，否则用户建好
 * 机器、点启动、看不到任何东西，会以为功能坏了。
 *
 * **必须排除「引擎根本没装好」的情况**：那时 caps 是全 false 的占位值，
 * 照着渲染会变成「没装 QEMU」却提示「HCS 不提供画面」——完全误导。
 */
const noDisplay = computed(
  () =>
    Boolean(engine.value)
    && engine.value?.available === true
    && engine.value?.caps.displayOutput === false,
);

/**
 * HCS 引擎状态。
 *
 * **不再作为用户可选项暴露**。原因不是「没做完」，而是它的定位站不住：
 * 它在 Windows 上唯一标榜的「CPU 由 Hyper-V 直接执行」，QEMU 后端通过 WHPX
 * 本来就已经具备；而它没有画面（无公开 framebuffer 接口）、没有共享目录、
 * 没有音频、没接端口转发——用户建完机器、点启动，会面对一个黑屏且在 NAT
 * 后面连不进去的空转进程。
 *
 * 保留探测与后端代码（将来做无头场景的基础），但界面上不提供入口，
 * 避免用户掉进这个坑。
 */
const hcs = ref<HcsStatus | null>(null);
const settings = ref<VmSettings | null>(null);
const savingEngine = ref(false);

/**
 * 引擎选项只保留「默认（QEMU）」。
 *
 * 这里刻意不按 `hcsUsable` 追加 HCS：它现在不可选。如果将来接上端口转发、
 * 明确转成「无头专用」，再把入口加回来，并在文案里写清代价。
 */
const engineOptions = computed(() => [
  { value: 'auto' as EnginePreference, label: t('vm.engine.prefAuto'), hint: t('vm.engine.prefAutoHint') },
]);

onMounted(async () => {
  try {
    const [status, current] = await Promise.all([VmApi.getHcsStatus(), VmApi.getSettings()]);
    hcs.value = status;
    settings.value = current;
  } catch {
    // 探测失败不影响主流程：没有 HCS 信息时就不显示引擎选项。
  }
  await refreshQemuPlan();
  // 上次安装没跑完（用户中途关了面板）时把进度接回来。
  await pollInstallOnce();
  // 上次进程被强杀可能留下在跑的机器，进页面就查一次。
  await refreshOrphans();
});

// ------------------------------------------------------------ QEMU 一键获取

/**
 * 「本机没有 QEMU 时怎么办」。
 *
 * 以前这里只写一句「请安装 QEMU 后重启」，把找官网、选版本、配 PATH 全丢给
 * 用户。现在改成一次点击：native 端会挑本机最可行的一条路（winget / 官方
 * 安装器 / Homebrew），我们只负责展示与轮询进度。
 */
const qemuPlan = ref<QemuPlan | null>(null);
const installStatus = ref<QemuInstallStatus | null>(null);
const installing = ref(false);
const reprobing = ref(false);
let pollTimer: ReturnType<typeof setInterval> | null = null;

/** 只有 QEMU 后端缺失时才需要这块。VZ / HCS 都不依赖 QEMU。 */
const needsQemu = computed(
  () => !engine.value?.available && (engine.value?.kind === 'qemu' || engine.value?.kind === 'none'),
);

const installBusy = computed(() => installing.value || isQemuInstallBusy(installStatus.value));

/** 进度百分比：native 端总量未知时会按名义大小估算，所以这里只做显示。 */
const installPercent = computed(() => {
  const progress = installStatus.value?.progress ?? 0;
  return Math.max(0, Math.min(100, Math.round(progress * 100)));
});

const refreshQemuPlan = async (): Promise<void> => {
  try {
    qemuPlan.value = await VmApi.getQemuPlan();
  } catch {
    // 拿不到方案就不显示一键获取入口，避免点了才出错。
  }
};

const pollInstallOnce = async (): Promise<void> => {
  try {
    installStatus.value = await VmApi.qemuInstallStatus();
    if (installStatus.value.phase === 'done') {
      stopPolling();
      installing.value = false;
      ElMessage.success(t('vm.qemu.done'));
      // 装完立刻重新探测，用户不用重启宿主。
      await reprobe();
      return;
    }
    if (installStatus.value.phase === 'failed') {
      stopPolling();
      installing.value = false;
    }
  } catch {
    // 轮询失败不打断：下一轮再试，用户随时可以手动重试。
  }
};

const startPolling = (): void => {
  if (pollTimer !== null) {
    return;
  }
  pollTimer = setInterval(() => {
    void pollInstallOnce();
  }, 700);
};

const stopPolling = (): void => {
  if (pollTimer !== null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
};

const installQemu = async (): Promise<void> => {
  installing.value = true;
  try {
    installStatus.value = await VmApi.installQemu();
    startPolling();
  } catch (error) {
    installing.value = false;
    ElMessage.error(error instanceof Error ? error.message : String(error));
  }
};

/** 重新探测引擎：用户在系统里自己装好 QEMU 后也能立刻生效。 */
const reprobe = async (): Promise<void> => {
  reprobing.value = true;
  try {
    await VmApi.reprobeEngine();
    await vmStore.load();
    await refreshQemuPlan();
  } catch (error) {
    // 有机器在跑时会被拒绝——把原因如实带出来，不要吞掉。
    ElMessage.warning(error instanceof Error ? error.message : String(error));
  } finally {
    reprobing.value = false;
  }
};

onBeforeUnmount(stopPolling);

const changeEngine = async (preference: EnginePreference): Promise<void> => {
  savingEngine.value = true;
  try {
    settings.value = await VmApi.setEnginePreference(preference);
    // 引擎已经重建，必须重新拉一次能力集：否则界面还显示旧引擎的 caps，
    // 用户会以为切换没生效。HCS 与 QEMU 的 displayOutput 正好相反，
    // 不刷新就会出现「切到了 QEMU 却仍说没有画面」。
    await vmStore.load();
    await refreshQemuPlan();
    ElMessage.success(t('vm.engine.prefSaved'));
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : String(error));
  } finally {
    savingEngine.value = false;
  }
};

// ------------------------------------------------------------ 孤儿清理

/**
 * 上次宿主进程被强杀（或插件崩溃）后，HCS 里可能还留着在跑的机器。
 *
 * 这些机器在虚拟机列表里根本看不到，但确实占着内存和 VHDX 文件——不提示
 * 的话用户只会觉得「电脑变卡了」，或者去任务管理器赌一把 `vmwp.exe`。
 * 所以进页面就查一次，有就直接摆出来。
 */
const orphans = ref<string[]>([]);
const killingOrphans = ref(false);

const refreshOrphans = async (): Promise<void> => {
  try {
    const result = await VmApi.getHcsOrphans();
    orphans.value = result.orphans;
  } catch {
    // 查不到就当没有：这是个附加提示，不该挡住主流程。
    orphans.value = [];
  }
};

const killOrphans = async (): Promise<void> => {
  killingOrphans.value = true;
  try {
    const result = await VmApi.killHcsOrphans(orphans.value);
    if (result.failed.length > 0) {
      ElMessage.warning(
        t('vm.hcs.orphanPartial', {
          done: String(result.killed.length),
          rest: String(result.failed.length),
        }),
      );
    } else {
      ElMessage.success(t('vm.hcs.orphanKilled'));
    }
    await refreshOrphans();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : String(error));
  } finally {
    killingOrphans.value = false;
  }
};
</script>

<template>
  <div class="vm-welcome">
    <div class="vm-welcome-inner">
      <span class="vm-welcome-badge">OTools VM</span>
      <h2>{{ t('vm.welcome.title') }}</h2>
      <p class="vm-welcome-lead">{{ t('vm.welcome.lead') }}</p>

      <div class="vm-welcome-create">
        <div>
          <h3>{{ t('vm.welcome.createTitle') }}</h3>
          <p>{{ t('vm.welcome.createDesc') }}</p>
        </div>
        <el-button type="primary" :icon="Plus" @click="emit('create')">
          {{ t('vm.action.create') }}
        </el-button>
      </div>

      <div class="vm-welcome-cards">
        <div class="vm-welcome-card">
          <h4>{{ t('vm.welcome.engineTitle') }}</h4>
          <p class="vm-welcome-engine">
            {{ engine?.available ? engine.label : t('vm.engine.unavailable') }}
            <span v-if="engine?.version">· {{ engine.version }}</span>
          </p>
          <p v-if="engine?.reason" class="vm-welcome-reason">{{ engine.reason }}</p>
          <p v-if="softwareAccel" class="vm-welcome-reason">{{ t('vm.engine.softwareAccel') }}</p>
          <p v-if="noDisplay" class="vm-welcome-reason">{{ t('vm.console.noDisplayHint') }}</p>
          <p v-if="host" class="vm-welcome-host">
            {{ host.os }} / {{ host.arch }} · {{ host.totalMemoryMb }} {{ t('vm.unit.mb') }}
          </p>
        </div>

        <!--
          本机没有 QEMU 时的出口。
          以前这里只有「请自行安装」，把找官网、选版本、配 PATH 都推给用户；
          现在是一键获取 + 进度 + 重新检测。
        -->
        <div v-if="needsQemu && qemuPlan" class="vm-welcome-card vm-welcome-card-wide">
          <h4>{{ t('vm.qemu.title') }}</h4>
          <p class="vm-welcome-engine">
            {{ qemuPlan.automatic ? t('vm.qemu.lead') : t('vm.qemu.manualLead') }}
          </p>
          <p class="vm-welcome-reason">{{ qemuPlan.reason }}</p>
          <p v-if="qemuPlan.note" class="vm-welcome-host">{{ qemuPlan.note }}</p>

          <template v-if="installBusy">
            <el-progress :percentage="installPercent" :stroke-width="6" style="margin-top: 10px" />
            <p class="vm-welcome-host">
              {{ installStatus?.phase === 'downloading' ? t('vm.qemu.downloading') : t('vm.qemu.installing') }}
            </p>
          </template>

          <div v-else class="vm-qemu-actions">
            <el-button
              v-if="qemuPlan.automatic"
              type="primary"
              size="small"
              @click="installQemu"
            >
              {{ t('vm.qemu.install') }}
            </el-button>
            <el-button size="small" :loading="reprobing" @click="reprobe">
              {{ t('vm.qemu.reprobe') }}
            </el-button>
          </div>

          <p v-if="installStatus?.phase === 'failed'" class="vm-welcome-reason">
            {{ installStatus.error }}
          </p>
          <p v-if="!qemuPlan.automatic && qemuPlan.command" class="vm-welcome-host vm-qemu-command">
            {{ qemuPlan.command }}
          </p>
        </div>

        <!--
          HCS 已不再作为可选引擎。但**已经选了 HCS 的用户**（旧版本存下的偏好）
          必须看到出口，否则他们被永久困在「没有画面」的后端里。
          正常情况下 engineOptions 只有一项，这块不渲染。
        -->
        <div
          v-if="hcs?.supported && (settings?.enginePreference === 'hcs' || engineOptions.length > 1)"
          class="vm-welcome-card"
        >
          <h4>{{ t('vm.engine.prefTitle') }}</h4>
          <p class="vm-welcome-engine">
            {{ hcs.available ? t('vm.hcs.available') : t('vm.hcs.needsElevation') }}
          </p>
          <p v-if="!hcs.available" class="vm-welcome-reason">
            {{ hcs.needsElevation ? t('vm.hcs.reasonElevation') : t('vm.hcs.reasonComponent') }}
          </p>
          <el-radio-group
            v-if="engineOptions.length > 1"
            :model-value="settings?.enginePreference ?? 'auto'"
            size="small"
            :disabled="savingEngine"
            @change="(value: string | number | boolean) => changeEngine(value as EnginePreference)"
          >
            <el-radio v-for="option in engineOptions" :key="option.value" :value="option.value">
              {{ option.label }}
            </el-radio>
          </el-radio-group>
          <!-- 误入 HCS 的旧用户：告诉他们现在为什么不该用，并给一键回退 -->
          <template v-else-if="settings?.enginePreference === 'hcs'">
            <p class="vm-welcome-reason">{{ t('vm.hcs.retiredHint') }}</p>
            <div class="vm-qemu-actions">
              <el-button size="small" type="primary" :loading="savingEngine" @click="changeEngine('auto')">
                {{ t('vm.hcs.retiredBack') }}
              </el-button>
            </div>
          </template>
        </div>

        <!--
          上次进程被强杀后留在 HCS 里的机器。
          列表里看不见但确实在跑，占内存也占着 VHDX，给一键清理。
        -->
        <div v-if="orphans.length > 0" class="vm-welcome-card vm-welcome-card-wide">
          <h4>{{ t('vm.hcs.orphanTitle') }}</h4>
          <p class="vm-welcome-engine">
            {{ t('vm.hcs.orphanLead', { count: String(orphans.length) }) }}
          </p>
          <p class="vm-welcome-host vm-hcs-orphans">{{ orphans.join('、') }}</p>
          <div class="vm-qemu-actions">
            <el-button type="primary" size="small" :loading="killingOrphans" @click="killOrphans">
              {{ t('vm.hcs.orphanKill') }}
            </el-button>
            <el-button size="small" :disabled="killingOrphans" @click="refreshOrphans">
              {{ t('vm.qemu.reprobe') }}
            </el-button>
          </div>
        </div>

        <div class="vm-welcome-card">
          <h4>{{ t('vm.welcome.capsTitle') }}</h4>
          <ul class="vm-welcome-caps">
            <li v-for="cap in caps" :key="cap.label" :class="{ off: !cap.on }">
              <span class="vm-cap-mark">{{ cap.on ? '✓' : '—' }}</span>
              {{ cap.label }}
            </li>
          </ul>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.vm-welcome {
  flex: 1;
  overflow: auto;
  padding: 28px 24px;
}

.vm-welcome-inner {
  max-width: 640px;
  margin: 0 auto;
}

.vm-welcome-badge {
  display: inline-block;
  padding: 2px 8px;
  border-radius: 999px;
  background: var(--el-color-primary-light-9);
  color: var(--el-color-primary);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.04em;
}

.vm-welcome h2 {
  margin: 12px 0 8px;
  font-size: 22px;
  font-weight: 650;
  color: var(--el-text-color-primary);
}

.vm-welcome-lead {
  margin: 0 0 20px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--el-text-color-regular);
}

.vm-welcome-create {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 16px;
  border-radius: 10px;
  border: 1px solid var(--el-border-color-lighter);
  background: var(--el-fill-color-blank);
}

.vm-welcome-create h3 {
  margin: 0 0 4px;
  font-size: 13px;
  font-weight: 600;
  color: var(--el-text-color-primary);
}

.vm-welcome-create p {
  margin: 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-text-color-secondary);
}

.vm-welcome-cards {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
  margin-top: 12px;
}

.vm-welcome-card {
  padding: 14px;
  border-radius: 10px;
  border: 1px solid var(--el-border-color-lighter);
  background: var(--el-fill-color-blank);
}

.vm-welcome-card h4 {
  margin: 0 0 8px;
  font-size: 12px;
  font-weight: 600;
  color: var(--el-text-color-secondary);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.vm-welcome-engine {
  margin: 0;
  font-size: 13px;
  color: var(--el-text-color-primary);
}

.vm-welcome-reason {
  margin: 6px 0 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-color-warning);
}

.vm-welcome-host {
  margin: 6px 0 0;
  font-size: 11px;
  color: var(--el-text-color-secondary);
}

.vm-welcome-caps {
  margin: 0;
  padding: 0;
  list-style: none;
}

.vm-welcome-caps li {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  line-height: 1.9;
  color: var(--el-text-color-regular);
}

.vm-welcome-caps li.off {
  color: var(--el-text-color-disabled);
}

.vm-cap-mark {
  width: 12px;
  text-align: center;
  color: var(--el-color-success);
}

.vm-welcome-caps li.off .vm-cap-mark {
  color: var(--el-text-color-disabled);
}

.vm-welcome-card-wide {
  grid-column: 1 / -1;
}

.vm-qemu-actions {
  display: flex;
  gap: 8px;
  margin-top: 10px;
}

.vm-qemu-command {
  display: block;
  word-break: break-all;
  font-family: var(--el-font-family-mono, monospace);
  background: var(--el-fill-color-light);
  border-radius: 6px;
  padding: 6px 8px;
  margin-top: 8px;
}

.vm-hcs-orphans {
  word-break: break-all;
  font-family: var(--el-font-family-mono, monospace);
}
</style>
