# Math

Inline formulas sit between single dollar signs: $x^2 + y^2 = r^2$,
$\alpha \rightarrow \beta$ and $\frac{a+b}{c}$. Off the cursor's line,
common LaTeX commands become Unicode symbols.

A display formula on one line goes between double dollar signs:

$$E = mc^2$$

$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$

A block can span several lines between `$$` lines. Off the cursor, a
block is laid out over several rows. A fraction:

$$
\frac{a + b}{c - d} = \frac{1}{2}
$$

A sum with its limits:

$$
\sum_{k=0}^{\infty} \frac{x^k}{k!} = e^x
$$

A matrix:

$$
A = \begin{pmatrix} a & b \\ c & d \end{pmatrix}
$$

Cases:

$$
|x| = \begin{cases} x & x \ge 0 \\ -x & x < 0 \end{cases}
$$

Aligned equations:

$$
\begin{aligned}
(a + b)^2 &= a^2 + 2ab + b^2 \\
(a - b)^2 &= a^2 - 2ab + b^2
\end{aligned}
$$

Move the cursor onto a formula to see its TeX: on any line of a block,
all of the block shows.

`math = false` under `[markdown]` in `input.conf` keeps the TeX as
written.
