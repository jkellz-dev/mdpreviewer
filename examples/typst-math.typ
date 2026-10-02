#set page(paper: "a5", numbering: "1")
#set math.equation(numbering: "(1)")

= Inline and display math

Lorem ipsum $a^2 + b^2 = c^2$ dolor sit amet, with $alpha$, $beta$ and
$gamma$ inline. A numbered display equation:

$ integral_0^infinity e^(-x^2) dif x = sqrt(pi) / 2 $ <gauss>

As @gauss shows, consectetur adipiscing elit.

= Alignment

$
  (a + b)^2 & = (a + b)(a + b) \
            & = a^2 + a b + b a + b^2 \
            & = a^2 + 2 a b + b^2
$

= Matrices and cases

$
  A = mat(1, 2, 3; 4, 5, 6; 7, 8, 9), quad
  det(A) = 0
$

$
  f(x) = cases(
    x^2 & "if" x >= 0,
    -x & "otherwise",
  )
$

= Sums, limits and fractions

$ sum_(k=1)^n k^3 = ((n(n+1)) / 2)^2 $

$ lim_(n -> infinity) (1 + 1/n)^n = e $

$ phi = (1 + sqrt(5)) / 2 = 1 + 1 / (1 + 1 / (1 + 1 / (1 + dots.down))) $

#lorem(60)
