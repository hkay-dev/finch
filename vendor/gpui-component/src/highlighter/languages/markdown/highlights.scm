;From nvim-treesitter/nvim-treesitter
(atx_heading (inline) @markup.heading)
(setext_heading (paragraph) @markup.heading)

[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
] @markup.heading.marker

[
  (link_title)
  (indented_code_block)
  (fenced_code_block)
] @text.literal

[
  (fenced_code_block_delimiter)
] @punctuation.delimiter

(code_fence_content) @none

[
  (link_destination)
] @text.uri

[
  (link_label)
] @text.reference

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
  (thematic_break)
] @punctuation.list_marker

((block_continuation) @markup.quote
  (#match? @markup.quote ">"))

(block_continuation) @punctuation.special

(block_quote_marker) @markup.quote

[
  (backslash_escape)
] @string.escape

(pipe_table_header "|" @punctuation.delimiter)
(pipe_table_row "|" @punctuation.delimiter)
(pipe_table_delimiter_row "|" @punctuation.delimiter)
(pipe_table_delimiter_cell "-" @punctuation.delimiter)
(pipe_table_delimiter_cell (pipe_table_align_left) @punctuation.delimiter)
(pipe_table_delimiter_cell (pipe_table_align_right) @punctuation.delimiter)
