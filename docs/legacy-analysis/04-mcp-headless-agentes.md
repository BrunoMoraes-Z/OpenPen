# 04 — Superfície de IA/automação: MCP, headless e agentes (Pen v1.2.23)

> Análise estática, somente leitura, do app Electron "Pen" (pen.dev / High Agency) v1.2.23.
> Fontes: `app.asar` extraído (`out/*.js`, `node_modules/@ha/{mcp,headless,agent,shared,ipc}`, bundle do editor `out/editor/assets/index.js`), `app.asar.unpacked/out/mcp-server-windows-x64.exe` (apenas `strings`, nada executado) e `app.asar.unpacked/out/skills/pen-dev/**`.
> Nenhum segredo/chave foi encontrado ou registrado. Trechos de código de terceiros foram resumidos, não copiados.

---

## 0. Visão geral (arquitetura em uma figura)

```
 Clientes MCP externos                         Agente de chat embutido (processo main do Electron)
 (Claude Code, Codex CLI, Gemini CLI,          @ha/agent: ClaudeAgent | CodexAgent | CursorAgent | PiAgent
  OpenCode, Kiro, Antigravity, Claude Desktop)       |                    |
        | stdio (JSON-RPC MCP)                       | in-process         | (Codex usa o binário stdio)
        v                                            | (createSdkMcpServer|
 mcp-server-<os>-<arch>(.exe)  [Go, mcp-go]          |  / customTools)    |
        | named pipe \\.\pipe\pencil-<app>           |                    |
        | (unix: ~/.pencil/socket/pencil-<app>.sock) |                    |
        v                                            v                    |
 MCPTransport (@ha/mcp, node-ipc server) ---> IPCDeviceManager (@ha/ipc) <-+
                                                   | ipc.request("<tool-name-com-hífen>", params)
                                                   v
                                    Renderer do editor (out/editor/assets/index.js)
                                    handlers: get-app-state, execute, get-style, read-skill,
                                    start-task, spawn-agents, browser, import-website, save
                                                   |
                                       execute -> QuickJS (WASM, asyncify) sandbox
                                       -> SceneGraph (beginUpdate/commitBlock/rollback)
                                       -> renderer Skia/WASM (pencil-*.wasm), flashes, cursores
```

Pontos-chave:
- **Toda lógica das ferramentas vive no renderer do editor** (o mesmo código que roda no headless). O binário Go é só um *proxy* stdio→pipe; o agente embutido pula o binário e chama o mesmo handler via IPC "in-process".
- **`execute` = JavaScript arbitrário executado em QuickJS (WASM)**, com uma API global pequena (`Insert`, `Update`, `Get`, …). Não é um DSL próprio nem JSON de operações.
- O código JS é **executado em streaming**, instrução por instrução, enquanto o LLM ainda está gerando o argumento da ferramenta — é isso que dá o efeito "o canvas desenha ao vivo".

---

## 1. O servidor MCP

### 1.1 Binário `mcp-server-windows-x64.exe` — fingerprint

| Item | Evidência (strings) |
|---|---|
| Linguagem/runtime | **Go** (`Go build ID`, `go1.26.11`, símbolos `runtime.*`). Não é Rust nem JS compilado com Bun. |
| Biblioteca MCP | `github.com/mark3labs/mcp-go@v0.48.0` (`server.NewStdioServer`, `server.ServeStdio`). Versões de protocolo embutidas: `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`. |
| Outras deps | `github.com/Microsoft/go-winio@v0.6.2` (named pipes no Windows), `google/jsonschema-go`, `google/uuid`, `spf13/cast`, `yosida95/uritemplate`. |
| Módulo próprio | `github.com/highagency/ha/servers/mcp` com pacotes `tools/` (`browser.go`, `execute.go`, `get_app_state.go`, `get_style.go`, `read_skill.go`, `save.go`, `spawn_agents.go`, `start_task.go`, `editor_request.go`, `util.go`) e `transport/` (`client.go`, `dial_windows.go`). `main.startStdioServer`, `main.clientNameFromContext`. |
| Transporte para o cliente MCP | **apenas stdio** (não há servidor SSE/Streamable HTTP; nenhuma porta TCP). |
| Transporte para o app | **cliente de named pipe** `\\.\pipe\pencil-%s` (Windows) — no macOS/Linux, socket Unix `~/.pencil/socket/pencil-<app>.sock`. `<app>` = `desktop` (ou `desktop-dev`). |

Flags de linha de comando (strings + `getMcpConfiguration` em `@ha/mcp`):
- `--app <nome>` — qual instância do app (define o nome do pipe).
- `--conversation_id <id>` — "Optional conversation id to identify the MCP tool calls to" (usado quando o Codex embutido usa o binário, para que as chamadas sejam atribuídas à conversa certa → cursor/flash coloridos do agente certo).
- `--enable_spawn_agents` — habilita a ferramenta `spawn_agents`.
- `--disable_save_tool` — remove `save` (o Codex embutido usa isso; o app salva sozinho).
- `--agent <nome>` — "Optional name of agent connecting to the app"; se ausente, o nome vem do `clientInfo` MCP (`clientNameFromContext`).

### 1.2 Protocolo binário ↔ app (pipe)

O lado servidor é `MCPSocketServer` em `@ha/mcp` usando **`@node-ipc/node-ipc`**; o lado Go reimplementa o *wire format* do node-ipc:
- Mensagens são JSON `{"type": <evento>, "data": <payload>}` **delimitadas por form-feed `\f`** (`splitByFormFeed`, `{"type":"tool_request","data":%s}%c`).
- Handshake: ao conectar, o app atribui um UUID e envia `tool_response` com `request_id: "client-id-assignment"` e `client_id`. O Go loga `received client ID` / `received handshake-ack`.
- Em seguida o Go envia `agent_connected {client_id, agent}` → o app emite `connected-agents-changed` para a UI (lista de agentes externos conectados). Ao desconectar, `agent-disconnected`.
- Chamada de ferramenta: `tool_request {client_id, request_id, name, payload:{uri|filePath, ...params}, timeout_ms}` → resposta `tool_response {client_id, request_id, success, result, error}`.
- Retentativas de conexão: "failed to connect to running Pencil app: %s after %d retries" (o app precisa estar aberto).
- Nomes das requisições IPC usam hífen (`get-app-state`, `read-skill`, `start-task`, `spawn-agents`…); o `@ha/ipc` faz `toolName.replaceAll("_","-")`.

Roteamento no app (`MCPTransport.handleIPCRequest` + `IPCDeviceManager.getMCPToolIPC`):
1. Extrai `uri` (ou o legado `filePath`) do payload.
2. Se o documento já está aberto, usa o `IPCHost` daquela aba; senão **abre o arquivo** (`openFileForAgent`) e espera ficar pronto.
3. Sem documento utilizável, cai para o último documento focado.
4. Encaminha `ipc.request(name, params, timeout_ms)` para o renderer daquela aba.

Erro típico devolvido: "A document needs to be open in the editor to perform this action" / "you are probably referencing the wrong .pen file".

### 1.3 Instalação automática em clientes de terceiros (`@ha/mcp` installer + `desktop-mcp-adapter.js`)

Na inicialização (após `mcp-transport-ready`), o app **escreve a entrada `pencil`** nos arquivos de config dos clientes habilitados (padrão: todos os suportados no desktop):

| Integração | Arquivo | Chave |
|---|---|---|
| claudeCodeCLI | `~/.claude.json` (+ adiciona `mcp__pencil` em `permissions.allow` de `~/.claude/settings.json`) | `mcpServers` |
| codexCLI | `~/.codex/config.toml` | `mcp_servers` (TOML) |
| geminiCLI | `~/.gemini/settings.json` | `mcpServers` |
| antigravity | `~/.gemini/antigravity/mcp_config.json` | `mcpServers` |
| openCodeCLI | `~/.config/opencode/opencode.json` | `mcp` (`type: local`, `command` como array) |
| kiroCLI | `~/.kiro/settings/mcp.json` | `mcpServers` |
| claudeDesktop | `%APPDATA%/Claude/claude_desktop_config.json` (mac: `~/Library/Application Support/Claude/…`) | `mcpServers` |
| (tipos existentes mas não habilitados no desktop) windsurfIDE, cursorCLI, antigravityIDE, copilotIDE | — | — |

Configuração gravada: `{ command: <app>/out/mcp-server-<os>-<arch>, args: ["--app","desktop","--agent","<integração>"], type: "stdio" }`. O app remove e reinstala a cada boot (para limpar integrações não mais suportadas). Há IPC `get-mcp-config` para a UI mostrar o JSON manualmente.

### 1.4 Instruções do servidor (MCP `instructions`)

Resumo: "este servidor serve para interagir com apps pen.dev e arquivos `.pen`… Arquivos `.pen` são **criptografados**: acesse-os só pelas ferramentas MCP — nunca use Read/Grep em `.pen`. Siga os schemas; conheça o schema .pen e as instruções do `execute`." (A afirmação de criptografia é do próprio servidor; o `HeadlessResourceDevice` lê o arquivo como texto UTF-8 — ver §2 e Incertezas.)

### 1.5 Lista completa de ferramentas

Duas fontes: `@ha/mcp/dist/schemas-*.mjs` (`toolSchemas`, usado pelos agentes embutidos) e o binário Go (clientes externos). Diferenças: **`save` só existe no Go**; **`import_website` só existe no JS** (não há handler Go). Quase todas exigem `uri` (`file:///…/design.pen`).

| Ferramenta | Anotações | Parâmetros | Semântica |
|---|---|---|---|
| `start_task` | readOnly, idempotent | `skills: string[]` (obrig., caminhos do SKILL.md, ex. `guide/landing-page.md`; `pen-schema.md` e `execute.md` são necessários para editar), `style?: boolean` (design novo sem marca → anexa lista de estilos), `title?: string` (título 2–5 palavras da sessão, no idioma do usuário) | "Chame antes de qualquer outra ferramenta." Lê cada arquivo de skill (`read-skill-file`) e devolve tudo concatenado em `<path>/<content>`; renomeia a conversa. Desabilitada em sessões retomadas. Resultado pode ter até 200 000 chars (`anthropic/maxResultSizeChars`) para o Claude não truncar em arquivo. |
| `get_app_state` | readOnly | — | Markdown com: URI do editor ativo; nós selecionados (`id (tipo): nome`); até 10 nós top-level marcando "user visible"/"outside viewport"; componentes reutilizáveis; estado do browser lateral (URL carregada, caminho do elemento selecionado com nome de componente detectado); "Signed in to pen.dev: yes/no". O app **injeta automaticamente** esse resultado no prompt do agente embutido antes do primeiro turno. |
| `read_skill` | readOnly | `path?` (relativo ao SKILL.md; ausente = SKILL.md) | Lê arquivos de `out/skills/pen-dev` (com proteção contra path traversal). |
| `get_style` | readOnly | `name?`, `params?: object` | Fluxo em 3 passos: lista estilos → pede parâmetros obrigatórios → devolve o "arquétipo" (identidade, composição, tipografia sugerida com Google Fonts, cores, imagens). Os ~26 estilos (Aerial Gravitas, Blueprint Technical, Soft Bento, …) estão **embutidos no bundle do editor** como texto. "Estilos não salvam variáveis, só fornecem valores de referência." |
| `execute` | destructive | `uri`, `description` (rótulo 3–8 palavras, imperativo, mostrado ao usuário; deve vir primeiro), `input` (snippet JS), `editId` + `edits: [{find, replace, all?}]` (patch de um snippet que falhou) | Executa JS em sandbox QuickJS contra o documento — ver §1.6. |
| `browser` | openWorld | `uri`, `action` ∈ {`load-page`, `import-to-canvas`, `screenshot-to-canvas`, `return-element`, `return-screenshot`, `cdp`}, `nodeId?` (nó `browser` no canvas; ausente = browser lateral), `target` ∈ {`full-page`, `selection`, `query`}, `querySelector?`, `url?`, `method?` + `params?` (comando CDP) | Dirige um Chromium real embutido (sidebar ou nós `browser` no canvas). Importa página como camadas editáveis (cada nó recebe `context` = tag + nome do componente detectado, ex. "Card - div"), screenshot para o canvas ou para o modelo, DOM+estilos computados, ou um comando CDP cru (domínios permitidos: DOM, Input, Page, Runtime, Emulation, CSS, Accessibility). Não enviar Escape (o editor intercepta). |
| `import_website` | openWorld | `uri`, `url` | Importação **server-side** (servidores pen.dev) de uma página pública inteira como um frame top-level; timeout de 10 min. Usada quando o host não tem browser embutido (no desktop o agente recebe `browser` e `import_website` fica desabilitada). |
| `spawn_agents` | — | `uri`, `config: [{prompt, containerNodes: string[]}]` | Divide o trabalho entre até 8–10 "designer agents" paralelos, cada um num nó-contêiner placeholder já criado. Só disponível quando o usuário escolhe multiplicador de agentes > 1 (`--enable_spawn_agents`). Subagentes não recebem `get_app_state` nem `spawn_agents`. Também é *streamed*: cada item de `config` completo no JSON parcial já dispara um agente. |
| `save` | — | `uri` | (Só Go/externo.) Espera imagens/SVGs/transformações pendentes e pede `save-resource` ao host (timeout 300 s). |
| `update_properties` | — | — | Aparece apenas como rótulo legado na UI ("Updating properties..."); não há schema atual. Strings do Go mencionam "failed to execute batch design operations" — o `execute` é o sucessor de um antigo `batch_design`. |

Cobrança: cada handler passa por `billingManager.consume(featureKey)`; no plano gratuito há um limite de "dias ativos de uso MCP" por ciclo. Ao estourar, a ferramenta devolve um texto "STOP…" instruindo o modelo a não contornar o limite (inclusive não editar `.pen` direto no disco) e a responder com uma mensagem fixa de upgrade.

### 1.6 `execute` em profundidade

**Linguagem:** JavaScript (ES moderno) executado em **QuickJS compilado para WASM** (`@jitl/quickjs-wasmfile-release-asyncify` / `-sync`, carregados via `scriptManager.getAsyncModule()`). Cada chamada cria um `QuickJSContext` novo com:
- limite de memória **256 MiB**, pilha **512 KiB**;
- *interrupt handler* com orçamento de **2 s de CPU por trecho executado**, descontando o tempo gasto em chamadas ao host;
- timeout total da chamada **55 s** (`ExecuteProcessTimeoutError`).

**Prelúdio injetado** (`batch_design_runtime.js`): define globais que chamam funções FFI do host (`__batchDesignI/C/R/M/D/U/V/P/G/T/X/Get/GetVariables/FindEmptySpace`), sempre passando `new Error().stack` para que erros apontem a linha/coluna do snippet (`batch_design.js:L:C`) e a mensagem de erro cite o trecho exato da chamada culpada.

**API exposta** (de `execute.md` + implementação):

| Função | Retorno | Notas |
|---|---|---|
| `document` | id raiz | aliases aceitos: `document`, `root`, `#document`, `#root` |
| `Insert(parent, nodeData)` | id (string) | um nó por chamada; ids gerados sempre pelo app (ignora `id`); anima mudança de layout e faz *flash* |
| `Copy(path, parent, overrides?)` | id | descendentes ganham ids novos; overrides em descendentes via mapa `descendants`; copiar nó `reusable` cria instância `ref` |
| `Update(path, data)` | void | não muda `id`, `type`, `ref`, nem `children` |
| `Replace(path, nodeData)` | id | ideal para trocar filhos de instância (`inst/childId`) |
| `Move(path, parent?, index?)` | void | |
| `Delete(path)` | void | não apaga descendentes de instância (usar `enabled:false`) |
| `SetVariables(vars, replace?)` | void | `{nome:{type:"color"|"number"|"string", value}}`; valores temáticos `[{value, theme:{mode:"dark"}}]`; eixos de tema registrados automaticamente |
| `GetVariables()` | `{variables, themes}` | |
| `Get(path?, visitor?, options?)` | nó ou `T[]` | argumentos reconhecidos por tipo; sem visitor devolve schema que faz *round-trip* em Insert/Update; com visitor recebe `ctx {node, parentCtx, depth, index, bounds, problems:"partially clipped"|"fully clipped", skipChildren()}`; options `depth`, `resolveVariables`, `resolveInstances`, `includePathGeometry`. Ler o documento inteiro sem visitor é proibido (erro). |
| `FindEmptySpace({width,height,direction?,padding?,nodeId?})` | `{x,y,parentId?}` | |
| `Print(...values)` | void | adiciona linhas à resposta |
| `TakeScreenshot(nodeIds)` | void (async) | renderiza e anexa imagens à resposta (`"document"` = tudo) |
| `Export(nodeIds, format, outputPath, options?)` | void (async) | `png|jpeg|webp|pdf|svg|html-tailwind|html-css`; grava via IPC `write-file` |
| `Generate(type, …)` | void ou url | `ai`, `svg`, `stock`, `vectorize-image`, `remove-background`, `replace-background` — tudo assíncrono, resolve após o `execute` retornar (ver §3.5) |

**Semântica transacional:**
- Toda a chamada roda dentro de um *block* do scenegraph (`beginUpdate`). Falha em qualquer operação ⇒ `rollbackBlock` (nada aplicado, globais descartados). Sucesso ⇒ `commitBlock({undo:true})` — **um passo de undo por chamada**, nós criados marcados como "agent".
- Validação de schema por nó (`schemaValidationCallback`); propriedades inválidas viram *warnings* na resposta ("sempre corrija na próxima chamada").
- Resposta de sucesso: mensagem com mapeamento **nome → id** dos nós criados, linhas de `Print`, warnings, caminhos exportados e screenshots (como content `image`). Estatísticas (duração, nº de ops, tipos criados) vão para telemetria.
- **Persistência entre chamadas:** variáveis atribuídas sem `const/let` (globais) são exportadas (`__batchDesignExportGlobals`) e reinjetadas na próxima chamada **do mesmo agente/conversa** (`persistedBindings` por `agentId`), e podem ser serializadas com a sessão de chat. Locais e funções não persistem.
- **Correção por patch:** ao falhar, a resposta traz um `editId`; o modelo deve reenviar só `edits: [{find, replace}]` (aplicados em ordem, match único salvo `all:true`; o host tenta variantes com `\n` escapado). Isso economiza tokens de saída.

**Execução em streaming (detalhe importante):**
- O SDK do Claude roda com `ANTHROPIC_BETAS=fine-grained-tool-streaming-2025-05-14` e `includePartialMessages`. Em cada `input_json_delta` do `tool_use` `mcp__pencil__execute`, o agente reconstrói o JSON parcial (`jsonrepair`), extrai o crescimento de `input` e emite `tool-use` com `streaming:true`; o `IPCDeviceManager` repassa como `execute-streaming` (`partial:true`) ao editor.
- No editor, um **"rechunker" baseado em acorn** acumula o texto e só libera até o último ponto em que a(s) instrução(ões) top-level estão sintaticamente completas (tratando casos como `if` que pode ganhar `else`, `try` sem `finally`). Cada pedaço é avaliado na mesma `QuickJSContext` via fila de concorrência 1.
- A chamada final (não parcial) espera a fila esvaziar e então faz commit/rollback. Resultado: os nós aparecem no canvas enquanto o modelo ainda "digita" o código, mas a transação continua atômica.
- Cursor SDK faz o mesmo (rastreando o comprimento já enviado); Codex (via binário Go) não tem streaming.

### 1.7 Outras requisições IPC relacionadas no editor
`serialize`, `import-dom-snapshot`, `import-browser-clipboard`, `import-library`/`remove-library`/`list-libraries` (bibliotecas `.lib.pen`), `save-document`, `permission-request` (UI de aprovação de ferramenta), `fetch-website-import`, `read-skill-file`, `write-file`, `get-file-uri-for-path`.

---

## 2. `@ha/headless` — renderização/edição sem janela

Arquivos: `headless-resource-device.ts`, `loopback-ipc.ts`, `preview-process.ts`, `preview-request.ts`.

- **`createLoopbackIPC`**: um `IPCHost` cujo "send" emite no mesmo `EventEmitter` que o "receive" — host e editor conversam no mesmo processo, sem Electron/janela. O protocolo IPC é idêntico ao do renderer real, então **todo o código do editor (incluindo os handlers MCP e o `execute`) funciona igual headless**.
- **`HeadlessResourceDevice`** implementa a interface `ResourceDevice` (a mesma que o desktop implementa) apoiada em `fs`: lê o documento de `inputPath` (ou cria `{version, children: []}`), grava em `outputPath` via `ipc.request("serialize")` (sem `outputPath` ⇒ somente leitura), resolve URIs de arquivos/imagens relativos ao `.pen`, importa arquivos com nomes não colidentes, lê skills de `out/skills/pen-dev`. Diálogos, temas, sidebars, rename etc. são no-ops ou "Not implemented". Opções opcionais `account` (sessão/sign-in web) e `agents` (API keys/env) e o evento `prompt-agent` indicam que o mesmo device é usado por um **host CLI/servidor** capaz de rodar agentes sem UI (não incluído neste app — incerteza).
- **`preview-process`** (bundle de 6,7 MB, minificado, com o editor inteiro): recebe um `PreviewRenderRequest {documentPath, previewPath, appFolderPath, browserPreviewsFolder, hostVersion}` em `argv[2]`, monta `IPCDeviceManager` + `HeadlessResourceDevice` + **`HeadlessEditor.open(...)`** (de `@ha/pencil-editor`, sem acesso à API pen.dev, `isSignedIn=false`), e se houver nós chama `editor.renderToImage({PNG, scale 1, resolution 512, Fit})`. Grava atomicamente (`.tmp` + rename) e carimba o `mtime` do PNG com o `mtime` do documento — é assim que o host sabe se a miniatura está atual. Documento vazio ⇒ PNG 1×1 transparente.
- **Host** (`out/preview-queue.js`): `electron.utilityProcess.fork("preview-process.js")`, prioridade baixa, timeout de 120 s, sessão `defaultSession` (reaproveita cache HTTP de fontes/imagens; `out/preview-process.js` troca `globalThis.fetch` por `electron.net.fetch`). Falha não é re-tentada (grava preview vazio). Usado para as miniaturas do dashboard.
- **Renderer**: o mesmo motor do editor — `@highagency/pencil-wasm` (Skia/CanvasKit-like em WASM, `pencil-*.wasm`) — sem DOM. Efeitos visuais de agente (flash/cursor) são desativados no modo headless (guarda `if (headless) return` em `addFlash`).

---

## 3. Agente de chat embutido (`@ha/agent`, `@ha/shared`, `out/*.js`)

### 3.1 Fluxo de um prompt
1. Renderer envia `send-prompt {prompt, modelID, conversationId, sessionId?, files?, agentMultiplier?, parallelMode: split|side-by-side, permissionMode: auto|default, effort?, skillNames?, subagent?, penInference?}`.
2. `IPCDeviceManager.invokeAgent`:
   - deriva o tipo do agente do `modelID` (`claude` | `codex` | `cursor` | `pi`);
   - monta o **prompt final**: prompt do usuário + (se multiplicador>1 e modo split) instrução para dividir em até N−1 agentes com `spawn_agents` + `userMessageExtension` opcional + **resultado de `get_app_state` pré-buscado** com a nota "não está obsoleto, não chame de novo";
   - monta o **system prompt** = conteúdo de `SKILL.md` (os arquivos `execute.md` e `pen-schema.md` ficam de fora — entram via `start_task`) + regra `<final_message>` ("o usuário vê o canvas ao vivo; não recapitule; termine com uma frase curta ou nada");
   - anexa skills escolhidas pelo usuário (`skillNames`) como anexos de texto;
   - calcula ferramentas proibidas: `start_task` em sessões retomadas; `get_app_state`/`spawn_agents` para subagentes; `spawn_agents` sem multiplicador; `browser` **ou** `import_website` conforme o host;
   - cria o `inProcessToolHandler`: converte `snake_case`→`kebab-case`, injeta `{conversationId, id: toolUseID}`, resolve/abre o documento pelo `uri`, faz `ipc.request` e converte o retorno em content MCP (`text` ou `image`, múltiplas imagens para screenshots).
3. Eventos da sessão (`start`, `message(-start)`, `thinking(-start)`, `tool-use(-start)`, `tool-result`, `usage-update`, `permission-request`, `end`) são repassados ao renderer como `agent-session-event`. Uso de tokens é anotado com estimativas de tokens gastos em skills e no texto injetado.
4. `agent-stop`, `agent-steer` (inserir mensagem do usuário no meio do turno — Claude e Pi) e fechamento do documento param o agente.

### 3.2 Implementações por SDK

| Agente | SDK | Como recebe as ferramentas pencil | Notas |
|---|---|---|---|
| **Claude** | `@anthropic-ai/claude-agent-sdk` (^0.3.x) + `claude.exe` empacotado (`claude-agent-sdk-win32-x64`) | **in-process**: `createSdkMcpServer({name:"pencil", tools})` a partir de `toolSchemas` (JSON Schema → zod); handler usa `extra._meta["claudecode/toolUseId"]` | `systemPrompt: {preset:"claude_code", append: <skills>}`; `settingSources: local/project/user`; `skills:"all"`; `strictMcpConfig`; `allowedTools`: AskUserQuestion, WebSearch, WebFetch, `mcp__pencil__*`, MCPs do usuário; `disallowedTools: ["Agent"]`; `maxTurns 500`; *adaptive thinking* resumido; **effort "low" até a 1ª chamada de ferramenta** (hook PreToolUse restaura); `canUseTool`: auto-permite AskUserQuestion, acesso a arquivos dentro do `cwd` (pasta de workspace) e `Read` em `~/.claude/skills` e `~/.pencil/skills`; o resto vira `permission-request` na UI com opções allow / switch-to-auto / deny. Sessões em `PencilSessionStore` próprio (migra sessões antigas de `~/.claude/projects`). Login: subscription, api-key, oauth, Bedrock, Vertex, Foundry, custom (env `CLAUDE_CODE_USE_*`). |
| **Codex** | `@openai/codex-sdk` + binário `codex` empacotado | **via binário Go stdio** (`getMcpConfiguration` com `conversation_id`, `disable_save_tool`) em `mcp_servers.pencil` com `default_tools_approval_mode: approve`, `disabled_tools`, timeouts 30 s/5 min | `sandboxMode: workspace-write`, `approvalPolicy: never` (o SDK não tem callback de aprovação), web search on, `features.plugins=false` (evita o plugin pen.dev duplicado); system prompt **prefixado no primeiro turno** (limite de 32 KB de cmdline no Windows). |
| **Cursor** | `@cursor/sdk` (+ `cursorsandbox.exe`, `rg.exe`) | `local.customTools` in-process | system prompt prefixado no 1º turno; streaming de `execute` emulado; HTTP/1 opcional. |
| **Pi** | `@earendil-works/pi-coding-agent` / `pi-agent-core` / `pi-ai` | `customTools` in-process (`buildPenCustomTools`) + extensão MCP (`~/.pencil/mcp.json`) | Provedores: google, deepseek, moonshotai, kimi-coding, openrouter, together, opencode(-go), xai, fireworks, zai, github-copilot (OAuth), meta, **pen.dev** (proxy próprio compatível com OpenAI *completions*, corpo gzip, corte de imagens antigas acima de ~4,2 MB por limite do Vercel), e provedores custom em `~/.pencil/models.json`. Sessões em `~/.pencil/pi-sessions`. Thinking "minimal" até a 1ª ferramenta. Feedback 👍/👎 para modelos pen.dev. |

Probes de conexão (`AGENT_PROBE_SETTINGS`): prompt "What is 2+2?" com modelo pequeno de cada provedor, timeout 45 s; Bedrock/Vertex/Foundry/custom não são testados.

Outros arquivos:
- `out/claude.js` / `out/codex.js`: só resolvem o caminho dos pacotes em `app.asar.unpacked/node_modules/@anthropic-ai|@openai` e montam env por tipo de login.
- `out/agent-config-manager.js`: agrega para a UI os modelos/estados de login de cada agente (catálogos via `AgentAuthStore` em `~/.pencil/agent-auth`), API keys mascaradas (`maskApiKey`), provedores custom.
- MCPs do usuário: `~/.pencil/mcp.json` (com expansão `${VAR}`) é carregado em todos os agentes.

### 3.3 Skills (`out/skills-manager.js`)
- Skill interna **pen-dev** (`out/skills/pen-dev`) sempre disponível; 6 "guias" expostos também como skills selecionáveis (competitive-research, recruiting-profiles, sitemap, travel-plan, make-website-responsive, add-stacked-animations).
- **Pacote remoto**: baixa `<BLOB_STORAGE_URL>/skills/latest.json`, respeita `minDesktopAppVersion` (semver, *fail closed*), baixa zip, extrai em staging e troca atomicamente `CONFIG_FOLDER/skills`.
- **Skills custom** do usuário: caminhos de `SKILL.md` em config (`customSkills`), frontmatter YAML (`description`, `author`).

### 3.4 Visualização das edições do agente no canvas
- **Agent flash** (`@ha/shared/agent-flash.ts`): envelope ADSR de um anel + linha de varredura sobre o bounding box do nó — ataque 200 ms, *hold* 400 ms (1500 ms em leituras "longHold"), release 300 ms; a varredura tem duração proporcional à altura (velocidade aparente constante), repetida 3× (1,5× em longHold). Disparado em `Insert`/`Copy`/`Replace`/`Update` (escrita) e em `Get`/`spawn_agents`/leituras (longHold). Cor = cor do perfil do agente. A mesma função de timing é serializada e roda no overlay injetado nas páginas dos nós `browser` (`@ha/browser-inject`), para Skia e DOM ficarem em sincronia.
- **Agent cursor** (`agent-cursor.ts`): glyph de seta (ou um glyph de "caneta" para quando o gerador de SVG desenha traço a traço), rótulo com nome do agente, sombra/contorno. Movimento por *spring* com decaimento exponencial (taxa 2,5/s), passo máximo 0,1 s, fade-in 200 ms, fade-out após 5 s ocioso durante 1,5 s, "wobble" leve quando parado; o alvo é sorteado com distribuição triangular dentro do bounds do nó (evita empilhar). Cada flash move o cursor do agente até o nó (`moveToBounds`), e o flash atrasa 300 ms se o cursor teve que viajar. Renderizado em Skia (`agentCursors.render`) após a cena, e em DOM nos browsers.
- **Perfis de agentes paralelos** (`agentProfileTemplates`): cada subagente de `spawn_agents` recebe nome/cor próprios, então vários cursores coloridos trabalham simultaneamente em seus contêineres.
- Inserções animam a mudança de layout dos pais (`animateLayoutChange`). Placeholders (`placeholder:true`) e efeitos "generating" (animação girando) marcam áreas em construção / geração pendente.

### 3.5 Geração e transformação de imagens (`@ha/shared/*-provider.ts` + editor)
Tudo passa pelo backend pen.dev (`sendAPIRequest`, requer login; o preview headless não tem API):
- **Imagem raster (`Generate("ai")`)** → `POST generate-image`. Provedores: `nano_banana`, `nano_banana_lite` (padrão e único gratuito), `openai_gpt_image_2_6_flare`, `openai_gpt_image_2_6_sunburst` (com normalização de nomes legados `openai`, `openai_gpt_image_2`…).
- **Stock (`Generate("stock")`)** → `POST get-stock-image` (Unsplash, com crédito/link do autor guardados).
- **SVG (`Generate("svg")`)** → `POST generate-svg` em **stream** (eventos `draft`/`content`/`error`); provedores `quiver`, `recraft`, `pen` (padrão/gratuito). O resultado vira nós `path` dentro do frame-alvo, desenhados progressivamente com o "pen cursor".
- **Transformações** (`remove-background`, `replace-background` com prompt, `vectorize-image`) → provedor `recraft`; JPEG/PNG/WEBP, ≤16 MB, 256–4096 px.
- Modelo assíncrono: durante o `execute` só é escrita a "contabilidade" — fill com URL `pencil:pending-image-…` (reescrita no lugar quando chega) ou frame com `placeholder:true` (limpo ao terminar). Limite de plano ⇒ URL `pencil://deferred-generation/...?prompt=…&source=…` que o usuário dispara depois pelo canvas. `save` espera todas as gerações pendentes.

---

## 4. Skills `pen-dev` — o que cada uma ensina

| Arquivo | Conteúdo / workflow |
|---|---|
| `SKILL.md` | Índice (tabela "quick reference") + regras gerais: copiar e alterar em vez de gerar do zero; não sobrescrever variáveis; fazer só o pedido; documento multiplayer (re-ler antes de assumir estado); raiz limpa (só telas/componentes/browsers); componentes reutilizáveis para UI repetida; mostrar progresso rápido; checklist de verificação por seção (layout não colapsado, sem clipping, contraste, alinhamento); estilo anti-"AI genérico" (sem cartões em tudo, sem gradientes/sombras gratuitos, nunca desenhar logos à mão); lista dos ~26 arquétipos do `get_style`; regras de objetos/flexbox/`textGrowth`/`fill` obrigatório em texto/sem percentuais; placeholders; SVG `viewBox`; gráficos (preferir barras; donut com `innerRadius`). |
| `execute.md` | Referência da API do `execute` (ver §1.6): regras (sem comentários, sempre `name`, nunca `id`, globais para persistir, corrigir com `edits`), assinaturas TS, semântica de cada função, exemplos (componente Metric, loops, instâncias com `descendants`, Copy+Delete, tokens temáticos). |
| `pen-schema.md` | Schema TypeScript do `.pen` v2.22: variáveis/temas; `Layout` (flex de eixo único, `gap`, `padding`, `justifyContent`, `alignItems`); `SizingBehavior` (`fit_content`/`fill_container` com fallback `(N)`); fills (cor, gradiente linear/radial/angular, imagem com `mode` e transform de crop, **shader** GLSL, **mesh_gradient**); strokes por lado; effects (blur, background_blur, shadow, shader); tipos de nó: `frame`, `group`, `rectangle`, `ellipse` (arco/anel), `polygon`, `path`, `text`, `note`, `icon` (lucide, feather, Material Symbols, phosphor), `script`, `browser`, `ref` (instância com `descendants`); `imports` de bibliotecas. |
| `generate.md` | Uso do `Generate`: quando usar cada tipo, assinatura/ordem dos argumentos, natureza assíncrona e como checar resultado com leituras baratas em chamadas posteriores (não screenshot), falhas silenciosas, URLs `deferred-generation`, orçamento de SVG (componentizar e reutilizar), limites das transformações. |
| `scripts-and-shaders.md` | Nós `script` (arquivo `.js` com `/** @schema 2.11 */`, `@input` tipados, recebe `pencil.width/height/input`, retorna array de nós; `Math.random` determinístico) e shaders WebGL 1.0 (uniforms anotados `@resolution`, `@time`, `@mouse`, `@sdf`, `@mipmap`, `@range`, `@label`, `@color`; samplers especiais `pencil:object` e `pencil:background`; shader como fill vs como effect encadeado). |
| `whiteboard.md` | Usar o canvas para pesquisa/planejamento: responder do tamanho da pergunta; todo dado acionável vem de fonte aberta na sessão (com fonte e data ao lado); "seja lúdico" (boarding passes, recibos, mapas de metrô); usar o canvas infinito com vários frames e nós `browser` (1000×1400) livres; escala tipográfica para zoom (120–240 px resposta, ≥24 px evidência); construir ao vivo com `FindEmptySpace` e placeholders; revisar com screenshots e `ctx.problems`; não fazer login/compras pelo usuário. |
| `competitive-research.md` | Showdown em torno do produto do usuário: rivais ao redor, cor de marca de cada um, uma tarefa real traçada em raias paralelas com crops `screenshot-to-canvas`, preços na mesma base, oportunidades; separar fato de interpretação. |
| `recruiting-profiles.md` | Dossiês de candidatos em torno da vaga: território igual por candidato, monograma (nunca foto), timeline, evidência por requisito (supported/partial/not found) com fonte, perguntas para lacunas; cuidados de privacidade e de não ranquear por tamanho. |
| `sitemap.md` | Mapear site existente: sitemap.xml → navegação; árvore crescendo da home, thumbnails por seção, cross-links, rota-chave destacada, inventário de URLs; números "descobertas vs inspecionadas". |
| `travel-plan.md` | Plano de viagem: tudo que é preço/horário verificado em fontes; jornada disposta geograficamente; paleta do destino; fotos stock; restrição crítica sinalizada com fallback; orçamento em barra (observado/estimado/desconhecido). |
| `code-website.md` | Design → código com preview vivo: ler frames/variáveis/componentes; framework do projeto (default Vite+React+TS+Tailwind v4); dev server; nós `browser` desktop e phone ao lado do design (snippet com `FindEmptySpace`); construir seção a seção comparando `return-screenshot` × `TakeScreenshot` e `ctx.bounds` × `getBoundingClientRect` via CDP; manter design e código sincronizados nos dois sentidos. |
| `guide/code.md` | Gerar código a partir de `.pen`: usar frameworks/libs existentes do projeto, mesmos textos/ícones/espaçamentos, atualizar componentes existentes, mapear variáveis para tokens. |
| `guide/tailwind.md` | Tailwind v4: só classes, variáveis do `.pen` em `:root`, fontes em `@layer base`, armadilhas (`@theme`, next/font). |
| `guide/components.md` | Componentes (`reusable`) e instâncias (`ref`), overrides via `descendants` com caminhos `inst/child` para instâncias aninhadas, slots. |
| `guide/design-system.md` | Compor telas com componentes de design system: padrões de nomes (`Button/*`…), slots (`slot: [ids recomendados]`, inserir em `inst/slotId`), ícones, layouts de dashboard. |
| `guide/landing-page.md` | Princípios de landing page (conversão, headline de transformação, CTA dominante, ritmo de seções). |
| `guide/mobile-app.md` | Telas mobile: status bar 62 px, wrapper com padding único, tab bar em cápsula "Liquid Glass", altura `fit_content(844)`. |
| `guide/web-app.md` | Leis estruturais de UI de produto (propósito único, região dominante, divulgação progressiva…). |
| `guide/slides.md` | Slides 1920×1080 legíveis (corpo ≥24 px, títulos ≥40 px, uma ideia por slide). |
| `guide/table.md` | Hierarquia obrigatória Table → Row → Cell (frame) → conteúdo; cards no mobile. |
| `guide/make-website-responsive.md` / `guide/add-stacked-animations.md` | Skills de código (HTML/CSS) para reflow responsivo e animações escalonadas com IntersectionObserver/`prefers-reduced-motion`. |
| `guide/whiteboard-example.md` | Exemplo trabalhado (comparar issue trackers) mostrando a mecânica de chamadas `execute`+`browser`. |

---

## 5. Implicações para Rust/GPUI

### 5.1 Servidor MCP
- **Arquitetura recomendada:** servidor MCP **dentro do processo do app** usando o crate oficial **`rmcp`** (Rust MCP SDK) com transporte *streamable HTTP* em `127.0.0.1:<porta>` (+ token) **e** um *shim* stdio. Clientes como Claude Code/Codex ainda preferem stdio: manter um binário mínimo `openpen-mcp` (o próprio executável com subcomando `mcp`, ou um binário Rust pequeno) que faz ponte stdio ↔ socket local, exatamente como o legado faz com Go.
- **IPC local:** `interprocess` (named pipes Windows / Unix sockets) ou o `tokio::net::windows::named_pipe`. Não há motivo para copiar o framing node-ipc (`{"type","data"}\f`); usar JSON-RPC MCP direto pelo pipe simplifica (o shim vira um *byte pump*). Manter o conceito de `client_id`, nome do agente conectado (lista na UI) e `conversation_id` para atribuir cursor/cor.
- **Roteamento por documento:** toda ferramenta recebe `uri`; o host abre o documento se necessário e direciona ao `Entity<Document>` correspondente. Reproduzir o fallback "último documento focado".
- **Instalador de integrações:** portar a tabela de arquivos de config (§1.3) com `serde_json`/`toml_edit` (preservando formatação). Tornar opt-in (o legado faz opt-out e reescreve no boot — intrusivo).
- **Paridade de ferramentas:** `start_task`, `get_app_state`, `read_skill`, `get_style`, `execute`, `browser`, `save`, `spawn_agents` (+ opcional `import_website`). Gerar os JSON Schemas a partir de structs com `schemars`, compartilhados entre o servidor MCP e o agente embutido (como o `toolSchemas` único do legado).

### 5.2 API equivalente ao `execute`
Opções:

| Abordagem | Prós | Contras |
|---|---|---|
| **JS embutido — `rquickjs`** (QuickJS, como o legado) | Compatível com tudo que os LLMs já sabem fazer (loops, helpers, spreads); permite reaproveitar quase literalmente `execute.md`/skills; QuickJS tem limites de memória/stack e *interrupt handler* (mesmo modelo de segurança do legado); `rquickjs` tem async. | Precisa ponte de tipos JSON ↔ JS; GC/lifetimes do `rquickjs` exigem cuidado; parser incremental para streaming. |
| `boa_engine` | Rust puro, sem C. | Mais lento, conformidade ES menor, sem interrupção por orçamento tão madura. |
| `deno_core`/V8 | Desempenho e ES completo. | Binário enorme, build complexo, superfície de segurança maior. |
| Lista de ops JSON (`[{op:"insert",parent,data}…]`) | Simples de validar, determinística, fácil de *replay*/undo. | Muito mais tokens para UI repetitiva; perde `Get` com visitor, variáveis entre ops e computação; modelos erram referências a ids ainda não criados. O próprio legado migrou de `batch_design` (ops) para JS. |

**Recomendação:** `rquickjs` com a mesma API global (Insert/Copy/Update/Replace/Move/Delete/Get/SetVariables/GetVariables/FindEmptySpace/Print/TakeScreenshot/Export/Generate), implementada como funções Rust que operam numa **transação** do modelo de documento (aplicar em uma cópia/"block" e fazer commit único = 1 passo de undo; rollback em erro). Internamente, cada função pode emitir uma op tipada (`enum DocOp`) — assim a lista de ops existe como camada interna (para undo, colaboração/CRDT e telemetria), e JS é só o front-end para o LLM. Opcionalmente expor também uma ferramenta `apply_ops` JSON para clientes não-LLM.

Detalhes a reproduzir:
- limites: memória (~256 MB), stack, orçamento de CPU por trecho, timeout total (~55 s);
- erros com linha/coluna e trecho da chamada culpada; `editId` + `edits` find/replace para correções baratas;
- resposta com mapa nome→id, `Print`, warnings de schema, imagens de screenshot;
- persistência de globais por conversa (serializar só valores JSON pequenos);
- **streaming**: usar um parser JS incremental (ex.: `oxc_parser`/`swc_ecma_parser` para achar limites de instruções completas) e avaliar instrução a instrução enquanto os deltas do tool-call chegam; commit só na mensagem final.
- `Generate`/`TakeScreenshot`/`Export` como funções async (asyncify no legado; em `rquickjs`, `Promise` + `AsyncRuntime`).

### 5.3 Headless
- Separar o núcleo em crates sem GPUI: `doc-model` (schema `.pen`, serde), `layout` (flex de eixo único — `taffy` serve), `render` (rasterização offline), `ops/execute`. O app GPUI e um binário `openpen-headless` (CLI/preview/servidor) consomem os mesmos crates — análogo ao `HeadlessEditor` + loopback IPC.
- **Renderização sem janela com o mesmo renderer:** o GPUI renderiza via Metal/DirectX/Vulkan (blade) e não oferece render offscreen trivial. Para miniaturas/screenshots/export do agente, ter um **backend de rasterização próprio do canvas** independente do GPUI: `tiny-skia` (CPU, simples, PNG direto) ou `vello` (GPU via wgpu, headless possível) ou `skia-safe` (paridade máxima com o legado, que usa Skia). Para o canvas interativo, desenhar com o mesmo backend numa textura e exibir no GPUI garante pixels idênticos entre canvas, `TakeScreenshot`, export PNG e preview — crucial para o agente "ver" o que o usuário vê.
- Previews: processo filho de baixa prioridade com timeout, escrita atômica e carimbo de mtime (mesmo truque do legado) — ou uma thread pool, já que Rust não tem o custo de inicializar Chromium.

### 5.4 Agente embutido
- Opções: (a) chamar diretamente as APIs (Anthropic Messages com streaming de `input_json_delta`, OpenAI Responses/Chat) com um laço agentic próprio em Rust — controle total, streaming do `execute` garantido; (b) orquestrar CLIs/SDKs existentes (Claude Agent SDK/claude.exe, Codex) como subprocessos apontando para o MCP do app — menos código, herda permissões/sessões, mas sem streaming do `execute` para alguns. O legado faz (b) com quatro SDKs; para um projeto novo, começar com (a) para um provedor + suporte MCP externo cobre a maior parte do valor.
- Reproduzir: system prompt = SKILL.md + regra de mensagem final; `start_task` carregando `execute.md`/`pen-schema.md` sob demanda; injeção do `get_app_state` no primeiro turno; esforço/raciocínio baixo até a 1ª ferramenta; UI de permissão (allow / auto / deny); steering.
- Cursor/flash: portar `AgentCursorMotion` e o envelope do flash (são funções puras pequenas) para o renderer do canvas; cor por perfil de agente; eventos disparados pelas funções do `execute` (escrita e leitura).
- Geração de imagens: definir um trait `ImageProvider`/`SvgProvider`/`ImageTransformProvider` com resultados assíncronos via URL pendente (`pending://…`) reescrita no documento — o padrão "placeholder + polling barato" do legado funciona bem com LLMs.

---

## 6. Incertezas e limites desta análise

- O binário Go foi analisado só por `strings`; a lista de ferramentas e flags é confiável, mas detalhes finos (número de retries, timeouts do lado Go, formato exato do handshake) são inferidos.
- "Arquivos `.pen` são criptografados" vem das instruções do MCP; o código headless lê o documento como UTF-8/JSON. Possivelmente apenas arquivos gerenciados/compartilhados são cifrados, ou a afirmação é dissuasória — não verificado (fora do escopo).
- O bundle do editor é minificado; nomes de funções internas (handlers, rechunker, limites) foram lidos do código minificado e podem ter nuances não capturadas.
- `HeadlessResourceDevice` com `account`/`agents`/`prompt-agent` sugere um host CLI/servidor que roda agentes sem UI, ausente neste pacote.
- O endpoint base do backend pen.dev (`BLOB_STORAGE_URL`, API de geração, proxy de inferência) não foi documentado aqui de propósito; nenhum segredo foi encontrado.
- Nomes de modelos citados no código (ex. probes) refletem a versão analisada e podem não corresponder ao que está disponível publicamente.
