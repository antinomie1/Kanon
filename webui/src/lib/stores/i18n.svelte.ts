export type Locale = 'zh' | 'en';

export const dictionaries = {
  en: {
    // Navigation
    'nav.overview': 'Overview',
    'nav.instances': 'Instances',
    'nav.chat': 'Chat',
    'nav.pipeline': 'Pipeline & Logs',
    'nav.plugins': 'Plugins & Adapters',
    'nav.sessions': 'Sessions',
    'nav.personas': 'Personas',
    'nav.playground': 'Chat',
    'nav.providers': 'Model Providers',
    'nav.models': 'Model Catalog',
    'nav.system': 'System Settings',

    // Titles & Subtitles
    'title.instances': 'Bot Instances',
    'subtitle.instances':
      'Which bots answer on which platforms, with their persona, model and policies',
    'title.overview': 'Node Overview & Health',
    'subtitle.overview':
      'Microkernel node runtime, process supervisor, and Prometheus exposition',
    'title.chat': 'Interactive Chat',
    'subtitle.chat':
      'Interactive streaming chat with multi-turn reasoning and tool calling inspection',
    'title.pipeline': 'Pipeline Tracing & Log Console',
    'subtitle.pipeline':
      'Real-time WebSocket streaming for pipeline lifecycle transitions and server logs',
    'title.plugins': 'Plugins & Platform Adapters',
    'subtitle.plugins':
      'Out-of-process gRPC plugin hosts, dynamic JSON schemas, and platform adapters',
    'title.sessions': 'Conversation Sessions',
    'subtitle.sessions':
      'Tracked conversations, token usage and per-session persona binding',
    'title.personas': 'Personas',
    'subtitle.personas': 'Manage the persona presets your bot can use',
    'title.playground': 'Interactive Chat',
    'subtitle.playground':
      'Interactive streaming chat with multi-turn reasoning and tool calling inspection',
    'title.providers': 'Model Providers',
    'subtitle.providers':
      'Provider endpoints, connectivity testing, and the one global default model',
    'title.models': 'Model Catalog',
    'subtitle.models':
      'Per-model context window, modalities and sampling overrides, keyed by provider reference',
    'title.system': 'System Configuration',
    'subtitle.system':
      'Kernel IPC socket, runtime & data paths, memory window and policy defaults',

    // General & Status
    'status.healthy': 'Healthy',
    'status.connecting': 'Connecting',
    'status.offline': 'Offline',
    'status.connected': 'Connected',
    'status.disconnected': 'Disconnected',
    'status.reconnecting': 'Reconnecting',
    'common.retry': 'Retry now',
    'common.refresh': 'Refresh',
    'common.search': 'Search...',
    'common.clear': 'Clear',
    'common.save': 'Save Changes',
    'common.cancel': 'Cancel',
    'common.close': 'Close',
    'common.loading': 'Loading...',
    'common.error': 'Error',
    'common.success': 'Success',
    'common.appearance': 'Appearance',
    'common.language': 'Language',
    'common.command_menu': 'Command Menu',
    'common.events': 'Events',
    'common.logs': 'Logs',
    'common.version': 'Version',
    'common.uptime': 'Uptime',
    'common.latency': 'Latency',

    // Overview Cards
    'overview.node_status': 'Node Status',
    'overview.resident_memory': 'Resident Memory (RSS)',
    'overview.virtual_memory': 'Virtual Memory',
    'overview.instances_enabled': 'Running bots',
    'overview.instances_hint':
      'Inbound platform messages are answered only while an instance claims the adapter.',
    'overview.llm_engine': 'LLM Gateway Engine',
    'overview.llm_ready': 'Configured & Ready',
    'overview.llm_disabled': 'Disabled (Unset)',
    'overview.plugin_hosts': 'Active Plugin Hosts',
    'overview.plugins_loaded': 'Plugins Loaded',
    'overview.sessions_total': 'Total Sessions',
    'overview.sessions_active': 'Active Sessions',
    'overview.ws_connections': 'Active WebSockets',
    'overview.event_listeners': 'Event Listeners',
    'overview.log_listeners': 'Log Listeners',
    'overview.quick_actions': 'Quick Navigation',

    // Pipeline & Logs
    'pipeline.live_events': 'Pipeline Event Stream',
    'pipeline.server_logs': 'Structured Server Logs',
    'pipeline.autoscroll': 'Auto-scroll',
    'pipeline.filter_stage': 'Filter Stage',
    'pipeline.filter_level': 'Log Level',
    'pipeline.empty_events':
      'No pipeline events recorded yet. Send a message to see lifecycle stages.',
    'pipeline.empty_logs': 'No log records received yet.',
    'pipeline.offline': 'Not connected to the node — this stream is offline.',
    'pipeline.reconnect': 'Reconnect',
    'pipeline.no_instance_hint':
      'No bot instance is enabled, so inbound messages are dropped before the pipeline runs. Enable one on the Instances page.',
    'pipeline.level_filter_hint':
      'Log level filter is set to {level}; select ALL to see every record.',

    // Plugins & Adapters
    'plugins.hosts_title': 'Supervised Plugin Hosts',
    'plugins.adapters_title': 'Platform Adapters',
    'plugins.host_id': 'Host ID',
    'plugins.runtime': 'Runtime',
    'plugins.pid': 'PID',
    'plugins.hostless_title': 'Plugins without a running host',
    'plugins.hostless_hint':
      'A disabled plugin has no process: its commands, tools and adapter are inactive until you enable it again.',
    'plugins.not_running': 'Not running',
    'plugins.enable': 'Enable',
    'plugins.disable': 'Disable',
    'plugins.disabled': 'Disabled',
    'plugins.crashed': 'Crashed',
    'plugins.restarts': 'restarts',
    'plugins.restart_unavailable':
      'This host runs no plugin, so it cannot be addressed for a restart',
    'plugins.restart': 'Restart Process',
    'plugins.config': 'Configure',
    'plugins.commands': 'Commands',
    'plugins.tools': 'Tools',
    'plugins.no_hosts': 'No out-of-process plugin hosts running.',
    'plugins.no_adapters': 'No platform adapters registered.',
    'plugins.config_modal_title': 'Plugin Configuration',
    'plugins.cas_version': 'CAS Version',
    'plugins.install': 'Install Plugin',
    'plugins.tab_plugins': 'Plugins',
    'plugins.tab_mcp': 'MCP Servers',
    'plugins.tab_tools': 'Tools',

    // Tool catalog
    'tools.title': 'Tool Catalog',
    'tools.subtitle':
      'Every tool the model can call right now, and who provides it',
    'tools.total': 'Total tools',
    'tools.source_builtin': 'Builtin',
    'tools.source_plugin': 'Plugin',
    'tools.source_mcp': 'MCP',
    'tools.search_placeholder': 'Filter by name, description or provider',
    'tools.parameters': 'Schema',
    'tools.empty': 'No tool is available.',
    'tools.empty_hint':
      'Register native tools, start a plugin host or enable an MCP server to expose tools to the model.',
    'tools.no_match': 'No tool matches the current filter.',
    'plugins.tab_adapters': 'Adapters',
    'plugins.tab_skills': 'Skills',

    // MCP servers
    'mcp.title': 'MCP Servers',
    'mcp.subtitle':
      'Model Context Protocol servers whose tools are offered to every instance that allows them',
    'mcp.add': 'Add Server',
    'mcp.add_title': 'Add MCP Server',
    'mcp.edit_title': 'Edit MCP Server',
    'mcp.edit': 'Edit definition',
    'mcp.remove': 'Remove server',
    'mcp.empty': 'No MCP servers configured.',
    'mcp.empty_hint':
      'Add a stdio or HTTP server to expose its tools to the model.',
    'mcp.disabled': 'disabled',
    'mcp.tools': 'tools',
    'mcp.failures': 'failed probes',
    'mcp.field_id': 'Identifier',
    'mcp.field_name': 'Display name',
    'mcp.field_transport': 'Transport',
    'mcp.transport_stdio': 'stdio (child process)',
    'mcp.transport_http': 'HTTP (remote endpoint)',
    'mcp.field_command': 'Command',
    'mcp.field_args': 'Arguments',
    'mcp.args_hint': 'Space separated; passed to the command in order.',
    'mcp.field_url': 'URL',
    'mcp.field_env': 'Environment',
    'mcp.field_headers': 'Headers',
    'mcp.keyvalues_hint': 'One KEY=VALUE pair per line.',

    // Skills
    'skills.title': 'Skills',
    'skills.subtitle':
      'Installed instruction bundles; only their descriptions enter the prompt until the model reads one',
    'skills.install': 'Install Skill',
    'skills.install_title': 'Install Skill',
    'skills.remove': 'Remove skill',
    'skills.empty': 'No skills installed.',
    'skills.empty_hint':
      'Upload a zip archive or point at a directory containing SKILL.md.',
    'skills.archive': 'Archive',
    'skills.archive_hint':
      'A .zip holding SKILL.md at its root or inside a single top-level directory.',
    'skills.path': 'Local directory',
    'skills.path_hint': 'Directory containing a SKILL.md on this node.',
    'skills.field_id': 'Identifier (optional)',
    'skills.id_hint': 'Defaults to the archive folder or directory name.',
    'plugins.install_modal_title': 'Install New Plugin',
    'plugins.tab_local_path': 'Local Directory',
    'plugins.tab_upload_archive': 'Upload Package (.kpk / .zip)',
    'plugins.local_path_label': 'Plugin Directory Path',
    'plugins.local_path_placeholder': 'e.g. ./plugins/demo_weather',
    'plugins.local_path_help':
      'Path on server containing a valid plugin.toml manifest',
    'plugins.archive_label': 'Select Package File',
    'plugins.archive_help': 'Standard .kpk distribution archive or .zip bundle',
    'plugins.install_btn': 'Start Installation',
    'plugins.installing': 'Installing & Activating...',
    'plugins.install_success': 'Plugin installed and loaded successfully!',
    'plugins.runtime_unavailable': 'Runtime Unavailable',
    'adapters.qq_qr_btn': 'QQ Official QR Bind',
    'adapters.qq_qr_title': 'QQ Official Bot Quick Bind',
    'adapters.qq_qr_desc':
      'Scan the QR code with Mobile QQ to authorize the bot and automatically configure credentials.',
    'adapters.qq_qr_generating': 'Generating QR binding task...',
    'adapters.qq_qr_waiting': 'Waiting for authorization in Mobile QQ...',
    'adapters.qq_qr_success':
      'Binding successful! AppID and secret configured and hot-reloaded.',
    'adapters.qq_qr_expired': 'QR code expired. Click to refresh.',
    'adapters.qq_qr_open_link': 'Open in Mobile QQ',
    'adapters.qq_qr_copy_link': 'Copy Auth URL',
    'adapters.qq_qr_copied': 'Copied to clipboard!',
    'adapters.qq_qr_retry': 'Regenerate QR Code',
    'adapters.qq_title': 'QQ Official Bot Adapter',
    'adapters.qq_enabled': 'Enable QQ Official adapter',
    'adapters.qq_enabled_hint':
      'Saving connects to or disconnects from the QQ gateway; no restart needed.',
    'adapters.qq_unsaved': 'Unsaved change (press Save & apply)',
    'adapters.qq_appid': 'Bot AppID',
    'adapters.qq_secret': 'Bot AppSecret',
    'adapters.qq_secret_keep': 'Leave empty to keep the stored secret',
    'adapters.qq_secret_none': 'No secret configured',
    'adapters.qq_sandbox': 'Sandbox environment',
    'adapters.qq_use_markdown':
      'Send native Markdown (needs the Markdown permission)',
    'adapters.qq_save': 'Save & apply',
    'adapters.qq_saving': 'Saving...',
    'adapters.qq_saved':
      'Saved and applied; see the status below for the connection result',
    'adapters.qq_bot': 'Bot',
    'adapters.qq_state_disabled': 'Disabled',
    'adapters.qq_state_connecting': 'Connecting',
    'adapters.qq_state_connected': 'Connected',
    'adapters.qq_state_disconnected': 'Disconnected',
    'adapters.qq_state_stopped': 'Stopped',
    'adapters.qq_not_hosted':
      'This node does not host the QQ Official adapter.',

    'reply.quote': 'Quote the message being answered',
    'reply.quote_hint':
      "In groups and channels, the bot's reply quotes the message it answers so everyone sees who it is talking to. Private chats are never quoted.",
    'context.expand_forward': 'Expand merged forwards',
    'context.expand_forward_hint':
      'Show the model the messages inside a forwarded chat log (with their pictures for vision models) instead of only its title.',
    'events.title': 'Event responses',
    'events.updated': 'Event responses updated',
    'events.hint':
      'Platform events that are not messages. The bot only reacts to the ones switched on here, and then regardless of the reply policy.',
    'events.welcome': 'Welcome new group members',
    'events.welcome_hint':
      'When someone joins a group, the bot greets them in its own persona.',
    'events.greet': 'Say hello when added',
    'events.greet_hint':
      'When the bot is added to a group or as a friend, it introduces itself.',
    'events.poke': 'Respond to pokes',
    'events.poke_hint': 'When someone pokes (nudges) the bot, it answers.',
    'events.recall': 'Tell the model about recalls',
    'events.recall_hint':
      'When a message the model already saw is recalled, its next turn in that conversation says so, so it stops referring to it. Recalled messages it never saw are not revealed.',
    'adapters.automation': 'Automation',
    'adapters.auto_accept_friends': 'Accept friend requests automatically',
    'adapters.auto_accept_group_invites':
      'Accept group invitations automatically',
    'adapters.reaction_ack':
      'React with 👍 to a group message the bot is about to answer',
    'adapters.qq_typing':
      'Show "typing…" in private chats while the model works (uses one of QQ\'s passive-reply slots)',

    // OneBot v11 adapter
    'adapters.onebot_title': 'OneBot v11 Adapter',
    'adapters.onebot_unsaved': 'Unsaved change (press Save & apply)',
    'adapters.onebot_enabled': 'Enable OneBot v11 adapter',
    'adapters.onebot_enabled_hint':
      'Saving starts or stops the configured WebSocket connection.',
    'adapters.onebot_ws_url': 'WebSocket URL',
    'adapters.onebot_transport': 'Connection mode',
    'adapters.onebot_transport_forward': 'Forward WebSocket',
    'adapters.onebot_transport_reverse': 'Reverse WebSocket',
    'adapters.onebot_forward_hint':
      'Kanon connects to the combined API and event WebSocket endpoint of NapCat, Lagrange, or another OneBot v11 implementation.',
    'adapters.onebot_reverse_hint':
      'Kanon listens on this ws:// IP address and port. Configure the protocol implementation to connect to the reachable address, path and matching token. One account per listener.',
    'adapters.onebot_token': 'Access Token',
    'adapters.onebot_token_keep': 'Leave empty to keep the stored token',
    'adapters.onebot_token_none': 'No token configured',
    'adapters.onebot_token_show': 'Show token',
    'adapters.onebot_token_hide': 'Hide token',
    'adapters.onebot_clear_token': 'Remove the stored token',
    'adapters.onebot_platform': 'Platform identifier',
    'adapters.onebot_display_name': 'Display name',
    'adapters.onebot_readonly_hint':
      'Identity fields are fixed while the node runs. Change them in the saved configuration and restart.',
    'adapters.onebot_save': 'Save & apply',
    'adapters.onebot_saving': 'Saving...',
    'adapters.onebot_saved':
      'Settings saved and applied; see connection status below',
    'adapters.onebot_login': 'Account ID',
    'adapters.onebot_state_disabled': 'Disabled',
    'adapters.onebot_state_connecting': 'Connecting',
    'adapters.onebot_state_listening':
      'Listening — waiting for protocol client',
    'adapters.onebot_state_connected': 'Connected',
    'adapters.onebot_state_disconnected': 'Disconnected',
    'adapters.onebot_state_stopped': 'Stopped',
    'adapters.onebot_not_hosted':
      'This node does not host the OneBot v11 adapter.',

    // Milky platform adapter
    'adapters.milky_title': 'Milky Protocol Adapter',
    'adapters.empty': 'No platform adapter is registered on this node.',
    'adapters.milky_unsaved': 'Unsaved change (press Save & apply)',
    'adapters.milky_enabled': 'Enable Milky adapter',
    'adapters.milky_enabled_hint':
      'While disabled the adapter holds no connection and every delivery fails explicitly.',
    'adapters.milky_base_url': 'Protocol implementation base URL',
    'adapters.milky_transport': 'Inbound transport',
    'adapters.milky_transport_sse': 'Server-Sent Events',
    'adapters.milky_transport_ws': 'WebSocket',
    'adapters.milky_token': 'Access Token',
    'adapters.milky_token_keep': 'Leave empty to keep the stored token',
    'adapters.milky_token_none': 'No token configured',
    'adapters.milky_token_show': 'Show token',
    'adapters.milky_token_hide': 'Hide token',
    'adapters.milky_clear_token': 'Remove the stored token',
    'adapters.milky_platform': 'Platform identifier',
    'adapters.milky_display_name': 'Display name',
    'adapters.milky_readonly_hint':
      'The platform identifier and display name identify the adapter inside the node registry; changing either requires a restart.',
    'adapters.milky_save': 'Save & apply',
    'adapters.milky_saving': 'Saving...',
    'adapters.milky_saved': 'Saved, persisted and applied',
    'adapters.milky_test': 'Test connection',
    'adapters.milky_testing': 'Testing...',
    'adapters.milky_test_ok': 'Endpoint reachable',
    'adapters.milky_login': 'Signed-in account',
    'adapters.milky_impl': 'Protocol implementation',
    'adapters.milky_counters': 'Ingested / delivered',
    'adapters.milky_events': 'events',
    'adapters.milky_rejected': 'rejected',
    'adapters.milky_last_event': 'Last event',
    'adapters.milky_state_disabled': 'Disabled',
    'adapters.milky_state_connecting': 'Connecting',
    'adapters.milky_state_connected': 'Connected',
    'adapters.milky_state_error': 'Connection error',
    'adapters.milky_not_hosted':
      'This node does not host the Milky adapter; it was not registered at startup.',

    // Sessions & Personas
    'sessions.active_sessions': 'Tracked Sessions',
    'sessions.session_id': 'Session ID',
    'sessions.turns': 'Turns',
    'sessions.tokens': 'Tokens Used',
    'sessions.persona': 'Active Persona',
    'sessions.reset': 'Reset History',
    'sessions.reset_confirm': "Reset conversation history for session '{id}'?",
    'sessions.reset_failed': 'Reset failed',
    'sessions.binding': 'Switching persona...',
    'sessions.bind_failed': 'Switch failed',
    'sessions.bind_title': 'Persona for this session',
    'sessions.bind_select': 'Persona',
    'sessions.bind_none': 'None — use the base assistant',
    'sessions.bind_hint':
      'Takes effect on the next message. Conversations of a bot instance follow that instance’s persona.',
    'sessions.bind_apply': 'Apply',

    // Personas
    'personas.heading': 'Persona presets',
    'personas.intro':
      'A persona is the fixed instruction text placed at the very top of every request. Add the ones you want and pick them per instance or per session. Only the base assistant ships with the node.',
    'personas.add': 'New persona',
    'personas.edit': 'Edit',
    'personas.edit_title': 'Edit persona',
    'personas.delete': 'Delete persona',
    'personas.delete_confirm': 'Delete the persona “{name}”?',
    'personas.in_use': 'Used by instance: {instances}',
    'personas.builtin': 'built in',
    'personas.builtin_hint':
      'Used whenever nothing else is chosen. It cannot be edited or removed.',
    'personas.empty':
      'You have not added a persona yet. Only the base assistant is available.',
    'personas.name': 'Name',
    'personas.name_placeholder': 'e.g. Code reviewer',
    'personas.description': 'Description',
    'personas.prompt': 'Prompt',
    'personas.prompt_placeholder': 'You are …',
    'personas.prompt_hint':
      'Sent as-is at the top of every request, which keeps the provider’s prompt cache warm. Keep it fixed: things that change while the bot runs (time, who is speaking) are added after it automatically and do not belong here.',
    'personas.save': 'Save',
    'sessions.no_sessions': 'No conversation sessions recorded yet.',

    // Playground
    'playground.model': 'Model',
    'playground.persona_override': 'Persona Override',
    'playground.enable_tools': 'Enable Tool Calling',
    'playground.send': 'Send',
    'playground.placeholder':
      'Type a prompt to test conversational reasoning or tool dispatch...',
    'playground.tools_executed': 'Executed Tools',
    'playground.empty_chat':
      'Start a sandbox chat turn to test the default model and tool calling.',

    // Providers & System
    'providers.protocol': 'Protocol',
    'providers.base_url': 'Base URL',
    'providers.api_key': 'API Key',
    'providers.api_key_set': 'Configured (Masked)',
    'providers.api_key_unset': 'Not Configured',
    'providers.temperature': 'Temperature',
    'providers.max_tokens': 'Max Tokens',
    'providers.testing': 'Testing connection...',
    'providers.test_prompt': 'Test Prompt',
    'providers.test_result': 'Test Result',
    'providers.latency_ms': 'Round-trip Latency',
    'providers.response_preview': 'Response Preview',
    'providers.presets_title': 'Supported Provider Presets',
    'providers.use_preset': 'Use Preset',
    'providers.system_config_title': 'System & Node Configuration',
    'providers.ipc_socket': 'Core IPC Socket',
    'providers.run_dir': 'Run Directory',
    'providers.data_dir': 'Data Directory',
    'providers.signature_verify': 'HMAC Signature Verification',
    'providers.env_title': 'Runtime Environment',
    'providers.os_arch': 'OS & Architecture',
    'providers.rust_edition': 'Rust Edition',

    // Global default model (one decision for the whole node)
    'providers.default_model_title': 'Global default model',
    'providers.default_model_desc':
      'The model the bot answers with. Instances can still pick a different one for themselves; everything else uses this.',
    'providers.default_model_unset': '— Not set —',
    'providers.default_model_none':
      'No default model is set — the bot will not answer plain messages until you pick one.',
    'providers.default_model_no_models':
      'No models yet. Use “Discover models” on a provider below (or add one by hand), then pick the default here.',
    'providers.serves_default': 'Serves the global default model',
    'providers.delete_confirm':
      'Delete this provider? Its models are removed from the catalog with it.',
    'providers.delete_default_warning':
      'It serves the global default model, so the node will have no default model afterwards.',
    'providers.test_title': 'Connectivity & models',
    'providers.test_model_placeholder': 'model id, e.g. deepseek-chat',
    'providers.test_key_hint':
      'The stored key is used by the node itself and never sent to your browser. Values typed above (URL, protocol, key) are tested as-is, so you can check an edit before saving it.',

    // Named provider directory
    'providers.context_length': 'Context window',
    'providers.capabilities': 'Capabilities',
    'providers.directory_title': 'Provider directory',
    'providers.edit_title': 'Edit provider',
    'providers.add_provider': 'Add provider',
    'providers.select_hint': 'Select a provider to edit it.',
    'providers.empty_title': 'No model provider configured yet',
    'providers.empty_hint':
      'Configure an endpoint first (API base URL and credential), then discover the models it serves. A model is always addressed as provider/model-id.',
    'providers.name': 'Provider name (identifier)',
    'providers.name_hint':
      'Used as the prefix of every model reference this endpoint serves, e.g. deepseek/deepseek-chat.',
    'providers.base_url_default': 'Provider default endpoint',
    'providers.api_key_keep': 'Leave empty to keep the stored credential',
    'providers.api_key_clear': 'Remove the stored credential',
    'providers.api_key_hint':
      'Online providers need a valid API key before models can be listed or tested.',
    'providers.api_key_configured': 'Credential stored',
    'providers.temperature_hint':
      'Sampling temperature applied to models on this endpoint.',
    'providers.max_tokens_hint':
      'Generation ceiling applied to models on this endpoint.',
    'providers.save': 'Save changes',
    'providers.saved': 'Saved',
    'providers.delete': 'Delete provider',
    'providers.models_in_catalog': '{count} models in catalog',
    'providers.test': 'Test connectivity',
    'providers.test_model_label': 'Model to probe',
    'providers.discover': 'Discover models',
    'providers.discovering': 'Discovering...',
    'providers.discover_done': 'Discovered {count} models, stored {persisted}',
    'providers.discover_hint':
      'Reads the endpoint model listing and stores it. Entries you edited by hand are never overwritten.',
    'providers.quick_config': 'Quick setup',
    'providers.quick_config_title': 'Provider templates',
    'providers.quick_config_hint':
      'Pick a common service template to prefill the protocol and base URL, then add the API key.',
    'providers.manual_add_title': 'Add model provider',
    'providers.create_provider': 'Create provider',
    'providers.preset_applied':
      'Template applied: fill in the credential to finish.',
    'providers.no_providers': 'No provider endpoint configured.',

    // Model catalog
    'models.title': 'Model Catalog',
    'models.subtitle':
      'Context window, modalities and sampling overrides for every model the node may route to',
    'models.total': 'Catalog entries',
    'models.default_model': 'Global default model',
    'models.default_none': 'Not set',
    'models.default_badge': 'global default',
    'models.set_default': 'Set as default',
    'models.set_default_hint': 'Use this model as the global default',
    'models.filter_provider': 'Provider',
    'models.all_providers': 'All providers',
    'models.add': 'Add model',
    'models.edit': 'Edit',
    'models.delete': 'Remove from catalog',
    'models.delete_confirm': 'Remove this model from the catalog?',
    'models.empty': 'The model catalog is empty.',
    'models.empty_hint':
      'Add an endpoint on the Model Providers page and discover its models, or add one entry manually here.',
    'models.no_match': 'No model matches the current filter.',
    'models.reference': 'Reference',
    'models.provider': 'Provider',
    'models.model_id': 'Model ID',
    'models.display_name': 'Display name',
    'models.display_name_placeholder': 'Optional label',
    'models.context_length': 'Context length',
    'models.max_output': 'Max output tokens',
    'models.temperature': 'Temperature',
    'models.temperature_hint': 'Between 0.0 and 2.0.',
    'models.source': 'Source',
    'models.source_unknown': 'Unknown',
    'models.source_upstream': 'Discovered',
    'models.source_manual': 'Manual',
    'models.capabilities': 'Capabilities',
    'models.cap_text': 'Text',
    'models.cap_vision': 'Vision',
    'models.cap_audio': 'Audio',
    'models.cap_video': 'Video',
    'models.cap_tool_calling': 'Tools',
    'models.cap_reasoning': 'Reasoning',
    'models.save': 'Save',
    'models.cancel': 'Cancel',
    'models.saving': 'Saving...',
    'models.discover_all': 'Discover all providers',
    'models.discover_done': 'Discovered {count} models, stored {persisted}',
    'models.optional': 'optional',

    // Reply policy (shared by the instance form and the node settings)
    'context.title': 'Context extras',
    'context.hint':
      'Choose what the node adds to every prompt besides the message itself. Ids and the time are off by default: ids are personal data and a wall-clock time is not part of what the user said. Merged forwards are expanded by default.',
    'context.channel_id': 'Group / channel id',
    'context.channel_id_hint':
      'Prepend the conversation id (group number / channel id).',
    'context.sender_id': 'Sender id',
    'context.sender_id_hint':
      'Prepend the platform sender id (QQ number / openid) to the prompt.',
    'context.timestamp': 'Message time',
    'context.timestamp_hint': 'Prepend the message timestamp to the prompt.',
    'context.updated': 'Context policy updated',
    'context.none': 'No extras',
    'reply.title': 'Reply policy',
    'reply.mode_always': 'Always',
    'reply.mode_mention': 'Only when mentioned',
    'reply.mode_probability': 'By probability',
    'reply.mode_never': 'Never (groups)',
    'reply.inherit': 'Inherit node policy',
    'reply.probability': 'Reply probability',
    'reply.node_current': 'Node policy: {policy}',
    'reply.describe_always': 'Always reply',
    'reply.describe_mention': 'Reply only when mentioned',
    'reply.describe_never': 'Never reply in groups',
    'reply.describe_probability': 'Reply with probability {percent}%',

    // Bot instances
    'instances.gate_label': 'Running bot instances:',
    'instances.gate_none': 'No instance is running — messages are dropped',
    'instances.gate_hint':
      'Adapters only declare where messages come from. Until an enabled instance claims an adapter, inbound messages are dropped before reaching the model.',
    'instances.new': 'New Instance',
    'instances.loading': 'Loading instances...',
    'instances.empty_title': 'No bot instance configured yet',
    'instances.empty_hint':
      'Create an instance and pick the adapters, persona and optional model it uses. Only enabled instances answer messages.',
    'instances.running': 'Enabled',
    'instances.stopped': 'Stopped',
    'instances.start': 'Start',
    'instances.stop': 'Stop',
    'instances.edit': 'Edit',
    'instances.delete': 'Delete',
    'instances.edit_title': 'Edit Instance',
    'instances.new_title': 'New Instance',
    'instances.field_name': 'Name',
    'instances.field_enabled': 'State',
    'instances.field_adapters': 'Adapters',
    'instances.adapters_hint':
      'Only one enabled instance may claim an adapter at a time.',
    'instances.adapter_taken': 'Already claimed by {name}',
    'instances.adapter_unknown': 'This platform is not registered on the node',
    'instances.items_title': 'Plugins, skills & MCP',
    'instances.items_hint':
      'Inherit follows the node-wide switch. Disable hides the item from this instance even when it is enabled globally.',
    'instances.section_plugins': 'Plugins',
    'instances.section_skills': 'Skills',
    'instances.section_mcp': 'MCP servers',
    'instances.policy_inherit': 'Inherit',
    'instances.policy_enable': 'Enable',
    'instances.policy_disable': 'Disable',
    'instances.no_items': 'Nothing installed yet.',
    'instances.overrides': '{count} overrides',
    'instances.no_adapter': 'No adapter selected yet',
    'instances.no_adapters': 'No adapter has been discovered on this node yet.',
    'instances.field_persona': 'Persona',
    'instances.persona_none': 'None (node default)',
    'instances.field_prompt': 'Instance prompt',
    'instances.prompt_placeholder': 'You are a helpful assistant...',
    'instances.prompt_hint':
      'Used only by this instance; it does not modify the shared persona catalog.',
    'instances.model_override': 'Use a custom model',
    'instances.model_hint': 'Leave empty to use the node default.',
    'instances.model_default': 'Node default model',
    'instances.persona_label': 'Persona',
    'instances.custom_prompt': 'Custom prompt',
    'instances.warn_no_adapter':
      'This instance is enabled but claims no adapter, so it will never receive a message.',
    'instances.cancel': 'Cancel',
    'instances.save': 'Save Instance',
    'instances.saving': 'Saving...',
    'instances.field_model': 'Model',
    'instances.model_inherit': 'Inherit node default',
    'instances.model_inherit_named': 'Inherit node default ({model})',
    'instances.model_catalog_empty': 'The model catalog is empty.',
    'instances.reply_policy': 'Reply policy',
    'instances.reply_policy_hint':
      'Decides whether group and channel messages are answered. Private conversations are always answered.',
    'instances.reply_override_hint':
      'This instance overrides the node policy; other instances are unaffected.',
    'instances.reply_inherit_hint':
      'This instance follows the node-wide policy shown above.',
    'instances.reply_policy_badge': 'Reply: {policy}',

    // Node-wide reply policy
    'system.reply_policy_title': 'Node-wide reply policy',
    'system.reply_policy_hint':
      'Every instance without its own override answers group and channel messages according to this policy. Private conversations are always answered.',
    'system.reply_policy_current': 'Effective policy',
    'system.reply_policy_save': 'Apply policy',
    'system.reply_policy_saving': 'Applying...',
    'system.reply_policy_saved': 'Policy applied to the running node',
  },
  zh: {
    // 导航项
    'nav.overview': '节点概览',
    'nav.instances': '实例',
    'nav.chat': '对话',
    'nav.pipeline': '流水线与日志',
    'nav.plugins': '插件与适配器',
    'nav.sessions': '会话',
    'nav.personas': '人设',
    'nav.playground': '对话',
    'instances.gate_label': '当前在线的机器人实例:',
    'instances.gate_none': '没有实例开启 —— 消息会被丢弃',
    'instances.gate_hint':
      '适配器只声明消息来自哪里；在某个启用的实例认领该适配器之前，入站消息会被直接丢弃，不会进入模型。',
    'instances.new': '新建实例',
    'instances.loading': '正在加载实例...',
    'instances.empty_title': '还没有配置机器人实例',
    'instances.empty_hint':
      '创建一个实例，选择它使用的适配器、人设以及可选的模型。只有启用的实例才会真正回复消息。',
    'instances.running': '已启用',
    'instances.stopped': '已停用',
    'instances.start': '启用',
    'instances.stop': '停用',
    'instances.edit': '编辑',
    'instances.delete': '删除',
    'instances.edit_title': '编辑实例',
    'instances.new_title': '新建实例',
    'instances.field_name': '名称',
    'instances.field_enabled': '状态',
    'instances.field_adapters': '适配器',
    'instances.adapters_hint': '同一个适配器同时只能被一个启用的实例认领。',
    'instances.adapter_taken': '已被 {name} 启用',
    'instances.adapter_unknown': '该平台未在节点上注册',
    'instances.items_title': '插件、技能与 MCP',
    'instances.items_hint':
      '「继承」跟随全局开关；「禁用」表示即使全局开启，该实例也不使用此项。',
    'instances.section_plugins': '插件',
    'instances.section_skills': '技能',
    'instances.section_mcp': 'MCP 服务器',
    'instances.policy_inherit': '继承',
    'instances.policy_enable': '启用',
    'instances.policy_disable': '禁用',
    'instances.no_items': '尚未安装任何项目。',
    'instances.overrides': '{count} 项覆盖',
    'instances.no_adapter': '尚未选择适配器',
    'instances.no_adapters': '节点上还没有发现任何适配器。',
    'instances.field_persona': '人格',
    'instances.persona_none': '不指定（使用节点默认）',
    'instances.persona_label': '人格',
    'instances.field_prompt': '实例人格提示词',
    'instances.prompt_placeholder':
      '例如：你是黑猪AI，一只活泼、用中文回答的助手。',
    'instances.prompt_hint':
      '填写后将覆盖所选人格，本实例的所有会话都使用这段提示词。',
    'instances.custom_prompt': '自定义提示词',
    'instances.model_override': '为该实例单独指定模型',
    'instances.model_hint':
      '不勾选则使用节点默认模型；模型提供商始终共用节点配置。',
    'instances.model_default': '节点默认模型',
    'instances.warn_no_adapter': '启用但没有适配器的实例不会回复任何消息。',
    'instances.save': '保存',
    'instances.saving': '保存中...',
    'instances.cancel': '取消',
    'instances.field_model': '模型',
    'instances.model_inherit': '继承节点默认模型',
    'instances.model_inherit_named': '继承节点默认模型（{model}）',
    'instances.model_catalog_empty': '模型目录为空。',
    'instances.reply_policy': '回复策略',
    'instances.reply_policy_hint':
      '决定群聊与频道消息是否被回复；私聊始终回复。',
    'instances.reply_override_hint': '该实例覆盖了节点策略，不影响其他实例。',
    'instances.reply_inherit_hint': '该实例跟随上方的节点级策略。',
    'instances.reply_policy_badge': '回复策略：{policy}',
    'nav.providers': '模型提供商',
    'nav.models': '模型目录',
    'nav.system': '系统配置',

    // 标题与副标题
    'title.instances': '机器人实例',
    'subtitle.instances': '哪些机器人在哪些平台上回复，以及各自的人设、模型与策略',
    'title.overview': '微内核概览与健康状态',
    'subtitle.overview':
      '微内核运行时、进程监管 Supervisor 与 Prometheus 指标导出',
    'title.chat': '对话',
    'subtitle.chat': '与大模型进行交互对话，支持多轮推理与插件工具调用',
    'title.pipeline': '流水线追踪与日志控制台',
    'subtitle.pipeline':
      '基于 WebSocket 的流水线生命周期状态转移与服务器日志实时流',
    'title.plugins': '插件宿主与平台适配器',
    'subtitle.plugins':
      '物理隔离的跨进程 gRPC 插件宿主、动态 JSON Schema 配置与平台适配器',
    'title.sessions': '会话',
    'subtitle.sessions': '追踪中的对话、Token 消耗统计与会话人设绑定',
    'title.personas': '人设',
    'subtitle.personas': '管理机器人可以使用的人设预设（添加与删除）',
    'title.playground': '对话',
    'subtitle.playground': '与大模型进行交互对话，支持多轮推理与插件工具调用',
    'title.providers': '模型提供商',
    'subtitle.providers': '模型提供商端点、连通性测试，以及唯一的全局默认模型',
    'title.models': '模型目录',
    'subtitle.models':
      '按「提供商/模型」引用记录每个模型的上下文窗口、模态能力与采样参数',
    'title.system': '系统配置',
    'subtitle.system':
      '微内核 IPC 通信套接字、运行与存储路径、记忆窗口及平台适配器',

    // 通用与状态
    'status.healthy': '运行正常',
    'status.connecting': '正在连接',
    'status.offline': '离线',
    'status.connected': '已连接',
    'status.disconnected': '已断开',
    'status.reconnecting': '重连中',
    'common.retry': '立即重试',
    'common.refresh': '刷新状态',
    'common.search': '搜索...',
    'common.clear': '清除',
    'common.save': '保存修改',
    'common.cancel': '取消',
    'common.close': '关闭',
    'common.loading': '加载中...',
    'common.error': '错误',
    'common.success': '成功',
    'common.appearance': '外观主题',
    'common.language': '界面语言',
    'common.command_menu': '快捷指令菜单',
    'common.events': '事件流',
    'common.logs': '日志流',
    'common.version': '版本号',
    'common.uptime': '运行时间',
    'common.latency': '往返延迟',

    // 概览卡片
    'overview.node_status': '节点运行状态',
    'overview.resident_memory': '常驻物理内存 (RSS)',
    'overview.virtual_memory': '虚拟地址空间',
    'overview.instances_enabled': '在线机器人',
    'overview.instances_hint': '只有被实例认领的适配器，其入站消息才会被处理。',
    'overview.llm_engine': '大模型网关引擎',
    'overview.llm_ready': '已配置就绪',
    'overview.llm_disabled': '未配置 (已停用)',
    'overview.plugin_hosts': '监管中的插件宿主进程',
    'overview.plugins_loaded': '已加载插件实例',
    'overview.sessions_total': '会话总数',
    'overview.sessions_active': '活跃会话',
    'overview.ws_connections': 'WebSocket 连接数',
    'overview.event_listeners': '生命周期监听者',
    'overview.log_listeners': '日志监听者',
    'overview.quick_actions': '快捷导航',

    // 流水线与日志
    'pipeline.live_events': '流水线实时事件流',
    'pipeline.server_logs': '结构化服务器日志',
    'pipeline.autoscroll': '自动滚动',
    'pipeline.filter_stage': '按阶段筛选',
    'pipeline.filter_level': '日志级别',
    'pipeline.empty_events':
      '暂无流水线事件记录。向 Bot 发送消息即可观察生命周期阶段。',
    'pipeline.empty_logs': '暂无日志输出。',
    'pipeline.offline': '未连接到节点 —— 该实时流已断开。',
    'pipeline.reconnect': '重新连接',
    'pipeline.no_instance_hint':
      '当前没有启用的实例，入站消息会在进入流水线之前被丢弃（到「实例」页启用一个实例）。',
    'pipeline.level_filter_hint':
      '当前日志级别筛选为 {level}，点 ALL 可查看全部记录。',

    // 插件与适配器
    'plugins.hosts_title': '进程监管中的插件宿主',
    'plugins.adapters_title': '平台适配器',
    'plugins.host_id': '宿主 ID',
    'plugins.runtime': '运行时环境',
    'plugins.pid': '进程 PID',
    'plugins.hostless_title': '未运行的插件',
    'plugins.hostless_hint':
      '已停用的插件不会再启动进程：它的指令、工具与适配器在重新启用前都处于停用状态。',
    'plugins.not_running': '未运行',
    'plugins.enable': '启用',
    'plugins.disable': '停用',
    'plugins.disabled': '已停用',
    'plugins.crashed': '已崩溃',
    'plugins.restarts': '次重启',
    'plugins.restart_unavailable': '该宿主未运行任何插件，无法定位重启目标',
    'plugins.restart': '重启宿主进程',
    'plugins.config': '配置参数',
    'plugins.commands': '指令声明',
    'plugins.tools': '工具声明',
    'plugins.no_hosts': '暂无运行中的独立插件子进程。',
    'plugins.no_adapters': '暂无注册的平台适配器。',
    'plugins.config_modal_title': '插件配置管理',
    'plugins.cas_version': 'CAS 版本号',
    'plugins.install': '安装插件',
    'plugins.tab_plugins': '插件',
    'plugins.tab_mcp': 'MCP 服务器',
    'plugins.tab_tools': '工具',

    // Tool catalog
    'tools.title': '工具列表',
    'tools.subtitle': '模型当前可以调用的全部工具及其提供方',
    'tools.total': '工具总数',
    'tools.source_builtin': '内置',
    'tools.source_plugin': '插件',
    'tools.source_mcp': 'MCP',
    'tools.search_placeholder': '按名称、描述或提供方筛选',
    'tools.parameters': '参数结构',
    'tools.empty': '当前没有任何可用工具。',
    'tools.empty_hint':
      '注册内置工具、启动插件宿主或启用 MCP 服务器后，模型即可调用这些工具。',
    'tools.no_match': '没有符合当前筛选条件的工具。',
    'plugins.tab_adapters': '适配器',
    'plugins.tab_skills': '技能',

    // MCP servers
    'mcp.title': 'MCP 服务器',
    'mcp.subtitle':
      'Model Context Protocol 服务器；其工具会提供给允许使用它们的实例',
    'mcp.add': '添加服务器',
    'mcp.add_title': '添加 MCP 服务器',
    'mcp.edit_title': '编辑 MCP 服务器',
    'mcp.edit': '编辑配置',
    'mcp.remove': '删除服务器',
    'mcp.empty': '尚未配置 MCP 服务器。',
    'mcp.empty_hint': '添加 stdio 或 HTTP 服务器即可把它的工具提供给模型。',
    'mcp.disabled': '已禁用',
    'mcp.tools': '个工具',
    'mcp.failures': '次探测失败',
    'mcp.field_id': '标识符',
    'mcp.field_name': '显示名称',
    'mcp.field_transport': '传输方式',
    'mcp.transport_stdio': 'stdio（子进程）',
    'mcp.transport_http': 'HTTP（远程端点）',
    'mcp.field_command': '命令',
    'mcp.field_args': '参数',
    'mcp.args_hint': '以空格分隔，按顺序传给命令。',
    'mcp.field_url': 'URL',
    'mcp.field_env': '环境变量',
    'mcp.field_headers': '请求头',
    'mcp.keyvalues_hint': '每行一个 KEY=VALUE。',

    // Skills
    'skills.title': '技能',
    'skills.subtitle': '已安装的指令包；在模型读取之前只有描述会进入提示词',
    'skills.install': '安装技能',
    'skills.install_title': '安装技能',
    'skills.remove': '删除技能',
    'skills.empty': '尚未安装技能。',
    'skills.empty_hint': '上传 zip 压缩包，或指定包含 SKILL.md 的目录。',
    'skills.archive': '压缩包',
    'skills.archive_hint': 'zip 包内 SKILL.md 可位于根目录或唯一的一级目录中。',
    'skills.path': '本地目录',
    'skills.path_hint': '本节点上包含 SKILL.md 的目录。',
    'skills.field_id': '标识符（可选）',
    'skills.id_hint': '默认使用压缩包或目录名称。',
    'plugins.install_modal_title': '安装新插件',
    'plugins.tab_local_path': '本地目录导入',
    'plugins.tab_upload_archive': '上传插件包 (.kpk / .zip)',
    'plugins.local_path_label': '插件目录路径',
    'plugins.local_path_placeholder': '例如：./plugins/demo_weather',
    'plugins.local_path_help':
      '服务器上包含有效 plugin.toml 清单的本地目录路径',
    'plugins.archive_label': '选择插件包文件',
    'plugins.archive_help': '标准 .kpk 分发包或 .zip 压缩包',
    'plugins.install_btn': '开始安装',
    'plugins.installing': '正在安装并激活...',
    'plugins.install_success': '插件安装并加载成功！',
    'plugins.runtime_unavailable': '运行环境缺失',
    'adapters.qq_qr_btn': 'QQ 官方扫码绑定',
    'adapters.qq_qr_title': 'QQ 官方机器人快速扫码绑定',
    'adapters.qq_qr_desc':
      '请使用手机 QQ 扫描下方二维码完成机器人授权，授权成功后将自动同步 AppID 与 AppSecret 并热重载。',
    'adapters.qq_qr_generating': '正在生成授权二维码...',
    'adapters.qq_qr_waiting': '等待手机 QQ 扫码授权中...',
    'adapters.qq_qr_success': '绑定成功！凭据已自动写入配置文件并热重载生效。',
    'adapters.qq_qr_expired': '二维码已过期，请点击重新获取。',
    'adapters.qq_qr_open_link': '手机 QQ 打开',
    'adapters.qq_qr_copy_link': '复制授权链接',
    'adapters.qq_qr_copied': '已复制到剪贴板！',
    'adapters.qq_qr_retry': '重新获取二维码',
    'adapters.qq_title': 'QQ 官方机器人适配器',
    'adapters.qq_enabled': '启用 QQ 官方机器人适配器',
    'adapters.qq_enabled_hint': '保存后连接或断开 QQ 网关，无需重启。',
    'adapters.qq_unsaved': '有未保存的改动（请点“保存并生效”）',
    'adapters.qq_appid': '机器人 AppID',
    'adapters.qq_secret': '机器人 AppSecret',
    'adapters.qq_secret_keep': '留空表示保留已保存的密钥',
    'adapters.qq_secret_none': '尚未配置密钥',
    'adapters.qq_sandbox': '沙箱测试环境',
    'adapters.qq_use_markdown': '发送原生 Markdown（需要 Markdown 权限）',
    'adapters.qq_save': '保存并生效',
    'adapters.qq_saving': '保存中...',
    'adapters.qq_saved': '设置已保存并应用，连接结果请查看下方状态',
    'adapters.qq_bot': '机器人',
    'adapters.qq_state_disabled': '未启用',
    'adapters.qq_state_connecting': '连接中',
    'adapters.qq_state_connected': '已连接',
    'adapters.qq_state_disconnected': '已断开',
    'adapters.qq_state_stopped': '已停止',
    'adapters.qq_not_hosted': '本节点未注册 QQ 官方机器人适配器。',

    'reply.quote': '回复时引用原消息',
    'reply.quote_hint':
      '在群聊和频道中，机器人的回复会引用它所回答的那条消息，大家能看清它在回答谁。私聊不会引用。',
    'context.expand_forward': '展开合并转发',
    'context.expand_forward_hint':
      '把合并转发里的每条消息（以及其中的图片，供识图模型查看）交给模型，而不是只给一个标题。',
    'events.title': '事件响应',
    'events.updated': '事件响应已更新',
    'events.hint':
      '非消息类的平台事件。机器人只对这里开启的事件作出回应，且不受回复策略限制。',
    'events.welcome': '欢迎新成员',
    'events.welcome_hint': '有人进群时，机器人以自己的人设打招呼欢迎。',
    'events.greet': '被添加时打招呼',
    'events.greet_hint': '机器人被拉进群或被加为好友时，主动做个自我介绍。',
    'events.poke': '回应戳一戳',
    'events.poke_hint': '有人戳了戳机器人时，机器人会回应。',
    'events.recall': '撤回提示',
    'events.recall_hint':
      '模型看过的消息被撤回后，在该会话的下一轮告诉模型，避免它继续引用。模型没看过的消息不会因此被透露。',
    'adapters.automation': '自动化',
    'adapters.auto_accept_friends': '自动同意好友申请',
    'adapters.auto_accept_group_invites': '自动同意入群邀请',
    'adapters.reaction_ack': '准备回复群消息时先给它点个 👍',
    'adapters.qq_typing':
      '私聊中模型思考时显示「对方正在输入」（会占用一次 QQ 被动回复额度）',

    // OneBot v11 adapter
    'adapters.onebot_title': 'OneBot v11 适配器',
    'adapters.onebot_unsaved': '有未保存的改动（请点“保存并生效”）',
    'adapters.onebot_enabled': '启用 OneBot v11 适配器',
    'adapters.onebot_enabled_hint': '保存后启动或停止所配置的 WebSocket 连接。',
    'adapters.onebot_ws_url': 'WebSocket 地址',
    'adapters.onebot_transport': '连接方式',
    'adapters.onebot_transport_forward': '正向 WebSocket',
    'adapters.onebot_transport_reverse': '反向 WebSocket',
    'adapters.onebot_forward_hint':
      '由 Kanon 连接 NapCat、Lagrange 等 OneBot v11 协议端的 API 与事件共用 WebSocket 地址。',
    'adapters.onebot_reverse_hint':
      '由 Kanon 在此 ws:// IP 地址和端口监听。请让协议端连接可达的监听地址与路径，并配置相同 Token；每个监听器连接一个账号。',
    'adapters.onebot_token': 'Access Token',
    'adapters.onebot_token_keep': '留空表示保留已保存的 Token',
    'adapters.onebot_token_none': '尚未配置 Token',
    'adapters.onebot_token_show': '显示 Token',
    'adapters.onebot_token_hide': '隐藏 Token',
    'adapters.onebot_clear_token': '清除已保存的 Token',
    'adapters.onebot_platform': '平台标识',
    'adapters.onebot_display_name': '显示名称',
    'adapters.onebot_readonly_hint':
      '运行期间不能修改身份字段；需在已保存的配置中修改并重启节点。',
    'adapters.onebot_save': '保存并生效',
    'adapters.onebot_saving': '保存中...',
    'adapters.onebot_saved': '设置已保存并应用，连接结果请查看下方状态',
    'adapters.onebot_login': '账号 ID',
    'adapters.onebot_state_disabled': '未启用',
    'adapters.onebot_state_connecting': '连接中',
    'adapters.onebot_state_listening': '监听中，等待协议端连接',
    'adapters.onebot_state_connected': '已连接',
    'adapters.onebot_state_disconnected': '已断开',
    'adapters.onebot_state_stopped': '已停止',
    'adapters.onebot_not_hosted': '本节点未注册 OneBot v11 适配器。',

    // Milky 协议适配器
    'adapters.milky_title': 'Milky 协议适配器',
    'adapters.empty': '本节点尚未注册任何平台适配器。',
    'adapters.milky_unsaved': '有未保存的改动（请点“保存并生效”）',
    'adapters.milky_enabled': '启用 Milky 适配器',
    'adapters.milky_enabled_hint':
      '关闭时不建立任何连接，所有出站投递都会明确报错。',
    'adapters.milky_base_url': '协议端基础地址',
    'adapters.milky_transport': '事件接收方式',
    'adapters.milky_transport_sse': 'Server-Sent Events',
    'adapters.milky_transport_ws': 'WebSocket',
    'adapters.milky_token': 'Access Token',
    'adapters.milky_token_keep': '留空表示保留已保存的 Token',
    'adapters.milky_token_none': '尚未配置 Token',
    'adapters.milky_token_show': '显示 Token',
    'adapters.milky_token_hide': '隐藏 Token',
    'adapters.milky_clear_token': '清除已保存的 Token',
    'adapters.milky_platform': '平台标识',
    'adapters.milky_display_name': '显示名称',
    'adapters.milky_readonly_hint':
      '平台标识与显示名称是适配器在节点注册表中的身份，修改需要重启节点。',
    'adapters.milky_save': '保存并生效',
    'adapters.milky_saving': '保存中...',
    'adapters.milky_saved': '已保存、已持久化、已生效',
    'adapters.milky_test': '测试连接',
    'adapters.milky_testing': '测试中...',
    'adapters.milky_test_ok': '协议端可达',
    'adapters.milky_login': '登录账号',
    'adapters.milky_impl': '协议端实现',
    'adapters.milky_counters': '入站 / 出站',
    'adapters.milky_events': '事件',
    'adapters.milky_rejected': '被拒',
    'adapters.milky_last_event': '最近事件',
    'adapters.milky_state_disabled': '未启用',
    'adapters.milky_state_connecting': '连接中',
    'adapters.milky_state_connected': '已连接',
    'adapters.milky_state_error': '连接异常',
    'adapters.milky_not_hosted':
      '本节点未注册 Milky 适配器，启动时未启用该扩展。',

    // 会话与人设
    'sessions.active_sessions': '追踪中的会话列表',
    'sessions.session_id': '会话 ID',
    'sessions.turns': '交互轮数',
    'sessions.tokens': 'Token 消耗总量',
    'sessions.persona': '当前人设',
    'sessions.reset': '清空历史记忆',
    'sessions.reset_confirm': '确定要清空会话「{id}」的历史记忆吗？',
    'sessions.reset_failed': '重置失败',
    'sessions.binding': '正在切换人设...',
    'sessions.bind_failed': '切换失败',
    'sessions.bind_title': '本会话使用的人设',
    'sessions.bind_select': '人设',
    'sessions.bind_none': '不指定 —— 使用基础助手',
    'sessions.bind_hint':
      '下一条消息起生效。机器人实例下的会话会跟随该实例设置的人设。',
    'sessions.bind_apply': '应用',

    // 人设
    'personas.heading': '人设预设',
    'personas.intro':
      '人设是放在每次请求最前面的固定指令文本。添加你需要的人设，再按实例或按会话选用。节点只自带一个基础助手。',
    'personas.add': '新建人设',
    'personas.edit': '编辑',
    'personas.edit_title': '编辑人设',
    'personas.delete': '删除人设',
    'personas.delete_confirm': '确定要删除人设「{name}」吗？',
    'personas.in_use': '正被实例使用：{instances}',
    'personas.builtin': '内置',
    'personas.builtin_hint': '未选择其他人设时使用，不可编辑或删除。',
    'personas.empty': '你还没有添加人设，目前只有基础助手可用。',
    'personas.name': '名称',
    'personas.name_placeholder': '例如：代码审查员',
    'personas.description': '简介',
    'personas.prompt': '提示词',
    'personas.prompt_placeholder': '你是……',
    'personas.prompt_hint':
      '会原样放在每次请求的最前面，这样能持续命中模型服务商的提示词缓存。请保持固定：时间、发言者这类运行时才会变化的信息会自动附加在它后面，不要写在这里。',
    'personas.save': '保存',
    'sessions.no_sessions': '暂无会话记录。',

    // 沙箱
    'playground.model': '指定模型',
    'playground.persona_override': '临时切换人设',
    'playground.enable_tools': '允许调用插件工具 (Tool Calling)',
    'playground.send': '发送测试',
    'playground.placeholder': '输入测试提示词，验证多轮对话推理或工具调度...',
    'playground.tools_executed': '实际执行的工具调用',
    'playground.empty_chat': '发起一轮对话测试默认模型与工具调用。',

    // 模型与系统配置
    'providers.protocol': '协议格式',
    'providers.base_url': '接口 Base URL',
    'providers.api_key': '凭证密钥',
    'providers.api_key_set': '已配置 (受保护隐藏)',
    'providers.api_key_unset': '未设置',
    'providers.temperature': '采样温度',
    'providers.max_tokens': '单次最大 Token 限制',
    'providers.testing': '正在连接测试...',
    'providers.test_prompt': '测试提示词',
    'providers.test_result': '测试结果',
    'providers.latency_ms': '网络往返延迟',
    'providers.response_preview': '模型回复预览',
    'providers.presets_title': '主流服务商配置预设',
    'providers.use_preset': '应用预设',
    'providers.system_config_title': '微内核系统与运行时参数',
    'providers.ipc_socket': 'Core IPC Socket 路径',
    'providers.run_dir': '运行时目录 (Run Dir)',
    'providers.data_dir': '持久化数据目录 (Data Dir)',
    'providers.signature_verify': 'HMAC-SHA256 签名校验',
    'providers.env_title': '系统环境参数',
    'providers.os_arch': '操作系统与架构',
    'providers.rust_edition': 'Rust 版本规范',

    // 全局默认模型（整个节点只有这一个决定）
    'providers.default_model_title': '全局默认模型',
    'providers.default_model_desc':
      '机器人默认使用的模型。各实例仍可单独指定别的模型，其余情况一律使用这里选定的模型。',
    'providers.default_model_unset': '— 未设置 —',
    'providers.default_model_none':
      '尚未设置默认模型 —— 选定之前，机器人不会回复普通消息。',
    'providers.default_model_no_models':
      '还没有模型。请在下方提供商中点击「发现模型」（或手动添加），然后在这里选择默认模型。',
    'providers.serves_default': '该提供商提供全局默认模型',
    'providers.delete_confirm':
      '确定要删除该提供商吗？其目录中的模型也会一并移除。',
    'providers.delete_default_warning':
      '它提供的正是全局默认模型，删除后节点将没有默认模型。',
    'providers.test_title': '连通性与模型',
    'providers.test_model_placeholder': '模型 ID，例如 deepseek-chat',
    'providers.test_key_hint':
      '已保存的密钥由节点自己使用，不会发送到浏览器。上方填写的内容（地址、协议、密钥）会按原样测试，因此可以先验证修改再保存。',

    // 命名提供商目录
    'providers.context_length': '上下文窗口',
    'providers.capabilities': '能力',
    'providers.directory_title': '提供商列表',
    'providers.edit_title': '编辑提供商',
    'providers.add_provider': '添加提供商',
    'providers.select_hint': '请选择一个提供商进行编辑。',
    'providers.empty_title': '暂未配置模型提供商',
    'providers.empty_hint':
      '先配置提供商端点（API Base URL 与密钥），再发现它所提供的模型。模型统一表示为「提供商名称/模型ID」。',
    'providers.name': '提供商名称 (标识符)',
    'providers.name_hint':
      '作为该端点旗下所有模型引用的前缀，例如 deepseek/deepseek-chat。',
    'providers.base_url_default': '使用协议默认地址',
    'providers.api_key_keep': '留空表示保留已保存的密钥',
    'providers.api_key_clear': '清除已保存的密钥',
    'providers.api_key_hint':
      '在线服务商必须填入有效 API Key，才能获取模型列表或进行对话测试。',
    'providers.api_key_configured': '密钥已保存',
    'providers.temperature_hint': '该端点下模型使用的默认采样温度。',
    'providers.max_tokens_hint': '该端点下模型使用的默认最大生成 Token 数。',
    'providers.save': '保存修改',
    'providers.saved': '已保存',
    'providers.delete': '删除提供商',
    'providers.models_in_catalog': '目录中 {count} 个模型',
    'providers.test': '测试连通性',
    'providers.test_model_label': '用于测试的模型',
    'providers.discover': '发现模型',
    'providers.discovering': '正在发现...',
    'providers.discover_done': '发现 {count} 个模型，已存储 {persisted} 个',
    'providers.discover_hint':
      '读取端点自身的模型列表并写入目录；手动编辑过的条目不会被覆盖。',
    'providers.quick_config': '一键配置',
    'providers.quick_config_title': '一键配置提供商模板',
    'providers.quick_config_hint':
      '选择常见服务商模板以自动填入协议与 Base URL，创建后填入 API 密钥即可使用。',
    'providers.manual_add_title': '手动添加模型提供商',
    'providers.create_provider': '创建提供商',
    'providers.preset_applied': '模板已填入，补充密钥后即可创建。',
    'providers.no_providers': '尚未配置任何提供商端点。',

    // 模型目录
    'models.title': '模型目录',
    'models.subtitle':
      '记录节点可路由的每个模型的上下文窗口、模态能力与采样参数',
    'models.total': '目录条目数',
    'models.default_model': '全局默认模型',
    'models.default_none': '未设置',
    'models.default_badge': '全局默认',
    'models.set_default': '设为默认',
    'models.set_default_hint': '将此模型设为全局默认模型',
    'models.filter_provider': '提供商',
    'models.all_providers': '全部提供商',
    'models.add': '添加模型',
    'models.edit': '编辑',
    'models.delete': '从目录移除',
    'models.delete_confirm': '确定要从此目录移除该模型吗？',
    'models.empty': '模型目录为空。',
    'models.empty_hint':
      '先在「模型提供商」页添加端点并点击「发现模型」，或在此手动添加一条记录。',
    'models.no_match': '没有符合当前筛选条件的模型。',
    'models.reference': '模型引用',
    'models.provider': '提供商',
    'models.model_id': '模型 ID',
    'models.display_name': '展示名称',
    'models.display_name_placeholder': '可选别名',
    'models.context_length': '上下文长度',
    'models.max_output': '最大输出 Token',
    'models.temperature': '采样温度',
    'models.temperature_hint': '取值范围 0.0 至 2.0。',
    'models.source': '来源',
    'models.source_unknown': '未知',
    'models.source_upstream': '自动发现',
    'models.source_manual': '手动',
    'models.capabilities': '能力',
    'models.cap_text': '文本',
    'models.cap_vision': '视觉',
    'models.cap_audio': '音频',
    'models.cap_video': '视频',
    'models.cap_tool_calling': '工具调用',
    'models.cap_reasoning': '推理',
    'models.save': '保存',
    'models.cancel': '取消',
    'models.saving': '保存中...',
    'models.discover_all': '发现全部提供商的模型',
    'models.discover_done': '发现 {count} 个模型，已存储 {persisted} 个',
    'models.optional': '可选',

    // 回复策略（实例表单与节点设置共用）
    'context.title': '上下文附加信息',
    'context.hint':
      '选择节点在消息本身之外额外加入提示词的内容。ID 与时间默认关闭：各种 ID 属于个人数据，时间也不是用户说的话；合并转发默认展开。',
    'context.channel_id': '群号 / 频道 ID',
    'context.channel_id_hint': '在提示词中加入会话 ID（群号 / 频道 ID）。',
    'context.sender_id': '发送者 ID',
    'context.sender_id_hint': '在提示词中加入平台发送者 ID（QQ 号 / openid）。',
    'context.timestamp': '消息时间',
    'context.timestamp_hint': '在提示词中加入消息时间戳。',
    'context.updated': '上下文策略已更新',
    'context.none': '不附加',
    'reply.title': '回复策略',
    'reply.mode_always': '总是回复',
    'reply.mode_mention': '仅被 @ 时回复',
    'reply.mode_probability': '按概率回复',
    'reply.mode_never': '从不回复（群聊）',
    'reply.inherit': '继承节点策略',
    'reply.probability': '回复概率',
    'reply.node_current': '节点当前策略：{policy}',
    'reply.describe_always': '总是回复',
    'reply.describe_mention': '仅在被 @ 时回复',
    'reply.describe_never': '群聊中从不回复',
    'reply.describe_probability': '以 {percent}% 的概率回复',

    // 节点级回复策略
    'system.reply_policy_title': '节点级回复策略',
    'system.reply_policy_hint':
      '所有未单独设置策略的实例都按此策略回复群聊与频道消息；私聊始终回复。',
    'system.reply_policy_current': '当前生效策略',
    'system.reply_policy_save': '应用策略',
    'system.reply_policy_saving': '应用中...',
    'system.reply_policy_saved': '策略已应用到运行中的节点',
  },
};

class I18nStore {
  locale = $state<Locale>('zh');

  constructor() {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem('kanon-locale') as Locale | null;
      if (saved === 'en' || saved === 'zh') {
        this.locale = saved;
      } else {
        const navLang = navigator.language.toLowerCase();
        this.locale = navLang.startsWith('zh') ? 'zh' : 'en';
      }
      document.documentElement.lang = this.locale === 'zh' ? 'zh-CN' : 'en';
    }
  }

  setLocale(l: Locale) {
    this.locale = l;
    if (typeof window !== 'undefined') {
      localStorage.setItem('kanon-locale', l);
      document.documentElement.lang = l === 'zh' ? 'zh-CN' : 'en';
    }
  }

  toggle() {
    this.setLocale(this.locale === 'zh' ? 'en' : 'zh');
  }

  t(key: string, params?: Record<string, string | number>): string {
    const dict = dictionaries[this.locale] || dictionaries.en;
    let text =
      (dict as Record<string, string>)[key] ??
      (dictionaries.en as Record<string, string>)[key] ??
      key;
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        text = text.replace(new RegExp(`{${k}}`, 'g'), String(v));
      }
    }
    return text;
  }
}

export const i18n = new I18nStore();
export const t = (key: string, params?: Record<string, string | number>) =>
  i18n.t(key, params);
