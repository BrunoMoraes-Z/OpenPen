# Integração com agentes no app (MCP ao vivo)

Fase 5 de `docs/legacy-analysis/00-visao-geral.md`: um agente externo (Claude Code, Codex, …)
conecta no OpenPen **em execução**, edita o documento aberto pelo `execute` e o usuário vê a
edição acontecer no canvas, como um passo de undo. O protocolo fica em `crates/pen-mcp`
(`docs/pen-mcp.md`) e o motor do `execute` em `crates/pen-script` (`docs/pen-script.md`). Este
documento cobre a parte do app (`apps/openpen/src/agent/`).

## Como experimentar

1. Compile o app e o shim: `cargo build --release -p openpen -p openpen-cli`.
2. Abra o OpenPen (`target/release/openpen.exe arquivo.pen`). Na inicialização ele abre o socket
   `\\.\pipe\openpen-desktop` (Windows) ou `~/.openpen/sock/openpen-desktop.sock`. O log
   (`%TEMP%/openpen.log`) mostra `MCP: listening on …` e de onde as skills vieram.
3. Clique no ícone de robô na ilha direita da barra de abas (ou ☰ › File › **Connect an AI
   agent…**). O diálogo mostra o JSON exato para o Claude Code, com o caminho do
   `openpen-cli` compilado ao lado do app, e o comando equivalente:

   ```
   claude mcp add --scope user openpen -- "C:\…\target\release\openpen-cli.exe" mcp
   ```

   Os dois têm botão **Copy**. O OpenPen **não escreve** nenhum arquivo de configuração de
   outro app: a instalação é sempre manual (opt-in).
4. No Claude Code, peça uma tela ("crie uma tela de login no OpenPen"). O agente chama
   `get_app_state`, `start_task`/`read_skill` e `execute`. A tela aparece no canvas com o flash
   e o cursor do agente, o painel de camadas e o inspector se atualizam, a aba fica com o ponto
   de "não salvo" e **Ctrl+Z** desfaz a chamada inteira de uma vez.

Para rodar uma segunda instância (ou testes) sem disputar o socket padrão, defina
`OPENPEN_MCP_SOCKET=<nome>` ao abrir o app e use `openpen-cli mcp --socket <nome>`. O diálogo
já inclui o `--socket` quando o nome não é o padrão. Se o socket já estiver em uso (outro
OpenPen aberto), o app registra o erro no log e segue sem servidor. O diálogo então explica que o
servidor não está rodando.

## Arquitetura

```
 tokio (threads do BackgroundSocketServer)          thread principal da GPUI
 ───────────────────────────────────────           ─────────────────────────────────────────
 sessão rmcp ─► GuiBackend::execute ──mpsc──►  tarefa `dispatch` (cx.spawn, uma por app)
                 (aguarda oneshot)                 ├─ route(uri) → aba / abrir / erro
                                                   ├─ espera carregar e o fim de um arraste
                                                   ├─ EditorState::run_agent_script
                                                   │    └─ SceneDocument::run_script
                                                   │         └─ ScriptEngine::execute (1 passo)
                                                   ├─ changed() → layers, inspector, canvas
                                                   └─ AgentFx::touch → flash + cursor
                 ◄──────── oneshot ToolResult ─────┘
```

- **`agent/backend.rs`**: `GuiBackend` implementa `pen_mcp::ToolBackend`. Cada chamada vira um
  `AgentRequest` num `futures::channel::mpsc` sem limite e espera a resposta num `oneshot`. Se o
  app fechou (canal ou resposta descartados), o agente recebe `Unavailable` (JSON-RPC `-32603`).
  `agent_connected`/`agent_disconnected` também passam pelo canal.
- **`agent/hub.rs`**: o `AgentHub` (global da GPUI) guarda o servidor, a lista de agentes
  conectados (nome e cor, atribuída na ordem de conexão) e o nome do socket. A tarefa `dispatch`
  é dona do `ScriptEngine` e atende as chamadas **uma de cada vez, em ordem**, então dois agentes
  nunca editam ao mesmo tempo. O id do agente no motor é o `client_id` da sessão: as globais
  persistidas e os `editId` são por conexão e são descartados na desconexão
  (`forget_agent`). O servidor para no `on_app_quit`.
- **Roteamento** (`agent/route.rs`, função pura): sem `uri` vai para o documento focado (ou o
  último que esteve ativo, se a Home estiver aberta); com `uri` de uma aba aberta (caminho ou
  `file://`, comparado em forma canônica) vai para essa aba; com `uri` de um `.pen` existente
  que não está aberto, abre numa aba nova e espera carregar; nos outros casos, ou sem nenhum
  documento, devolve `NoDocument` (`isError: true`, com texto explicativo). O `get_app_state`
  sem documento responde normalmente, dizendo que nada está aberto.
- **Pronto para editar**: a chamada espera (até 30 s) o documento terminar de carregar e o usuário
  soltar um arraste em andamento. Assim o passo do agente nunca entra no bloco de undo de um
  gesto.
- **`execute`**: `SceneDocument::run_script` chama `ScriptEngine::execute` com a cena, o
  pipeline e o `UndoStack` do documento. No sucesso com operações, registra um passo de undo
  igual ao dos gestos (o estado "não salvo" usa os mesmos ids de passo), e depois roda o
  `pipe.update` incremental. Em erro, o motor reverte tudo e o documento fica igual. Em seguida,
  `EditorState::changed` faz o mesmo que uma edição manual: remapeia a seleção, atualiza as
  linhas do painel de camadas e o inspector, e notifica o canvas, que troca a `DisplayList`.
- **`get_app_state`**: `pen_script::describe_app_state` com o caminho da aba, a seleção, a área
  visível do canvas e os nomes dos agentes conectados. Quando há mais de uma aba, o app
  acrescenta a seção "Open documents" (nome, caminho e qual está focado).
- **`save`**: usa o mesmo caminho do Ctrl+S (`Workspace::write_doc`): serializa na thread da
  UI, grava de forma atômica no executor de fundo, marca como salvo no passo serializado e
  adiciona aos recentes. Um documento que nunca foi salvo dá erro pedindo para o usuário usar
  Save As, porque o agente não abre diálogos.
- **Screenshots/Export**: `AppScriptHost` usa as implementações padrão do `ScriptHost`, que
  renderizam na CPU com pen-raster a mesma `DisplayList` do canvas, sem o overlay.

## Feedback ao vivo no canvas

Comportamento portado do legado (`04-mcp-headless-agentes.md` §3.4), com implementação
própria em `agent/fx.rs` (funções puras do tempo, testadas sem janela) e `agent/paint.rs`
(desenho no overlay do `CanvasHost`).

- **Flash**: anel na cor do agente sobre o retângulo do nó, com envelope de ataque de 200 ms,
  hold de 400 ms (1500 ms em leituras) e release de 300 ms, com rampas suaves. O anel "assenta"
  no nó durante o ataque (encolhe de +8 px para +2 px) e acompanha o raio dos cantos. Uma
  linha de varredura com rastro em degradê desce pelo nó a uma velocidade aparente constante de
  900 px/s, com período entre 0,25 e 1,2 s por passada. São 3 passadas (4,5 nas leituras),
  limitadas à duração do flash. O nó é procurado de novo pelo id a cada quadro, então o anel
  acompanha mudanças de layout.
- **Quais nós**: os `touched` da resposta. Cada nó entra uma vez (escrita vence leitura). Um nó
  cujo ancestral também foi tocado com o mesmo tipo de acesso é omitido: uma tela nova pisca
  uma vez, não uma vez por filho. O limite é de 24 flashes por chamada.
- **Cursor**: seta com contorno branco e sombra, mais um rótulo em pílula com o nome do agente
  na cor dele. Ele nasce no centro da área visível e aparece com fade-in de 200 ms. Visita os nós
  em sequência, com 250 ms entre um e outro (o intervalo encolhe para que a sequência toda caiba
  em 2,5 s). O alvo é sorteado com distribuição triangular dentro do nó. O movimento é uma mola
  exponencial com solução analítica (posição = função do tempo, sem integrar por quadro). O
  flash de um nó espera 300 ms se o cursor teve que viajar até ele. Parado, o cursor oscila
  levemente; some com fade-out de 1,5 s depois de 5 s ocioso e é removido quando o agente
  desconecta.
- **Sem CPU ocioso**: o overlay só chama `window.request_animation_frame()` enquanto
  `AgentFx::animating(now)` for verdadeiro. Depois do último fade, nenhum quadro extra é pedido.
- **Cores**: violeta, laranja, verde, rosa, azul e amarelo, nessa ordem de conexão.

Diferença consciente do legado: a taxa da mola é 8/s (o legado cita 2,5/s com passo máximo de
0,1 s). Como aqui todos os nós de uma chamada chegam juntos no fim do `execute` (sem streaming),
2,5/s deixava o cursor sempre atrás do flash.

## UI

- **Indicador de agentes** (ilha direita da barra de abas, antes de Settings): sem agentes, é
  um ícone de robô com a dica "Connect an AI agent…". Com agentes, mostra bolinhas sobrepostas
  nas cores deles e o nome do primeiro ("Claude Code +1" quando há mais de um). A dica lista
  todos os nomes. O clique abre o diálogo.
- **Diálogo "Connect an AI agent"**: o JSON de `pen_mcp::config::claude_code_config_string`
  apontando para o `openpen-cli` ao lado do executável (com aviso se o arquivo não existir), o
  comando `claude mcp add`, o estado do servidor (socket ou motivo de não estar rodando) e os
  agentes conectados.
- **Ação** `ConnectAgent`, no menu ☰ › File e no menu nativo do macOS.

## Testes

- `apps/openpen/src/agent/fx.rs`: envelope, varredura (velocidade, passadas, leituras), mola
  (monotônica e composável), opacidade do cursor, RNG triangular, cursor seguindo waypoints,
  sequência de flashes com atraso de viagem, limites e `animating` desligando.
- `apps/openpen/src/agent/route.rs`: sem `uri` (focado, primeira aba, nenhum documento), `uri`
  de aba aberta (caminho e `file://`), arquivo fechado (abre), URIs inválidas, arquivo
  inexistente e extensão errada.
- `apps/openpen/src/agent/backend.rs`: ida e volta pelo canal e `Unavailable` quando a resposta
  ou o receptor somem.
- `apps/openpen/tests/agent_execute.rs` (sem janela): o `execute` cria uma tela com filhos,
  marca o documento como sujo, atualiza layout e `DisplayList`, e **um** undo volta ao texto
  salvo idêntico (o redo também funciona). Um script com erro não muda nada (nem sujo, nem
  passo de undo). Os `touched` geram um flash por subárvore, e as leituras também. No
  `t-process-recorder.pen` (com skip se o corpus não existir), o undo devolve os bytes
  idênticos.
- **Ponta a ponta (manual, Windows)**: um script Python lança `target/release/openpen.exe` com
  uma cópia de `testdata/user/*.pen` em `target/agent-e2e/` e `OPENPEN_MCP_SOCKET` único. Ele
  conecta pelo `openpen-cli mcp --agent "Claude Code"` falando JSON-RPC, roda
  `get_app_state` e um `execute` que cria uma tela de login com `FindEmptySpace`, e captura a
  janela (`PIL.ImageGrab`) durante a animação, depois dela, depois do fade e com o diálogo aberto.
  Por fim, manda Ctrl+Z e confirma por outro `execute` que o nó sumiu. As capturas ficam em
  `docs/screenshots/agent-*.png` (`agent-small-*` no `framegrab.pen`, a 50%). O `execute`
  levou cerca de 3 ms no documento de 2995 nós.

O harness de teste da GPUI (`gpui-kit/test-support`) não foi usado: o `Workspace` precisa de
janela, tema e assets inicializados, e a feature recompilaria toda a pilha da GPUI nos testes.
Por isso a lógica foi separada em partes puras (roteamento, canal, `run_script`, efeitos),
testadas sem janela. A cola com a GPUI é coberta pelo teste ponta a ponta.

## Lacunas

- Sem streaming (`execute_partial`): a edição aparece de uma vez no fim da chamada, e o cursor
  visita os nós depois. O motor já suporta streaming; falta o `pen-mcp` repassar o snippet
  parcial.
- O `execute` roda síncrono na thread da UI (limite de 55 s do motor). Um script pesado
  congela a janela enquanto roda.
- Sem placeholders/"generating", sem animação de mudança de layout dos pais e sem perfis de
  subagentes (`spawn_agents`).
- Agentes não conseguem salvar um documento sem caminho (Save As exige o usuário).
- O app não leva a vista até o que o agente editou fora da área visível; o painel de camadas
  mostra o nó novo.
- Achado durante o teste ponta a ponta (fora do escopo, não corrigido): um clique sintético com
  mouse-down e mouse-up no mesmo quadro deixa um marquee "preso" no canvas. A captura de
  arraste só existe depois do próximo render, e o mouse-up se perde.
