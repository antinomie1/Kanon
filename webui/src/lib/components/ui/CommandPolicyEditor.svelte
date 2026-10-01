<script lang="ts">
import { Plus, X } from 'lucide-svelte';
import type { CommandPolicyDraft } from '../../stores/commandPolicy.svelte';
import { t } from '../../stores/i18n.svelte';
import type { CommandAccess } from '../../types';
import Button from './Button.svelte';
import Select from './Select.svelte';
import SupportBadge from './SupportBadge.svelte';
import Switch from './Switch.svelte';
import TextField from './TextField.svelte';

/**
 * Editor for one command policy: administrators, whether group roles count, and the access table.
 *
 * Shared by the node-wide settings and the per-instance override so both edit exactly the same
 * fields; the owner decides when the draft is saved.
 */
let { draft = $bindable() }: { draft: CommandPolicyDraft } = $props();

let newCommand = $state('');

/** Access levels in the order the selector lists them. */
const accessLevels: { value: CommandAccess; labelKey: string }[] = [
  { value: 'everyone', labelKey: 'commands.level_everyone' },
  { value: 'admins_in_groups', labelKey: 'commands.level_admins_in_groups' },
  { value: 'admins', labelKey: 'commands.level_admins' },
];

/** Adds a command row, restricted to administrators by default. */
function addCommand() {
  const command = newCommand.trim().replace(/^\//, '').toLowerCase();
  if (!command || draft.rows.some((row) => row.command === command)) return;
  draft.rows = [...draft.rows, { command, access: 'admins' }];
  newCommand = '';
}
</script>

<div class="flex flex-col gap-5">
  <label class="block">
    <span class="label">{t('commands.admins')}</span>
    <textarea
      bind:value={draft.admins}
      rows="3"
      placeholder="onebot:12345&#10;qqofficial:5361A5D2..."
      class="input mono"
    ></textarea>
    <span class="mt-1.5 block hint">{t('commands.admins_hint')}</span>
  </label>

  <div class="flex items-start gap-3 text-[14.5px]">
    <span class="min-w-0 flex-1">
      <span class="block font-medium">{t('commands.group_admins')}</span>
      <span class="block hint">{t('commands.group_admins_hint')}</span>
      <SupportBadge capabilities={['sender_role']} />
    </span>
    <Switch
      checked={draft.groupAdminsAreAdmins}
      label={t('commands.group_admins')}
      onchange={(next) => (draft.groupAdminsAreAdmins = next)}
    />
  </div>

  <div>
    <span class="label">{t('commands.access')}</span>
    <span class="mb-2.5 block hint">{t('commands.access_hint')}</span>
    <div class="flex flex-col gap-2">
      {#each draft.rows as row, index (row.command)}
        <div class="flex items-center gap-2.5">
          <code class="w-28 shrink-0 truncate text-[13.5px] font-medium">/{row.command}</code>
          <Select bind:value={draft.rows[index].access} class="flex-1" aria-label="/{row.command}">
            {#each accessLevels as level (level.value)}
              <option value={level.value}>{t(level.labelKey)}</option>
            {/each}
          </Select>
          <Button
            type="button"
            onclick={() => (draft.rows = draft.rows.filter((_, i) => i !== index))}
            variant="text"
            size="sm"
            square
            aria-label={t('commands.remove_named', { command: row.command })}
          >
            <X size={16} strokeWidth={2} />
          </Button>
        </div>
      {/each}
      <div class="flex items-center gap-2.5">
        <TextField
          bind:value={newCommand}
          placeholder={t('commands.add_placeholder')}
          aria-label={t('commands.add_placeholder')}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              addCommand();
            }
          }}
          mono class="flex-1"
        />
        <Button type="button" onclick={addCommand} size="sm" disabled={!newCommand.trim()}>
          <Plus size={14} strokeWidth={2.2} />
          {t('commands.add')}
        </Button>
      </div>
    </div>
  </div>
</div>
