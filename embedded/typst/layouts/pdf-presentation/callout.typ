// Callout / pull-quote slide.

#let layout(body) = [
  #set page(paper: "presentation-16-9", margin: 3cm, fill: rgb("#f7f4ed"))
  #set text(font: "New Computer Modern", size: 32pt, style: "italic")
  #align(center + horizon)[
    #eval(body, mode: "markup")
  ]
]
