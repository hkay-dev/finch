[
  (code_span)
] @text.code.span

((emphasis_delimiter) @markup.mark
  (#set! highlight.allow-overlap))

((code_span_delimiter) @markup.code.mark
  (#set! highlight.allow-overlap))

((emphasis) @markup.emphasis
  (#set! highlight.allow-overlap))

((strong_emphasis) @markup.strong
  (#set! highlight.allow-overlap))

(link_destination) @markup.link.uri

(uri_autolink) @link_uri

[
  (link_label)
  (link_text)
  (image_description)
] @link_text

; The marks of a link or image, after `link_text` so they win over it where they meet. They sit
; beside or inside other captures (a link's text, a heading), so each pattern allows overlap.
((inline_link "[" @markup.bracket)
  (#set! highlight.allow-overlap))
((inline_link "]" @markup.bracket)
  (#set! highlight.allow-overlap))
((inline_link "(" @markup.paren)
  (#set! highlight.allow-overlap))
((inline_link ")" @markup.paren)
  (#set! highlight.allow-overlap))
((image "!" @markup.paren)
  (#set! highlight.allow-overlap))
((image "[" @markup.bracket)
  (#set! highlight.allow-overlap))
((image "]" @markup.bracket)
  (#set! highlight.allow-overlap))
((image "(" @markup.paren)
  (#set! highlight.allow-overlap))
((image ")" @markup.paren)
  (#set! highlight.allow-overlap))
((full_reference_link "[" @markup.bracket)
  (#set! highlight.allow-overlap))
((full_reference_link "]" @markup.bracket)
  (#set! highlight.allow-overlap))
((collapsed_reference_link "[" @markup.bracket)
  (#set! highlight.allow-overlap))
((collapsed_reference_link "]" @markup.bracket)
  (#set! highlight.allow-overlap))
((shortcut_link "[" @markup.bracket)
  (#set! highlight.allow-overlap))
((shortcut_link "]" @markup.bracket)
  (#set! highlight.allow-overlap))
