//! Implements docs/04-document-model.md §11–§12 (selection, edit context,
//! entity paths) and docs/08-cli-specification.md §5 (the selection query
//! language shared by CLI, RPC and scripting).
//!
//! `Query` is the *source text* (what `Params` fields of type `Sel` carry);
//! `QueryExpr` is the parsed AST. Parsing is separate from evaluation so the
//! CLI can validate syntax without a document.
//!
//! Owner key: `document`.

use std::fmt;

use indexmap::IndexSet;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::ids::{EntityId, EntityKind, Pid};

/// An entity addressed by in-memory id or by persistent id (docs/04 §12
/// "EntityId-or-Pid").
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum EntityKey {
    /// In-memory id (valid within one container, not stable across undo).
    Id(EntityId),
    /// Persistent id.
    Pid(Pid),
}

impl fmt::Display for EntityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityKey::Id(id) => write!(f, "{id}"),
            EntityKey::Pid(pid) => write!(f, "{pid}"),
        }
    }
}

/// The chain of groups/instances from the model root to a container occurrence
/// (glossary "Instance path"). Empty = the model root.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct InstancePath {
    /// Each step is a group or instance in the previous container.
    pub steps: Vec<EntityKey>,
}

impl InstancePath {
    /// The model root.
    pub const fn root() -> Self {
        InstancePath { steps: Vec::new() }
    }

    /// `true` for the model root.
    pub fn is_root(&self) -> bool {
        self.steps.is_empty()
    }

    /// Nesting depth.
    pub fn depth(&self) -> usize {
        self.steps.len()
    }

    /// This path extended by one step.
    pub fn child(&self, step: EntityKey) -> Self {
        let mut steps = self.steps.clone();
        steps.push(step);
        InstancePath { steps }
    }

    /// The parent path (`None` at the root).
    pub fn parent(&self) -> Option<Self> {
        let (_, rest) = self.steps.split_last()?;
        Some(InstancePath {
            steps: rest.to_vec(),
        })
    }

    /// `true` when `self` is `other` or an ancestor of it.
    pub fn is_prefix_of(&self, other: &InstancePath) -> bool {
        other.steps.starts_with(&self.steps)
    }
}

impl fmt::Display for InstancePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("#Model")?;
        for step in &self.steps {
            write!(f, "/{step}")?;
        }
        Ok(())
    }
}

/// An entity *occurrence*: the instance path to its container plus the entity
/// (docs/04 §12). Operating on a path inside a definition affects all
/// instances of that definition.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct EntityPath {
    /// Container occurrence.
    pub instances: InstancePath,
    /// The entity inside that container.
    pub target: EntityKey,
}

impl EntityPath {
    /// An entity in the model root.
    pub fn in_model(target: EntityKey) -> Self {
        EntityPath {
            instances: InstancePath::root(),
            target,
        }
    }

    /// An entity in the given container occurrence.
    pub fn new(instances: InstancePath, target: EntityKey) -> Self {
        EntityPath { instances, target }
    }

    /// Parses the `#Model/Kitchen/Cabinet[2]/@42` syntax of docs/08 §5.3,
    /// resolving names against `doc` (names are not stored in paths).
    pub fn parse(text: &str, doc: &Document) -> Result<Self, QueryError> {
        todo!("docs/08 §5.3 path parsing")
    }

    /// Renders the path with names and instance indices (`#Model/Kitchen/Cabinet[2]/@42`).
    pub fn display_with(&self, doc: &Document) -> String {
        todo!("docs/08 §5.3 path display")
    }
}

impl fmt::Display for EntityPath {
    /// Key-based rendering without names (`#Model/@3/@42`); see
    /// [`EntityPath::display_with`] for the named form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.instances, self.target)
    }
}

/// Query source text (docs/08 §5), the `Sel` parameter type of docs/05 §1.
/// Validated by [`Query::parse`]; serialized as a plain string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Query(pub String);

impl Query {
    /// Wraps source text without validating it.
    pub fn new(source: impl Into<String>) -> Self {
        Query(source.into())
    }

    /// The source text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses the grammar of docs/08 §5 into an AST.
    pub fn parse(&self) -> Result<QueryExpr, QueryError> {
        parse_query(&self.0)
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Query {
    fn from(s: &str) -> Self {
        Query(s.to_string())
    }
}

/// The `EntityRef` parameter type of docs/05 §1 / docs/08 §2: `@pid`, `#path`
/// or a `{query}` that must match exactly one entity (`E1102` otherwise).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum EntityRef {
    /// `@pid`.
    Pid(Pid),
    /// `#Model/…`.
    Path(EntityPath),
    /// `{query}`.
    Query(Query),
}

impl fmt::Display for EntityRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityRef::Pid(pid) => write!(f, "{pid}"),
            EntityRef::Path(path) => write!(f, "{path}"),
            EntityRef::Query(q) => write!(f, "{{{q}}}"),
        }
    }
}

impl From<Pid> for EntityRef {
    fn from(pid: Pid) -> Self {
        EntityRef::Pid(pid)
    }
}

impl From<EntityPath> for EntityRef {
    fn from(path: EntityPath) -> Self {
        EntityRef::Path(path)
    }
}

/// How a selection change combines with the current selection (docs/05 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SelectMode {
    /// Replace the selection.
    #[default]
    Replace,
    /// Add to the selection.
    Add,
    /// Remove from the selection.
    Remove,
    /// Toggle membership.
    Toggle,
}

/// An ordered, deduplicated set of entity occurrences (docs/04 §11).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Selection {
    items: IndexSet<EntityPath>,
}

impl Selection {
    /// Empty selection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Selection of one occurrence.
    pub fn single(path: EntityPath) -> Self {
        let mut s = Self::new();
        s.insert(path);
        s
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Membership test.
    pub fn contains(&self, path: &EntityPath) -> bool {
        self.items.contains(path)
    }

    /// Adds an item; `true` if it was new.
    pub fn insert(&mut self, path: EntityPath) -> bool {
        self.items.insert(path)
    }

    /// Removes an item preserving order; `true` if it was present.
    pub fn remove(&mut self, path: &EntityPath) -> bool {
        self.items.shift_remove(path)
    }

    /// Toggles membership.
    pub fn toggle(&mut self, path: EntityPath) {
        if !self.items.shift_remove(&path) {
            self.items.insert(path);
        }
    }

    /// Clears everything.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Items in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &EntityPath> + '_ {
        self.items.iter()
    }

    /// The only item, if exactly one.
    pub fn single_item(&self) -> Option<&EntityPath> {
        if self.items.len() == 1 {
            self.items.first()
        } else {
            None
        }
    }

    /// Combines `other` into `self` per `mode`.
    pub fn apply(&mut self, mode: SelectMode, other: &Selection) {
        match mode {
            SelectMode::Replace => *self = other.clone(),
            SelectMode::Add => self.items.extend(other.items.iter().cloned()),
            SelectMode::Remove => {
                for p in &other.items {
                    self.items.shift_remove(p);
                }
            }
            SelectMode::Toggle => {
                for p in &other.items {
                    self.toggle(p.clone());
                }
            }
        }
    }
}

impl FromIterator<EntityPath> for Selection {
    fn from_iter<I: IntoIterator<Item = EntityPath>>(iter: I) -> Self {
        Selection {
            items: iter.into_iter().collect(),
        }
    }
}

/// The container currently open for editing (docs/04 §11); empty = model.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct EditContext {
    /// Path to the open container.
    pub path: InstancePath,
}

impl EditContext {
    /// The model root.
    pub fn model() -> Self {
        Self::default()
    }

    /// `true` when the model root is open.
    pub fn is_model(&self) -> bool {
        self.path.is_root()
    }
}

// ----- query language (docs/08 §5) ---------------------------------------------

/// Comparison operator for numeric atoms (`area>40m2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CmpOp {
    /// `>`
    Gt,
    /// `<`
    Lt,
    /// `>=`
    Ge,
    /// `<=`
    Le,
    /// `=`
    Eq,
}

/// Keys of `key:value` and `key op number` atoms (docs/08 §5 `key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryKey {
    /// `tag:` (alias `layer:`).
    Tag,
    /// `material:`
    Material,
    /// `name:` (glob `*`), groups/instances/definitions.
    Name,
    /// `definition:`
    Definition,
    /// `attr.<dict>.<key>`
    Attr {
        /// Dictionary.
        dict: String,
        /// Key.
        key: String,
    },
    /// `plane:xy@1200`
    Plane,
    /// `normal:z`
    Normal,
    /// `area` (mm²; CLI accepts `m2`).
    Area,
    /// `length` (mm).
    Length,
    /// `volume` (mm³).
    Volume,
    /// `id` (pid).
    Id,
}

/// Keyword atoms (docs/08 §5 `keyword`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QueryKeyword {
    /// Everything in the context.
    All,
    /// The current selection.
    Selection,
    /// Entities created by the previous command.
    Last,
    /// Containers that are solids.
    Solid,
    /// Hidden entities.
    Hidden,
    /// Visible entities.
    Visible,
    /// Locked groups/instances.
    Locked,
    /// Geometry connected to the matches of the inner query.
    Connected(Box<QueryExpr>),
    /// Edges bounding the faces matched by the inner query.
    BoundedBy(Box<QueryExpr>),
    /// Entities inside the bounds of the inner matches.
    Inside(Box<QueryExpr>),
    /// Children of the matched containers.
    Children(Box<QueryExpr>),
}

/// Leaf of the query AST (docs/08 §5 `atom`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QueryAtom {
    /// `edge`, `face`, …
    Kind(EntityKind),
    /// `@42`
    Pid(Pid),
    /// `#Model/…` (kept as text; resolved against the document at eval time).
    Path(String),
    /// `key:value`
    Match {
        /// Key.
        key: QueryKey,
        /// Raw value text (quotes removed).
        value: String,
    },
    /// `key op number`
    Compare {
        /// Key.
        key: QueryKey,
        /// Operator.
        op: CmpOp,
        /// Value converted to core units (mm, mm², mm³).
        value: f64,
    },
    /// Keyword.
    Keyword(QueryKeyword),
}

/// Parsed query (docs/08 §5 `query := or`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QueryExpr {
    /// `a | b | …`
    Or(Vec<QueryExpr>),
    /// `a & b`, also written `a b`.
    And(Vec<QueryExpr>),
    /// `!a`
    Not(Box<QueryExpr>),
    /// Leaf.
    Atom(QueryAtom),
}

/// Engine state a query may refer to (`selection`, `last`, the edit context).
#[derive(Debug, Clone, Copy)]
pub struct QueryEnv<'a> {
    /// Current selection.
    pub selection: &'a Selection,
    /// Pids created by the previous command.
    pub last: &'a [Pid],
    /// Current edit context (queries evaluate inside it).
    pub context: &'a EditContext,
}

/// Query errors (`E1001` for syntax, `E1101`/`E1102` surfaced by callers).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    /// Syntax error at a byte offset.
    #[error("query syntax error at {pos}: {message}")]
    Syntax {
        /// Byte offset in the source.
        pos: usize,
        /// Explanation.
        message: String,
    },
    /// Unknown kind word.
    #[error("unknown kind `{0}`")]
    UnknownKind(String),
    /// Unknown key.
    #[error("unknown key `{0}`")]
    UnknownKey(String),
    /// Unknown keyword.
    #[error("unknown keyword `{0}`")]
    UnknownKeyword(String),
    /// A number or unit could not be parsed.
    #[error("invalid number `{0}`")]
    InvalidNumber(String),
    /// A path segment did not resolve.
    #[error("path not found: {0}")]
    PathNotFound(String),
    /// Evaluation failure (e.g. field not applicable to the kind).
    #[error("query evaluation failed: {0}")]
    Eval(String),
}

/// Parses the grammar of docs/08 §5.
pub fn parse_query(source: &str) -> Result<QueryExpr, QueryError> {
    todo!("docs/08 §5 query parser")
}

impl QueryExpr {
    /// Evaluates against `doc` inside `env.context`, returning matches in a
    /// deterministic order (container order, then pid).
    pub fn eval(&self, doc: &Document, env: &QueryEnv<'_>) -> Result<Selection, QueryError> {
        todo!("docs/08 §5 query evaluation")
    }

    /// Canonical source text for the AST (`a & b`, quoted values).
    pub fn to_source(&self) -> String {
        todo!("docs/08 §5 query unparse")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(pid: u64) -> EntityPath {
        EntityPath::in_model(EntityKey::Pid(Pid(pid)))
    }

    #[test]
    fn selection_is_ordered_and_deduplicated() {
        let mut s = Selection::new();
        assert!(s.insert(path(2)));
        assert!(s.insert(path(1)));
        assert!(!s.insert(path(2)));
        assert_eq!(
            s.iter().cloned().collect::<Vec<_>>(),
            vec![path(2), path(1)]
        );
        s.toggle(path(2));
        assert_eq!(s.len(), 1);
        assert_eq!(s.single_item(), Some(&path(1)));
    }

    #[test]
    fn select_modes() {
        let mut s = Selection::from_iter([path(1), path(2)]);
        s.apply(SelectMode::Remove, &Selection::single(path(1)));
        assert!(!s.contains(&path(1)));
        s.apply(
            SelectMode::Toggle,
            &Selection::from_iter([path(2), path(3)]),
        );
        assert_eq!(s.iter().cloned().collect::<Vec<_>>(), vec![path(3)]);
        s.apply(SelectMode::Replace, &Selection::new());
        assert!(s.is_empty());
    }

    #[test]
    fn paths_display_and_nest() {
        let root = InstancePath::root();
        let kitchen = root.child(EntityKey::Pid(Pid(7)));
        assert!(root.is_prefix_of(&kitchen));
        assert_eq!(kitchen.parent(), Some(root.clone()));
        assert_eq!(
            EntityPath::new(kitchen, EntityKey::Pid(Pid(42))).to_string(),
            "#Model/@7/@42"
        );
        assert_eq!(
            EntityRef::Query(Query::from("face & last")).to_string(),
            "{face & last}"
        );
    }

    #[test]
    fn query_serializes_as_string() {
        let q = Query::from("tag:Walls");
        assert_eq!(serde_json::to_string(&q).unwrap(), "\"tag:Walls\"");
    }
}
