// Deliberately broken: the delimiter opened on line 9 is never closed, so the
// preview shows only the error banner. Fix it and save to see the pages; break
// it again and the last pages that compiled stay up under the banner.
#set page(paper: "a6")

= Errors

#lorem(30)
#let broken = (1, 2,
