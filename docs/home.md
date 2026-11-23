# Home (dashboard)

A aba Home é o painel de documentos recentes, como o *Dashboard* do Pen
(`legacy/extracted/editor/assets/index.js`, componentes do `dashboard`, e
`legacy/extracted/main/{dashboard,dashboard-documents,preview-queue,recent-files}.js`).
Código: `apps/openpen/src/home/` (view + `model.rs` + `files.rs`),
`apps/openpen/src/recent.rs` e `apps/openpen/src/preview.rs`.

## Layout (igual ao Pen desktop)

No app desktop o Pen renderiza o dashboard sem cabeçalho próprio
(`showHeader: false`) e com `topInset` = altura da barra de abas (50 px).
Medidas usadas:

| Elemento | Pen (Tailwind) | OpenPen |
|---|---|---|
| Fundo | `bg-canvas-bg` | `Palette::canvas` (#f6f6f6 / #18181b) |
| Conteúdo | `px-5 pb-6`, `paddingTop = topInset + 10` | 20 px laterais, 60 px no topo, 24 px embaixo |
| Barra da seção | `mb-3`, abas em pílula `h-7 rounded-full px-3 text-xs font-medium` | aba única "Recents" (não há rascunhos nem kits embutidos) |
| Alternar grade/lista | `rounded-lg bg-foreground/5 p-0.5`, botões `size-6`, ativo `bg-background shadow-sm` | idem; a escolha fica em `home-layout.txt` |
| Grade | `grid-template-columns: repeat(auto-fill, minmax(280px, 1fr))`, `gap-5` | `model::grid_columns` (mesma regra, calculada pela largura da janela) |
| Cartão | `rounded-xl border bg-background`; preview `aspect-[16/10] bg-muted/70`, `p-4` quando há imagem; rodapé `h-[60px] p-3 border-t bg-card` | idem; seleção = borda azul + anel de 1 px (`blue-500`, dark `blue-400`) |
| Título do cartão | ícone de arquivo 12 px, `pasta / ` em `muted-foreground/70`, nome `font-semibold 12px` | idem |
| Subtítulo | `11px muted-foreground/70`: "Edited 5m ago" / "Opened 5m ago" | idem (formato "narrow" do `Intl`: now, 5m, 3h, yesterday, 4d, depois data média) |
| Botão "…" | `size-6 rounded-md bg-foreground/5`, só no hover ou selecionado | idem |
| Sem preview | contorno de "pena" 36×46, `#D0D0D0` / `#404040` | desenhado com `PathBuilder` |
| Lista | colunas `minmax(0,1fr) 140px 140px 24px`, cabeçalho `10px semibold uppercase`, linha `py-2.5 text-[13px]`, miniatura 64×44 | idem; ordenação clicando no cabeçalho (Name A→Z, datas mais novas primeiro; clicar de novo inverte) |
| Vazio | `rounded-lg border-dashed py-20 text-sm` "No documents here yet" | idem + dica de criar/abrir/soltar |
| Arrastar arquivo | overlay `bg-foreground/50 border-2 border-primary/50` + "Drop .pen file to open" | idem |

Diferenças deliberadas:

- **Ações New File / Open File e busca**: no Pen desktop "New" fica na barra
  de abas e não há busca no dashboard. Aqui a barra da seção tem, à direita,
  busca (filtra por nome e pasta, todos os termos, sem diferenciar
  maiúsculas), o alternador grade/lista, "Open File" (ghost) e "New File"
  (primário `bg-foreground text-background`), copiados do cabeçalho do
  dashboard do Pen (`dashboard.header.*`).
- **Arquivos inexistentes**: o Pen os esconde. Aqui continuam na lista com
  "File not found" (âmbar), ícone de arquivo riscado e opacidade reduzida;
  abrir oferece "Remove from Recents".
- Sem o compositor de prompt do agente (fora do escopo).

## Interação

- Clique seleciona (Ctrl alterna, Shift seleciona intervalo); **duplo clique
  ou Enter abre** (como no Pen); clique no fundo limpa a seleção.
- Teclado (com o foco na Home): setas navegam na grade (cima/baixo pulam uma
  linha; na última linha curta, baixo vai para o último cartão), Home/End,
  Enter abre, Esc fecha o menu ou limpa a seleção, Ctrl+A seleciona tudo,
  Ctrl+F vai para a busca, F2 renomeia, Delete remove dos recentes, tecla
  Menu abre o menu de contexto. A rolagem acompanha o cartão focado.
- Menu de contexto (clique direito ou "…"): Open, Show in Explorer, Rename…,
  Duplicate, Remove from Recents (plural "Remove N from Recents"), Move to
  Recycle Bin (vermelho). Com vários selecionados, só as remoções.
  - *Show in Explorer*: `explorer /select,"<caminho>"`.
  - *Rename*: diálogo com o nome selecionado sem a extensão (como o Pen);
    valida nomes do Windows (separadores, reservados como `CON`, ponto ou
    espaço no fim), acrescenta `.pen` se faltar, não sobrescreve; move a
    entrada dos recentes e o preview em cache. Documentos abertos em aba
    precisam ser fechados antes.
  - *Duplicate*: copia para `nome copy.pen` (`copy 2`, …) e põe a cópia no
    topo dos recentes.
  - *Move to Recycle Bin*: confirmação nativa e `SHFileOperationW` com
    `FOF_ALLOWUNDO` (recuperável pela Lixeira). Fora do Windows, recusa.
- Soltar arquivos `.pen` em qualquer lugar da janela abre cada um numa aba;
  outros tipos geram "OpenPen can only open .pen files.".

## Recentes (`recent.rs`)

`%APPDATA%/OpenPen/recent-documents.json` (ou `$OPENPEN_DATA_DIR`), formato
`{"documents":[{"path", "openedAt"}]}` como o `recent-documents.json` do Pen;
até 200 entradas, sem duplicatas (comparação sem caixa no Windows), escrita
atômica. Lê e migra o antigo `recent.txt`. O menu "Open Recent" mostra os 14
primeiros que existem. JSON inválido ou entradas vazias/duplicadas são
descartados na leitura.

A Home copia a lista, faz `stat` de todos os arquivos numa tarefa de fundo e
só então mostra tempos de edição / "File not found"; antes disso os cartões
já aparecem com nome e "Opened …".

## Previews (`preview.rs`)

- `PreviewPool`: threads próprias (`openpen-preview`) com prioridade de fundo
  (`THREAD_MODE_BACKGROUND_BEGIN`, CPU e E/S), no máximo
  `min(3, núcleos/2)`, criadas sob demanda. Cada pedido substitui a fila
  (ordem = "last used"), sem repetir o que já está em andamento.
- Por documento: se `previews/<id>.png` existe e está "carimbado" com o
  mtime do documento (comparação por segundo, como `floor(ms/1000)` do Pen),
  só decodifica; senão carrega com pen-doc, faz layout com pen-layout
  (scripts e shaders incluídos), rasteriza com pen-raster (contexto por
  thread, sem disputar o lock do canvas) cabendo em 512 px e grava
  `.tmp` + `set_modified(mtime)` + `rename` (3 tentativas a cada 100 ms).
- `id` = FNV-1a 64 bits do caminho normalizado (estável entre execuções).
- Falha (JSON inválido, pânico do renderer) ou documento vazio ⇒ PNG 1×1
  transparente carimbado: não tenta de novo até o arquivo mudar; a Home mostra
  o placeholder.
- A Home só pede previews ausentes ou mais velhos que o arquivo; a imagem
  antiga continua visível até a nova chegar. As imagens chegam por um canal e
  viram `RenderImage` (BGRA) ainda na thread de trabalho.
- Ao renomear/apagar pelo menu, o preview em cache é movido/removido.

## Medidas

App release com 20 recentes (19 cópias do corpus em `target/home-e2e/docs`
+ 1 inexistente), `OPENPEN_DATA_DIR=target/home-e2e/data`, máquina de
desenvolvimento (log `%TEMP%/openpen.log`, linhas `home:`):

| Etapa | Tempo |
|---|---|
| Primeiro frame da Home após criar a view | 0,6–0,8 ms (cartões com nome e "Opened …") |
| `stat` dos 20 arquivos (thread de fundo) | 13–14 ms (2,7 ms com cache do SO) |
| 19 previews gerados do zero (cache vazio) | 180–310 ms no total; 6–90 ms cada (`pencil-demo.pen` é o maior) |
| 19 previews lidos do cache (PNG carimbado) | 9 ms no total; 1–2 ms cada |
| Teste `corpus_previews` (release) | 11–134 ms por documento |

A janela fica visível ~1,75 s após o processo iniciar (inicialização do
GPUI/fontes/MCP, igual com ou sem recentes); a Home nunca espera previews.

Capturas: `docs/screenshots/home-grid.png`, `home-missing.png`,
`home-rename.png`, `home-empty.png` (claro) e `home-dark-grid.png`,
`home-dark-menu.png`, `home-dark-keyboard.png`, `home-dark-list.png`,
`home-dark-search.png`, `home-dark-open.png` (escuro). Roteiro:
`home_e2e.py` (copia o corpus para `target/`, grava os recentes, roda uma
cópia renomeada do executável e captura a janela).

## Lacunas

- Sem caixa de seleção por arrasto (o Pen tem *box select* na grade).
- Sem virtualização: com 200 recentes todos os cartões são montados (as
  imagens ficam na memória: ~1 MB por preview de 512 px).
- Sem *timeout* de render (o Pen mata o processo após 120 s); uma thread
  presa num documento patológico fica ocupada até terminar.
- "Remove from Recents" não apaga o PNG do cache (fica órfão; é pequeno).
- Previews de documentos abertos usam o arquivo salvo em disco, não o estado
  em memória.

## Testes

- `recent::tests`: dedupe/limite, remover, renomear, JSON (ida e volta,
  lixo), migração do `recent.txt`, caminhos do Windows sem caixa.
- `home::model::tests`: subtítulos, tempos relativos, busca, ordenação,
  geometria da grade, navegação por teclado, nomes de arquivo, nome da cópia.
- `home::files::tests`: renomear (sem sobrescrever, só caixa) e duplicar;
  `trash_moves_the_file_away` é `#[ignore]` (deixa um arquivo na Lixeira).
- `preview::tests`: carimbo por segundo, ids, geração + reuso + invalidação
  por mtime, documento quebrado/vazio/inexistente, pool com 2 threads,
  previews de cópias do corpus em `target/tmp/` (pula sem `testdata/`).
