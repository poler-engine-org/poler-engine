# POLER Mathematical Treatise — Volume IV: Active Inference & IIR Field Resonance

> **Mathematical Grounding:** Karl Friston Variational Free Energy Principle (FEP), IIR Digital Filter Transfer Functions, Information Density Dynamics  
> **Code Artifacts:** `src/resonance/mod.rs`, `src/psi.rs`, `src/poler.rs`

---

## 1. Theorem IV.1 (Equivalence of IIR Resonance Filter to Continuous Decay Integral)

### Statement
The first-order discrete IIR recurrence:
$$R[t] = \rho \cdot R[t-1] + \alpha \cdot x[t], \quad \rho \in (0, 1), \quad \alpha > 0$$
has the Z-transform transfer function:
$$H(z) = \frac{\alpha}{1 - \rho z^{-1}}$$
and its impulse response corresponds exactly to the exponential memory kernel:
$$h[n] = \alpha \cdot \rho^n u[n]$$
whose continuous limit ($t = n \Delta t$, $\rho = e^{-\lambda \Delta t}$) is the Volterra memory convolution:
$$R(t) = \alpha \int_{-\infty}^t e^{-\lambda(t - \tau)} x(\tau) d\tau$$

### Formal Proof
1. Take the Z-transform of both sides:
   $$\mathcal{Z}\{R[t]\} = \rho z^{-1} \mathcal{Z}\{R[t]\} + \alpha \mathcal{Z}\{x[t]\}$$
   $$R(z)(1 - \rho z^{-1}) = \alpha X(z) \implies H(z) = \frac{R(z)}{X(z)} = \frac{\alpha}{1 - \rho z^{-1}}$$
2. Taking the inverse Z-transform:
   $$h[n] = \mathcal{Z}^{-1}\left\{ \frac{\alpha}{1 - \rho z^{-1}} \right\} = \alpha \rho^n u[n]$$
3. By convolution theorem:
   $$R[n] = (x * h)[n] = \sum_{k=0}^n x[k] \cdot \alpha \rho^{n-k} = \alpha \sum_{k=0}^n e^{-\lambda \Delta t (n-k)} x[k]$$
   As $\Delta t \to 0$, this Riemann sum converges to the continuous integral $\int_0^t e^{-\lambda(t-\tau)} x(\tau) d\tau \quad \blacksquare$$

---

## 2. Theorem IV.2 (Variational Free Energy Minimization in Semantic Retrieval)

### Statement
Let $o$ be the observed query and $p$ be the latent semantic document manifold. The variational free energy:
$$\mathcal{F}(p, o) = D_{\text{KL}}(q(s|p) \parallel P(s)) - \mathbb{E}_{q}[\ln P(o|s)]$$
is minimized when the latent state $p$ matches the active attention field:
$$\frac{dp}{dt} = -\eta \cdot \Pi_\Lambda \Big( D \cdot p + \gamma J(p) \cdot p + \nabla_p \mathcal{F}(p, o) \Big)$$
guaranteeing convergence to a stable cognitive attractor $p^*$ with minimum semantic divergence.
