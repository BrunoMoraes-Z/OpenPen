"""Regenerates assets/fonts/* from the official upstream releases.

Usage: python -I prepare.py <download-dir> <assets/fonts dir>

<download-dir> must contain (see ../LICENSES.md for exact versions/URLs):
  lucide/package/        npm pack lucide-static@0.563.0 (extracted)
  phosphor/package/      npm pack phosphor-icons@1.4.2 (extracted)
  material/              MaterialSymbols{Outlined,Rounded,Sharp}.{ttf,codepoints} + LICENSE
                         (google/material-design-icons, variablefont/)
  inter/                 Inter-4.1.zip (extracted)
Requires fontTools (pip install fonttools) for the Material Symbols instancing.

Outputs: unmodified Inter/Lucide/Phosphor TTFs, Material Symbols instanced to
FILL=0 GRAD=0 opsz=24 (wght axis kept; GSUB ligatures and STAT dropped), and
one `<name> <hex codepoint>` map per icon set, sorted by name.
"""
import json, os, shutil, sys

src, dst = sys.argv[1], sys.argv[2]

def write_map(path, pairs):
    pairs = sorted(pairs)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        for name, cp in pairs:
            f.write(f"{name} {cp:x}\n")

def copy(a, b):
    os.makedirs(os.path.dirname(b), exist_ok=True)
    shutil.copyfile(a, b)

# Inter (OFL-1.1): static TTFs so every backend matches the family "Inter".
for w in ["Light", "Regular", "Medium", "SemiBold", "Bold", "ExtraBold", "Black", "Italic"]:
    copy(f"{src}/inter/extras/ttf/Inter-{w}.ttf", f"{dst}/inter/Inter-{w}.ttf")
copy(f"{src}/inter/LICENSE.txt", f"{dst}/inter/LICENSE.txt")

# Lucide (ISC)
copy(f"{src}/lucide/package/font/lucide.ttf", f"{dst}/lucide/lucide.ttf")
copy(f"{src}/lucide/package/LICENSE", f"{dst}/lucide/LICENSE")
info = json.load(open(f"{src}/lucide/package/font/info.json", encoding="utf-8"))
write_map(f"{dst}/lucide/lucide.codepoints",
          [(k, int(v["encodedCode"].lstrip("\\"), 16)) for k, v in info.items()])

# Phosphor (MIT)
copy(f"{src}/phosphor/package/src/fonts/Phosphor.ttf", f"{dst}/phosphor/Phosphor.ttf")
copy(f"{src}/phosphor/package/LICENSE", f"{dst}/phosphor/LICENSE")
m = json.load(open(f"{src}/phosphor/package/src/unicodesMap.json", encoding="utf-8"))
write_map(f"{dst}/phosphor/phosphor.codepoints",
          [(k.removeprefix("ph-"), v) for k, v in m.items()])

# Material Symbols (Apache-2.0)
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
for s in ["Outlined", "Rounded", "Sharp"]:
    f = TTFont(f"{src}/material/MaterialSymbols{s}.ttf")
    f = instancer.instantiateVariableFont(f, {"FILL": 0, "GRAD": 0, "opsz": 24}, updateFontNames=False)
    for t in ("GSUB", "STAT"):
        if t in f:
            del f[t]
    os.makedirs(f"{dst}/material-symbols", exist_ok=True)
    f.save(f"{dst}/material-symbols/MaterialSymbols{s}.ttf")
copy(f"{src}/material/LICENSE", f"{dst}/material-symbols/LICENSE")
pairs = []
for line in open(f"{src}/material/MaterialSymbolsOutlined.codepoints", encoding="utf-8"):
    p = line.split()
    if len(p) == 2:
        pairs.append((p[0], int(p[1], 16)))
write_map(f"{dst}/material-symbols/material-symbols.codepoints", pairs)
