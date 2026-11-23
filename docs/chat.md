# Chat embutido

O painel **Chat** da barra lateral esquerda conversa com um agente que edita o documento da aba ao
vivo. O núcleo (motores, eventos, chave da API) é o `crates/pen-agent` (`docs/pen-agent.md`). O
`execute` em streaming é o de `docs/streaming-execute.md`. Este documento cobre a parte do app:
`apps/openpen/src/chat/`.

## Como usar

1. Abra ou crie um `.pen` e pressione **Ctrl+K**, ou clique na aba **Chat** da barra lateral. O
   composer recebe o foco.
2. Escreva o pedido. **Enter** envia e **Shift+Enter** quebra a linha. Os exemplos do estado vazio
   enviam com um clique.
3. O agente lê o estado do editor, carrega as skills e chama `execute`. O canvas se preenche
   **enquanto** ele escreve o código, com o cursor "OpenPen Chat" e o flash na cor do chat. Cada
   `execute` vira **um** passo de undo (Ctrl+Z).
4. Durante o turno:
   - o botão ■ (ou **Stop** na faixa de documento travado) interrompe tudo: `Session::stop` +
     `stream_cancel`;
   - mandar outra mensagem faz *steering*: ela vai para o turno em andamento (`Session::steer`).
5. **New** começa outra conversa. A anterior continua salva em disco.

## Motores e configurações

O botão no cabeçalho (`Claude Code · Haiku ⌄`) e o rótulo abaixo do composer abrem o popover de
configurações. As configurações são globais (todas as abas) e ficam em
`<data>/chat-settings.json`. `<data>` é `OPENPEN_DATA_DIR`, senão `%APPDATA%/OpenPen`.

| Opção | Claude Code (padrão) | Anthropic API |
|---|---|---|
| Modelo | `Default` (o do CLI), `Sonnet`, `Opus`, `Haiku` (aliases) | `claude-opus-5-5` (padrão), `claude-sonnet-5-5`, `claude-haiku-5-5` ou um id qualquer (campo "Custom model id", Enter aplica) |
| Effort | Low (padrão), Medium, High, XHigh, Max | idem |
| Extras | "Use my Claude Code settings" (desligado), "Allow web search and fetch" (ligado) | Chave da API |

- Mudar uma configuração recria a sessão na próxima mensagem. Se houver um turno em andamento, a
  troca acontece depois dele. Com o Claude Code, a conversa continua por `--resume`.
- **Isolamento**: por padrão, o Claude Code roda com `--setting-sources ""` (via
  `CliConfig::extra_args`; valor conferido no `claude --help` 2.1.330 e testado com o CLI real). Hooks,
  plugins e permissões do usuário ficam de fora, e só o `--mcp-config` do OpenPen vale
  (`--strict-mcp-config`). O toggle "Use my Claude Code settings" remove o argumento.
- **Chave da API**: o campo é mascarado. **Save** (ou Enter) grava no cofre do sistema
  (`pen_agent::key::set_stored_key`, Windows Credential Manager). Depois disso o painel mostra só o
  `mask_key` (`sk-ant-…wxyz`) e um botão **Clear**. A chave nunca vai para o JSON de configurações,
  para os logs ou para a conversa salva. Sem chave salva, vale `ANTHROPIC_API_KEY`. Uma nota lembra
  que o uso é cobrado por token na conta do Anthropic Console.
- **Privacidade**: o motor Claude Code executa o **seu** Claude Code instalado, com a sua conta e
  assinatura. O OpenPen não vê credenciais; só lança o processo e lê a saída stream-json.

## Arquitetura

| Arquivo | Papel |
|---|---|
| `chat/model.rs` | Redutor puro `AgentEvent` → itens do painel (`ChatModel::apply`) e efeitos (`Effect`). |
| `chat/settings.rs` | `ChatSettings` (serde, campos com padrão), rótulos e `resolve_openpen_cli`. |
| `chat/store.rs` | Persistência das conversas (JSON por conversa). |
| `chat/engine.rs` | `SessionConfig` a partir das configurações (`session_config`). |
| `chat/panel.rs` | View GPUI: cabeçalho, lista, faixa de documento travado, composer e popover. |
| `layers/panel.rs` | Cria o `ChatPanel` da aba (um por documento) quando a aba Chat aparece. |
| `agent/hub.rs` | `register_chat`: cor e rótulo próprios do chat. `external_agents` tira o chat da pílula de agentes. |
| `agent/stream.rs` | Guarda contra parciais atrasados (abaixo). |

### Fluxo de eventos

```
thread pen-agent ──AgentEvent──► EventReceiver ──► tarefa cx.spawn (foreground)
                                                   ├─ junta o que já chegou (try_recv) num lote
                                                   ├─ ChatModel::apply → itens + efeitos
                                                   │    StreamPartial → GuiBackend::stream_partial
                                                   │    CancelStream  → GuiBackend::stream_cancel
                                                   │    Persist       → grava o JSON (background)
                                                   └─ cx.notify()
```

Nada bloqueia a thread da UI:

- `Session` tem `Drop` que espera o worker por até 3 s, então a sessão é solta numa thread
  própria (`Live::drop`).
- O `get_app_state` do primeiro turno é um `await` sobre o canal do hub.
- Leitura e gravação das conversas rodam no executor de fundo.

### Redutor (`ChatModel`)

- Itens: `User`, `Text` (markdown), `Thinking`, `Tool(ToolCall)`, `TurnEnd { stop_reason, usage }`
  e `Error { kind, message }`. Deltas de texto e de raciocínio se juntam ao último bloco do mesmo
  tipo.
- `ToolCall` passa por `Streaming` → `Running` → `Done` | `Failed`. Ele vira `Cancelled` quando o
  turno acaba antes (stop, erro) ou quando a conversa é restaurada no meio de um turno.
- Para o `execute`:
  - o rótulo é o `description`, decodificado do JSON parcial por `partial_string_field`, então
    aparece enquanto o modelo ainda escreve;
  - o código é o `execute_input_prefix`;
  - os outros tools mostram o nome e o JSON de entrada.
- Com `stream_partials` (motor Claude Code), cada `ToolInputDelta` de `execute` cujo prefixo
  decodificado cresceu emite `Effect::StreamPartial`. Prefixos repetidos não são reenviados, e um
  delta que chega depois do `ToolUseEnd` é descartado.
- No `TurnEnd`, os `execute` ainda em `Streaming` emitem `CancelStream`, e o `Usage` pendente vai
  para o item `TurnEnd`.
- `classify_error` separa os banners: Claude Code ausente, não logado, sem chave da API, rede e
  outros.

### Ligação com o streaming do `execute`

- **Claude Code**:
  1. O `openpen-cli` é procurado ao lado do executável, depois na pasta `target` acima dele
     (`release` antes de `debug`), em `CARGO_TARGET_DIR` ou no `target` do workspace, e por fim no
     `PATH`.
  2. O socket é o do hub (`--socket` só quando não é o padrão). O nome do agente MCP é o da sessão
     (`OpenPen Chat#<marcador>`).
  3. Os parciais vão com `agent_id` = esse nome e o `uri` do documento da aba.
  4. O `execute` final chega pelo MCP vindo do `openpen-cli`, como `client-N` com
     `_meta.claudecode/toolUseId`. O coordenador casa pelo id e cria o alias. Isso foi verificado ponta
     a ponta (abaixo).
- **API**: `BackendExecutor::new(hub.backend, skills, CallCtx { client_id: "chat-<id>", agent_name:
  "OpenPen Chat" })`, com `with_partial_hook` → `stream_partial` (na thread do agente) e
  `with_abandon_hook` → `stream_cancel`. O redutor não emite parciais nesse motor.
- **Prompt**:
  - O sistema vem de `prompt::chat_system_prompt(SKILL.md)`, com as skills de
    `pen_mcp::find_skills_dir`.
  - A primeira mensagem de cada sessão leva o `get_app_state` do documento da aba em
    `UserMessage.context`, nos dois motores. O `prefetch_app_state` da API fica desligado para não
    buscar o documento focado em vez do da aba.
- **Canvas**:
  - `AgentHub::register_chat` dá ao chat uma cor própria e o rótulo "OpenPen Chat" para o cursor e
    os flashes do `fx`.
  - A conexão MCP do processo do Claude Code é reconhecida pelo nome e herda cor e rótulo. Ela fica
    marcada como `internal`, fora da pílula de agentes e do diálogo de conexão.
- **Parciais atrasados**:
  - Os parciais passam pela thread da UI. O `execute` final chega por outro caminho (MCP), então um
    parcial pode chegar **depois** do final.
  - O `StreamCoordinator` guarda os últimos 256 `tool_use_id` encerrados (finalizados, executados
    sem stream, cancelados ou expirados) e ignora parciais deles.
  - Sem essa guarda, um parcial tardio abriria um stream novo, que travaria o documento e duplicaria
    nós até o timeout.

### Documento travado

Enquanto um stream segura o documento (`SceneDocument::stream_lock`), uma faixa acima do composer
diz "The agent is writing to this document. Your edits wait until it finishes.", com **Stop**. Se o
stream for de outro agente, o Stop chama `stream_cancel` para ele. O cabeçalho mostra um ponto azul
pulsante enquanto o turno roda.

### Persistência

- `<data>/chats/<doc key>-<id>.json`, um arquivo por conversa:
  - `doc key`: FNV-1a de 64 bits do caminho canônico, sem diferenciar maiúsculas no Windows;
  - `id`: `<ms de 13 dígitos>-<hex>`, ordenável pela criação.
- O JSON guarda `version`, `doc`, `title`, `engine`, `model`, `session_id` (o do Claude Code, para
  `--resume`), as datas e os itens.
- Ao abrir o painel, o app lista a pasta pelo prefixo da chave (sem ler os outros arquivos) e
  reabre a conversa mais nova. Arquivos corrompidos são ignorados, e a gravação é atômica.
- A gravação acontece:
  - ao enviar;
  - ao receber o `session_id`;
  - num erro;
  - no fim de cada turno.
- Documentos sem caminho (não salvos) não são persistidos até ganharem um caminho.
- O `session_id` só é usado se a conversa foi do Claude Code. O motor da API não guarda o histórico
  do servidor, então uma conversa restaurada recomeça o contexto nesse motor.

## Visual (referência: o chat do Pen)

Os valores foram lidos no bundle e no CSS do legado, só como referência visual:

- **Cabeçalho**: título 13 px medium `foreground/80` ("New chat" até a primeira mensagem); ponto azul
  pulsante com a dica "Design generation in progress…"; botão outline **New**.
- **Mensagem do usuário**: bolha à direita, largura máxima de 85 %, raio 18, 13 px / 20, fundo
  `zinc-100` (escuro: `zinc-700/50`).
- **Resposta**: markdown sem bolha (13 px, `foreground/85`, `TextView::markdown` do gpui-component,
  com fade de streaming), blocos de código em `zinc-100` / `zinc-800` com raio 6.
- **Thinking**: linha recolhida "Thinking…" (enquanto chega) ou "Thought", com chevron. Abre com um
  clique.
- **Linha de tool**: ícone 14 px `foreground/50`, rótulo (descrição do `execute` ou nome do tool),
  spinner enquanto roda, check verde no sucesso, X vermelho no erro, "stopped" quando cancelada.
  Expandida, mostra um cartão com borda e as seções RESULT e SCRIPT/INPUT (mono 11 px, rolagem).
- **Fim do turno**: uso discreto (`83.3k in · 1.5k out · $0.0052`, 11 px `foreground/40`; a dica
  mostra cache read/write) e "Agent stopped by user." quando interrompido. O motor da API não
  informa custo, só tokens.
- **Banners de erro**: título amarelo "Oops, something went wrong with Claude Code!" (ou "…with the
  Anthropic API!"), texto explicativo e ações:
  - **Use the Anthropic API**, quando o Claude Code não está instalado;
  - **Open settings**, quando falta a chave;
  - **Try again** nos outros casos.
- **Composer**: cartão com raio 14 e textarea que cresce (1 a 10 linhas). Abaixo, o rótulo
  motor · modelo · effort, o botão de stop (círculo `zinc-500/80` com quadrado) e o de envio
  (círculo `zinc-600`, ou `zinc-200` desabilitado, com seta).

## Testes

- `chat/model.rs`:
  - junção de texto e raciocínio;
  - rótulo, código e parciais do `execute` (sem duplicatas, nada depois do `ToolUseEnd`);
  - motor da API sem parciais;
  - outros tools;
  - turno interrompido (cancelamentos, uso);
  - classificação de erros;
  - `session_id`;
  - conversa restaurada;
  - `partial_string_field`;
  - formatação de tokens e custo;
  - ida e volta dos itens em JSON.
- `chat/settings.rs`: ida e volta em disco (sem chave no arquivo), arquivo parcial ou quebrado,
  escolhas de modelo e a ordem de busca do `openpen-cli`.
- `chat/store.rs`: ida e volta sem perda, a mais nova por documento, arquivo quebrado ignorado,
  regravação sem duplicar, chaves e ids.
- `chat/engine.rs`: config do Claude Code (socket, `--setting-sources`, resume, erros sem
  `openpen-cli` ou sem servidor) e da API (sem resume, sem prefetch).
- `agent/stream.rs`: parciais atrasados depois do final, de um stream cancelado e de um tool novo.
- `tests/chat_engine.rs`:
  1. O transcript real gravado (`claude-cli-execute.jsonl`) passa pelo `CliMapper` e pelo redutor:
     prefixos crescentes, uma linha de tool `Done` com a descrição, texto, uso com custo e
     `session_id`.
  2. O mesmo turno pelo motor real do CLI (config do `session_config`) contra o
     `pen-agent-fake-claude`. É pulado se o binário não foi compilado; com `cargo test --workspace`
     ele existe.
  3. O motor da API montado pelo `session_config` contra um mock da Messages API (wiremock), com o
     `GuiBackend` atendido por um despachante falso. Os parciais chegam pelo hook com o agente, o
     `tool_use_id` e o `uri` da aba e só crescem. O `execute` final traz o mesmo `tool_use_id`. Não
     há prefetch, e a linha de tool, o texto e o uso (sem custo) aparecem no painel.

## Verificação real (23/11/2026)

- Ambiente:
  - `target/release/openpen.exe` copiado para `target/chat-demo/openpen-chat-demo.exe`, com o
    `openpen-cli.exe` ao lado;
  - `OPENPEN_DATA_DIR=target/chat-demo/data`, socket próprio;
  - documento vazio `target/chat-demo/demo.pen`.
- Motor Claude Code 2.1.330, modelo Haiku, effort Low. O script de automação (Python, pyautogui +
  PIL) ficou no scratchpad da sessão, fora do repositório.
- Prompts e custo:

  | Prompt | Resultado | Custo |
  |---|---|---|
  | "Design a small profile card: avatar circle, name, role, a short bio and a Follow button." | Cartão criado e salvo pelo agente | $0,0052 |
  | Linha de estatísticas dentro do cartão | Inserida ao vivo | $0,0064 |
  | Depois de fechar e reabrir o app: "what text did you put on the button earlier?" | "The Follow button says "Follow"." (a conversa continuou por `--resume`) | $0,0067 |

  Total: **≈ $0,018**, mais $0,0016 de um teste avulso do `--setting-sources ""`.
- O log do app mostra `stream … opened` → `execute … (finalized stream): ok` → `finalized after N
  evaluation(s): committed`. O parcial do chat e o `execute` final que chegou pelo MCP casaram pelo
  `tool_use_id`. Com o Haiku, os scripts eram curtos e chegaram em poucos pedaços (0 a 1 avaliação
  antes do final).
- **Ctrl+Z** depois dos turnos: o primeiro desfaz a linha de estatísticas inteira, e o segundo o
  cartão inteiro (um passo por `execute`).
- Screenshots (pasta `docs/screenshots/`, fora do git):
  - `chat-empty.png`: estado vazio;
  - `chat-settings.png`: popover de configurações;
  - `chat-streaming.png`: linha de tool com spinner e faixa de documento travado;
  - `chat-live-canvas.png`: o canvas recebendo a linha de estatísticas, com flash e cursor
    "OpenPen Chat". É uma cópia reduzida (427×277): o quadro em tamanho cheio foi apagado por
    engano na limpeza;
  - `chat-final.png`: estado final, com uso por turno;
  - `chat-undo.png`: depois do Ctrl+Z;
  - `chat-restored.png`: conversa reaberta depois de reiniciar o app;
  - `chat-resume.png`: resposta com o contexto retomado.

## Lacunas

- **O motor da API não foi testado de verdade** (não há chave nesta máquina). Só há o teste com o
  mock.
- O motor da API não persiste o histórico (`messages`): uma conversa restaurada recomeça o contexto
  nesse motor.
- Sem anexos (imagens/arquivos), menções `@`, skills `/`, histórico de prompts com ↑/↓, fila de
  prompts editável e feedback 👍/👎 do Pen.
- A aba **Chat history** só explica onde as conversas ficam. Ainda não há lista para reabrir
  conversas antigas (elas continuam nos arquivos).
- Sem UI de permissões: o CLI roda com `dontAsk` e a allowlist fixa do `pen-agent`.
- Documento sem caminho: o `uri` dos parciais é `None` (o documento focado). Trocar de aba durante
  um turno pode mandar o stream para outra aba. O `execute` final também usa o focado.
- O título do cabeçalho é a primeira linha da primeira mensagem (o Pen gera um título).
- O texto do assistente é re-renderizado como markdown a cada delta. Respostas muito longas
  custariam mais, mas as do chat são curtas pela regra `<final_message>`.
- Durante o streaming, a lista inteira é reconstruída a cada lote de eventos. Não há virtualização
  de mensagens.
