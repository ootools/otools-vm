/**
 * 路径选择的界面胶水层。
 *
 * 直接在组件里 `catch` 再弹一句固定文案会丢掉**失败原因**——用户只看到
 * 「打不开面板」，既不知道该重试还是该改粘贴。这里把「取错误文本」这一步
 * 统一做掉，各调用点只负责给字段赋值。
 */

import { ElMessage } from 'element-plus';
import { t } from '@/i18n';
import { PICK_DIALOG_UNAVAILABLE, pickPath, type PickPathOptions } from '@/sdk/runtime';

/**
 * 弹面板并返回路径；取消返回 `null`。
 *
 * 失败时**不抛异常**，而是弹一条带原因的提示后返回 `null`：三个调用点的
 * 后续行为完全一致（不改字段），没必要各自重写一遍 catch。
 */
export async function pickPathWithFeedback(
  options: PickPathOptions,
  fallbackFieldLabel?: string,
): Promise<string | null> {
  try {
    return await pickPath(options);
  } catch (error) {
    ElMessage.warning({
      message: t('vm.action.pickUnavailable', {
        reason: describePickError(error, fallbackFieldLabel),
      }),
      duration: 6000,
    });
    return null;
  }
}

/**
 * 把异常翻译成一句用户能懂的原因。
 *
 * 优先用底层原文：多数情况下它是操作系统的真实错误（权限、路径不存在…），
 * 比任何概括都准确；只有拿不到任何信息时才退回通用说法。
 */
function describePickError(error: unknown, fallbackFieldLabel?: string): string {
  const raw = typeof error === 'string' ? error : (error as Error | undefined)?.message ?? '';
  const text = raw.trim();

  if (text.includes(PICK_DIALOG_UNAVAILABLE)) {
    return t('vm.action.pickNoNativePanel');
  }
  if (text) {
    return text;
  }
  return fallbackFieldLabel ?? t('vm.action.pickUnknownReason');
}
