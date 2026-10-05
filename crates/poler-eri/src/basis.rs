//! NWChem basis set parser and built-in basis definitions.
//!
//! This module provides:
//! - A parser for NWChem-format basis set files (the de facto standard format
//!   used by most quantum chemistry programs)
//! - Built-in STO-3G basis definitions for H and Li (for testing)
//! - Element number lookup from periodic table symbols
//!
//! # NWChem Format
//!
//! A typical NWChem basis set file looks like:
//!
//! ```text
//! BASIS SET:
//! H S
//!      3
//!        3.449  0.154
//!        0.627  0.535
//!        0.168  0.444
//! C SP
//!      6
//!       71.62  0.154  0.000
//!       13.04  0.535  0.000
//!        3.53  0.444  0.444
//!        2.94  0.000  0.154
//!        0.68  0.000  0.535
//!        0.22  0.000  0.444
//! END
//! ```
//!
//! Each section specifies an element, shell type (S/P/D/F/G/H/SP), number of
//! primitives, then alpha-coefficient pairs per line. SP shells produce both
//! an s-shell and a p-shell from the same exponents but different coefficients.

use crate::types::{Shell, BasisSet};
use crate::normalization::normalize_shell;
use std::fs;


/// Parse an NWChem-format basis set file into a [`BasisSet`].
///
/// The parser handles the most common features:
/// - Shell types: S, P, D, F, G, H, SP
/// - SP shells (split into separate s and p shells)
/// - Comment lines starting with `!`
/// - Variable number of primitives per shell
///
/// # Arguments
///
/// * `content` — The full text content of the basis set file
///
/// # Returns
///
/// `Ok(BasisSet)` on success, or `Err(String)` with a description of the
/// parse error. Note: the returned `BasisSet` may have an empty `atoms` vector
/// because atom positions are not specified in basis set files — they must be
/// added separately from molecular geometry data.
///
/// # Example
///
/// ```rust
/// use poler_eri::basis::parse_nwchem;
///
/// let content = "H S\n3\n3.449 0.154\n0.627 0.535\n0.168 0.444\n";
/// let basis = parse_nwchem(content).unwrap();
/// assert!(!basis.shells.is_empty());
/// ```
pub fn parse_nwchem(content: &str) -> Result<BasisSet, String> {
    let atoms = Vec::new();
    let mut shells = Vec::new();

    let mut _current_element: Option<String> = None;
    let mut current_l: usize = 0;
    let mut alphas: Vec<f64> = Vec::new();
    let mut coeffs: Vec<f64> = Vec::new();
    let mut sp_coeffs: Vec<f64> = Vec::new();
    let mut reading = false;
    let mut sp_pending = false;

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines, comments, and header/footer markers
        if line.is_empty() || line.starts_with('!') || line.starts_with("BASIS") || line.starts_with("END") {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();

        // Check for element header: "H S" or "C SP" etc.
        if parts.len() >= 2 && parts[0].chars().all(|c| c.is_alphabetic()) {
            // Save previous shell before starting a new one
            if reading && !alphas.is_empty() {
                let mut shell = Shell::new(current_l, atoms.len().saturating_sub(1), alphas.clone(), coeffs.clone());
                normalize_shell(&mut shell);
                shells.push(shell);

                // If this was an SP shell, also create the p-shell
                if sp_pending && !sp_coeffs.is_empty() {
                    let mut p_shell = Shell::new(1, atoms.len().saturating_sub(1), alphas.clone(), sp_coeffs.clone());
                    normalize_shell(&mut p_shell);
                    shells.push(p_shell);
                }

                alphas.clear();
                coeffs.clear();
                sp_coeffs.clear();
            }

            _current_element = Some(parts[0].to_string());
            let shell_type = parts[1];
            current_l = match shell_type {
                "S"  => 0,
                "SP" => 0, // SP creates both s and p shells
                "P"  => 1,
                "D"  => 2,
                "F"  => 3,
                "G"  => 4,
                "H"  => 5,
                _ => continue,
            };
            sp_pending = shell_type == "SP";
            reading = true;
            continue;
        }

        // Parse primitive: alpha coeff [sp_coeff]
        if reading && parts.len() >= 2 {
            if let Ok(alpha) = parts[0].parse::<f64>() {
                if let Ok(coeff) = parts[1].parse::<f64>() {
                    alphas.push(alpha);
                    coeffs.push(coeff);

                    // SP shells have a second coefficient for the p-component
                    if sp_pending {
                        if let Some(sp_coeff) = parts.get(2).and_then(|s| s.parse::<f64>().ok()) {
                            sp_coeffs.push(sp_coeff);
                        } else {
                            // If no SP coefficient given, reuse the s-coefficient
                            sp_coeffs.push(coeff);
                        }
                    }
                }
            }
        }
    }

    // Save the last shell
    if reading && !alphas.is_empty() {
        let mut shell = Shell::new(current_l, atoms.len(), alphas.clone(), coeffs.clone());
        normalize_shell(&mut shell);
        shells.push(shell);

        if sp_pending && !sp_coeffs.is_empty() {
            let mut p_shell = Shell::new(1, atoms.len(), alphas.clone(), sp_coeffs.clone());
            normalize_shell(&mut p_shell);
            shells.push(p_shell);
        }
    }

    Ok(BasisSet { atoms, shells })
}

/// Read and parse a basis set file from disk.
///
/// Convenience wrapper around [`parse_nwchem`] that reads the file first.
pub fn read_basis_file(path: &str) -> Result<BasisSet, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read basis file '{}': {}", path, e))?;
    parse_nwchem(&content)
}

/// Look up an element's atomic number from its chemical symbol.
///
/// Supports elements 1–18 (H through Ar). Returns 0 for unknown symbols.
///
/// # Example
///
/// ```
/// use poler_eri::basis::element_number;
/// assert_eq!(element_number("H"), 1);
/// assert_eq!(element_number("Li"), 3);
/// assert_eq!(element_number("C"), 6);
/// ```
pub fn element_number(symbol: &str) -> usize {
    match symbol {
        "H" => 1, "He" => 2,
        "Li" => 3, "Be" => 4, "B" => 5, "C" => 6,
        "N" => 7, "O" => 8, "F" => 9, "Ne" => 10,
        "Na" => 11, "Mg" => 12, "Al" => 13, "Si" => 14,
        "P" => 15, "S" => 16, "Cl" => 17, "Ar" => 18,
        _ => 0,
    }
}

/// Build a minimal STO-3G basis for hydrogen.
///
/// Returns a single s-shell with 3 primitives and the standard STO-3G
/// coefficients. The shell is already normalized.
///
/// | Primitive | Alpha   | Coefficient |
/// |-----------|---------|-------------|
/// | 1         | 3.449   | 0.154       |
/// | 2         | 0.627   | 0.535       |
/// | 3         | 0.168   | 0.444       |
pub fn sto3g_h() -> Shell {
    let mut s = Shell::new(0, 0,
        vec![3.449, 0.627, 0.168],
        vec![0.154, 0.535, 0.444]);
    normalize_shell(&mut s);
    s
}

/// Build a minimal STO-3G basis for lithium.
///
/// Returns two shells:
/// 1. An s-shell (1s) with exponents [16.119, 2.936, 0.795]
/// 2. An SP shell (2s/2p) with exponents [0.636, 0.148, 0.048]
///    — split into a separate s-shell and p-shell
///
/// All shells are normalized.
pub fn sto3g_li() -> Vec<Shell> {
    // 1s shell
    let mut s1 = Shell::new(0, 0,
        vec![16.119, 2.936, 0.795],
        vec![0.064, 0.368, 0.672]);
    normalize_shell(&mut s1);

    // 2s from SP split
    let mut s2 = Shell::new(0, 0,
        vec![0.636, 0.148, 0.048],
        vec![0.064, 0.368, 0.672]);
    normalize_shell(&mut s2);

    // 2p from SP split
    let mut p = Shell::new(1, 0,
        vec![0.636, 0.148, 0.048],
        vec![0.064, 0.368, 0.672]);
    normalize_shell(&mut p);

    vec![s1, s2, p]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_element_number() {
        assert_eq!(element_number("H"), 1);
        assert_eq!(element_number("C"), 6);
        assert_eq!(element_number("Li"), 3);
        assert_eq!(element_number("Xx"), 0);
    }

    #[test]
    fn test_sto3g_h() {
        let shell = sto3g_h();
        assert_eq!(shell.l, 0);
        assert_eq!(shell.n_prim(), 3);
        // After normalization, norm_coeffs should differ from raw coeffs
        assert_ne!(shell.norm_coeffs[0], shell.coeffs[0]);
    }

    #[test]
    fn test_sto3g_li() {
        let shells = sto3g_li();
        assert_eq!(shells.len(), 3); // 1s, 2s, 2p
        assert_eq!(shells[0].l, 0); // 1s
        assert_eq!(shells[1].l, 0); // 2s
        assert_eq!(shells[2].l, 1); // 2p
    }

    #[test]
    fn test_parse_nwchem_h() {
        let content = "H S\n3\n3.449 0.154\n0.627 0.535\n0.168 0.444\n";
        let basis = parse_nwchem(content).unwrap();
        assert_eq!(basis.shells.len(), 1);
        assert_eq!(basis.shells[0].l, 0);
        assert_eq!(basis.shells[0].n_prim(), 3);
    }

    #[test]
    fn test_parse_nwchem_sp() {
        let content = "Li SP\n6\n16.119 0.064 0.000\n2.936 0.368 0.000\n0.795 0.672 0.000\n0.636 0.000 0.064\n0.148 0.000 0.368\n0.048 0.000 0.672\n";
        let basis = parse_nwchem(content).unwrap();
        // SP should produce two shells: s and p
        assert_eq!(basis.shells.len(), 2);
        assert_eq!(basis.shells[0].l, 0); // s from SP
        assert_eq!(basis.shells[1].l, 1); // p from SP
    }

    #[test]
    fn test_parse_nwchem_comments() {
        let content = "! This is a comment\nH S\n! Another comment\n3\n3.449 0.154\n0.627 0.535\n0.168 0.444\n";
        let basis = parse_nwchem(content).unwrap();
        assert_eq!(basis.shells.len(), 1);
    }

    #[test]
    fn test_parse_nwchem_multi_element() {
        let content = "\
H S
3
3.449 0.154
0.627 0.535
0.168 0.444
Li S
3
16.119 0.064
2.936 0.368
0.795 0.672
Li P
3
0.636 0.064
0.148 0.368
0.048 0.672
";
        let basis = parse_nwchem(content).unwrap();
        assert_eq!(basis.shells.len(), 3);
    }
}
