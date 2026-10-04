/** Native DSH plugin: real agent identity, native context assembly, Kanon's shared tool policy. */
import type { Context } from "@deepseek-ai/cordis";
import type { Agent } from "@deepseek-ai/dsh-agent";
import "@deepseek-ai/dsh-api-session-controller";
import "@deepseek-ai/dsh-system-prompt";
import "@deepseek-ai/dsh-tools";
import z from "@deepseek-ai/schemastery";
import { createMcpToolDefinition } from "@deepseek-ai/dsh-mcp-client";
import { Remote, TypertRemoteService } from "@deepseek-ai/dsh-typert-protocol";
import { BridgeConnection, type ConnectionConfig, type TurnCatalog } from "./wire.ts";

/** Plugin configuration is read by DSH's own loader, never environment variables. */
export const Config = z.object({
  coreSocket: z.string().required(),
  tokenFile: z.string(),
  timeoutSeconds: z.number().min(1).max(3600).default(600),
});

interface PreparedTurn {
  requestId: string;
  catalog: TurnCatalog;
  turn?: number;
  dispose: (() => void)[];
}

/** The optional native service required before Kanon submits a platform prompt. */
export default class KanonBridge extends TypertRemoteService {
  static inject = ["agents", "sessionController", "tools", "systemPrompt"];
  static Config = Config;
  private readonly connection: BridgeConnection;
  private readonly prepared = new WeakMap<Agent, PreparedTurn>();
  private readonly owned = new Set<Agent>();

  /** Installs scoped admission and call guards; ordinary DSH sessions are unaffected. */
  constructor(ctx: Context, config: ConnectionConfig) {
    super(ctx, "kanonBridge", { namespace: "kanon" });
    this.connection = new BridgeConnection(config);
    ctx.effect(() => async () => {
      // Keep scoped restrictions installed until active work has drained. Otherwise unloading
      // the plugin could expose unrestricted global tools halfway through a platform turn.
      const agents = [...this.owned];
      for (const agent of agents) agent.cancel({ kind: "disposed" });
      await Promise.all(agents.map(agent => agent.whenIdle()));
      for (const agent of agents) this.release(agent);
      this.connection.close();
    });
    ctx.on("agent/disposed", ({ agent }) => this.release(agent));
    ctx.on("agent/pre-step", async ({ agent, messages, turn, signal }, next) => {
      if (!agent.session.id.startsWith("kanon-")) return next();
      const prepared = this.prepared.get(agent);
      if (!prepared) throw new Error("This Kanon session has no admitted platform turn");
      // Bind once to the real user-message source written by DSH's SessionController. Neither
      // a model argument nor a different prompt submitted from the native UI can claim it.
      const matching = messages.some(message =>
        "rpcId" in message.source && message.source.rpcId === prepared.requestId);
      if (messages.some(message => "rpcId" in message.source && message.source.rpcId !== prepared.requestId)) {
        throw new Error("Another prompt cannot enter an active Kanon turn");
      }
      if (prepared.turn === undefined) {
        if (!matching) throw new Error("Prompt does not own the prepared Kanon turn");
        prepared.turn = turn;
      } else if (prepared.turn !== turn) {
        throw new Error("Another prompt cannot enter an active Kanon turn");
      }
      const current = await this.connection.describe(agent.session.id, prepared.requestId, signal);
      if (current.leaseId !== prepared.catalog.leaseId) throw new Error("Kanon tool lease expired");
      return next();
    });
  }

  /** Prepares one live native agent and acknowledges the exact core-issued lease. */
  @Remote
  async prepare(sessionId: string, requestId: string, signal: AbortSignal): Promise<{ leaseId: string; requestId: string }> {
    if (!sessionId.startsWith("kanon-") || !requestId) throw new Error("Invalid Kanon turn identity");
    const resolved = await this.ctx.sessionController.resolveAgent(sessionId as Agent["id"]);
    if ("error" in resolved) throw resolved.error;
    const agent = resolved.agent;
    if (agent.status !== "idle") throw new Error("DSH session is busy");
    return agent.runMaintenance(async maintenanceSignal => {
      const combined = AbortSignal.any([signal, maintenanceSignal]);
      const catalog = await this.connection.describe(sessionId, requestId, combined);
      combined.throwIfAborted();
      const previous = this.prepared.get(agent);
      previous?.dispose.forEach(dispose => dispose());
      this.prepared.delete(agent);
      const prepared: PreparedTurn = { requestId, catalog, dispose: [] };
      try {
        // Platform turns use the same tool surface and Bash gate as builtin. Native DSH tools
        // from the global scope cannot bypass the instance's plugin/MCP/skill permissions.
        prepared.dispose.push(agent.ctx.tools.restrict({ allow: [] }));
        for (const tool of catalog.tools) {
          const definition = createMcpToolDefinition(agent.ctx, {
            name: tool.name, rawName: tool.name, description: tool.description,
            inputSchema: tool.parameters,
            call: async (args, execution) => {
              if (execution.agent !== agent || this.prepared.get(agent) !== prepared) {
                throw new Error("Kanon tool belongs to a different or expired agent turn");
              }
              const result = await this.connection.call(execution.agent.session.id, catalog.leaseId,
                execution.callId, tool.name, args, execution.signal);
              return { content: [{ type: "text", text: result.text }], isError: !result.success };
            },
          });
          prepared.dispose.push(agent.ctx.tools.register(definition));
        }
        prepared.dispose.push(agent.ctx.systemPrompt.section({
          name: "kanon", order: 100, text: catalog.instructions, interpolate: false,
        }));
        this.prepared.set(agent, prepared);
        this.owned.add(agent);
        return { leaseId: catalog.leaseId, requestId };
      } catch (error) {
        prepared.dispose.reverse().forEach(dispose => dispose());
        throw error;
      }
    });
  }

  private release(agent: Agent): void {
    this.prepared.get(agent)?.dispose.reverse().forEach(dispose => dispose());
    this.prepared.delete(agent);
    this.owned.delete(agent);
  }
}
