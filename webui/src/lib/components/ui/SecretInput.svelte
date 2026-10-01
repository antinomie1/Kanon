<script lang="ts">
import { Eye, EyeOff } from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import Button from './Button.svelte';
import TextField from './TextField.svelte';

/**
 * Password-style input with a show/hide toggle, for tokens and secrets the node never returns.
 *
 * The browser is told not to autofill it: these are credentials for other services, and a saved
 * console login filled in here would be sent to them.
 */
let {
  value = $bindable(''),
  id,
  placeholder,
}: {
  value?: string;
  id?: string;
  placeholder?: string;
} = $props();

let shown = $state(false);
</script>

<TextField
  {id}
  mono
  type={shown ? 'text' : 'password'}
  autocomplete="new-password"
  spellcheck={false}
  {placeholder}
  bind:value
>
  {#snippet trailing()}
    <Button
      type="button"
      variant="text"
      size="xs"
      square
      aria-label={shown ? t('platforms.hide_secret') : t('platforms.show_secret')}
      aria-pressed={shown}
      onclick={() => (shown = !shown)}
    >
      {#if shown}
        <EyeOff size={16} strokeWidth={2} />
      {:else}
        <Eye size={16} strokeWidth={2} />
      {/if}
    </Button>
  {/snippet}
</TextField>
