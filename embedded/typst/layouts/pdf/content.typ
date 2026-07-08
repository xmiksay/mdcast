// Default page layout. Body is a raw markdown string — v1 typesets it as
// pre-formatted text; richer rendering is a Phase-4 concern once a markdown→
// typst pre-processor is in place.
//
// Reads `doc-meta` for an optional running header (title + any `extra` key,
// e.g. `classification` — see README: "Typst layout context"). No title and
// no matching extra key means an empty header, same as before this existed.
#import "/context.typ": doc-meta

#let layout(body) = [
  #set page(
    margin: 2cm,
    header: if doc-meta.title != "" or "classification" in doc-meta [
      #set text(size: 9pt, fill: luma(120))
      #grid(
        columns: (1fr, 1fr),
        align(left)[#doc-meta.title],
        align(right)[#doc-meta.at("classification", default: "")],
      )
    ],
  )
  #set text(font: "New Computer Modern", size: 11pt)
  #eval(body, mode: "markup")
]
