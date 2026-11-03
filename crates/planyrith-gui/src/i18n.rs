//! Implements docs/07 §12 (i18n): the interface's language. English is the
//! source: code writes `t("Save As…")`, and the Fluent files of other
//! languages give each English text a translation under an identifier made
//! from the text ([`id`]). A text without one shows in English
//! (docs/07 Decisions, 2026-11-02).

use std::cell::Cell;
use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use planyrith_core::commands::CommandError;
use planyrith_core::geom::GeometryError;
use planyrith_core::io::IoError;
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

/// A command error in the interface language: each kind's frame is
/// translated; details written by the core (reasons, names) stay as they are.
pub fn command_error(e: &CommandError) -> String {
    let s = |v: &str| FluentValue::from(v.to_owned());
    match e {
        CommandError::InvalidParam { name, reason } => t_args(
            "invalid parameter `{ $name }`: { $reason }",
            &[("name", s(name)), ("reason", s(reason))],
        ),
        CommandError::OutOfRange { name, reason } => t_args(
            "parameter `{ $name }` out of range: { $reason }",
            &[("name", s(name)), ("reason", s(reason))],
        ),
        CommandError::NotFound(entity) => t_args(
            "entity not found: { $entity }",
            &[("entity", s(&entity.to_string()))],
        ),
        CommandError::Ambiguous { query, matched } => t_args(
            "ambiguous selection: `{ $query }` matched { $matched } entities",
            &[
                ("query", s(&query.to_string())),
                ("matched", s(&matched.to_string())),
            ],
        ),
        CommandError::NotApplicable(reason) => t_args(
            "operation not applicable: { $reason }",
            &[("reason", s(reason))],
        ),
        CommandError::Geometry(e) => geometry_error(e),
        CommandError::Cancelled => t("cancelled"),
        CommandError::Io(e) => io_error(e),
        CommandError::Script { line, col, message } => t_args(
            "script error at { $line }:{ $col }: { $message }",
            &[
                ("line", s(&line.to_string())),
                ("col", s(&col.to_string())),
                ("message", s(message)),
            ],
        ),
        CommandError::Internal(reason) => {
            t_args("internal error: { $reason }", &[("reason", s(reason))])
        }
    }
}

/// A geometry error in the interface language (see [`command_error`]).
pub fn geometry_error(e: &GeometryError) -> String {
    let r = |v: &str| [("reason", FluentValue::from(v.to_owned()))];
    match e {
        GeometryError::Degenerate(v) => t_args("degenerate geometry: { $reason }", &r(v)),
        GeometryError::NonRobust(v) => t_args("non-robust geometry: { $reason }", &r(v)),
        GeometryError::OutOfRange => t("coordinate out of the supported range (±1e9 mm)"),
        GeometryError::NotFound(v) => t_args("mesh element not found: { $reason }", &r(v)),
        GeometryError::NotApplicable(v) => t_args("operation not applicable: { $reason }", &r(v)),
        GeometryError::Invariant(v) => t_args("mesh invariant violated: { $reason }", &r(v)),
    }
}

/// A file or storage error in the interface language (see [`command_error`]).
pub fn io_error(e: &IoError) -> String {
    let s = |v: &str| FluentValue::from(v.to_owned());
    match e {
        IoError::UnsupportedVersion { found, max } => t_args(
            "document format version { $found } is newer than the supported { $max }",
            &[
                ("found", s(&found.to_string())),
                ("max", s(&max.to_string())),
            ],
        ),
        IoError::UnsupportedFormat(r) => {
            t_args("unsupported format: { $reason }", &[("reason", s(r))])
        }
        IoError::Corrupt { part, reason } => t_args(
            "corrupt container: { $part }: { $reason }",
            &[("part", s(part)), ("reason", s(reason))],
        ),
        IoError::Conversion { format, reason } => t_args(
            "{ $format } conversion failed: { $reason }",
            &[("format", s(format)), ("reason", s(reason))],
        ),
        IoError::Storage { key, reason } => t_args(
            "storage error on `{ $key }`: { $reason }",
            &[("key", s(key)), ("reason", s(reason))],
        ),
        IoError::NotFound(r) => t_args("not found: { $reason }", &[("reason", s(r))]),
        IoError::ReadOnly(r) => t_args("read-only: { $reason }", &[("reason", s(r))]),
        IoError::LimitExceeded(r) => t_args("limit exceeded: { $reason }", &[("reason", s(r))]),
        IoError::Cancelled => t("cancelled"),
    }
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
        // Every source file; this one only up to its tests, which hold
        // sample texts.
        #[allow(clippy::unwrap_used)]
        fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    out.push(if path.ends_with("src/i18n.rs") {
                        text.split("#[cfg(test)]").next().unwrap_or("").to_string()
                    } else {
                        text
                    });
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let sources: Vec<&str> = files.iter().map(String::as_str).collect();
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
        // Error frames are translated; the core's details are kept.
        let e = CommandError::InvalidParam {
            name: "distance".into(),
            reason: "must be positive".into(),
        };
        assert_eq!(
            command_error(&e),
            "parametro `distance` non valido: must be positive"
        );
        set_language(Language::English);
        assert_eq!(command_error(&e), e.to_string());
        let io = IoError::Corrupt {
            part: "manifest.json".into(),
            reason: "bad".into(),
        };
        assert_eq!(command_error(&CommandError::Io(io.clone())), io_error(&io));
        assert_eq!(io_error(&io), io.to_string());
    }
}
