/** zh settings and shared platform translations. */
export default {
    // Settings
    'settings.theme': '主题',
    'settings.theme_system': '跟随系统',
    'settings.theme_light': '浅色',
    'settings.theme_dark': '深色',
    'settings.section_appearance': '外观',
    'settings.section_agent': 'Agent',
    'settings.section_replies': '默认回复规则',
    'settings.section_context': '发给模型的信息',
    'settings.section_events': '平台事件',
    'settings.section_commands': '指令权限',
    'settings.section_bash': 'Bash',
    'settings.section_node': '节点',
    'settings.theme_hint': '跟随系统时，会随电脑的浅色或深色设置切换。',
    'settings.accent': '主题色',
    'settings.accent_hint':
      '按钮、选中项和开关都用这个颜色，背景也会带上淡淡的同色调。只保存在这个浏览器里。',
    'settings.accent_graphite': '石墨',
    'settings.accent_violet': '紫罗兰',
    'settings.accent_blue': '海蓝',
    'settings.accent_teal': '青绿',
    'settings.accent_rose': '玫瑰',
    'settings.preview': '预览',
    'settings.language_hint': '界面文字的语言。',
    'settings.saved_toast': '已保存',
    'settings.reply_title': '群聊里什么时候回答',
    'settings.reply_hint': '没有单独设置的实例都按这里回答。私聊总是会回答。',
    'settings.reply_how': '怎么回答',
    'settings.reply_how_hint': '只在平台支持时生效。',
    'settings.node_status': '运行状态',
    'settings.node_status_hint': '每 5 秒更新一次。',
    'settings.node_version': '版本',
    'settings.node_uptime': '已运行',
    'settings.node_memory': '内存',
    'settings.node_plugins': '插件',
    'settings.node_plugins_value': '{hosts} 个宿主，{loaded} 个插件',
    'settings.node_sessions': '会话',
    'settings.node_sessions_value': '{total} 个，{active} 个活跃',
    'settings.node_sockets': '实时连接',
    'settings.node_paths': '路径',
    'settings.node_paths_hint': '节点存放套接字和数据的位置。',
    'settings.node_metrics': '监控指标',
    'settings.node_metrics_hint': '/api/v1/metrics 提供的 Prometheus 文本。',
    'settings.metrics_show': '显示指标',
    'settings.metrics_hide': '收起指标',
    'settings.copied': '已复制',
    'settings.copy_value': '复制{label}',
    // Reply policy
    'reply.acknowledge_hint':
      '模型思考时，让平台显示「正在回复」——QQ 私聊显示「对方正在输入」（会占用一次 QQ 被动回复额度），Milky 群聊给原消息点个 👍。',
    'reply.reasoning': '发送思考内容',
    'reply.reasoning_hint':
      '在回答前发送独立推理通道的内容。正文中的字面标签和代码示例保持不变。',
    'reply.describe_always': '总是回复',
    'reply.describe_mention': '仅在被 @ 时回复',
    'reply.describe_never': '群聊中从不回复',
    'reply.describe_probability': '以 {percent}% 的概率回复',
    'reply.quote': '回复时引用原消息',
    'reply.split_lines': '按换行分条发送',
    'reply.split_lines_hint':
      '模型回答的每个非空行单独发送，代码行也会拆分；空白行跳过。QQ 官方机器人会按回复限制合并超出的行。',
    'reply.acknowledge': '回答前先显示正在处理',
    'reply.quote_hint':
      '在群和频道里，回复会引用它回答的那条消息，大家能看清它在回答谁。私聊不会引用。',
    // Context extras
    'context.expand_forward': '展开合并转发',
    'context.expand_forward_hint':
      '把合并转发里的每条消息（以及其中的图片，供识图模型查看）交给模型，而不是只给一个标题。',
    'context.channel_id': '群号 / 频道 ID',
    'context.channel_id_hint':
      '附加群号、群 OpenID 或频道 ID，按平台明确标注；群名有数据时始终显示。',
    'context.sender_id': '发送者 ID',
    'context.sender_id_hint':
      '附加 QQ 号或用户 OpenID，按平台明确标注；昵称和群名片有数据时始终显示。',
    'context.timestamp': '消息时间',
    'context.timestamp_hint': '在提示词中加入消息时间戳。',
    'context.none': '不附加',
    'agents.title': '默认 Agent',
    'agents.hint':
      "未单独指定 Agent 的实例使用此选项。可选后端取决于节点编译时启用的模块。",
    'agents.builtin': '内置 Agent',
    'agents.builtin_hint':
      'Kanon 自带的对话循环：使用所选模型、插件与 MCP 工具、人设和会话记忆。',
    "agents.dsh_hint": "选择 DSH 的实例，其设置、模型、会话、记忆与上下文均由 DSH 管理。",
    "agents.dsh_url": "DSH 地址",
    "agents.dsh_cookie": "节点上的 Cookie 文件（可选）",
    "agents.dsh_load": "读取 DSH 设置",
    "agents.dsh_open": "打开 DSH",
    "agents.dsh_namespace": "设置分区",
    "agents.dsh_current": "当前设置（敏感值已隐藏）",
    "agents.dsh_patch": "要修改的字段（JSON 对象）",
    "agents.dsh_patch_hint": "仅修改提供的字段。设置若被其他人修改，会拒绝覆盖，请重新读取。",
    "agents.dsh_patch_object": "请输入 JSON 对象。",
    "agents.dsh_owned": "此实例使用 DSH，请在 Agent 设置或 DSH 中管理模型、人设、记忆与上下文。",
    'context.title': '发给模型的信息',
    'context.hint':
      '节点在消息本身之外额外加入提示词的内容。ID 和时间默认关闭：ID 属于个人数据，时间也不是用户说的话；合并转发默认展开。',
    // Event handling
    'events.welcome': '欢迎新成员',
    'events.greet': '被添加时打招呼',
    'events.poke': '回应戳一戳',
    'events.recall': '撤回提示',
    'events.recall_hint':
      '模型看过的消息被撤回后，在该会话的下一轮告诉模型，避免它继续引用。模型没看过的消息不会因此被透露。',
    'events.accept_friends': '自动同意好友申请',
    'events.accept_friends_hint': '关闭时申请留在平台上等人工处理。',
    'events.accept_invites': '自动同意入群邀请',
    'events.accept_invites_hint': '关闭时邀请留在平台上等人工处理。',
    'events.title': '平台事件',
    'events.hint':
      '消息之外的平台事件。实例只对这里开启的事件作出反应，不受回复规则限制。',
    'events.welcome_hint': '有人进群时，实例用自己的人设打招呼欢迎。',
    'events.greet_hint': '被拉进群或被加为好友时，实例主动做个自我介绍。',
    'events.poke_hint': '有人戳了戳实例时，它会回应。',
    // Command permissions
    'commands.level_admins': '仅管理员',
    'commands.group_admins': '群主和群管理员也算管理员',
    'commands.group_admins_hint':
      '仅在其所在的群内生效，以平台报告的群角色为准。',
    'commands.level_everyone': '所有人',
    'commands.level_admins_in_groups': '私聊所有人，群聊仅管理员',
    'commands.title': '指令权限',
    'commands.admins': '管理员',
    'commands.hint':
      '有些指令会影响别人：/model 会切换整个实例的模型，共享群会话里的 /new 会清空全群的上下文。在这里决定谁能用；每个实例也可以有自己的指令权限。',
    'commands.remove_named': '移除 /{command}',
    'commands.add': '添加',
    'commands.admins_hint':
      '每行一个，格式为 <平台>:<用户 ID>。被拒绝的指令会回复发送者的 ID，可以直接复制到这里。',
    'commands.access': '每个指令谁能用',
    'commands.access_hint': '没列出的指令所有人都能用；也可以加入插件的指令。',
    'commands.add_placeholder': '指令名，例如 weather',
    // Bash tool
    'bash.title': 'Bash 工具',
    'bash.execution_mode': '执行模式',
    'bash.local_workdir': '本机工作目录',
    'bash.auto_review': '本机执行前进行 AI 自动审查',
    'bash.review_model': '审查模型（留空使用默认模型）',
    'bash.review_hint':
      '每次执行前单独调用模型审查；拒绝、输出无效或审查失败都会阻止执行。审查可以降低风险，但不提供隔离。',
    'bash.network': '允许公网联网',
    'bash.sandbox_hint':
      '跨命令和节点重启复用同一个容器，保留工作区、.home 和后台进程。修改镜像或隔离设置后需重置容器。',
    'bash.image': '已准备的沙箱镜像',
    'bash.enabled': '允许管理员让 AI 执行 Bash',
    'bash.mode_hint': '命令在哪里执行。',
    'bash.mode_sandbox': '沙箱容器',
    'bash.mode_local': '本机',
    'bash.local_hint':
      '命令以运行 Kanon 的账号权限执行，始终会检查管理员权限。',
    'bash.hint':
      '全局总开关。每个实例可以在它的高级设置里决定管理员在哪些对话里可以用。',
    'bash.identity_hint':
      '只有按 ID 列在实例指令权限里（跟随全局时为全局指令权限）的管理员可以用，群主和群管理员不算。平台事件和控制台对话里始终不可用。',
    'bash.reset_title': '重置沙箱？',
    'bash.reset_confirm':
      '后台进程会停止，容器的临时状态会被清空。工作区和 .home 会保留，下次执行时创建新容器。',
    'bash.reset': '重置沙箱',
    'bash.reset_done': '沙箱已重置',
    'bash.limits': '内存 {memory} MiB，{cpus} 个 CPU，最多 {pids} 个进程',
    'bash.saved': 'Bash 设置已保存',
    // Group conversations
    'group.observe': '旁听群聊',
    'group.title': '群聊记忆',
    'group.hint': '实例在群里怎么记住对话。',
    'group.scope_user_hint':
      '成员之间看不到彼此和实例的对话，/new 只重置发送者自己的。',
    'group.scope_group_hint':
      '实例能跟上多人讨论，每条消息都会标注说话人，/new 会重置全群的对话。',
    'group.observe_hint':
      '没被回复的群消息（最多 30 条、30 分钟内）会在实例下次被 @ 时一并交给模型，因此也会发送给模型服务商。',
    // Node details
    'providers.ipc_socket': 'IPC 套接字',
    'providers.run_dir': '运行目录',
    'providers.data_dir': '数据目录',
    'providers.os_arch': '系统',
    // Command palette
    'palette.placeholder': '跳转到页面、实例或操作',
    'palette.pages': '页面',
    'palette.instances': '实例',
    'palette.actions': '操作',
    'palette.refresh': '刷新全部数据',
    'palette.theme_light': '切换到浅色',
    'palette.theme_dark': '切换到深色',
    'palette.empty': '没有匹配的结果。',
};
