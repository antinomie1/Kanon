export type Locale = 'zh' | 'en';

export const dictionaries = {
  en: {
    // Navigation
    'nav.overview': 'Overview',
    'nav.instances': 'Instances',
    'nav.chat': 'Chat',
    'nav.pipeline': 'Pipeline & Logs',
    'nav.plugins': 'Plugins & Adapters',
    'nav.sessions': 'Sessions & Personas',
    'nav.playground': 'Chat',
    'nav.providers': 'Model Providers',
    'nav.system': 'System Settings',

    // Titles & Subtitles
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
    'title.sessions': 'Sessions & Persona Catalogs',
    'subtitle.sessions':
      'Conversation context memory, token consumption counters, and persona prompts',
    'title.playground': 'Interactive Chat',
    'subtitle.playground':
      'Interactive streaming chat with multi-turn reasoning and tool calling inspection',
    'title.providers': 'Model Providers',
    'subtitle.providers':
      'LLM gateway backends, connectivity testing, and provider presets',
    'title.system': 'System Configuration',
    'subtitle.system':
      'Microkernel IPC socket, runtime & data paths, memory window, and platform webhook parameters',

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
    'plugins.restart_unavailable': 'This host runs no plugin, so it cannot be addressed for a restart',
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
    'adapters.qq_visual_config': 'QQ Official Settings',
    'adapters.qq_appid': 'Bot AppID',
    'adapters.qq_secret': 'Bot AppSecret',
    'adapters.qq_sandbox': 'Sandbox Mode',
    'adapters.qq_group_c2c': 'Group & Private (C2C) Messages',
    'adapters.qq_guild_dm': 'Guild Direct Messages',
    'adapters.qq_use_markdown': 'Native Markdown Support',
    'adapters.qq_md_template': 'Markdown Template ID',
    'adapters.qq_md_param': 'Template Parameter Key',

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
    'sessions.persona_catalog': 'Persona Catalog',
    'sessions.system_prompt': 'System Prompt',
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
      'Start a sandbox chat turn to test the active LLM provider and tool calling.',

    // Providers & System
    'providers.active_provider': 'Active Model Provider',
    'providers.protocol': 'Protocol',
    'providers.model_name': 'Default Model',
    'providers.base_url': 'Base URL',
    'providers.api_key': 'API Key',
    'providers.api_key_set': 'Configured (Masked)',
    'providers.api_key_unset': 'Not Configured',
    'providers.temperature': 'Temperature',
    'providers.max_tokens': 'Max Tokens',
    'providers.test_connectivity': 'Test Connectivity & Latency',
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
    'providers.memory_window': 'Memory Sliding Window',
    'providers.webhook_adapter': 'Platform Webhook',
    'providers.signature_verify': 'HMAC Signature Verification',
    'providers.env_title': 'Runtime Environment',
    'providers.os_arch': 'OS & Architecture',
    'providers.rust_edition': 'Rust Edition',

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
  },
  zh: {
    // 导航项
    'nav.overview': '节点概览',
    'nav.instances': '实例',
    'nav.chat': '对话',
    'nav.pipeline': '流水线与日志',
    'nav.plugins': '插件与适配器',
    'nav.sessions': '会话与人设',
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
    'nav.providers': '模型提供商',
    'nav.system': '系统配置',

    // 标题与副标题
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
    'title.sessions': '会话上下文与人设库',
    'subtitle.sessions': '对话上下文滑动窗口记忆、Token 消耗统计与人设提示词库',
    'title.playground': '对话',
    'subtitle.playground': '与大模型进行交互对话，支持多轮推理与插件工具调用',
    'title.providers': '模型提供商',
    'subtitle.providers': '大语言模型提供商配置、连通性测速与主流服务商预设',
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
    'adapters.qq_visual_config': 'QQ 官方机器人配置',
    'adapters.qq_appid': '机器人 AppID',
    'adapters.qq_secret': '机器人 AppSecret',
    'adapters.qq_sandbox': '沙箱测试环境',
    'adapters.qq_group_c2c': '启用群聊与单聊私信 (C2C)',
    'adapters.qq_guild_dm': '启用频道私信支持',
    'adapters.qq_use_markdown': '启用原生 Markdown',
    'adapters.qq_md_template': 'Markdown 模板 ID (可选)',
    'adapters.qq_md_param': '模板变量 Key (默认: text)',

    // Milky 协议适配器
    'adapters.milky_title': 'Milky 协议适配器',
    'adapters.empty': '本节点尚未注册任何平台适配器。',
    'adapters.milky_unsaved': '有未保存的改动（请点“保存并生效”）',
    'adapters.milky_enabled': '启用 Milky 适配器',
    'adapters.milky_enabled_hint': '关闭时不建立任何连接，所有出站投递都会明确报错。',
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
    'adapters.milky_not_hosted': '本节点未注册 Milky 适配器，启动时未启用该扩展。',

    // 会话与人设
    'sessions.active_sessions': '追踪中的会话列表',
    'sessions.session_id': '会话 ID',
    'sessions.turns': '交互轮数',
    'sessions.tokens': 'Token 消耗总量',
    'sessions.persona': '当前人设',
    'sessions.reset': '清空历史记忆',
    'sessions.persona_catalog': '人设预设库',
    'sessions.system_prompt': '系统提示词 (System Prompt)',
    'sessions.no_sessions': '暂无会话记录。',

    // 沙箱
    'playground.model': '指定模型',
    'playground.persona_override': '临时切换人设',
    'playground.enable_tools': '允许调用插件工具 (Tool Calling)',
    'playground.send': '发送测试',
    'playground.placeholder': '输入测试提示词，验证多轮对话推理或工具调度...',
    'playground.tools_executed': '实际执行的工具调用',
    'playground.empty_chat':
      '发起一轮对话测试当前配置的 LLM Provider 与工具调用。',

    // 模型与系统配置
    'providers.active_provider': '当前启用的 LLM Provider',
    'providers.protocol': '协议格式',
    'providers.model_name': '默认模型标识',
    'providers.base_url': '接口 Base URL',
    'providers.api_key': '凭证密钥',
    'providers.api_key_set': '已配置 (受保护隐藏)',
    'providers.api_key_unset': '未设置',
    'providers.temperature': '采样温度',
    'providers.max_tokens': '单次最大 Token 限制',
    'providers.test_connectivity': '连通性与测速测试',
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
    'providers.memory_window': '会话记忆滑动窗口大小',
    'providers.webhook_adapter': '内置 Webhook 适配器',
    'providers.signature_verify': 'HMAC-SHA256 签名校验',
    'providers.env_title': '系统环境参数',
    'providers.os_arch': '操作系统与架构',
    'providers.rust_edition': 'Rust 版本规范',
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
