# Site fonts

| File | Font | Source |
|---|---|---|
| `inter-latin-var.woff2` | Inter variable, Latin | `@fontsource-variable/inter` (`files/inter-latin-wght-normal.woff2`) |
| `pretendard-ko-subset-var.woff2` | Pretendard Variable, Korean subset | `pretendard` 1.3.9 (`dist/web/variable/woff2/PretendardVariable.woff2`) |
| `jetbrains-mono-latin-400.woff2` | JetBrains Mono Regular, Latin | `@fontsource/jetbrains-mono` (`files/jetbrains-mono-latin-400-normal.woff2`) |

Licenses: SIL Open Font License 1.1, see [OFL.txt](OFL.txt).

## Regenerating the Korean subset

The Pretendard file holds only the Hangul and CJK punctuation glyphs used in `index.html`, `404.html` and `assets/site.js`. After changing Korean copy, regenerate it, or new syllables fall back to the system Korean font:

```bash
npm i --prefix /tmp/fonts pretendard
pip install fonttools brotli
python - <<'EOF'
import re
from fontTools import subset
txt = "".join(open(p, encoding="utf8").read() for p in ["site/index.html", "site/404.html", "site/assets/site.js"])
chars = sorted({c for c in txt if re.match(r"[ᄀ-ᇿ　-〿㄰-㆏가-힯！-｠]", c)})
o = subset.Options(); o.flavor = "woff2"; o.layout_features = ["*"]; o.name_IDs = ["*"]
f = subset.load_font("/tmp/fonts/node_modules/pretendard/dist/web/variable/woff2/PretendardVariable.woff2", o)
s = subset.Subsetter(o); s.populate(text="".join(chars)); s.subset(f)
subset.save_font(f, "site/fonts/pretendard-ko-subset-var.woff2", o)
EOF
```
