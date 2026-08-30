//! # Sandbox Judge Probe — аудиторский пробник Terminal Gateway (v0.22.x)
//!
//! Читает строки из stdin, печатает вердикт sandbox для каждой:
//! `ALLOW` / `CONFIRM <причина>` / `BLOCK <причина>` / `PARSE-ERR <…>`.
//!
//! **Ничего не исполняет** — чистый вызов классификатора
//! (`sandbox::judge_pipeline`). Предназначен для security-аудитов:
//!   printf "rm -rf /\nenv rm -rf /usr\n" | cargo run --example sandbox_probe
//!
//! Все сегменты считаются хостовыми (консервативно): сегменты движка
//! безопасны by construction, но для аудита полезнее видеть вердикт по
//! максимуму поверхности.

use poler_engine::gateway::pipeline::parse_line;
use poler_engine::gateway::sandbox::{judge_pipeline, Policy};
use std::io::BufRead;

fn main() {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let verdict = match parse_line(trimmed) {
            Err(e) => format!("PARSE-ERR\t{e}"),
            Ok(p) => {
                let host: Vec<usize> = (0..p.segments.len()).collect();
                match judge_pipeline(&p, &host) {
                    Policy::Allow => "ALLOW\t—".to_string(),
                    Policy::Confirm(w) => format!("CONFIRM\t{w}"),
                    Policy::Block(w) => format!("BLOCK\t{w}"),
                }
            }
        };
        // команда \t вердикт — парсится скриптом аудита
        let cmd = trimmed.replace(['\t', '\n'], " ");
        println!("{cmd}\t{verdict}");
    }
}
