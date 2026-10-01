import { confirmDialog } from './stores/confirm.svelte';
import { t } from './stores/i18n.svelte';
import { instancesStore } from './stores/instances.svelte';
import { toasts } from './stores/toast.svelte';
import type { BotInstanceView } from './types';

/**
 * Starts or stops an instance immediately and offers to undo it.
 *
 * The switch acts at once because stopping an instance that misbehaves must be one click; the
 * undo in the confirmation covers the accidental click instead of a dialog before every change.
 */
export async function toggleInstance(instance: BotInstanceView): Promise<void> {
  const ok = await instancesStore.toggleEnabled(instance);
  if (!ok) {
    toasts.error(
      t('instances.toggle_failed', {
        name: instance.name,
        error: instancesStore.error ?? '',
      }),
    );
    return;
  }
  const updated = instancesStore.find(instance.id);
  toasts.ok(
    t(
      instance.enabled ? 'instances.stopped_toast' : 'instances.started_toast',
      {
        name: instance.name,
      },
    ),
    updated
      ? { label: t('common.undo'), run: () => void toggleInstance(updated) }
      : undefined,
  );
}

/** Asks before deleting an instance; resolves to `true` once it is gone. */
export async function deleteInstance(
  instance: BotInstanceView,
): Promise<boolean> {
  const yes = await confirmDialog({
    title: t('instances.delete_title', { name: instance.name }),
    message: t('instances.delete_text'),
    confirm: t('instances.delete_confirm'),
    danger: true,
  });
  if (!yes) return false;
  const ok = await instancesStore.remove(instance.id);
  if (ok) {
    toasts.ok(t('instances.deleted_toast', { name: instance.name }));
  } else {
    toasts.error(instancesStore.error ?? t('common.error'));
  }
  return ok;
}
