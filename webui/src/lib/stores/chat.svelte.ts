import { ApiError, api } from '../api/client';
import { streamChatCompletion } from '../api/sse';
import { dshConsoleTarget, dshHistory } from '../dsh-history';
import type { DshSession } from '../types';

/** One message of the test chat. */
export interface ChatTurn {
  id: number;
  role: 'user' | 'assistant';
  content: string;
  /** Reasoning the model streamed separately from its answer. */
  reasoning: string;
  /** Why the reply stopped, when it failed; shown under whatever arrived before. */
  error: string | null;
}

/** Everything one message needs besides its text; resolved by the page from the chosen target. */
export interface SendOptions {
  /** Selected backend and trusted instance policy, resolved by the page. */
  agent: string;
  instanceId?: string;
  /** Persona to apply to the session before this turn. */
  personaId: string;
  /** Canonical `<provider>/<model>` reference; `undefined` lets the node use its default. */
  model: string | undefined;
}

/**
 * State of the test chat, kept outside the page so a conversation survives a trip to another
 * page and back.
 *
 * Each target (the base assistant, an instance's setup, a persona) talks in a session of its own,
 * and a conversation that starts empty on screen also starts empty on the node: its first message
 * resets that session, so the model never remembers a transcript the page no longer shows.
 */
class ChatStore {
  /** Who answers: `''` for the base assistant, `i:<id>` for an instance, `p:<id>` for a persona. */
  target = $state('');
  /** Model picked for the chat; `''` means the target's own model or the node default. */
  model = $state('');
  tools = $state(true);
  turns = $state<ChatTurn[]>([]);
  streaming = $state(false);

  private controller: AbortController | null = null;
  private nextId = 1;
  /** A failed or cancelled reset must be retried even when its failed turn remains visible. */
  private needsReset = true;
  /** A new remote identity starts a new DSH conversation without deleting its old journal. */
  private remoteId = '';
  private backend = '';

  /** Session the node keeps this conversation in. */
  get sessionId(): string {
    if (this.target.startsWith('i:')) return `instance:${this.target.slice(2)}:webui:chat#0`;
    return this.target ? `webui:chat:${this.target}` : 'webui:chat';
  }

  /** Switches who answers, which starts a new conversation. */
  choose(target: string) {
    if (target === this.target) return;
    this.stop();
    this.target = target;
    this.model = '';
    this.turns = [];
    this.needsReset = true;
    this.remoteId = '';
  }

  /** Starts over; the node forgets the conversation when the next message is sent. */
  clear() {
    this.stop();
    this.turns = [];
    this.needsReset = true;
    this.remoteId = '';
  }

  stop() {
    this.controller?.abort();
    this.controller = null;
    this.streaming = false;
  }

  /** Backend changes start a separate conversation and clear incompatible model choices. */
  useAgent(agent: string) {
    if (this.backend === agent) return;
    this.clear();
    this.model = '';
    this.backend = agent;
  }

  /** Resumes a remote console conversation from DSH's authoritative recent journal. */
  async resumeDsh(session: DshSession) {
    const target = dshConsoleTarget(session.sessionId);
    if (target === null) throw new Error('This DSH session belongs to platform traffic');
    const snapshot = await api.getDshSession(session.sessionId);
    const history = dshHistory(snapshot.records);
    this.useAgent('dsh');
    this.choose(target);
    this.stop();
    this.remoteId = session.sessionId;
    const next = session.projections?.values.modelSelection?.next;
    this.model = next ? `${next.provider}/${next.model}` : '';
    this.turns = history.map(message => ({
      id: this.nextId++, role: message.role, content: message.text,
      reasoning: message.reasoning, error: null,
    }));
  }

  async send(text: string, options: SendOptions) {
    if (this.streaming) return;
    if (this.backend !== options.agent) {
      this.useAgent(options.agent);
    }
    if (options.agent === 'dsh' && !this.remoteId) {
      this.remoteId = `kanon-console-${crypto.randomUUID()}-${encodeURIComponent(this.target)}`;
    }
    const sessionId = options.agent === 'dsh' ? this.remoteId : this.sessionId;
    const tools = this.tools;
    this.turns.push(
      {
        id: this.nextId++,
        role: 'user',
        content: text,
        reasoning: '',
        error: null,
      },
      {
        id: this.nextId++,
        role: 'assistant',
        content: '',
        reasoning: '',
        error: null,
      },
    );
    // Read back through the store so edits go through the reactive proxy.
    const reply = this.turns[this.turns.length - 1];
    this.streaming = true;
    const controller = new AbortController();
    this.controller = controller;

    try {
      if (this.needsReset && options.agent !== 'dsh') {
        try {
          await api.resetSession(sessionId, controller.signal);
        } catch (e) {
          // 404 means the node has never seen this session: it is already empty, as wanted.
          if (!(e instanceof ApiError && e.status === 404)) throw e;
        }
        // Stop or target changes may have replaced this conversation while reset was pending.
        if (controller.signal.aborted) return;
        this.needsReset = false;
      }
      await streamChatCompletion(
        {
          session_id: sessionId,
          agent: options.agent,
          instance_id: options.instanceId,
          message: text,
          model: options.model,
          persona_id: options.agent === 'dsh' ? undefined : options.personaId,
          tools,
        },
        {
          onChunk: (delta, reasoning) => {
            if (controller.signal.aborted) return;
            reply.content += delta;
            if (reasoning) reply.reasoning += reasoning;
          },
          onError: (err) => {
            if (!controller.signal.aborted) reply.error = err.message;
          },
        },
        controller.signal,
      );
      if (options.agent === 'dsh' && !controller.signal.aborted && !reply.error) {
        // The remote journal owns history and compaction. Reload its current display surface
        // instead of pretending that the console's prior transcript remains authoritative.
        const snapshot = await api.getDshSession(sessionId);
        if (controller.signal.aborted) return;
        this.turns = dshHistory(snapshot.records).map(message => ({
          id: this.nextId++, role: message.role, content: message.text,
          reasoning: message.reasoning, error: null,
        }));
      }
    } catch (e) {
      if (!controller.signal.aborted) {
        reply.error = e instanceof Error ? e.message : String(e);
      }
    } finally {
      // A newer message may have replaced this one's controller; only clear our own.
      if (this.controller === controller) {
        this.controller = null;
        this.streaming = false;
      }
    }
  }
}

export const chatStore = new ChatStore();
