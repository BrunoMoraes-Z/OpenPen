# pen-schema — notas de implementação

Crate `crates/pen-schema`: modelo tipado do `.pen` 2.22, migrações 1.0→2.22, validação. Referências:
`docs/legacy-analysis/01-formato-pen.md` e `legacy/extracted/schema/pen.schema.json`.

## Decisões

- **Pipeline**: bytes → `parse_json` (remove BOM; entrada vazia = documento vazio; se o JSON falhar, tenta de novo
  sem comentários `//`/`/* */`, como o legado) → `migrate` sobre `serde_json::Value`, mutando no lugar → conversão
  tipada → reparos de load. `save_document` reproduz `JSON.stringify(doc, null, 2)`: indentação de 2 espaços, sem
  newline final, números no formato do JS (`1` e não `1.0`; expoente só fora de 1e-7..1e21).
  `save_document_with(doc, &SaveOptions)` escolhe fim de linha (`LineEnding::Lf`/`CrLf`), newline final e BOM;
  o load detecta os do arquivo (`LoadReport::save_options`, via `SaveOptions::detect`) para salvar igual.
- **Um struct `Node` para todos os tipos**: `kind` (o `type`) mais a união de todas as propriedades, cada uma
  `Option`. Assim um patch de `descendants` (sem `type`) é um `Node` com `kind == None`, uma substituição é um `Node`
  tipado, e os overrides da raiz de um `ref` são campos normais. Quais campos valem para cada tipo é checado em
  `validate` (`allowed_fields`), nunca no load. Custo: `size_of::<Node>()` ≈ 2,6 KB.
- **Sem perda**: cada objeto (`pen_object!`) tem `extra` (chaves desconhecidas **e** chaves conhecidas com valor de
  forma inválida, guardadas literalmente) e `order` (ordem original das chaves, 1 byte por chave). Campos ausentes
  continuam ausentes. Objetos criados em código saem na ordem do serializador legado. Fills/efeitos de tipo
  desconhecido viram `Fill::Unknown` / `Effect::Unknown`; nó de tipo desconhecido vira `NodeKind::Other`.
- **Valores**: `Bind<T>` (`Value(T)` | `Var(nome sem $)`) para number/bool; `StrOrVar` e `ColorOrVar` guardam a
  string crua (`.var()`, `.literal()`, `.rgba()`); `Sizing` = `Fixed | Var | FitContent(Option<f64>) |
  FillContainer(Option<f64>)` com a regex do schema; `OneOrMany` lembra se era objeto ou array. `ColorOrVar` é estrito
  (`#RGB/#RRGGBB/#RRGGBBAA` ou `$var`); cor inválida vai para `extra` com diagnóstico.
- **Reparos no load** (`LoadOptions::repair`, ligado por padrão, como no legado): gera IDs ausentes, regenera IDs
  duplicados na árvore (incluindo filhos de slot de `ref`), apaga `placeholder` de frame/ref e descarta `children`
  cujo primeiro filho é um `ref` auto-referente. Tudo vai para `LoadReport.warnings` (`DiagCode::Repaired`).
  `fileToken` não é atribuído (isso é coisa do app no save).
- **IDs**: 5 alfanuméricos; se o primeiro for dígito, prefixa uma letra (6 chars), igual ao legado. PRNG próprio
  (SplitMix64), sem dependência.

## Ambiguidades resolvidas

- **Escopo dos passos de migração**: reproduzido fielmente. 2.2→2.3, 2.3→2.4, 2.4→2.5 e 2.14→2.15 só percorrem
  `children`; os demais também percorrem os valores de `descendants`. 2.6→2.7 só inverte `sweepAngle` em nós com
  `type` ellipse/ref (patches sem `type` não são tocados, como no legado).
- **Sombra interna (2.18→2.19)**: o legado converte `inner` em `outer` e remove `spread`. Reproduzido como está
  (o documento abre igual ao app original); a partir da 2.19, `inner` volta a ser válido.
- **`strokeAlignment`**: ausente ⇒ `center` para arquivos ≥ 2.12. A migração 2.11→2.12 grava explicitamente `inner`
  quando o `align` antigo era ausente/`inside`, então não há ambiguidade depois da migração.
- **Rich text**: `content` em array é tipado como `TextContent::Spans(Vec<TextSpan>)` (cada span = `content` +
  TextStyle) e preservado; nenhum arquivo do corpus usa.
- **`descendants`**: um único nível com caminhos `a/b`; chave com `type` = substituição, sem `type` = patch (pode ter
  `children` para preencher slot). A validação resolve caminhos atravessando instâncias aninhadas, substituições e
  filhos adicionados por patches; não resolvido = aviso (`DanglingOverridePath`), como o "had problems" do legado.
- **1.0→2.0**: nós de tipo desconhecido são descartados (aviso). Desvio consciente: o legado deixava `null` dentro
  de `children` nesses casos; aqui o item é filtrado.
- **2.3→2.4**: a 2.3 deixa `url` no retângulo criado a partir de `image`; mantido (a validação acusa
  `PropertyNotAllowed`).
- **Textos começando com `$`**: arquivos antigos têm `content: "$67,890"` literal. O modelo trata como variável (o
  schema diz isso) e a validação só avisa `UndefinedVariable`.

## Resultados (corpus local)

- 11 arquivos `legacy/` (2.6–2.13) e 4 `user/` antigos (2.14/2.17): carregam sem erro, `migrar→salvar→carregar→salvar`
  é ponto fixo, 0 erros de validação, sem IDs duplicados.
- 5 arquivos `user/` em 2.22: `save_document_with(load(x), report.save_options)` é **byte a byte idêntico** ao
  original, inclusive o `t-process-recorder.pen` em CRLF (o teste de corpus falha se só o fim de linha diferir).
- Bench (`cargo bench -p pen-schema --bench load`, release, máquina com carga): `t-process-recorder.pen` (1,4 MB)
  parse ≈ 9 ms, load completo ≈ 19 ms, save ≈ 4 ms; `pencil-demo.pen` (2.6→2.22) ≈ 14 ms.

## Pendências

- `Node` grande (2,6 KB); se a memória pesar, agrupar propriedades raras em `Option<Box<_>>`.
- Ciclos entre variáveis não são detectados pela validação.
- Comparação de números no `fallback` do sizing usa o formato do JS: `fit_content(44.50)` seria regravado como
  `fit_content(44.5)` (não acontece em arquivos gravados pelo legado).
