# Edição de fills no inspector

O fill (e o stroke, que usa o mesmo editor) agora edita todos os tipos de paint do Pen que o renderer desenha: cor sólida, gradiente linear, radial e angular, e imagem. Clicar no swatch de uma linha de Fill abre o popover do editor à esquerda do inspector.

| Arquivo | Papel |
|---|---|
| `apps/openpen/src/inspector/fill.rs` | Modelo puro (sem GPUI): `FillType`, `GradientInfo`/`Stop`, `ImageInfo`, `PaintOp`, conversões e edições sobre `pen_schema::Fill`, geometria dos handles no canvas, import de imagem. |
| `apps/openpen/src/inspector/panel.rs` | Popover (abas de tipo, barra de stops, lista de stops, ângulo, picker, aba de imagem), linhas de fill com chip de variável e "Mixed fills". |
| `apps/openpen/src/canvas_host/gradient.rs` | Handles do gradiente no canvas (arrastar pontas, centro, raios e stops, adicionar stop clicando na linha). |
| `apps/openpen/src/scene_document.rs` | `PropEdit::Paint` → `apply_paint_op`: lê a lista do schema, aplica `fill::apply`, grava um `DocOp`. Snapshot (`paints_from_schema`) com stops, geometria, imagem e variáveis resolvidas. |
| `crates/pen-layout/src/assets.rs`, `display.rs`, `pipeline.rs` | Cache de imagens por arquivo (caminho + tamanho + mtime) e `reload_changed_assets`. |

## O que o Pen faz (referência)

Do bundle do editor (`editor/assets/index.js`) e das strings da UI:

- O editor de fill tem abas **Color / Gradient / Image / Shader** (ícones). Gradient tem um segmentado **Linear / Radial / Angular / Mesh**; Image tem **Stretch / Fill / Fit** (`stretch` / `cover` / `contain`) e um campo "Image path or URL" com o botão "Import image file".
- Cada aba lembra o último valor da sessão. Os padrões são: cor `#d9d9d9`; gradiente linear com `rotation 0`, centro `(.5,.5)`, tamanho `(1,1)` e stops `#000000` → `#ffffff`; imagem com `url ""` e `mode cover`.
- A barra de stops tem 24 px de altura, xadrez por baixo, um marcador de 20×20 com borda de 2 px (o selecionado fica com a borda `accent-active`) e um triângulo. Clicar na barra cria um stop com a cor interpolada e já começa a arrastá-lo. A lista "Stops" fica ordenada por posição: campo de % com 56 px, cor e "Remove color stop" (desabilitado com um stop só). O "+" cria o stop na metade entre os dois últimos.
- No canvas, o modo de gradiente desliga os handles de resize. O linear mostra duas pontas; o radial e o angular mostram centro, raio e elipse. O knob do segundo raio fica do lado **oposto** do `ellipsePoint` (`start - ellipse`). Arrastar o centro mantém o handle de raio parado: a elipse gira e muda de tamanho, e a largura acompanha na mesma proporção (alvo `center` do bundle). Arrastar o segundo raio projeta o ponteiro no eixo e grava `size.width = -2·projeção` (passar do centro deixa a largura negativa, como no Pen). Os stops ficam deslocados para o lado da linha. Clicar na linha adiciona um stop. Delete/Backspace remove o stop selecionado. Os pontos encaixam nos 4 cantos e nos 4 meios das bordas da caixa, não no centro (Ctrl desliga).
- O renderer do Pen monta todo gradiente com a matriz de três pontos (`O2n`: centro, ponta do raio e `ellipsePoint`), calculados no espaço normalizado da caixa (girados em torno do centro) e depois escalados por largura e altura. Por isso um radial girado numa caixa não quadrada fica cisalhado.
- O seletor de cor tem um botão de variável (ícone hexágono, "Apply variable" / "Variable: {nome}") que abre a lista do Pen: busca "Search variables...", "None" primeiro (separado por uma linha) e as variáveis de cor ordenadas por nome, com o valor à direita e a atual marcada.
- Arquivos importados são copiados para a pasta do documento como `nome.ext`. Se já existe um arquivo igual, ele é reaproveitado; se o conteúdo é outro, o nome vira `nome-1.ext`, `nome-2.ext`… (`desktop-resource-device.js`, `importFiles`). O `url` gravado é relativo ao `.pen` (no corpus, por exemplo, `instalador/assets/logo.png`).
- Com seleção múltipla e fills diferentes, o Pen mostra "Mixed fills".

## Comportamento no OpenPen

### Troca de tipo (`PaintOp::SetType`)

| De → para | Resultado |
|---|---|
| Sólido → gradiente | A cor atual vira o 1º stop (posição 0). Uma cor ligada a variável (`$nome`) continua ligada. O 2º stop é a mesma cor com alfa 0. |
| Gradiente → outro gradiente | Muda só `gradientType`. Stops, centro, tamanho, rotação, opacidade e campos desconhecidos ficam. Radial/angular ganham `size.width` se faltar. |
| Gradiente → sólido | Usa a cor do 1º stop por posição (com a variável, se houver). |
| Imagem/outros → gradiente | Usa o gradiente padrão do Pen (preto → branco) e mantém `opacity`. |
| Qualquer → imagem | `url ""` e `mode cover`, mantendo `opacity`. |
| Qualquer → sólido sem cor de origem | `#D9D9D9`, o padrão do Pen. |

Em toda troca, `enabled` e `blendMode` são preservados. Um fill desabilitado continua desabilitado. Objetos novos levam `"enabled": true`, como o Pen grava. No linear grava-se só `size.height`, como no corpus (`"size": {"height": 1}`).

Diferença intencional em relação ao Pen: ele restaura o último valor da aba (ou o padrão preto → branco), enquanto o OpenPen converte a cor atual. É o que a tarefa pede e evita perder a cor.

### Gradiente

- **Barra de stops**: clicar adiciona um stop com a cor interpolada e começa a arrastar (tudo num único passo de undo). Arrastar um marcador move o stop. Puxar o marcador a mais de 28 px da barra o deixa esmaecido, e ele é removido ao soltar. A posição é arredondada a 2 casas, como no Pen.
- **Lista**: ordenada por posição, com campo de % (aceita expressões como os outros campos), swatch (com xadrez quando translúcido), hex ou chip de variável, alfa e "−". O "+" segue a regra do Pen.
- **Cor do stop**: o picker (SV + matiz + hex + %) embaixo edita o stop selecionado ou, sem seleção, o primeiro por posição. Mudar a cor de um stop desfaz a ligação com a variável só nesse stop.
- **Ângulo**: o campo "Angle" mostra o ângulo no sentido horário, como no CSS (90° = da esquerda para a direita). No arquivo, `rotation` é anti-horário, com 0 apontando para cima, e a conversão é `display = (-rotation) mod 360`.
- **Delete/Backspace**: com um stop selecionado no popover (o foco vai para o popover) ou no canvas (`EditorState::delete_selection` trata o stop antes dos nós), remove o stop. O último stop nunca é removido.
- **Handles no canvas** (`canvas_host/gradient.rs`): aparecem com o popover aberto num gradiente e um único nó selecionado. Os handles de resize somem e deixam de responder. No linear, as duas pontas definem centro, comprimento (`size.height`) e rotação. No radial e no angular, o centro gira a elipse em volta do handle de raio parado (regra do Pen acima), o handle de raio define `size.height` e a rotação, e o segundo handle (do lado oposto) define `size.width`. Arrastar um stop muda a posição dele. Clicar na linha (linear/radial) cria um stop. Os pontos encaixam nos 8 pontos da borda da caixa (Ctrl desliga). Cada arrasto é um passo de undo (`begin_gesture`/`end_gesture`), com prévia ao vivo pelo `Pipeline::update` incremental. Clicar num handle não fecha o popover (`EditorState::fill_handle_drag`).
- A geometria segue a do renderer (`pen-layout/src/display.rs`): o linear é calculado em espaço normalizado e depois escalado; o angular gira em pixels (`start = -90° - rotation`).

### Imagem

- **Stretch / Fill / Fit** grava `mode` `stretch` / `cover` / `contain`. O formato não tem modo tile. O crop (`transform`) é preservado sem edição.
- **Import image file** abre o diálogo nativo (`cx.prompt_for_paths`) e aceita PNG, JPEG e WebP, que são os formatos que o renderer decodifica. Um arquivo que já está dentro da pasta do documento é referenciado onde está, por caminho relativo com `/`. Um arquivo de fora é copiado para junto do `.pen` com a regra de nomes do Pen (`fill::import_file`). Em documento nunca salvo, grava o caminho absoluto. Trocar a imagem é importar de novo ou editar o campo de caminho/URL.
- A opacidade fica no campo % da linha de fill. Em gradientes e imagens, esse campo é a `opacity` da paint; em sólidos, é o alfa da cor.
- Depois de trocar o `url`, `EditorState::watch_assets` acompanha a decodificação em segundo plano até a imagem aparecer.

### Radial girado (contrato da DisplayList)

`pen_render::Paint::Radial` agora é `{ center, axis_x, axis_y, stops }`: o círculo unitário mapeado
por `center + x·axis_x + y·axis_y` (`Paint::radial(center, radius, stops)` cria o caso alinhado aos
eixos; `radial_to_unit` dá a inversa, `None` quando a elipse colapsa). O pen-layout preenche os
eixos como o Pen: `axis_y` = (−sen r, −cos r)·`size.height`/2 e `axis_x` = (cos r, −sen r)·`size.width`/2
no espaço normalizado, escalados pela caixa. Cada backend:

| Backend | Como |
|---|---|
| pen-raster | `RadialGradient` do tiny-skia com o transform `from_row(axis_x, axis_y, center)` |
| pen-canvas | O GPUI só tem gradiente linear; o radial já era uma textura feita na CPU (`gradient_texture`). `gradient_t` agora inverte a matriz dos eixos. Em paths o item continua indo para o fallback CPU (pen-raster). |
| pen-export SVG/PDF | `<radialGradient cx=0 cy=0 r=1 gradientTransform="matrix(...)">` com a mesma matriz |

Golden novo: `25_radial_rotated` (elipse girada num retângulo e elipse cisalhada numa elipse) no
pen-raster, conferido contra o SVG exportado e rasterizado pelo resvg no pen-export. Nenhum golden
existente mudou (sem rotação, a matriz é a mesma de antes).

### Variáveis, várias seleções e undo

- Um fill sólido ligado a uma variável mostra um chip com o nome no lugar do hex, tanto na linha quanto no popover ("Variable"). Os stops ligados também mostram chip. As cores são resolvidas com o tema do nó (`SceneDoc::resolve_variable`).
- **Ligar e soltar** (`PaintOp::BindColor { stop, var }`): o botão de variável do seletor abre a lista no lugar do corpo do popover; escolher uma variável grava `"$nome"` na cor sólida ou no stop que o seletor edita (o selecionado, ou o primeiro por posição). "None" solta: grava a cor resolvida como literal (`#RRGGBB`/`#RRGGBBAA`), como o `S==null ? resolvido` do campo do Pen. Soltar uma cor literal não faz nada. Um passo de undo; os outros campos do fill/stop são mantidos. A lista vem de `SceneDocument::color_variables` (variáveis `color` do documento, com o valor no tema do nó). Sombras e inputs de script não têm o botão.
- Com vários nós selecionados e listas de fill diferentes, a seção mostra "Mixed fills" ("Mixed strokes" no stroke). O "+" adiciona um fill em todos. Com listas iguais, as edições valem para todos os nós.
- Toda edição é um `PropEdit::Paint { stroke, index, op }` → um `DocOp::set` do campo `fill`/`stroke`, ou seja, um passo de undo. Arrastos (barra, picker, handles) ficam num único bloco. Campos desconhecidos do fill, dos stops, do centro e do tamanho são preservados porque a edição é feita sobre o `pen_schema::Fill` lido do documento.
- Nós dentro de instâncias (sem nó de schema próprio) agora convertem gradientes e imagens resolvidos de volta para o schema (`schema_from_scene_paints`), em vez de descartá-los.

## Foco ao fechar o popover (correção)

Fechar o popover pelo X (ou clicando fora, ou de novo no swatch) removia o elemento que tinha o
foco, e a janela ficava sem foco: Ctrl+Z e os outros atalhos do editor só voltavam depois de um
clique no canvas. O `InspectorPanel` recebe o foco do canvas (`set_return_focus`, ligado pelo
`Editor`) e `close_color_popover` devolve o foco a ele quando o foco estava no popover ou em lugar
nenhum. O mesmo vale quando o popover some porque o alvo sumiu. Os outros popovers e diálogos
(menus `DropdownMenu`/`PopupMenu` e diálogos do gpui-component) já restauram o foco anterior, e o
menu de contexto do canvas já devolvia o foco.

Também corrigido no mesmo arquivo: arrastos do inspector (scrub de campos, SV/matiz do seletor,
stops na barra, reordenar fills) cujo botão subia antes de existir a captura da janela (down e up
no mesmo frame) deixavam o bloco de undo aberto, e a edição seguinte entrava no mesmo passo.
Agora o primeiro movimento sem botão encerra o arrasto. O painel de camadas tinha o mesmo
problema, pior: o próximo clique em qualquer lugar soltava a camada ali (ela saía do frame).

## Cache de imagens (correção)

Antes, o id da imagem (`ImageData::id`) era o hash do `url`, e o builder guardava a imagem por `url` para sempre. Um arquivo alterado no disco mantinha a textura velha no pen-canvas e no pen-raster, que indexam texturas por esse id.

Agora:

- `FsAssets` registra, para cada `url` entregue, o caminho e o `FileStamp` (tamanho + mtime). O id passa a ser `file_image_id(path, stamp)`, e um arquivo novo gera um id novo e, com isso, texturas novas.
- `AssetResolver::changed_images()` (padrão: nada muda) refaz o `stat` dos arquivos vistos, esquece os que mudaram (inclusive no cache diferido) e devolve os `url`s alterados.
- `DisplayListBuilder::reload_changed_assets` / `Pipeline::reload_changed_assets` descartam essas imagens e reconstroem só os chunks que as usam (`image_roots`: url → raízes).
- O app chama `EditorState::reload_changed_assets` quando a janela é reativada (`observe_window_activation` no `Workspace`): editar a imagem em outro programa e voltar atualiza o canvas. Não foi preciso mudar o pen-canvas, que já indexa por `ImageData::id`.

## Testes

- `inspector::fill::tests` (unitários, sem janela): conversões entre tipos (variável preservada, campos desconhecidos, `enabled`), edições de stops (interpolação, posição arredondada, último stop), opacidade e imagem, geometria dos handles conferida contra o renderer (ida e volta em linear, radial e angular), ângulos, nomes de import (reuso, `-1`, arquivo dentro da pasta, documento sem caminho).
- `apps/openpen/tests/fill_editing.rs`: o mesmo caminho do inspector num `SceneDocument`. Sólido com variável → linear e undo em um passo; stops, cor, geometria e opacidade com ida e volta save → load sem perda (campos desconhecidos do fill e dos stops mantidos, snapshot igual após recarregar); arrasto em um passo de undo; imagem (url, fit, opacidade, volta ao sólido); remoção do último stop sem efeito.
- `crates/pen-layout/tests/image_reload.rs`: arquivo reescrito → `reload_changed_assets` → id novo, nos modos síncrono e diferido.

## Verificação no app

Feita no binário release, numa cópia de `testdata/user/maark.pen` em `target/fill-e2e/`. As capturas estão em `docs/screenshots/`:

- `fill-gradient-linear.png`: linear de 3 stops com ângulo 90°, handles no canvas e stop selecionado.
- `fill-gradient-stops.png`: stop adicionado na barra e arrastado, com a lista ordenada.
- `fill-gradient-radial.png`: radial com elipse, centro e raio arrastados ao vivo.
- `fill-gradient-angular.png`: angular girado pelo handle (a costura acompanha o handle).
- `fill-image.png`: imagem importada pelo diálogo, copiada como `photo.png` ao lado do `.pen`, com Fit e 60 %.
- `fill-image-reload.png`: o mesmo arquivo reescrito no disco e recarregado ao reativar a janela.
- `fill-variable.png` / `fill-variable-gradient.png`: chip da variável `surface`, que continua ligada no 1º stop após virar gradiente.
- `fill-mixed.png`: "Mixed fills" com dois nós de fills diferentes.
- `fix-radial-rotated.png`: radial com `rotation: 30` num retângulo de 260×140, desenhado girado e cisalhado, com a elipse dos handles por cima.
- `fix-radial-center-drag.png`: o centro arrastado; o handle de raio ficou no lugar e a elipse girou.
- `fix-focus-undo.png`: popover fechado pelo X e Ctrl+Z logo em seguida (o arrasto acima foi desfeito).
- `fix-variable-picker.png`, `fix-variable-bound.png`, `fix-variable-detached.png`: lista de variáveis no seletor, `brand` ligada (chip na linha e no popover) e solta com "None" (`3366FF` literal).

## Limitações conhecidas

- **Angular**: o renderer gira a varredura em pixels; o Pen usa a mesma matriz de três pontos do radial (cisalhada em caixas não quadradas) e o sinal de `size.width`. Os handles do angular seguem o renderer.
- **Mesh gradient e shader**: aparecem com o nome ("Mesh", "Shader") e podem ser convertidos pelas abas, mas não são editados aqui. O Pen tem editor próprio para os dois.
- **Crop de imagem** (`transform`): é preservado, mas não há modo de crop no canvas. As ações de IA do Pen (remover fundo, vetorizar, aplicar shader) não existem.
- **Variáveis**: a lista abre dentro do popover (no Pen é um popover ao lado do campo); só as variáveis do documento aparecem (não as de bibliotecas importadas); a linha de fill do inspector não tem o botão de variável (só o seletor).
- Cada aba não lembra o último valor da sessão (o Pen lembra). A conversão parte sempre do paint atual.
- Imagens só recarregam ao reativar a janela; não há file watcher como o chokidar do Pen.
