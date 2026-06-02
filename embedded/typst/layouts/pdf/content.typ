// Default page layout. Body is a raw markdown string — v1 typesets it as
// pre-formatted text; richer rendering is a Phase-4 concern once a markdown→
// typst pre-processor is in place.

#let layout(body) = [
  #set page(margin: 2cm)
  #set text(font: "New Computer Modern", size: 11pt)
  #eval(body, mode: "markup")
]
