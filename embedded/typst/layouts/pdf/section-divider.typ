// Section divider: large centred heading on an otherwise empty page.

#let layout(body) = [
  #set page(margin: 4cm, fill: rgb("#f5f5f5"))
  #set text(font: "New Computer Modern")
  #align(center + horizon)[
    #text(size: 32pt, weight: "bold")[#eval(body, mode: "markup")]
  ]
]
