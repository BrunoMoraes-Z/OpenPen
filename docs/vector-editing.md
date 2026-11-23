# Pen tool e edição vetorial

Este documento descreve a ferramenta Pen (P) e o modo de edição de pontos de nós `path` no
OpenPen. O comportamento segue o editor do Pen, lido no bundle minificado
(`legacy/extracted/editor/assets/index.js`: estado de edição de path, sub-ferramentas
`pen`/`pen_move`/`pen_cut`, modelo de contornos e serializador). Nenhum código do legado foi
copiado; os números (tamanhos, raios, limiares) foram conferidos lá.

## 1. Onde está o código

| Parte | Arquivo | Papel |
|---|---|---|
| Geometria pura | `crates/pen-layout/src/vector.rs` | Modelo de contornos/âncoras/handles, parse e escrita de SVG `d`, bounds, hit test de âncoras/handles/segmentos, divisão de Bézier, apagar pontos, alternar canto/suave, restrição de 45°, cortar (`cut_at`) e unir (`join_anchors`). Sem GPUI. |
| Formas como path | `crates/pen-layout/src/vector_shapes.rs` | Contornos editáveis de retângulo, elipse/anel/arco, polígono e linha, como o `getEditableContours` do Pen. O renderer usa o mesmo polígono. |
| Parser SVG | `crates/pen-layout/src/path.rs` | `parse_els` (crate-interno) devolve os elementos e se a entrada foi lida inteira; `parse_svg_path` continua igual para o renderer. |
| Sessão de edição | `apps/openpen/src/canvas_host/vector.rs` | Submódulo do `CanvasHost`: estado da sessão, gestos, escrita no documento (`write_path`, `create_path`, `convert_to_path`), atalhos (Esc, X, Ctrl+J) e desenho do overlay. |
| Sub-toolbar | `apps/openpen/src/toolbar.rs` (`path_toolbar`) | Pen / Select points / Cut / Done ao lado da coluna de ferramentas; estado em `EditorState::path_mode`, `path_cut`, `path_done_requested`. |
| Ganchos | `apps/openpen/src/canvas_host.rs` | `Gesture::Vector`, chamadas em mouse down/move/up/hover, overlay, ações de teclado interceptadas e o conserto do mouse-up perdido (§6). |

## 2. Formato gravado (igual ao Pen)

O Pen guarda um path assim, e o OpenPen grava do mesmo jeito:

- **Criação** (pen tool): nó `path` no frame sob o primeiro clique (ou no topo), com
  `stroke: "#000000"`, `strokeWidth: 1` e sem fill. A geometria fica no espaço do frame pai
  transladado para que o **primeiro ponto seja `0 0`**; `x`/`y`/`width`/`height` são a caixa
  justa (*tight bounds*) da geometria. Exemplo: pontos (100,50), (60,80), (140,90) no mundo
  viram `geometry: "M0 0 L-40 30 L40 40"`, `x: 60, y: 50, width: 80, height: 40`.
- **Escrita**: `M`/`L`/`C`/`Z` absolutos, separados por espaço, números arredondados a 3 casas
  na forma mais curta (`1.5`, `-2`, `0.333`). Um contorno fechado escreve o segmento de volta
  ao primeiro ponto antes do `Z` (`M0 0 L10 0 L10 10 L0 0 Z`).
- **Sem `viewBox` explícito** (o caso normal): a geometria mantém as coordenadas e a caixa do
  nó acompanha os novos bounds. A posição é recalculada para o desenho não sair do lugar,
  inclusive com rotação e flip (`x' = T_antigo(offset) − T_novo(0)`, em que `T` é a
  transformação local do nó). A escala `width / largura do viewBox` é preservada.
- **Com `viewBox` explícito** (ex.: `framegrab.pen`): a caixa e o `viewBox` não mudam; só a
  geometria.

### Ida e volta sem perda

`VectorPath::parse` guarda o texto original de cada contorno. Na escrita, um contorno igual ao
que foi lido sai **byte a byte** como estava (arcos, quadráticas, comandos relativos, `H`/`V`).
Só os contornos editados são reescritos no formato acima. O primeiro `m` relativo de um
contorno vira `M` absoluto (com `l` explícito se houver coordenadas implícitas), já que ele
depende do contorno anterior, que pode ter mudado. Se o texto não puder ser recortado sem
ambiguidade (erro de parse, desenho após `Z` sem `M`), todos os contornos são reescritos.
Entrar e sair do modo sem mexer em nada não grava nada.

Regras de leitura (as do Pen): quadráticas viram cúbicas, arcos viram cúbicas, handles em cima
da própria âncora são descartados, e num contorno fechado cujo último ponto repete o primeiro
os dois se fundem (o handle de entrada passa para o primeiro).

O teste `corpus_paths_round_trip` passa todos os paths do corpus (145) por parse + escrita e
confere que o desenho é o mesmo.

## 3. Pen tool (P)

| Gesto | Efeito |
|---|---|
| Clique | Ponto de canto. O primeiro clique ainda não cria o nó; o segundo cria (um passo de undo). |
| Clique e arrasto | Ponto suave: o arrasto puxa o handle de saída e o de entrada espelha (simétrico). |
| Alt durante o arrasto | Quebra a simetria: só o handle arrastado se move dali em diante. |
| Ctrl durante o arrasto | Volta à simetria. |
| Shift | Restringe o ponto a múltiplos de 45° a partir do ponto anterior (também ao puxar handles). |
| Clique no primeiro ponto | Fecha o contorno; arrastar a partir dele ajusta a curva de fechamento. |
| Enter | Termina o path e sai do modo (o path fica selecionado, ferramenta volta a Move). |
| Esc | Para de desenhar e **continua no modo** com a pen (contornos de um ponto só somem). Os Esc seguintes, como no Pen: limpam a seleção de pontos, trocam a pen por Select points, saem do modo. |
| Ctrl+Z com só um ponto | Descarta o ponto. Depois disso cada ponto é um passo de undo. |
| **Alt**+clique num segmento (sem desenhar) | Insere um ponto ali, sem mudar a forma. O segmento só acende sob o cursor com Alt. |
| Clique num segmento, sem Alt | Começa um novo contorno a partir daquele ponto do segmento (Pen: `StartContour`). |
| Clique numa ponta de contorno aberto | Continua o contorno a partir dela. |
| Clique numa âncora do meio | Começa um novo contorno na posição dela; com Alt, move a âncora. |
| Clique no vazio (editando um path) | Começa um novo contorno no mesmo path (como no Pen). |
| P com um path/forma selecionado | Entra no modo de edição já com a pen (Pen: `onToolChange` para `pen`). |

**Snapping**: o ponto encosta (8 px de tela, como no Pen) nas outras âncoras do path e nas
bordas/centros dos irmãos e do pai, com guias vermelhas. Ctrl desliga. Eixos sem snap são
arredondados para 0,5 (a grade do Pen com `roundToPixels`). Um handle solto em cima da própria
âncora (≤ 4 px) é removido.

Enquanto desenha, uma linha elástica azul de 1 px liga o último ponto ao cursor (usando o
handle de saída, se houver), com um ponto-guia branco de 8 px no destino.

## 4. Modo de edição de pontos

Entrada: duplo clique num `path`, `rectangle`, `ellipse`, `polygon` ou `line` (selecionado, ou
sem outra seleção) ou Enter com um único deles selecionado. Um retângulo com fill de imagem não
entra (no Pen, ele abre o crop da imagem, que o OpenPen não tem).

**Formas viram path na primeira edição** (como o `write` do modelo do Pen, com
`changeShapeType`): os contornos são os do Pen (`vector_shapes`: cantos arredondados com handles
de 0,5523 do raio e raios ajustados como no CSS; elipse começando no topo, anel com o contorno
interno invertido, arco/pizza com o centro; polígono com os vértices na elipse inscrita e cantos
em arcos circulares; linha de (0,0) a (w,h)), no espaço local do nó. Na primeira gravação o nó
é trocado por um `path` com o mesmo `id`, posição na árvore, paints e campos desconhecidos; os
campos que um `path` não tem (`cornerRadius`, `polygonCount`, `innerRadius`…) saem, e a caixa
passa a ser os bounds justos da geometria (um triângulo de 120×120 fica com 103,9×90). Troca +
edição são **um** passo de undo (Delete + Insert + `SetFields` no mesmo bloco). Entrar e sair
sem mexer não muda o nó.

**Sub-toolbar** (Pen: `tools-panel` à direita da coluna de ferramentas): Pen (P), Select points
(V), Cut (X), um separador e Done (↵, ícone X). Botões de 28 px, o ativo com borda.

P e V alternam entre a pen e a seleção de pontos dentro do modo; X liga o corte. Outra
ferramenta, Enter ou Done saem. Duplo clique no vazio também sai.

| Gesto (seleção de pontos, V) | Efeito |
|---|---|
| Clique numa âncora | Seleciona (Shift alterna). Arrastar move os pontos selecionados com seus handles. |
| Arrasto no vazio | Seleção por retângulo (Shift soma à seleção atual). |
| Arrastar um handle | Mantém o tipo do ponto: simétrico continua simétrico, suave mantém o alinhamento (comprimento do oposto preservado), canto move só o handle. Alt quebra, Ctrl força simetria. |
| Clique num segmento | Seleciona as duas pontas e permite arrastar o segmento. |
| Alt+clique num segmento, ou clique no ponto do meio de um segmento reto | Insere um ponto (divisão de Bézier exata) e permite arrastá-lo. |
| Duplo clique numa âncora | Alterna canto/suave (regra do Pen: com dois handles vira canto; com um, ganha o espelho; sem handles, ganha handles simétricos na direção dos vizinhos, ⅙ da distância entre eles). |
| Duplo clique num segmento | Seleciona o contorno inteiro. |
| Delete / Backspace | Apaga os pontos selecionados; os vizinhos continuam ligados. Contornos sem desenho somem; se nada sobrar, o nó é apagado. |
| Ctrl+A | Seleciona todos os pontos. |
| Setas (Shift = 10) | Movem os pontos selecionados. |
| Ctrl+J | Une os pontos selecionados, em pares, na ordem (Pen: `joinSelectedPoints`): as duas pontas de um contorno aberto o fecham; pontas de contornos diferentes viram um contorno só, com segmento reto (os handles das pontas unidas somem); uma âncora do meio começa um contorno novo na posição dela. Pares já ligados são ignorados. Só na seleção de pontos. |
| Esc | Limpa a seleção de pontos; com ela vazia, sai do modo. |

| Gesto (Cut, X) | Efeito |
|---|---|
| Clique numa âncora | Corta ali: um contorno fechado abre (a âncora vira as duas pontas); um aberto se divide em dois. Pontas de contorno aberto não cortam. |
| Clique num segmento | Insere um ponto no lugar exato e corta nele. Um ponto azul de 7 px mostra onde. |
| Depois do corte | Volta para Select points, como no Pen. Um passo de undo. |

Shift ao arrastar pontos trava o eixo dominante. Os pontos movidos fazem snap nas outras
âncoras e nos irmãos.

**Undo**: cada gesto (um arrasto, um ponto colocado com seu handle, um duplo clique, um
Delete, uma seta) é **um** passo, via `begin_gesture`/`end_gesture` sobre `DocOp::SetFields`.
A sessão relê a geometria do nó quando ela muda por fora (undo, redo, agente), então undo e
redo funcionam dentro do modo sem estado extra.

**Custo do arrasto**: cada movimento do mouse recalcula a partir de uma cópia do path tirada no
início do gesto, serializa (microssegundos para paths de ícones), aplica um `SetFields` e roda o
`pipe.update` incremental, o mesmo caminho do move/resize. Não há reconstrução da display list
inteira.

## 5. Aparência (overlay)

Valores do Pen (`#3D99FF` = azul de seleção, `#AAAAAA` = cinza):

| Elemento | Desenho |
|---|---|
| Contorno do path | traço cinza de 1 px |
| Âncora | círculo de 8 px, fundo branco, borda azul 1 px; selecionada: fundo azul, borda branca; sob o mouse: 11 px |
| Handle | linha azul de 1 px até um losango de 9 px (12 px sob o mouse), fundo branco e borda azul |
| Segmento sob o mouse | azul 2,5 px com 40% de opacidade; segmento reto mostra o ponto do meio (8 px, 11 px sob o mouse) |
| Vários pontos selecionados | retângulo azul de 1 px em volta |
| Pen desenhando | linha elástica azul 1 px + ponto-guia branco de 8 px |

Handles aparecem nos pontos selecionados, nos vizinhos deles e no ponto que está sendo
estendido. No modo, a caixa de seleção, os handles de resize e o rótulo de tamanho somem
(no Pen, `hidesSelectionOutline`).

Capturas: `docs/screenshots/pen-tool-*.png` e, desta rodada, `fix-vector-subtoolbar.png`
(retângulo no modo, com a sub-toolbar), `fix-vector-shape-converted.png` (canto arrastado: virou
path, sem campo de raio), `fix-vector-esc-stops-drawing.png` (Esc parou o desenho, a pen continua
ativa), `fix-vector-join.png` (pontas selecionadas e unidas com Ctrl+J), `fix-vector-cut.png`
(elipse cortada com X e a ponta arrastada), `fix-vector-alt-insert.png` (Alt+clique com a pen) e
`fix-vector-polygon.png` (P com o triângulo selecionado; contorno igual ao desenho).

## 6. Conserto: marquee preso com down + up no mesmo frame

O `drag::capture` (que recebe os moves e o mouse-up do arrasto na janela inteira) só existe
depois do próximo render. Se o botão subia antes disso, o mouse-up não chegava a ninguém e o
gesto ficava ativo: o retângulo de seleção ficava preso na tela. Agora o overlay do canvas
registra, a cada frame, um ouvinte de mouse-up na janela que encerra qualquer gesto ativo
(`CanvasHost::gesture_active` + `on_up`). Como `on_up` troca o gesto por `None`, uma segunda
chamada no mesmo evento (o capture, quando já existe) não faz nada.

## 7. Polígonos no renderer (correção)

O `polygon_path` do pen-layout esticava o polígono até ocupar a caixa. O Pen desenha o polígono
com os mesmos contornos da edição (`getEditableContours` no `getFillPath`): vértices na elipse
inscrita, primeiro no topo. Um triângulo numa caixa de 120×120 tem a base a 3/4 da altura, não
na borda de baixo. Agora o renderer usa `vector_shapes::polygon`, e os cantos arredondados são
arcos circulares como no Pen. Com menos de 3 lados nada é desenhado (Pen).

Testes: `vector::tests::cut_opens_or_splits_like_pen` e `join_like_pen` (pen-layout),
`vector_shapes::tests` (contornos das formas), `path::tests::fit_and_shapes` (polígono do
renderer) e `canvas_host::vector::tests::shapes_become_paths_like_pen` (retângulo girado com
raio vira path no mesmo lugar, campos desconhecidos mantidos, undo em um passo; triângulo com a
caixa justa).

## 8. Lacunas

- Uma sessão edita um nó só (o Pen edita vários paths de uma vez com Enter/P).
- Curvar segmento com Ctrl+arrasto, puxar handles com Ctrl+clique numa âncora, seleção de
  segmentos e handles como entidades próprias, resize/rotação da seleção de pontos,
  copiar/colar e duplicar (Ctrl+D) pontos, e a seção Vector do inspector ainda faltam.
- O Cut não muda o cursor e o clique da sub-toolbar em Cut não limpa a seleção de pontos (o X
  limpa).
- O cursor da pen é a cruz padrão (o Pen tem cursores próprios).
- Path dentro de auto layout: `x`/`y` não podem mudar, então a geometria pode deslocar o
  desenho quando os bounds mudam.
