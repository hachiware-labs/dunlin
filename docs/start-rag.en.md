# RAG: an API that answers your app with sources

[日本語](start-rag.md) · [All introductions](getting-started.en.md)

Build an API that your app or chatbot sends questions to and that answers from your documents, with sources. Responses have the shape of the OpenAI Responses API, so the OpenAI SDK works as is.

## When to use it

- You want to add Q&A based on your documents to an internal portal or a chatbot
- Answers to inquiries must always come with their evidence
- You want answers delivered to your own code, not to an agent

## How it works

```text
app ─question→ Knowledge-Nest ─search→ evidence ─→ answering model ─→ answer with sources ─→ app
```

Unlike MCP, the answer is written by a model that Knowledge-Nest calls. Your app sends one question and receives one answer.

## 1 Prepare an answering model

This page uses `qwen3:4b` on a local Ollama. Start Ollama and pull the model with `ollama pull qwen3:4b`.

```sh
nest config set llm --provider ollama --model qwen3:4b
```

Once registered, every RAG, Wiki, and PARA project you create uses this model. To use an API such as OpenAI, use `--provider openai` and put the API key in the environment variable `OPENAI_API_KEY`. The key never goes in the settings file.

## 2 Create a project and ask

```sh
nest init rag ./rag --source ./manuals
cd rag
nest build
nest ask "ポンプの点検はいつ実施しますか"
```

`ask` numbers the sources in its answer, like `[1]`, and shows the file, lines, and heading of each. To check search alone, use `nest search "..."`.

## 3 Serve it as an API

```sh
nest serve
```

One port, `127.0.0.1:4318`, offers three entry points:

| Entry point | What it does |
|---|---|
| `POST /v1/responses` | Send a question, get an answer with sources (OpenAI Responses API shape) |
| `POST /v1/search` | Returns only search results and the assembled context |
| `/mcp` | MCP for agents. Returns evidence, writes no answer |

### Call it with curl

```sh
curl http://127.0.0.1:4318/v1/responses -H "Content-Type: application/json" -d '{"input":"How often is the mechanical seal replaced?"}'
```

On Windows, Japanese typed directly into `curl` may not be sent as UTF-8. If so, save the JSON to a UTF-8 file and send it with `--data-binary @request.json`.

### Call it with the OpenAI SDK

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:4318/v1", api_key="unused")
response = client.responses.create(model="qwen3:4b", input="How often is the mechanical seal replaced?")
print(response.output_text)
```

Pass the model name set for the project as `model`; a different name gives `MODEL_MISMATCH`. Citations in the answer are in `output[].content[].annotations`, and the cited document versions and ranges in the original are in `knowledge_nest_citations`.

## 4 Update the documents

After adding or editing documents, run `nest build`. Only changed documents are processed, and a running `serve` uses the new index from the next request.

## 5 Expose it to other machines

By default `serve` accepts connections only from your own machine (`127.0.0.1`). To let other machines use it, set `serve.access` in `KnowledgeNestfile.yaml`:

- `auth.type: token` and `token_env` (the name of the environment variable holding the token). Clients send `Authorization: Bearer <token>`
- `allowed_hosts` and `allowed_origins` (accepted host names and browser origins)
- `serve.limits` (body size, time, and concurrent requests)

[HTTP access limits](mcp.md#http-の接続制限) (Japanese)

## 6 When something goes wrong

| Symptom | What to do |
|---|---|
| `/v1/responses` returns `503 MODEL_REQUIRED` | No answering model is set. Set `models.answerer.model` and restart `serve`. Search (`/v1/search`) and MCP work without a model |
| The model cannot be reached | Check that Ollama is running and the model is pulled. For an API, check the environment variable named in `api_key_env` |
| `MODEL_MISMATCH` | Leave `model` out, or use the project's model name |
| `INVALID_JSON` | Send the body as UTF-8 JSON |
| The port is in use | Pick a free one, like `nest serve --port 4320` |

Other cases are in [Troubleshooting](troubleshooting.md) (Japanese).

## Where next

- Improve answers by numbers: [Guide](guide.en.md) · [Evaluation and dictionary](evaluation.md) (Japanese)
- Let an agent search directly: [MCP](start-mcp.en.md)
- Response shape and settings in detail: [MCP and RAG](mcp.md) (Japanese)
