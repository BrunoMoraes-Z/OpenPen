# pen-canvas: canvas GPU sobre GPUI

`crates/pen-canvas` desenha a `DisplayList` (contrato de `crates/pen-render`) com as primitivas do
GPUI (`gpui-kit 0.9.1` → `gpui-pre 0.3.12`, renderer DirectX 11 no Windows). Não muda o contrato.

## 1. Spike de capacidades do GPUI

O que foi lido (fonte real em `~/.cargo/registry/src/*/`): `gpui-pre-0.3.12/src/window.rs`
(`paint_quad`, `paint_path`, `paint_drop_shadows`/`paint_inset_shadows`, `paint_image`,
`paint_glyph`/`paint_emoji`, `with_content_mask`, `paint_layer`), `scene.rs` (`Path`,
`insert_primitive`, `BoundsTree`), `path_builder.rs` (lyon), `color.rs` (`Background`),
`text_system.rs` (`layout_line`, `LineLayout`) e `gpui-pre-windows-0.3.12/src/shaders.hlsl` +
`directx_renderer.rs`.

Achados que definem o desenho:

- **Quads** (`PaintQuad`): bounds axis-aligned, raio **por canto**, borda **por lado**
  (`Edges`), `BorderStyle::Solid|Dashed` (padrão de traço fixo), fundo `Background`.
  O GPUI arredonda os bounds para o pixel do dispositivo (`snap_bounds`).
- **`Background`**: sólido ou **gradiente linear de 2 paradas**, definido por ângulo CSS e
  percentuais avaliados sobre os bounds do quad (ou sobre os bounds *clipados* do path).
  Não há radial, cônico nem N paradas.
- **Sombras**: `BoxShadow` com blur (gaussiano; `blur_radius` = σ), spread, offset, drop e
  inset, para retângulos arredondados apenas. O drop é recortado sob o elemento (como no Figma).
- **Paths**: `Path<Pixels>` é uma lista de triângulos (Loop-Blinn) rasterizada num alvo MSAA
  intermediário e composta por sprite. Não há transformação, só cor/`Background` por path.
  O `PathBuilder` do próprio GPUI usa lyon (fill e stroke com dash). Usamos lyon direto para
  controlar fill rule, caps/joins, miter, dash e cache.
- **Imagens**: `paint_image(bounds, image_bounds, corner_radii, Arc<RenderImage>)` (BGRA
  **alfa reto**, sampler linear, sem mipmaps), com recorte por retângulo arredondado.
  A opacidade vem de `element_opacity`, que é `pub(crate)`.
- **Texto**: `WindowTextSystem::layout_line` (shaping) + `paint_glyph(origem na baseline,
  font_id, glyph_id, font_size, cor)` público. Os glifos não giram.
- **Clip**: `with_content_mask` aceita só **retângulo axis-aligned**, sem anti-aliasing.
- **Opacidade de grupo, blend modes, blur de camada e backdrop blur**: não existem.
  `with_element_opacity` é privado, e não há render-to-texture público.
- **Ordem de desenho**: cada primitiva entra num `BoundsTree` que atribui uma ordem pela
  sobreposição (~1,5 µs/primitiva medido). `paint_layer(bounds)` dá uma ordem única a tudo
  que está dentro, sem consultar a árvore, mas dentro da mesma ordem o GPUI agrupa por tipo
  (sombras < quads < paths < sprites).

### Matriz (DisplayList → GPUI)

| Recurso | Status | Como |
|---|---|---|
| Rect axis-aligned (inclui flips e rotações de 90°), raio por canto | **nativo** | `paint_quad`; raios permutados conforme a transformação, clamp CSS |
| Círculo (elipse com w = h) | **nativo** | quad com raio w/2 (SDF, AA perfeito) |
| Rect rotacionado/skew, elipse w ≠ h, paths | **nativo (tesselado)** | lyon fill (NonZero/EvenOdd) → `paint_path` |
| Stroke sólido em rect axis-aligned (larguras por lado, inside/center/outside) | **nativo** | quad só com borda, com bounds/raios expandidos pelo alinhamento |
| Stroke com dash, gradiente ou em paths | **nativo (tesselado)** | lyon stroke (cap/join/miter, dash por medição de comprimento) |
| Alinhamento inside/outside em rect/elipse tesselados | **exato** | geometria inset/outset antes do stroke centrado |
| Alinhamento inside/outside em paths arbitrários | aproximado | tratado como center |
| Larguras por lado em shapes tesselados | aproximado | usa a maior largura |
| Gradiente linear de 2 paradas (quad e path) | **nativo** | conversão exata start/end → ângulo + percentuais do shader (testada contra uma reimplementação do shader); transformações não conformes usam `A^-T d` |
| Gradiente linear N paradas, radial (com rotação: `gradient_t` inverte a matriz dos eixos), angular em rect/círculo | **exato via textura** | textura CPU (≤ 256²) gerada uma vez por (chunk, versão, item, fill, alfa) e desenhada com `paint_image` + raios |
| Idem em paths/elipses/rects rotacionados | **exato via fallback CPU** | textura `pen-raster` do item (linear de 2 paradas continua nativo) |
| Imagem em rect axis-aligned (cover/contain/stretch/tile) | **nativo** | `paint_image` com recorte arredondado; tile = N sprites (máx. 4096, sem raio) |
| Imagem em path/elipse | **exato via fallback CPU** | textura `pen-raster` do item (abaixo de 6 px de tela: cor média) |
| Opacidade da imagem / da camada sobre imagens | **exato** | variante da textura com alfa pré-multiplicado no upload (cache por id + alfa em 1/64) |
| Drop/inner shadow em rect/círculo | **nativo** | `paint_drop_shadows` / `paint_inset_shadows`; blur do design = 2σ → `blur_radius = blur/2` |
| Drop shadow sem blur e sem spread em path | **exato** | o path repetido com offset |
| Sombras com blur em paths/rects rotacionados; inner shadow em paths | **exato via fallback CPU** | textura do item |
| Texto (shaping por run, peso, itálico, letter-spacing, sublinhado, tachado) | **nativo** | `layout_line` no tamanho do documento (cache por chunk), glifos posicionados e escalados no paint; alinhamento reaplicado com a largura medida pelo GPUI |
| Texto com paint gradiente/imagem; texto rotacionado/espelhado | **exato via fallback CPU** | textura do item (shaping do cosmic-text, não do DirectWrite) |
| Clip retangular axis-aligned | **nativo** | content mask |
| Clip arredondado/círculo com conteúdo dentro da forma (após o recorte pelo bbox) | **nativo** | content mask = bbox; teste de convexidade nos 4 cantos do filho ∩ bbox |
| Clip arredondado/círculo sobre filho quad só com fills (header, imagem cover/stretch) | **nativo** | "adoção": desenha filho ∩ clip com os raios do clip nos cantos coincidentes |
| Demais clips arredondados, elípticos, path ou rotacionados | **exato via fallback CPU** | textura do escopo inteiro |
| Opacidade de camada sem sobreposição entre filhos | **nativo** | alfa multiplicado em cada item (exato) |
| Opacidade de grupo com filhos sobrepostos (ou item com fill+stroke/sombra) | **exato via fallback CPU** | textura isolada com o alfa embutido |
| Layer blur | **exato via fallback CPU** | textura isolada (margem de 1,5 × blur) |
| Blend modes, backdrop blur | **exato via fallback CPU** | textura "backdrop": tudo que foi pintado antes dentro dos bounds + o escopo, sobre o fundo do canvas, desenhada opaca |

Ainda aproximados: alinhamento inside/outside e larguras por lado em paths, stroke com
paint radial/angular em quads (vira fallback do item) e qualquer escopo de fallback menor que
`LodPolicy::fallback_min_px` (6 px) na tela ou enquanto a textura não fica pronta (ver §3).

## 1b. Fallback por textura CPU (`fallback.rs`)

Fluxo, todo dentro do `PreparedChunk` (o contrato não muda):

1. **Detecção** (`fallback::analyze`, uma vez por `(root, version)`): percorre a árvore de
   escopos do chunk e marca candidatos. Para layers: blend ≠ Normal e backdrop blur (tipo
   *Backdrop*), layer blur e opacidade < 1 com filhos sobrepostos (*Isolated*). A sobreposição
   é testada com um sweep por x dos bounds dos filhos, e um item com mais de uma camada de
   pintura conta como sobreposto. Para clips: rect axis-aligned sem raio é sempre nativo. Nos
   outros, cada filho é recortado pelo bbox do clip (o content mask do GPUI faz isso exatamente)
   e só vira fallback se esse pedaço sair da forma convexa e não puder ser "adotado". Clip de
   path sempre vira fallback. Também há candidatos de item único (efeitos de path, texto). Ficam
   só os escopos **mais externos**, e um blend/backdrop interno promove o externo a *Backdrop*.
2. **Região e bucket** (`plan_region`): escala de dispositivo `zoom × scale_factor`,
   quantizada em potências de √2. Se o escopo inteiro cabe em 4096² e 8 Mpx, a textura cobre o
   escopo (a chave não depende do pan). Senão, cobre a parte visível com 15% de margem,
   alinhada a células de 256 px de dispositivo, e o bucket desce até caber.
3. **Cache** (`FallbackCache`): chave `(root, version, índice do escopo, bucket, alfa em 1/64,
   env, células)`. `env` é o hash dos `(root, version)` dos chunks abaixo dentro dos bounds,
   mais o fundo (só para *Backdrop*). LRU por frame com orçamento de bytes
   (`CanvasOptions::fallback_budget_bytes`, 192 MB por padrão). Nunca despeja o que foi usado no
   frame. O despejo chama `window.drop_image` e versões velhas saem no `set_display_list`.
4. **Render fora da UI** (`RasterJob`): o paint só registra pedidos. O `DesignCanvas` os
   despacha no `background_executor` (no máximo `fallback_max_jobs` = 3 em voo, primeiro os que
   não têm substituto). O job monta a sublista (o escopo sozinho, ou os chunks abaixo + o
   prefixo do chunk com os pops que faltam), renderiza com um `RasterContext` de um pool,
   converte para BGRA reto e cria o `RenderImage`. Ao terminar, faz `cx.notify()`.
5. **Até ficar pronto**: usa a textura de outro bucket do mesmo escopo que cubra a área
   visível (o mais próximo, preferindo o mais nítido). Se não houver, usa a aproximação GPU de
   antes (contada em `approximated`). Isso evita buracos no pan/zoom, que só ficam alguns frames
   borrados.

Diagnóstico em `FrameStats`: `fallback_scopes`, `fallback_drawn`, `fallback_standin`,
`fallback_waiting`, `fallback_jobs`, `fallback_bytes`. `CanvasOptions::raster_fallback = false`
desliga tudo.

## 2. API (`DesignCanvas`)

```rust
let canvas = cx.new(|cx| DesignCanvas::new(cx));
canvas.update(cx, |c, cx| { c.set_display_list(list, cx); c.zoom_to_fit(cx); });
```

- Conteúdo: `set_display_list(Arc<DisplayList>)` (mantém o cache dos chunks com o mesmo
  `(root, version)`), `display_list()`, `invalidate_caches()`. Os chunks são `Arc<Chunk>`
  compartilhados com o pipeline: o app entrega uma lista nova a cada edição sem copiar itens, e os
  jobs de fallback *backdrop* reaproveitam os chunks de baixo em vez de cloná-los.
- Viewport: `viewport()`/`set_viewport(Viewport)`, `zoom()`, `pan_by`, `zoom_to(zoom, anchor)`,
  `zoom_in`/`zoom_out` (passos 2%…25600%), `zoom_to_fit()`, `zoom_to_rect(rect)` (adiados até o
  primeiro layout), `visible_doc_rect()`, `canvas_size()`, `canvas_bounds()`.
- Coordenadas: `screen_to_doc(Point<Pixels>)`, `doc_to_screen(Vec2)`, `doc_rect_to_screen(&Rect)`
  (coordenadas da janela). A matemática pura está em `pen_canvas::viewport::Viewport`
  (`screen = doc * zoom + offset`, zoom em `[0.02, 256]`).
- Opções (`options_mut`): fundo (`#f6f6f6`, o `--canvas-bg` claro do Pen; o escuro é `#18181b`),
  grade de pixels (ligada a partir de 800%, linhas de 1 px de dispositivo), `LodPolicy`,
  padding do fit, atalhos embutidos.
- Diagnóstico: `last_frame_stats()` → `FrameStats` (tempos, contadores, `approximated`).
- Entrada: roda/trackpad = pan (Shift+roda = horizontal); Ctrl/Cmd+roda = zoom no cursor
  (deltas em linhas ou em pixels); pinch (`PinchEvent`) = zoom no centro do gesto; arrastar
  com o botão do meio ou Espaço+arrastar = pan (cursor mão aberta/fechada, continua fora do
  canvas); atalhos Shift+1 (fit), Ctrl+0 (100%), Ctrl+=/- (passos).

## 3. Performance

- **Culling** por chunk (bounds × viewport) e por item (bounds pré-calculados no documento, com
  stroke, sombra e overflow de texto). Itens com menos de 0,5 px de tela são ignorados, e escopos
  de clip fora da tela são pulados inteiros.
- **Cache por chunk** (`PreparedChunk`): válido só enquanto o `Arc<Chunk>` for a **mesma
  alocação** (`Arc::ptr_eq`). O par `(root, version)` só é único dentro de um mesmo builder, e
  listas vindas de outro pipeline ou de um doc recarregado podem repeti-lo. As texturas usam um id
  local do canvas como chave. O cache guarda bounds dos itens,
  tesselações lyon em espaço local e linhas de texto já moldadas (`Arc<LineLayout>`). A
  tesselação guarda a tolerância usada e só é refeita quando sai da faixa de 0,02 a 0,5 px de
  tela, ou seja, uma janela de zoom de 25× (equivale ao "bucket de zoom"). Texturas de
  imagem/gradiente vivem num cache com despejo após 600 frames sem uso (`drop_image`).
- **LOD**: texto abaixo de 4 px vira barra cinza (sem shaping); shapes não retangulares abaixo de
  3 px viram quad com alfa de cobertura; sombras somem abaixo de 25% de zoom ou quando têm
  menos de 0,5 px; o tamanho de fonte do glifo é quantizado (0,25 px até 12 px e depois 1/24 de
  oitava), para o zoom contínuo reaproveitar o atlas.
- **Ordem barata e exata** (`order.rs`): os glifos de cada bloco de texto vão numa
  `paint_layer`. Abaixo de 50% de zoom, o chunk é pintado em **segmentos**, cada um numa
  `paint_layer`. Dentro de uma layer o GPUI desenha por tipo (sombras < quads < paths < glifos <
  imagens, e as imagens são ordenadas por textura do atlas). A versão anterior punha o chunk
  inteiro numa layer, o que quebrava sobreposições reais: um rect sobre texto ficava embaixo
  dos glifos, o stroke quad de um círculo com gradiente-textura ficava embaixo da textura e o
  inner shadow ficava embaixo do fill. Agora o segmentador (por versão do chunk, em bounds de
  documento) fecha a layer antes de um item que sobrepõe um item anterior do mesmo segmento
  com tipo desenhado depois (ou imagem sobre imagem). Os tipos possíveis de cada item são
  calculados de forma conservadora, cobrindo todos os LODs. Itens cujos próprios primitivos
  saem fora da ordem dos tipos (fill textura + stroke quad, inner shadow) e escopos de fallback
  com vários itens vão em segmentos *diretos* (o `BoundsTree` por primitiva). Clips e alfa
  ficam numa pilha explícita no painter, então um segmento pode começar no meio de um escopo.
  Há uma segunda segmentação para quando todo texto do chunk vira barra (zoom <
  `text_bar_px / maior fonte`), em que o texto conta só como quad. No stress isso reduz de ~18
  para ~10 o número de layers por chunk. Teste: `randomized_segments_never_misorder` simula a
  ordenação do GPUI e verifica que nenhum par sobreposto sai invertido.
- Alocações por frame: o GPUI exige um `Path` novo por primitiva (e o clona duas vezes
  internamente). O resto (lista de chunks visíveis, pilhas de alfa e sombras) reusa buffers.

### `examples/canvas_stress.rs`

```
cargo run -p pen-canvas --release --example canvas_stress -- [segundos=10] [frames=200]
```

São 200 frames × ~56 itens (11.200 itens): rects arredondados com e sem stroke/sombra,
cabeçalho com gradiente linear, textos (Segoe UI), estrelas (path + stroke), ondas com stroke
tracejado, rects rotacionados com radial (fallback de item), círculos com radial de 3 paradas,
camadas com opacidade e clip arredondado por frame (o header é "adotado", sem fallback). A
cada 8 frames entra conteúdo de fallback: layer blur, multiply, backdrop blur, clip circular
com estrela e barra rotacionada atravessando a borda, e grupo com opacidade sobre círculos
sobrepostos. Variáveis: `STRESS_NO_FALLBACK`, `STRESS_NO_LAYERS`, `STRESS_ALWAYS_LAYERS`,
`STRESS_VERBOSE`. Com `STRESS_STILL="x,y,zoom"` a câmera fica parada, e com
`STRESS_RASTER_OUT=arquivo.png` o exemplo grava o render `pen-raster` da mesma vista para
comparação. A câmera oscila entre "fit all" (~0,07×) e 250% enquanto passeia.
Para cada frame, o exemplo mede o intervalo entre renders, a CPU do render até o fim do paint
do canvas e o paint do canvas (separado em shapes e texto).

Medido nesta máquina (Windows 11, monitor de 60 Hz, release, 15 s, com outros builds rodando em
paralelo na 1ª execução):

| Execução | intervalo p50 / p95 | CPU render→paint avg / p95 / max | paint do canvas avg / p95 |
|---|---|---|---|
| 1 (máquina ocupada) | 16,68 / 19,75 ms | 2,76 / 7,49 / 21,5 ms | 2,68 / 7,39 ms |
| 2 | 16,67 / 16,89 ms | 2,13 / 4,94 / 8,1 ms | 2,08 / 4,87 ms |

Com fallback e segmentação exata (3 execuções limpas de 15 s, a lista acima com o conteúdo
de fallback):

| Execução | intervalo p50 / p95 | CPU render→paint avg / p95 / max |
|---|---|---|
| 1 | 16,67 / 16,83 ms | 2,66 / 6,21 / 9,6 ms |
| 2 | 16,66 / 17,06 ms | 2,92 / 7,21 / 9,0 ms |
| 3 | 16,67 / 17,08 ms | 2,92 / 7,17 / 9,2 ms |
| sem fallback (`STRESS_NO_FALLBACK`) | — | 2,90 / 7,22 / 10,6 ms |

Por frame, em média: ~710 layers e 7 segmentos diretos, ~460 escopos de fallback visíveis
(a maioria são os rects rotacionados com radial, e muitos ficam abaixo de 6 px). Desses, ~60
são desenhados pela textura exata, ~137 por um substituto de outro bucket e ~104 pela
aproximação GPU. O stress roda com zoom sempre em movimento, então o bucket muda ~4×/s. Foram
~2.000 jobs em 15 s e o cache ficou em 12,6 MB no pico. O custo do fallback na thread de UI é
desprezível (o p95 com e sem ele é igual). A segmentação exata custa mais que a layer única
por chunk: os piores frames estão em ~16% de zoom com os 200 chunks visíveis (~1.450 layers,
~8,5 ms), contra ~7 ms da layer única, que desenhava errado.

A meta (p95 < 8 ms de CPU por frame) foi atingida, e o app fica preso no vsync (60 fps).

Verificação visual: `docs/screenshots/canvas-fallback.png` (à esquerda, a captura da janela
do exemplo com `STRESS_STILL="1160,150,2.5"`; à direita, o `pen-raster` da mesma lista e da
mesma vista). Diferença média absoluta por região: layer blur 0,7/255, multiply 1,4,
backdrop blur 0,7, clip circular 1,2 e grupo com opacidade 6,1. Nesse último, a diferença vem
do texto que cruza a região (DirectWrite × cosmic-text), não dos círculos. Em média, cada frame teve ~3.000 itens desenhados, ~1.700 quads, ~1.300 paths,
~1.650 glifos e ~650 barras de texto. Os piores frames são os de zoom entre 50% e 100% com toda
a grade visível (o `BoundsTree` volta a valer) e os primeiros frames em que um nível de zoom
aparece (rasterização de glifos e tesselação).

## 4. Testes

`cargo test -p pen-canvas`: 38 testes puros. Os do fallback cobrem detecção (efeitos de
layer, só os mais externos, promoção a backdrop, auto-sobreposição, clip seguro/adotado/
fallback, borda reta × canto, clip rotacionado e circular, efeitos de item), as regras de
adoção, buckets e regiões (estabilidade no pan, redução de bucket), o cache (exato,
substituto mais próximo, LRU, nada despejado no frame em uso, purga de versões) e jobs reais
de `pen-raster` (sublistas isolada e backdrop, pops de fechamento, alfa embutido, env). Os de
segmentação cobrem rect sobre texto, conteúdo ascendente/disjunto em uma layer só, imagens
sobrepostas, unidades diretas, a cobertura contígua, um teste aleatório contra o simulador de
ordem do GPUI e o regime de barras. Os demais cobrem a matemática do viewport (ida e volta
tela↔documento, invariante do zoom no cursor, clamp, fit, retângulo visível, roda e passos),
o culling e os limiares de LOD, o mapeamento de rect/raios sob flips e rotações de 90°, o
clamp de raios, as áreas das tesselações (rect, elipse, even-odd, MoveTo implícito, alinhamentos
inside/center/outside, dash, dash degenerado), a histerese da tolerância, o gradiente linear
contra a reimplementação do shader, as paradas/radial/angular/textura, o image fit, a
quantização de fonte, os bounds de item, o `screen_linear` sob escala não uniforme, a conversão
de imagem (de pré-multiplicado para reto, downsample, média) e o salto de escopos.

O smoke test com `TestAppContext` (`gpui-kit` feature `test-support`) foi escrito, mas **não
entrou**: ligar `test-support` como dev-dependency obriga o Cargo a resolver `gpui-fast`, que
exige `taffy =0.15.0`, enquanto o lockfile do workspace tem `taffy 0.15.1` (vindo do
`gpui-pre`). Resolver isso exigiria mexer no `Cargo.lock` compartilhado.
