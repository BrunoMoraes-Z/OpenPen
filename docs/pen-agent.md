# pen-agent — núcleo do chat embutido

O crate `crates/pen-agent` roda **uma sessão de chat** com um agente, por um de dois motores
escolhidos pelo usuário, e entrega **um único fluxo de eventos normalizado** para o app. Não tem
GPUI nem dependência de `pen-doc`; o app pluga o documento pelo trait `ToolExecutor` (motor da
API) ou pelo servidor MCP que já existe (motor do Claude Code). Referência do comportamento
legado: `docs/legacy-analysis/04-mcp-headless-agentes.md` §3 e §1.6.

## API pública

```rust
pub fn start_session(config: SessionConfig) -> Result<Session, AgentError>;

pub struct SessionConfig {
    pub engine: EngineConfig,          // ClaudeCli(CliConfig) | Api(ApiConfig)
    pub system_prompt: String,         // prompt::chat_system_prompt(SKILL.md)
    pub model: Option<String>,         // None: padrão do CLI / "claude-opus-5-5"
    pub effort: Effort,                // Low (padrão) | Medium | High | XHigh | Max
    pub resume: Option<String>,        // session_id de uma sessão anterior
}

impl Session {
    pub fn send(&self, msg: UserMessage) -> Result<(), AgentError>;  // texto + anexos + contexto
    pub fn send_text(&self, text, attachments: Vec<Attachment>) -> Result<(), AgentError>;
    pub fn steer(&self, text) -> Result<(), AgentError>;             // fala com o turno em andamento
    pub fn stop(&self);                                              // interrompe o turno
    pub fn take_events(&mut self) -> Option<EventReceiver>;          // uma vez
    pub fn session_id(&self) -> Option<String>;                      // para retomar
    pub fn agent_name(&self) -> Option<&str>;                        // CLI: "OpenPen Chat#<marcador>"
}
// Drop da Session: encerra o turno, mata o processo do Claude Code e apaga os temporários.

pub enum AgentEvent {
    TurnStart,
    SessionId(String),
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart { id, name },                 // name sem prefixo MCP ("execute")
    ToolInputDelta { id, partial_json },       // JSON acumulado até agora
    ToolUseEnd { id, input: Value },
    ToolResult { id, ok, text, images },
    Usage(Usage),                              // do turno inteiro
    TurnEnd { stop_reason },                   // end_turn | interrupted | refusal | max_tokens | error | ...
    Error(String),
}
pub struct Usage { input_tokens, output_tokens, cache_read, cache_write, cost_usd: Option<f64> }

pub fn execute_input_prefix(partial_json: &str) -> Option<String>;
```

- `EventReceiver` funciona de qualquer thread ou executor: `recv().await`, `blocking_recv()`,
  `try_recv()` e `impl Stream`. Cada sessão tem uma thread própria (`pen-agent`) com um runtime
  Tokio de thread única; o app não precisa de Tokio.
- `UserMessage { text, attachments, context }`: `context` é texto volátil enviado **antes** do texto
  na mesma mensagem (por exemplo, o `get_app_state` pré-buscado, embrulhado por
  `prompt::app_state_context`). Anexos: `Attachment::Image { media_type, data }` (bloco `image`
  base64) e `Attachment::Text { name, text }`.
- `send` durante um turno enfileira a mensagem para depois dele. `stop()` descarta a fila.
- `prompt::chat_system_prompt(skill_md)` = preâmbulo curto + `SKILL.md` (sem front matter) + a
  regra `<final_message>` (o usuário vê o canvas ao vivo: nada de recapitular; no máximo uma
  frase). `execute.md` e `pen-schema.md` ficam fora e entram por `start_task`, como no legado. O
  prompt é determinístico (faz parte do prefixo em cache).
- `key`: `get_stored_key` / `set_stored_key` / `delete_stored_key` no cofre do sistema (Windows
  Credential Manager via `keyring`, serviço `OpenPen`, usuário `anthropic-api-key`);
  `resolve_api_key()` usa o cofre e depois `ANTHROPIC_API_KEY`; `mask_key` para exibir. A chave
  nunca vai para log, `Debug` (`ApiKey(***)`) ou mensagem de erro.

### `execute_input_prefix`

Extrai o maior prefixo **decodificado** do campo `input` (string, nível de topo) de um objeto
JSON parcial. Trata escapes simples, `\uXXXX`, pares substitutos e escapes cortados no fim (que
ficam de fora até completarem). Aceita `input` antes ou depois de `description`, pula objetos e
arrays aninhados (um `input` aninhado é ignorado) e devolve `None` enquanto o valor não começou ou
se `input` não for string. O resultado só cresce à medida que o JSON cresce, então serve para
alimentar `ScriptEngine::execute_partial` enquanto o modelo escreve.

## Motor 1: Claude Code CLI (padrão)

```
claude -p --input-format stream-json --output-format stream-json --verbose
       --include-partial-messages --strict-mcp-config
       --mcp-config <tmp>/openpen-agent/chat-<marcador>-mcp.json
       --append-system-prompt-file <tmp>/openpen-agent/chat-<marcador>-prompt.md
       --tools WebSearch,WebFetch            (ou "" sem web)
       --allowedTools mcp__openpen__*,WebSearch,WebFetch
       --disallowedTools Bash,PowerShell,Edit,Write,NotebookEdit,Agent
       --permission-mode dontAsk  --effort <effort>  [--model M]  [--resume <session_id>]
```

- Flags conferidas no `claude --help` (2.1.330). `--append-system-prompt-file` (que não aparece
  na ajuda resumida) foi validado com o CLI real. O arquivo evita passar o prompt com quebras de
  linha pela linha de comando, o que quebraria num shim `.cmd`.
- O `--mcp-config` aponta para `openpen-cli mcp --agent "OpenPen Chat#<marcador>"` (mais
  `--socket` e `CliConfig::mcp_extra_args`, por exemplo `--headless arquivo.pen`). O marcador
  (8 hex) é por sessão: o app reconhece as chamadas do chat pelo `CallCtx::agent_name`. Cada
  `tools/call` do Claude Code também traz `_meta["claudecode/toolUseId"]`, que o pen-mcp expõe como
  `CallCtx::tool_use_id`. Esse id é o mesmo `id` dos eventos `ToolUseStart` / `ToolInputDelta`,
  então o app casa o `execute` final com o streaming da mesma chamada.
- Localização do `claude`: `CliConfig::claude_path` ou `which` no PATH (com `PATHEXT`: acha
  `claude.exe` ou um shim `claude.cmd`), depois `~/.local/bin` e `%APPDATA%\npm`. Sem executável,
  `start_session` falha com `AgentError::ClaudeNotFound`.
- Processo: criado no primeiro `send` e mantido vivo entre turnos (stdin aberto, uma linha
  stream-json por mensagem). No Windows usa `CREATE_NO_WINDOW`. `stop()` e o drop matam o processo
  (`kill_on_drop`). A mensagem seguinte cria outro com `--resume <session_id>`, e o mesmo acontece
  se o processo morrer entre turnos. O stderr é capturado (últimos 8 a 16 KiB). Se o processo
  morrer no meio de um turno, sai `Error` com o fim do stderr e o código de saída, seguido de
  `TurnEnd { "error" }`. Texto de autenticação (`/login`, `Invalid API key`, ...) no `result` ou no
  stderr vira `AgentError::ClaudeNotLoggedIn`.
- Pasta de trabalho: `CliConfig::cwd` ou `%TEMP%/openpen-chat`, que é fixa porque o Claude Code
  guarda sessões por pasta e o `--resume` precisa da mesma. Ela não tem `CLAUDE.md` de projeto.
- `control_request` vindo do CLI (pedido de permissão) recebe `deny`. Com `dontAsk` e a allowlist,
  isso não deveria acontecer.
- `steer(text)`: escreve outra mensagem de usuário no stdin. O Claude Code entrega a mensagem
  durante ou logo depois do turno atual. Um `message_start` recebido fora de turno reabre o turno
  (`TurnStart`), então uma mensagem enfileirada aparece como um turno próprio.

### Mapeamento stream-json → eventos (`CliMapper`, função pura)

| Linha | Evento |
|---|---|
| `system/init` | `SessionId` |
| `stream_event` `content_block_start` `tool_use` | `ToolUseStart` (sem `mcp__openpen__`) |
| `stream_event` `text_delta` / `thinking_delta` | `TextDelta` / `ThinkingDelta` (vazios são ignorados) |
| `stream_event` `input_json_delta` | `ToolInputDelta` com o JSON acumulado |
| `assistant` (tool_use) ou `content_block_stop` | `ToolUseEnd` (uma vez por id; o `assistant` costuma chegar **antes** do stop) |
| `assistant` sem `stream_event` antes | texto, raciocínio e ferramentas a partir da mensagem completa |
| `user` com `tool_result` | `ToolResult` (texto concatenado, nº de imagens, `is_error`) |
| `result` | `SessionId`, `Usage` (com `total_cost_usd`), `Error` se `is_error`, `TurnEnd` |
| mensagens com `parent_tool_use_id` (subagentes) | ignoradas |

### O CLI faz streaming do input de ferramentas MCP? **Sim.**

Capturado em 23/11/2026 com Claude Code 2.1.330 e `--include-partial-messages`. A ferramenta
`mcp__openpen__execute` foi servida por `openpen-cli mcp --headless`. O CLI emitiu
`content_block_start` (`tool_use`, `input: {}`) e depois 14 `input_json_delta` com o script
chegando **linha a linha** (`"{\"description\": \"Print a list"`,
`"\", \"input\": \"Print(\\\"line 1\\\");"`, `"\\nPrint(\\\"line 2\\\");"`, …), e só então a
mensagem `assistant` completa, o `content_block_stop` e o `tool_result`. A versão sanitizada (sem
hooks, caminhos ou dados da conta) está em `crates/pen-agent/tests/fixtures/claude-cli-execute.jsonl`;
a bruta ficou em `target/agent-capture/`. Uma segunda execução, já pelo motor (`tests/real.rs`,
com todas as flags acima), gerou 11 `ToolInputDelta` crescentes para um `execute` de 8 linhas.
Ou seja, o **streaming do `execute` funciona com o motor do CLI**: o app alimenta
`execute_partial` com `execute_input_prefix(partial_json)` a partir dos eventos e casa a chamada
MCP final pelo `tool_use_id`.

## Motor 2: API Messages direta

HTTP cru com `reqwest` (rustls com o provedor `ring`, sem aws-lc/NASM) e SSE próprio
(`api::sse::SseParser`, que aceita cortes em qualquer byte). Cabeçalhos: `x-api-key`,
`anthropic-version: 2026-10-21` e `anthropic-beta: server-side-fallback-2026-07-01` (com o
fallback ligado). Corpo:

```json
{ "model": "claude-opus-5-5", "max_tokens": 64000, "stream": true,
  "system": [{"type":"text","text":"<prompt>","cache_control":{"type":"ephemeral"}}],
  "tools": [{"name":"execute", "description":"…", "input_schema":{…}, "eager_input_streaming":true}, …],
  "tool_choice": {"type":"auto"},
  "thinking": {"type":"adaptive","display":"summarized"},
  "output_config": {"effort":"low"},
  "fallbacks": "default",
  "messages": [ …, {"role":"user","content":[…, {…, "cache_control":{"type":"ephemeral"}}]} ] }
```

- **Ferramentas**: as mesmas do MCP (`start_task`, `read_skill`, `get_app_state`, `execute`,
  `save`; o `get_style` fica fora). Os schemas vêm de `pen_mcp::tools::tool_list`, sem `$schema` e
  sem `title`, pela função `pen_tool_defs`. O pen-mcp não precisou mudar. A serialização é
  determinística.
- **Eager input streaming**: cada `input_json_delta` chama `ToolExecutor::on_partial_input(id,
  name, json_acumulado)` e emite `ToolInputDelta`. Como a API não valida mais o input, o laço faz o
  parse estrito e valida contra o schema (`tools::validate_input`: `type`, `required`,
  `properties`, `items`, `enum`, `anyOf`/`oneOf`, `$ref` local). JSON inválido vira `tool_result`
  com `is_error` e `{"INVALID_JSON": "<bruto>"}` (montado com serde). Violação de schema vira
  `is_error` com o motivo. Em nenhum dos dois casos o executor roda, e chamadas com input parcial
  que não vão rodar recebem `on_input_abandoned` (o app faz `cancel_stream`).
- **Stop reasons**: `tool_use` roda as ferramentas **em sequência** e manda **todos** os
  resultados numa única mensagem `user` (imagens como blocos `image` base64, tipo detectado pelos
  bytes). `max_tokens` com `tool_use` não executa nada: guarda `tool_result` de erro para a próxima
  mensagem e emite `Error`. `refusal` não executa nada e ecoa só o texto. `pause_turn` continua.
  Os outros encerram o turno.
- **Raciocínio**: blocos `thinking` (com `signature_delta`) e `redacted_thinking` voltam
  **inalterados** no histórico. O histórico só cresce (append-only); o breakpoint de cache da última
  mensagem é colocado só no corpo serializado de cada requisição.
- **Fallback de recusa**: `fallbacks: "default"`. Se aparecer um bloco `fallback`, o que vem antes
  do último deles é ecoado só como `text` + `fallback`. Raciocínio, ferramentas e outros blocos da
  tentativa recusada são descartados (`assemble::sanitize_fallback`), e as ferramentas descartadas
  recebem `ToolResult { ok: false }`.
- **Cache de prompt**: breakpoint no bloco de `system` (cobre ferramentas + sistema, que não
  mudam) e no último bloco da última mensagem. O conteúdo volátil (estado do editor) vai na
  mensagem de usuário. Modelo, ferramentas e prompt não mudam durante a sessão.
- **Erros e retry**: 429 (exceto teto de gasto `enforced_spend_limit_reached`), 408, 409 e 5xx/529
  são repetidos com backoff exponencial e jitter (padrão: 4 tentativas extras, 1 s → 30 s),
  respeitando `retry-after`. 400/401/403/404/413 não são repetidos. Um `event: error` do SSE
  (`overloaded_error`, `api_error`) também é repetido, mas só se nada foi emitido ainda (senão o
  texto duplicaria). Estouro de tentativas vira `Error` + `TurnEnd { "error" }`.
- **Primeiro turno**: com `ApiConfig::prefetch_app_state` (padrão) e sem `context`, o laço chama
  `get_app_state` pelo executor e envia o resultado embrulhado antes do texto, como no legado.
- **Steering**: `steer(text)` durante o turno entra como bloco de texto **depois** dos
  `tool_result` da próxima rodada. Se o modelo terminar antes, vira uma requisição de
  continuação.
- **Stop**: derruba a requisição ou a ferramenta em andamento e responde as chamadas pendentes
  com `tool_result` de erro ("the user stopped this turn"), que vão na próxima mensagem. O
  histórico continua válido.
- **`ToolExecutor`** (implementado pelo app):

  ```rust
  pub trait ToolExecutor: Send + Sync + 'static {
      fn call(&self, name: &str, input: Value, tool_use_id: &str) -> BoxFuture<'static, ToolOutput>;
      fn on_partial_input(&self, tool_use_id: &str, name: &str, partial_json: &str) {}
      fn on_input_abandoned(&self, tool_use_id: &str, name: &str) {}
  }
  ```

  `BackendExecutor::new(backend: Arc<dyn pen_mcp::ToolBackend>, skills_dir, ctx)` é uma
  implementação pronta sobre o mesmo `ToolBackend` do servidor MCP. Ela responde
  `start_task`/`read_skill` com o `SkillStore`, valida `ExecuteArgs`, põe o `tool_use_id` em
  `CallCtx::tool_use_id` e aceita ganchos `with_partial_hook` / `with_abandon_hook`.
  `ToolOutput` é o do pen-mcp.

## Testes

- `src/partial.rs`: todo prefixo de vários JSONs (descrição antes e depois, `uri`, espaços,
  escapes, `\u` maiúsculo e minúsculo, pares substitutos, substitutos soltos, chave escapada,
  chaves parecidas, `input` aninhado, valores não-string), escapes cortados, os pedaços reais do
  CLI e um script de 200 linhas cortado em cada byte. Em todos, o resultado só cresce e é prefixo
  do final.
- `src/api/sse.rs`: cortes em cada byte (incluindo no meio de UTF-8 e de `\r\n`), byte a byte,
  comentários, várias linhas `data:`, evento sem linha em branco no fim.
- `src/api/assemble.rs`: `input_json_delta` cortado no meio de escapes, pares substitutos e
  chaves, em vários pontos de corte do SSE; JSON inválido; input vazio; `event: error`;
  sanitização do fallback.
- `src/cli.rs`: o transcript real gravado → eventos (deltas crescentes, um `ToolUseEnd`,
  resultado, texto, uso com custo), modo sem mensagens parciais, erros/login, flags, config MCP,
  linha de entrada com anexo e resposta a `control_request`.
- `tests/cli_engine.rs`: o motor contra `pen-agent-fake-claude` (binário de teste do crate que
  reproduz o transcript): turno completo, segundo turno no mesmo processo, arquivos temporários
  (apagados no drop), `stop` mata e o próximo `send` usa `--resume`, processo que sai entre turnos,
  crash com stderr e código de saída, `claude` inexistente.
- `tests/api_engine.rs` (wiremock): laço completo com 529 + `retry-after` → `tool_use` em
  streaming → executor → `tool_result` com imagem → texto final. Confere o corpo de cada
  requisição (cabeçalhos, effort, thinking, fallback, `eager_input_streaming`, breakpoints de cache
  que andam, thinking com assinatura devolvido igual, prefetch do estado). Também: JSON inválido,
  violação de schema, 400 sem retry, limite de retries, `overloaded` no meio do stream,
  `max_tokens`, `refusal`, stop durante a ferramenta, steering e fila.
- `tests/real.rs` (pulam por padrão): conversa de dois turnos na API real conferindo
  `cache_read_input_tokens > 0` (precisa de `ANTHROPIC_API_KEY`; **não rodou aqui**, sem chave);
  TLS + 401 contra api.anthropic.com com chave falsa (`PEN_AGENT_NET_TESTS=1`, passou); o motor do
  CLI com o `claude` real e `openpen-cli mcp --headless` (`PEN_AGENT_REAL_CLAUDE=1`, passou).

## Lacunas

- Esforço "low até a primeira ferramenta" (legado) não foi feito: trocar o `effort` no meio
  invalida o cache das mensagens. O effort é fixo por sessão (padrão `low`).
- O motor da API não persiste o histórico. O `session_id` (`api-<hex>`) só identifica a sessão, e
  retomar exige guardar `messages`, o que fica para o app/persistência.
- Sem WebSearch/WebFetch no motor da API (ferramentas de servidor). Só o CLI tem.
- Sem UI de permissão (allow / auto / deny). O CLI roda com `dontAsk` e allowlist fixa.
- O CLI herda hooks e configurações do usuário (por exemplo, hooks `SessionStart`). Para isolar,
  passe `--setting-sources ""` em `CliConfig::extra_args`. O chat do app faz isso por padrão
  (`docs/chat.md`).
- O `steer` no CLI depende de como o Claude Code trata mensagens que chegam durante um turno (fila
  interna). Não foi testado contra o CLI real.
- O `display: "summarized"` do thinking e o cache real não foram verificados contra a API real
  (sem chave nesta máquina).
- O `stop()` do CLI mata o processo. O turno seguinte paga a partida do processo e do MCP
  (~1–2 s). Um `interrupt` pelo protocolo de controle seria mais leve.
