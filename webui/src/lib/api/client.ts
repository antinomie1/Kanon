import type {
  ActivateProviderRequest,
  ActivateProviderResponse,
  AdaptersResponse,
  CallPluginToolResponse,
  ChatCompletionRequest,
  ChatCompletionResponse,
  DeleteModelRequest,
  DeleteProviderRequest,
  DiscoverModelsRequest,
  DiscoverModelsResponse,
  FetchModelsRequest,
  FetchModelsResponse,
  InstallPluginResponse,
  InstanceMutationResponse,
  InstanceRequest,
  InstancesResponse,
  McpCatalog,
  McpServerView,
  McpStateResponse,
  MilkyConfigRequest,
  MilkyConfigView,
  MilkyTestReport,
  MilkyTestRequest,
  ModelSpec,
  ModelsResponse,
  NodeHealth,
  PersonasResponse,
  PluginConfigResponse,
  PluginConfigUpdateResponse,
  PluginStateResponse,
  PluginsResponse,
  ProvidersCatalog,
  QQOfficialPollLoginResponse,
  QQOfficialQrLoginResponse,
  ReplyPolicy,
  ReplyPolicyResponse,
  SessionsResponse,
  SetDefaultProviderRequest,
  SkillCatalog,
  SkillStateResponse,
  SystemConfig,
  TestProviderRequest,
  TestProviderResponse,
  ToolCatalog,
  UpsertMcpServerRequest,
  UpsertProviderRequest,
} from '../types';

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/**
 * Sends a multipart body and decodes the structured error envelope on failure.
 *
 * Uploads cannot use [`request`] because the browser must set the multipart boundary itself.
 */
async function sendMultipart<T>(path: string, body: FormData): Promise<T> {
  const res = await fetch(path, { method: 'POST', body });
  if (!res.ok) {
    let code = 'error';
    let message = `HTTP ${res.status}: ${res.statusText}`;
    try {
      const errJson = await res.json();
      if (errJson.error?.code) code = errJson.error.code;
      else if (errJson.code) code = errJson.code;
      if (errJson.error?.message) message = errJson.error.message;
      else if (errJson.message) message = errJson.message;
    } catch {
      // ignore json parse error
    }
    throw new ApiError(res.status, code, message);
  }
  return res.json() as Promise<T>;
}

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    headers: {
      'Content-Type': 'application/json',
      ...options?.headers,
    },
    ...options,
  });

  if (!res.ok) {
    let code = 'error';
    let message = `HTTP ${res.status}: ${res.statusText}`;
    try {
      const errJson = await res.json();
      if (errJson.error?.code) code = errJson.error.code;
      else if (errJson.code) code = errJson.code;
      if (errJson.error?.message) message = errJson.error.message;
      else if (errJson.message) message = errJson.message;
    } catch {
      // ignore json parse error
    }
    throw new ApiError(res.status, code, message);
  }

  return res.json() as Promise<T>;
}

export const api = {
  getHealth: () => request<NodeHealth>('/api/v1/health'),
  getMetrics: async () => {
    const res = await fetch('/api/v1/metrics');
    return res.text();
  },

  getSystemConfig: () => request<SystemConfig>('/api/v1/system/config'),
  // Node-wide reply policy inherited by every instance that carries no override.
  getReplyPolicy: () =>
    request<ReplyPolicyResponse>('/api/v1/system/reply-policy'),
  setReplyPolicy: (policy: ReplyPolicy) =>
    request<ReplyPolicyResponse>('/api/v1/system/reply-policy', {
      method: 'PUT',
      body: JSON.stringify(policy),
    }),

  getProviders: () => request<ProvidersCatalog>('/api/v1/providers'),
  // Named-endpoint directory: a model is addressed as `<provider>/<model-id>`, so every mutation
  // here changes how the node routes those references and returns the refreshed catalog.
  upsertProvider: (req: UpsertProviderRequest) =>
    request<ProvidersCatalog>('/api/v1/providers', {
      method: 'POST',
      body: JSON.stringify(req),
    }),
  setDefaultProvider: (req: SetDefaultProviderRequest) =>
    request<ProvidersCatalog>('/api/v1/providers/default', {
      method: 'PUT',
      body: JSON.stringify(req),
    }),
  deleteProvider: (req: DeleteProviderRequest) =>
    request<ProvidersCatalog>('/api/v1/providers/delete', {
      method: 'POST',
      body: JSON.stringify(req),
    }),
  // Provider selection is a node property, not a browser one: this persists it to
  // data/system.json and applies it to the running node immediately.
  activateProvider: (req: ActivateProviderRequest) =>
    request<ActivateProviderResponse>('/api/v1/providers/active', {
      method: 'PUT',
      body: JSON.stringify(req),
    }),
  clearActiveProvider: () =>
    request<ActivateProviderResponse>('/api/v1/providers/active', {
      method: 'DELETE',
    }),
  testProvider: (req?: TestProviderRequest) =>
    request<TestProviderResponse>('/api/v1/providers/test', {
      method: 'POST',
      body: JSON.stringify(req ?? {}),
    }),
  fetchModels: (req: FetchModelsRequest) =>
    request<FetchModelsResponse>('/api/v1/providers/models', {
      method: 'POST',
      body: JSON.stringify(req),
    }),

  // Model catalog: per-model context window, modalities and sampling overrides keyed by reference.
  getModels: () => request<ModelsResponse>('/api/v1/models'),
  // The body is the whole entry; the server forces `source: "manual"` because an operator's value
  // is a decision, not a discovery result.
  upsertModel: (spec: ModelSpec) =>
    request<ModelsResponse>('/api/v1/models', {
      method: 'PUT',
      body: JSON.stringify(spec),
    }),
  deleteModel: (req: DeleteModelRequest) =>
    request<ModelsResponse>('/api/v1/models/delete', {
      method: 'POST',
      body: JSON.stringify(req),
    }),
  discoverModels: (req: DiscoverModelsRequest) =>
    request<DiscoverModelsResponse>('/api/v1/models/discover', {
      method: 'POST',
      body: JSON.stringify(req),
    }),

  // Bot instances: the catalog that decides whether inbound platform traffic is answered.
  getInstances: () => request<InstancesResponse>('/api/v1/instances'),
  createInstance: (req: InstanceRequest) =>
    request<InstanceMutationResponse>('/api/v1/instances', {
      method: 'POST',
      body: JSON.stringify(req),
    }),
  updateInstance: (id: string, req: InstanceRequest) =>
    request<InstanceMutationResponse>(
      `/api/v1/instances/${encodeURIComponent(id)}`,
      {
        method: 'PUT',
        body: JSON.stringify(req),
      },
    ),
  deleteInstance: (id: string) =>
    request<InstanceMutationResponse>(
      `/api/v1/instances/${encodeURIComponent(id)}`,
      {
        method: 'DELETE',
      },
    ),

  getPlugins: () => request<PluginsResponse>('/api/v1/plugins'),
  // Enabling spawns the plugin host; disabling stops it, so the plugin leaves routing entirely.
  setPluginEnabled: (pluginId: string, enabled: boolean) =>
    request<PluginStateResponse>(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/enabled`,
      { method: 'PUT', body: JSON.stringify({ enabled }) },
    ),

  installPluginPath: (path: string) =>
    request<InstallPluginResponse>('/api/v1/plugins/install', {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
  installPluginArchive: (file: File) => {
    const formData = new FormData();
    formData.append('file', file);
    return sendMultipart<InstallPluginResponse>(
      '/api/v1/plugins/install',
      formData,
    );
  },
  getPluginConfig: (pluginId: string) =>
    request<PluginConfigResponse>(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/config`,
    ),
  // The gateway names the payload `values` and the concurrency token `version`; sending the
  // version it handed us is what makes a stale editor fail with 409 instead of overwriting a
  // change made elsewhere.
  updatePluginConfig: (
    pluginId: string,
    values: Record<string, unknown>,
    expectedVersion?: number,
  ) =>
    request<PluginConfigUpdateResponse>(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/config`,
      {
        method: 'PUT',
        body: JSON.stringify({ values, version: expectedVersion }),
      },
    ),
  // Tool catalog: every tool the model can call, grouped by provider.
  getTools: () => request<ToolCatalog>('/api/v1/tools'),

  // Skills: installed instruction bundles the model pulls in through `read_skill`.
  getSkills: () => request<SkillCatalog>('/api/v1/skills'),
  setSkillEnabled: (skillId: string, enabled: boolean) =>
    request<SkillStateResponse>(
      `/api/v1/skills/${encodeURIComponent(skillId)}/enabled`,
      { method: 'PUT', body: JSON.stringify({ enabled }) },
    ),
  removeSkill: (skillId: string) =>
    request<SkillStateResponse>(
      `/api/v1/skills/${encodeURIComponent(skillId)}`,
      {
        method: 'DELETE',
      },
    ),
  installSkillArchive: async (file: File, id?: string) => {
    const formData = new FormData();
    formData.append('file', file);
    if (id?.trim()) formData.append('id', id.trim());
    return sendMultipart<SkillCatalog['skills'][number]>(
      '/api/v1/skills',
      formData,
    );
  },
  installSkillPath: (path: string, id?: string) =>
    request<SkillCatalog['skills'][number]>('/api/v1/skills', {
      method: 'POST',
      body: JSON.stringify({ path, id: id?.trim() || null }),
    }),

  // MCP servers: definitions are edited here, the pool turns them into tool hosts.
  getMcpServers: () => request<McpCatalog>('/api/v1/mcp/servers'),
  upsertMcpServer: (serverId: string, req: UpsertMcpServerRequest) =>
    request<McpServerView>(
      `/api/v1/mcp/servers/${encodeURIComponent(serverId)}`,
      {
        method: 'PUT',
        body: JSON.stringify(req),
      },
    ),
  removeMcpServer: (serverId: string) =>
    request<McpStateResponse>(
      `/api/v1/mcp/servers/${encodeURIComponent(serverId)}`,
      { method: 'DELETE' },
    ),
  setMcpServerEnabled: (serverId: string, enabled: boolean) =>
    request<McpStateResponse>(
      `/api/v1/mcp/servers/${encodeURIComponent(serverId)}/enabled`,
      { method: 'PUT', body: JSON.stringify({ enabled }) },
    ),

  restartPlugin: (pluginId: string) =>
    request<{ success: boolean; message: string }>(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/restart`,
      { method: 'POST' },
    ),
  callPluginTool: (
    pluginId: string,
    toolName: string,
    args: Record<string, unknown> = {},
  ) =>
    request<CallPluginToolResponse>(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/tools/${encodeURIComponent(toolName)}`,
      {
        method: 'POST',
        body: JSON.stringify({ arguments: args }),
      },
    ),

  getAdapters: () => request<AdaptersResponse>('/api/v1/adapters'),

  // Milky platform adapter: an account-level configuration that is validated, persisted to
  // data/system.json and hot-applied to the running adapter in one call.
  getMilkyConfig: () =>
    request<MilkyConfigView>('/api/v1/adapters/milky/config'),
  updateMilkyConfig: (req: MilkyConfigRequest) =>
    request<MilkyConfigView>('/api/v1/adapters/milky/config', {
      method: 'PUT',
      body: JSON.stringify(req),
    }),
  testMilkyConfig: (req: MilkyTestRequest) =>
    request<MilkyTestReport>('/api/v1/adapters/milky/config/test', {
      method: 'POST',
      body: JSON.stringify(req),
    }),
  ingestEvent: (platform: string, payload: Record<string, unknown>) =>
    request<{ accepted: boolean; event_id: string }>(
      `/api/v1/adapters/${encodeURIComponent(platform)}/ingest`,
      {
        method: 'POST',
        body: JSON.stringify(payload),
      },
    ),
  requestQQOfficialLoginQr: (bindHost?: string) =>
    request<QQOfficialQrLoginResponse>('/api/v1/adapters/qqofficial/login/qr', {
      method: 'POST',
      body: JSON.stringify({ bind_host: bindHost }),
    }),
  pollQQOfficialLogin: (
    taskId: string,
    bindKey: string,
    bindHost?: string,
    autoSave = true,
  ) =>
    request<QQOfficialPollLoginResponse>(
      '/api/v1/adapters/qqofficial/login/poll',
      {
        method: 'POST',
        body: JSON.stringify({
          task_id: taskId,
          bind_key: bindKey,
          bind_host: bindHost,
          auto_save: autoSave,
        }),
      },
    ),

  getSessions: (limit = 50, offset = 0) =>
    request<SessionsResponse>(
      `/api/v1/sessions?limit=${limit}&offset=${offset}`,
    ),
  resetSession: (sessionId: string) =>
    request<{ success: boolean }>(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/reset`,
      {
        method: 'POST',
      },
    ),
  setSessionPersona: (sessionId: string, persona: string) =>
    request<{ success: boolean }>(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/persona`,
      {
        method: 'POST',
        body: JSON.stringify({ persona }),
      },
    ),

  getPersonas: () => request<PersonasResponse>('/api/v1/personas'),

  chatCompletion: (req: ChatCompletionRequest) =>
    request<ChatCompletionResponse>('/api/v1/chat/completions', {
      method: 'POST',
      body: JSON.stringify(req),
    }),
};
