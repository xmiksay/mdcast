// Hero / cover page: large centred title, wide margins.

#let layout(body) = [
  #set page(margin: (top: 6cm, bottom: 4cm, x: 3cm))
  #set text(font: "New Computer Modern", size: 14pt)
  #align(center)[
    #text(size: 28pt, weight: "bold")[#eval(body, mode: "markup")]
  ]
]
