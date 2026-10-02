<script lang="ts">
import type { JsonSchema } from '../../configSchema';
import { withKey } from '../../configSchema';
import ConfigField from './ConfigField.svelte';

/**
 * The fields of one object in a plugin's settings, one control per declared property.
 *
 * Controlled: every edit hands a new object to `onchange`, so the drawer owns the only copy of the
 * settings and the JSON view always shows exactly what the form holds. Nested objects render this
 * component again, one level deeper in `path`.
 */
let {
  schema,
  value,
  onchange,
  path = '',
  texts,
}: {
  /** An object schema with `properties`. */
  schema: JsonSchema;
  value: Record<string, unknown>;
  onchange: (next: Record<string, unknown>) => void;
  /** Dotted property path of this object from the settings root (`''` at the root). */
  path?: string;
  /** The plugin's translations for the console's language (`config.<path>.title`, …). */
  texts: Record<string, string>;
} = $props();

const keys = $derived(Object.keys(schema.properties ?? {}));
const required = $derived(new Set(schema.required ?? []));
</script>

<div class="flex flex-col gap-4">
  {#each keys as key (key)}
    <ConfigField
      name={key}
      schema={(schema.properties ?? {})[key]}
      value={value[key]}
      required={required.has(key)}
      path={path ? `${path}.${key}` : key}
      {texts}
      onchange={(next) => onchange(withKey(value, key, next))}
    />
  {/each}
</div>
