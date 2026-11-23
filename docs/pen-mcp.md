# pen-mcp — servidor MCP do OpenPen

O crate `crates/pen-mcp` expõe o OpenPen a agentes externos (Claude Code, Codex, …) via
[Model Context Protocol](https://modelcontextprotocol.io), usando o SDK oficial em Rust
(`rmcp` 3.11). Referência do desenho legado: `docs/legacy-analysis/04-mcp-headless-agentes.md`
§1 e §5.1.

## Arquitetura

```
 Claude Code / Codex / ...            (cliente MCP, fala stdio)
        | stdio (JSON-RPC MCP, uma mensagem por linha)
        v
 openpen-cli mcp [--socket N] [--agent A]     <- shim: bomba de bytes stdio <-> socket
        | named pipe  \\.\pipe\openpen-<N>     (Windows)
        | unix socket ~/.openpen/sock/openpen-<N>.sock (macOS/Linux)
        v
 SocketServer (dentro do app OpenPen)  -> uma sessão rmcp por conexão
        | ToolBackend (trait)
        v
 app GPUI (pula para a main thread, aplica no documento, responde por oneshot)
 ou backend headless (pen-script, etapa futura)
```

Pontos-chave:

- **`pen-mcp` não depende do motor de documentos.** Ele só fala o protocolo. Quem possui os
  documentos implementa o trait `ToolBackend` (o app GUI e, depois, o backend headless com
  `pen-script`). Isso mantém o crate sem GPUI e sem `pen-doc`.
- Ao contrário do legado (que usa o framing do node-ipc), o socket carrega **JSON-RPC MCP puro**,
  com o mesmo enquadramento do stdio (uma mensagem JSON por linha). O shim é só uma bomba de
  bytes e cada conexão é uma sessão MCP normal do `rmcp`.
- A única extensão é uma **linha de saudação opcional** enviada pelo shim antes do tráfego MCP:
  `{"openpenHello":{"agent":"<nome>"}}`. Ela carrega o `--agent`. Se a primeira linha não for uma
  saudação, ela é tratada como tráfego MCP (clientes que conectam direto funcionam).

### O trait `ToolBackend`

```rust
pub struct CallCtx { pub client_id: String, pub agent_name: String }
pub struct ToolOutput { pub text: String, pub images: Vec<Vec<u8>> /* PNG */, pub is_error: bool }

pub trait ToolBackend: Send + Sync + 'static {
    fn get_app_state(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult>;
    fn execute(&self, ctx: CallCtx, args: ExecuteArgs) -> BoxFuture<'static, ToolResult>;
    fn save(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult>;
    fn agent_connected(&self, ctx: &CallCtx) {}
    fn agent_disconnected(&self, ctx: &CallCtx) {}
}
```

- Os métodos devolvem futures `'static` porque a implementação da GUI vai saltar para a thread
  principal do GPUI e aguardar um `oneshot`.
- `CallCtx.client_id` é único por conexão (`client-1`, `client-2`, …; `stdio` no modo stdio).
  `agent_name` vem, nesta ordem: da saudação do shim (`--agent`), de `McpOptions::agent_name`, do
  `clientInfo.name` do MCP, ou `"agent"`.
- `agent_connected` é chamado uma vez por sessão, na primeira requisição (normalmente o
  `initialize`, quando o nome já é conhecido); `agent_disconnected` quando a sessão termina
  (só se o `connected` tiver sido emitido). Servem para a lista de agentes conectados na UI.
- `uri_to_path` converte o `uri` das ferramentas (caminho ou `file://…`, com escapes `%XX`) em
  caminho do sistema de arquivos; `None` = documento focado.

### Mapeamento de erros

| Resultado do backend | Resposta MCP |
|---|---|
| `Ok(ToolOutput { is_error: false, .. })` | `CallToolResult` com texto e imagens (`image/png` em base64) |
| `Ok(ToolOutput { is_error: true, .. })` | `CallToolResult` com `isError: true` (ex.: erro de script com `editId`) |
| `Err(ToolError::NoDocument / Failed)` | `CallToolResult` com `isError: true` e a mensagem |
| `Err(ToolError::InvalidParams)` | erro JSON-RPC `-32602` |
| `Err(ToolError::Unavailable)` | erro JSON-RPC `-32603` |

Argumentos que violam o schema, combinações inválidas no `execute` (ex.: `input` e `editId`
juntos) e ferramentas desconhecidas viram `-32602` sem chegar ao backend.

## Ferramentas

Os schemas de entrada são gerados com `schemars` a partir das structs em `src/tools.rs`
(descrições escritas por nós). Todas, exceto `read_skill` e `start_task`, aceitam `uri` opcional
(caminho ou URI `file://` do `.pen`; ausente = documento focado).

| Ferramenta | Parâmetros | Quem atende |
|---|---|---|
| `start_task` | `skills: string[]`, `title?` | `pen-mcp` (lê e concatena arquivos de skill, cada um em `<skill path="…">…</skill>`; falhas individuais aparecem inline) |
| `read_skill` | `path?` (padrão `SKILL.md`) | `pen-mcp` |
| `get_app_state` | `uri?` | backend |
| `execute` | `uri?`, `description`, `input?`, `editId?`, `edits[{find, replace, all?}]` | backend |
| `save` | `uri?` | backend (pode ser desligada com `McpOptions::enable_save = false`) |
| `get_style` | `uri?`, `name?`, `params?` | `pen-mcp` (responde que presets de estilo não estão disponíveis) |

As skills são lidas de `McpOptions::skills_dir`. `McpOptions::with_default_skills()` procura em
`$OPENPEN_SKILLS_DIR` e depois em `skills/openpen` ao lado do executável ou em um ancestral
(cobre `target/debug`). A leitura rejeita caminhos absolutos, `..`, prefixos de drive, `:`
(streams alternativos do NTFS) e links simbólicos que escapam da pasta.

O texto de `instructions` do servidor (constante `INSTRUCTIONS`) orienta o agente a carregar as
skills, usar `get_app_state` e editar `.pen` **somente pelas ferramentas**, para que undo e a
visualização ao vivo funcionem.

## Transportes

1. **stdio direto** — `pen_mcp::serve_stdio(backend, opts)`: uma sessão em stdin/stdout. Para o
   modo headless.
2. **Socket local** — `SocketServer::bind(nome, backend, opts)` (dentro de um runtime Tokio) ou
   `BackgroundSocketServer::start(...)` (cria o próprio runtime em threads de fundo; é o que o app
   GPUI deve usar). Aceita vários clientes simultâneos; `shutdown()` encerra todas as sessões.
   Um segundo servidor com o mesmo nome falha (pipe com `FIRST_PIPE_INSTANCE` no Windows; no Unix
   o arquivo de socket só é removido se ninguém responder nele).
3. **Shim** — `run_stdio_shim(nome, agente)`: conecta (5 tentativas, 200 ms de intervalo), envia
   a saudação e copia bytes nos dois sentidos. Sem app rodando, sai com o erro
   `OpenPen is not running: could not connect to …`. Quando o stdin fecha, ainda repassa respostas
   pendentes por até 1 s.

Nomes: `socket_path("desktop")` = `\\.\pipe\openpen-desktop` no Windows e
`~/.openpen/sock/openpen-desktop.sock` nos demais (pasta criada com permissão `0700`). Um nome que
já seja caminho de pipe (`\\.\pipe\…`) ou caminho absoluto é usado como está. O nome padrão é
`desktop`.

Também há `serve_io(backend, opts, client_id, read, write)` para sessões em memória (testes,
agente embutido futuro).

## CLI

```
openpen-cli mcp [--socket NOME] [--agent NOME]   # shim stdio <-> app em execução
openpen-cli mcp --print-config [--socket NOME]   # imprime o trecho de config do Claude Code
openpen-cli mcp --headless arquivo.pen           # TODO: ainda não implementado (erro, código 2)
```

O `--headless` já está no parser; o ponto de extensão está marcado com
`TODO(headless-mcp)` em `apps/openpen-cli/src/main.rs` (montar um `ToolBackend` com `pen-script`
e chamar `serve_stdio`).

## Como conectar o Claude Code (manual, opt-in)

O OpenPen **não escreve** arquivos de configuração de outros aplicativos (o legado reescrevia
as configs de vários clientes a cada inicialização; aqui a instalação é opt-in). Para conectar:

1. Rode `openpen-cli mcp --print-config` e copie o JSON gerado (o mesmo de
   `pen_mcp::config::claude_code_config`):

   ```json
   {
     "mcpServers": {
       "openpen": {
         "type": "stdio",
         "command": "C:\\caminho\\para\\openpen-cli.exe",
         "args": ["mcp"]
       }
     }
   }
   ```

2. Cole a entrada `openpen` em `mcpServers` do `~/.claude.json` (escopo do usuário) ou de um
   `.mcp.json` no projeto. Alternativa: `claude mcp add openpen -- <caminho>/openpen-cli mcp`.
3. Abra o OpenPen antes de usar as ferramentas. Para outra instância, passe
   `--socket NOME` nos dois lados.

## Segurança

- **Só IPC local**: named pipe no Windows (criado com `PIPE_REJECT_REMOTE_CLIENTS`, o padrão do
  `interprocess`) e socket Unix em pasta `0700` nos demais. Não há porta TCP nem HTTP.
- Qualquer processo do mesmo usuário pode conectar e editar documentos abertos — o mesmo modelo
  de confiança de um cliente MCP stdio.
- `read_skill`/`start_task` só leem arquivos dentro da pasta de skills (proteção contra path
  traversal e symlinks) e com no máximo 2 MiB.
- Linhas de entrada são limitadas a 16 MiB (limite do transporte do `rmcp`).
- Nenhum arquivo de configuração de terceiros é criado ou alterado.

## Testes

- `crates/pen-mcp/tests/session.rs`: sessão MCP completa em memória (cliente `rmcp` sobre
  `tokio::io::duplex`) com backend simulado: `initialize`, `tools/list` (schemas), cada
  ferramenta, mapeamento de erros, conteúdo de imagem, skills e path traversal.
- `crates/pen-mcp/tests/socket.rs`: servidor de socket com nome único por teste, dois clientes
  simultâneos, callbacks de conexão/desconexão, shutdown, shim ponta a ponta e erro "not running".
- `apps/openpen-cli/tests/mcp.rs`: o binário `openpen-cli mcp` contra um servidor de socket
  criado no teste, `--print-config` e `--headless`.
