// Callout / quote page: large italic body on a soft background.

#let layout(body) = [
  #set page(margin: 3cm, fill: rgb("#fafafa"))
  #set text(font: "New Computer Modern", size: 16pt, style: "italic")
  #align(center + horizon)[
    #eval(body, mode: "markup")
  ]
]
