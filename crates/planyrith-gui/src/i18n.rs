//! Implements docs/07 §12 (i18n): the interface's language. English is the
//! source: code writes `t("Save As…")`, and the Fluent files of other
//! languages give each English text a translation under an identifier made
//! from the text ([`id`]). A text without one shows in English
//! (docs/07 Decisions, 2026-11-02).

use std::cell::Cell;
use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use serde::{Deserialize, Serialize};

/// The interface languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Language {
    /// English, the source language.
    #[default]
    English,
    /// Italian.
    Italian,
}

/// The user's choice: a language, or the system's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LanguageChoice {
    /// The operating system's or browser's language when it is one of
    /// ours, else English.
    #[default]
    System,
    /// This language.
    Fixed(Language),
}

impl LanguageChoice {
    /// The language this choice means here.
    pub fn resolve(self) -> Language {
        match self {
            LanguageChoice::Fixed(l) => l,
            LanguageChoice::System => crate::platform::system_language()
                .filter(|tag| tag.to_ascii_lowercase().starts_with("it"))
                .map_or(Language::English, |_| Language::Italian),
        }
    }
}

const ITALIAN: &str = include_str!("../i18n/it.ftl");

thread_local! {
    // The interface draws on one thread; tests each get their own.
    static CURRENT: Cell<Language> = const { Cell::new(Language::English) };
}

/// Shows the interface in `language` from the next frame on (on this
/// thread, the one that draws it).
pub fn set_language(language: Language) {
    CURRENT.with(|c| c.set(language));
}

/// The interface's language.
pub fn language() -> Language {
    CURRENT.with(Cell::get)
}

fn italian() -> &'static FluentBundle<FluentResource> {
    static BUNDLE: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let lang: unic_langid::LanguageIdentifier = "it".parse().unwrap_or_default();
        let mut bundle = FluentBundle::new_concurrent(vec![lang]);
        // egui draws isolation marks as boxes.
        bundle.set_use_isolating(false);
        let resource = FluentResource::try_new(ITALIAN.to_string()).unwrap_or_else(|(r, _)| r);
        let _ = bundle.add_resource(resource);
        bundle
    })
}

/// The Fluent identifier of an English text: its letters and digits in
/// lower case, every other run as one `-`; `…` counts as `-more`, so "Save"
/// and "Save…" differ. Starts with `t-`, as identifiers start with a letter.
pub fn id(english: &str) -> String {
    let mut out = String::from("t-");
    let mut dash = false;
    for c in english.replace('…', " more").chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && out.len() > 2 {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Marks an English text for translation where it is defined (a menu
/// table, a tool's label) and returns it as it is: [`t`] translates it
/// where it is shown, and the coverage test finds it here.
pub const fn en(english: &'static str) -> &'static str {
    english
}

/// `english` in the interface's language.
pub fn t(english: &str) -> String {
    t_args(english, &[])
}

/// `english` with `{ $name }` placeholders filled from `args`, in the
/// interface's language.
pub fn t_args(english: &str, args: &[(&str, FluentValue<'_>)]) -> String {
    let fill = |pattern: &str| {
        let mut s = pattern.to_string();
        for (name, value) in args {
            let text = match value {
                FluentValue::String(v) => v.to_string(),
                FluentValue::Number(n) => n.as_string().to_string(),
                _ => String::new(),
            };
            s = s.replace(&format!("{{ ${name} }}"), &text);
        }
        s
    };
    if language() == Language::English {
        return fill(english);
    }
    let bundle = italian();
    let Some(pattern) = bundle.get_message(&id(english)).and_then(|m| m.value()) else {
        return fill(english);
    };
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, value.clone());
    }
    let mut errors = Vec::new();
    bundle
        .format_pattern(pattern, Some(&fluent_args), &mut errors)
        .into_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Every text the interface passes to `t` or `t_args` as a literal has an
    /// Italian translation, every translation is used, and no two texts share
    /// an identifier.
    #[test]
    fn every_interface_text_is_translated() {
        let sources: Vec<&str> = vec![
            include_str!("app.rs"),
            include_str!("resources.rs"),
            include_str!("viewport.rs"),
            include_str!("sheets.rs"),
            include_str!("extensions.rs"),
            include_str!("panels.rs"),
            include_str!("theme.rs"),
            include_str!("tools/mod.rs"),
            include_str!("tools/annotate.rs"),
            include_str!("tools/component.rs"),
            include_str!("tools/draw.rs"),
            include_str!("tools/edit.rs"),
            include_str!("tools/select.rs"),
            include_str!("tools/sketch.rs"),
            include_str!("tools/solid.rs"),
            include_str!("tools/texture.rs"),
        ];
        let mut used: std::collections::BTreeMap<String, String> = Default::default();
        let mut clashes: Vec<String> = Vec::new();
        for source in sources {
            for call in ["t(", "t_args(", "en("] {
                let mut rest = source;
                while let Some(at) = rest.find(call) {
                    let preceded = at > 0 && {
                        let c = rest.as_bytes()[at - 1];
                        c.is_ascii_alphanumeric() || c == b'_'
                    };
                    rest = &rest[at + call.len()..];
                    // rustfmt may put the text on the next line.
                    let text = rest.trim_start();
                    if preceded || !text.starts_with('"') {
                        continue;
                    }
                    // The literal's value: escapes as Rust reads them.
                    let mut english = String::new();
                    let mut chars = text[1..].chars();
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => break,
                            '\\' => match chars.next() {
                                Some('n') => english.push('\n'),
                                Some('t') => english.push('\t'),
                                Some(other) => english.push(other),
                                None => break,
                            },
                            c => english.push(c),
                        }
                    }
                    let key = id(&english);
                    if let Some(other) = used.get(&key)
                        && other != &english
                    {
                        clashes.push(format!("{key}: {other:?} and {english:?}"));
                    }
                    used.insert(key, english);
                }
            }
        }
        // Texts the core gives the interface: command names (Undo), step
        // prompts and value fields, and inference labels.
        let registry = planyrith_core::CommandRegistry::with_builtins();
        let mut core: Vec<String> = planyrith_core::inference::status_labels();
        for name in registry.names() {
            if let Some(command) = registry.get(name) {
                let meta = command.meta();
                core.push(meta.label.clone());
                for step in &meta.steps {
                    core.push(step.prompt.clone());
                    core.push(step.value_field.clone());
                }
            }
        }
        // A core text may differ from an interface text only in capitals or
        // punctuation ("Insert Component", "length"): one identifier, one
        // translation, as intended.
        for english in core.into_iter().filter(|e| !e.is_empty()) {
            used.entry(id(&english)).or_insert(english);
        }
        assert!(
            clashes.is_empty(),
            "texts sharing an identifier:\n{}",
            clashes.join("\n")
        );
        let resource = FluentResource::try_new(ITALIAN.to_string()).expect("it.ftl parses");
        let translated: std::collections::BTreeSet<String> = resource
            .entries()
            .filter_map(|e| match e {
                fluent_syntax::ast::Entry::Message(m) => Some(m.id.name.to_string()),
                _ => None,
            })
            .collect();
        let missing: Vec<String> = used
            .iter()
            .filter(|(k, _)| !translated.contains(*k))
            .map(|(k, v)| format!("{k} = {v}"))
            .collect();
        assert!(missing.is_empty(), "untranslated:\n{}", missing.join("\n"));
        let unused: Vec<&String> = translated
            .iter()
            .filter(|k| !used.contains_key(*k))
            .collect();
        assert!(
            unused.is_empty(),
            "translations of texts no longer used: {unused:?}"
        );
    }

    #[test]
    fn identifiers_and_fallback() {
        assert_eq!(id("Save As…"), "t-save-as-more");
        assert_eq!(id("Save"), "t-save");
        assert_eq!(id("  Back edges (dashed)"), "t-back-edges-dashed");
        set_language(Language::English);
        assert_eq!(t("Save"), "Save");
        assert_eq!(
            t_args("Placing { $name }", &[("name", "Crate".into())]),
            "Placing Crate"
        );
        set_language(Language::Italian);
        assert_eq!(t("Save As…"), "Salva con nome…");
        assert_eq!(
            t_args("Undo { $action } (Ctrl+Z)", &[("action", "Line".into())]),
            "Annulla Line (Ctrl+Z)"
        );
        // A text without a translation shows in English.
        assert_eq!(
            t("Not a text of the interface"),
            "Not a text of the interface"
        );
        set_language(Language::English);
    }
}
