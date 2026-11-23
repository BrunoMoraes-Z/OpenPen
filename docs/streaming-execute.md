# Streaming do `execute`

As edições de um `execute` aparecem no canvas **enquanto o agente ainda escreve o código**, e a
chamada continua atômica: um passo de undo no fim, ou nada em caso de erro. É a versão do OpenPen
do desenho do legado (`docs/legacy-analysis/04-mcp-headless-agentes.md` §1.6: *rechunker* que só
libera instruções completas, avaliação no mesmo contexto numa fila de concorrência 1, e a chamada
final esperando a fila para fazer commit ou rollback).

Peças:

| Onde | O quê |
|---|---|
| `crates/pen-script` | `ScriptEngine::execute_partial` / `cancel_stream` / `discard_stream` e o divisor `complete_prefix` (ver `docs/pen-script.md`). |
| `apps/openpen/src/agent/stream.rs` | `StreamCoordinator`: estado dos streams, throttling, finalização, regras de segurança. Sem GPUI (testado sem janela). |
| `apps/openpen/src/agent/hub.rs` | A tarefa `dispatch` (thread principal) que dirige o coordenador e implementa o `StreamHost` do app. |
| `apps/openpen/src/scene_document.rs` | `stream_partial`, `stream_cancel`, a trava do documento e o `run_script` que finaliza. |
| `crates/pen-mcp` | `CallCtx::tool_use_id` (do `_meta` `claudecode/toolUseId`) e `ToolBackend::call_extension`. |

## API (lado do app)

```rust
// Qualquer thread: o chat embutido pega o backend no global `AgentHub` (`hub.backend.clone()`).
let backend: GuiBackend = cx.global::<AgentHub>().backend.clone();
backend.stream_partial(agent_id, tool_use_id, doc_uri /* Option<String> */, input_prefix);
backend.stream_cancel(agent_id, Some(tool_use_id));
// A chamada final é um execute comum, com o tool_use_id no CallCtx:
backend.execute(CallCtx { client_id: agent_id, agent_name, tool_use_id: Some(id) }, args);
```

- `input_prefix` é o maior prefixo **decodificado** do código JS (o valor do campo `input`), não o
  JSON parcial. O `pen-agent` tem o decodificador (`partial::execute_input_prefix`).
- As chamadas não bloqueiam: viram mensagens (`AgentRequest::StreamPartial` / `StreamCancel`) no
  mesmo canal das ferramentas MCP, atendido pela tarefa `dispatch` na thread da UI. Mandar a cada
  delta do modelo é aceitável: o coordenador guarda só o último prefixo.
- `doc_uri` escolhe o documento como o `uri` das ferramentas (`None` = focado). Um arquivo `.pen`
  fechado é aberto numa aba nova, e o stream espera ele carregar.
- O dispatcher roda sempre, mesmo sem o socket MCP (outra instância com o socket): agentes no
  mesmo processo não dependem dele.

## Fluxo

1. **Parcial**: `StreamCoordinator::partial` guarda o prefixo mais recente do stream
   `(agente, tool_use_id)`. Não avalia nada.
2. **Pump** (depois de cada mensagem e quando o timer acorda): para cada stream que segura o
   documento, se `complete_prefix(prefixo)` cresceu desde a última avaliação e o stream está
   "vencido", chama `SceneDocument::stream_partial` → `ScriptEngine::execute_partial`. O app
   atualiza layers, inspector e canvas (`EditorState::changed`) e faz o flash e o cursor do agente
   nos nós tocados **desde o parcial anterior** (`PartialStatus::touched`).
3. **Final**: o `execute` com o código completo chega (do chat, no mesmo processo, ou pelo MCP vindo
   do `openpen-cli mcp`). O coordenador procura o stream **pelo tool_use_id**, senão pelo agente.
   - Se o stream está ativo, no mesmo documento, a chamada não é `editId` e o código final começa
     com a parte já avaliada: **finaliza**. O motor continua no mesmo contexto, avalia o resto, faz
     screenshots/exports e grava **um** passo de undo com tudo. A resposta é a mesma de um
     `execute` direto (texto, ids criados, `Print`, avisos). Os flashes do fim cobrem só o que não
     piscou ao vivo.
   - Caso contrário (o modelo mudou o código, `editId`, outro documento, stream que falhou ou
     ainda esperando): **cancela** o stream (rollback) e roda o `execute` normalmente.
   - Em erro, tudo é revertido. Se a falha vier de um stream continuado, o motor ainda **refaz a
     chamada do zero uma vez**: um corte numa fronteira falsa não derruba um snippet válido, e um
     erro real sai idêntico ao de uma chamada direta.
4. Quando um cliente MCP finaliza o stream de outro id (Claude Code CLI: parciais do chat com o id
   do chat, final pelo MCP com `client-N`), o coordenador cria um **alias** `client-N → id do
   chat`. As chamadas seguintes desse cliente usam as mesmas globais persistidas do stream.

## Throttling

- No máximo **uma avaliação por stream a cada 16 ms** (um quadro). Prefixos que chegam no meio
  ficam coalescidos: só o último é avaliado.
- O intervalo é adaptativo: `max(16 ms, 3 × custo da última avaliação)`, limitado a 250 ms. Um
  pedaço lento faz o próximo esperar mais, e a thread da UI fica livre em ≥ 75% do tempo.
- Um prefixo cujo trecho completo não mudou (só cresceu um token incompleto, uma string aberta,
  um `if` que ainda pode ganhar `else`) **não** é reavaliado.
- O timer da tarefa `dispatch` (`next_wake`) acorda só quando há avaliação pendente, retry ou
  timeout. Sem streams, nada roda.

## Regras de segurança (escolhidas)

| Situação | Comportamento |
|---|---|
| Edição do usuário no documento com stream aberto | **Recusada**: `SceneDocument::apply_op` devolve `false` enquanto a trava existe (`stream_lock`). O agente vence, e o usuário espera o commit (ou o stop/timeout). |
| Gesto do usuário (arraste) já aberto quando chega um parcial | O **stream espera**: `ready()` é falso enquanto o bloco de undo do gesto está aberto, e o parcial tenta de novo a cada 20 ms. |
| Undo / Redo | Recusados enquanto a trava existe: os passos nunca se intercalam com as inversas pendentes do stream. |
| Save do usuário (Ctrl+S, Save ao fechar) | **Espera** o fim do stream (notificação "An AI agent is writing…") e então grava. Nunca grava estado meio aplicado. Não há autosave no app. |
| `save` pelo MCP | Erro explicativo enquanto o documento está travado (a fila do dispatcher não pode esperar a si mesma). |
| `execute` de outro agente no mesmo documento | **Estacionado** até o stream terminar e então executado, em ordem. Desiste após 120 s. |
| Parciais de um segundo agente no mesmo documento | **Serializados**: o stream dele fica `Waiting` (sem avaliar) e começa, alcançando o último prefixo, quando o primeiro termina. |
| 60 s sem prefixo novo | Timeout: rollback e o stream some. Um final que chegue depois roda como `execute` normal. |
| Aba fechada | `Workspace::close` / `remove_tab` mandam `DocClosed`: rollback (ou descarte, se o documento já sumiu). O final roda normalmente onde o `uri` apontar. |
| Agente desconecta | Os streams com o id dele são revertidos e as globais esquecidas. |
| Erro num pedaço | Rollback **imediato** (o canvas volta), o stream vira `Failed` e não avalia mais nada. O final roda o snippet inteiro do zero e reporta o erro como um `execute` direto. |
| Novo `tool_use_id` do mesmo agente | O stream anterior é cancelado. |

A trava é do `SceneDocument` (não do coordenador), então vale para qualquer caminho de edição da
UI. O `run_script` de outro agente num documento travado também é recusado como defesa extra.

## Casando a chamada final

- **Motor direto (chat no processo)**: o chat usa o mesmo `client_id` nos parciais e no
  `execute`, e põe o `tool_use_id` no `CallCtx`.
- **Claude Code CLI**: a chamada final chega pelo MCP. O Claude Code manda
  `_meta: {"claudecode/toolUseId": "toolu_…"}` em cada `tools/call`. O rmcp move o `_meta` dos
  params para o `RequestContext::meta`, e o `pen-mcp` lê os dois e preenche
  `CallCtx::tool_use_id`. O `GuiBackend::execute` repassa, e o coordenador casa pelo id (senão
  pelo agente).

## Hook de depuração (para a demo)

Com `OPENPEN_DEBUG_STREAM=1` no ambiente do app, o `GuiBackend` aceita a ferramenta MCP oculta
`debug_stream_partial` (não aparece no `tools/list`; chega por `ToolBackend::call_extension`):

```json
{"name": "debug_stream_partial",
 "arguments": {"toolUseId": "toolu_x", "input": "<prefixo JS>", "agentId": "chat-demo", "uri": "…", "cancel": false}}
```

Ela faz o mesmo que o `stream_partial` do chat. Sem a variável, a ferramenta não existe.

## Medidas

- **Corpus** (`t-process-recorder.pen`, 2995 nós, cópia em `target/stream-bench/`; teste
  `corpus_partial_cost`, release): 8 avaliações de um snippet de tela de login, mediana 0,12 ms,
  pior 1,0 ms, média 0,24 ms por parcial. A finalização custou 0,37 ms. O undo devolve o arquivo
  idêntico byte a byte.
- **App real** (release, mesmo arquivo, demo abaixo, custo medido em volta do `StreamHost::partial`,
  incluindo o refresh de layers, inspector e canvas e os flashes): 10 avaliações em 6,2 ms no total,
  pior 1,4 ms. O `execute` final (finalização) levou 0,43 ms. No documento vazio: 4,9 ms no total,
  pior 1,3 ms.
- Ou seja, um parcial custa menos de 1/10 de um quadro. O intervalo adaptativo só entra em ação
  em documentos ou snippets muito mais pesados.

## Demo no app real

Script `stream_demo.py` (no scratchpad da sessão, não versionado): copia o
`target/release/openpen.exe` para `target/stream-demo/openpen-stream-demo.exe`, abre a cópia de
um arquivo do corpus com `OPENPEN_DATA_DIR=target/stream-demo/data` (os recentes do usuário ficam
limpos), `OPENPEN_DEBUG_STREAM=1` e um socket próprio, conecta pelo `openpen-cli mcp` e manda 70
prefixos crescentes de um dashboard em ~5 s pelo `debug_stream_partial` (agente `chat-demo`).
Depois manda o `execute` final com `_meta.claudecode/toolUseId` como cliente "Claude Code", e um
Ctrl+Z remove tudo de uma vez (o `Get(board)` seguinte, com a global compartilhada pelo alias,
diz "node not found").

- `docs/screenshots/stream-1.png` … `stream-4.png`: quadros no meio do stream (título; cards
  piscando com o cursor `chat-demo`; gráfico sendo preenchido). A aba **não** fica suja durante o
  stream.
- `docs/screenshots/stream-final.png`: depois do `execute` final (aba suja, um passo de undo).

## Testes

- `agent/stream.rs` (unitários, sem janela, `LocalDocs` como host): coalescência por quadro e
  prefixo inalterado sem reavaliação; intervalo adaptativo com pedaço lento; finalização = 1 passo
  de undo com documento e texto de resposta **idênticos** a um `execute` direto (ids com semente);
  final que não continua o prefixo e `editId` cancelam e rodam normalmente; timeout com rollback;
  erro no meio → rollback imediato e o final reporta o mesmo erro de uma chamada direta; trava
  recusando edição, undo e redo; gesto do usuário fazendo o stream esperar; segundo agente
  serializado e terceiro estacionado; final em outro documento e documento fechado.
- `apps/openpen/tests/agent_stream.rs`: um snippet realista (loop com templates, `if/else`)
  mandado em prefixos de 1, 7 e 61 bytes mais cortes no meio de string, dentro de `${…}`, dentro
  de template, depois de `if {…}` sem `else` ainda e em `e`/`els`; mesmo documento, resposta e
  flashes que a chamada direta; nenhum corte avalia meia instrução; erro na última instrução
  reverte tudo; custo no corpus.
- `crates/pen-mcp/tests/session.rs`: o `_meta` `claudecode/toolUseId` de um `tools/call` chega no
  `CallCtx` (e some na chamada seguinte sem `_meta`).
- `agent/backend.rs`: parciais, cancelamento e a ferramenta de depuração (só quando habilitada)
  passam pelo canal.

## Lacunas

- A UI não mostra que o documento está travado (as edições só não acontecem). O chat embutido
  deve mostrar o estado e oferecer "stop" (`stream_cancel`).
- O divisor é heurístico: uma fronteira falsa faz o stream falhar (e voltar ao início no canvas),
  mas o final refaz do zero e o resultado é o mesmo.
- Screenshots/exports do snippet só acontecem na finalização.
- Agentes MCP externos sem o chat embutido não fazem streaming: o MCP não carrega o JSON parcial
  da chamada, só a final.
- `execute` estacionado espera em fila dentro do dispatcher. O limite de 120 s pode estourar o
  timeout do cliente MCP antes.
