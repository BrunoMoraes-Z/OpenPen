# Nós `script` e shaders

Como o OpenPen executa os nós `script` (geradores procedurais em JavaScript) e os shaders GLSL
(`fill`/`stroke`/`effect` do tipo `shader`). Semântica do legado: `docs/legacy-analysis/01-formato-pen.md`
§12 e `02-editor-e-renderizacao.md` §11–12, conferida no bundle minificado (`ScriptNodeRunner`,
`prepareScriptOutput`, transpilador GLSL→SkSL).

## Visão geral

```
SceneDoc ──layout──► ScriptExpander (pen-layout) ──ScriptRunner──► pen-script (QuickJS)
    ▲                       │ set_generated(script, nós)
    └───────────────────────┘ (subárvore gerada, nunca salva)

DisplayListBuilder ──ShaderRenderer──► pen-shader: GLSL → naga ──► GPU (wgpu headless)
        │                                                     └──► CPU (intérprete de IR naga)
        └── Paint::Image (fill) / item de imagem no lugar do nó (effect)
```

Nenhum backend conhece scripts ou shaders: o canvas GPUI, o pen-raster (PNG) e o pen-export
(SVG/PDF) só veem nós comuns e imagens. pen-layout não depende de QuickJS nem de naga/wgpu:
os dois motores entram por traits (`ScriptRunner`, `ShaderRenderer`) plugadas pelo app, pela CLI
e pelo headless:

```rust
pipe.set_script_runner(Box::new(pen_script::ScriptNodeRunner::new()));
pipe.set_shader_renderer(Arc::new(pen_shader::ShaderEngine::new()));      // CLI/headless: síncrono
pipe.set_shader_renderer(Arc::new(pen_shader::ShaderEngine::deferred())); // app: thread de trabalho
pipe.rebuild(&mut scene);
```

Sem motor plugado, o script desenha o placeholder cinza tracejado de antes e shaders são ignorados.

## 1. Nós `script`

### Contrato (igual ao legado)

- Arquivo `.js` em `scriptUri`, relativo ao `.pen`. Cabeçalho `/** ... */` obrigatório com
  `@schema <versão>` ("Missing @schema version tag" sem ele) e `@input nome: tipo[(args)] [= default]`.
  Tipos: `number(min=, max=)`, `string`, `boolean`, `color`, `ref`, `enum("a", "b")`. O parser do
  cabeçalho (lexer + diretiva) reproduz as mensagens de erro do legado.
- O corpo roda como `((pencil) => { corpo })({width, height, input})`; precisa `return [nós]`.
  Só os inputs declarados são passados; faltando, vale o default. `number`: número, ou
  `parseFloat(String(v))`; NaN vira `min ?? 0`; clamp em min/max. `enum` fora das opções é erro
  ("Invalid input value"). `$variáveis` nos `inputs` são resolvidas pelo pen-doc antes.
- `Math.random` = mulberry32 com seed 0 reiniciado a cada execução (determinístico).
- Limites: 16 MiB de heap, 512 KiB de pilha, 2 s (interrupt handler), 1000 nós na saída.
  Saída precisa ser array; `reusable` e `connection` são rejeitados; ids descartados; migrada de
  `@schema` até 2.22 pela mesma cadeia do loader.
- Erros (compilação, execução com trecho `> linha: código`, timeout, memória, limites, arquivo
  ausente) viram um placeholder vermelho tracejado com título e detalhes desenhados dentro do nó.

### Onde a saída vive

- `SceneDoc::set_generated(script, &[Node], tag)` constrói os nós como filhos resolvidos do script
  (variáveis, temas, `ref` para componentes do documento, `descendants`) com
  `Origin::Generated { script }`. Eles **não** entram em `children` do script nem em `by_id`:
  layers, seleção, agentes (`Get`) e o save não os veem; `id_path`/`node_ref` devolvem `None`
  (não editáveis); `generated(k)`, `generated_owner(k)` e `script_error(k)` dão acesso.
- Layout: o nó script é uma folha no taffy (o tamanho não depende da saída). Cada nó gerado de
  topo é diagramado como raiz própria, posicionado no `x/y` dentro do script (como um frame com
  `layout: none`); frames gerados com flex funcionam normalmente.
- DisplayList: o script desenha a saída como filhos, com `clip` se o nó pedir.
- Hit-test no app: pelo retângulo do próprio script (clicar na saída seleciona o script). Conteúdo
  gerado fora do retângulo, sem `clip`, não é clicável.

### Quando roda (`ScriptExpander`, pen-layout)

Depois de cada passo de layout (`Pipeline::update`/`rebuild`), para os scripts dentro das raízes
re-diagramadas e os scripts sujos: chave = hash(conteúdo do arquivo, inputs, largura, altura).
Se a chave anexada ao nó (`generated_tag`) é a mesma, nada acontece; senão a saída vem do cache
(128 entradas) ou o script roda; o nó é marcado e as raízes afetadas são diagramadas de novo no
mesmo `update`. Consequências:

- mover a página, mexer em outros nós ou desfazer/refazer não roda JavaScript;
- reconstruir a raiz (editar um componente usado pela saída) re-anexa do cache, re-resolvendo os
  `ref`s com o componente novo;
- o arquivo é relido quando muda (mtime/tamanho, verificado só para scripts afetados);
  `Pipeline::refresh_scripts` reverifica todos (ex.: ao voltar o foco do app).

### Inputs no inspector

Com um nó `script` selecionado, o inspector mostra a seção **Script**, como a do Pen
(`properties.script.*` no bundle): o caminho do arquivo, uma linha por `@input` do cabeçalho
(rótulo de 80 px + editor) e o botão "Reload script" (ícone de recarregar no título).

| Tipo | Editor (igual ao Pen) | Gravado em `inputs` |
|---|---|---|
| `number(min, max)` | campo numérico com `#` para arrastar (passo 0,1; Shift ×10), aceita expressões; valor limitado a `min`/`max` | número |
| `color` | swatch + hex; abre o seletor de cor do inspector | `"#RRGGBB"` / `"#RRGGBBAA"` |
| `boolean` | checkbox | `true`/`false` |
| `enum("a", "b")` | select com as opções | a string |
| `string` | campo de texto | a string |
| `ref` | campo de texto com o id (o Pen abre um seletor de componentes) | a string |

- O valor mostrado é o que o script recebe: o guardado no nó (variáveis resolvidas) ou o default
  do cabeçalho; números já limitados. Um valor ligado a variável mostra o chip; editar troca a
  ligação pelo valor.
- Cada edição grava o mapa `inputs` inteiro num `DocOp` (um passo de undo; um arrasto do `#` é um
  passo). A ordem e os outros inputs ficam, e só o input editado muda. O script roda de novo pelo
  caminho normal (a chave do cache inclui os inputs).
- Erro no cabeçalho (ou arquivo ausente) aparece em vermelho no lugar das linhas.
- "Reload script" chama `Pipeline::refresh_scripts` (via `SceneDocument::refresh_scripts`) e relê o
  cabeçalho. O mesmo acontece quando a janela volta a ficar ativa (o gancho de
  `observe_window_activation` das imagens chama `EditorState::reload_changed_assets`, que agora
  também atualiza os scripts): editar o `.js` em outro programa e voltar atualiza a saída e os
  inputs.
- Modelo puro em `apps/openpen/src/inspector/script.rs` (`script_props`, `input_scalar`,
  `parse_number`); UI em `inspector/panel/script_ui.rs`; escrita em
  `SceneDocument::set_script_input`.
- Difere do Pen: inputs `number` com faixa usam campo, não slider (no Pen o slider é dos uniforms
  de shader; o de script é campo). O campo de caminho é só leitura (o Pen permite trocar e
  importar o arquivo) e "Convert to layers" ainda não existe.

Capturas: `docs/screenshots/fix-script-inputs.png` (9 barras, `layout` = rows, `rounded`
desligado, cor escolhida no seletor) e `fix-script-reload.png` (input `gap` novo depois de editar
o `.js` e clicar em "Reload script").

O `ScriptNodeRunner` (pen-script) guarda até 16 scripts compilados (um runtime/contexto QuickJS
cada, função de wrapper como global): re-executar com outro tamanho é só uma chamada.

A execução é síncrona (como no legado, que roda numa fila no mesmo thread): um script de 2 s
trava o `update` por 2 s. Scripts típicos custam < 1 ms.

### Números (release, `cargo bench -p pen-script --bench node_script`, gráfico de 60 barras ≈ 90 nós)

| Caso | Tempo |
|---|---|
| passo de redimensionar (patch de largura + script + build da cena + relayout + chunk) | 0,96 ms |
| redimensionar para tamanhos já vistos (cache, sem JS) | 0,24 ms |
| execução a frio (parse do cabeçalho + runtime/contexto + compilação + execução) | 0,59 ms |

Os 11 scripts de exemplo do legado (clock, chart, gauge, radar, flow-field, circle-text…) rodam
com os defaults e geram saída (teste `legacy_goodies_run`, só com `legacy/` presente).

## 2. Shaders

### Abordagem e por quê

O GPUI 0.3 (gpui-kit) não expõe pipelines próprios (o renderer Windows é DirectX 11 interno;
`paint_quad`/`paint_image`/paths são o limite), então shader não roda dentro do canvas. Os
shaders são **renderizados fora da tela em uma imagem** no espaço local do nó, e o builder da
DisplayList desenha essa imagem — como imagem de fill, ou no lugar do nó para efeitos. Assim o
mesmo resultado vale para o canvas, o PNG/export e o headless.

1. **Front-end**: o GLSL do Pen (WebGL 1.0 + `textureSize`) é reescrito token a token para GLSL
   4.50 e entregue ao front-end GLSL do **naga** (sem copiar o transpilador do legado):
   `uniform` vira global privada preenchida pelo host; `sampler2D` vira `int` (slot de textura,
   o que permite passar samplers para funções); `texture2D`/`texture2DLod`/`textureSize` chamam
   funções do host; `gl_FragCoord`/`gl_FragColor` viram globais; `#version`, `#extension`,
   `precision` e qualificadores de precisão somem; identificadores que são palavras-chave ou
   built-ins só no 4.50 (`sample`, `filter`, `round`/`tanh` definidos pelo usuário…) são
   renomeados. As linhas são preservadas (erros apontam a linha do usuário). As anotações
   (`@label @default @range @min @max @color @resolution @time @mouse @sdf @mipmap`) são lidas do
   comentário `/** */` antes de cada `uniform`.
2. **GPU (principal)**: um dispositivo **wgpu headless** (DX12/Vulkan/Metal) por processo. O
   mesmo código vira um fragment shader de verdade: um bloco uniforme com o mapeamento da vista,
   o tamanho do nó, mapeamento/estado de 8 slots de textura e os uniforms do usuário empacotados
   em `vec4[]` (um vec4 por escalar/vetor/elemento/coluna, sem contas de std140); o `main` do
   usuário é renomeado e embrulhado (desempacota uniforms, calcula `gl_FragCoord` em unidades do
   nó com origem GL embaixo à esquerda, copia `gl_FragColor` para o alvo RGBA8). O módulo naga é
   validado e passado direto ao wgpu (`ShaderSource::Naga`); erros de validação são pegos por
   *error scope*. Resultado lido de volta (pré-multiplicado).
3. **CPU (fallback e testes)**: um intérprete da IR do naga (`eval.rs`): valores escalares/vetores
   /matrizes inline, arrays/structs, ponteiros com caminho de acesso (inclusive `inout`/`out`),
   todas as funções matemáticas usadas, `discard`, limite de 2²⁰ iterações por pixel, linhas em
   paralelo nos núcleos. É ~100× mais lento que a GPU, por isso usa um orçamento de pixels menor
   (40 k contra 2 M). É o caminho sem adaptador (CI, `PEN_SHADER_GPU=0`) e o dos goldens
   (determinístico).

Texturas: imagens (relativas ao `.pen`; http(s) não é baixado) com FLIP_Y, filtro bilinear,
mipmaps em caixa com `@mipmap` (LOD = `bias` de `texture2D` ou o LOD explícito, sem derivadas);
`pencil:object` (transparente fora), `pencil:background` (espelhado fora) e `@sdf` cobrem a região
renderizada. O SDF é calculado na CPU (transformada de distância euclidiana exata de
Felzenszwalb sobre a máscara de cobertura a 50 %, com ajuste sub-texel nas bordas): `r` = distância
assinada em unidades do nó (positiva dentro), `gb` = gradiente unitário.

Uniforms: valores de `uniforms` (número, booleano, `#hex`, arrays, `$var` já resolvida) ou
`@default`; `@resolution` = tamanho do nó; `@time` e `@mouse` vêm do estado de animação do nó
(ver "Animação" abaixo); fora do editor (CLI, export, goldens) `@time` = 0 e `@mouse` = centro do
nó, como nos exports do Pen.

### Fill, stroke e effect

- **Fill**: imagem do shader sobre a caixa do nó (`ImageFit::Stretch`), recortada pela forma, com
  `opacity`/`blendMode` do fill. `pencil:object` = os fills abaixo dele, sobre transparente
  (sem sombras); `@sdf` = a forma do nó.
- **Stroke**: mesma imagem (sem `pencil:object`).
- **Effect**: todos os itens do nó (fills, filhos, stroke, demais efeitos) são levados para o
  espaço local e viram o `pencil:object`; o shader roda sobre os bounds visuais (que incluem a
  caixa do nó) e a imagem substitui os itens do nó. Vários efeitos encadeiam na ordem da lista.
  `@sdf` vem do alfa do conteúdo. Os efeitos de shader são aplicados depois dos demais efeitos
  (o legado respeita a posição na lista).
- `pencil:background` = itens desenhados antes do nó **no mesmo chunk** (a mesma raiz de topo),
  levados para o espaço local, com os escopos abertos fechados.

### Cache, assíncrono e erros

- Chave = hash(url + identidade do arquivo compilado, uniforms, tamanho, região, forma,
  conteúdo de `pencil:object`/`pencil:background` — imagens pelo id). 256 resultados em cache;
  programas compilados por arquivo (recompilados quando o mtime/tamanho muda).
- Só chunks reconstruídos pedem shaders; mover uma raiz translada o chunk sem renderizar nada.
- No app (`ShaderEngine::deferred`) a renderização vai para uma thread: o builder recebe
  `Pending`, desenha o nó sem o shader e marca o chunk; quando termina, `take_ready` faz o
  `poll_assets` existente (o mesmo das imagens) refazer só esses chunks.
- Erro de compilação/execução/arquivo ausente: o fill vira um tom magenta translúcido
  (`SHADER_ERROR`) e a mensagem fica em `ShaderEngine::stats().errors`; um efeito com erro deixa
  o nó como se não tivesse o efeito.

### Animação (`@time`, `@mouse`)

Comportamento do Pen, conferido no bundle (`animatingNodes`, `advanceAnimation`, `makeNodeShader`):

- Um shader é "animado" se declara um uniform `@time` ou `@mouse` (`isAnimated`).
- **Só anima o que está selecionado**: o conjunto de nós animados é refeito a cada mudança de
  seleção e contém os nós selecionados e seus descendentes que têm shader animado (mais o slide
  atual no modo apresentação, que não existe aqui). Os outros mostram o último quadro.
- `@time` é um relógio **por nó**, em segundos, que só avança enquanto o nó anima (selecionar de
  novo continua de onde parou). O passo de cada quadro é limitado a 0,1 s; enquanto o quadro
  anterior ainda não apareceu, o tempo acumula e entra todo no próximo passo.
- `@mouse` é o ponteiro em unidades locais do nó, convenção GL (origem embaixo à esquerda:
  `[x, altura - y]`), suavizado a cada quadro: `alvo + (atual - alvo)·e^(-5·dt)`. Começa no
  centro e segue o ponteiro em qualquer lugar do canvas (não só sobre o nó); sem ponteiro fica onde
  está. `@mouse` não aceita `@default` no Pen.
- Exports (PNG/SVG/PDF e afins) usam t = 0 e o centro do nó.

No OpenPen:

- **pen-layout**: `ShaderRequest` ganhou `time`, `mouse` (local, y para baixo; o motor converte
  para GL) e `slot` (identidade estável nó + posição do fill/effect). O builder guarda um
  `ShaderAnim` por nó (`set_shader_anim`), põe o tempo/mouse na chave só dos shaders que os usam
  (os estáticos não mudam de chave: goldens iguais) e registra os nós animados com a raiz cujo
  chunk os desenha (`animated_shaders`). `Pipeline::animate_shaders(scene, &[(nó, ShaderAnim)])`
  aplica um quadro: reconstrói **só os chunks** desses nós (e os que esperavam resultados
  prontos), mantendo os outros `Arc`s. `node_list` (export de um nó) ignora o estado de animação;
  `set_export_shader_time` muda o t dos exports (`openpen-cli render --time <s>`).
- **pen-shader**: no motor adiado, shaders animados não passam pelo cache comum (256 entradas que
  os quadros esvaziariam): cada `slot` guarda o último quadro pronto e no máximo um quadro em
  renderização. Pedido com chave nova → enfileira (se nada estiver rodando nesse slot) e responde
  `ShaderLookup::Updating(quadro anterior)`: o builder desenha o quadro anterior e marca o chunk
  como pendente — sem piscar. `busy()` diz se há trabalho na fila. Os quadros levam o bit
  `pen_render::TRANSIENT_IMAGE` no id.
- **pen-canvas**: imagens transitórias saem do atlas da GPU assim que um frame é pintado sem
  elas (antes ficariam 600 frames; a 40 fps isso seriam centenas de MB).
- **app** (`apps/openpen/src/shader_anim.rs`, lógica pura com testes; `canvas_host.rs`): anima os
  nós que estão na seleção (ou dentro dela) **e** cruzam a área visível do canvas, com a janela
  ativa. Um laço com timer de 16 ms (limite de 60 fps; no Windows o timer dá ~40 ticks/s) avança
  os relógios e chama `animate_shaders`; se o motor ainda está ocupado, o tick só acumula tempo
  (como o Pen). O quadro vai direto para o canvas, sem notificar o `EditorState` (painéis não
  redesenham). O laço termina quando nada anima (seleção vazia, nó fora da tela, janela inativa)
  e recomeça no próximo `render` do canvas (mudança de seleção, pan/zoom, volta da janela). Se o
  canvas para de renderizar por 300 ms (janela minimizada, mesmo que o SO ainda a dê como ativa) os
  relógios pausam e o laço só cutuca a janela a cada 0,5 s. O ponteiro do documento vem dos
  eventos de movimento do canvas.
- `OPENPEN_ANIM_LOG=1` grava em `%TEMP%/openpen.log` quadros/s e custo no thread de UI a cada 2 s.

Números (release, GPU dedicada):

| Caso | Valor |
|---|---|
| `cargo bench -p pen-shader --bench anim`: tick no thread de UI (avança o tempo de 4 fills fbm 300×200 numa página com 400 retângulos, reconstrói o chunk e enfileira) | **0,09 ms** |
| idem, quadro inteiro (tick + 4 renderizações de 240 k px na GPU + leitura + troca) | **2,3 ms** |
| app, demo com 3 shaders animados (`target/shader-anim-demo/`), zoom 91 % | ~40 quadros/s, 0,06 ms/tick no thread de UI, 11–15 % de um núcleo no processo |
| app, mesmo documento com a página fora da tela / nada selecionado | 0,0 % |
| app, janela minimizada com a seleção animada | ~1 % (timer de 16 ms ocioso) |

O custo de CPU restante ao animar é principalmente do canvas (conversão RGBA pré-multiplicada →
BGRA e upload de ~1,7 MB por shader por quadro, a 2 px por unidade) e do redesenho da janela.
Antes desta mudança cada consulta a um programa fazia um `stat` do arquivo (dezenas de µs no
Windows, várias por shader por build): agora o arquivo é reverificado no máximo a cada 0,5 s, o que
levou o tick do benchmark de 1,9 ms para 0,09 ms.

Capturas: `docs/screenshots/shader-anim-1.png`, `-2.png`, `-3.png` (app release, a página
selecionada; o gradiente e o fbm mudam entre as capturas e o anel do círculo segue o ponteiro,
em cima à esquerda na primeira e embaixo à direita nas outras).

### Números (release, 16 núcleos, GPU dedicada)

- Biblioteca do legado: **122/122** shaders (`goodies/shaders` + `shader-library/assets`)
  compilam no naga, validam e rodam na GPU; o intérprete de CPU também roda os 122 sem
  construções não suportadas.
- GPU, 400×240 px (região de 200×120 a 2×): 2–8 ms por shader, incluindo upload e leitura.
  A primeira renderização de cada shader paga a compilação do pipeline pelo driver (~0,3 s).
- CPU, 100×60 px: mediana ~0,3 s, os mais pesados (fbm em laço, ray marching) 4–17 s — por isso
  o orçamento de 40 k pixels e a GPU como caminho padrão.
- `cargo bench -p pen-shader` (fbm de 5 oitavas, fill 300×200 = 60 k px): GPU **0,25 ms**
  (upload, desenho e leitura); CPU **217 ms**; reescrita + parse naga + preparação da IR 0,15 ms.
- Exemplos da shader-library (75 cenas `*.pen.json`, teste `legacy_library_examples_render`):
  todas montam e renderizam sem erro de shader (imagens remotas https não são baixadas, então
  vidros/refrações mostram só o que está no documento).

Capturas: `docs/screenshots/script-app.png` (app real com `target/script-demo/demo.pen`: dois
scripts, um script com erro, fill de shader, efeito `pencil:object` e fill com `@sdf`; o painel de
camadas não mostra a saída gerada) e `docs/screenshots/script-cli-render.png` (o mesmo documento
pela CLI).

## Lacunas conhecidas

- Animação: sem modo apresentação (no Pen o slide atual também anima); a resolução do quadro
  animado é a mesma do estático (até 2 px por unidade), sem reduzir para economizar; um nó animado
  dentro de uma raiz grande reconstrói o chunk inteiro da raiz a cada quadro (barato até centenas
  de nós; não há patch só da imagem). Erros de execução de um shader animado são tentados de novo a
  cada quadro.
- `pencil:background` só vê o mesmo chunk e não é invalidado quando outro chunk muda.
- Shader effects são aplicados após os outros efeitos, não na posição da lista.
- A resolução do shader é limitada pelo orçamento (até 2 px por unidade); com zoom alto ou export
  em escala grande a imagem fica suavizada.
- Sem derivadas (`dFdx` = 0) nem LOD implícito.
- Scripts rodam no thread do `update` (bloqueia até 2 s no pior caso); a saída gerada não é
  clicável fora do retângulo do script; a ação "Convert to layers" e o seletor de componentes do
  input `ref` não existem; o caminho do script não é editável no inspector.
- O corpus `testdata/` não tem nós `script` nem shaders; o teste de corpus só confirma que nada
  quebra.
