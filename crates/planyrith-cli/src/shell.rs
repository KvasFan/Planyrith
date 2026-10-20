//! Splits batch lines (docs/08 §2 `batch`): `;` separates commands, `#`
//! starts a comment, words are separated by whitespace and may be quoted with
//! `'…'` (literal) or `"…"` (backslash escapes `\"` and `\\`).

/// Splits one line into commands, each a list of words. Errors on an
/// unterminated quote.
pub fn split_line(line: &str) -> Result<Vec<Vec<String>>, String> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => word.push(ch),
                        None => return Err("unterminated ' quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(ch) => word.push(ch),
                            None => return Err("unterminated \" quote".into()),
                        },
                        Some(ch) => word.push(ch),
                        None => return Err("unterminated \" quote".into()),
                    }
                }
            }
            '#' if !in_word => break,
            ';' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_commands_words_quotes_and_comments() {
        let cmds = split_line(
            r#"select 'face & plane:xy@0' ; move --targets selection --delta 0,0,700 # lift"#,
        )
        .unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], vec!["select", "face & plane:xy@0"]);
        assert_eq!(
            cmds[1],
            vec!["move", "--targets", "selection", "--delta", "0,0,700"]
        );
        assert_eq!(
            split_line("# only a comment").unwrap(),
            Vec::<Vec<String>>::new()
        );
        assert_eq!(
            split_line(r#"name:"Oak \"floor\"" a;b"#).unwrap(),
            vec![
                vec![r#"name:Oak "floor""#.to_string(), "a".into()],
                vec!["b".to_string()]
            ]
        );
        assert!(split_line("'open").is_err());
    }
}
