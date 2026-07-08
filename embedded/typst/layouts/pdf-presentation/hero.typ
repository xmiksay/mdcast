// Title slide. Reads `doc-meta` / `brand` from the driver-injected context
// (README: "Typst layout context") — no metadata/brand means the accent
// falls back to white and the font to the prior hardcoded default.
#import "/context.typ": doc-meta, brand-color, brand-font

#let layout(body) = [
  #set page(paper: "presentation-16-9", margin: 2cm, fill: rgb("#0b1d3a"))
  #set text(font: brand-font("sans", default: "New Computer Modern"), fill: white)
  #align(center + horizon)[
    #text(size: 48pt, weight: "bold", fill: brand-color("accent", default: white))[#eval(body, mode: "markup")]
    #if doc-meta.author != "" or doc-meta.date != "" [
      #v(0.5cm)
      #text(size: 18pt)[
        #doc-meta.author
        #if doc-meta.author != "" and doc-meta.date != "" [ · ]
        #doc-meta.date
      ]
    ]
  ]
]
