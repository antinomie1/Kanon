import { api } from '../api/client';
import type {
  CreatePersonaRequest,
  PersonaItem,
  PersonasResponse,
  UpdatePersonaRequest,
} from '../types';

/**
 * Console state for the persona library.
 *
 * The node owns the library (`data/personas.json` plus the built-in base assistant); the browser
 * keeps no copy. Every mutation answers with the refreshed catalog, which replaces the local state,
 * so the list can never show a persona the running node does not have.
 */
class PersonasStore {
  catalog = $state<PersonasResponse | null>(null);
  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);

  constructor() {
    this.load();
  }

  /** Every persona, base assistant first. */
  get all(): PersonaItem[] {
    return this.catalog?.personas ?? [];
  }

  /**
   * Personas an operator manages or selects: the base assistant and their own.
   *
   * Instance personas are generated from a bot instance's prompt and are edited on that instance,
   * so they stay out of the library and out of the pickers.
   */
  get library(): PersonaItem[] {
    return this.all.filter((persona) => persona.kind !== 'instance');
  }

  /** The persona used when nothing else was chosen. */
  get baseId(): string {
    return this.catalog?.base_persona_id ?? 'assistant';
  }

  async load() {
    this.loading = true;
    this.error = null;
    try {
      this.catalog = await api.getPersonas();
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  private async mutate(action: () => Promise<PersonasResponse>) {
    this.saving = true;
    this.error = null;
    try {
      this.catalog = await action();
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }

  create(req: CreatePersonaRequest) {
    return this.mutate(() => api.createPersona(req));
  }

  update(id: string, req: UpdatePersonaRequest) {
    return this.mutate(() => api.updatePersona(id, req));
  }

  remove(id: string) {
    return this.mutate(() => api.deletePersona(id));
  }
}

export const personasStore = new PersonasStore();
