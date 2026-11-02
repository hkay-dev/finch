---
title: Code colors
feature: code-colors
type: guide
summary: View > Code colors picks a color scheme for code and switches vivid hues, colored roles, rainbow brackets and semantic colors, and [code_colors] in the settings file does the same with per-role overrides.
covers:
  - src/settings.rs
  - src/theme.rs
  - src/tokens.rs
  - src/menu.rs
  - src/app.rs
  - src/document.rs
  - vendor/gpui-component/src/highlighter/registry.rs
  - vendor/gpui-component/src/highlighter/highlighter.rs
  - vendor/gpui-base/src/input/editor/highlighting.rs
since: 2026-11-01
---

Finch colors code with one of 25 schemes, each in a dark and a light version from its own authors.
Choose **View > Code colors** and pick one; the check mark shows the current one. Xcode comes first,
then the rest by name. The Id column is what `scheme` takes in the settings file.

| Scheme | Dark | Light | Id |
|---|---|---|---|
| **Xcode** (the default) | Default (Dark) | Default (Light) | `xcode` |
| **Ayu** | Ayu Dark | Ayu Light | `ayu` |
| **Bluloco** | Bluloco Dark | Bluloco Light | `bluloco` |
| **Catppuccin** | Mocha | Latte | `catppuccin` |
| **Dracula** | Dracula | Alucard | `dracula` |
| **Everforest** | Dark Medium | Light Medium | `everforest` |
| **Flexoki** | Flexoki Dark | Flexoki Light | `flexoki` |
| **GitHub** | Dark Default | Light Default | `github` |
| **Gruvbox** | Dark Medium | Light Medium | `gruvbox` |
| **Horizon** | Horizon Dark | Horizon Bright | `horizon` |
| **JetBrains** | Darcula | IntelliJ Light | `jetbrains` |
| **Kanagawa** | Wave | Lotus | `kanagawa` |
| **Material** | Material Darker | Material Lighter | `material` |
| **Modus** | Modus Vivendi | Modus Operandi | `modus` |
| **Monokai Pro** | Monokai Pro | Monokai Pro Light | `monokai-pro` |
| **Night Owl** | Night Owl | Light Owl | `night-owl` |
| **Nightfox** | Nightfox | Dayfox | `nightfox` |
| **One** | Atom One Dark | Atom One Light | `one` |
| **Oxocarbon** | Oxocarbon Dark | Oxocarbon Light | `oxocarbon` |
| **Rosé Pine** | Main | Dawn | `rose-pine` |
| **Solarized** | Solarized Dark | Solarized Light | `solarized` |
| **Tomorrow** | Tomorrow Night | Tomorrow | `tomorrow` |
| **Tokyo Night** | Night | Day | `tokyo-night` |
| **Vitesse** | Vitesse Dark | Vitesse Light | `vitesse` |
| **VS Code** | Dark Modern | Light Modern | `vscode` |

Each scheme's hex values come from its own published theme files. Where a theme gives a role no
color of its own (fields or parameters, often), that role takes the color the theme draws it in,
usually its plain text color. One change from a theme: Oxocarbon Light draws comments in near-black,
darker than its code, so Finch gives them that theme's own light grey.

The colors follow the Mac's light or dark mode (or the `theme` setting) like everything else. The
Markdown preview's code blocks use the same colors. The ground
behind the code stays Finch's own in every scheme; only the code's hues change.

## The switches

Under the schemes, four checked items change how much the colors say:

- **Vivid** lifts the saturation of every hue, and in the light theme darkens any hue that would
  read thin, so each stays at 4.5:1 or better against the white page (and against the dark grey
  in the dark). Greys keep their saturation but get the same contrast lift; only comments are left alone. On by default.
- **Colored roles** gives fields, parameters, constants, enum variants, `self`, built-in types,
  methods, lifetimes and labels, string escapes, Markdown list markers and Markdown quote marks a
  hue of their own. Off, they look as they did before: fields, parameters, variables and
  operators take the plain text color, constants the number's, variants and built-in types the
  type's, `self` the keyword's, methods the function's and escapes the string's. On by default.
- **Rainbow brackets** gives code's `()`, `[]` and `{}` a hue by how deep they nest, so a pair
  shares one color. Brackets in strings and comments, and in Markdown files, keep their usual color.
  On by default.
- **Semantic colors** gives each variable and parameter a hue of its own from its spelling, so every
  use of one name has one color. Keywords, types, functions and fields keep the scheme's colors.
  Off by default, since it makes a busy page.

Each item writes its line in the settings file, and a line edited by hand applies at once.

## In the settings file

```toml
[code_colors]
scheme = "xcode"          # any id from the table above
vivid = true
colored_roles = true
rainbow_brackets = true
semantic = false

[code_colors.dark]
keyword = "#FC5FA3"

[code_colors.light]
keyword = "#9B2393"
```

A missing table, or a missing key, means the defaults above. The two optional tables set single
roles in one theme, as `#RRGGBB`, and are applied last, over the scheme, vivid and colored roles.
A value that isn't a hex color shows the settings alert with the line and the key ("keyword must be a color like
#FC5FA3"), and Finch keeps the colors in use until it is fixed. The Settings window (Finch > Settings, Code colors) writes
only these two tables' keys, one per role you set, and keeps your comments, order and every other key; taking off
the last color leaves a valid file. Reset, at the foot of the list while the current theme has colors of its own,
takes them all off at once (only that theme's table; the other theme's colors stay). See [Settings](settings.md).

Any role name works. The common ones are `keyword`, `string`, `number`, `comment`, `type`,
`type.interface`, `function`, `function.special` (macros), `attribute`, `constant`, `property`
(fields), `variable.parameter`, `variable.special` (`self`), `variant`, `type.builtin`,
`function.method`, `label`, `string.escape`, `punctuation.list_marker`, `markup.quote` and
`link_uri`. A role Finch gives no color keeps the plain text color. An override sets the color and
keeps the role's other style (italic, weight).

## How the roles are found

The colors come from the names the language grammars give to pieces of code (`@property`,
`@variable.parameter`, `@type.builtin` and so on). A name with no color of its own takes the one of
the name before its last dot (`function.method.call` takes `function.method`, then `function`).
Rust's enum variants are named `@variant` in definitions and where a path ends in a CamelCase name
(`Ordering::Less`); the end of a `use` path stays a type. Markdown's block quote mark is named
`@markup.quote`.
