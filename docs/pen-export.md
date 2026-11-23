# pen-export: export vetorial (SVG e PDF)

`crates/pen-export` transforma a `DisplayList` de `pen-render` (espaço do documento, agnóstica de
backend) em **SVG** (`String`) e **PDF** (`Vec<u8>`). Não depende de GPUI; usa `pen-raster` só
para o que não tem equivalente vetorial (gradiente angular, backdrop blur) e para o contorno dos
glifos no modo "texto em curvas".

## API

```rust
use pen_export::{ExportOptions, TextMode};

let opts = ExportOptions {
    roots: None,                 // None = todos os chunks; Some(vec![k]) = só esses nós de topo
    region: None,                // None = união dos bounds dos chunks escolhidos (como o CLI)
    scale: 1.0,                  // px do SVG / pt do PDF por unidade do documento
    background: None,            // Some(cor) pinta um retângulo sob tudo
    text: TextMode::Text,        // ou TextMode::Outlines (texto em curvas)
    raster_scale: 2.0,           // px por unidade de saída nas partes rasterizadas
};
let svg: String = pen_export::svg::export(&list, &opts);
let pdf: Vec<u8> = pen_export::pdf::export(&list, &opts)?;            // uma página
let pdf = pen_export::pdf::export_pages(&list, &[opts_a, opts_b])?;    // várias páginas
```

- `svg::export` / `pdf::export*` usam o `RasterContext` compartilhado de `pen-raster`
  (`with_shared_context`); as variantes `*_with(&mut RasterContext, ..)` recebem um contexto
  próprio (bench, threads).
- `ExportOptions::root(k)` exporta um nó de topo recortado aos bounds do seu chunk;
  `ExportOptions::resolve_region(&list)` devolve o retângulo efetivo (mínimo 1×1).
- **Nó aninhado** (subárvore): como `openpen-cli render --node`, monte a lista com
  `Pipeline::node_list(&scene, k)` e passe `region = Pipeline::node_rect(&scene, k)`.
- **Documento inteiro**: `pipe.list` com `region = None` (= `list.bounds()`, igual ao CLI).

### Uso sugerido no CLI (`openpen-cli export`)

```rust
// mesma carga/tema/bg/--node de run_render; depois:
let opts = pen_export::ExportOptions {
    region: Some(viewport),            // node_rect ou list.bounds(), como no render
    scale,
    background: bg_opt,                // None quando --bg não é passado
    text: if outline { TextMode::Outlines } else { TextMode::Text },
    ..Default::default()
};
match output.extension() {
    Some(e) if e == "svg" => std::fs::write(output, pen_export::svg::export(&list, &opts))?,
    Some(e) if e == "pdf" => std::fs::write(output, pen_export::pdf::export(&list, &opts)?)?,
    _ => /* png: pen_raster::render_to_png como hoje */,
}
// "--pages": uma página por frame de topo
let pages: Vec<_> = scene.roots().iter()
    .map(|&r| ExportOptions { region: pipe.node_rect(&scene, r), ..ExportOptions::root(r) })
    .collect();
pen_export::pdf::export_pages(&pipe.list, &pages)?;
```

## SVG: mapeamento DisplayList → SVG 1.1

| DisplayList | SVG | Observações |
|---|---|---|
| raiz | `<svg width/height = região×scale, viewBox = região>` | coordenadas do documento preservadas; `background` vira `<rect>` |
| `Chunk` | itens em ordem de pintura | só chunks selecionados (`roots`) que tocam a região |
| `Shape::Rect` (raios iguais) | `<rect rx>` | raios limitados pela regra do CSS (`clamp_radii` de pen-raster) — o `rx` do SVG sozinho deixaria cantos elípticos |
| `Shape::Rect` (raios por canto) | `<path>` com arcos `A` | |
| `Shape::Ellipse` | `<ellipse>` | |
| `Shape::Path` | `<path d>` + `fill-rule="evenodd"` | M/L/Q/C/Z absolutos |
| `transform` | `<g transform="translate(..)"/"matrix(..)">` | identidade omitida |
| `Paint::Solid` | `fill="#rrggbb"` + `fill-opacity` | |
| `Paint::Linear` | `<linearGradient gradientUnits="userSpaceOnUse">` | início = fim → cor do último stop (como tiny-skia) |
| `Paint::Radial` | `<radialGradient cx=0 cy=0 r=1 gradientTransform="matrix(ax.x ax.y ay.x ay.y cx cy)">` (os dois eixos do contrato) | elíptico exato, girado/cisalhado |
| `Paint::Angular` | `<pattern>` com PNG renderizado pelo pen-raster | SVG não tem gradiente cônico |
| `Paint::Image` (fill, não-tile) | `<image>` em `<defs>` (data URI PNG) + `<use transform>` recortado pela forma | cover/contain/stretch idênticos ao pen-raster; cada imagem é embutida uma vez |
| `Paint::Image` (tile, stroke, texto) | `<pattern>` com `<use>` da imagem | |
| `Stroke` center | `stroke`, `stroke-width`, `linecap`, `linejoin`, `miterlimit`, `dasharray` | |
| `Stroke` inside/outside | traço com 2× a largura recortado pela forma (inside) ou pelo complemento (outside, `clip-rule=evenodd`) | caminhos abertos sempre center, como no raster |
| `Stroke` em retângulo (larguras por lado ou inside/outside sem tracejado) | anel exato `<path fill-rule="evenodd">` | mesma geometria do `rect_ring` do pen-raster |
| `Shadow::Drop` | cópia da forma (crescida por `spread`) transladada, `filter` `feGaussianBlur` (σ = blur/2), `opacity` = alfa | desenhada antes dos fills |
| `Shadow::Inner` | retângulo da cor com `mask` (forma deslocada/encolhida em preto), desfocado e recortado pela forma | desenhada depois dos fills |
| `PushClip`/`PopClip` | `<clipPath>` encadeado (`clip-path` no próprio clipPath = interseção) aplicado nas **folhas** | não envolve grupos: assim blend modes continuam vendo o fundo |
| `PushLayer` opacity/blend/blur | `<g opacity style="mix-blend-mode:..." filter>` | grupo isolado, como a camada offscreen do raster |
| `PushLayer` backdrop_blur | pen-raster renderiza tudo o que veio antes + o blur; vira `<image>` recortada nos bounds da camada e no clip ativo | fallback rasterizado |
| `Text` (modo `Text`) | `<text x y xml:space="preserve">` por `TextLine` + `<tspan>` por run | família (`'Nome', sans-serif` ou genérico), tamanho, peso, itálico, `letter-spacing`, `text-decoration`, paint |
| `Text` (modo `Outlines`) | `<path>` por grupo de glifos do mesmo run | `RasterContext::text_outlines`: mesmo shaping/fontes (cosmic-text + pen-fonts) do pen-raster, inclusive justify e decorações |
| `TextBlock::clip` | `<clipPath>` do retângulo do bloco | |

Saída determinística: ids sequenciais (`c` clip, `g` gradiente, `p` pattern, `i` imagem, `f`
filtro, `m` máscara) na ordem de pintura, números com no máximo 3 casas (6 nas matrizes),
filtros iguais deduplicados. Isso permite goldens de texto exato.

## PDF: escolha da biblioteca

Avaliadas:

| Opção | Prós | Contras |
|---|---|---|
| `pdf-writer` direto | controle total, zero dependências | reimplementar tudo: gradientes (shading functions), soft masks, blend, imagens, subset/embedding de fontes (CIDFontType2, ToUnicode), filtros — milhares de linhas |
| `krilla` 0.8 | API de alto nível, fontes com subset, cores | não sabe SVG: ainda teríamos um segundo backend completo; `krilla-svg` puxa `usvg`/`resvg` 0.47 e `tiny-skia` 0.12 (versões duplicadas na árvore) |
| **`svg2pdf` 0.13** (escolhida) | reaproveita o SVG já gerado (um único mapeamento a manter); fontes embutidas com subset (TrueType/CFF, ToUnicode → texto selecionável); gradientes, clips, masks, blend modes e imagens vetoriais/sem perda; usa `usvg` 0.45 / `fontdb` 0.23 / `tiny-skia` 0.11, **as mesmas versões já presentes** no workspace | grupos com filtro (sombras, blur de camada) são rasterizados (em `raster_scale`); o texto é re-shapeado pelo usvg (rustybuzz) |

Menos código com melhor fidelidade: o PDF herda todo o mapeamento do SVG, que já é testado
pixel a pixel contra o pen-raster. Páginas: cada SVG vira um Form XObject (`svg2pdf::to_chunk`)
e `pdf-writer` 0.12 monta o documento (uma página por `ExportOptions`, MediaBox = região×scale
em pt, grupo de transparência isolado por página).

Fontes do PDF: o banco (`fontdb`) começa com as fontes embutidas (Inter,
`pen_fonts::system::bundled_db`), recebe as famílias instaladas que o documento cita
(`load_family`, nome canônico reescrito no SVG), os genéricos serif/monospace se usados, e as
famílias de fallback da plataforma (`pen_fonts::system::fallback_families`) só se algum
caractere não tiver glifo nas anteriores. Com `TextMode::Outlines` nenhuma fonte é embutida.

## Testes

- `tests/golden.rs`: as 24 cenas do golden do pen-raster são exportadas, rasterizadas com
  `resvg` e comparadas com a renderização do pen-raster (tolerâncias: vetor puro 16/255 em até
  1% dos pixels; blur, imagem ampliada e fallbacks 32/255; texto real 96/255 em 6%, pois o
  shaping difere). Cenas simples também têm golden de texto `tests/golden/*.svg`
  (`PEN_EXPORT_UPDATE_GOLDEN=1` regrava; divergência vira `*.actual.svg`, falha de pixel vira
  `*.actual.png` + `*.raster.actual.png`).
- `tests/pdf.rs`: relê com `lopdf` — nº de páginas, MediaBox (inclusive `scale`), fontes com
  `FontFile*` (embutidas), operadores de texto no conteúdo, modo outlines sem fontes.
- `tests/corpus.rs`: todo `testdata/{legacy,user}/*.pen` exporta SVG (texto e curvas, relido
  pelo usvg) e PDF (documento + uma página por frame de topo) sem pânico; saídas em
  `target/pen-export-corpus/`.
- `benches/export.rs`: export SVG do maior arquivo do corpus (`cargo bench -p pen-export`).

## Lacunas conhecidas de fidelidade

- **Justify no modo texto**: o espaço extra entre palavras não é aplicado (o SVG não tem
  `word-spacing` por linha confiável); use `TextMode::Outlines` para justificado exato.
- **Shaping no modo texto**: quem abre o SVG/PDF re-shapeia; kerning e posição sub-pixel podem
  diferir levemente do canvas, e fontes ausentes no visualizador caem no `sans-serif`.
- **Glifos coloridos (emoji)** não têm contorno: somem no modo `Outlines`.
- **Spread negativo em sombra de `Path`** (drop) é ignorado; spread em `Path` usa traço
  arredondado (igual ao raster, mas sem a subtração `DestinationOut`).
- **Outside stroke em paths com auto-interseção não-zero**: o complemento usa even-odd.
- **Backdrop blur** é uma imagem: se estiver dentro de uma camada com opacidade, o fundo recebe a
  opacidade dessa camada de novo. Custo proporcional à área (renderiza o que vem antes).
- **Gradiente angular** é raster (máx. 4096 px por lado).
- **PDF**: sombras e blur de camada saem rasterizados (svg2pdf rasteriza grupos com filtro);
  páginas muito grandes (> 14 400 pt) são válidas mas alguns leitores limitam o tamanho.
- Blur: SVG usa gaussiano (o pen-raster usa 3 box blurs); diferenças de poucos níveis.
