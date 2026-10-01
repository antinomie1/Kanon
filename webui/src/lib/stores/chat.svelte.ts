import { ApiError, api } from '../api/client';
import { streamChatCompletion } from '../api/sse';

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

  /** Session the node keeps this conversation in. */
  get sessionId(): string {
    return this.target ? `webui:chat:${this.target}` : 'webui:chat';
  }

  /** Switches who answers, which starts a new conversation. */
  choose(target: string) {
    if (target === this.target) return;
    this.stop();
    this.target = target;
    this.model = '';
    this.turns = [];
  }

  /** Starts over; the node forgets the conversation when the next message is sent. */
  clear() {
    this.stop();
    this.turns = [];
  }

  stop() {
    this.controller?.abort();
    this.controller = null;
    this.streaming = false;
  }

  async send(text: string, options: SendOptions) {
    if (this.streaming) return;
    const fresh = this.turns.length === 0;
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
      if (fresh) {
        try {
          await api.resetSession(this.sessionId);
        } catch (e) {
          // 404 means the node has never seen this session: it is already empty, as wanted.
          if (!(e instanceof ApiError && e.status === 404)) throw e;
        }
      }
      await streamChatCompletion(
        {
          session_id: this.sessionId,
          message: text,
          model: options.model,
          persona_id: options.personaId,
          tools: this.tools,
        },
        {
          onChunk: (delta, reasoning) => {
            reply.content += delta;
            if (reasoning) reply.reasoning += reasoning;
          },
          onError: (err) => {
            reply.error = err.message;
          },
        },
        controller.signal,
      );
    } catch (e) {
      reply.error = e instanceof Error ? e.message : String(e);
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
