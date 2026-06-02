// Default slide layout for touying-style presentations. v1 lays out body as
// raw markdown; richer rendering arrives with the md→typst pre-processor.

#let layout(body) = [
  #set page(paper: "presentation-16-9", margin: 1.5cm)
  #set text(font: "New Computer Modern", size: 22pt)
  #eval(body, mode: "markup")
]
