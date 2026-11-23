# Importar página web (app e MCP)

Uma página web vira **um frame de topo** do documento, com camadas editáveis (frames, textos,
imagens e vetores posicionados como na página renderizada). O motor de captura e conversão fica
em `crates/pen-import` (Edge/Chrome headless via DevTools Protocol); este documento cobre a parte
do app (`apps/openpen/src/web_import/`), da ferramenta MCP `import_website` e do modo headless.

Referência do legado: `docs/legacy-analysis/03-processo-principal-electron.md` §10 (importação
pelo `<webview>`) e `04-mcp-headless-agentes.md` §1.5 (`browser` e `import_website`). No Pen a
importação de desktop passava pelo navegador embutido e a ferramenta `import_website` rodava nos
servidores do pen.dev. Aqui tudo roda localmente, com o navegador instalado na máquina.

## Privacidade

- O navegador sempre abre com um **perfil temporário novo**, apagado no fim (sucesso, erro,
  timeout ou cancelamento). Cookies, logins e extensões do usuário nunca são usados, então
  páginas que exigem login não são importáveis.
- Nada sai da máquina além das requisições que a própria página faz.
- O navegador roda sem janela (`--headless=new`) e é encerrado com a árvore de processos
  (`taskkill /T` no Windows) quando a importação termina ou é cancelada.

## No app

**Abrir**: botão de globo na ilha direita da barra de abas (dica "Import a web page onto the
canvas"), ☰ › File › **Import Web Page…**, menu nativo File (macOS) ou a ação
`openpen::ImportWebPage`. Sem documento aberto, uma notificação pede para abrir um.

**Diálogo** (`web_import/dialog.rs`, no estilo dos diálogos do Pen: título de 16 px, 460 px de
largura, botões arredondados de 32 px, controle segmentado como o do chat):

- **URL**: normalizada por `pen_mcp::normalize_web_url` (a mesma regra da ferramenta MCP):
  espaços nas pontas removidos, `https://` acrescentado quando falta o esquema (`http://` para
  `localhost`, IPv4 e `[IPv6]`, como no legado), só `http`/`https` (e `file://` para páginas
  locais), host obrigatório, `exemplo` sem ponto é recusado. Erros aparecem em vermelho embaixo do
  formulário; Enter em qualquer campo inicia.
- **Viewport**: Desktop 1440×900, Tablet 768×1024, Mobile 390×844 ou Custom (largura digitada,
  200–4096 px, altura 900).
- **Full page**: ligado importa a página inteira (com rolagem para carregar conteúdo
  preguiçoso); desligado só a primeira tela.
- **Advanced**: seletor CSS opcional (importa só o primeiro elemento que casar) e
  **Infer auto layout** (refaz linhas/colunas flex simples como auto layout; desligado por
  padrão).

**Execução**: `web_import::spawn` roda o `pen_import::import_url` numa thread própria com um
runtime Tokio multi-thread (o lançamento do navegador bloqueia; a UI nunca espera). O progresso
(`ImportProgress` → `Stage`) chega por canal e o diálogo mostra etapa, barra e segundos
decorridos. A barra só avança (`ProgressState::advance` ignora eventos atrasados de etapas
anteriores); downloads avançam dentro da faixa deles (`n/total`).

**Cancelar**: o botão Cancel, Esc ou fechar o diálogo derrubam o `CancelGuard`. A thread
seleciona entre o cancelamento (verificado primeiro) e a importação; ao cancelar, o future da
importação é descartado, e o `Drop` do navegador no pen-import mata o processo e apaga o perfil.
Clicar fora do diálogo não fecha (para não cancelar sem querer uma importação longa).

**Inserção** (`web_import::insert_page`): o frame passa pelo mesmo caminho do `execute` dos
agentes. `pen_script::insert_at_free_spot` gera um script que faz `JSON.parse` do frame,
`FindEmptySpace({width, height, padding: 100})` e `Insert(document, frame)`. Por isso os ids são
gerados pelo motor, a inserção é **um passo de undo** e um erro não deixa nada para trás. Depois
`EditorState::run_agent_script` atualiza painéis e canvas e faz o **flash de agente** no frame
(um anel só, porque os filhos ficam cobertos pelo ancestral), com o cursor "Web import" em
verde-azulado. O frame fica selecionado, o canvas dá zoom nele (`fit_selection_requested`) e o
`watch_assets` acompanha a decodificação das imagens. O nome do frame é o título da página quando
o conversor não deu nome.

**Resultado**: notificação de sucesso ("Imported “título”.") e, se houver avisos, uma segunda
notificação de aviso com até 4 deles e a contagem do resto.

**Erros** (`web_import::Failure`, com título e dica):

| Caso | Mensagem |
|---|---|
| Sem navegador | "No browser found", pede para instalar Edge, Chrome ou Chromium |
| Falha de navegação | "The page could not be loaded" + erro do navegador (ex.: `net::ERR_NAME_NOT_RESOLVED`) |
| Timeout (120 s) | sugere desligar "Full page" ou usar um seletor |
| Documento fechado, inserção recusada | mensagem do motor |

O estado de erro tem **Back** (volta ao formulário com os mesmos valores) e **Close**.

## Documento não salvo

**Decisão: é preciso salvar antes.** As imagens e fontes vão para arquivos ao lado do `.pen`
(`<pasta do .pen>/images/<site>/` e `fonts/<site>/`, nomes por hash do conteúdo) e os fills
referenciam esses arquivos por caminho relativo. Um documento sem caminho não tem pasta; usar uma
pasta temporária deixaria referências quebradas quando o usuário salvasse em outro lugar. Então:

- no diálogo, um aviso explica o motivo e oferece **Save As…** (abre o diálogo de salvar da
  própria aba, `Workspace::save_as`); depois de salvar, o aviso some e Import funciona;
- na ferramenta MCP, a chamada falha com a mesma explicação (o agente não abre diálogos; ele
  pede ao usuário para salvar).

## Ferramenta MCP `import_website`

```
import_website {uri?, url, width?, height?, fullPage?, selector?}
```

- Schema e descrição em `crates/pen-mcp/src/tools.rs` (`ImportWebsiteArgs`, texto nosso). As
  anotações marcam `openWorldHint: true` (acessa a web), não destrutiva, não idempotente.
- `pen-mcp` valida antes de chamar o backend: URL (`normalize_web_url`), `width`/`height` entre
  200 e 4096, `selector` não vazio. Argumentos inválidos viram JSON-RPC `-32602`.
- Roteamento: método novo `ToolBackend::import_website(ctx, args, url)` com implementação padrão
  "This OpenPen server cannot import web pages." (`isError`). `pen-mcp` continua sem depender
  do `pen-import`.
- **Resposta** (`pen_mcp::ImportedPage::to_output`, igual na GUI e no headless): texto com o id do
  frame criado, título, tamanho, estatísticas (camadas, textos, imagens, vetores, elementos lidos,
  tempo), número de arquivos salvos, avisos (até 30) e uma orientação para usar `Get` com limite de
  profundidade e `TakeScreenshot` antes de refinar; mais a **screenshot do viewport** da página
  original como conteúdo `image/png`, para o agente comparar.

### GUI (`agent/backend.rs` + `agent/hub.rs`)

O `GuiBackend::import_website` não ocupa o despachante durante a importação (que pode levar um
minuto): (1) `AgentRequest::ImportTarget` resolve o documento no foreground como as outras
ferramentas (`uri` ou focado, abrindo o arquivo se preciso) e devolve o caminho (erro se não
salvo); (2) a importação roda em `web_import::spawn`, fora do foreground, enquanto outras
chamadas MCP continuam sendo atendidas; (3) `AgentRequest::ImportInsert` insere o frame no
documento daquele caminho com `insert_page`, com a cor e o cursor do agente, seleção e zoom. Se o
cliente desistir da chamada, o future é descartado e o navegador morre junto.

### Headless (`openpen-cli mcp --headless arquivo.pen`)

`HeadlessBackend::import_website` (`apps/openpen-cli/src/headless.rs` +
`apps/openpen-cli/src/web_import.rs`): mesma validação de `uri`, importação numa thread com
runtime próprio (cancelável pelo drop), inserção na thread do documento com
`insert_at_free_spot` (um passo de undo) e a mesma resposta. Os assets vão para a pasta do
arquivo servido. Com `--readonly` a ferramenta responde erro, porque importar grava arquivos.

### Tempo de espera

A ferramenta leva de 5 a 90 s. O servidor MCP não tem limite próprio por chamada. O chat embutido
com o motor Claude Code define `MCP_TOOL_TIMEOUT=600000` no processo do `claude` (se o usuário
não definiu outro); o motor da API espera o future sem limite. A importação em si tem limite de
120 s.

## Chat e skills

- `pen_agent::pen_tool_defs` deriva de `pen_mcp::tools::tool_list`, então `import_website` entrou
  na lista de ferramentas do motor da API; o `BackendExecutor` encaminha para
  `ToolBackend::import_website`. O motor Claude Code já libera `mcp__openpen__*`.
- O prompt do chat cita a ferramenta, e `skills/openpen/SKILL.md` ganhou a seção "Starting from a
  live web page" (quando usar, que o resultado é um retrato achatado a refinar, `Get` e
  screenshot depois).

## Stub para testes

`OPENPEN_WEB_IMPORT_STUB=1` (app e CLI) troca o navegador por uma página fixa ("Example Domain")
com progresso simulado. No app, `ImportRequest::stub` permite escolher por requisição nos testes
(a variável de ambiente só define o padrão).

## Testes

- `crates/pen-mcp`: normalização de URL (esquemas, localhost/IP, `mailto:`, espaços, host
  incompleto), validação de argumentos e nomes no fio (`fullPage`), schema/anotações da
  ferramenta, formatação da resposta; sessão MCP completa com backend simulado (roteamento, imagem,
  erro do backend como `isError`, argumentos inválidos sem chegar ao backend) e backend sem suporte.
- `crates/pen-agent`: lista de ferramentas do motor da API e encaminhamento no `BackendExecutor`.
- `crates/pen-script/tests/place.rs`: inserção num ponto livre como um passo de undo, ids gerados,
  texto com aspas, `U+2028` e `</script>` preservado, undo restaurando os bytes; frame inválido não
  muda nada.
- `apps/openpen/src/web_import` (unitários): presets e largura custom, máquina de progresso
  monotônica, textos de erro, argumentos MCP → requisição, nome do frame, cancelamento antes de
  iniciar.
- `apps/openpen/tests/web_import.rs` (sem janela): página stub como um passo de undo à direita do
  conteúdo, um flash só, undo/redo; duas importações não se sobrepõem; frame inválido não muda
  nada; job stub com progresso e cancelamento; **navegador real** (pula sem Edge/Chrome) com
  página servida em `127.0.0.1` pelo próprio teste.
- `apps/openpen-cli/tests/headless.rs`: binário real `mcp --headless` com o stub (`tools/list`,
  inserção, `uri` errado, URL inválida `-32602`, `save`), `--readonly` e **navegador real** (pula
  sem navegador) com página e imagem servidas localmente: texto importado, imagem gravada em
  `images/` ao lado do documento e screenshot na resposta.

## Lacunas

- As fontes web baixadas (`ImportResult::fonts`) não entram na lista `fonts` do documento: não
  há `DocOp` para isso no pen-doc. O texto usa a família pelo nome (fontes do sistema quando
  existirem) e os arquivos ficam em `fonts/<site>/`.
- Sem importação para documento não salvo (ver acima).
- `inferLayout` não é parâmetro da ferramenta MCP (o schema segue o combinado); só o diálogo
  oferece.
- O diálogo não mostra a screenshot original; o agente recebe, o usuário não.
- Uma única importação por diálogo; importações MCP simultâneas rodam em paralelo (cada uma com
  seu navegador).
