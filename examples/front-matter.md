---
title: "Lorem <ipsum> & dolor"
draft: true
nested:
  sit: amet
  list:
    - consectetur
    - adipiscing
multiline: |
  Sed do eiusmod tempor
  incididunt ut labore
---

# Front Matter Edge Cases

The front matter above has nested maps, a list, a block scalar, and HTML
special characters, which must appear escaped in the collapsed block.

Lorem ipsum dolor sit amet, consectetur adipiscing elit.

---

The rule above is mid-document and must render as a horizontal rule, not as
front matter.
