/** The DSH plugin's bounded gRPC connection to the existing core IPC listener. */
import * as grpc from "@grpc/grpc-js";
import * as loader from "@grpc/proto-loader";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { fromProtoStruct, toProtoStruct } from "../../sdks/typescript/src/sdk/struct.ts";

/** Connection coordinates supplied by the DSH plugin configuration file. */
export interface ConnectionConfig {
  coreSocket: string;
  tokenFile?: string;
  timeoutSeconds: number;
}

/** One immutable Kanon tool surface for the admitted prompt. */
export interface TurnCatalog {
  leaseId: string;
  instructions: string;
  tools: { name: string; description: string; parameters: Record<string, unknown> }[];
}

/** Reuses one channel; never retries a mutation whose outcome may already be committed. */
export class BridgeConnection {
  private client?: grpc.Client;
  constructor(private readonly config: ConnectionConfig) {}

  /** Releases the pooled channel when the native plugin unloads. */
  close(): void { this.client?.close(); }

  /** Validates prompt ownership and reads only that turn's advertised tools. */
  async describe(sessionId: string, requestId: string, signal: AbortSignal): Promise<TurnCatalog> {
    const value = await this.invoke("DescribeTurn", { sessionId, requestId }, signal);
    return {
      leaseId: value.leaseId,
      instructions: value.instructions,
      tools: value.tools.map((tool: any) => ({ ...tool, parameters: fromProtoStruct(tool.parameters) })),
    };
  }

  /** Uses genuine execution identity supplied by the native plugin, outside model arguments. */
  async call(sessionId: string, leaseId: string, callId: string, name: string,
    args: Record<string, unknown>, signal: AbortSignal): Promise<{ success: boolean; text: string }> {
    return this.invoke("CallTool", { sessionId, leaseId, callId, name, arguments: toProtoStruct(args) }, signal);
  }

  private async invoke(method: string, input: unknown, signal: AbortSignal): Promise<any> {
    signal.throwIfAborted();
    if (!this.client) {
      const protoRoot = fileURLToPath(new URL(import.meta.url.endsWith(".ts") ? "../../proto/" : "./proto/", import.meta.url));
      const definition = loader.loadSync(`${protoRoot}kanon/v1/agent.proto`, {
        includeDirs: [protoRoot], defaults: true, longs: String,
      });
      const service = (grpc.loadPackageDefinition(definition) as any).kanon.agent.v1.AgentBridgeService;
      let address = `unix:${this.config.coreSocket}`;
      if (process.platform === "win32") {
        address = (await readFile(this.config.coreSocket, "utf8")).trim();
        if (!/^127\.0\.0\.1:\d+$/.test(address) || !this.config.tokenFile) {
          throw new Error("Windows bridge requires an authenticated loopback endpoint");
        }
      }
      // No awaits between checking and publishing the channel on Unix. On Windows concurrent
      // setup can race the file read, so discard the second candidate instead of leaking it.
      if (!this.client) this.client = new service(address, grpc.credentials.createInsecure(), {
        "grpc.max_receive_message_length": 4 * 1024 * 1024,
        "grpc.max_send_message_length": 4 * 1024 * 1024,
      });
    }
    const metadata = new grpc.Metadata();
    if (this.config.tokenFile) {
      const token = (await readFile(this.config.tokenFile, "utf8")).trim();
      if (!/^[a-fA-F0-9]{64}$/.test(token)) throw new Error("Invalid IPC token file");
      metadata.set("x-kanon-auth-token", token);
    }
    signal.throwIfAborted();
    return new Promise((resolve, reject) => {
      const client = this.client as grpc.Client & Record<string, Function>;
      const call = client[method](input, metadata,
        { deadline: Date.now() + this.config.timeoutSeconds * 1000 },
        (error: Error | null, value: unknown) => {
          signal.removeEventListener("abort", abort);
          if (error) reject(error); else resolve(value);
        });
      const abort = () => call.cancel();
      signal.addEventListener("abort", abort, { once: true });
      if (signal.aborted) abort();
    });
  }
}
