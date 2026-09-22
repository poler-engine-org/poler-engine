//! Матрично-квантовое ядро POLER Matrix Calc (цикл M, v0.48.0).
//!
//! Исправленные дефекты прошлой сессии (задокументированы тестами):
//! - Паде [6/6] для expm: коэффициент b4 = **1/792** (не 1/1584);
//! - Фаддеев–Леврерье: M_k = A·M_{k−1} + c_k·I — прибавление ТОЛЬКО к
//!   диагонали (add_scalar ко всем элементам ломал характеристический
//!   полином);
//! - собственные значения — через Дюрана–Кернера по charpoly.
//!
//! Квантовая часть: генераторы Ли so(2)/so(3) и их экспоненты —
//! точные вращения expm(J·θ), как в живом голосе (J = A − Aᵀ).

use super::solve::{durand_kerner, sort_roots, Complex};

/// Плотная матрица, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Matrix {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f64>,
}

impl Matrix {
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Matrix { rows, cols, data: vec![0.0; rows * cols] }
    }

    pub fn identity(n: usize) -> Self {
        let mut m = Self::zeros(n, n);
        for i in 0..n {
            m.data[i * n + i] = 1.0;
        }
        m
    }

    pub fn from_rows(rows: &[Vec<f64>]) -> Result<Self, String> {
        if rows.is_empty() {
            return Err("пустая матрица".into());
        }
        let cols = rows[0].len();
        if cols == 0 || rows.iter().any(|r| r.len() != cols) {
            return Err("строки матрицы разной длины".into());
        }
        Ok(Matrix {
            rows: rows.len(),
            cols,
            data: rows.iter().flat_map(|r| r.iter().copied()).collect(),
        })
    }

    #[inline]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        self.data[i * self.cols + j]
    }
    #[inline]
    pub fn set(&mut self, i: usize, j: usize, v: f64) {
        self.data[i * self.cols + j] = v;
    }

    pub fn is_square(&self) -> bool {
        self.rows == self.cols
    }

    pub fn transpose(&self) -> Matrix {
        let mut t = Matrix::zeros(self.cols, self.rows);
        for i in 0..self.rows {
            for j in 0..self.cols {
                t.set(j, i, self.get(i, j));
            }
        }
        t
    }

    pub fn trace(&self) -> Result<f64, String> {
        if !self.is_square() {
            return Err("след определён только для квадратных матриц".into());
        }
        Ok((0..self.rows).map(|i| self.get(i, i)).sum())
    }

    pub fn add(&self, o: &Matrix) -> Result<Matrix, String> {
        if self.rows != o.rows || self.cols != o.cols {
            return Err(format!(
                "размеры не совпадают: {}×{} + {}×{}",
                self.rows, self.cols, o.rows, o.cols
            ));
        }
        let mut r = self.clone();
        for (a, b) in r.data.iter_mut().zip(o.data.iter()) {
            *a += b;
        }
        Ok(r)
    }

    pub fn sub(&self, o: &Matrix) -> Result<Matrix, String> {
        if self.rows != o.rows || self.cols != o.cols {
            return Err(format!(
                "размеры не совпадают: {}×{} − {}×{}",
                self.rows, self.cols, o.rows, o.cols
            ));
        }
        let mut r = self.clone();
        for (a, b) in r.data.iter_mut().zip(o.data.iter()) {
            *a -= b;
        }
        Ok(r)
    }

    pub fn scale(&self, k: f64) -> Matrix {
        let mut r = self.clone();
        for v in r.data.iter_mut() {
            *v *= k;
        }
        r
    }

    pub fn mul(&self, o: &Matrix) -> Result<Matrix, String> {
        if self.cols != o.rows {
            return Err(format!(
                "несовместимо для умножения: {}×{} · {}×{}",
                self.rows, self.cols, o.rows, o.cols
            ));
        }
        let mut r = Matrix::zeros(self.rows, o.cols);
        for i in 0..self.rows {
            for k in 0..self.cols {
                let a = self.get(i, k);
                if a == 0.0 {
                    continue;
                }
                for j in 0..o.cols {
                    let v = r.get(i, j) + a * o.get(k, j);
                    r.set(i, j, v);
                }
            }
        }
        Ok(r)
    }

    /// Определитель: LU с частичным выбором ведущего элемента.
    pub fn det(&self) -> Result<f64, String> {
        if !self.is_square() {
            return Err("определитель только для квадратных матриц".into());
        }
        let n = self.rows;
        let mut a = self.data.clone();
        let mut det = 1.0;
        for col in 0..n {
            let mut piv = col;
            for r in col + 1..n {
                if a[r * n + col].abs() > a[piv * n + col].abs() {
                    piv = r;
                }
            }
            if a[piv * n + col].abs() < 1e-300 {
                return Ok(0.0);
            }
            if piv != col {
                for j in 0..n {
                    a.swap(piv * n + j, col * n + j);
                }
                det = -det;
            }
            let d = a[col * n + col];
            det *= d;
            for r in col + 1..n {
                let k = a[r * n + col] / d;
                if k != 0.0 {
                    for j in col..n {
                        a[r * n + j] -= k * a[col * n + j];
                    }
                }
            }
        }
        Ok(det)
    }

    /// Обратная матрица: Гаусс–Жордан с выбором ведущего.
    pub fn inv(&self) -> Result<Matrix, String> {
        if !self.is_square() {
            return Err("обратная только для квадратных матриц".into());
        }
        let n = self.rows;
        let mut a = self.data.clone();
        let mut b = Matrix::identity(n).data;
        for col in 0..n {
            let mut piv = col;
            for r in col + 1..n {
                if a[r * n + col].abs() > a[piv * n + col].abs() {
                    piv = r;
                }
            }
            if a[piv * n + col].abs() < 1e-13 {
                return Err(
                    "матрица вырождена (сингулярна) — псевдообратная Мура–Пенроуза: pinv(...)".into(),
                );
            }
            swap_rows_vec(&mut a, n, piv, col);
            swap_rows_vec(&mut b, n, piv, col);
            let d = a[col * n + col];
            for j in 0..n {
                a[col * n + j] /= d;
                b[col * n + j] /= d;
            }
            for r in 0..n {
                if r != col {
                    let k = a[r * n + col];
                    if k != 0.0 {
                        for j in 0..n {
                            a[r * n + j] -= k * a[col * n + j];
                            b[r * n + j] -= k * b[col * n + j];
                        }
                    }
                }
            }
        }
        Ok(Matrix { rows: n, cols: n, data: b })
    }

    /// Псевдообратная Мура–Пенроуза: алгоритм Гревилля (колоночный, без SVD).
    ///
    /// A⁺ определена для ЛЮБОЙ матрицы — вырожденной, прямоугольной, нулевой.
    /// Строится наращиванием по столбцам: для нового столбца aₖ остаток
    /// cₖ = aₖ − Aₖ₋₁Aₖ₋₁⁺aₖ вне образа предыдущих столбцов либо становится
    /// новой строкой bₖᵀ = cₖᵀ/(cₖᵀcₖ), либо (cₖ ≈ 0) сворачивается в
    /// bₖᵀ = dₖᵀAₖ₋₁⁺/(1+dₖᵀdₖ), а прошлые строки корректируются на dₖbₖᵀ.
    /// Удовлетворяет четырём условиям Мура–Пенроуза (см. тест): проекции
    /// AA⁺ и A⁺A симметричны и идемпотентны. Для плохо обусловленных
    /// матриц шум усиливается — это цена отказа от SVD-пути.
    pub fn pinv(&self) -> Result<Matrix, String> {
        let (m, n) = (self.rows, self.cols);
        if !self.data.iter().all(|v| v.is_finite()) {
            return Err("pinv: элементы должны быть конечны".into());
        }
        let scale = self.norm_inf();
        if scale == 0.0 {
            return Ok(Matrix::zeros(n, m)); // нулевая матрица → нулевая A⁺
        }
        // P — растущая Aₖ⁺: k строк × m столбцов.
        let mut p: Vec<Vec<f64>> = Vec::with_capacity(n);
        for k in 0..n {
            // столбец aₖ (m-вектор)
            let a: Vec<f64> = (0..m).map(|i| self.get(i, k)).collect();
            // d = P·aₖ (k-вектор)
            let d: Vec<f64> = p
                .iter()
                .map(|r| r.iter().zip(&a).map(|(x, y)| x * y).sum())
                .collect();
            // c = aₖ − Aₖ₋₁·d — остаток вне образа предыдущих столбцов
            let mut c = a.clone();
            for (j, dj) in d.iter().enumerate() {
                if *dj != 0.0 {
                    for i in 0..m {
                        c[i] -= self.get(i, j) * dj;
                    }
                }
            }
            let c2: f64 = c.iter().map(|x| x * x).sum();
            // относительный порог численного нуля (к столбцу и к масштабу A)
            let a2: f64 = a.iter().map(|x| x * x).sum();
            let tol2 = (1e-10 * a2.sqrt().max(1e-12 * scale)).powi(2);
            let b: Vec<f64> = if c2 > tol2 {
                c.iter().map(|x| x / c2).collect()
            } else {
                let d2: f64 = d.iter().map(|x| x * x).sum();
                let denom = 1.0 + d2;
                (0..m)
                    .map(|col| {
                        d.iter().zip(&p).map(|(dv, r)| dv * r[col]).sum::<f64>() / denom
                    })
                    .collect()
            };
            // Pₖ = [Pₖ₋₁ − d·bᵀ ; b]
            for (i, dv) in d.iter().enumerate() {
                if *dv != 0.0 {
                    for col in 0..m {
                        p[i][col] -= dv * b[col];
                    }
                }
            }
            p.push(b);
        }
        Matrix::from_rows(&p)
    }

    /// Норма ∞ (максимум сумм модулей строк).
    pub fn norm_inf(&self) -> f64 {
        (0..self.rows)
            .map(|i| (0..self.cols).map(|j| self.get(i, j).abs()).sum::<f64>())
            .fold(0.0, f64::max)
    }

    /// Экспонента матрицы: масштабирование-возведение в степень + Паде [6/6].
    ///
    /// Коэффициенты числителя Паде [6/6] функции e^x (проверены
    /// символьно: p_k = (12−k)!·6! / (12!·k!·(6−k)!)):
    ///   [1, 1/2, 5/44, 1/66, **1/792**, 1/15840, 1/665280]
    /// Регрессия прошлой сессии: b4 ошибочно был 1/1584.
    pub fn expm(&self) -> Result<Matrix, String> {
        if !self.is_square() {
            return Err("expm определён для квадратных матриц".into());
        }
        let n = self.rows;
        let b: [f64; 7] = [
            1.0,
            1.0 / 2.0,
            5.0 / 44.0,
            1.0 / 66.0,
            1.0 / 792.0,
            1.0 / 15840.0,
            1.0 / 665280.0,
        ];

        // масштабируем к ||A||∞ ≤ 1/2
        let mut s = 0u32;
        let norm = self.norm_inf();
        if norm > 0.5 {
            s = (norm / 0.5).log2().ceil().max(0.0) as u32 + 1;
        }
        let scale = 1.0 / (2u64.pow(s) as f64);
        let bs = self.scale(scale);

        // P(B) по Хорнеру: I·b6, затем B·acc + I·b_k
        let mut acc = Matrix::identity(n).scale(b[6]);
        for k in (0..6).rev() {
            acc = bs.mul(&acc)?; // B·acc
            // acc += b[k]·I — прибавление к диагонали
            for i in 0..n {
                let v = acc.get(i, i) + b[k];
                acc.set(i, i, v);
            }
        }
        // P(−B)
        let negb = bs.scale(-1.0);
        let mut negp = Matrix::identity(n).scale(b[6]);
        for k in (0..6).rev() {
            negp = negb.mul(&negp)?;
            for i in 0..n {
                let v = negp.get(i, i) + b[k];
                negp.set(i, i, v);
            }
        }
        // X = P(−B)^{-1}·P(B)
        let mut x = negp.inv()?.mul(&acc)?;
        // возведение в квадрат s раз
        for _ in 0..s {
            x = x.mul(&x)?;
        }
        Ok(x)
    }

    /// Характеристический полином по Фаддееву–Леврерье.
    /// Возвращает коэффициенты по убыванию степени (monic):
    /// p(λ) = λⁿ + c₁λⁿ⁻¹ + … + cₙ.
    ///
    /// ИНВАРИАНТ (регрессия прошлой сессии): прибавление c_k·I — только
    /// к ДИАГОНАЛИ M_k, ни в коем случае не ко всем элементам.
    pub fn charpoly(&self) -> Result<Vec<f64>, String> {
        if !self.is_square() {
            return Err("характеристический полином — только для квадратных".into());
        }
        let n = self.rows;
        let mut coeffs = vec![0.0; n + 1];
        coeffs[0] = 1.0; // старший (monic)
        let mut m = Matrix::zeros(n, n); // M_0 = 0
        for k in 1..=n {
            // M_k = A·M_{k−1} + c_{k−1}·I  (c с индексом n−k+1 в 1-based нотации)
            m = self.mul(&m)?;
            for i in 0..n {
                let v = m.get(i, i) + coeffs[k - 1];
                m.set(i, i, v); // ТОЛЬКО диагональ
            }
            // c_k = −tr(A·M_k)/k
            let am = self.mul(&m)?;
            let tr: f64 = (0..n).map(|i| am.get(i, i)).sum();
            coeffs[k] = -tr / k as f64;
        }
        Ok(coeffs)
    }

    /// Собственные значения: charpoly → Дюран–Кернер (комплексные).
    pub fn eigenvalues(&self) -> Result<Vec<Complex>, String> {
        let coeffs = self.charpoly()?;
        if self.rows == 0 {
            return Ok(Vec::new());
        }
        Ok(sort_roots(durand_kerner(&coeffs, 300)))
    }

    // -----------------------------------------------------------------
    // Группа Ли SO(n): генераторы и вращения
    // -----------------------------------------------------------------

    /// Генератор вращения плоскости (i,j) в R^n: J_ij имеет −1 на (i,j),
    /// +1 на (j,i) — кососимметричный, как ротор живого голоса J = A − Aᵀ.
    /// КОНВЕНЦИЯ (регрессия прошлой сессии): expm(so_generator(i,j)·θ)
    /// РАВЕН rotation(i,j,θ) — знаки согласованы.
    pub fn so_generator(n: usize, i: usize, j: usize) -> Result<Matrix, String> {
        if i >= n || j >= n || i == j {
            return Err("нужны два разных индекса 0..n".into());
        }
        let mut g = Matrix::zeros(n, n);
        g.set(i, j, -1.0);
        g.set(j, i, 1.0);
        Ok(g)
    }

    /// Точное вращение плоскости (i,j) на угол θ (рад):
    /// R = expm(J_ij·θ) — но записываем аналитически (быстро и точно).
    pub fn rotation(n: usize, i: usize, j: usize, theta: f64) -> Result<Matrix, String> {
        if i >= n || j >= n || i == j {
            return Err("нужны два разных индекса 0..n".into());
        }
        let mut r = Matrix::identity(n);
        r.set(i, i, theta.cos());
        r.set(j, j, theta.cos());
        r.set(i, j, -theta.sin());
        r.set(j, i, theta.sin());
        Ok(r)
    }

    /// Вращение 2D (θ в радианах).
    pub fn rot2(theta: f64) -> Matrix {
        Matrix::from_rows(&[
            vec![theta.cos(), -theta.sin()],
            vec![theta.sin(), theta.cos()],
        ])
        .unwrap()
    }

    /// Вращение 3D вокруг оси X.
    pub fn rot_x(theta: f64) -> Matrix {
        Matrix::from_rows(&[
            vec![1.0, 0.0, 0.0],
            vec![0.0, theta.cos(), -theta.sin()],
            vec![0.0, theta.sin(), theta.cos()],
        ])
        .unwrap()
    }

    /// Вращение 3D вокруг оси Y.
    pub fn rot_y(theta: f64) -> Matrix {
        Matrix::from_rows(&[
            vec![theta.cos(), 0.0, theta.sin()],
            vec![0.0, 1.0, 0.0],
            vec![-theta.sin(), 0.0, theta.cos()],
        ])
        .unwrap()
    }

    /// Вращение 3D вокруг оси Z.
    pub fn rot_z(theta: f64) -> Matrix {
        Matrix::from_rows(&[
            vec![theta.cos(), -theta.sin(), 0.0],
            vec![theta.sin(), theta.cos(), 0.0],
            vec![0.0, 0.0, 1.0],
        ])
        .unwrap()
    }
}

/// Перестановка строк r1 ↔ r2 в плотном n×n буфере (row-major).
fn swap_rows_vec(v: &mut [f64], n: usize, r1: usize, r2: usize) {
    for j in 0..n {
        v.swap(r1 * n + j, r2 * n + j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * (1.0 + a.abs() + b.abs())
    }

    fn m22(a: f64, b: f64, c: f64, d: f64) -> Matrix {
        Matrix::from_rows(&[vec![a, b], vec![c, d]]).unwrap()
    }

    #[test]
    fn construction_errors() {
        assert!(Matrix::from_rows(&[]).is_err());
        assert!(Matrix::from_rows(&[vec![1.0], vec![1.0, 2.0]]).is_err());
        assert!(Matrix::from_rows(&[vec![]]).is_err());
    }

    #[test]
    fn pinv_moore_penrose() {
        // вырожденная [1,2;2,4] (det = 0, stress-тест цикла N):
        // A = u·vᵀ с u = v = [1;2] → A⁺ = A/25
        let a = m22(1.0, 2.0, 2.0, 4.0);
        let p = a.pinv().unwrap();
        assert!(close(p.get(0, 0), 0.04, 1e-12), "{}", p.get(0, 0));
        assert!(close(p.get(0, 1), 0.08, 1e-12));
        assert!(close(p.get(1, 0), 0.08, 1e-12));
        assert!(close(p.get(1, 1), 0.16, 1e-12));

        // четыре условия Мура–Пенроуза: AA⁺A=A, A⁺AA⁺=A⁺,
        // (AA⁺)ᵀ=AA⁺, (A⁺A)ᵀ=A⁺A
        let apa = a.mul(&p).unwrap().mul(&a).unwrap();
        assert!(close(apa.get(0, 0), 1.0, 1e-12) && close(apa.get(1, 1), 4.0, 1e-12));
        let pap = p.mul(&a).unwrap().mul(&p).unwrap();
        assert!(close(pap.get(0, 0), 0.04, 1e-12) && close(pap.get(1, 1), 0.16, 1e-12));
        let aat = a.mul(&p).unwrap();
        let sym1 = aat.transpose();
        for i in 0..2 {
            for j in 0..2 {
                assert!(close(aat.get(i, j), sym1.get(i, j), 1e-12));
            }
        }

        // невырожденная: pinv = inv
        let b = m22(4.0, 7.0, 2.0, 6.0);
        let bp = b.pinv().unwrap();
        let bi = b.inv().unwrap();
        for i in 0..2 {
            for j in 0..2 {
                assert!(close(bp.get(i, j), bi.get(i, j), 1e-9));
            }
        }

        // прямоугольная строка [1,2,3]: A⁺ = aᵀ/‖a‖² = [1,2,3]ᵀ/14
        let row = Matrix::from_rows(&[vec![1.0, 2.0, 3.0]]).unwrap();
        let rp = row.pinv().unwrap();
        assert_eq!((rp.rows, rp.cols), (3, 1));
        assert!(close(rp.get(0, 0), 1.0 / 14.0, 1e-12));
        assert!(close(rp.get(1, 0), 2.0 / 14.0, 1e-12));
        assert!(close(rp.get(2, 0), 3.0 / 14.0, 1e-12));

        // нулевой столбец — нулевая строка A⁺; единичная — сама себя
        let z = Matrix::from_rows(&[vec![0.0, 1.0], vec![0.0, 2.0]]).unwrap();
        let zp = z.pinv().unwrap();
        assert_eq!(zp.get(0, 0).abs() + zp.get(0, 1).abs(), 0.0);
        let eye = Matrix::identity(3);
        let ep = eye.pinv().unwrap();
        for i in 0..3 {
            for j in 0..3 {
                assert_eq!(ep.get(i, j), eye.get(i, j));
            }
        }

        // нулевая матрица — нулевая A⁺ (транспонированной формы)
        let zz = Matrix::from_rows(&[vec![0.0, 0.0], vec![0.0, 0.0]]).unwrap();
        let zpp = zz.pinv().unwrap();
        assert_eq!((zpp.rows, zpp.cols), (2, 2));
        assert!(zpp.data.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn mul_add_basic() {
        let a = m22(1.0, 2.0, 3.0, 4.0);
        let b = m22(5.0, 6.0, 7.0, 8.0);
        let p = a.mul(&b).unwrap();
        assert_eq!(p.get(0, 0), 19.0);
        assert_eq!(p.get(0, 1), 22.0);
        assert_eq!(p.get(1, 0), 43.0);
        assert_eq!(p.get(1, 1), 50.0);
        let s = a.add(&b).unwrap();
        assert_eq!(s.get(1, 1), 12.0);
        // несовместимые
        let c = Matrix::from_rows(&[vec![1.0, 2.0, 3.0]]).unwrap();
        assert!(a.mul(&c).is_err());
        assert!(a.add(&c).is_err());
    }

    #[test]
    fn det_and_inv() {
        let a = m22(1.0, 2.0, 3.0, 4.0);
        assert!((a.det().unwrap() - (-2.0)).abs() < 1e-12);
        let inv = a.inv().unwrap();
        // A·A⁻¹ = I
        let eye = a.mul(&inv).unwrap();
        assert!(close(eye.get(0, 0), 1.0, 1e-12));
        assert!(close(eye.get(0, 1), 0.0, 1e-12));
        assert!(close(eye.get(1, 1), 1.0, 1e-12));
        // сингулярная
        assert!(m22(1.0, 2.0, 2.0, 4.0).inv().is_err());
        // det 3×3 (правило Сарруса): |1 2 3; 4 5 6; 7 8 10| = 1(50−48) −2(40−42) +3(32−35) = 2+4−9 = −3
        let m3 = Matrix::from_rows(&[vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0], vec![7.0, 8.0, 10.0]]).unwrap();
        assert!((m3.det().unwrap() - (-3.0)).abs() < 1e-12);
    }

    #[test]
    fn trace_transpose() {
        let a = m22(1.0, 2.0, 3.0, 4.0);
        assert!((a.trace().unwrap() - 5.0).abs() < 1e-15);
        let t = a.transpose();
        assert_eq!(t.get(0, 1), 3.0);
        assert_eq!(t.get(1, 0), 2.0);
        let rect = Matrix::from_rows(&[vec![1.0, 2.0, 3.0]]).unwrap();
        assert!(rect.trace().is_err());
        assert!(rect.det().is_err());
    }

    // ================================================================
    // РЕГРЕССИЯ прошлой сессии: Паде b4 = 1/792 (было 1/1584 — вдвое
    // меньше правильного). expm вращательного генератора обязан давать
    // точную ротацию; ошибка в b4 ломала её на ~1e-4.
    // ================================================================
    #[test]
    fn expm_rotation_generator() {
        // J = [[0, −θ],[θ, 0]] → expm(J) = [[cos θ, −sin θ],[sin θ, cos θ]]
        for &theta in &[0.7, 2.5, 10.0] {
            let j = m22(0.0, -theta, theta, 0.0);
            let e = j.expm().unwrap();
            let r = Matrix::rot2(theta);
            for (a, b) in e.data.iter().zip(r.data.iter()) {
                assert!(close(*a, *b, 1e-12), "expm ≠ rot при θ={theta}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn expm_diagonal_and_nilpotent() {
        // expm(αI) = e^α·I — проверяет масштабирование (норма 10 → s>0)
        let a = Matrix::identity(3).scale(10.0);
        let e = a.expm().unwrap();
        let want = 10.0f64.exp();
        for i in 0..3 {
            assert!(close(e.get(i, i), want, 1e-12));
            for j in 0..3 {
                if i != j {
                    assert!(close(e.get(i, j), 0.0, 1e-12));
                }
            }
        }
        // нильпотентная: expm([[0,1],[0,0]]) = [[1,1],[0,0]]+I = [[1,1],[0,1]]
        let nil = m22(0.0, 1.0, 0.0, 0.0);
        let e = nil.expm().unwrap();
        assert!(close(e.get(0, 0), 1.0, 1e-14));
        assert!(close(e.get(0, 1), 1.0, 1e-14));
        assert!(close(e.get(1, 0), 0.0, 1e-14));
        assert!(close(e.get(1, 1), 1.0, 1e-14));
        // expm(0) = I
        let z = Matrix::zeros(4, 4);
        assert_eq!(z.expm().unwrap(), Matrix::identity(4));
        // неквадратная — ошибка
        let rect = Matrix::from_rows(&[vec![1.0, 2.0]]).unwrap();
        assert!(rect.expm().is_err());
    }

    #[test]
    fn expm_agrees_with_series() {
        // небольшая матрица: сравнение с рядом Тейлора 20 членов
        let a = m22(0.3, -0.2, 0.1, 0.05);
        let mut t = Matrix::identity(2);
        let mut term = Matrix::identity(2);
        for k in 1..=20 {
            term = term.mul(&a).unwrap().scale(1.0 / k as f64);
            t = t.add(&term).unwrap();
        }
        let e = a.expm().unwrap();
        for (x, y) in e.data.iter().zip(t.data.iter()) {
            assert!(close(*x, *y, 1e-12));
        }
        // det(expm(A)) = e^tr(A) — фундаментальное тождество
        let tr = a.trace().unwrap();
        assert!(close(e.det().unwrap(), tr.exp(), 1e-10));
    }

    // ================================================================
    // РЕГРЕССИЯ прошлой сессии: Фаддеев–Леврерье, add_scalar только на
    // диагональ. Ручная трассировка [[1,2],[3,4]]: p(λ)=λ²−5λ−2.
    // ================================================================
    #[test]
    fn charpoly_faddeev_leverrier() {
        let a = m22(1.0, 2.0, 3.0, 4.0);
        let c = a.charpoly().unwrap();
        assert_eq!(c.len(), 3);
        assert!((c[0] - 1.0).abs() < 1e-12);
        assert!((c[1] - (-5.0)).abs() < 1e-12, "c1 = {}", c[1]);
        assert!((c[2] - (-2.0)).abs() < 1e-12, "c2 = {}", c[2]);

        // единичная: (λ−1)ⁿ
        let i3 = Matrix::identity(3);
        let c = i3.charpoly().unwrap();
        assert!((c[0] - 1.0).abs() < 1e-12);
        assert!((c[1] - (-3.0)).abs() < 1e-12);
        assert!((c[2] - 3.0).abs() < 1e-12);
        assert!((c[3] - (-1.0)).abs() < 1e-12);

        // 3×3: [[2,0,0],[0,3,0],[0,0,5]] → (λ−2)(λ−3)(λ−5)
        let d = Matrix::from_rows(&[vec![2.0, 0.0, 0.0], vec![0.0, 3.0, 0.0], vec![0.0, 0.0, 5.0]]).unwrap();
        let c = d.charpoly().unwrap();
        // λ³ − 10λ² + 31λ − 30
        assert!((c[1] - (-10.0)).abs() < 1e-12, "{}", c[1]);
        assert!((c[2] - 31.0).abs() < 1e-12, "{}", c[2]);
        assert!((c[3] - (-30.0)).abs() < 1e-12, "{}", c[3]);

        // Кэли–Гамильтон на 4×4 случайной (фиксированный сид):
        // p(A) = 0
        let m4 = Matrix::from_rows(&[
            vec![0.5, 1.2, 0.0, -0.3],
            vec![-1.1, 0.2, 0.7, 0.0],
            vec![0.4, 0.0, 1.3, -0.8],
            vec![0.0, 0.9, -0.2, 0.6],
        ])
        .unwrap();
        let c = m4.charpoly().unwrap();
        let n = 4;
        let mut pa = Matrix::zeros(n, n); // p(A) = 0·Aⁿ + c1·Aⁿ⁻¹ + … (старший при Aⁿ — c0=1)
        let mut ak = Matrix::identity(n);
        // идём от старшей степени вниз: c0·A⁴? нет: c[0]·A^4 + c[1]·A^3 + ... + c[4]·I
        for (k, coef) in c.iter().enumerate() {
            let power = n - k;
            let mut term = Matrix::identity(n);
            for _ in 0..power {
                term = term.mul(&m4).unwrap();
            }
            pa = pa.add(&term.scale(*coef)).unwrap();
            let _ = ak;
        }
        for v in pa.data.iter() {
            assert!(v.abs() < 1e-9, "Кэли–Гамильтон нарушен: {v}");
        }
    }

    #[test]
    fn eigenvalues_2x2() {
        // [[1,2],[3,4]] → (5±√33)/2 ≈ 5.37228, −0.37228
        let a = m22(1.0, 2.0, 3.0, 4.0);
        let ev = a.eigenvalues().unwrap();
        assert_eq!(ev.len(), 2);
        let s33 = 33.0f64.sqrt();
        assert!(close(ev[0].re, (5.0 - s33) / 2.0, 1e-9), "{:?}", ev);
        assert!(close(ev[1].re, (5.0 + s33) / 2.0, 1e-9), "{:?}", ev);
        assert!(ev.iter().all(|e| e.im.abs() < 1e-9));
    }

    #[test]
    fn eigenvalues_rotation_pure_imaginary() {
        // вращение 90°: собственные значения ±i (регрессия прошлой сессии)
        let r = Matrix::rot2(std::f64::consts::FRAC_PI_2);
        let ev = r.eigenvalues().unwrap();
        assert_eq!(ev.len(), 2);
        assert!(ev[0].im.abs() > 0.99 && ev[1].im.abs() > 0.99, "{:?}", ev);
        assert!((ev[0].im + ev[1].im).abs() < 1e-9);
        assert!(ev.iter().all(|e| e.re.abs() < 1e-9), "{:?}", ev);
    }

    #[test]
    fn eigenvalues_diagonal() {
        let d = Matrix::from_rows(&[vec![2.0, 0.0], vec![0.0, -7.0]]).unwrap();
        let ev = d.eigenvalues().unwrap();
        let rs: Vec<f64> = ev.iter().map(|e| e.re).collect();
        assert!(rs.iter().any(|r| close(*r, 2.0, 1e-9)));
        assert!(rs.iter().any(|r| close(*r, -7.0, 1e-9)));
    }

    #[test]
    fn lie_rotations() {
        // rot_x(π/2) поворачивает ось Y → Z
        let r = Matrix::rot_x(std::f64::consts::FRAC_PI_2);
        let y = Matrix::from_rows(&[vec![0.0], vec![1.0], vec![0.0]]).unwrap();
        let z = r.mul(&y).unwrap();
        assert!(close(z.get(0, 0), 0.0, 1e-12));
        assert!(close(z.get(1, 0), 0.0, 1e-12));
        assert!(close(z.get(2, 0), 1.0, 1e-12));

        // rot_z поворачивает X → Y
        let r = Matrix::rot_z(std::f64::consts::FRAC_PI_2);
        let x = Matrix::from_rows(&[vec![1.0], vec![0.0], vec![0.0]]).unwrap();
        let y2 = r.mul(&x).unwrap();
        assert!(close(y2.get(1, 0), 1.0, 1e-12));

        // rot_y поворачивает Z → X
        let r = Matrix::rot_y(std::f64::consts::FRAC_PI_2);
        let zv = Matrix::from_rows(&[vec![0.0], vec![0.0], vec![1.0]]).unwrap();
        let x2 = r.mul(&zv).unwrap();
        assert!(close(x2.get(0, 0), 1.0, 1e-12));

        // генератор: expm(J·θ) = rotation
        let g = Matrix::so_generator(3, 0, 1).unwrap();
        let e = g.scale(std::f64::consts::FRAC_PI_2).expm().unwrap();
        let r = Matrix::rotation(3, 0, 1, std::f64::consts::FRAC_PI_2).unwrap();
        for (a, b) in e.data.iter().zip(r.data.iter()) {
            assert!(close(*a, *b, 1e-12));
        }
        assert!(Matrix::so_generator(3, 1, 1).is_err());
        assert!(Matrix::so_generator(3, 0, 3).is_err());
    }

    #[test]
    fn norm_inf() {
        let a = m22(1.0, -2.0, 3.0, 0.5);
        assert!((a.norm_inf() - 3.5).abs() < 1e-15);
    }
}
