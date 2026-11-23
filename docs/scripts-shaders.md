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
`@default`; `@resolution` = tamanho do nó; `@mouse` sem valor = centro do nó; **`@time` = 0**.

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

- `@time` é estático (t = 0): sem animação. `@mouse` não acompanha o ponteiro.
- `pencil:background` só vê o mesmo chunk e não é invalidado quando outro chunk muda.
- Shader effects são aplicados após os outros efeitos, não na posição da lista.
- A resolução do shader é limitada pelo orçamento (até 2 px por unidade); com zoom alto ou export
  em escala grande a imagem fica suavizada.
- Sem derivadas (`dFdx` = 0) nem LOD implícito.
- Scripts rodam no thread do `update` (bloqueia até 2 s no pior caso); a saída gerada não é
  clicável fora do retângulo do script; não há UI no inspector para os `@input` (os valores do
  arquivo são passados) nem a ação "Convert to layers".
- O corpus `testdata/` não tem nós `script` nem shaders; o teste de corpus só confirma que nada
  quebra.
