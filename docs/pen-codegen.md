# pen-codegen: export de código (HTML + CSS / HTML + Tailwind)

`crates/pen-codegen` transforma uma subárvore do `SceneDoc` (cena resolvida) mais o layout
calculado por `pen-layout` em código: **HTML + CSS** (`index.html` + `styles.css`) ou
**HTML + Tailwind v4** (`index.html` com classes utilitárias). Equivale aos formatos
`html-css` / `html-tailwind` do `Export` do Pen. Não depende de GPUI.

## API

```rust
use pen_codegen::{CodeFormat, CodegenOptions};

let opts = CodegenOptions {
    format: CodeFormat::HtmlTailwind,   // ou HtmlCss (padrão)
    scaffold: true,                     // documento completo (doctype/head/body); false = só o markup
    component_comments: true,           // <!-- component: Nome --> antes de cada instância
    layer_names: false,                 // data-name="<nome da camada>"
    layer_ids: false,                   // data-id="<id path>"
    google_fonts: true,                 // <link> do Google Fonts por família usada
    title: None,                        // <title> (padrão: nome do primeiro nó)
};
// roots vazio = todos os nós de topo; o layout precisa estar atualizado (Pipeline::rebuild).
let out = pen_codegen::generate(&scene, &pipe.layout, &[key], &opts);
out.write_to(dir)?;                 // dir/index.html, dir/styles.css, dir/assets/*
out.write_html_as(&dir.join("x.html"))?; // HTML com outro nome; CSS e assets ao lado
// out.html, out.css (Option), out.assets (origem -> caminho relativo), out.warnings
```

- **Um nó**: o elemento raiz é o próprio nó (sem `left/top`).
- **Vários nós**: um `<div class="canvas">` com o tamanho da união dos retângulos (mundo) e cada
  nó em `position: absolute` na sua posição relativa.
- Saída **determinística** (sem hashes nem ids aleatórios): ids de `<defs>` SVG são contadores
  (`pen-g1`, `pen-c2`…), classes vêm dos nomes.

### Onde está plugado

- **pen-script**: `Export(ids, "html-css" | "html-tailwind", outputPath)` →
  `host::export_code`. Todos os ids viram **uma** página. `outputPath` é um diretório
  (`index.html` + `styles.css` + `assets/`) ou um caminho terminado em `.html`.
- **CLI**: `openpen-cli export-code arquivo.pen [--node ID]... --format html-css|html-tailwind
  -o DIR [--theme eixo=valor] [--no-scaffold] [--layer-names] [--layer-ids] [--no-google-fonts]`.

## Estrutura da saída

- Árvore intermediária neutra (`El`: tag, declarações CSS, atributos, filhos, texto, SVG
  bruto). Dois emissores:
  - **html-css**: cada elemento recebe uma classe derivada do nome da camada (`slug`:
    minúsculas ASCII, `-`; sem nome → tipo: `frame`, `text`, `icon`, `rect`, `image`…). Elementos
    com **o mesmo nome e as mesmas declarações** compartilham a classe (instâncias iguais de um
    componente geram uma regra só); diferentes ganham `nome-2`, `nome-3`… em ordem de documento.
  - **html-tailwind**: as declarações viram utilitários v4 (`flex`, `flex-col`, `items-center`,
    `gap-[12px]`, `p-[16px]`/`px-`/`py-`/`pt-`…, `w-[320px]`, `w-full`, `w-fit`, `flex-1`,
    `shrink-0`, `self-stretch`, `rounded-[8px]`, `bg-[#hex]`, `bg-(color:--var)`,
    `text-[14px]`, `font-semibold`, `leading-[1.2]`, `tracking-[..]`, `shadow-[..]`,
    `blur-[..]`, `backdrop-blur-[..]`, `mix-blend-*`, `object-cover`, `origin-top-left`…). O que
    não tem utilitário vira propriedade arbitrária `[prop:valor]` (espaços → `_`). Carrega
    `@tailwindcss/browser@4` do CDN.
- Regras base (html-css em `styles.css`; Tailwind num `<style>` em `@layer base`):
  `box-sizing: border-box`, `margin: 0` em `body`/`p`, `img, svg { display: block }` e
  **`[data-pen-root], [data-pen-root] * { position: relative }`**: o elemento de topo leva o
  atributo `data-pen-root`; com todo elemento posicionado, filhos absolutos resolvem contra o pai
  e a pintura segue a ordem do documento, como no Pen (sem isso um filho absoluto pintaria por
  cima de irmãos posteriores não posicionados).

## Mapeamento

| Pen | HTML/CSS | Observações |
|---|---|---|
| `frame` / `group` / `rectangle` | `<div>` | `script` também (com a saída gerada como filhos) |
| `frame` com `layout` horizontal/vertical | `display: flex`, `flex-direction: column` (vertical) | `flex-wrap` não existe no Pen |
| `gap`, `padding` | `gap`, `padding` (atalho de 1/2/4 valores) | `layoutIncludeStroke` soma `stroke × (1 / 0,5 / 0)` ao padding (inside/center/outside), igual ao taffy |
| `justifyContent` | `justify-content: center / flex-end / space-between / space-around` | `start` omitido |
| `alignItems` | `align-items: flex-start / center / flex-end` | sempre emitido (o padrão do CSS é `stretch`, que o Pen não tem) |
| filho de flex (fluxo) | `flex-shrink: 0` | no Pen nada encolhe, exceto `fill_container` |
| tamanho fixo | `width/height: Npx` | |
| `fit_content` | nada (o CSS abraça) — na raiz `fit-content` | só para frames flex e textos; frame sem layout / grupo / formas usam o tamanho calculado em px (CSS não abraça filhos absolutos) |
| `fill_container` no eixo principal | `flex: 1 1 0; min-width/min-height: 0` | |
| `fill_container` no eixo cruzado | `align-self: stretch` | |
| `fill_container` fora de flex | tamanho calculado (px) | |
| `layout: none`, grupos, `layoutPosition: absolute` | `position: absolute; left; top` (posição do layout) | |
| `cornerRadius` | `border-radius` (1, 2 ou 4 valores) | elipse cheia: `border-radius: 50%` |
| fill sólido | `background-color` | |
| vários fills | `background: camada-topo, …, cor-base` | sólidos acima da base viram `linear-gradient(c, c)` |
| gradiente linear | `linear-gradient(Adeg, stops)` | ângulo e posições dos stops recalculados para a linha de gradiente do CSS: **exato** para qualquer `center/size/rotation` |
| gradiente radial | `radial-gradient(RXpx RYpx at CX CY, stops)` | exato sem rotação (ou múltiplos de 90°); com rotação usa os comprimentos dos eixos |
| gradiente angular | `conic-gradient(from Adeg at CX CY, stops)` | |
| fill de imagem | `url("assets/x.png") center / cover|contain|100% 100% no-repeat` | retângulo/frame sem filhos com uma única imagem vira `<img src alt>` com `object-fit` (opacidade da imagem dobrada na `opacity`) |
| `blendMode` de fill | 1 fill: `mix-blend-mode`; vários: `background-blend-mode` | o Pen mistura com o que está atrás do nó; `mix-blend-mode` mistura o elemento todo |
| stroke de largura uniforme | `outline: W solid C` + `outline-offset: -W` (inside) / `-W/2` (center) / — (outside) | não afeta o layout (como no Pen), segue os raios e pinta acima dos filhos |
| stroke por lado | `box-shadow` (faixas `inset` para dentro, externas para fora; center = metade de cada) | |
| stroke com gradiente/vários paints | cor do primeiro stop / só o paint do topo | warning |
| `shadow` outer/inner | `box-shadow: [inset] x y blur 0 cor` | ordem invertida (CSS lista o de cima primeiro); blur do Pen = raio do CSS (σ = blur/2) |
| `blur` | `filter: blur(raio/2)` | o raio do Pen é 2σ; `blur()` do CSS recebe σ |
| `background_blur` | `backdrop-filter` (+ `-webkit-`) | |
| `opacity` | `opacity` | |
| `rotation` | `transform: rotate(-Rdeg); transform-origin: 0 0` | anti-horário em torno do canto superior esquerdo, como no Pen; não afeta o layout (igual) |
| `flipX/flipY` | `scale(-1, 1)` em torno do centro | combinado com a rotação via `translate` |
| `clip` | `overflow: hidden` | recorta com os raios |
| `text` | `<p>` | `white-space: pre` (`auto`) ou `pre-wrap` (largura fixa); quebras de linha preservadas sem indentação |
| família / tamanho / peso / itálico | `font-family: "Família", sans-serif|serif|monospace`, `font-size`, `font-weight` (≠ 400), `font-style` | fallback genérico pelo nome |
| `lineHeight` | `line-height: k` (sem unidade); 0 → `1.2` | 1,2 é o automático do pen-layout |
| `letterSpacing`, `textAlign`, `underline`, `strikethrough` | `letter-spacing`, `text-align`, `text-decoration-line` | |
| `textAlignVertical` (fixed-width-height) | `display: flex; flex-direction: column; justify-content` | |
| cor do texto | `color` (fill do topo) | gradiente/imagem: `background-image` + `background-clip: text` + `color: transparent` |
| sombra de texto / stroke de texto | `text-shadow` / `-webkit-text-stroke` | |
| `icon` | `<svg viewBox>` inline com o contorno do glifo (`bundled_icon`), `fill`/`stroke="currentColor"` | a cor vai em `color` (aceita `var()`); conjuntos de traço (lucide/feather) viram `stroke` arredondado |
| `path`, `polygon`, `line`, elipse parcial/anel | `<svg viewBox="0 0 w h" preserveAspectRatio="none" overflow="visible">` com `<path>` | geometria de `pen-layout` (`fit_path`, `polygon_path`, `ellipse_path`) |
| stroke em SVG | `stroke`, `stroke-width`, `stroke-linecap/linejoin` | inside: largura 2× + `clip-path` da forma; outside: 2× + `mask` (forma em preto); caminhos abertos sempre center |
| gradiente/imagem em SVG | `<linearGradient>` / `<radialGradient gradientTransform>` (`userSpaceOnUse`) / `<pattern>` com `<image>` | angular em SVG → cor sólida (warning) |
| sombras / blur em SVG | `filter: drop-shadow(...) blur(...)` | sombra interna não exportada |
| `browser` | `<iframe src>` | |
| `note`, `connection`, ref quebrada, tipos desconhecidos | omitidos | ref quebrada gera warning |
| componente / instância | markup repetido; `<!-- component: Nome -->` antes de cada instância | overrides (`descendants`) já vêm aplicados pela cena |

### Variáveis e temas

- Cada variável do documento principal (`color`, `number`, `string`; `boolean` é ignorada) vira
  uma custom property: `color.primary` → `--color-primary` (letras, dígitos e `-`;
  colisões ganham `-2`). Valores que referenciam outra variável viram `var(--outra)`.
- `:root { ... }` recebe o valor padrão (a última entrada sem `theme`, ou o valor resolvido
  para o tema padrão do documento). Cada entrada com `theme` vira um bloco
  `[data-<eixo>="<valor>"] { ... }` (vários eixos: `[data-a="x"][data-b="y"]`), na ordem do
  arquivo.
- Se o documento tem `themes`, o elemento de topo recebe `data-<eixo>="<valor>"` do seu tema
  efetivo, e cada descendente cujo tema muda (`theme` no nó) recebe os eixos alterados — o
  navegador resolve os `var()` pelo ancestral mais próximo, como o Pen.
- Uso: as propriedades ligadas a variáveis (`fill`/`stroke` sólidos, cor de sombra, `gap`,
  `padding`, `cornerRadius`, `strokeWidth` uniforme, `opacity`, `width/height`, `fontSize`,
  `fontFamily`, `fontWeight`, `letterSpacing`, `lineHeight`) saem como `var(--x)`; números em
  comprimentos como `calc(var(--x) * 1px)` (os números ficam sem unidade na definição, então
  servem também para `opacity`/`line-height`).
- As ligações são lidas do schema (a cena já está resolvida): o próprio nó (documento) ou, para
  nós expandidos de componentes, os patches `descendants` das instâncias envolventes seguidos do
  nó do componente (inclusive instâncias aninhadas `a/b`).
- No Tailwind, as variáveis vão num `<style>` (`:root` + blocos de tema) e são usadas com
  `bg-(color:--x)`, `text-(color:--x)` ou valores arbitrários `[gap:calc(var(--x)_*_1px)]`.

### Fontes e assets

- `google_fonts`: um `<link>` por família (pesos e itálicos usados), exceto genéricas
  (`sans-serif`, `system-ui`…). Uma família inexistente no Google Fonts só quebra o próprio link.
- Imagens de fills com caminho relativo ao `.pen` são copiadas para `assets/<nome>` (nome
  sanitizado, deduplicado) e referenciadas por URL relativa; `http(s):` e `data:` ficam como
  estão; arquivo inexistente mantém a URL original e gera warning.

## Testes

- Unitários (`src/*`): formatação de números/cores, ângulos e stops de gradientes, `path d`,
  slugs/fontes, blocos de variáveis/temas, utilitários Tailwind.
- `tests/mapping.rs`: uma regra por teste em documentos mínimos (flex, fill/fit, absolutos,
  fills/strokes/efeitos, texto, SVG, variáveis/temas, classes de componentes, opções, Tailwind).
- `tests/golden.rs`: fixtures `tests/fixtures/{card,shapes,text}.pen` → `tests/golden/<fixture>.<formato>/`
  (comparação exata; `PEN_CODEGEN_UPDATE_GOLDEN=1` regrava; divergência vira `*.actual`). Também
  verifica determinismo e grava a saída completa (com assets) em `target/pen-codegen-golden/`.
- `tests/corpus.rs`: todos os frames de topo de `testdata/user` e `testdata/legacy` nos dois
  formatos (tags balanceadas, CSS balanceado); saídas em `target/pen-codegen-corpus/`. Pula sem
  o corpus. ~124 frames × 2 formatos em ~1 s (debug).
- Visual (manual): `openpen-cli render` (PNG) vs. screenshot do HTML no Edge headless
  (`msedge --headless=new --screenshot --window-size=W,H --virtual-time-budget=8000`) nas três
  fixtures e em frames de `t-process-recorder`, `smart_docs` e `genie`: layout, cores,
  gradientes, strokes, sombras, ícones e ordem de pintura coincidem; as diferenças restantes são
  de fonte (métricas do navegador vs. cosmic-text, e fontes que o pen-raster não tem e o Google
  Fonts tem), que mudam quebras de linha em textos de largura fixa no limite.

## Lacunas conhecidas

- **Medida de texto**: o navegador mede com a própria fonte; textos de largura fixa podem quebrar
  em outro ponto e textos `auto` ficar alguns px mais largos/estreitos que no Pen.
- **Gradiente radial rotacionado** (não múltiplo de 90°) é aproximado por uma elipse alinhada aos
  eixos; em caixas não quadradas o Pen gera eixos não perpendiculares.
- **Stroke**: largura por lado via `box-shadow` não faz cantos/junções perfeitos com raios;
  strokes de gradiente/imagem em caixas viram cor sólida; vários paints de stroke → só o do topo;
  tracejado não existe no modelo.
- **Blend**: `blendMode` de um fill único vira `mix-blend-mode` do elemento inteiro (inclui filhos);
  blend da cor de base com vários fills e blend de sombras não são exportados.
- **Opacidade de fill de imagem** em `background` não tem equivalente direto (warning); no `<img>`
  é dobrada na `opacity`.
- **Sombra interna** em SVG (paths/ícones) e **efeitos/fills de shader** não são exportados
  (warning).
- **Variáveis de bibliotecas importadas** (`$alias:x`) e nós vindos de bibliotecas saem com
  valores literais; `boolean` (ex.: `enabled` ligado a variável) não vira CSS.
- **Temas com vários eixos**: a precedência do Pen (última entrada que casa) é aproximada pela
  ordem dos blocos; entradas de eixos combinados têm especificidade maior.
- `descendants` com substituição tipada no meio de um caminho (`slot/x`) não recupera as ligações
  de variáveis dos nós internos (os valores literais continuam corretos).
- `note` e `connection` são omitidos; `browser` vira `<iframe>` sem zoom/scroll.
- Rich text (spans) sai como texto simples com o estilo do nó.
- Ícones de famílias não embutidas viram uma `<div>` vazia do tamanho do ícone (warning).
