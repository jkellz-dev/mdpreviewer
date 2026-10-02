// Imports a package from Typst Universe. The first preview downloads it into
// the shared typst package cache; later ones read it from there.
#import "@preview/cetz:0.4.2"

#set page(paper: "a5")

= A package diagram

#lorem(30)

#cetz.canvas({
  import cetz.draw: *
  circle((0, 0), radius: 1, fill: blue.lighten(60%))
  rect((1.5, -1), (3.5, 1), fill: green.lighten(60%))
  line((-1.5, -1.5), (4, 1.5), stroke: red, mark: (end: ">"))
  content((1.25, -1.6), [cetz])
})

#lorem(40)
