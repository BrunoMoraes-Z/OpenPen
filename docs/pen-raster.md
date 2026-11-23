# pen-raster: backend CPU da DisplayList

`crates/pen-raster` desenha uma `pen_render::DisplayList` na CPU com **tiny-skia** (geometria, gradientes, blend) e **cosmic-text** (shaping e glifos). É o backend de referência: thumbnails, export PNG, testes golden e o que um agente "vê".

## API

```rust
let mut ctx = pen_raster::RasterContext::new();          // Inter embutida; famílias do sistema copiadas sob demanda
let pixmap = ctx.render(&list, viewport, scale, bg);     // viewport em espaço de documento; saída = ceil(viewport * scale)
ctx.render_into(&list, viewport, scale, bg, &mut pixmap); // reaproveita a alocação entre frames
let png = ctx.render_to_png(&list, viewport, scale, bg)?;
// Atalhos com um contexto global compartilhado (protegido por mutex):
pen_raster::render(&list, viewport, scale, bg);
pen_raster::render_to_png(&list, viewport, scale, bg)?;
// Contornos vetoriais do texto (mesmo shaping/fontes do render), usados pelo pen-export:
let outlines: Vec<pen_raster::TextOutline> = ctx.text_outlines(&block); // (run, Path) em espaço local
```

As fontes seguem a mesma política do pen-layout (`pen_fonts::system::LazyFonts`): o banco começa só com a Inter, cada família citada é copiada do catálogo do sistema (escaneado uma vez por processo) na primeira vez que aparece, e um glifo faltando carrega os fallbacks da plataforma e, depois, todas as faces. `with_font_system(fs)` usa só o banco dado.

`RasterContext` guarda `FontSystem`, `SwashCache`, cache de shaping por linha, cache de imagens (com mipmaps e orçamento de memória LRU) e pools de máscaras/pixmaps/paths. Para renderizar em paralelo, use um contexto por thread. Também há `with_font_system`, `load_font_data`, `has_font_family`, `clear_caches` e `set_image_cache_budget`.

## Recursos suportados

- **Formas**: retângulo com raio por canto (limitado como no CSS), elipse, path (`NonZero`/`EvenOdd`).
- **Fills** (de baixo para cima): sólido, linear, radial (elíptico, girado e cisalhado: o círculo unitário mapeado por `center + x·axis_x + y·axis_y` via transform do shader), angular/cônico (gerado por pixel numa textura do tamanho da região visível) e imagem com `Cover`/`Contain`/`Stretch`/`Tile` e opacidade. Imagens reduzidas usam mipmaps (box 2x2) e amostragem bilinear.
- **Stroke**: alinhamento `Inside`/`Center`/`Outside`, caps, joins, miter limit, dash, várias paints. Em retângulos, inside/outside e larguras por lado viram um anel exato (outer − inner, even-odd). Nas demais formas, inside/outside usa traço com largura dobrada recortado pela máscara da forma (ou do complemento). Paths abertos sempre usam `Center`.
- **Sombras**: drop shadow abaixo dos fills e inner shadow acima dos fills (abaixo do stroke), com offset, blur e spread. Spread em retângulo/elipse é geométrico; em paths usa o contorno do traço (dilata ou erode). Blur grande é calculado em resolução reduzida (como o Skia) e ampliado de forma bilinear.
- **Blur**: aproximação de gaussiana com 3 box blurs por eixo (somas deslizantes). Convenção: `sigma = raio / 2` (a mesma do CSS e do Figma).
- **Texto**: cada `TextLine` é moldada com cosmic-text usando os estilos dos runs (família, tamanho, peso, itálico, letter spacing em px). Os glifos são desenhados em `x`/`baseline` da linha. Sublinhado e tachado usam as métricas da fonte. Com transform só de escala e translação e paint sólida, os glifos vêm do bitmap do `SwashCache`. Nos outros casos (rotação ou gradiente) vêm do contorno vetorial. Família ausente cai para a sans-serif do sistema (Inter, Segoe UI, Arial…). `Justify` distribui a sobra entre os espaços. `clip` recorta ao box.
- **Clip**: `PushClip`/`PopClip` com máscara anti-aliased e aninhamento (interseção). As máscaras são reaproveitadas e só a área suja é limpa.
- **Layers**: `PushLayer`/`PopLayer` desenha num offscreen do tamanho de `bounds` (mais a margem do blur), composto com opacidade, blend mode (os 16 mapeiam 1:1 para tiny-skia) e blur. O backdrop blur desfoca o que já está embaixo, dentro de `bounds` e do clip atual, antes do conteúdo. Layers triviais (opacidade 1, normal, sem blur) não alocam offscreen.
- **Culling**: por chunk (`DisplayList::visible`), por item (bounds com stroke e sombra contra o clip atual) e por layer.

## Diferenças conhecidas em relação ao Skia/Pen

- O blur é uma aproximação por box blur, com diferença de até ~1–2 % na intensidade. Sombras e blur que dependem de pixels fora do viewport são aproximados nas bordas da imagem.
- A sombra usa só a silhueta da geometria, sem "knockout" dentro de formas translúcidas.
- O backdrop blur vale para o retângulo `bounds` (∩ clip). Para recortar pela forma, o gerador da lista deve envolver a layer num `PushClip`.
- No inside/outside de formas que não são retângulo, a largura dobrada também dobra caps e miters de dashes e de cantos agudos.
- Larguras por lado só valem em retângulos sem dash. Nos outros casos, usa-se a maior largura.
- Os glifos são renderizados sem hinting e em tons de cinza (sem subpixel). A interpolação dos gradientes é em sRGB não pré-multiplicado.
- O gradiente angular não tem anti-aliasing na emenda de 0°/360°.
- Não há suporte a shaders GLSL nem a mesh gradient (não fazem parte do contrato).

## Testes e benchmark

- `cargo test -p pen-raster`: testes unitários de geometria, blur e paint, mais cerca de 24 golden images em `crates/pen-raster/tests/golden/`. Quando o golden não existe, ele é gravado (bootstrap). Se houver divergência, o teste grava `<nome>.actual.png` e falha. `PEN_RASTER_UPDATE_GOLDEN=1` regrava todos. Os goldens de texto exigem a fonte Arial e fazem skip sem ela.
- `cargo bench -p pen-raster`: lista sintética com 4.920 itens (40 cards com sombra e clip, retângulos, elipses com gradiente, estrelas com stroke e textos) em 1920x1080. Resultado: cerca de **52 ms** por frame (release, Windows).
