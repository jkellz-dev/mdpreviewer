#set page(paper: "a5", numbering: "1")
#set heading(numbering: "1.")

= Lorem ipsum

#lorem(80)

== Mathematics

The sum of the first $n$ integers:

$ sum_(i=1)^n i = (n(n+1)) / 2 $

== A table

#table(
  columns: 3,
  [*Name*], [*Kind*], [*Size*],
  [alpha], [first], [1],
  [beta], [second], [22],
  [gamma], [third], [333],
)

== A figure

#figure(
  image("typst-figure.svg", width: 60%),
  caption: [An image read from a file next to the document.],
)

#include "typst-chapter.typ"

= Dolor sit amet

#lorem(300)
