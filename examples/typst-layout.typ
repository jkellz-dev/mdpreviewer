// Pages of different sizes and orientations, columns, footnotes, a running
// header and a floating figure: the line markers must land on the right page
// whatever its shape.
#set page(
  paper: "a5",
  header: align(right, text(8pt)[Layout fixture]),
  numbering: "1 / 1",
)
#set par(justify: true)

= Portrait page

Lorem ipsum dolor sit amet#footnote[A footnote at the foot of the page.],
consectetur adipiscing elit. #lorem(60)

#figure(
  placement: top,
  rect(width: 100%, height: 3cm, fill: luma(230)),
  caption: [A figure floated to the top of the page.],
)

#lorem(80)

#set page(flipped: true)

= Landscape page in two columns

#columns(2)[
  #lorem(120)

  #colbreak()

  #lorem(100)
]

#set page(paper: "a6", flipped: false)

= A small page

#lorem(40)#footnote[Footnotes on a smaller page.]

#pagebreak()

== Continued

#lorem(50)
