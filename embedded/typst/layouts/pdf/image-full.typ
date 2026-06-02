// Full-bleed image page. v1 renders body as raw markdown; the image-resolution
// step that turns ![alt](key) into an actual `image()` lookup against the
// AssetProvider lands with the Mermaid/diagram pre-processor in Phase 4.

#let layout(body) = [
  #set page(margin: 0cm)
  #eval(body, mode: "markup")
]
