// Section divider slide.

#let layout(body) = [
  #set page(paper: "presentation-16-9", margin: 2cm, fill: rgb("#1f2933"))
  #set text(font: "New Computer Modern", fill: white)
  #align(center + horizon)[
    #text(size: 44pt, weight: "bold")[#eval(body, mode: "markup")]
  ]
]
