// Hero / cover page: large centred title, wide margins. Reads `doc-meta` /
// `brand` from the driver-injected context (README: "Typst layout context")
// — a document with no metadata or brand renders exactly as before, since
// every accessor below falls back to the prior hardcoded default.
#import "/context.typ": doc-meta, brand-color, brand-font

#let layout(body) = [
  #set page(margin: (top: 6cm, bottom: 4cm, x: 3cm))
  #set text(font: brand-font("sans", default: "New Computer Modern"), size: 14pt)
  #align(center)[
    #text(size: 28pt, weight: "bold", fill: brand-color("accent", default: black))[#eval(body, mode: "markup")]
    #if doc-meta.author != "" or doc-meta.date != "" [
      #v(0.5cm)
      #text(size: 12pt)[
        #doc-meta.author
        #if doc-meta.author != "" and doc-meta.date != "" [ · ]
        #doc-meta.date
      ]
    ]
  ]
]
