// Title slide.

#let layout(body) = [
  #set page(paper: "presentation-16-9", margin: 2cm, fill: rgb("#0b1d3a"))
  #set text(font: "New Computer Modern", fill: white)
  #align(center + horizon)[
    #text(size: 48pt, weight: "bold")[#eval(body, mode: "markup")]
  ]
]
