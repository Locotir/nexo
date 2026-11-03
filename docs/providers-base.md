# Providers: the common base

Every hosted language model is reached over HTTP with a JSON body and answers
either at once or as a stream of small events. Under the vendor names there are
only **four wire protocols** that matter, plus one local variant. A harness that
implements those four as *protocol adapters* and keeps vendors as *presets*
(a base URL, an auth header, a key variable, a model list) can speak to
practically every provider, and adding one more vendor is a configuration
entry, not code.

This document is the specification the adapters are written from. It names
what is the same everywhere (the model of the conversation, the stream, the
usage) and, per protocol, the exact fields that differ.

## 1. Who speaks what

| Protocol | Endpoint | Vendors that serve it natively or compatibly |
|---|---|---|
| **OpenAI Chat Completions** | `POST {base}/chat/completions` | OpenAI, DeepSeek, xAI (legacy but served; Grok Build itself uses it), Groq, Mistral, Together, Fireworks, Cerebras, Perplexity, OpenRouter, Moonshot/Kimi, Alibaba Qwen (DashScope compat), Zhipu/GLM, MiniMax, Gemini (`/v1beta/openai`), GitHub Models, Azure OpenAI (own URL/auth), Ollama (`/v1`), LM Studio, vLLM, llama.cpp, SGLang, LiteLLM, any gateway |
| **OpenAI Responses** | `POST {base}/responses` | OpenAI (preferred for its reasoning models), xAI (recommended by xAI for Grok reasoning models), Azure OpenAI |
| **Anthropic Messages** | `POST {base}/v1/messages` | Anthropic, Bedrock and Vertex (same body, other auth), and the compatible endpoints of DeepSeek (`/anthropic`), MiniMax (`/anthropic`), Moonshot/Kimi, Zhipu/GLM, and self-hosted servers (LMDeploy, BentoML) |
| **Gemini GenerateContent** | `POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse` | Google AI Studio, Vertex (other auth) |
| Ollama native (optional) | `POST {base}/api/chat` (NDJSON) | Ollama; only needed for Ollama-specific options, since `/v1` already works |

Two vendors need a different *auth scheme* rather than a different protocol:
AWS Bedrock (SigV4 signing) and Google Vertex (OAuth bearer). They are
protocol-wise Anthropic Messages and Gemini respectively, so they are a signer
plugged into an existing adapter, to be added when wanted.

Coverage: the first protocol alone reaches about twenty vendors; the four
together reach every provider a coding harness is asked for today.

## 2. What is the same everywhere (the unified model)

The adapters translate to and from **one** internal representation. It is the
representation the curator already uses (role + content blocks), so curated
views travel unchanged to any provider.

### Request

```
Request {
  model: string
  system: string                     // instructions, kept apart from messages
  messages: Message[]                // the conversation (the curated view)
  tools: ToolSpec[]                  // name, description, JSON-schema parameters
  max_tokens: int
  temperature?: float
  reasoning?: { effort: low|medium|high } | { budget_tokens: int }
  stop?: string[]
  stream: true                       // always; a non-streaming call is the same with stream=false
}
Message { role: user|assistant, parts: Part[] }
Part =
  | Text        { text }
  | Reasoning   { text, signature?, encrypted?, redacted? }   // opaque data must be echoed back
  | ToolCall    { id, name, args: object }
  | ToolResult  { call_id, content: string | Part[], is_error }
  | Image       { media_type, data | url }
ToolSpec { name, description, parameters: JSONSchema }
```

Rules that hold for every protocol:

* Tool results are sent by the harness as the next *user*-side message, one
  result per call, in the order of the calls, each tied to the call's `id`.
* Every tool call has an `id`; where the protocol gives none (Gemini) the
  adapter makes one and keeps it stable for the session.
* Reasoning the model returns as opaque data (signatures, encrypted items,
  thought signatures) is stored on the message and echoed back verbatim on
  the next request; readable reasoning text is stored for display and is
  **not** sent back unless the protocol asks for it (DeepSeek rejects it with
  400 when it is).
* The system text and the tool list are sent first and kept byte-stable
  across requests: that is what the providers' prefix caches key on.

### Stream events

Every protocol's stream reduces to the same ten events, in this order of
appearance:

```
Start        { response_id?, model }
TextDelta    { text }
ReasoningDelta { text }
ToolCallStart { id, name }
ToolCallDelta { id, partial_json }         // arguments arrive as JSON fragments
ToolCallEnd   { id, name, args }           // the adapter joins and parses the fragments
Usage        { input, output, cache_read, cache_write, reasoning }
Finish       { reason: stop | length | tool_use | content_filter | error }
Error        { message, status?, retryable }
Done
```

Invariants: at most one `Finish` and one `Usage`; `ToolCallEnd` for every
`ToolCallStart`; the harness turns the events into one assistant `Message`
(text parts, reasoning parts, tool-call parts) and only then decides whether
to run tools. A non-streaming response is parsed into the same events.

### Usage and price

Four numbers price a request everywhere: input tokens, output tokens, cached
input read, cached input written (0 where the vendor has no write price).
Cost = Σ tokens × price per million, with the price table per model from the
preset (or the models.dev catalogue). Reasoning tokens are part of output.

### Errors and retries

* `429`, `500`, `502`, `503`, `529`/`overloaded_error`: retry with exponential
  backoff (1 s, 2 s, 4 s, 8 s, 16 s; honour `retry-after`), at most 5 times.
* `400` whose message names context length / token limit: not retryable; the
  harness compacts the context and tries once more.
* `401`/`403`: not retryable; report which key variable is missing.
* A stream cut mid-way cannot be resumed: the whole request is retried if
  no `TextDelta` reached the user yet, else the partial message is kept and
  the cut reported.

## 3. Per-protocol mapping

### 3.1 OpenAI Chat Completions

* **Auth**: `Authorization: Bearer $KEY`. Azure: `api-key: $KEY` and the
  deployment in the URL.
* **Request**: `messages` carries the system as `{"role":"system"}` (or
  `"developer"` for OpenAI reasoning models) first; tools as
  `tools: [{type:"function", function:{name, description, parameters}}]`;
  tool results as `{"role":"tool", "tool_call_id", "content"}` messages, one
  per call, right after the assistant message that carries the calls;
  `stream: true` and `stream_options: {include_usage: true}` (without it most
  vendors send no usage); `max_tokens` (`max_completion_tokens` for OpenAI's
  newer models); reasoning as `reasoning_effort` (OpenAI, xAI, Groq) or the
  vendor's field.
* **Stream**: SSE; each `data:` line is a JSON chunk; `data: [DONE]` ends it.
  Deltas in `choices[0].delta`: `content` → TextDelta; `reasoning_content`
  (DeepSeek, Kimi, Qwen, xAI, OpenRouter) or `reasoning` → ReasoningDelta;
  `tool_calls[i]` with `index`, first chunk carries `id` and `function.name`
  → ToolCallStart, later chunks `function.arguments` fragments →
  ToolCallDelta. `choices[0].finish_reason` → Finish (`stop`, `length`,
  `tool_calls`→tool_use, `content_filter`). The last chunk (empty `choices`)
  carries `usage`.
* **Usage**: `prompt_tokens`, `completion_tokens`,
  `prompt_tokens_details.cached_tokens` (OpenAI, Groq, xAI, OpenRouter),
  `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` (DeepSeek, where
  `prompt_tokens` = hit + miss), `completion_tokens_details.reasoning_tokens`.
* **Quirks**: some servers stream tool-call arguments whole in one chunk
  (Ollama, llama.cpp): same events, one delta. OpenRouter adds `provider`
  routing fields. Mistral names its reasoning field `content` of type
  `thinking` inside a list: treated as ReasoningDelta.

### 3.2 OpenAI Responses

* **Auth**: as above.
* **Request**: `instructions` carries the system; `input` is a list of items:
  `{"type":"message","role","content":[{"type":"input_text","text"}]}` for
  user turns, the assistant's own `message` and `function_call` items as the
  model returned them, and `{"type":"function_call_output","call_id","output"}`
  for results; `reasoning` items (`encrypted_content`) echoed verbatim
  (xAI returns one on every response and requires it back); tools as
  `tools: [{type:"function", name, description, parameters}]` (flat);
  `store: false` for a stateless harness (or `previous_response_id` to let
  the server keep the thread); `reasoning: {effort}`; `max_output_tokens`.
* **Stream**: SSE of typed events: `response.created` → Start;
  `response.output_text.delta` → TextDelta;
  `response.reasoning_summary_text.delta` / `response.reasoning_text.delta` →
  ReasoningDelta; `response.output_item.added` with `type: function_call` →
  ToolCallStart (`call_id`, `name`);
  `response.function_call_arguments.delta` → ToolCallDelta;
  `response.function_call_arguments.done` → ToolCallEnd;
  `response.completed` carries `response.usage` → Usage and the `status` →
  Finish (`completed`→stop, `incomplete` with `max_output_tokens`→length);
  `response.failed`/`error` → Error.
* **Usage**: `input_tokens`, `output_tokens`,
  `input_tokens_details.cached_tokens`, `output_tokens_details.reasoning_tokens`.

### 3.3 Anthropic Messages

* **Auth**: `x-api-key: $KEY` and `anthropic-version: 2023-06-01`
  (compatible endpoints accept the same; some also take a bearer token).
* **Request**: `system` is a field (string or blocks); `messages` alternate
  user/assistant; content is a list of blocks `text`, `tool_use` (`id`,
  `name`, `input`), `tool_result` (`tool_use_id`, `content`, `is_error`)
  inside a *user* message, `thinking` (`thinking`, `signature`) and
  `redacted_thinking` echoed back verbatim when the turn used tools; tools as
  `tools: [{name, description, input_schema}]`; `max_tokens` is required;
  reasoning as `thinking: {type:"enabled", budget_tokens}`; prompt cache
  breakpoints as `cache_control: {type:"ephemeral"}` on the last block of the
  system, of the tools and of the latest messages (up to four).
* **Stream**: SSE with named events: `message_start` (carries `usage.input_tokens`
  and the cache fields) → Start; `content_block_start` with `type` `text` |
  `thinking` | `tool_use` (`id`, `name`) → nothing or ToolCallStart;
  `content_block_delta` with `delta.type` `text_delta` → TextDelta,
  `thinking_delta` → ReasoningDelta, `signature_delta` → stored on the
  reasoning part, `input_json_delta.partial_json` → ToolCallDelta;
  `content_block_stop` → ToolCallEnd for a tool block; `message_delta`
  carries `delta.stop_reason` (`end_turn`→stop, `max_tokens`→length,
  `tool_use`→tool_use, `refusal`→content_filter) and `usage.output_tokens`;
  `message_stop` → Done; `error` → Error; `ping` ignored.
* **Usage**: `input_tokens`, `output_tokens`, `cache_read_input_tokens`,
  `cache_creation_input_tokens` (the one protocol with a cache *write* count).

### 3.4 Gemini GenerateContent

* **Auth**: `x-goog-api-key: $KEY` (or `?key=`); Vertex: bearer token and a
  project/location URL.
* **Request**: `systemInstruction: {parts:[{text}]}`; `contents` with roles
  `user` | `model` and `parts`: `{text}`, `{functionCall:{name,args}}`,
  `{functionResponse:{name, response:{...}}}` (inside a *user* content),
  `{thought:true, text}` and `thoughtSignature` echoed back on the model's
  parts; tools as `tools: [{functionDeclarations:[{name, description,
  parameters}]}]`; `generationConfig: {maxOutputTokens, temperature,
  thinkingConfig:{thinkingBudget, includeThoughts}}`.
* **Stream**: SSE (`?alt=sse`) of whole `GenerateContentResponse` objects:
  `candidates[0].content.parts[]` where a `text` part with `thought:true` →
  ReasoningDelta, other `text` → TextDelta, `functionCall` → ToolCallStart +
  ToolCallEnd at once (arguments arrive whole; an id is synthesised as
  `call_{n}`); `candidates[0].finishReason` (`STOP`→stop, `MAX_TOKENS`→length,
  `SAFETY`/`RECITATION`→content_filter; a turn with function calls is
  tool_use); `usageMetadata` on the last chunk → Usage.
* **Usage**: `promptTokenCount`, `candidatesTokenCount`,
  `cachedContentTokenCount`, `thoughtsTokenCount`.

### 3.5 Ollama native (optional)

NDJSON lines `{message:{role, content, thinking?, tool_calls?}, done,
done_reason, prompt_eval_count, eval_count}`; tool calls arrive whole; the
same events. Only worth an adapter for options `/v1` does not expose
(`num_ctx`, `keep_alive`).

## 4. Prompt caches: the part the curator is for

| Vendor | Cache | Minimum | Reported as |
|---|---|---|---|
| OpenAI, xAI, Groq | automatic on the prefix | ~1024 tokens | `cached_tokens` |
| DeepSeek | automatic, 64-token blocks | 64 tokens | `prompt_cache_hit_tokens` |
| Anthropic | explicit breakpoints, 5 min (or 1 h) TTL | 1024–4096 tokens | `cache_read_input_tokens`, `cache_creation_input_tokens` |
| Gemini | implicit on the prefix (+ explicit caches) | 1024–4096 tokens | `cachedContentTokenCount` |
| Ollama / local | the KV cache of the running model | – | – |

All of them key on a byte-identical prefix: system, tools, then messages in
order. A curator that only ever appends, or rewrites at the moments the
cache has expired anyway, keeps the hit rate; one that edits the middle of
the history pays the whole prompt again. That is the cost model the curator
in this repository optimises, and why its view is applied *before* the
adapter: the adapter never reorders or rewrites what it is given.

## 5. Adding a provider

A **vendor** is a preset:

```toml
[providers.deepseek]
protocol = "openai-chat"              # or openai-responses | anthropic | gemini
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"      # the key itself is never written to a file
models = ["deepseek-chat", "deepseek-reasoner"]
[providers.deepseek.pricing."deepseek-chat"]   # per million tokens, optional
input = 0.28
output = 0.42
cache_read = 0.028
```

A **protocol** is an adapter with three responsibilities: build the request
body and headers from a `Request`; turn the response (stream or not) into the
events above; read the usage. Nothing else in the harness knows which
protocol is in use. Model names, context windows and prices come from the
preset, from the vendor's `/models` list where it exists, or from the
models.dev catalogue (the open dataset OpenCode uses) when fetched and
cached.

## 6. Checklist for an adapter

1. system placed where the protocol wants it, never duplicated in messages;
2. tool results carried in the protocol's shape, one per call, ids matching;
3. opaque reasoning data echoed back verbatim; readable reasoning not sent
   where the vendor rejects it;
4. usage read from the last chunk even when `choices` is empty;
5. tool-call argument fragments joined per call `index`/`id` before parsing;
6. finish reason normalised; a turn with any tool call is `tool_use`;
7. no reordering, trimming or rewriting of the messages given (the curator's job);
8. `retry-after` honoured; a context-length 400 reported as such;
9. a recorded fixture of a real stream per protocol in the tests, replayed
   through the parser.
