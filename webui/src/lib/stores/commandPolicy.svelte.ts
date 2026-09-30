import { api } from '../api/client';
import type { CommandAccess, CommandPolicy } from '../types';

/**
 * Editable form of a command policy: administrators one per line, and the access table as rows so
 * a command can be added or removed while it is edited.
 */
export interface CommandPolicyDraft {
  admins: string;
  groupAdminsAreAdmins: boolean;
  rows: { command: string; access: CommandAccess }[];
}

/** Turns a stored policy into the form the editor works on. */
export function draftOfCommandPolicy(
  policy: CommandPolicy,
): CommandPolicyDraft {
  return {
    admins: policy.admins.join('\n'),
    groupAdminsAreAdmins: policy.group_admins_are_admins,
    rows: Object.entries(policy.access).map(([command, access]) => ({
      command,
      access,
    })),
  };
}

/** Turns an edited form back into a policy; the node normalizes and validates it on save. */
export function commandPolicyOfDraft(draft: CommandPolicyDraft): CommandPolicy {
  return {
    admins: draft.admins
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean),
    group_admins_are_admins: draft.groupAdminsAreAdmins,
    access: Object.fromEntries(
      draft.rows.map((row) => [row.command, row.access]),
    ),
  };
}

/**
 * Console state for the node-wide command permissions: who the bot's administrators are and which
 * commands only they may run.
 */
class CommandPolicyStore {
  policy = $state<CommandPolicy | null>(null);
  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);

  async load() {
    this.loading = true;
    this.error = null;
    try {
      this.policy = (await api.getCommandPolicy()).policy;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  /** Saves a policy; the node normalizes it and echoes what it enforces. */
  async save(policy: CommandPolicy): Promise<boolean> {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      this.policy = (await api.setCommandPolicy(policy)).policy;
      this.notice = 'saved';
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }
}

export const commandPolicyStore = new CommandPolicyStore();
