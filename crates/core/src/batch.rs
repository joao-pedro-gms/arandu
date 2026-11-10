//! Renomeação em lote: expansão de padrões de nome.
//!
//! A expansão é pura (não toca no disco), o que permite pré-visualizar o
//! resultado a cada edição do padrão e testá-la sem fixtures.

/// Expande um padrão de renomeação para um arquivo.
///
/// Placeholders reconhecidos:
/// - `{name}`: nome sem a extensão;
/// - `{ext}`: extensão **com** o ponto (ex.: `.txt`), vazia quando ausente;
/// - `{n}`: contador 1-based com zeros à esquerda até a largura do total
///   (mínimo de dois dígitos; ex.: total `3` → `01`, `02`, `03`; total `120` →
///   `001`, `002`, …);
///
/// Placeholders desconhecidos permanecem literais.
pub fn apply_pattern(pattern: &str, name: &str, ext: &str, index: usize, total: usize) -> String {
    let width = total.max(1).to_string().len().max(2);
    let counter = format!("{index:0width$}");
    pattern
        .replace("{name}", name)
        .replace("{ext}", ext)
        .replace("{n}", &counter)
}

#[cfg(test)]
mod tests {
    use super::apply_pattern;

    #[test]
    fn replaces_name_placeholder() {
        assert_eq!(apply_pattern("{name}", "foto", ".png", 1, 1), "foto");
    }

    #[test]
    fn replaces_extension_with_dot() {
        assert_eq!(
            apply_pattern("{name}-backup{ext}", "doc", ".txt", 1, 1),
            "doc-backup.txt"
        );
    }

    #[test]
    fn empty_extension_leaves_nothing() {
        assert_eq!(apply_pattern("{name}{ext}", "README", "", 1, 1), "README");
    }

    #[test]
    fn counter_pads_to_total_width() {
        assert_eq!(
            apply_pattern("foto-{n}{ext}", "foto", ".jpg", 1, 3),
            "foto-01.jpg"
        );
        assert_eq!(
            apply_pattern("foto-{n}{ext}", "foto", ".jpg", 3, 3),
            "foto-03.jpg"
        );
        assert_eq!(apply_pattern("{n}", "x", "", 7, 120), "007");
    }

    #[test]
    fn unknown_placeholder_is_kept_literal() {
        assert_eq!(
            apply_pattern("{name}-{desconhecido}", "a", "", 1, 1),
            "a-{desconhecido}"
        );
    }

    #[test]
    fn combines_all_placeholders() {
        assert_eq!(
            apply_pattern("{n}-{name}{ext}", "relatorio", ".pdf", 12, 99),
            "12-relatorio.pdf"
        );
    }
}
