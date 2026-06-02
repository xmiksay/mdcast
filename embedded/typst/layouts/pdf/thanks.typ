// Closing page: large centred message.

#let layout(body) = [
  #set page(margin: (top: 8cm, bottom: 4cm, x: 3cm))
  #set text(font: "New Computer Modern")
  #align(center)[
    #text(size: 24pt, weight: "bold")[Thank you]
    #v(1cm)
    #text(size: 14pt)[#eval(body, mode: "markup")]
  ]
]
