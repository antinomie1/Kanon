/** en settings and shared platform translations. */
export default {
    // Settings
    'settings.theme': 'Theme',
    'settings.theme_system': 'Match system',
    'settings.theme_light': 'Light',
    'settings.theme_dark': 'Dark',
    'settings.section_appearance': 'Appearance',
    'settings.section_agent': 'Agent',
    'settings.section_replies': 'Default replies',
    'settings.section_context': 'What the model is told',
    'settings.section_events': 'Platform events',
    'settings.section_commands': 'Command permissions',
    'settings.section_bash': 'Bash',
    'settings.section_node': 'Node',
    'settings.theme_hint':
      "Match system follows your computer's light or dark setting.",
    'settings.accent': 'Accent colour',
    'settings.accent_hint':
      'Used for buttons, selections and switches, and lightly tints the background. Saved in this browser only.',
    'settings.accent_graphite': 'Graphite',
    'settings.accent_violet': 'Violet',
    'settings.accent_blue': 'Blue',
    'settings.accent_teal': 'Teal',
    'settings.accent_rose': 'Rose',
    'settings.preview': 'Preview',
    'settings.language_hint': 'The language of the console.',
    'settings.saved_toast': 'Saved',
    'settings.reply_title': 'When to answer in groups',
    'settings.reply_hint':
      'Every instance without its own rule follows this. Private chats are always answered.',
    'settings.reply_how': 'How to answer',
    'settings.reply_how_hint': 'Only where the platform supports it.',
    'settings.node_status': 'Status',
    'settings.node_status_hint': 'Updated every 5 seconds.',
    'settings.node_version': 'Version',
    'settings.node_uptime': 'Running for',
    'settings.node_memory': 'Memory',
    'settings.node_plugins': 'Plugins',
    'settings.node_plugins_value': '{loaded} in {hosts} hosts',
    'settings.node_sessions': 'Sessions',
    'settings.node_sessions_value': '{active} active of {total}',
    'settings.node_sockets': 'Live connections',
    'settings.node_paths': 'Paths',
    'settings.node_paths_hint': 'Where the node keeps its sockets and data.',
    'settings.node_metrics': 'Metrics',
    'settings.node_metrics_hint':
      'The Prometheus text served at /api/v1/metrics.',
    'settings.metrics_show': 'Show metrics',
    'settings.metrics_hide': 'Hide metrics',
    'settings.copied': 'Copied',
    'settings.copy_value': 'Copy {label}',
    // Reply policy
    'reply.acknowledge_hint':
      "While the model works, the platform shows that an answer is coming — a typing indicator in QQ private chats (uses one of QQ's passive-reply slots), a 👍 on the group message in Milky.",
    'reply.reasoning': 'Send reasoning',
    'reply.reasoning_hint':
      'Send the separate reasoning channel before the answer. Literal tags and code in the answer are preserved.',
    'reply.describe_always': 'Always reply',
    'reply.describe_mention': 'Reply only when mentioned',
    'reply.describe_never': 'Never reply in groups',
    'reply.describe_probability': 'Reply with probability {percent}%',
    'reply.quote': 'Quote the message being answered',
    'reply.split_lines': 'Send each line separately',
    'reply.split_lines_hint':
      'Send each nonblank line of a model answer as a separate message, including code lines. Blank lines are skipped. QQ Official merges excess lines to fit its reply limits.',
    'reply.acknowledge': "Show that it's working before it answers",
    'reply.quote_hint':
      "In groups and channels the reply quotes the message it answers, so everyone can see who it's answering. Never in private chats.",
    // Context extras
    'context.expand_forward': 'Expand merged forwards',
    'context.expand_forward_hint':
      'Show the model the messages inside a forwarded chat log (with their pictures for vision models) instead of only its title.',
    'context.channel_id': 'Group / channel id',
    'context.channel_id_hint':
      'Prepend the conversation id (group number / channel id).',
    'context.sender_id': 'Sender id',
    'context.sender_id_hint':
      'Prepend the platform sender id (QQ number / openid) to the prompt.',
    'context.timestamp': 'Message time',
    'context.timestamp_hint': 'Prepend the message timestamp to the prompt.',
    'context.none': 'No extras',
    'agents.title': 'Default agent',
    'agents.hint':
      "The agent used by instances without their own selection. Available backends depend on this node’s build.",
    'agents.builtin': 'Built-in agent',
    'agents.builtin_hint':
      "Kanon's own loop: the chosen model, plugin and MCP tools, personas and conversation memory.",
    "agents.dsh_hint": "DSH owns settings, models, sessions, memory and context for instances that select it.",
    "agents.dsh_url": "DSH address",
    "agents.dsh_cookie": "Cookie file on the node (optional)",
    "agents.dsh_load": "Read DSH settings",
    "agents.dsh_open": "Open DSH",
    "agents.dsh_namespace": "Settings section",
    "agents.dsh_current": "Current settings (secrets hidden)",
    "agents.dsh_patch": "Changes as a JSON object",
    "agents.dsh_patch_hint": "Only supplied fields change. Concurrent edits are rejected; reload before retrying.",
    "agents.dsh_patch_object": "Enter a JSON object.",
    "agents.dsh_owned": "This instance uses DSH. Configure its models, persona, memory and context in the Agent settings or DSH.",
    'context.title': 'What the model is told',
    'context.hint':
      "What the node adds to the prompt besides the message itself. Ids and time are off by default: ids are personal data and the time isn't something the user said. Merged forwards are expanded by default.",
    // Event handling
    'events.welcome': 'Welcome new group members',
    'events.greet': 'Say hello when added',
    'events.poke': 'Respond to pokes',
    'events.recall': 'Tell the model about recalls',
    'events.recall_hint':
      'When a message the model already saw is recalled, its next turn in that conversation says so, so it stops referring to it. Recalled messages it never saw are not revealed.',
    'events.accept_friends': 'Accept friend requests automatically',
    'events.accept_friends_hint':
      'Otherwise requests wait for a human on the platform.',
    'events.accept_invites': 'Accept group invitations automatically',
    'events.accept_invites_hint':
      'Otherwise invitations wait for a human on the platform.',
    'events.title': 'Platform events',
    'events.hint':
      'Things that happen on a platform besides messages. Instances react only to the ones switched on here, regardless of the reply rules.',
    'events.welcome_hint':
      'When someone joins a group, the instance welcomes them in its persona.',
    'events.greet_hint':
      'When added to a group or as a friend, the instance introduces itself.',
    'events.poke_hint': 'When someone pokes the instance, it responds.',
    // Command permissions
    'commands.level_admins': 'Administrators only',
    'commands.group_admins': 'Group owners and admins count as administrators',
    'commands.group_admins_hint':
      'In their own group, as the platform reports their role.',
    'commands.level_everyone': 'Everyone',
    'commands.level_admins_in_groups':
      'Anyone in private chats, admins in groups',
    'commands.title': 'Command permissions',
    'commands.admins': 'Admins',
    'commands.hint':
      'Some commands affect other people: /model changes the model of the whole instance, and /new in a shared group conversation clears it for everyone. Decide who may use them here; each instance can also have its own.',
    'commands.remove_named': 'Remove /{command}',
    'commands.add': 'Add',
    'commands.admins_hint':
      "One per line, as <platform>:<user id>. A refused command replies with the sender's id, which you can paste here.",
    'commands.access': 'Who can run each command',
    'commands.access_hint':
      'Commands not listed are open to everyone. Plugin commands can be added too.',
    'commands.add_placeholder': 'Command name, e.g. weather',
    // Bash tool
    'bash.title': 'Bash tool',
    'bash.execution_mode': 'Execution mode',
    'bash.local_workdir': 'Local working directory',
    'bash.auto_review': 'AI review before local execution',
    'bash.review_model': 'Reviewer model (blank uses the default model)',
    'bash.review_hint':
      'Each command requires an explicit approval from a separate model request. Rejection, invalid output or review failure blocks execution. Review reduces risk but is not isolation.',
    'bash.network': 'Allow public Internet access',
    'bash.sandbox_hint':
      'The same container is reused across commands and node restarts. Workspace, .home and background processes persist. Reset after changing the image or isolation settings.',
    'bash.image': 'Prepared sandbox image',
    'bash.enabled': 'Let administrators ask the AI to run Bash',
    'bash.mode_hint': 'Where commands run.',
    'bash.mode_sandbox': 'Sandbox container',
    'bash.mode_local': 'This machine',
    'bash.local_hint':
      'Commands run with the permissions of the account Kanon runs as. Admin permission is always checked.',
    'bash.hint':
      'The global switch. Each instance decides where its admins may use Bash, in its advanced settings.',
    'bash.identity_hint':
      "Only admins listed by id in the instance's command permissions (or the global ones when it follows them) may use it; group owners and admins don't count. Never available for platform events or the console chat.",
    'bash.reset_title': 'Reset the sandbox?',
    'bash.reset_confirm':
      "Background processes stop and the container's temporary state is cleared. The workspace and .home are kept; a new container starts on the next command.",
    'bash.reset': 'Reset sandbox',
    'bash.reset_done': 'Sandbox reset',
    'bash.limits': '{memory} MiB memory, {cpus} CPU, up to {pids} processes',
    'bash.saved': 'Bash settings saved',
    // Group conversations
    'group.observe': 'Observe the group',
    'group.title': 'Group memory',
    'group.hint': 'How the instance remembers conversations in a group.',
    'group.scope_user_hint':
      "Members don't see each other's conversations with the instance; /new resets only the sender's.",
    'group.scope_group_hint':
      'The instance can follow a discussion between several people; every message is labelled with its speaker and /new resets it for the whole group.',
    'group.observe_hint':
      'Unanswered group messages (up to 30, within 30 minutes) are handed to the model the next time the instance is @-mentioned, so they are also sent to the model provider.',
    // Node details
    'providers.ipc_socket': 'IPC socket',
    'providers.run_dir': 'Run directory',
    'providers.data_dir': 'Data directory',
    'providers.os_arch': 'System',
    // Command palette
    'palette.placeholder': 'Go to a page, instance or action',
    'palette.pages': 'Pages',
    'palette.instances': 'Instances',
    'palette.actions': 'Actions',
    'palette.refresh': 'Refresh everything',
    'palette.theme_light': 'Switch to light theme',
    'palette.theme_dark': 'Switch to dark theme',
    'palette.empty': 'Nothing matches.',
};
