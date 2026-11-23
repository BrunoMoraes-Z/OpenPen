# Fontes embutidas

Todas as fontes aqui são open source e vêm dos releases oficiais (nada foi copiado de `legacy/`).
São embutidas no binário pelo crate `crates/pen-fonts` (`include_bytes!`). Para regenerar:
`python -I tools/prepare.py <pasta-com-downloads> .` (instruções no cabeçalho do script).

| Fonte | Versão | Licença | Arquivos | Tamanho | Origem |
|---|---|---|---|---|---|
| Inter | 4.1 | SIL OFL 1.1 (`inter/LICENSE.txt`) | `inter/Inter-{Light,Regular,Italic,Medium,SemiBold,Bold,ExtraBold,Black}.ttf` (estáticas, sem modificação) | 3,3 MB | https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip (`extras/ttf/`) |
| Lucide | lucide-static 0.563.0 | ISC (`lucide/LICENSE`) | `lucide/lucide.ttf` (sem modificação) + `lucide.codepoints` (gerado de `font/info.json`) | 0,8 MB | https://registry.npmjs.org/lucide-static/-/lucide-static-0.563.0.tgz |
| Material Symbols Outlined / Rounded / Sharp | google/material-design-icons @ `10ecc14790612ca8fd164e3f4ffe9c16fc50d5b7` (2026-10-23) | Apache 2.0 (`material-symbols/LICENSE`) | `material-symbols/MaterialSymbols{Outlined,Rounded,Sharp}.ttf` + `material-symbols.codepoints` | 2,7 + 3,7 + 2,2 MB | https://github.com/google/material-design-icons/tree/master/variablefont |
| Phosphor | phosphor-icons 1.4.2 | MIT (`phosphor/LICENSE`) | `phosphor/Phosphor.ttf` (sem modificação) + `phosphor.codepoints` (gerado de `src/unicodesMap.json`, sem o prefixo `ph-`) | 1,1 MB | https://registry.npmjs.org/phosphor-icons/-/phosphor-icons-1.4.2.tgz |

**Modificação (Apache 2.0, §4b):** as fontes Material Symbols são instâncias derivadas das fontes
variáveis originais `MaterialSymbols*[FILL,GRAD,opsz,wght].ttf`: eixos fixados em `FILL=0`, `GRAD=0`,
`opsz=24` (eixo `wght` 100–700 mantido) e tabelas `GSUB` (ligaduras) e `STAT` removidas, com
fontTools `varLib.instancer`. Os arquivos originais têm 8,9–15 MB cada.

`*.codepoints`: uma linha `<nome> <codepoint hex>` por ícone, ordenada por nome.

`feather` (oferecido pelo editor legado) não tem fonte oficial; os nomes são resolvidos na Lucide
(fork da Feather, com os mesmos nomes na grande maioria).
