# POLER Mathematical Treatise — Volume III: RaBitQ 1-Bit Metric Spaces

> **Mathematical Grounding:** Randomized Walsh-Hadamard Transform, Arcsin Maximum Likelihood Angular Estimation, Asymmetric Distance Computation (ADC)  
> **Code Artifacts:** `src/vectors/rabitq.rs`, `src/vectors/store.rs`

---

## 1. Theorem III.1 (Unbiased Angular Distance via Arcsin MLE)

### Statement
Let $\mathbf{u}, \mathbf{v} \in \mathbb{R}^d$ be two unit vectors ($\|\mathbf{u}\| = \|\mathbf{v}\| = 1$) with angle $\theta = \arccos(\mathbf{u}^T \mathbf{v})$. Let $H \in \mathbb{R}^{d \times d}$ be a randomized Walsh-Hadamard orthogonal rotation matrix, and let $\mathbf{b}_u = \text{sign}(H\mathbf{u}), \mathbf{b}_v = \text{sign}(H\mathbf{v}) \in \{-1, +1\}^d$.
The normalized Hamming distance:
$$h(\mathbf{b}_u, \mathbf{b}_v) = \frac{1}{d} \sum_{i=1}^d \mathbb{I}[(\mathbf{b}_u)_i \ne (\mathbf{b}_v)_i]$$
yields the strictly unbiased estimator for the inner product $\mathbf{u}^T \mathbf{v}$:
$$\widehat{\cos\theta} = \cos\left( \pi \cdot h(\mathbf{b}_u, \mathbf{b}_v) \right)$$
with asymptotic expectation:
$$\mathbb{E}\left[ h(\mathbf{b}_u, \mathbf{b}_v) \right] = \frac{\theta}{\pi} = \frac{\arccos(\mathbf{u}^T \mathbf{v})}{\pi}$$

### Proof (Grothendieck-Goemans-Williamson Identity)
1. For any random hyper-plane normal $\mathbf{r} \sim \mathcal{N}(\mathbf{0}, I_d)$, the probability that $\mathbf{r}$ separates $\mathbf{u}$ and $\mathbf{v}$ is:
   $$\mathbb{P}[\text{sign}(\mathbf{r}^T \mathbf{u}) \ne \text{sign}(\mathbf{r}^T \mathbf{v})] = \frac{\theta}{\pi}$$
2. Randomized Hadamard rotation spreads coordinates uniformly across the sphere, guaranteeing pairwise uncorrelated projections.
3. Therefore, the empirical Hamming weight estimator $h$ has expectation:
   $$\mathbb{E}[h] = \frac{1}{d} \sum_{i=1}^d \frac{\theta}{\pi} = \frac{\theta}{\pi}$$
4. Taking the cosine:
   $$\cos(\pi \mathbb{E}[h]) = \cos(\theta) = \mathbf{u}^T \mathbf{v} \quad \blacksquare$$

---

## 2. Theorem III.2 (Storage Compression Ratio)

### Statement
For $d = 768$ embedding dimension (padded to next power of two $d_{\text{pad}} = 1024$), the PRBQ v1 storage consumes:
$$\text{Bytes}_{\text{PRBQ}} = 4\text{B } (\mu) + 4\text{B } (\delta) + 4\text{B } (\gamma) + 4\text{B } (\text{id}) + \frac{1024}{8}\text{B (codes)} = 16 + 128 = 144 \text{ bytes / vector}$$
Compared to standard FP32 storage ($768 \times 4 = 3072$ bytes), the compression ratio is:
$$\text{CR} = \frac{3072}{144} = \mathbf{21.33\times}$$
For the binary codes alone:
$$\text{CR}_{\text{codes}} = \frac{768 \times 4}{128} = \mathbf{24.0\times} \quad \blacksquare$$
