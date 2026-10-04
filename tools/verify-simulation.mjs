/**
 * End-to-end simulation checks against an isolated node and local fake OneBot/model servers.
 * Run after `cargo build -p kanon`: `bun tools/verify-simulation.mjs`.
 * No real credentials, production data, adapters or external services are used.
 */
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const root = await mkdtemp(join(tmpdir(), 'kanon-simulation-'));
const requests = [];
const deliveries = [];
let connection;
let milkyConnection;
let serial = 1;
let slow = false;
const server = Bun.serve({
  hostname: '127.0.0.1', port: 0,
  async fetch(request, server) {
    const url = new URL(request.url);
    if (['/onebot', '/event'].includes(url.pathname) && server.upgrade(request, { data: { kind: url.pathname } })) return;
    if (url.pathname.startsWith('/api/')) {
      const endpoint = url.pathname.split('/').at(-1);
      const data = endpoint === 'get_login_info' ? { uin: 10001, nickname: 'Milky check' }
        : endpoint === 'get_impl_info' ? { impl_name: 'FakeMilky', impl_version: '1', qq_protocol_version: '1', qq_protocol_type: 'linux', milky_version: '1.1' }
        : { message_seq: serial++, time: Math.floor(Date.now()/1000) };
      return Response.json({ status: 'ok', retcode: 0, data });
    }
    if (url.pathname !== '/v1/chat/completions') return new Response('missing', { status: 404 });
    const body = await request.json();
    requests.push(body);
    const user = body.messages.findLast((message) => message.role === 'user');
    const text = typeof user.content === 'string' ? user.content : JSON.stringify(user.content);
    const last = body.messages.at(-1);
    const calls = [];
    const action = (name, args = {}) => calls.push({ id: `tool_${serial++}`, type: 'function', function: { name, arguments: JSON.stringify(args) } });
    const summarizing = text.startsWith('The conversation above is about to be compacted');
    if (last.role === 'user' && !summarizing) {
      if (text.includes('ASSISTANT_RULES')) { /* Ordinary assistant speech exercises independent guidance. */ }
      else if (text.includes('HANG')) { slow = true; await Bun.sleep(15000); }
      else if (text.includes('DELAYED_SAY')) { await Bun.sleep(500); action('conversation_say', { text: 'MUST_NOT_SEND_AFTER_DISABLE' }); action('conversation_leave'); }
      else if (text.includes('TOO_LONG')) { action('conversation_say', { text: '长'.repeat(121) }); action('conversation_leave'); }
      else if (text.includes('REQUESTED_DETAIL')) { action('conversation_say', { text: '详'.repeat(180), expanded: true }); action('conversation_leave'); }
      else if (text.includes('PLAIN_ONLY')) { /* Ordinary final text must never reach the chat. */ }
      else if (text.includes('SILENT')) action('conversation_leave');
      else if (text.includes('WAIT_STOP') || text.includes('COMMAND_BARRIER')) action('conversation_wait', { seconds: 3 });
      else if (text.includes('LIMIT')) {
        for (let i = 0; i < 4; i++) action('conversation_say', { text: `limited-${i}` });
        action('conversation_wait', { seconds: 3 });
      } else {
        action('conversation_say', { text: text.includes('FOLLOW_B') ? '继续聊' : '接上了\n这是完整一条' });
        action(text.includes('HELLO_A') ? 'conversation_wait' : 'conversation_leave', text.includes('HELLO_A') ? { seconds: 3 } : {});
      }
    }
    return Response.json({ id: 'mock', object: 'chat.completion', model: 'fake', choices: [{ index: 0,
      message: { role: 'assistant', content: calls.length ? null : summarizing ? 'SUMMARY_MARKER: The group discussed astronomy.' : 'INTERNAL_FINAL_DO_NOT_SEND', ...(calls.length ? { tool_calls: calls } : {}) },
      finish_reason: calls.length ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 50, completion_tokens: 5, total_tokens: 55 } });
  },
  websocket: {
    open(ws) { if (ws.data.kind === '/event') milkyConnection = ws; else connection = ws; },
    message(ws, raw) {
      const message = JSON.parse(String(raw));
      let data = {};
      if (message.action === 'get_group_info') data = { group_id: message.params.group_id, group_name: '测试群', member_count: 3, max_member_count: 100 };
      if (message.action === 'get_login_info') data = { user_id: 999, nickname: '测试机器人' };
      if (message.action === 'get_status') data = { online: true, good: true };
      if (message.action === 'send_group_msg' || message.action === 'send_private_msg') {
        deliveries.push(message);
        data = { message_id: serial++ };
      }
      ws.send(JSON.stringify({ status: 'ok', retcode: 0, data, echo: message.echo }));
    },
  },
});
const portReservation = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: () => new Response('reserved') });
const apiPort = portReservation.port;
portReservation.stop(true);
const apiBase = `http://127.0.0.1:${apiPort}`;
await mkdir(join(root, 'data'));
await writeFile(join(root, 'data/system.json'), JSON.stringify({
  startup: { api_addr: `127.0.0.1:${apiPort}`, run_dir: 'run', log: 'info', install_dependencies: false },
  providers: [{ name: 'local', protocol: 'openai', base_url: `http://127.0.0.1:${server.port}/v1` }],
  default_model: 'local/fake',
  models: [{ provider: 'local', model: 'fake', capabilities: { text: true, tool_calling: true }, context_length: 100000 }],
  milky: { enabled: true, platform: 'milky', transport: 'websocket', base_url: `http://127.0.0.1:${server.port}` },
  onebot: { enabled: true, platform: 'onebot', ws_url: `ws://127.0.0.1:${server.port}/onebot` },
}));
let child;
let logs = '';
function launch() {
  child = Bun.spawn([resolve('target/debug/kanon')], { cwd: root, stdout: 'pipe', stderr: 'pipe' });
  for (const stream of [child.stdout, child.stderr]) (async () => { for await (const chunk of stream) logs += new TextDecoder().decode(chunk); })();
}
async function waitFor(condition, label, timeout = 7000) {
  const until = Date.now() + timeout;
  while (Date.now() < until) { if (await condition()) return; await Bun.sleep(25); }
  throw new Error(`Timed out: ${label}`);
}
async function api(path, method = 'GET', body) {
  const response = await fetch(apiBase + path, { method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
  assert(response.ok, `${method} ${path}: ${response.status} ${await response.clone().text()}`);
  return response.json();
}
function send(text, { group = 123, sender = 456, mention = true } = {}) {
  const id = serial++;
  connection.send(JSON.stringify({ post_type: 'message', message_type: 'group', self_id: 999, group_id: group, user_id: sender, message_id: id, time: Math.floor(Date.now()/1000),
    sender: { user_id: sender, nickname: sender === 456 ? '小明' : '小红', card: sender === 456 ? '明明' : '红红', role: 'owner' },
    raw_message: text, message: [...(mention ? [{ type: 'at', data: { qq: '999' } }] : []), { type: 'text', data: { text: mention ? ` ${text}` : text } }] }));
  return id;
}
const currentRequests = () => requests.filter((request) => request.messages.at(-1)?.role === 'user');
let draft = { name: 'Simulation check', enabled: true, adapters: ['onebot', 'milky'], conversation_mode: 'simulation', system_prompt: 'PERSONA_MARKER: You are a calm and concise astronomy enthusiast.',
  simulation: { quiet_ms: 200, max_batch_ms: 600, listen_seconds: 3, max_participation_seconds: 10, max_messages: 5 },
  reply_policy: { mode: 'mention', probability: 1, split_lines: true, acknowledge: true, send_reasoning: true },
  context_policy: { include_channel_id: true, include_sender_id: true, include_timestamp: true, expand_forward: true },
};
let id;
try {
  launch();
  await waitFor(async () => { try { return (await fetch(apiBase + '/api/v1/instances')).ok && connection; } catch { return false; } }, 'startup');
  const preset = (await api('/api/v1/instances', 'POST', { name: 'Preset check', conversation_mode: 'simulation' })).instance;
  assert.equal(preset.conversation_rules, true, 'enabling simulation should default rules on');
  assert.deepEqual(preset.simulation, { quiet_ms: 2500, max_batch_ms: 10000, listen_seconds: 30, max_participation_seconds: 180, max_messages: 3 });
  const off = (await api(`/api/v1/instances/${preset.id}`, 'PUT', { name: preset.name, conversation_mode: 'assistant' })).instance;
  assert.equal(off.conversation_rules, true, 'disabling simulation should preserve rules');
  await api(`/api/v1/instances/${preset.id}`, 'DELETE');
  id = (await api('/api/v1/instances', 'POST', draft)).instance.id;
  // Observation should not wake the model, and the background name lookup should fill its cache.
  send('OBSERVED_BEFORE', { mention: false });
  await Bun.sleep(250);
  assert.equal(requests.length, 0);
  send('HELLO_A first thought');
  await Bun.sleep(50);
  send('HELLO_A second thought', { sender: 789, mention: false });
  await waitFor(() => deliveries.length === 1, 'first explicit speech');
  assert.equal(currentRequests().length, 1, 'burst should form one current user turn');
  const first = requests[0];
  const firstText = first.messages.at(-1).content;
  for (const expected of ['OBSERVED_BEFORE','first thought','second thought','昵称="小明"','群名片="红红"','QQ号="789"','QQ群号="123"','群名="测试群"']) assert(firstText.includes(expected), expected);
  assert(first.messages[0].content.includes('PERSONA_MARKER'));
  assert(first.messages[0].content.includes('Conversation mode: simulation'));
  assert(first.messages[0].content.includes('considerate participant'));
  assert.equal(first.messages.filter((message) => message.role === 'system').length, 1);
  const names = first.tools.map((tool) => tool.function.name);
  assert.deepEqual(names, [...names].sort());
  await Bun.sleep(200);
  send('FOLLOW_B without another mention', { sender: 789, mention: false });
  await waitFor(() => deliveries.length === 2, 'unmentioned follow-up while listening');
  await waitFor(() => requests.length >= 4, 'follow-up finalization');
  for (const request of requests) {
    assert.deepEqual(request.tools, first.tools, 'tool prefix changed within participation');
    assert.deepEqual(request.messages[0], first.messages[0], 'system prefix changed');
  }
  const following = currentRequests()[1];
  assert.deepEqual(following.messages.slice(0, first.messages.length), first.messages, 'history was not append-only');
  assert(!JSON.stringify(deliveries).includes('INTERNAL_FINAL'));
  assert.equal(deliveries[0].params.message.filter((segment) => segment.type === 'text').length, 1, 'speech was split');
  const beforeQuiet = deliveries.length;
  send('SILENT', { group: 124 });
  send('PLAIN_ONLY', { group: 125 });
  await waitFor(() => currentRequests().length === 4, 'independent groups');
  await Bun.sleep(250);
  assert.equal(deliveries.length, beforeQuiet, 'silence/internal final leaked');
  send('WAIT_STOP', { group: 126 });
  await waitFor(() => requests.some((request) => request.messages.at(-1)?.role === 'tool' && request.messages.some((message) => JSON.stringify(message).includes('WAIT_STOP'))), 'listening selected');
  const requestCount = currentRequests().length;
  send('/stop', { group: 126 });
  await waitFor(() => deliveries.length > beforeQuiet, '/stop response');
  send('AFTER_STOP', { group: 126, mention: false });
  await Bun.sleep(500);
  assert.equal(currentRequests().length, requestCount, '/stop left a live listener');
  draft = { ...draft, conversation_rules: false, simulation: { ...draft.simulation, max_messages: 2 } };
  await api(`/api/v1/instances/${id}`, 'PUT', draft);
  const beforeLimit = deliveries.length;
  send('LIMIT', { group: 127 });
  await waitFor(() => deliveries.length === beforeLimit + 1, 'one contribution per batch');
  await Bun.sleep(250);
  assert.equal(deliveries.length, beforeLimit + 1, 'batch produced multiple speeches');
  send('LIMIT_FOLLOW', { group: 127, mention: false });
  await waitFor(() => deliveries.length === beforeLimit + 2, 'speech limit');
  await Bun.sleep(350);
  assert.equal(deliveries.length, beforeLimit + 2);
  send('LIMIT_AFTER', { group: 127, mention: false });
  await Bun.sleep(300);
  assert.equal(deliveries.length, beforeLimit + 2, 'participation cap left a live speaker');
  const limit = currentRequests().at(-1);
  assert(limit.messages[0].content.includes('PERSONA_MARKER'));
  assert(limit.messages[0].content.includes('Conversation mode: simulation'));
  assert(!limit.messages[0].content.includes('considerate participant'));
  const beforeLong = deliveries.length;
  send('TOO_LONG', { group: 135 });
  await waitFor(() => requests.some((r) => r.messages.some((m) => m.role === 'tool' && JSON.stringify(m).includes('Casual speech exceeds'))), 'long casual message rejected');
  assert.equal(deliveries.length, beforeLong);
  send('REQUESTED_DETAIL', { group: 136 });
  await waitFor(() => deliveries.length === beforeLong + 1, 'explicit detail can expand');
  const invalid = await fetch(apiBase + `/api/v1/instances/${id}`, { method: 'PUT', headers: { 'content-type':'application/json' }, body: JSON.stringify({ ...draft, simulation: { ...draft.simulation, max_messages: 0 } }) });
  assert.equal(invalid.status, 400, 'invalid limits accepted');
  // A queued generation-changing command must precede the message following it.
  send('COMMAND_BARRIER', { group: 130 });
  await waitFor(() => currentRequests().some((r) => JSON.stringify(r.messages.at(-1)).includes('COMMAND_BARRIER')), 'barrier owner');
  send('/new', { group: 130 });
  send('AFTER_NEW', { group: 130 });
  await waitFor(() => currentRequests().some((r) => JSON.stringify(r.messages.at(-1)).includes('AFTER_NEW')), 'message after /new');
  const afterNew = currentRequests().find((r) => JSON.stringify(r.messages.at(-1)).includes('AFTER_NEW'));
  assert(!JSON.stringify(afterNew).includes('COMMAND_BARRIER'), 'message overtook /new');
  await Bun.sleep(250);
  send('HANG', { group: 128 });
  await waitFor(() => slow, 'hung provider');
  for (let i = 0; i < 70; i++) send(`OVERLOAD_${i}`, { group: 128, mention: false });
  await Bun.sleep(600);
  send('/stop', { group: 128, mention: false });
  await Bun.sleep(500);
  const deadFiles = await readdir(join(root, 'data/dead_letter'));
  const dead = (await Promise.all(deadFiles.map((file) => readFile(join(root,'data/dead_letter',file),'utf8')))).join('\n');
  for (let i = 0; i < 70; i++) assert(dead.includes(`OVERLOAD_${i}`), `accepted event ${i} was lost`);
  const beforeDisable = deliveries.length;
  send('DELAYED_SAY', { group: 129 });
  await waitFor(() => currentRequests().some((r) => JSON.stringify(r.messages.at(-1)).includes('DELAYED_SAY')), 'pending speech');
  await api(`/api/v1/instances/${id}`, 'PUT', { ...draft, enabled: false });
  await Bun.sleep(700);
  assert.equal(deliveries.length, beforeDisable, 'disabled instance sent a late reply');
  await api(`/api/v1/instances/${id}`, 'PUT', draft);
  slow = false;
  const beforeDeadline = deliveries.length;
  send('HANG_DEADLINE', { group: 131 });
  await waitFor(() => slow, 'deadline model');
  await Bun.sleep(10500);
  assert.equal(deliveries.length, beforeDeadline, 'expired model sent a reply');
  const countAtDeadline = currentRequests().length;
  send('AFTER_DEADLINE', { group: 131, mention: false });
  await Bun.sleep(300);
  assert.equal(currentRequests().length, countAtDeadline, 'expired participation kept listening');
  const persisted = JSON.parse(await readFile(join(root, 'data/instances.json'), 'utf8'));
  assert(JSON.stringify(persisted).includes('simulation'));
  child.kill('SIGINT');
  await child.exited;
  const legacy = JSON.parse(await readFile(join(root, 'data/instances.json'), 'utf8'));
  legacy.instances[0].simulation.behavior_prompt = legacy.instances[0].conversation_rules;
  delete legacy.instances[0].conversation_rules;
  await writeFile(join(root, 'data/instances.json'), JSON.stringify(legacy));
  connection = undefined;
  milkyConnection = undefined;
  launch();
  await waitFor(async () => { try { return connection && (await api('/api/v1/instances')).instances?.length === 1; } catch { return false; } }, 'restart');
  const restored = (await api('/api/v1/instances')).instances[0];
  assert.equal(restored.conversation_rules, false);
  assert.equal(restored.simulation.max_messages, 2);
  send('RESTART_CONTINUATION', { group: 123 });
  await waitFor(() => currentRequests().some((r) => JSON.stringify(r.messages.at(-1)).includes('RESTART_CONTINUATION')), 'durable session continuation');
  assert(JSON.stringify(currentRequests().at(-1)).includes('FOLLOW_B'), 'history lost on restart');
  await Bun.sleep(250);
  draft = { ...draft, reply_policy: { ...draft.reply_policy, mode: 'always' } };
  await api(`/api/v1/instances/${id}`, 'PUT', draft);
  await waitFor(() => milkyConnection, 'second platform');
  milkyConnection.send(JSON.stringify({ time: Math.floor(Date.now()/1000), self_id: 10001, event_type: 'message_receive', data: {
    message_scene: 'group', peer_id: 123, message_seq: serial++, sender_id: 456, time: Math.floor(Date.now()/1000),
    segments: [{ type: 'text', data: { text: 'SILENT_CROSS_PLATFORM' } }],
    group: { group_id: 123, group_name: 'Another platform', member_count: 3, max_member_count: 200, remark: '', created_time: 1600000000, description: '', question: '', announcement: '' },
    group_member: { user_id: 456, nickname: 'Milky member', sex: 'unknown', group_id: 123, card: 'Member card', title: '', level: 1, role: 'member', join_time: 1600000000, last_sent_time: 1700000000 },
  } }));
  await waitFor(() => currentRequests().some((r) => JSON.stringify(r.messages.at(-1)).includes('SILENT_CROSS_PLATFORM')), 'cross-platform message');
  const cross = currentRequests().find((r) => JSON.stringify(r.messages.at(-1)).includes('SILENT_CROSS_PLATFORM'));
  assert(!JSON.stringify(cross).includes('FOLLOW_B'), 'same-number groups on different platforms shared history');
  // Rules must work without the simulation protocol, and stay independently switchable.
  await Bun.sleep(250);
  const assistant = { ...draft, conversation_mode: 'assistant', conversation_rules: true,
    reply_policy: { ...draft.reply_policy, split_lines: false, acknowledge: false, send_reasoning: false } };
  await api(`/api/v1/instances/${id}`, 'PUT', assistant);
  const beforeAssistant = deliveries.length;
  send('ASSISTANT_RULES_ON', { group: 133 });
  await waitFor(() => deliveries.length > beforeAssistant, 'assistant with supplementary rules');
  const rulesOn = currentRequests().find((r) => JSON.stringify(r.messages.at(-1)).includes('ASSISTANT_RULES_ON'));
  assert(rulesOn.messages[0].content.includes('considerate participant'));
  assert(!rulesOn.messages[0].content.includes('Conversation mode: simulation'));
  assert(!(rulesOn.tools ?? []).some((tool) => tool.function.name.startsWith('conversation_')));
  await api(`/api/v1/instances/${id}`, 'PUT', { ...assistant, conversation_rules: false });
  send('ASSISTANT_RULES_OFF', { group: 134 });
  await waitFor(() => currentRequests().some((r) => JSON.stringify(r.messages.at(-1)).includes('ASSISTANT_RULES_OFF')), 'assistant without rules');
  const rulesOff = currentRequests().find((r) => JSON.stringify(r.messages.at(-1)).includes('ASSISTANT_RULES_OFF'));
  assert(!rulesOff.messages[0].content.includes('considerate participant'));
  await Bun.sleep(250);
  const { conversation_rules: _, ...reenabled } = draft;
  const toggled = (await api(`/api/v1/instances/${id}`, 'PUT', reenabled)).instance;
  assert.equal(toggled.conversation_rules, true, 're-enabling simulation should enable rules when omitted');
  await api(`/api/v1/instances/${id}`, 'PUT', draft);
  await api('/api/v1/models', 'PUT', { provider: 'local', model: 'fake', capabilities: { text: true, tool_calling: true }, context_length: 64 });
  const beforeCompact = requests.length;
  send('COMPACT_TARGET', { group: 132 });
  await waitFor(() => requests.slice(beforeCompact).some((r) => String(r.messages.at(-1)?.content).startsWith('The conversation above is about to be compacted')), 'background compaction');
  const compactRequests = requests.slice(beforeCompact);
  const ordinary = compactRequests[0];
  const summary = compactRequests.find((r) => String(r.messages.at(-1)?.content).startsWith('The conversation above is about to be compacted'));
  assert.deepEqual(summary.tools, ordinary.tools, 'compaction changed the tools');
  assert.deepEqual(summary.messages[0], ordinary.messages[0], 'compaction lost simulation instructions');
  assert.deepEqual(summary.messages.slice(0, ordinary.messages.length), ordinary.messages, 'compaction rewrote history prefix');
  console.log('PASS: independent switches, ready-to-use preset, batching, listen/continue, silence, custom persona, stable prefix, identities, group/platform isolation, stop, limits, FIFO commands, overload, hot disable, deadline, restart and compaction.');
} finally {
  if (child && child.exitCode === null) { child.kill('SIGINT'); await child.exited; }
  server.stop(true);
  await writeFile(join(root, 'node.log'), logs);
  await writeFile(join(root, 'requests.json'), JSON.stringify(requests, null, 2));
  await writeFile(join(root, 'deliveries.json'), JSON.stringify(deliveries, null, 2));
  console.log(`Verification artifacts: ${root}`);
}
