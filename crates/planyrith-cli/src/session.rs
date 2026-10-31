//! A document session (docs/08 §1): open or create, run commands through the
//! engine, save once.

use std::cell::RefCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use planyrith_core::commands::{CommandRegistry, Engine, Event};
use planyrith_core::diagnostics::Warning;
use planyrith_core::document::{Document, EditContext, EntityPath};
use planyrith_core::io::NoProgress;
use planyrith_core::io::autosave::Recovered;
use planyrith_core::io::native::{HistoryEntry, LoadedDocument, WriteOptions, write_document};
use planyrith_core::io::storage::{NativeFs, Storage, StorageKey};
use planyrith_core::units::LengthUnit;

use crate::{CliError, Global};

/// An open document with its engine.
pub struct Session {
    /// Where it was read from (`None` = new, or standard input).
    pub path: Option<PathBuf>,
    /// First save of an externally opened model defaults to a native copy.
    pub native_copy_pending: bool,
    /// The engine owning the document.
    pub engine: Engine,
    /// Container metadata carried over on save.
    loaded: Option<LoadedDocument>,
    /// Journal rows (loaded + this session).
    pub history: Vec<HistoryEntry>,
    /// Warnings emitted by commands.
    pub warnings: Rc<RefCell<Vec<Warning>>>,
    /// Size of the file read, in bytes.
    pub file_size: Option<u64>,
}

impl Session {
    /// Opens `global.file`; a missing file gives a new document when
    /// `create` is set (else `E1403`).
    pub fn open(global: &Global, create: bool) -> Result<Session, CliError> {
        let registry = Arc::new(crate::extensions::registry()?);
        let (doc, loaded, path, file_size) = match &global.file {
            None if create => (Document::new(), None, None, None),
            None => return Err(CliError::usage("this verb needs a document: -f <path>")),
            Some(p) if p.as_os_str() == "-" => {
                let mut bytes = Vec::new();
                std::io::stdin().read_to_end(&mut bytes).map_err(|e| {
                    CliError::io(planyrith_core::io::IoError::storage(
                        "standard input",
                        e.to_string(),
                    ))
                })?;
                let size = bytes.len() as u64;
                let mut loaded =
                    planyrith_core::io::model_file::read(&bytes, "-", &NativeFs::new())
                        .map_err(CliError::io)?;
                let doc = std::mem::take(&mut loaded.document);
                (doc, Some(loaded), None, Some(size))
            }
            Some(p) if !p.exists() => {
                if !create {
                    return Err(CliError::io(planyrith_core::io::IoError::NotFound(
                        p.display().to_string(),
                    )));
                }
                (Document::new(), None, Some(p.clone()), None)
            }
            Some(p) => {
                let bytes = NativeFs::new().read(&key(p)).map_err(CliError::io)?;
                let size = bytes.len() as u64;
                let mut loaded = planyrith_core::io::model_file::read(
                    &bytes,
                    &p.to_string_lossy(),
                    &NativeFs::new(),
                )
                .map_err(CliError::io)?;
                let doc = std::mem::take(&mut loaded.document);
                (doc, Some(loaded), Some(p.clone()), Some(size))
            }
        };
        let mut session = Session::build(registry, doc, loaded, path, file_size);
        session.apply_view_options(global)?;
        Ok(session)
    }

    /// A session on a document that exists only in memory so far (`new`);
    /// `save` writes it to `path` (or `-o`).
    pub fn create(doc: Document, path: Option<PathBuf>) -> Result<Session, CliError> {
        let registry = Arc::new(crate::extensions::registry()?);
        Ok(Session::build(registry, doc, None, path, None))
    }

    /// A session on a recovered document (docs/11 §2) that saves to `path`.
    pub fn recovered(recovered: Recovered, path: Option<PathBuf>) -> Result<Session, CliError> {
        let doc = recovered.engine.document().clone();
        let mut session = Session::build(
            Arc::new(crate::extensions::registry()?),
            doc,
            recovered.loaded,
            path,
            None,
        );
        session.history = recovered.history;
        if let Some(last) = session.history.last() {
            session.engine.set_last_created(last.created.clone());
        }
        Ok(session)
    }

    fn build(
        registry: Arc<CommandRegistry>,
        doc: Document,
        loaded: Option<LoadedDocument>,
        path: Option<PathBuf>,
        file_size: Option<u64>,
    ) -> Session {
        let history = loaded
            .as_ref()
            .map(|l| l.history.clone())
            .unwrap_or_default();
        let mut engine = Engine::new(doc, registry);
        // File commands (textures, libraries) read and write next to the
        // working directory (docs/01 §3.1 rule 2).
        engine.set_storage(Arc::new(NativeFs::new()));
        if let Some(last) = history.last() {
            engine.set_last_created(last.created.clone());
        }
        let warnings = Rc::new(RefCell::new(
            loaded
                .as_ref()
                .map(|l| l.warnings.clone())
                .unwrap_or_default(),
        ));
        let sink = Rc::clone(&warnings);
        engine.events_mut().subscribe(Box::new(move |event| {
            if let Event::Warning(w) = event {
                sink.borrow_mut().push(w.clone());
            }
        }));
        Session {
            native_copy_pending: loaded.is_some()
                && path
                    .as_ref()
                    .is_some_and(|p| planyrith_core::io::model_file::is_skp(&p.to_string_lossy())),
            path,
            engine,
            loaded,
            history,
            warnings,
            file_size,
        }
    }

    /// `--context` and `--select`.
    fn apply_view_options(&mut self, global: &Global) -> Result<(), CliError> {
        if let Some(text) = &global.context {
            let path = EntityPath::parse(text, self.engine.document())
                .map_err(|e| CliError::usage(format!("--context: {e}")))?;
            let context = EditContext {
                path: path.instances.child(path.target),
            };
            self.engine
                .set_context(context)
                .map_err(CliError::from_command)?;
        }
        if let Some(query) = &global.select {
            self.engine
                .execute_erased("select", serde_json::json!({ "query": query }))
                .map_err(CliError::from_command)?;
        }
        Ok(())
    }

    /// The unit of bare numbers.
    pub fn unit(&self, global: &Global) -> Result<LengthUnit, CliError> {
        match &global.units {
            Some(u) => u
                .parse::<LengthUnit>()
                .map_err(|e| CliError::usage(format!("--units: {e}"))),
            None => Ok(self.engine.document().info.display_units),
        }
    }

    /// Dispatches a command, enforcing the script's host-side time budget.
    pub fn execute(
        &mut self,
        name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, planyrith_core::commands::CommandError> {
        if name != "script-eval" {
            return self.engine.execute_erased(name, params);
        }
        let p: planyrith_core::scripting::ScriptParams =
            serde_json::from_value(params).map_err(|e| {
                planyrith_core::commands::CommandError::invalid_param("params", e.to_string())
            })?;
        let start = std::time::Instant::now();
        let control = planyrith_core::scripting::ScriptControl {
            interrupted: Some(Arc::new(move || start.elapsed().as_secs() >= 60)),
            ..Default::default()
        };
        let output = planyrith_core::scripting::eval(
            &mut self.engine,
            &p.source,
            &p.args,
            Default::default(),
            control,
        )?;
        serde_json::to_value(output)
            .map_err(|e| planyrith_core::commands::CommandError::Internal(e.to_string()))
    }

    /// Records a journal row for an executed command.
    pub fn journal(
        &mut self,
        command: &str,
        params: &serde_json::Value,
        group: Option<&str>,
        global: &Global,
    ) {
        self.history.push(HistoryEntry {
            command: command.to_string(),
            params: params.clone(),
            group: group.map(str::to_string),
            timestamp: (!global.deterministic).then(now_rfc3339),
            created: self.engine.last_created().to_vec(),
        });
    }

    /// Saves unless `--no-save`/`--dry-run`; returns the path written.
    pub fn save(&self, global: &Global) -> Result<Option<String>, CliError> {
        if global.no_save || global.dry_run {
            return Ok(None);
        }
        let target = global.out.clone().or_else(|| self.path.clone());
        let Some(mut target) = target else {
            return Err(CliError::usage(
                "nowhere to save: give -f <path> or -o <path> (or --no-save)",
            ));
        };
        if global.out.is_none() && self.native_copy_pending {
            target = PathBuf::from(planyrith_core::io::model_file::default_save_path(
                &target.to_string_lossy(),
            ));
            if NativeFs::new()
                .exists(&key(&target))
                .map_err(CliError::io)?
            {
                return Err(CliError::usage(format!(
                    "native working copy {} already exists; choose an explicit -o path",
                    target.display()
                )));
            }
        }
        let now = now_rfc3339();
        let mut opts = WriteOptions {
            deterministic: global.deterministic,
            created: now.clone(),
            modified: now,
            ..WriteOptions::default()
        };
        if let Some(loaded) = &self.loaded {
            opts = opts.carry_over(loaded);
        }
        let bytes = write_document(self.engine.document(), &self.history, &opts, &NoProgress)
            .map_err(CliError::io)?;
        if target.as_os_str() == "-" {
            crate::out::bytes(&bytes)?;
            return Ok(Some("-".into()));
        }
        let converted = planyrith_core::io::model_file::for_save(
            bytes,
            &target.to_string_lossy(),
            &NativeFs::new(),
        )
        .map_err(CliError::io)?;
        self.warnings.borrow_mut().extend(
            converted.warnings.into_iter().map(|message| {
                Warning::new(planyrith_core::diagnostics::WarningCode::W2004, message)
            }),
        );
        NativeFs::new()
            .write(&key(&target), &converted.bytes)
            .map_err(CliError::io)?;
        Ok(Some(target.display().to_string()))
    }
}

/// Storage key of a filesystem path.
pub fn key(path: &Path) -> StorageKey {
    StorageKey::new(path.to_string_lossy().to_string())
}

/// Current UTC time as RFC 3339 (the core has no clock; the CLI supplies it).
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_746), (2026, 10, 20));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }
}
