import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from 'typescript';

// Load the production parser without requiring browser globals or a bundled application.
const source = await readFile(new URL('../src/lib/api/sse.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext },
});
const { streamChatCompletion } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`
);

/** Feeds gateway frames through the real parser and records its public callbacks. */
async function receive(t, body) {
  t.mock.method(globalThis, 'fetch', async () => new Response(body));
  const result = { chunks: [], errors: [], finishes: [] };
  await streamChatCompletion({ session_id: 'test', message: 'hello' }, {
    onChunk: (delta, reasoning) => result.chunks.push({ delta, reasoning }),
    onError: (error) => result.errors.push(error.message),
    onFinish: (reason) => result.finishes.push(reason),
  });
  return result;
}

test('a gateway error preserves partial output and never reports completion', async (t) => {
  const result = await receive(t,
    'data: {"type":"delta","delta":"partial"}\n\n' +
    'data: {"type":"error","message":"provider timed out"}\n\n',
  );
  assert.deepEqual(result, {
    chunks: [{ delta: 'partial', reasoning: undefined }],
    errors: ['provider timed out'],
    finishes: [],
  });
});

test('an error before the first token is visible', async (t) => {
  const result = await receive(t, 'data: {"type":"error","message":"tool failed"}\n\n');
  assert.deepEqual(result.errors, ['tool failed']);
  assert.deepEqual(result.finishes, []);
});

test('a done frame completes once even without a provider finish reason', async (t) => {
  const result = await receive(t,
    ': keep-alive\n\ndata: {"type":"done","delta":"answer","reasoning":"reason","finish_reason":null}\n\n',
  );
  assert.deepEqual(result, {
    chunks: [{ delta: 'answer', reasoning: 'reason' }],
    errors: [],
    finishes: ['stop'],
  });
});

test('EOF without a terminal frame reports an interrupted response', async (t) => {
  const result = await receive(t, 'data: {"type":"delta","delta":"partial"}\n\n');
  assert.deepEqual(result.errors, ['Chat stream ended before a completion event']);
  assert.deepEqual(result.finishes, []);
});

test('malformed JSON is an error rather than assistant text', async (t) => {
  const result = await receive(t, 'data: {broken\n\n');
  assert.equal(result.errors.length, 1);
  assert.deepEqual(result.chunks, []);
  assert.deepEqual(result.finishes, []);
});
