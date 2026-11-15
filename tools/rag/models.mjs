import { chargeTrialApi } from './project-trials.mjs';

const openaiBase = 'https://api.openai.com/v1';
const ollamaBase = 'http://127.0.0.1:11434';
const strataBase = 'http://127.0.0.1:8080/v1';

export function createGenerator(provider, model, baseUrl, apiKey, { maxInputBytes, includeUsage = false } = {}) {
  switch (provider) {
    case 'openai': {
      const key = apiKey ?? openaiKey();
      const url = endpoint(baseUrl ?? process.env.OPENAI_BASE_URL ?? openaiBase, 'responses');
      return async (instructions, input, schema, signal) => {
        const result = await post(provider, url, key, {
          model, instructions, input, store: false, max_output_tokens: 2048,
          text: { format: { type: 'json_schema', name: 'grounded_answer', strict: true, schema } },
        }, signal, maxInputBytes);
        if (result.status !== 'completed') throw new Error(`OpenAI response status: ${result.status ?? 'unknown'}`);
        const content = result.output?.flatMap((item) => item.type === 'message' ? item.content : []) ?? [];
        if (content.some((part) => part.type === 'refusal')) throw new Error('OpenAI declined the request');
        const output = content.filter((part) => part.type === 'output_text').map((part) => part.text).join('');
        return includeUsage ? { text: output, usage: result.usage } : output;
      };
    }
    case 'ollama': {
      const url = endpoint(baseUrl ?? process.env.OLLAMA_BASE_URL ?? ollamaBase, 'api/chat');
      return async (instructions, input, schema, signal) => {
        const result = await post(provider, url, apiKey, {
          model, messages: chatMessages(instructions, input), format: schema, stream: false,
          think: false, options: { temperature: 0, num_predict: 1024 },
        }, signal, maxInputBytes);
        if (!result.done || result.done_reason === 'length') throw new Error('Ollama response was incomplete');
        return includeUsage ? { text: result.message?.content,
          usage: { input_tokens: result.prompt_eval_count, output_tokens: result.eval_count } } : result.message?.content;
      };
    }
    case 'strata': {
      const url = endpoint(baseUrl ?? process.env.STRATA_BASE_URL ?? strataBase, 'chat/completions');
      const key = apiKey ?? process.env.STRATA_API_KEY;
      return async (instructions, input, schema, signal) => {
        const result = await post(provider, url, key, {
          model, messages: chatMessages(instructions, input), stream: false, max_tokens: 1024,
          temperature: 0, reasoning_effort: 'none',
          response_format: { type: 'json_schema', json_schema: { name: 'grounded_answer', strict: true, schema } },
        }, signal, maxInputBytes);
        const choice = result.choices?.[0];
        if (choice?.finish_reason !== 'stop') throw new Error('Strata response was incomplete');
        return includeUsage ? { text: choice.message?.content,
          usage: { input_tokens: result.usage?.prompt_tokens, output_tokens: result.usage?.completion_tokens } } : choice.message?.content;
      };
    }
    default:
      throw new Error('Expected --provider openai, ollama, or strata');
  }
}

export function createEmbedder(provider, model, baseUrl, apiKey, { maxInputBytes } = {}) {
  if (provider === 'ollama') {
    const url = endpoint(baseUrl ?? process.env.OLLAMA_BASE_URL ?? ollamaBase, 'api/embed');
    return {
      identity: [provider, model, url.href],
      async embed(input, signal) {
        const result = await post('Ollama embeddings', url, apiKey, { model, input, truncate: false }, signal, maxInputBytes);
        return result.embeddings;
      },
    };
  }
  if (provider === 'openai') {
    const url = endpoint(baseUrl ?? process.env.OPENAI_BASE_URL ?? openaiBase, 'embeddings');
    const key = apiKey ?? openaiKey();
    return {
      identity: [provider, model, url.href],
      async embed(input, signal) {
        const result = await post('OpenAI embeddings', url, key, { model, input, encoding_format: 'float' }, signal, maxInputBytes);
        const vectors = [];
        for (const { index, embedding } of result.data ?? []) {
          if (!Number.isSafeInteger(index) || index < 0 || index >= input.length || vectors[index] !== undefined) {
            throw new Error('OpenAI returned an invalid embedding index');
          }
          vectors[index] = embedding;
        }
        return vectors;
      },
    };
  }
  throw new Error('Expected --embedding-provider ollama or openai');
}

function openaiKey() {
  const key = process.env.OPENAI_API_KEY;
  if (!key?.trim()) throw new Error('Set OPENAI_API_KEY to use OpenAI');
  return key;
}

function chatMessages(instructions, input) {
  return [{ role: 'system', content: instructions }, { role: 'user', content: input }];
}

function endpoint(base, route) {
  const url = new URL(base);
  if (!['https:', 'http:'].includes(url.protocol) || url.search || url.hash || url.username || url.password) {
    throw new Error('Expected an HTTP(S) model base URL without credentials, query, or fragment');
  }
  url.pathname = `${url.pathname.replace(/\/+$/u, '')}/${route}`;
  return url;
}

async function post(provider, url, key, body, signal, maxInputBytes) {
  const payload = JSON.stringify(body);
  if (maxInputBytes !== undefined && Buffer.byteLength(payload) > maxInputBytes) {
    throw Object.assign(new Error('The model request exceeds max_input_bytes.'), { code: 'MODEL_INPUT_LIMIT', exitCode: 2 });
  }
  signal = chargeTrialApi(payload, signal);
  const timeout = AbortSignal.timeout(120_000);
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...(key && { authorization: `Bearer ${key}` }) },
    body: payload,
    signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    redirect: 'error',
  });
  if (!response.ok) {
    await response.body?.cancel();
    throw new Error(`${provider} generation failed (HTTP ${response.status})`);
  }
  return response.json();
}
