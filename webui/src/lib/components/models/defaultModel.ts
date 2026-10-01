import { t } from '../../stores/i18n.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { toasts } from '../../stores/toast.svelte';

/**
 * Makes `next` the node's default model (or clears it with `null`) and says so in a toast that
 * can undo the change. The node applies the choice before answering, so there is no save step.
 *
 * Returns whether the node accepted it; a refusal is reported in an error toast.
 */
export async function changeDefaultModel(
  next: string | null,
): Promise<boolean> {
  const previous = modelsStore.defaultModel;
  const ok = await modelsStore.setDefault(next);
  if (!ok) {
    toasts.error(t('llm.default_failed', { error: modelsStore.error ?? '' }));
    return false;
  }
  toasts.ok(
    next
      ? t('llm.default_set_toast', { model: next })
      : t('llm.default_cleared_toast'),
    {
      label: t('common.undo'),
      run: () => void changeDefaultModel(previous),
    },
  );
  return true;
}
