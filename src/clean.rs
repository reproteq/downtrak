use unicode_normalization::UnicodeNormalization;

/// Igual que clean_filename() del script Python:
/// quita tildes, símbolos raros y caracteres que los coches no suelen leer.
pub fn clean_filename(title: &str) -> String {
    // 1. Quitar tildes y acentos (NFKD + descartar no-ASCII)
    let ascii: String = title.nfkd().filter(|c| c.is_ascii()).collect();
    // 2. Barra vertical de YouTube -> guion (la ｜ ancha ya se convierte en | con NFKD)
    let ascii = ascii.replace('|', "-");
    // 3. Solo letras, números, espacios, guiones, puntos y guion bajo
    let filtered: String = ascii
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace() || matches!(c, '.' | '-' | '_'))
        .collect();
    // 4. Limpiar espacios dobles
    let joined = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    // Extra: Windows no admite nombres que terminen en punto o espacio
    let out = joined.trim_end_matches(['.', ' ']).to_string();
    if out.is_empty() { "audio".to_string() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limpia() {
        assert_eq!(clean_filename("Canción ｜ Mañana  (Official Vídeo) 🎵"), "Cancion - Manana Official Video");
        assert_eq!(clean_filename("AC/DC | T.N.T."), "ACDC - T.N.T");
        assert_eq!(clean_filename("🎵🎵"), "audio");
    }
}
