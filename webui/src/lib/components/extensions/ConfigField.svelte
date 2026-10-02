<script lang="ts">
import type { JsonSchema } from '../../configSchema';
import { fieldKind, schemaType } from '../../configSchema';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import Select from '../ui/Select.svelte';
import Switch from '../ui/Switch.svelte';
import TextField from '../ui/TextField.svelte';
import ConfigForm from './ConfigForm.svelte';

/**
 * One declared setting, edited with the control its schema calls for (see `fieldKind`).
 *
 * Clearing a field hands `undefined` up, which removes the key so the plugin's default applies
 * again, rather than saving an empty string the plugin never asked for.
 */
let {
  name,
  schema,
  value,
  onchange,
  required = false,
  path,
  texts,
}: {
  /** Property name, the label when the schema has no title. */
  name: string;
  schema: JsonSchema;
  value: unknown;
  onchange: (next: unknown) => void;
  required?: boolean;
  /** Dotted property path from the settings root, the key of this field's translations. */
  path: string;
  texts: Record<string, string>;
} = $props();

const kind = $derived(fieldKind(schema));
const id = $derived(`config-${path.replaceAll('.', '-')}`);
const title = $derived(texts[`config.${path}.title`] || schema.title || name);
const description = $derived(
  texts[`config.${path}.description`] || schema.description || '',
);

/** Text typed into a number field that does not parse yet, kept so typing `1.` is not undone. */
let numberDraft = $state<string | null>(null);
/** JSON being typed for a field the form cannot show, and why it does not parse. */
let jsonDraft = $state<string | null>(null);
let jsonError = $state<string | null>(null);

function setNumber(raw: string) {
  const text = raw.trim();
  if (text === '') {
    numberDraft = null;
    onchange(undefined);
    return;
  }
  const parsed = Number(text);
  const valid =
    Number.isFinite(parsed) && (kind !== 'integer' || Number.isInteger(parsed));
  numberDraft = valid ? null : raw;
  if (valid) onchange(parsed);
}

/** List items are one per line; numbers that do not parse are kept as typed for the node to refuse. */
function setList(raw: string) {
  const lines = raw
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '');
  const itemType = schema.items ? schemaType(schema.items) : 'string';
  onchange(
    lines.map((line) =>
      itemType === 'string' || !Number.isFinite(Number(line))
        ? line
        : Number(line),
    ),
  );
}

function setJson(raw: string) {
  jsonDraft = raw;
  if (raw.trim() === '') {
    jsonError = null;
    onchange(undefined);
    return;
  }
  try {
    onchange(JSON.parse(raw));
    jsonError = null;
  } catch (e) {
    jsonError = t('extensions.config_bad_json', { error: errorText(e) });
  }
}

const asText = (v: unknown) => (v === undefined || v === null ? '' : String(v));
</script>

{#if kind === 'boolean'}
  <div class="flex items-start justify-between gap-4">
    <div class="min-w-0">
      <span class="label mb-0">{title}</span>
      {#if description}<p class="m-0 mt-1 hint">{description}</p>{/if}
    </div>
    <Switch
      checked={value === undefined ? schema.default === true : value === true}
      label={title}
      onchange={(next) => onchange(next)}
    />
  </div>
{:else if kind === 'object'}
  <fieldset class="m-0 flex flex-col gap-3 rounded-2xl border border-line p-4">
    <legend class="px-1 text-[14px] font-semibold">{title}</legend>
    {#if description}<p class="m-0 hint">{description}</p>{/if}
    <ConfigForm
      {schema}
      value={value && typeof value === 'object' && !Array.isArray(value)
        ? (value as Record<string, unknown>)
        : {}}
      onchange={(next) => onchange(next)}
      {path}
      {texts}
    />
  </fieldset>
{:else}
  <div>
    <label class="label" for={id}>
      {title}{#if required}<span class="text-danger"> *</span>{/if}
    </label>
    {#if kind === 'enum'}
      <Select
        {id}
        value={asText(value ?? schema.default)}
        onchange={(e) => {
          const raw = e.currentTarget.value;
          onchange(
            raw === ''
              ? undefined
              : (schema.enum ?? []).find((option) => String(option) === raw),
          );
        }}
      >
        {#if !required}<option value="">{t('extensions.config_unset')}</option>{/if}
        {#each schema.enum ?? [] as option (String(option))}
          <option value={String(option)}>{String(option)}</option>
        {/each}
      </Select>
    {:else if kind === 'string' || kind === 'secret'}
      <TextField
        {id}
        class="w-full"
        type={kind === 'secret' ? 'password' : 'text'}
        autocomplete={kind === 'secret' ? 'new-password' : 'off'}
        spellcheck={false}
        placeholder={schema.default === undefined ? '' : String(schema.default)}
        value={asText(value)}
        oninput={(e) => {
          const raw = (e.currentTarget as unknown as { value: string }).value;
          onchange(raw === '' ? undefined : raw);
        }}
      />
    {:else if kind === 'number' || kind === 'integer'}
      <TextField
        {id}
        class="w-full"
        mono
        inputmode={kind === 'integer' ? 'numeric' : 'decimal'}
        placeholder={schema.default === undefined ? '' : String(schema.default)}
        value={numberDraft ?? asText(value)}
        oninput={(e) =>
          setNumber((e.currentTarget as unknown as { value: string }).value)}
      />
      {#if numberDraft !== null}
        <p class="m-0 mt-1.5 text-[13px] text-danger">
          {t(kind === 'integer' ? 'extensions.config_not_integer' : 'extensions.config_not_number')}
        </p>
      {/if}
    {:else if kind === 'text'}
      <textarea
        {id}
        class="input min-h-[96px]"
        placeholder={schema.default === undefined ? '' : String(schema.default)}
        value={asText(value)}
        oninput={(e) =>
          onchange(e.currentTarget.value === '' ? undefined : e.currentTarget.value)}
      ></textarea>
    {:else if kind === 'list'}
      <textarea
        {id}
        class="input mono min-h-[96px]"
        spellcheck="false"
        value={Array.isArray(value) ? value.map(String).join('\n') : ''}
        oninput={(e) => setList(e.currentTarget.value)}
      ></textarea>
      <p class="m-0 mt-1.5 hint">{t('extensions.config_one_per_line')}</p>
    {:else}
      <textarea
        {id}
        class="input mono min-h-[96px]"
        spellcheck="false"
        value={jsonDraft ?? (value === undefined ? '' : JSON.stringify(value, null, 2))}
        oninput={(e) => setJson(e.currentTarget.value)}
      ></textarea>
      {#if jsonError}
        <p class="m-0 mt-1.5 text-[13px] text-danger">{jsonError}</p>
      {/if}
    {/if}
    {#if description}<p class="m-0 mt-1.5 hint">{description}</p>{/if}
  </div>
{/if}
