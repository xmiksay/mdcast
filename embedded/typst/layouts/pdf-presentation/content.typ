// Default slide layout for touying-style presentations. v1 lays out body as
// raw markdown; richer rendering arrives with the md→typst pre-processor.
//
// Reads `doc-meta` for an optional running header (title + any `extra` key,
// e.g. `classification` — see README: "Typst layout context"). No title and
// no matching extra key means an empty header, same as before this existed.
#import "/context.typ": doc-meta

#let layout(body) = [
  #set page(
    paper: "presentation-16-9",
    margin: 1.5cm,
    header: if doc-meta.title != "" or "classification" in doc-meta [
      #set text(size: 12pt, fill: luma(120))
      #grid(
        columns: (1fr, 1fr),
        align(left)[#doc-meta.title],
        align(right)[#doc-meta.at("classification", default: "")],
      )
    ],
  )
  #set text(font: "New Computer Modern", size: 22pt)
  #eval(body, mode: "markup")
]
