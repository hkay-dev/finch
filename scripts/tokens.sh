#!/bin/sh
# Prints every token of src/tokens.rs as a Markdown table: name, type, value, source.
# The source is the `///` line right above the const; the value is what follows `=`.
# Rewrites the table in docs/design/system.md (between the `<!-- tokens -->` markers).
cd "$(dirname "$0")/.." || exit 1
python3 - src/tokens.rs docs/design/system.md <<'PY'
import re, sys

doc, rows = [], []
for line in open(sys.argv[1]):
    line = line.rstrip("\n")
    if line.startswith("///"):
        doc.append(line[3:].strip())
        continue
    m = re.match(r"pub const (\w+): ([^=]+?) = (.+);$", line)
    if m and doc:
        name, ty, value = m.groups()
        rows.append((name, ty.strip(), value.strip(), " ".join(doc)))
    doc = []

def cell(text):
    return text.replace("|", "\\|")

table = ["| Token | Type | Value | Source |", "|---|---|---|---|"]
for name, ty, value, source in rows:
    table.append(f"| `{name}` | `{cell(ty)}` | `{cell(value)}` | {cell(source)} |")

text = open(sys.argv[2]).read()
head, rest = text.split("<!-- tokens -->\n", 1)
_, tail = rest.split("<!-- /tokens -->\n", 1)
open(sys.argv[2], "w").write(head + "<!-- tokens -->\n" + "\n".join(table) + "\n<!-- /tokens -->\n" + tail)
PY
