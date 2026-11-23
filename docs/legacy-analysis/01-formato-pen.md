# 01 — Formato de documento `.pen` (Pen v1.2.23)

> Análise de engenharia reversa (somente leitura) do app Electron legado "Pen" (pen.dev, High Agency), v1.2.23,
> com o objetivo de projetar um modelo equivalente em Rust/GPUI. Descreve o comportamento e as estruturas; não
> reproduz código do fornecedor.

Fontes consultadas (relativas ao `app.asar` extraído):

| Fonte | Conteúdo relevante |
|---|---|
| `node_modules/@ha/schema/pen.schema.json` | JSON Schema (draft 2020-12) canônico, `version: const "2.22"`. Campos `_PRIVATE` são ocultados do schema público. |
| `node_modules/@ha/schema/src/generated-types-{public,private}.ts` | Tipos TS gerados do schema. O privado acrescenta `line`, `connection`, rich text, `fileToken`, `fonts`. |
| `node_modules/@ha/schema/src/generated-validator.ts` | Validador gerado (permite `partial` para overrides). |
| `node_modules/@ha/schema/src/index.ts` | `CURRENT_SCHEMA_VERSION = "2.22"`. |
| `out/skills/pen-dev/*.md` | Documentação para agentes de IA (schema minificado, API `execute`, scripts e shaders, componentes). |
| `out/data/*.pen`, `*.lib.pen` | Documentos de exemplo e bibliotecas embarcadas (versões 2.6, 2.8 e 2.13). |
| `node_modules/@ha/shared/src/*` | IDs, documentos embarcados, compartilhamento (zstd). |
| `out/editor/assets/index.js` (minificado) | Cadeia de migração, loader, serializador, runtime de scripts, resolução de variáveis. |
| `out/editor/goodies/` | Scripts (`scripts/*.js`), shaders (`shaders/*.glsl`, `shader-library/assets/*.glsl`) e exemplos (`shader-library/examples/*.pen.json`). |

---

## 1. Visão geral

- Um `.pen` é **JSON UTF-8 puro**, gravado com `JSON.stringify(doc, null, 2)` (indentação de 2 espaços). **Não há
  criptografia nem compressão** no arquivo local. (As instruções do servidor MCP do produto dizem que os arquivos
  são "encrypted". Isso parece ser só uma regra para impedir agentes de lerem os arquivos direto; não há
  criptografia no caminho de leitura/gravação: `load-document.js` lê com `readFile("utf8")`.)
- O loader aceita **JSONC**: se `JSON.parse` falhar, ele remove comentários `//` e `/* */` (preservando
  whitespace) e tenta de novo. Vírgulas finais (*trailing commas*) não são aceitas.
- Arquivo vazio (`""`) equivale a `{"version":"2.22","children":[]}`.
- Bibliotecas de componentes usam a extensão **`.lib.pen`**, com exatamente o mesmo formato. A diferença está no
  comportamento: uma `.lib.pen` não processa os próprios `imports` e só pode ser importada se tiver essa extensão.
- Versão atual do schema: **`2.22`**. Os exemplos embarcados estão em 2.6/2.8/2.13 e são migrados ao abrir
  (ver §11).

### 1.1 Estrutura raiz (`Document`)

| Campo | Tipo | Obrigatório | Descrição |
|---|---|---|---|
| `version` | `"2.22"` | sim | Versão do schema. |
| `children` | `Node[]` | sim | Nós de topo (páginas/telas, componentes, notas, browsers...). |
| `themes` | `{ [eixo: string]: string[] }` | não | Eixos de tema e seus valores possíveis. Chave sem `:`. O **primeiro valor de cada eixo é o default**. |
| `variables` | `{ [nome: string]: VariableDef }` | não | Tokens de design (§7). Nome sem `:`. |
| `imports` | `{ [alias: string]: uri }` | não | Bibliotecas importadas (§8). URI relativa ao `.pen`, segmentos codificados com `encodeURIComponent`, ou `pencil:<nome>.lib.pen`. |
| `fileToken` | `string` (privado) | não | Token estável do arquivo, atribuído (via `crypto.randomUUID()`) no primeiro save se estiver ausente. |
| `fonts` | `FontDef[]` (privado) | não | Fontes customizadas embarcadas/referenciadas (§6.3). |

Ordem de serialização usada pelo app: `version`, `children`, `themes`, `variables`, `imports`, `fileToken`, `fonts`.
Campos vazios são omitidos.

Exemplo mínimo (de `pencil-welcome.pen`, v2.13, encurtado):

```json
{
  "version": "2.13",
  "children": [
    { "type": "frame", "id": "YKsGt", "x": 0, "y": 0, "name": "Welcome to pen.dev",
      "clip": true, "width": 1000, "height": 1458, "layout": "none",
      "children": [
        { "type": "path", "id": "AmD9T", "x": 267.80, "y": 632, "rotation": -45,
          "geometry": "M7 0l0 14m7-7l-7 7-7-7", "width": 366, "height": 366,
          "stroke": "#ff8302ff", "strokeWidth": 50,
          "strokeLinejoin": "round", "strokeLinecap": "round" }
      ] }
  ],
  "themes": { "Mode": ["Light", "Dark"] },
  "variables": { "--white": { "type": "color", "value": "#FFFFFF" } }
}
```

---

## 2. Tipos primitivos e convenções

| Tipo | Representação | Observações |
|---|---|---|
| `Variable` | string que começa com `$` | Liga a propriedade a uma variável: `"$--primary"`, `"$radius/2xl"`, `"$halo:--accent"` (variável de biblioteca). |
| `NumberOrVariable` | `number \| "$var"` | |
| `BooleanOrVariable` | `boolean \| "$var"` | |
| `StringOrVariable` | `string \| "$var"` | **Escape:** uma string literal que começa com `$` é gravada como `"\$..."` (barra invertida + `$`). Na leitura, um prefixo `\$` vira `$` literal. |
| `Color` | `#RGB`, `#RRGGBB` ou `#RRGGBBAA` | A opacidade de cor sólida fica **só** no canal alfa. Os exemplos usam maiúsculas e minúsculas. |
| `SizingBehavior` | regex `^(fit_content\|fill_container)(\(-?[0-9]+(\.[0-9]+)?\))?$` | Ex.: `"fill_container"`, `"fit_content(44)"`. O número entre parênteses é o **fallback**. |
| `Position` | `x`, `y` (number, sem variável) | Relativos ao pai, X para a direita, Y para baixo. **Ignorados** quando o pai tem layout flex (o serializador nem grava). |
| `AffineTransform` | `[a, b, c, d, tx, ty]` | Usado no crop de imagem. |
| `IdPath` | `id(/id)*` | Caminho através de **fronteiras de instância** (não da hierarquia comum). |
| `Theme` | `{ [eixo]: valor }` | Seleção parcial de tema (§7.2). |
| Ângulos | graus | `rotation` de nó: graus **anti-horários** em torno do canto superior esquerdo. |

Regra geral de omissão no serializador: valores default não são gravados (`enabled: true`, `opacity: 1`,
`rotation: 0`, `layout: "horizontal"`, `justifyContent/alignItems: "start"`, `gap: 0`, `padding: 0`,
`strokeAlignment: "center"`, `strokeLinejoin: "miter"`, `strokeLinecap: "butt"`, `textAlign: "left"`,
`textAlignVertical: "top"`, `fontWeight: "400"`, `fontStyle: "normal"`, `letterSpacing: 0`, `lineHeight: 0`
(= automático), `textGrowth: "auto"`, cornerRadius todo zero, `clip: false`).

Compactações aplicadas pelo serializador:
- `padding` `[t,r,b,l]` com `t==b && r==l` vira `[v,h]`, e `[v,v]` vira `v`.
- `cornerRadius` com quatro cantos iguais vira escalar.
- `strokeWidth` com quatro lados iguais vira escalar; senão vira objeto só com os lados diferentes de zero.
- `fill`/`stroke`/`effect` com um único item são gravados como objeto (não array). Uma cor sólida habilitada e
  sem blend vira a string curta `"#hex"` ou `"$var"`.

---

## 3. Nós (`Child`)

União discriminada por `type`. A maioria dos tipos usa `additionalProperties: false` (validação estrita); só
`ref` aceita propriedades extras (os overrides).

| `type` | Público? | Herda | Específicos |
|---|---|---|---|
| `frame` | sim | Rectangleish + Layout + children | `clip`, `placeholder`, `slot` |
| `group` | sim | Entity + children + effects | `width`/`height` só como SizingBehavior (privado) |
| `rectangle` | sim | Rectangleish | — |
| `ellipse` | sim | Entity + Size + Graphics | `innerRadius`, `startAngle`, `sweepAngle` |
| `line` | **privado** | Entity + Size + Graphics | — (definida pela bounding box) |
| `polygon` | sim | Entity + Size + Graphics | `polygonCount`, `cornerRadius` |
| `path` | sim | Entity + Size + Graphics | `geometry`, `viewBox`, `fillRule` |
| `text` | sim | Entity + Size + Graphics + TextStyle | `content`, `textGrowth` |
| `note` | sim | Entity + Size | `content`, `color` |
| `icon` | sim | Entity + Size + Effects | `library`, `icon`, `weight`, `fill` |
| `script` | sim | Entity + Size | `clip`, `scriptUri`, `inputs` |
| `browser` | sim | Entity + Size + Effects + Stroke | `url`, `deviceId`, `zoom`, `scrollX`, `scrollY`, `cornerRadius` |
| `connection` | **privado** | Entity + Stroke | `source`, `target` |
| `ref` | sim | Entity + overrides livres | `ref`, `descendants`, (`children`) |

Tipos legados que só aparecem antes da migração: `image` (<2.3), `icon_font` (<2.12), `prompt` e `context`
(<2.22), `sticky_note` (1.0).

### 3.1 `Entity` (comum a todos)

| Campo | Tipo | Default | Semântica |
|---|---|---|---|
| `id` | string sem `/` | gerado se ausente | Único no documento (§9). |
| `name` | string | — | Nome de camada. |
| `context` | string | — | Descrição em linguagem natural para a IA (ex.: "Vertical sidebar navigation with app logo..."). |
| `reusable` | boolean | `false` | Marca o nó como **componente** (§5). |
| `theme` | `Theme` | — | Sobrescreve eixos de tema para a subárvore. |
| `enabled` | BooleanOrVariable | `true` | Visibilidade. Desligar também é a forma de "apagar" descendentes em instâncias. |
| `opacity` | NumberOrVariable (0..1) | `1` | |
| `flipX`, `flipY` | BooleanOrVariable | `false` | |
| `layoutPosition` | `"auto" \| "absolute"` | `auto` | `absolute` tira o filho do fluxo do layout do pai e passa a usar `x`/`y`. |
| `metadata` | `{ type: string, ...any }` | — | Dados livres, preservados. |
| `rotation` | NumberOrVariable | `0` | Graus anti-horários em torno do canto superior esquerdo. |
| `x`, `y` | number | — | Posição relativa ao pai. |

Ordem de serialização (útil para diffs estáveis): `type`, `id`, `layoutPosition`, `x`, `y`, `name`, `context`,
`theme`, `reusable`, `enabled`, `opacity`, `rotation`, `flipX`, `flipY`, `metadata` e depois os campos do tipo.
Para `ref`, a ordem é `id`, `type`, `reusable`, `ref` e depois os overrides.

### 3.2 `Size`

`width`, `height`: `NumberOrVariable | SizingBehavior` (§4.2).

### 3.3 `Rectangleish` = Entity + Size + Graphics + `cornerRadius`

`cornerRadius`: `NumberOrVariable | [tl, tr, br, bl]` (cada item NumberOrVariable). O exemplo
`["$--radius-pill", "$--radius-xs", "$--radius-xs", "$--radius-pill"]` sugere a ordem tl, tr, br, bl (convenção
CSS).

### 3.4 Detalhes por tipo

- **frame**: container com layout. Defaults do schema: `layout = horizontal`, `width = height = fit_content`,
  `clip = false`.
  - `clip`: recorta o overflow.
  - `placeholder`: marca um frame que a IA está gerando. É apagado no load (`frame`/`ref`) e zerado num save
    iniciado pelo usuário. Não use como estado persistente.
  - `slot: false | string[]`: marca o frame como **slot** de componente. O array lista IDs de componentes
    recomendados para preenchê-lo (ex.: itens de menu). No editor o slot vazio é desenhado com hachura.
- **group**: agrupamento sem layout desde a 2.16 (grupos com layout viraram frames). Bounds = união dos filhos
  habilitados.
- **rectangle**: posição = canto superior esquerdo.
- **ellipse**: definida pela bounding box. `innerRadius` 0..1 é a razão anel interno/externo (0 = sólida,
  1 = oca). `startAngle` em graus anti-horários a partir da direita (default 0). `sweepAngle` vai de -360 a 360
  (default 360), com positivo = anti-horário (o sinal foi invertido na migração 2.6→2.7). Exemplo de spinner:
  `{"type":"ellipse","innerRadius":0.75,"sweepAngle":270,"stroke":"$accent/accent","strokeWidth":2.5,"strokeAlignment":"inner"}`.
- **line** (privado): segmento ao longo da bounding box, desenhado pelo stroke. Uso típico: separador
  `{"type":"line","width":"fill_container","height":0,"stroke":"$--border","strokeWidth":1}`.
- **polygon**: `polygonCount` (lados) e `cornerRadius` escalar.
- **path**: `geometry` = dado SVG `d`. `viewBox [x,y,w,h]` mapeia o espaço SVG para a caixa do nó (default =
  bbox justa da geometria), ou seja, o path é **escalado** para `width × height`. `fillRule`: `nonzero` (default) ou
  `evenodd`.
- **text**: ver §6.
- **note**: anotação tipo post-it no canvas. `content` (texto) e `color` (default `#FEE484`). As notas legadas
  `prompt` (cor `#AEDEFF`) e `context` (cor `#DBDEE1`) viraram `note` na 2.22, e a note perdeu as propriedades
  tipográficas.
- **icon**: glifo de fonte de ícones. `library` ∈ `lucide`, `feather`, `Material Symbols Outlined`,
  `Material Symbols Rounded`, `Material Symbols Sharp`, `phosphor`. `icon` = nome do glifo (ex.:
  `chevron-down`). `weight` vai de 100 a 700, só para fontes variáveis; o serializador omite 200, que parece ser o
  default. Escalado para caber em `width × height`. Cor via `fill`.
- **script**: gera filhos via JavaScript (§10.1).
- **browser**: página web ao vivo no canvas. `url` (vazio = prompt de URL), `deviceId` (preset de emulação;
  ausente = responsivo), `zoom` (default 1), `scrollX`/`scrollY`, `cornerRadius` (**default 16**, não 0). No
  snapshot zip, as prévias vão em `browser-previews/<path>.jpg`.
- **connection** (privado): conector entre objetos. `source`/`target` = `{ path: IdPath, anchor: "center"|"top"|"left"|"bottom"|"right" }`,
  com estilo via Stroke. Veio do formato 1.0 (`connections` no topo). Scripts não podem gerar conexões.
- **ref**: instância de componente (§5).

---

## 4. Modelo de layout

### 4.1 Layout flex (frames)

| Campo | Valores | Default | Notas |
|---|---|---|---|
| `layout` | `"none" \| "horizontal" \| "vertical"` | `horizontal` | `none` = filhos posicionados por `x`/`y`. O serializador **omite `horizontal`**, então um frame sem `layout` é horizontal. |
| `gap` | NumberOrVariable | 0 | Espaço no eixo principal. |
| `padding` | `v \| [vertical, horizontal] \| [top, right, bottom, left]` | 0 | Cada item NumberOrVariable (ex.: `"$spacing/6"`). |
| `justifyContent` | `start\|center\|end\|space_between\|space_around` | `start` | Eixo principal. |
| `alignItems` | `start\|center\|end` | `start` | Eixo cruzado. **Não há `stretch`**: usa-se `fill_container` no filho. |
| `layoutIncludeStroke` | boolean | false | Inclui a espessura do stroke nas medidas do layout. |

- Com `layout: "none"` o serializador não grava gap/padding/justify/align.
- Filhos de um pai com layout não gravam `x`/`y`, a menos que tenham `layoutPosition: "absolute"`.
- Não há wrap, grid nem `flex-grow` numérico: o "grow" é `fill_container` (filhos com fill dividem o espaço
  restante, a confirmar no motor de layout).
- O editor tem um gerador de auto-layout (agrupar seleção) que infere a direção pelos bounds e usa um gap padrão
  de 10.

### 4.2 Sizing (`width` / `height`)

- `number` ou `"$var"`: tamanho fixo.
- `"fit_content"`: abraça os filhos ("hug"). **Exige layout no próprio nó**; sem filhos, usa o fallback.
- `"fill_container"`: ocupa o espaço do pai. **Exige layout no pai**; fora de layout ou com posição absoluta,
  usa o fallback.
- `"fit_content(N)"` / `"fill_container(N)"`: com o fallback `N`. O serializador grava o fallback (o tamanho
  atual) quando o nó tem tamanho fixo calculável.
- Valor inválido cai em `fit_content` com fallback 0.

Valores reais nos exemplos: `fill_container` (1299 ocorrências em frames), `fill_container(1024)`,
`fit_content(44)`, inteiros e floats.

### 4.3 Texto

Para `text`, o tamanho só tem efeito conforme `textGrowth`:
- `auto` (default, omitido): cresce para caber, sem quebra. Ignora width/height.
- `fixed-width`: largura fixa (pode ser `fill_container`), quebra linhas, altura cresce.
- `fixed-width-height`: ambos fixos, pode transbordar.

---

## 5. Componentes, instâncias, overrides e slots

### 5.1 Componente

Qualquer nó com `reusable: true`. Por convenção os componentes ficam em frames de topo (nas bibliotecas, dentro de
um frame do tipo "design system components").

### 5.2 Instância (`ref`)

```json
{ "id": "ISy9G", "type": "ref", "ref": "4otXl", "x": 4, "y": 4,
  "descendants": { "1dsjR": { "content": "Actions" } } }
```

- `ref`: ID do componente (sem `/`). Para componentes de biblioteca importada, o ID vem qualificado com o alias:
  `"halo:GqFgg"` (§8).
- **Overrides da raiz**: qualquer propriedade do nó raiz do componente pode aparecer direto no `ref` (`width`,
  `fill`, `padding`, `layout`, `effect`, `textGrowth` etc.). Por isso o `ref` aceita propriedades extras.
- **`descendants`**: mapa `IdPath → override`.
  - Sem `type`: **patch** de propriedades no descendente.
  - Com `type`: **substituição** completa da subárvore por um novo nó (com `id` e `children` próprios).
  - Chaves com `/` atravessam instâncias aninhadas: `"28dco/6yOzH": {"y": 8}` = descendente `6yOzH` dentro da
    instância `28dco`, que está dentro do componente. Usa-se sempre **um único nível** de `descendants` com
    caminhos, nunca `descendants` aninhados.
  - O patch pode conter `children` para preencher um slot dentro do componente.
- **Exclusão** em instância: `{"enabled": false}` no descendente (não dá para apagar de fato).
- **Instância também pode ser componente** (`reusable: true` num `ref`), o que funciona como variante. Exemplo de
  `halo.lib.pen`:

```json
{ "id": "eCcfa", "type": "ref", "reusable": true, "ref": "GqFgg", "name": "Modal/Icon",
  "descendants": {
    "g2OEs": { "type": "frame", "id": "F0ybG", "name": "Icon Base", "width": 48, "height": 48,
               "fill": "$--accent", "children": [ { "type": "icon_font", "id": "82bCO", "...": "..." } ] },
    "zkEXz": { "enabled": false } } }
```

### 5.3 Slots e `children` em instâncias

Um frame com `slot` dentro de um componente pode ter os filhos trocados na instância. Quando os filhos de um nó
de instância são sobrescritos (`childrenOverridden`), o serializador grava `children` no próprio `ref` (se o slot
é a raiz) ou na entrada de `descendants`. Exemplo (`pencil-heroui.pen`, componente "Tabs" `VYmGr` com
`slot: ["bgGZK","9XUkI"]`):

```json
{ "id": "47sM8", "type": "ref", "ref": "VYmGr", "x": 160, "y": 2157, "width": "fit_content",
  "children": [
    { "id": "ajKOI", "type": "ref", "ref": "bgGZK", "width": "fit_content" },
    { "id": "EqzVB", "type": "ref", "ref": "9XUkI", "width": "fit_content",
      "descendants": { "ogFcf": { "content": "Notifications" } } } ] }
```

### 5.4 Invariantes de instância (pelo serializador e loader)

- A estrutura da instância tem de bater com a do protótipo (mesmo número de filhos, mesma ordem), exceto nos nós
  com `childrenOverridden` ou nos substituídos ("unique"). Divergência é logada como erro.
- Os IDs dos nós internos de uma instância são os IDs do componente. Por isso todo acesso usa caminho
  `instanciaId/filhoId`.
- Validação no load: `ref` inválido e caminho de override inexistente geram aviso ("had problems") e o documento
  continua abrindo.
- Reparo automático: se o primeiro filho de um nó é um `ref` com `id == ref` (auto-referência corrompida), os
  `children` desse nó são descartados.

---

## 6. Texto

### 6.1 `TextStyle`

| Campo | Tipo | Default |
|---|---|---|
| `fontFamily` | StringOrVariable | — (ex.: `"Inter"`, `"$--font-primary"`) |
| `fontSize` | NumberOrVariable | sempre gravado |
| `fontWeight` | StringOrVariable | `"400"` (exemplos: `"normal"`, `"500"`, `"600"`, `"700"`...) |
| `fontStyle` | StringOrVariable | `"normal"` |
| `letterSpacing` | NumberOrVariable | 0 (px) |
| `lineHeight` | NumberOrVariable | 0 = métrica da fonte. **Multiplicador** de fontSize (ex.: `1.399999976158142`, um float32 promovido). |
| `textAlign` | `left\|center\|right\|justify` | `left` |
| `textAlignVertical` | `top\|middle\|bottom` | `top` |
| `underline`, `strikethrough` | BooleanOrVariable | false |
| `href` | string | — (link) |

A cor do texto vem de `fill` (Graphics), e texto também aceita stroke e effects.

### 6.2 Conteúdo e rich text

- `content`: `StringOrVariable` (pode estar ligado a uma variável string). `\n` quebra linha.
- **Privado/experimental**: `content` pode ser um array de *spans* `{ content: string, ...TextStyle }`, ou seja,
  runs de rich text com estilo por trecho. Nenhum exemplo usa isso, e não achei no deserializador do editor um
  tratamento evidente para arrays. Trate como *preservar sem interpretar*.

### 6.3 Fontes customizadas (`fonts`, privado)

Array de `{ name, url, style?: "normal"|"italic", weight?: number | [min,max], axes?: [{tag, start, end}] }`.
`url` é relativa ao `.pen` (no zip fica em `fonts/`). Entradas com o mesmo `name` viram estilos de uma família;
peso default 400; `weight` em faixa = fonte variável (usa o mínimo como base). Erros por entrada são acumulados e
não abortam o load. O editor também embarca um catálogo de Google Fonts (`fonts.gstatic.com`, com eixos
`wght`/`wdth`/`opsz`).

---

## 7. Variáveis, temas e tokens

### 7.1 Definição

```json
"variables": {
  "--white":        { "type": "color",  "value": "#FFFFFF" },
  "--font-primary": { "type": "string", "value": [ { "value": "Inter", "theme": { "Mode": "Light" } },
                                                   { "value": "Inter", "theme": { "Mode": "Dark" } } ] },
  "--background":   { "type": "color",  "value": [
      { "value": "#fafafa" },
      { "value": "#0a0a0a", "theme": { "Mode": "Dark" } },
      { "value": "#020617", "theme": { "Mode": "Dark", "Base": "Slate" } } ] }
}
```

- `type` ∈ `boolean | color | number | string`.
- `value`: um valor só, ou uma lista de `{ value, theme? }`.
- Valores podem referenciar **outras variáveis** (`"$outra"`). O editor rastreia dependências e detecta ciclos.
- Nomes são livres, sem `$` na definição e **sem `:`** (o `:` fica reservado para o namespace de biblioteca).
  Convenções vistas: `--primary` (estilo CSS custom property), `accent/hover`, `radius/2xl`, `spacing/6`.
- Parsing leniente: em `boolean`, uma string diferente de `"false"` vale true; em `number`, uma string passa por
  `parseFloat`.
- No uso, qualquer propriedade tipada como `*OrVariable` aceita `"$nome"`; padding, cornerRadius, gap, cores de
  gradiente, uniforms de shader e inputs de script também aceitam.

### 7.2 Temas e resolução

- `themes` declara eixos (`"Mode": ["Light","Dark"]`, `"Base": [...]`, `"Accent": [...]`; no HeroUI,
  `"semantic": ["light","dark"]`, `"primitives": ["default"]`, `"typography": ["default"]`).
- **Tema default** = primeiro valor de cada eixo, aplicado na raiz.
- **Tema efetivo de um nó** = tema do pai sobrescrito pelos eixos do `theme` do próprio nó (ex.: frame com
  `"theme": {"Mode": "Dark"}` mostra a versão dark da subárvore).
- **Resolução de valor**: percorre a lista de valores **do último para o primeiro** e retorna o primeiro cujo
  `theme` tem *todos* os pares eixo=valor iguais ao tema efetivo. Uma entrada sem `theme` casa sempre. Por isso a
  convenção é: entrada default primeiro e especializações (mais específicas) depois. Se nada casar, usa o default
  do tipo.
- Ao serializar, uma lista com um único valor sem tema volta a ser valor simples.

---

## 8. Bibliotecas (`.lib.pen`) e imports

- `imports: { "<alias>": "<uri>" }`. Exemplos de URI: `"./libs/halo.lib.pen"` (relativa ao `.pen`, segmentos
  URL-encoded desde a 2.10) ou `"pencil:halo.lib.pen"` (embarcada). As bibliotecas embarcadas são `shadcn`,
  `lunaris`, `halo` e `nitro` (`out/data/*.lib.pen`).
- No load, cada import é carregado como **subdocumento cujo id é o alias**. Todos os IDs desse subdocumento
  recebem o prefixo `alias:`, incluindo `id`, `ref`, chaves de `descendants` (cada segmento do caminho) e itens
  de `slot`. Variáveis e eixos de tema também são qualificados (`alias:nome`).
- No documento principal, as referências a elementos da biblioteca ficam **qualificadas**:
  `"ref": "halo:GqFgg"`, `"fill": "$halo:--accent"`. Ao salvar cada documento, só o prefixo do próprio
  documento é removido.
- `.lib.pen` **não processa `imports`**, então não há imports transitivos.
- Import que falha: o documento abre assim mesmo, com um aviso e um placeholder "quebrado" para o alias.
- A UI de "Libraries" só aceita arquivos `.lib.pen`. Em documentos *managed* (gerenciados pelo app), a cópia de
  dependências gera nomes únicos.
- Os documentos de exemplo (`pencil-*.pen`) **copiam** os componentes em vez de importar (ex.: `pencil-halo.pen`
  = `halo.lib.pen` + telas de demo + notas `prompt`).
- O guia para agentes diz "não é possível referenciar componentes entre arquivos". Isso vale para a API
  `execute`, mas o formato suporta imports.

---

## 9. IDs

- Gerador atual (`@ha/shared/src/unique-id.ts` e editor): `short-unique-id` com **5 caracteres alfanuméricos**
  (`[0-9A-Za-z]`). Se o primeiro caractere for dígito, prefixa uma letra aleatória (fica com 6 caracteres) para
  que o ID seja um identificador JS válido na API `execute`. Probabilidade de colisão estimada em ~4e-8 para 1000
  nós.
- IDs legados podem começar com dígito (`"45PNT"`, `"1dsjR"`) e precisam ser aceitos.
- O formato 1.0 usava UUID (`crypto.randomUUID`) quando faltava ID.
- Restrições: sem `/` (separador de IdPath) e, na prática, sem `:` (namespace de biblioteca). Unicidade
  **global no documento**, não só entre irmãos.
- Load: IDs ausentes são gerados. IDs duplicados são regenerados e o usuário recebe o aviso "The document
  contained duplicate identifiers".
- Saídas de script têm os IDs **descartados** e regerados.
- `fileToken`: `crypto.randomUUID()`.

---

## 10. Pintura: fills, strokes, efeitos

### 10.1 `Fill` (e `Fills = Fill | Fill[]`)

A lista é desenhada na ordem do array (o último fica por cima, presumivelmente). Todo fill tem `enabled?` e
`blendMode?`.

| `type` | Campos | Notas |
|---|---|---|
| (string) | `"#hex"` ou `"$var"` | Forma curta de cor sólida habilitada. |
| `color` | `color` | Opacidade só via alfa do hex. |
| `gradient` | `gradientType: linear\|radial\|angular`, `opacity` (0..1), `center {x,y}` (normalizado na bbox, default .5,.5), `size {width,height}` (normalizado, default 1,1), `rotation` (graus anti-horários, 0° = para cima, 90° = esquerda, 180° = baixo), `colors: [{color, position}]` | Linear: `size.height` = comprimento do gradiente e `width` é ignorado. Radial/angular: diâmetros da elipse. |
| `image` | `url` (relativa ao `.pen`, `http(s)`, ou vazia), `mode: cover\|contain\|stretch` (default `cover`; ausência após a migração 2.19 vira `stretch`), `opacity`, `transform` (crop afim) | O crop mapeia coordenadas normalizadas da imagem para a caixa 0..1 do crop: `[2,0,0,1,-1,0]` = metade direita. **Não existe nó `image`.** |
| `shader` | `url` (GLSL relativo), `uniforms`, `opacity` | Ver §12.2. |
| `mesh_gradient` | `columns`, `rows`, `colors[]` (um por vértice), `points[]` (row-major, `[x,y]` em 0..1 ou `{position, leftHandle?, rightHandle?, topHandle?, bottomHandle?}`), `opacity` | Grade de Bézier. Handles omitidos = automáticos, com offset default `±0.25/(n-1)` no eixo. Pontos arredondados a 4 casas. As bordas devem manter as posições default. |

`BlendMode`: `normal, darken, multiply, linearBurn, colorBurn, light, screen, linearDodge, colorDodge, overlay,
softLight, hardLight, difference, exclusion, hue, saturation, color, luminosity`. Note que o "lighten" do Figma
virou `light`.

Exemplo (gradiente linear, de `pencil-demo.pen`):

```json
{ "type": "gradient", "gradientType": "linear", "enabled": true, "rotation": 135,
  "size": { "height": 1 },
  "colors": [ { "color": "#FF5C00", "position": 0 }, { "color": "#FF8A4C", "position": 1 } ] }
```

### 10.2 Stroke (`CanHaveStroke`)

| Campo | Tipo | Default |
|---|---|---|
| `stroke` | `Fills` | — (qualquer tipo de fill: cor, gradiente, imagem, shader) |
| `strokeWidth` | NumberOrVariable ou `{top,right,bottom,left}` | — |
| `strokeAlignment` | `inner\|center\|outer` | `center` (omitido) |
| `strokeLinejoin` | `miter\|bevel\|round` | `miter` |
| `strokeLinecap` | `butt\|round\|square` | `butt` |

Bordas por lado (`{"bottom": 1}`) servem para separadores de tabela. A migração 2.14→2.15 força
`strokeLinejoin: "round"` nos casos não uniformes, para preservar a aparência.

Formato antigo (< 2.12): `"stroke": {"align":"inside","thickness":1,"fill":"$--input","join":"round","cap":"round"}`.

### 10.3 Efeitos (`effect: Effect | Effect[]`)

| `type` | Campos |
|---|---|
| `blur` | `radius` (blur do nó inteiro) |
| `background_blur` | `radius` (blur do fundo, "glass") |
| `shadow` | `shadowType: inner\|outer` (default outer), `offset {x,y}`, `blur`, `color` (default `#000000`), `blendMode` |
| `shader` | `url`, `uniforms` (pós-processa o nó renderizado; §12.2) |

Todos aceitam `enabled`. **Não existe `spread`**: ele foi removido na 2.19. Os exemplos antigos (2.6/2.8) ainda
trazem `"spread": -2`.

---

## 11. Versionamento e migrações

O loader aplica uma **cadeia linear** de funções `vN → vN+1` sobre o JSON cru (antes da tipagem), cada uma
mutando o objeto e atualizando `version`. Se depois da cadeia `version != "2.22"` (ex.: arquivo de uma versão
mais nova), o editor mostra "Unsupported file format X. Update pen.dev" e aborta. A validação de schema roda antes
dessa checagem e é **não fatal** (só log/telemetria).

| De → Para | Transformação |
|---|---|
| 1.0 → 2.0 | Formato totalmente diferente: nós com `properties` (`fillColor`, `fillGradient`, `strokeColor`, `strokeWidth`, `blurFilter`, `dropShadowFilter`, `textContent`, `pathData`, `cornerRadius`, `frameMaskDisabled`...); `sticky_note` → `note`; o array de topo `connections` (`sourceNodeId`, `sourceAnchor`...) vira nós `connection`. A rotação é convertida de unidade e tem o sinal invertido. IDs ausentes viram UUID. |
| 2.0 → 2.1 | `frameMaskDisabled` → `clip = !v`; `disabled` → `enabled = !v` (recursivo em qualquer objeto). |
| 2.1 → 2.2 | `ref.overrides: [{path?, property, value}]` → propriedades da raiz ou `descendants[path][property]`. |
| 2.2 → 2.3 | Nó `image` → `rectangle` com `fill: {type:"image", url, mode:"fill"}`. |
| 2.3 → 2.4 | Objeto `layout {mode, spacing, includeStroke, padding, justify, align}` → campos planos `layout/gap/layoutIncludeStroke/padding/justifyContent/alignItems`. frame/group sem layout ganham `layout:"none"` explícito. Frame sem `width/height` → 0; sem `clip` → `true`. |
| 2.4 → 2.5 | `text.size {width,height}` → `width/height`. |
| 2.5 → 2.6 | Reparametrização dos gradientes: `size.width ← antigo height`. Linear: `size.height ← antigo width/2` e `center` deslocado ao longo da rotação; outros: `size.height ← antigo width`. Afeta `fill` e `stroke.fill`, incluindo `descendants`. |
| 2.6 → 2.7 | `sweepAngle` invertido (ellipse e ref). |
| 2.7→2.8, 2.8→2.9 | Só troca de versão (adições compatíveis). |
| 2.9 → 2.10 | Valores de `imports` passam por `encodeURIComponent` por segmento (exceto `pencil:`). |
| 2.10 → 2.11 | Só versão. |
| 2.11 → 2.12 | `icon_font` → `icon`, `iconFontName` → `icon`, `iconFontFamily` → `library`. `stroke` objeto → `stroke` (fills) + `strokeWidth` (`thickness`) + `strokeLinejoin` (`join`, default miter) + `strokeLinecap` (`cap`, default butt) + `strokeAlignment` (`inside`/ausente → `inner`, `outside` → `outer`, `center` → `center`). |
| 2.12→2.13, 2.13→2.14 | Só versão. |
| 2.14 → 2.15 | rectangle/frame com `strokeWidth` por lado não uniforme → `strokeLinejoin: "round"`. |
| 2.15 → 2.16 | `group` com layout horizontal/vertical → `frame` com `clip:false`; nos demais grupos, os campos de layout são removidos. |
| 2.16→2.17, 2.17→2.18 | Só versão. |
| 2.18 → 2.19 | Sombras: `shadowType "inner"` → `"outer"` (!) e `spread` removido. |
| 2.19 → 2.20 | Image fill: `mode "fill"` → `cover`, `"fit"` → `contain`, ausente → `stretch`. |
| 2.20 → 2.21 | Só versão. |
| 2.21 → 2.22 | `prompt` → `note` (cor `#AEDEFF`, remove `model`); `context` → `note` (cor `#DBDEE1`). As notes perdem as props tipográficas. |

Observações:
- Algumas migrações percorrem só `children` e não `descendants` (2.2→2.3, 2.3→2.4, 2.4→2.5, 2.14→2.15). Isso é
  uma inconsistência do original; ao reimplementar, considere percorrer ambos (ou reproduzir fielmente).
- A 2.18→2.19 converter inner em outer parece estranho. Pode ser que a "inner shadow" antiga fosse um bug ou não
  fosse renderizada. Hoje `inner` volta a ser suportado (`Cn.InnerShadow`). **Incerto.**
- A mesma cadeia é reutilizada para **saídas de script**: o script declara `@schema X` e a saída é migrada de X
  para 2.22.
- O editor também normaliza entradas "soltas" vindas da IA (aliases como `flex-start` → `start`, `transparent` →
  `#00000000`, `borderRadius` → `cornerRadius`, padding `[a,b,c]` → `[a,b,c,b]`, padding objeto → array). Isso
  não faz parte do formato de arquivo, mas é útil para importar dados de agentes.

---

## 12. Scripts e shaders

### 12.1 Nó `script`

```json
{ "type": "script", "id": "k3Lm9", "x": 0, "y": 0, "width": 300, "height": 300, "clip": true,
  "scriptUri": "./clock.js", "inputs": { "seconds": 36.4, "color": "$accent" } }
```

- `scriptUri`: caminho do `.js` relativo ao `.pen`. Ao inserir um script de `goodies/scripts/*.js`, o arquivo é
  **copiado** para a pasta do documento via `import-files`; se já existir um arquivo com o mesmo nome e conteúdo
  diferente, ganha sufixo `-1`, `-2`... (em documentos *managed* vai para a subpasta `scripts/`). O arquivo é
  observado e recarregado ao mudar.
- `inputs`: valores por nome (`string | number | boolean | "$var"`). Os filhos gerados **não são serializados**:
  são recalculados.
- Cabeçalho obrigatório em bloco `/** ... */`:
  - `@schema <versão>` (os goodies usam 2.10; a doc cita 2.11). Sem ele dá erro "Missing @schema version tag".
  - `@input nome: tipo[(args)] [= default]`, com tipos:
    - `number(min=…, max=…)`, default 0. O valor é clampado; NaN vira `min ?? 0`.
    - `string`, default `""`, entre aspas.
    - `boolean`, `true`/`false`.
    - `color`, literal `#RRGGBB[AA]`, default `#000000`.
    - `ref`: ID de um nó (normalmente um componente) que o script instancia como `{type:"ref", ref}`.
    - `enum("a","b",…)`: o default precisa ser uma das opções.
- Runtime: **QuickJS (WASM, `quickjs-emscripten`)**, sem DOM nem I/O. O corpo do arquivo é embrulhado como
  `(pencil) => { <código> }`, por isso o `return` no topo funciona. Recebe
  `pencil = { width, height, input: {…} }`.
  - `Math.random` é substituído por **mulberry32 com seed 0 a cada execução**, então é determinístico.
  - Limites: 16 MiB de memória, 512 KiB de stack, **2 s** de tempo (interrupt), **1000 nós** na saída.
  - A saída precisa ser um array de nós do schema; nada de `reusable` nem `connection`. Os IDs são descartados.
    É migrada a partir de `@schema`.
- Exemplos (`goodies/scripts`): clock, radar, chart, candlestick, gauge (com 5 inputs `ref` para bezel, face,
  ticks e needle), flow-field, circle-text etc.

### 12.2 Shaders (fill e effect)

- Fill `{ "type":"shader", "url":"./glass.glsl", "uniforms":{…}, "opacity", "blendMode", "enabled" }`.
- Effect `{ "type":"shader", "url", "uniforms", "enabled" }` pós-processa o nó renderizado. Vários shader effects
  formam uma cadeia na ordem da lista, com o `pencil:object` de cada um sendo a saída do anterior.
- GLSL: **WebGL 1.0 (`#version 100`) fragment shader**, mais `textureSize(sampler, lod)`.
- Uniforms são descritos por anotações em comentários de bloco antes de cada `uniform`:

| Anotação | Efeito |
|---|---|
| `@resolution` | `vec2` com o tamanho do nó em px (no effect: tamanho dos bounds visuais). Auto-ligado, **não** vai em `uniforms`. |
| `@time` | Segundos decorridos (animação). Auto-ligado, não vai em `uniforms`. |
| `@mouse` | Posição do mouse no espaço de `gl_FragCoord`. |
| `@sdf` | `sampler2D` com o SDF da forma: `r` = distância assinada (positiva dentro), `gb` = gradiente. |
| `@mipmap` | Habilita mipmap da textura. |
| `@color` | `vec3`/`vec4` usa color picker. |
| `@default <v>` | Valor default (número, `#hex`, `pencil:background`...). |
| `@min`, `@max`, `@range a, b` | Faixa e slider na UI. |
| `@label <texto>` | Nome exibido. |

- Valores em `uniforms`: number (float/int), boolean, `"#RRGGBB[AA]"` (cor), array de 2 a 4 números
  (vec2/3/4), string URL de imagem (sampler2D, relativa ao `.pen`, carregada com FLIP_Y), `"pencil:object"`
  (renderização do próprio nó por baixo do shader, pré-multiplicada), `"pencil:background"` (conteúdo atrás do
  nó, espelhado fora dele; usado em vidro/refração) ou `"$var"` (number/color).
- Ao salvar, uniforms string que não começam com `#` são tratados como URI e reescritos relativos ao arquivo.
- Exemplo (de `scripts-and-shaders.md`):
  `{ "type": "shader", "url": "pixelate.glsl", "uniforms": { "u_content": "pencil:object" } }`.
- Os goodies de shader são copiados para junto do documento como os scripts, e as imagens referenciadas pelos
  uniforms também. Os exemplos em `shader-library/examples/*.pen.json` são **arrays de nós sem `id`** (formato de
  snippet/clipboard), com URLs `goodies/shader-library/assets/*.glsl` ou imagens num blob storage remoto.

---

## 13. Persistência, snapshot e compartilhamento

- **Arquivo local**: JSON pretty (2 espaços). O loader verifica um arquivo de backup (`backupPathFor`): se o
  backup for mais novo que o arquivo e tiver conteúdo diferente, ele é oferecido como recuperação; se não, é
  apagado.
- **Clipboard** ("copy as"): JSON de um nó ou de um array de nós, no mesmo schema.
- **Snapshot zip** (`draftSnapshot`, para share/upload):
  - Um único `.pen` (ou `.lib.pen`) na raiz do zip, em JSON **minificado**. Zero ou mais de um arquivo raiz é
    erro.
  - Dependências reescritas para URIs `zip:///…`: `fonts/`, `images/`, `scripts/`, `shaders/`,
    `libraries/<nome>.lib.pen`, `browser-previews/<path>.jpg`. Os nomes são deduplicados.
- **Share na nuvem** (`shared-document-provider.ts`): um manifest
  `{ version, rootPath, etag?, files: { "<path>": { id, contentType, sha256? } } }` obtido em
  `/s/<shareToken>/manifest`. Os objetos vêm de URLs assinadas. Objetos `.pen`/`.lib.pen` podem estar
  **comprimidos com zstd** (`contentType: "application/zstd"`), com descompressão limitada a **512 MiB** (proteção
  contra compression bomb). Os demais são JSON UTF-8 puro (legado). O polling usa `?etag=`, e 204 significa
  "sem mudança".
- **Documentos embarcados**: esquema de URI `pencil:` (somente leitura), ex.: `pencil:pencil-shadcn.pen`,
  `pencil:halo.lib.pen`. Templates: `pencil-<nome>.pen`. Novo documento: `untitled.pen`.
- Nome de arquivo válido: termina em `.pen`, até 255 caracteres, sem `/ \`, com as restrições de nomes do Windows.

---

## 14. Estatística dos exemplos (sanidade)

| Arquivo | Versão | Tipos (contagem) | Vars | Temas |
|---|---|---|---|---|
| `pencil-demo.pen` (1.3 MB) | 2.6 | frame 1431, text 1079, icon_font 233, rectangle 148, ellipse 31, path 27 | 0 | — |
| `pencil-heroui.pen` | 2.13 | frame 285, text 235, ref 156, icon 79, line 2, prompt 1… | 379 | semantic/primitives/typography |
| `pencil-shadcn.pen` | 2.6 | frame 118, ref 54, text 53, icon_font 24, prompt 3… | 28 | Mode × Base × Accent |
| `halo/lunaris/nitro/shadcn.lib.pen` | 2.8 | um frame de topo com os componentes, refs internos | 28–45 | Mode (+Base/Accent) |
| `pencil-welcome.pen` | 2.13 | frame, text, path | 41 | Mode |

---

## 15. Proposta de modelo Rust

Objetivos: (1) **round-trip** sem perda, inclusive de campos desconhecidos e de versões futuras; (2) migrações
sobre `serde_json::Value` **antes** da tipagem, como no original; (3) tipos explícitos para as uniões "valor ou
variável", "um ou muitos" e sizing.

Crates: `serde`, `serde_json` (features `preserve_order`; opcionalmente `arbitrary_precision` para preservar a
grafia dos números), `indexmap`, `rand`.

### 15.1 Pipeline

```rust
pub const CURRENT_VERSION: &str = "2.22";

pub fn load_pen(text: &str) -> Result<Document, LoadError> {
    let mut v: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({ "version": CURRENT_VERSION, "children": [] })
    } else {
        serde_json::from_str(text).or_else(|_| serde_json::from_str(&strip_jsonc_comments(text)))?
    };
    migrate::run(&mut v)?;                       // cadeia 1.0 → 2.22 (§11)
    if v["version"] != CURRENT_VERSION { return Err(LoadError::UnsupportedVersion); }
    let mut doc: Document = serde_json::from_value(v)?;
    repair::assign_missing_ids(&mut doc);        // ids vazios → new_id()
    repair::dedupe_ids(&mut doc);                // duplicados → new_id() + aviso
    repair::drop_placeholders(&mut doc);         // frame/ref.placeholder
    Ok(doc)
}

pub fn save_pen(doc: &Document) -> serde_json::Result<String> {
    serde_json::to_string_pretty(doc)            // 2 espaços, igual ao original
}
```

### 15.2 Primitivos

```rust
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Número compatível com JS: grava 1 em vez de 1.0 (serde_json escreveria "1.0").
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct JsNum(pub f64);
impl Serialize for JsNum {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        if v.fract() == 0.0 && v.abs() < 9.007_199_254_740_992e15 { s.serialize_i64(v as i64) } else { s.serialize_f64(v) }
    }
}
impl<'de> Deserialize<'de> for JsNum {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> { f64::deserialize(d).map(JsNum) }
}

/// String que pode ser literal ou "$variavel" (com escape "\$" para literal começando com '$').
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Str(pub String);
pub enum StrBinding<'a> { Literal(std::borrow::Cow<'a, str>), Var(&'a str) }
impl Str {
    pub fn binding(&self) -> StrBinding<'_> {
        if let Some(rest) = self.0.strip_prefix("\\$") { StrBinding::Literal(format!("${rest}").into()) }
        else if let Some(name) = self.0.strip_prefix('$') { StrBinding::Var(name) }
        else { StrBinding::Literal(self.0.as_str().into()) }
    }
}
pub type ColorOrVar = Str;       // "#RRGGBB[AA]" | "$var"

/// Valor tipado ou referência a variável (para number/bool, onde não há ambiguidade).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Bind<T> { Value(T), Var(String) }
pub type Num = Bind<JsNum>;
pub type Bool = Bind<bool>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany<T> { One(T), Many(Vec<T>) }

/// width/height: fixo, variável, fit_content(fallback?) ou fill_container(fallback?).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DimRaw", into = "DimRaw")]
pub enum Dim { Fixed(f64), Var(String), FitContent(Option<f64>), FillContainer(Option<f64>) }

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum DimRaw { N(JsNum), S(String) }

impl TryFrom<DimRaw> for Dim {
    type Error = String;
    fn try_from(r: DimRaw) -> Result<Self, String> {
        match r {
            DimRaw::N(n) => Ok(Dim::Fixed(n.0)),
            DimRaw::S(s) if s.starts_with('$') => Ok(Dim::Var(s[1..].into())),
            DimRaw::S(s) => {
                let (kind, fb) = match s.find('(') {
                    Some(i) if s.ends_with(')') => (&s[..i], Some(s[i + 1..s.len() - 1].parse::<f64>().map_err(|e| e.to_string())?)),
                    _ => (s.as_str(), None),
                };
                match kind {
                    "fit_content" => Ok(Dim::FitContent(fb)),
                    "fill_container" => Ok(Dim::FillContainer(fb)),
                    _ => Err(format!("sizing inválido: {s}")),
                }
            }
        }
    }
}
impl From<Dim> for DimRaw {
    fn from(d: Dim) -> Self {
        let f = |k: &str, fb: Option<f64>| match fb { Some(v) => format!("{k}({v})"), None => k.to_string() };
        match d {
            Dim::Fixed(v) => DimRaw::N(JsNum(v)),
            Dim::Var(n) => DimRaw::S(format!("${n}")),
            Dim::FitContent(fb) => DimRaw::S(f("fit_content", fb)),
            Dim::FillContainer(fb) => DimRaw::S(f("fill_container", fb)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Padding { All(Num), VH([Num; 2]), Sides([Num; 4]) }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CornerRadius { All(Num), Corners([Num; 4]) } // tl, tr, br, bl

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StrokeWidth {
    Uniform(Num),
    Sides {
        #[serde(skip_serializing_if = "Option::is_none")] top: Option<Num>,
        #[serde(skip_serializing_if = "Option::is_none")] right: Option<Num>,
        #[serde(skip_serializing_if = "Option::is_none")] bottom: Option<Num>,
        #[serde(skip_serializing_if = "Option::is_none")] left: Option<Num>,
    },
}

pub type Theme = IndexMap<String, String>;
```

### 15.3 Documento, variáveis, fontes

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub version: String,
    pub children: Vec<Node>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub themes: Option<IndexMap<String, Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<IndexMap<String, VariableDef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fonts: Option<Vec<FontDef>>,
    /// Campos desconhecidos (versões futuras) preservados.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum VariableDef {
    Boolean { value: VarValue<Bool> },
    Color   { value: VarValue<ColorOrVar> },
    Number  { value: VarValue<Num> },
    String  { value: VarValue<Str> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VarValue<T> { Single(T), Themed(Vec<ThemedValue<T>>) }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemedValue<T> {
    pub value: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
}

impl<T> VarValue<T> {
    /// Regra do original: do último para o primeiro; casa se todos os eixos do entry baterem.
    pub fn resolve(&self, active: &Theme) -> Option<&T> {
        match self {
            VarValue::Single(v) => Some(v),
            VarValue::Themed(list) => list.iter().rev().find(|e| match &e.theme {
                None => true,
                Some(t) => t.iter().all(|(k, v)| active.get(k) == Some(v)),
            }).map(|e| &e.value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontDef {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub style: Option<FontStyleKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub weight: Option<OneOrRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub axes: Option<Vec<FontAxis>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum FontStyleKind { Normal, Italic }
#[derive(Debug, Clone, Serialize, Deserialize)] #[serde(untagged)]
pub enum OneOrRange { One(JsNum), Range([JsNum; 2]) }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontAxis { pub tag: Option<String>, pub start: Option<JsNum>, pub end: Option<JsNum> }
```

### 15.4 Pintura

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Fill { Solid(ColorOrVar), Typed(FillObj) }
pub type Fills = OneOrMany<Fill>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FillObj {
    Color { #[serde(flatten)] base: PaintBase, color: ColorOrVar },
    #[serde(rename_all = "camelCase")]
    Gradient {
        #[serde(flatten)] base: PaintBase,
        #[serde(default, skip_serializing_if = "Option::is_none")] gradient_type: Option<GradientType>,
        #[serde(default, skip_serializing_if = "Option::is_none")] opacity: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")] center: Option<Point>,
        #[serde(default, skip_serializing_if = "Option::is_none")] size: Option<GradientSize>,
        #[serde(default, skip_serializing_if = "Option::is_none")] rotation: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")] colors: Option<Vec<ColorStop>>,
    },
    Image {
        #[serde(flatten)] base: PaintBase,
        #[serde(default, skip_serializing_if = "Option::is_none")] opacity: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")] url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")] mode: Option<ImageMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")] transform: Option<[JsNum; 6]>,
    },
    Shader {
        #[serde(flatten)] base: PaintBase,
        #[serde(default, skip_serializing_if = "Option::is_none")] opacity: Option<Num>,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")] uniforms: Option<IndexMap<String, Value>>,
    },
    MeshGradient {
        #[serde(flatten)] base: PaintBase,
        #[serde(default, skip_serializing_if = "Option::is_none")] opacity: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")] columns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")] rows: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")] colors: Option<Vec<ColorOrVar>>,
        #[serde(default, skip_serializing_if = "Option::is_none")] points: Option<Vec<MeshPoint>>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintBase {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub enabled: Option<Bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub blend_mode: Option<BlendMode>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum GradientType { Linear, Radial, Angular }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum ImageMode { Cover, Contain, Stretch }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point { #[serde(default)] pub x: Option<JsNum>, #[serde(default)] pub y: Option<JsNum> }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradientSize {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub width: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub height: Option<Num>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorStop { pub color: ColorOrVar, pub position: Num }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MeshPoint {
    Auto([JsNum; 2]),
    #[serde(rename_all = "camelCase")]
    Handles {
        position: [JsNum; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")] left_handle: Option<[JsNum; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")] right_handle: Option<[JsNum; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")] top_handle: Option<[JsNum; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")] bottom_handle: Option<[JsNum; 2]>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    Normal, Darken, Multiply, LinearBurn, ColorBurn, Light, Screen, LinearDodge, ColorDodge,
    Overlay, SoftLight, HardLight, Difference, Exclusion, Hue, Saturation, Color, Luminosity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    Blur { #[serde(default)] enabled: Option<Bool>, #[serde(default)] radius: Option<Num> },
    BackgroundBlur { #[serde(default)] enabled: Option<Bool>, #[serde(default)] radius: Option<Num> },
    #[serde(rename_all = "camelCase")]
    Shadow {
        #[serde(default, skip_serializing_if = "Option::is_none")] enabled: Option<Bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")] shadow_type: Option<ShadowType>,
        #[serde(default, skip_serializing_if = "Option::is_none")] offset: Option<Offset>,
        #[serde(default, skip_serializing_if = "Option::is_none")] blur: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")] color: Option<ColorOrVar>,
        #[serde(default, skip_serializing_if = "Option::is_none")] blend_mode: Option<BlendMode>,
    },
    Shader {
        #[serde(default, skip_serializing_if = "Option::is_none")] enabled: Option<Bool>,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")] uniforms: Option<IndexMap<String, Value>>,
    },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum ShadowType { Inner, Outer }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offset { pub x: Num, pub y: Num }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stroke: Option<Fills>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stroke_width: Option<StrokeWidth>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stroke_linecap: Option<LineCap>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stroke_linejoin: Option<LineJoin>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stroke_alignment: Option<StrokeAlign>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum LineCap { Butt, Round, Square }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum LineJoin { Miter, Bevel, Round }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum StrokeAlign { Inner, Center, Outer }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graphics {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub fill: Option<Fills>,
    #[serde(flatten)] pub stroke: Stroke,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub effect: Option<OneOrMany<Effect>>,
}
```

### 15.5 Nós

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    #[serde(default)] pub id: String,               // vazio → gerado no load
    #[serde(default, skip_serializing_if = "Option::is_none")] pub layout_position: Option<LayoutPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub x: Option<JsNum>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub y: Option<JsNum>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub theme: Option<Theme>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub reusable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub enabled: Option<Bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub opacity: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub rotation: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub flip_x: Option<Bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub flip_y: Option<Bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub metadata: Option<IndexMap<String, Value>>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum LayoutPosition { Auto, Absolute }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Size {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub width: Option<Dim>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub height: Option<Dim>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub layout: Option<LayoutMode>, // None ⇒ horizontal
    #[serde(default, skip_serializing_if = "Option::is_none")] pub gap: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub layout_include_stroke: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub padding: Option<Padding>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub justify_content: Option<Justify>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub align_items: Option<Align>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum LayoutMode { None, Horizontal, Vertical }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Justify { Start, Center, End, SpaceBetween, SpaceAround }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum Align { Start, Center, End }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub font_family: Option<Str>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub font_size: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub font_weight: Option<Str>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub letter_spacing: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub font_style: Option<Str>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub underline: Option<Bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub line_height: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub text_align: Option<TextAlign>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub text_align_vertical: Option<TextAlignV>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub strikethrough: Option<Bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub href: Option<String>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum TextAlign { Left, Center, Right, Justify }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum TextAlignV { Top, Middle, Bottom }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TextContent { Plain(Str), Spans(Vec<TextSpan>) }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSpan { #[serde(default)] pub content: Option<String>, #[serde(flatten)] pub style: TextStyle }

/// Discriminado por "type". Cada variante achata Entity/Size/Graphics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Frame(Frame), Group(Group), Rectangle(Shape), Ellipse(Ellipse), Line(Shape),
    Polygon(Polygon), Path(PathNode), Text(Text), Note(Note), Icon(Icon),
    Script(Script), Browser(Browser), Connection(Connection), Ref(RefNode),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    #[serde(flatten)] pub entity: Entity,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub slot: Option<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub clip: Option<Bool>,
    #[serde(flatten)] pub size: Size,
    #[serde(flatten)] pub graphics: Graphics,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub placeholder: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub corner_radius: Option<CornerRadius>,
    #[serde(flatten)] pub layout: Layout,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub children: Vec<Node>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Slot { Off(bool) /* sempre false */, Recommended(Vec<String>) }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    #[serde(flatten)] pub entity: Entity,
    #[serde(flatten)] pub size: Size, // só fit_content/fill_container
    #[serde(default, skip_serializing_if = "Option::is_none")] pub effect: Option<OneOrMany<Effect>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub children: Vec<Node>,
}

/// rectangle e line
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    #[serde(flatten)] pub entity: Entity,
    #[serde(flatten)] pub size: Size,
    #[serde(flatten)] pub graphics: Graphics,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub corner_radius: Option<CornerRadius>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ellipse {
    #[serde(flatten)] pub entity: Entity,
    #[serde(flatten)] pub size: Size,
    #[serde(flatten)] pub graphics: Graphics,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub inner_radius: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub start_angle: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub sweep_angle: Option<Num>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Polygon {
    #[serde(flatten)] pub entity: Entity, #[serde(flatten)] pub size: Size, #[serde(flatten)] pub graphics: Graphics,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub polygon_count: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub corner_radius: Option<Num>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathNode {
    #[serde(flatten)] pub entity: Entity, #[serde(flatten)] pub size: Size, #[serde(flatten)] pub graphics: Graphics,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub fill_rule: Option<FillRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub geometry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub view_box: Option<[JsNum; 4]>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum FillRule { Nonzero, Evenodd }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Text {
    #[serde(flatten)] pub entity: Entity,
    #[serde(flatten)] pub graphics: Graphics,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub text_growth: Option<TextGrowth>,
    #[serde(flatten)] pub size: Size,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub content: Option<TextContent>,
    #[serde(flatten)] pub style: TextStyle,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "kebab-case")]
pub enum TextGrowth { Auto, FixedWidth, FixedWidthHeight }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    #[serde(flatten)] pub entity: Entity, #[serde(flatten)] pub size: Size,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub content: Option<TextContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub color: Option<ColorOrVar>, // default #FEE484
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Icon {
    #[serde(flatten)] pub entity: Entity, #[serde(flatten)] pub size: Size,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub icon: Option<Str>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub library: Option<Str>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub weight: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub fill: Option<Fills>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub effect: Option<OneOrMany<Effect>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Script {
    #[serde(flatten)] pub entity: Entity, #[serde(flatten)] pub size: Size,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub clip: Option<Bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub script_uri: Option<String>,
    /// string | number | boolean | "$var"
    #[serde(default, skip_serializing_if = "Option::is_none")] pub inputs: Option<IndexMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Browser {
    #[serde(flatten)] pub entity: Entity, #[serde(flatten)] pub size: Size,
    #[serde(flatten)] pub stroke: Stroke,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub effect: Option<OneOrMany<Effect>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub zoom: Option<JsNum>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub scroll_x: Option<JsNum>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub scroll_y: Option<JsNum>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub corner_radius: Option<CornerRadius>, // default 16
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    #[serde(flatten)] pub entity: Entity, #[serde(flatten)] pub stroke: Stroke,
    pub source: Endpoint, pub target: Endpoint,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint { pub path: String, pub anchor: Anchor }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum Anchor { Center, Top, Left, Bottom, Right }

/// Instância. Overrides da raiz ficam em `overrides` (propriedades livres).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefNode {
    #[serde(flatten)] pub entity: Entity,
    #[serde(rename = "ref")] pub target: String,             // "id" ou "alias:id"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descendants: Option<IndexMap<String, DescendantOverride>>, // chave = IdPath
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Node>>,                          // slot preenchido na raiz
    /// Declarado por último: recebe só as chaves não consumidas acima.
    #[serde(flatten)] pub overrides: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DescendantOverride {
    /// Com "type": substitui a subárvore.
    Replace(Box<Node>),
    /// Sem "type": patch de propriedades (pode incluir "children" para slots).
    Patch(IndexMap<String, Value>),
}
```

### 15.6 IDs e migração

```rust
use rand::{distributions::Alphanumeric, Rng};

/// 5 alfanuméricos; prefixa uma letra se começar com dígito (compatível com o original).
pub fn new_id() -> String {
    let mut rng = rand::thread_rng();
    let id: String = (&mut rng).sample_iter(&Alphanumeric).take(5).map(char::from).collect();
    if id.as_bytes()[0].is_ascii_digit() {
        let c = loop { let ch = rng.sample(Alphanumeric) as char; if ch.is_ascii_alphabetic() { break ch } };
        format!("{c}{id}")
    } else { id }
}

pub mod migrate {
    use serde_json::Value;
    type Step = fn(&mut Value);
    /// (versão de origem, passo). Cada passo grava a versão seguinte em `version`.
    pub const STEPS: &[(&str, Step)] = &[
        ("2.0", v2_0), ("2.1", v2_1), ("2.2", v2_2), ("2.3", v2_3), ("2.4", v2_4), ("2.5", v2_5),
        ("2.6", v2_6), ("2.7", bump), ("2.8", bump), ("2.9", v2_9), ("2.10", bump), ("2.11", v2_11),
        ("2.12", bump), ("2.13", bump), ("2.14", v2_14), ("2.15", v2_15), ("2.16", bump), ("2.17", bump),
        ("2.18", v2_18), ("2.19", v2_19), ("2.20", bump), ("2.21", v2_21),
    ];
    pub fn run(doc: &mut Value) -> Result<(), String> {
        if doc["version"] == "1.0" { *doc = from_legacy_1_0(doc)?; }
        for (from, step) in STEPS {
            if doc["version"] == *from { step(doc); }
        }
        Ok(())
    }
    // Helper: visita recursivamente children e descendants (dentro de nós com "type").
    // fn walk_nodes(v: &mut Value, f: &mut dyn FnMut(&mut serde_json::Map<String, Value>)) { ... }
    // Implementações seguem a tabela do §11.
}
```

### 15.7 Notas de implementação

1. **Fidelidade de números**: o JS grava `1`, não `1.0`, e preserva floats f32 promovidos
   (`1.399999976158142`). Com `JsNum` (inteiros como i64) e o `ryu` do serde_json (menor representação que faz
   round-trip, como o JS), a saída fica igual na prática. A exceção são expoentes (`1e21` contra `1e+21`), que são
   irrelevantes aqui.
2. **Ordem de chaves**: os structs acima seguem a ordem do serializador original. Com `preserve_order`, as chaves
   extras também ficam estáveis. O tag `type` sai primeiro em todas as variantes; o original grava `id` antes de
   `type` em `ref`, uma diferença só cosmética.
3. **Tolerância a erros**: `#[serde(untagged)]` em `DescendantOverride` cai silenciosamente em `Patch` se a
   substituição for inválida. Para tipos de nó desconhecidos (versões futuras), use um wrapper
   `enum Child { Known(Node), Unknown(Value) }` com Deserialize manual: tente `Node`, guarde o erro como
   diagnóstico e preserve o `Value`.
4. **Resolução de instâncias**: a forma mais simples é clonar o `Value` do componente, aplicar o merge dos
   overrides da raiz, aplicar os `descendants` (patch = merge raso; replace = troca; `children` = troca de filhos
   de slot), resolver recursivamente refs aninhados (atenção a ciclos) e só então tipar. IDs internos continuam os
   do componente e são endereçados por IdPath.
5. **Namespacing de imports**: ao carregar uma biblioteca com alias `a`, prefixe `a:` em `id`, `ref`, nos
   segmentos das chaves de `descendants` e em `slot`. Ao salvar o documento-raiz, mantenha o prefixo nas
   referências externas e remova só o do próprio documento.
6. **Validação estrita x leniente**: o original valida, mas abre mesmo com erros. Recomenda-se o mesmo:
   diagnóstico não fatal e campos desconhecidos preservados nos `extra`/`overrides`.

---

## 16. Pontos incertos / a confirmar

- Rich text (`content` como array de spans) está no schema privado, mas não achei suporte claro no
  deserializador do editor.
- O motivo da migração 2.18→2.19 converter sombras `inner` em `outer`.
- O default exato de `icon.weight` (o serializador omite 200).
- A ordem de pintura de múltiplos fills (o último por cima é a hipótese) e a distribuição de espaço entre vários
  filhos `fill_container`.
- O formato do `fileToken` (validado por um schema zod não inspecionado; é gerado como UUID v4).
- O default de `strokeAlignment`: o serializador atual omite `center`, mas a migração 2.11→2.12 mapeia
  `align` ausente para `inner`. Para arquivos ≥ 2.12, assuma `center`.
