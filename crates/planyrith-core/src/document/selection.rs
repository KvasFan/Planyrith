//! Implements docs/04-document-model.md §11–§12 (selection, edit context,
//! entity paths) and docs/08-cli-specification.md §5 (the selection query
//! language shared by CLI, RPC and scripting) including §5.3 (paths).
//!
//! `Query` is the *source text* (what `Params` fields of type `Sel` carry);
//! `QueryExpr` is the parsed AST. Parsing is separate from evaluation so the
//! CLI can validate syntax without a document.
//!
//! Evaluation runs inside the edit context: the *domain* of a query is the
//! context container's entities plus every occurrence that a `children(…)`,
//! `connected(…)`, `bounded-by(…)`, `inside(…)` or `#path` term brings into
//! view; atoms are predicates over that domain and `!q` is the domain minus
//! `q`. Results come out in tree pre-order: container order, with the
//! contents of a group/instance right after it (docs/08 Decisions).
//!
//! Owner key: `document`.

#![warn(unused_variables, dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::rc::Rc;

use glam::{DAffine3, DVec3};
use indexmap::IndexSet;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::attributes::Value;
use crate::document::entities::{ComponentDefinition, Guide};
use crate::document::hierarchy::Entities;
use crate::document::materials::FrontOrBack;
use crate::document::{ContainerRef, Document};
use crate::geom::HalfEdgeMesh;
use crate::ids::{EntityId, EntityKind, MaterialId, Pid, TagId, VertexId};
use crate::math::{Aabb, Plane};
use crate::tolerance::Tolerances;
use crate::units::{LengthUnit, parse_length};

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
    ///
    /// Segments after `#Model` are `@pid` (or bare digits), a name, or a
    /// name with a 0-based index (`Cabinet[2]` = the third match in container
    /// order; no index = the first). Names match groups by their name and
    /// instances by their name, falling back to the definition name for
    /// unnamed instances; names with special characters are double-quoted.
    /// Every segment but the last must be a group or instance. Resolved
    /// entities are keyed by pid when they have one.
    ///
    /// Errors: [`QueryError::Syntax`] for malformed text,
    /// [`QueryError::PathNotFound`] when a segment does not resolve.
    pub fn parse(text: &str, doc: &Document) -> Result<Self, QueryError> {
        let segments = split_path(text)?;
        let mut container = ContainerRef::Model;
        let mut steps = Vec::new();
        for (i, segment) in segments.iter().enumerate() {
            let not_found = |why: &str| {
                QueryError::PathNotFound(format!("`{}` {why} in `{text}`", segment.raw))
            };
            let entities = doc
                .container(container)
                .ok_or_else(|| not_found("has no container"))?;
            let id = match &segment.kind {
                PathSegment::Pid(pid) => entities.id_of(*pid),
                PathSegment::Name { name, index } => find_named(doc, entities, name, *index),
            }
            .ok_or_else(|| not_found("does not exist"))?;
            let key = key_of(entities, id);
            if i + 1 == segments.len() {
                return Ok(EntityPath {
                    instances: InstancePath { steps },
                    target: key,
                });
            }
            let definition = entities
                .definition_of(id)
                .ok_or_else(|| not_found("is not a group or instance"))?;
            steps.push(key);
            if steps.len() > Entities::MAX_DEPTH {
                return Err(not_found("is deeper than the nesting limit"));
            }
            container = ContainerRef::Definition(definition);
        }
        Err(QueryError::PathNotFound(text.to_string()))
    }

    /// Renders the path with names and instance indices (`#Model/Kitchen/Cabinet[2]/@42`).
    ///
    /// Groups and instances with a (display) name are written by name, with
    /// `[n]` when an earlier sibling has the same name; other entities are
    /// written as `@pid`. Steps that do not resolve fall back to the key form
    /// of [`EntityKey`]'s `Display`. For pid-addressed paths
    /// `EntityPath::parse(&p.display_with(doc), doc) == Ok(p)`.
    pub fn display_with(&self, doc: &Document) -> String {
        let mut out = String::from("#Model");
        let mut container = Some(ContainerRef::Model);
        let keys = self
            .instances
            .steps
            .iter()
            .map(|k| (*k, true))
            .chain(std::iter::once((self.target, false)));
        for (key, is_step) in keys {
            out.push('/');
            let resolved = container
                .and_then(|c| doc.container(c))
                .and_then(|entities| entities.key_to_id(key).map(|id| (entities, id)));
            match resolved {
                Some((entities, id)) => {
                    out.push_str(&segment_text(doc, entities, id, key));
                    container = if is_step {
                        entities.definition_of(id).map(ContainerRef::Definition)
                    } else {
                        None
                    };
                }
                None => {
                    out.push_str(&key.to_string());
                    container = None;
                }
            }
        }
        out
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

impl CmpOp {
    /// The operator as written in queries.
    pub const fn symbol(self) -> &'static str {
        match self {
            CmpOp::Gt => ">",
            CmpOp::Lt => "<",
            CmpOp::Ge => ">=",
            CmpOp::Le => "<=",
            CmpOp::Eq => "=",
        }
    }

    /// `a op b` where values within `eps` count as equal: `>`/`<` need a
    /// difference beyond `eps`, `>=`/`<=`/`=` accept one within it.
    fn holds(self, a: f64, b: f64, eps: f64) -> bool {
        match self {
            CmpOp::Gt => a > b + eps,
            CmpOp::Lt => a < b - eps,
            CmpOp::Ge => a >= b - eps,
            CmpOp::Le => a <= b + eps,
            CmpOp::Eq => (a - b).abs() <= eps,
        }
    }
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

impl QueryKey {
    /// The key as written in canonical query text (`layer` is written `tag`).
    pub fn source_text(&self) -> String {
        match self {
            QueryKey::Tag => "tag".to_string(),
            QueryKey::Material => "material".to_string(),
            QueryKey::Name => "name".to_string(),
            QueryKey::Definition => "definition".to_string(),
            QueryKey::Attr { dict, key } => format!("attr.{dict}.{key}"),
            QueryKey::Plane => "plane".to_string(),
            QueryKey::Normal => "normal".to_string(),
            QueryKey::Area => "area".to_string(),
            QueryKey::Length => "length".to_string(),
            QueryKey::Volume => "volume".to_string(),
            QueryKey::Id => "id".to_string(),
        }
    }
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
///
/// Precedence from loosest to tightest: `|`, then `&` or juxtaposition
/// (whitespace), then `!`, then `( … )` and atoms. Parenthesized groups keep
/// their structure (`(a & b) & c` is an `And` inside an `And`) so that
/// [`QueryExpr::to_source`] round-trips.
pub fn parse_query(source: &str) -> Result<QueryExpr, QueryError> {
    let mut parser = Parser {
        src: source,
        pos: 0,
        depth: 0,
    };
    parser.skip_ws();
    if parser.at_end() {
        return Err(syntax(parser.pos, "empty query"));
    }
    let expr = parser.parse_or()?;
    parser.skip_ws();
    match parser.peek() {
        None => Ok(expr),
        Some(')') => Err(syntax(parser.pos, "unmatched `)`")),
        Some(c) => Err(syntax(parser.pos, format!("unexpected `{c}`"))),
    }
}

impl QueryExpr {
    /// Evaluates against `doc` inside `env.context`, returning matches in a
    /// deterministic order (container order, then pid).
    ///
    /// The order is tree pre-order over occurrences: entities of the context
    /// in container order (`Entities::iter()`, then mesh edges and faces that
    /// have no pid), the contents of a group/instance right after it.
    pub fn eval(&self, doc: &Document, env: &QueryEnv<'_>) -> Result<Selection, QueryError> {
        let (context, _) = doc.resolve_container(&env.context.path).map_err(|e| {
            QueryError::PathNotFound(format!("edit context {}: {e}", env.context.path))
        })?;
        let mut evaluator = Evaluator::new(doc, *env, context);
        let matches = evaluator.eval_query(self)?;
        Ok(matches.into_values().map(|c| c.path).collect())
    }

    /// Canonical source text for the AST (`a & b`, quoted values).
    ///
    /// Parentheses appear only where precedence (or a nested group of the
    /// same operator) needs them, numbers are written in core units (mm, mm²,
    /// mm³) and `layer` is written `tag`. For ASTs produced by
    /// [`parse_query`] (n-ary nodes with at least two operands, finite
    /// numbers) `parse_query(&e.to_source()) == Ok(e)`.
    pub fn to_source(&self) -> String {
        let mut out = String::new();
        write_expr(self, Slot::Top, &mut out);
        out
    }
}

// ----- parser ---------------------------------------------------------------------

/// Deepest nesting of `( … )`, `!` and function keywords the parser accepts.
const MAX_QUERY_NESTING: usize = 256;

fn syntax(pos: usize, message: impl Into<String>) -> QueryError {
    QueryError::Syntax {
        pos,
        message: message.into(),
    }
}

/// Characters of kind words, keywords and keys (`bounded-by`, `attr.d.k`).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Characters that end an unquoted value or path.
fn ends_token(c: char) -> bool {
    c.is_whitespace() || matches!(c, '|' | '&' | '(' | ')')
}

/// `true` when `c` can start a `not` term (juxtaposition means AND).
fn starts_term(c: char) -> bool {
    matches!(c, '(' | '!' | '@' | '#') || is_word_char(c)
}

fn plain_keyword(word: &str) -> Option<QueryKeyword> {
    Some(match word {
        "all" => QueryKeyword::All,
        "selection" => QueryKeyword::Selection,
        "last" => QueryKeyword::Last,
        "solid" => QueryKeyword::Solid,
        "hidden" => QueryKeyword::Hidden,
        "visible" => QueryKeyword::Visible,
        "locked" => QueryKeyword::Locked,
        _ => return None,
    })
}

fn function_keyword(word: &str) -> Option<fn(Box<QueryExpr>) -> QueryKeyword> {
    match word {
        "connected" => Some(QueryKeyword::Connected),
        "bounded-by" => Some(QueryKeyword::BoundedBy),
        "inside" => Some(QueryKeyword::Inside),
        "children" => Some(QueryKeyword::Children),
        _ => None,
    }
}

/// Keys that take `:value`.
const MATCH_KEYS: [&str; 7] = [
    "tag",
    "layer",
    "material",
    "name",
    "definition",
    "plane",
    "normal",
];

/// Keys that take a comparison.
const COMPARE_KEYS: [&str; 4] = ["area", "length", "volume", "id"];

/// `attr.<dict>.<key>` (the key is after the last dot); `Ok(None)` for other words.
fn attr_key(word: &str, pos: usize) -> Result<Option<QueryKey>, QueryError> {
    if word != "attr" && !word.starts_with("attr.") {
        return Ok(None);
    }
    match word
        .strip_prefix("attr.")
        .and_then(|rest| rest.rsplit_once('.'))
    {
        Some((dict, key)) if !dict.is_empty() && !key.is_empty() => Ok(Some(QueryKey::Attr {
            dict: dict.to_string(),
            key: key.to_string(),
        })),
        _ => Err(syntax(pos, "expected `attr.<dictionary>.<key>`")),
    }
}

fn match_key(word: &str, start: usize, colon: usize) -> Result<QueryKey, QueryError> {
    Ok(match word {
        "tag" | "layer" => QueryKey::Tag,
        "material" => QueryKey::Material,
        "name" => QueryKey::Name,
        "definition" => QueryKey::Definition,
        "plane" => QueryKey::Plane,
        "normal" => QueryKey::Normal,
        w if COMPARE_KEYS.contains(&w) => {
            return Err(syntax(
                colon,
                format!("`{w}` is compared with `>`, `<`, `>=`, `<=` or `=`, not `:`"),
            ));
        }
        w => attr_key(w, start)?.ok_or_else(|| QueryError::UnknownKey(w.to_string()))?,
    })
}

fn compare_key(word: &str, start: usize, op: usize) -> Result<QueryKey, QueryError> {
    Ok(match word {
        "area" => QueryKey::Area,
        "length" => QueryKey::Length,
        "volume" => QueryKey::Volume,
        "id" => QueryKey::Id,
        w if MATCH_KEYS.contains(&w) => {
            return Err(syntax(
                op,
                format!("`{w}` takes `:value`, not a comparison"),
            ));
        }
        w => attr_key(w, start)?.ok_or_else(|| QueryError::UnknownKey(w.to_string()))?,
    })
}

/// A plain decimal (`12`, `-1.5`, `.5`), no exponent and no unit.
fn parse_plain_number(text: &str) -> Option<f64> {
    let (sign, digits) = match text.as_bytes().first() {
        Some(b'-') => (-1.0, &text[1..]),
        Some(b'+') => (1.0, &text[1..]),
        _ => (1.0, text),
    };
    let dots = digits.bytes().filter(|b| *b == b'.').count();
    if dots > 1
        || !digits.bytes().any(|b| b.is_ascii_digit())
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return None;
    }
    digits.parse::<f64>().ok().map(|v| sign * v)
}

/// An area (`power = 2`) or volume (`power = 3`) with an optional unit:
/// `40` (mm²), `40m2`, `40m²`, `1500cm2`, `12ft2`, `2m3`, `2m³`.
fn parse_measure(text: &str, power: i32) -> Option<f64> {
    let text = text.trim();
    let split = text.find(|c: char| c.is_alphabetic()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number = parse_plain_number(number.trim_end())?;
    if unit.is_empty() {
        return Some(number);
    }
    let suffixes: [&str; 2] = if power == 2 { ["2", "²"] } else { ["3", "³"] };
    let base = suffixes.iter().find_map(|s| unit.strip_suffix(s))?;
    if !matches!(base, "mm" | "cm" | "m" | "in" | "ft") {
        return None;
    }
    let factor = LengthUnit::from_symbol(base)?.millimeters();
    Some(number * factor.powi(power))
}

/// Converts the number of `key op number` to core units.
fn convert_number(key: &QueryKey, text: &str) -> Result<f64, QueryError> {
    let invalid = || QueryError::InvalidNumber(text.to_string());
    let value = match key {
        QueryKey::Area => parse_measure(text, 2).ok_or_else(invalid)?,
        QueryKey::Volume => parse_measure(text, 3).ok_or_else(invalid)?,
        QueryKey::Id => {
            // Digits only; parsed as f64 so every written value parses back.
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            text.parse::<f64>().map_err(|_| invalid())?
        }
        // Lengths (and numeric attributes) use the length grammar; bare
        // numbers are millimeters.
        _ => parse_length(text, LengthUnit::Millimeter)
            .map_err(|_| invalid())?
            .as_mm(),
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(invalid())
    }
}

/// Reads a double-quoted string starting at `pos` (which holds `"`); `\`
/// escapes the next character. Returns the content and the offset after the
/// closing quote.
fn read_quoted(src: &str, pos: usize) -> Result<(String, usize), QueryError> {
    let mut out = String::new();
    let mut chars = src[pos..].char_indices().skip(1);
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, pos + i + 1)),
            '\\' => match chars.next() {
                Some((_, escaped)) => out.push(escaped),
                None => break,
            },
            _ => out.push(c),
        }
    }
    Err(syntax(pos, "unterminated quoted text"))
}

/// Writes `text` in double quotes, escaping `"` and `\`.
fn push_quoted(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn bump(&mut self, c: char) {
        self.pos += c.len_utf8();
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.bump(c);
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if !c.is_whitespace() {
                break;
            }
            self.bump(c);
        }
    }

    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> &'a str {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if !keep(c) {
                break;
            }
            self.bump(c);
        }
        &self.src[start..self.pos]
    }

    fn enter(&mut self) -> Result<(), QueryError> {
        self.depth += 1;
        if self.depth > MAX_QUERY_NESTING {
            Err(syntax(self.pos, "query nested too deeply"))
        } else {
            Ok(())
        }
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn parse_or(&mut self) -> Result<QueryExpr, QueryError> {
        let mut items = vec![self.parse_and()?];
        loop {
            self.skip_ws();
            if !self.eat('|') {
                break;
            }
            items.push(self.parse_and()?);
        }
        Ok(collapse(items, QueryExpr::Or))
    }

    fn parse_and(&mut self) -> Result<QueryExpr, QueryError> {
        let mut items = vec![self.parse_not()?];
        loop {
            self.skip_ws();
            if self.eat('&') || self.peek().is_some_and(starts_term) {
                items.push(self.parse_not()?);
            } else {
                break;
            }
        }
        Ok(collapse(items, QueryExpr::And))
    }

    fn parse_not(&mut self) -> Result<QueryExpr, QueryError> {
        self.skip_ws();
        if self.eat('!') {
            self.enter()?;
            let inner = self.parse_not()?;
            self.leave();
            Ok(QueryExpr::Not(Box::new(inner)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<QueryExpr, QueryError> {
        self.skip_ws();
        let start = self.pos;
        match self.peek() {
            None => Err(syntax(start, "expected a query term, found the end")),
            Some('(') => {
                self.bump('(');
                self.enter()?;
                let inner = self.parse_or()?;
                self.expect_close(start)?;
                self.leave();
                Ok(inner)
            }
            Some('@') => self.parse_pid(),
            Some('#') => self.parse_path(),
            Some(c) if is_word_char(c) => self.parse_word(),
            Some(c) => Err(syntax(
                start,
                format!("unexpected `{c}`, expected a query term"),
            )),
        }
    }

    fn expect_close(&mut self, open: usize) -> Result<(), QueryError> {
        self.skip_ws();
        if self.eat(')') {
            Ok(())
        } else {
            Err(syntax(
                self.pos,
                format!("expected `)` to close the `(` at {open}"),
            ))
        }
    }

    fn parse_pid(&mut self) -> Result<QueryExpr, QueryError> {
        let start = self.pos;
        self.bump('@');
        let digits = self.take_while(|c| c.is_ascii_digit());
        if digits.is_empty() {
            return Err(syntax(self.pos, "expected a number after `@`"));
        }
        if self.peek().is_some_and(is_word_char) {
            return Err(syntax(self.pos, "unexpected character after a pid"));
        }
        let pid = Pid::parse(digits).map_err(|e| syntax(start, e.to_string()))?;
        Ok(QueryExpr::Atom(QueryAtom::Pid(pid)))
    }

    fn parse_path(&mut self) -> Result<QueryExpr, QueryError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if ends_token(c) {
                break;
            }
            if c == '"' {
                let (_, end) = read_quoted(self.src, self.pos)?;
                self.pos = end;
            } else {
                self.bump(c);
            }
        }
        let text = &self.src[start..self.pos];
        // Validate the syntax now; names resolve at evaluation time.
        split_path(text).map_err(|e| match e {
            QueryError::Syntax { pos, message } => syntax(start + pos, message),
            other => other,
        })?;
        Ok(QueryExpr::Atom(QueryAtom::Path(text.to_string())))
    }

    fn parse_word(&mut self) -> Result<QueryExpr, QueryError> {
        let start = self.pos;
        let word = self.take_while(is_word_char);
        let atom = |a| Ok(QueryExpr::Atom(a));

        if let Some(make) = function_keyword(word) {
            self.skip_ws();
            let open = self.pos;
            if !self.eat('(') {
                return Err(syntax(
                    open,
                    format!("`{word}` needs a query in parentheses: `{word}(…)`"),
                ));
            }
            self.enter()?;
            let inner = self.parse_or()?;
            self.expect_close(open)?;
            self.leave();
            return atom(QueryAtom::Keyword(make(Box::new(inner))));
        }
        if self.peek() == Some('(') {
            return Err(syntax(self.pos, format!("`{word}` is not a function")));
        }

        let after_word = self.pos;
        self.skip_ws();
        match self.peek() {
            Some(':') => {
                let colon = self.pos;
                self.bump(':');
                let key = match_key(word, start, colon)?;
                let value = self.read_value("a value after `:`")?;
                return atom(QueryAtom::Match { key, value });
            }
            Some(c @ ('<' | '>' | '=')) => {
                let op_pos = self.pos;
                self.bump(c);
                let op = match c {
                    '>' if self.eat('=') => CmpOp::Ge,
                    '>' => CmpOp::Gt,
                    '<' if self.eat('=') => CmpOp::Le,
                    '<' => CmpOp::Lt,
                    _ => CmpOp::Eq,
                };
                let key = compare_key(word, start, op_pos)?;
                let text = self.read_value(&format!("a number after `{}`", op.symbol()))?;
                let value = convert_number(&key, &text)?;
                return atom(QueryAtom::Compare { key, op, value });
            }
            _ => self.pos = after_word,
        }

        if let Some(kind) = EntityKind::from_keyword(word) {
            return atom(QueryAtom::Kind(kind));
        }
        if let Some(keyword) = plain_keyword(word) {
            return atom(QueryAtom::Keyword(keyword));
        }
        if let Some(key) = attr_key(word, start)? {
            // A bare `attr.d.k` tests that the attribute exists.
            return atom(QueryAtom::Match {
                key,
                value: String::new(),
            });
        }
        if MATCH_KEYS.contains(&word) || COMPARE_KEYS.contains(&word) {
            return Err(syntax(
                after_word,
                format!("key `{word}` needs `:value` or a comparison"),
            ));
        }
        if word
            .strip_suffix('s')
            .and_then(EntityKind::from_keyword)
            .is_some()
        {
            return Err(QueryError::UnknownKind(word.to_string()));
        }
        Err(QueryError::UnknownKeyword(word.to_string()))
    }

    /// Reads a quoted value or a run of characters up to whitespace or one of
    /// `| & ( )`.
    fn read_value(&mut self, what: &str) -> Result<String, QueryError> {
        self.skip_ws();
        if self.peek() == Some('"') {
            let (text, end) = read_quoted(self.src, self.pos)?;
            self.pos = end;
            return Ok(text);
        }
        let start = self.pos;
        let text = self.take_while(|c| !ends_token(c));
        if text.is_empty() {
            return Err(syntax(start, format!("expected {what}")));
        }
        Ok(text.to_string())
    }
}

/// One operand stays itself; several become `make(items)`.
fn collapse(mut items: Vec<QueryExpr>, make: fn(Vec<QueryExpr>) -> QueryExpr) -> QueryExpr {
    if items.len() == 1
        && let Some(only) = items.pop()
    {
        return only;
    }
    make(items)
}

// ----- unparser -------------------------------------------------------------------

/// Where an expression is written, which decides its parentheses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// Whole query or a function argument.
    Top,
    /// Operand of `|`.
    OrItem,
    /// Operand of `&`.
    AndItem,
    /// Operand of `!`.
    NotOperand,
}

fn write_nary(items: &[QueryExpr], sep: &str, item_slot: Slot, paren: bool, out: &mut String) {
    if paren {
        out.push('(');
    }
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        write_expr(item, item_slot, out);
    }
    if paren {
        out.push(')');
    }
}

fn write_expr(expr: &QueryExpr, slot: Slot, out: &mut String) {
    match expr {
        QueryExpr::Or(items) => match items.as_slice() {
            [] => write_expr(
                &QueryExpr::Not(Box::new(QueryExpr::Atom(QueryAtom::Keyword(
                    QueryKeyword::All,
                )))),
                slot,
                out,
            ),
            [only] => write_expr(only, slot, out),
            _ => write_nary(items, " | ", Slot::OrItem, slot != Slot::Top, out),
        },
        QueryExpr::And(items) => match items.as_slice() {
            [] => out.push_str("all"),
            [only] => write_expr(only, slot, out),
            _ => write_nary(
                items,
                " & ",
                Slot::AndItem,
                matches!(slot, Slot::AndItem | Slot::NotOperand),
                out,
            ),
        },
        QueryExpr::Not(inner) => {
            out.push('!');
            write_expr(inner, Slot::NotOperand, out);
        }
        QueryExpr::Atom(atom) => write_atom(atom, out),
    }
}

fn write_atom(atom: &QueryAtom, out: &mut String) {
    match atom {
        QueryAtom::Kind(kind) => out.push_str(kind.keyword()),
        QueryAtom::Pid(pid) => out.push_str(&pid.to_string()),
        QueryAtom::Path(text) => out.push_str(text),
        QueryAtom::Match { key, value } => {
            out.push_str(&key.source_text());
            if matches!(key, QueryKey::Attr { .. }) && value.is_empty() {
                return;
            }
            out.push(':');
            if value.is_empty()
                || value
                    .chars()
                    .any(|c| ends_token(c) || matches!(c, '"' | '\\'))
            {
                push_quoted(out, value);
            } else {
                out.push_str(value);
            }
        }
        QueryAtom::Compare { key, op, value } => {
            out.push_str(&key.source_text());
            out.push_str(op.symbol());
            // `Display` for f64 is the shortest text that parses back exactly.
            out.push_str(&value.to_string());
        }
        QueryAtom::Keyword(keyword) => {
            let (name, inner) = match keyword {
                QueryKeyword::All => ("all", None),
                QueryKeyword::Selection => ("selection", None),
                QueryKeyword::Last => ("last", None),
                QueryKeyword::Solid => ("solid", None),
                QueryKeyword::Hidden => ("hidden", None),
                QueryKeyword::Visible => ("visible", None),
                QueryKeyword::Locked => ("locked", None),
                QueryKeyword::Connected(q) => ("connected", Some(q)),
                QueryKeyword::BoundedBy(q) => ("bounded-by", Some(q)),
                QueryKeyword::Inside(q) => ("inside", Some(q)),
                QueryKeyword::Children(q) => ("children", Some(q)),
            };
            out.push_str(name);
            if let Some(q) = inner {
                out.push('(');
                write_expr(q, Slot::Top, out);
                out.push(')');
            }
        }
    }
}

// ----- paths (docs/08 §5.3) ---------------------------------------------------------

/// What one path segment addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    /// `@42` or `42`.
    Pid(Pid),
    /// `Name` or `Name[n]` (0-based; no index = 0).
    Name { name: String, index: usize },
}

/// A segment with its source text and byte offset.
#[derive(Debug, Clone)]
struct RawSegment {
    raw: String,
    kind: PathSegment,
}

/// Splits `#Model/a/b[2]/@42` into the segments after `Model`.
fn split_path(text: &str) -> Result<Vec<RawSegment>, QueryError> {
    let mut pos = usize::from(text.starts_with('#'));
    if !text[pos..].starts_with("Model") {
        return Err(syntax(pos, "a path starts with `#Model`"));
    }
    pos += "Model".len();
    let mut segments = Vec::new();
    while pos < text.len() {
        if !text[pos..].starts_with('/') {
            return Err(syntax(pos, "expected `/` between path segments"));
        }
        pos += 1;
        let start = pos;
        let kind = if text[pos..].starts_with('"') {
            let (name, end) = read_quoted(text, pos)?;
            pos = end;
            let index = if text[pos..].starts_with('[') {
                let close = text[pos..]
                    .find(']')
                    .map(|i| pos + i)
                    .ok_or_else(|| syntax(pos, "unterminated `[` index"))?;
                let index = parse_index(&text[pos + 1..close], pos + 1)?;
                pos = close + 1;
                index
            } else {
                0
            };
            if pos < text.len() && !text[pos..].starts_with('/') {
                return Err(syntax(pos, "expected `/` after a quoted name"));
            }
            PathSegment::Name { name, index }
        } else {
            let end = text[pos..].find('/').map_or(text.len(), |i| pos + i);
            let raw = &text[pos..end];
            pos = end;
            parse_segment(raw, start)?
        };
        segments.push(RawSegment {
            raw: text[start..pos].to_string(),
            kind,
        });
    }
    if segments.is_empty() {
        return Err(syntax(text.len(), "a path needs an entity after `#Model`"));
    }
    Ok(segments)
}

fn parse_index(digits: &str, pos: usize) -> Result<usize, QueryError> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(syntax(pos, "a path index is a number: `Name[2]`"));
    }
    digits
        .parse()
        .map_err(|_| syntax(pos, "path index out of range"))
}

/// An unquoted segment: `@42`, `42`, `Name` or `Name[n]`.
fn parse_segment(raw: &str, pos: usize) -> Result<PathSegment, QueryError> {
    if raw.is_empty() {
        return Err(syntax(pos, "empty path segment"));
    }
    if let Some(digits) = raw.strip_prefix('@') {
        return Pid::parse(digits)
            .map(PathSegment::Pid)
            .map_err(|e| syntax(pos, e.to_string()));
    }
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        return Pid::parse(raw)
            .map(PathSegment::Pid)
            .map_err(|e| syntax(pos, e.to_string()));
    }
    let (name, index) = match (raw.strip_suffix(']'), raw.rfind('[')) {
        (Some(body), Some(open)) => (
            &raw[..open],
            parse_index(&body[open + 1..], pos + open + 1)?,
        ),
        _ => (raw, 0),
    };
    if let Some(bad) = name.find(['"', '[', ']']) {
        return Err(syntax(
            pos + bad,
            "names containing quotes or brackets must be double-quoted",
        ));
    }
    if name.is_empty() {
        return Err(syntax(pos, "empty name in path segment"));
    }
    Ok(PathSegment::Name {
        name: name.to_string(),
        index,
    })
}

/// The name a path uses for a group (its name) or an instance (its name,
/// else its definition's name); `None` for other entities and empty names.
fn display_name<'d>(doc: &'d Document, entities: &'d Entities, id: EntityId) -> Option<&'d str> {
    let name = match id {
        EntityId::Group(g) => entities.group(g).map(|g| g.name.as_str()),
        EntityId::Instance(i) => entities
            .instance(i)
            .and_then(|inst| match inst.name.as_deref() {
                Some(n) if !n.is_empty() => Some(n),
                _ => doc.definition(inst.definition).map(|d| d.name.as_str()),
            }),
        _ => None,
    }?;
    (!name.is_empty()).then_some(name)
}

/// The `index`-th group/instance named `name` in container order.
fn find_named(doc: &Document, entities: &Entities, name: &str, index: usize) -> Option<EntityId> {
    entities
        .iter()
        .filter(|id| display_name(doc, entities, *id) == Some(name))
        .nth(index)
}

/// The key a path uses for an entity: its pid, else its id.
fn key_of(entities: &Entities, id: EntityId) -> EntityKey {
    entities
        .pid_of(id)
        .map_or(EntityKey::Id(id), EntityKey::Pid)
}

/// One segment of [`EntityPath::display_with`].
fn segment_text(doc: &Document, entities: &Entities, id: EntityId, key: EntityKey) -> String {
    if let Some(name) = display_name(doc, entities, id) {
        let index = entities
            .iter()
            .take_while(|other| *other != id)
            .filter(|other| display_name(doc, entities, *other) == Some(name))
            .count();
        let mut out = String::new();
        let needs_quotes = name.starts_with('@')
            || name.bytes().all(|b| b.is_ascii_digit())
            || name
                .chars()
                .any(|c| ends_token(c) || matches!(c, '/' | '[' | ']' | '"' | '\\'));
        if needs_quotes {
            push_quoted(&mut out, name);
        } else {
            out.push_str(name);
        }
        if index > 0 {
            out.push_str(&format!("[{index}]"));
        }
        return out;
    }
    match entities.pid_of(id) {
        Some(pid) => pid.to_string(),
        None => key.to_string(),
    }
}

// ----- evaluation -----------------------------------------------------------------

/// Entities of one container in query order: `Entities::iter()` order, then
/// mesh edges and faces that have no pid (slot order).
#[derive(Debug, Default)]
struct Listing {
    items: Vec<(EntityId, EntityKey)>,
    index: BTreeMap<EntityId, usize>,
}

impl Listing {
    fn of(entities: &Entities) -> Self {
        let mesh = entities.mesh();
        let bound = entities.iter().filter(|id| entities.contains(*id));
        let unbound = mesh
            .edges()
            .map(|(e, _)| EntityId::Edge(e))
            .chain(mesh.faces().map(|(f, _)| EntityId::Face(f)))
            .filter(|id| entities.pid_of(*id).is_none());
        let items: Vec<(EntityId, EntityKey)> = bound
            .chain(unbound)
            .map(|id| (id, key_of(entities, id)))
            .collect();
        let index = items
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, i))
            .collect();
        Listing { items, index }
    }
}

/// One entity occurrence considered by the evaluator.
#[derive(Debug, Clone)]
struct Cand {
    /// The occurrence as reported to the caller.
    path: EntityPath,
    /// Container holding the entity.
    container: ContainerRef,
    /// The entity.
    id: EntityId,
    /// Container-to-context transform; `None` inside the context itself.
    local: Option<DAffine3>,
    /// Listing indices from the context down to the entity (pre-order key).
    order: Vec<usize>,
}

impl Cand {
    /// Maps a container-local point to the context frame.
    fn to_context(&self, p: DVec3) -> DVec3 {
        self.local.map_or(p, |t| t.transform_point3(p))
    }
}

/// Occurrences keyed (and therefore ordered) by their pre-order key.
type CandSet = BTreeMap<Vec<usize>, Cand>;

/// Numeric properties of `area`, `length`, `volume` and `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measure {
    Area,
    Length,
    Volume,
    Id,
}

/// A query compiled against one document: names resolved, values parsed,
/// function terms evaluated to sets.
#[derive(Debug, Clone)]
enum Node {
    Always,
    Never,
    Kind(EntityKind),
    Pid(Pid),
    /// Member of the set produced by a function term or `#path`.
    Member(usize),
    Selected,
    Last,
    Solid,
    Hidden,
    Visible,
    Locked,
    Tag(TagId),
    Material(MaterialId),
    Name(String),
    Definition(String),
    Attr {
        dict: String,
        key: String,
        /// `None` = the attribute exists.
        value: Option<String>,
    },
    AttrCmp {
        dict: String,
        key: String,
        op: CmpOp,
        value: f64,
    },
    Plane(Plane),
    Normal(DVec3),
    Measure {
        what: Measure,
        op: CmpOp,
        value: f64,
    },
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
}

struct Evaluator<'a> {
    doc: &'a Document,
    env: QueryEnv<'a>,
    tol: Tolerances,
    context: ContainerRef,
    listings: BTreeMap<ContainerRef, Rc<Listing>>,
    last: BTreeSet<Pid>,
    selected: Option<BTreeSet<Vec<usize>>>,
}

impl<'a> Evaluator<'a> {
    fn new(doc: &'a Document, env: QueryEnv<'a>, context: ContainerRef) -> Self {
        Evaluator {
            doc,
            env,
            tol: *doc.tolerances(),
            context,
            listings: BTreeMap::new(),
            last: env.last.iter().copied().collect(),
            selected: None,
        }
    }

    fn listing(&mut self, container: ContainerRef) -> Rc<Listing> {
        if let Some(listing) = self.listings.get(&container) {
            return Rc::clone(listing);
        }
        let listing = Rc::new(
            self.doc
                .container(container)
                .map(Listing::of)
                .unwrap_or_default(),
        );
        self.listings.insert(container, Rc::clone(&listing));
        listing
    }

    /// The context container's entities.
    fn universe(&mut self) -> CandSet {
        let listing = self.listing(self.context);
        listing
            .items
            .iter()
            .enumerate()
            .map(|(i, (id, key))| {
                let cand = Cand {
                    path: EntityPath::new(self.env.context.path.clone(), *key),
                    container: self.context,
                    id: *id,
                    local: None,
                    order: vec![i],
                };
                (vec![i], cand)
            })
            .collect()
    }

    /// Evaluates one (sub-)query: compile, then filter the domain.
    fn eval_query(&mut self, expr: &QueryExpr) -> Result<CandSet, QueryError> {
        let mut produced = Vec::new();
        let node = self.compile(expr, &mut produced)?;
        let mut domain = self.universe();
        for set in &produced {
            for (key, cand) in set {
                domain.entry(key.clone()).or_insert_with(|| cand.clone());
            }
        }
        Ok(domain
            .into_iter()
            .filter(|(_, cand)| self.test(&node, cand, &produced))
            .collect())
    }

    fn compile(
        &mut self,
        expr: &QueryExpr,
        produced: &mut Vec<CandSet>,
    ) -> Result<Node, QueryError> {
        Ok(match expr {
            QueryExpr::Or(items) => Node::Or(
                items
                    .iter()
                    .map(|e| self.compile(e, produced))
                    .collect::<Result<_, _>>()?,
            ),
            QueryExpr::And(items) => Node::And(
                items
                    .iter()
                    .map(|e| self.compile(e, produced))
                    .collect::<Result<_, _>>()?,
            ),
            QueryExpr::Not(inner) => Node::Not(Box::new(self.compile(inner, produced)?)),
            QueryExpr::Atom(atom) => self.compile_atom(atom, produced)?,
        })
    }

    fn compile_atom(
        &mut self,
        atom: &QueryAtom,
        produced: &mut Vec<CandSet>,
    ) -> Result<Node, QueryError> {
        let mut member = |set: CandSet| {
            produced.push(set);
            Node::Member(produced.len() - 1)
        };
        Ok(match atom {
            // Vertices are not entities (docs/04 §2): nothing to return.
            QueryAtom::Kind(EntityKind::Vertex) => Node::Never,
            QueryAtom::Kind(kind) => Node::Kind(*kind),
            QueryAtom::Pid(pid) => Node::Pid(*pid),
            QueryAtom::Path(text) => member(self.path_set(text)?),
            QueryAtom::Match { key, value } => self.compile_match(key, value)?,
            QueryAtom::Compare { key, op, value } => compile_compare(key, *op, *value)?,
            QueryAtom::Keyword(keyword) => match keyword {
                QueryKeyword::All => Node::Always,
                QueryKeyword::Selection => {
                    self.ensure_selected();
                    Node::Selected
                }
                QueryKeyword::Last => Node::Last,
                QueryKeyword::Solid => Node::Solid,
                QueryKeyword::Hidden => Node::Hidden,
                QueryKeyword::Visible => Node::Visible,
                QueryKeyword::Locked => Node::Locked,
                QueryKeyword::Connected(q) => {
                    let inner = self.eval_query(q)?;
                    member(self.connected(&inner))
                }
                QueryKeyword::BoundedBy(q) => {
                    let inner = self.eval_query(q)?;
                    member(self.bounded_by(&inner))
                }
                QueryKeyword::Inside(q) => {
                    let inner = self.eval_query(q)?;
                    member(self.inside(&inner))
                }
                QueryKeyword::Children(q) => {
                    let inner = self.eval_query(q)?;
                    member(self.children(&inner))
                }
            },
        })
    }

    fn compile_match(&self, key: &QueryKey, value: &str) -> Result<Node, QueryError> {
        Ok(match key {
            QueryKey::Tag => self.doc.tags.by_name(value).map_or(Node::Never, Node::Tag),
            QueryKey::Material => self
                .doc
                .materials
                .by_name(value)
                .map_or(Node::Never, Node::Material),
            QueryKey::Name => Node::Name(value.to_string()),
            QueryKey::Definition => Node::Definition(value.to_string()),
            QueryKey::Attr { dict, key } => Node::Attr {
                dict: dict.clone(),
                key: key.clone(),
                value: (!value.is_empty()).then(|| value.to_string()),
            },
            QueryKey::Plane => Node::Plane(parse_plane_value(value)?),
            QueryKey::Normal => Node::Normal(parse_normal_value(value)?),
            QueryKey::Area | QueryKey::Length | QueryKey::Volume | QueryKey::Id => {
                return Err(QueryError::Eval(format!(
                    "`{}` needs a comparison, not `:{value}`",
                    key.source_text()
                )));
            }
        })
    }

    fn ensure_selected(&mut self) {
        if self.selected.is_some() {
            return;
        }
        let selection = self.env.selection;
        let set = selection
            .iter()
            .filter_map(|path| self.cand_from_path(path))
            .map(|cand| cand.order)
            .collect();
        self.selected = Some(set);
    }

    /// The occurrence `path` denotes, when it lies in (or below) the context.
    fn cand_from_path(&mut self, path: &EntityPath) -> Option<Cand> {
        let doc = self.doc;
        let context: &'a EditContext = self.env.context;
        let context_path = &context.path;
        let steps = &path.instances.steps;
        if steps.len() < context_path.depth() {
            return None;
        }
        // The leading steps must name the context's occurrence.
        let mut container = ContainerRef::Model;
        for (mine, theirs) in steps.iter().zip(&context_path.steps) {
            let entities = doc.container(container)?;
            let id = entities.key_to_id(*mine)?;
            if entities.key_to_id(*theirs)? != id {
                return None;
            }
            container = ContainerRef::Definition(entities.definition_of(id)?);
        }
        let mut instances = context_path.clone();
        let mut local: Option<DAffine3> = None;
        let mut order = Vec::new();
        for step in steps.iter().skip(context_path.depth()) {
            let entities = doc.container(container)?;
            let id = entities.key_to_id(*step)?;
            let listing = self.listing(container);
            let i = *listing.index.get(&id)?;
            order.push(i);
            instances = instances.child(listing.items.get(i)?.1);
            let t = entities.transform_of(id)?;
            local = Some(local.map_or(t, |l| l * t));
            container = ContainerRef::Definition(entities.definition_of(id)?);
        }
        let entities = doc.container(container)?;
        let id = entities.key_to_id(path.target)?;
        let listing = self.listing(container);
        let i = *listing.index.get(&id)?;
        order.push(i);
        Some(Cand {
            path: EntityPath::new(instances, listing.items.get(i)?.1),
            container,
            id,
            local,
            order,
        })
    }

    /// `#path`: the one occurrence it names (unknown names match nothing).
    fn path_set(&mut self, text: &str) -> Result<CandSet, QueryError> {
        let path = match EntityPath::parse(text, self.doc) {
            Ok(path) => path,
            Err(QueryError::PathNotFound(_)) => return Ok(CandSet::new()),
            Err(e) => return Err(QueryError::Eval(format!("path `{text}`: {e}"))),
        };
        Ok(self
            .cand_from_path(&path)
            .map(|c| (c.order.clone(), c))
            .into_iter()
            .collect())
    }

    /// Another entity of the same container occurrence as `of`.
    fn sibling(&mut self, of: &Cand, id: EntityId) -> Option<Cand> {
        let listing = self.listing(of.container);
        let i = *listing.index.get(&id)?;
        let key = listing.items.get(i)?.1;
        let (_, prefix) = of.order.split_last()?;
        let mut order = prefix.to_vec();
        order.push(i);
        Some(Cand {
            path: EntityPath::new(of.path.instances.clone(), key),
            container: of.container,
            id,
            local: of.local,
            order,
        })
    }

    /// `children(q)`: the contents of every matched group/instance.
    fn children(&mut self, inner: &CandSet) -> CandSet {
        let doc = self.doc;
        let mut out = CandSet::new();
        for parent in inner.values() {
            let Some(entities) = doc.container(parent.container) else {
                continue;
            };
            let (Some(definition), Some(t)) = (
                entities.definition_of(parent.id),
                entities.transform_of(parent.id),
            ) else {
                continue;
            };
            let instances = parent.path.instances.child(parent.path.target);
            if instances.depth() > Entities::MAX_DEPTH {
                continue;
            }
            let container = ContainerRef::Definition(definition);
            let local = Some(parent.local.map_or(t, |l| l * t));
            let listing = self.listing(container);
            for (i, (id, key)) in listing.items.iter().enumerate() {
                let mut order = parent.order.clone();
                order.push(i);
                let cand = Cand {
                    path: EntityPath::new(instances.clone(), *key),
                    container,
                    id: *id,
                    local,
                    order: order.clone(),
                };
                out.insert(order, cand);
            }
        }
        out
    }

    /// `connected(q)`: edges and faces sharing vertices with the matched
    /// edges/faces, transitively (seeds included).
    fn connected(&mut self, inner: &CandSet) -> CandSet {
        let doc = self.doc;
        // Seeds grouped by container occurrence (pre-order key minus the last index).
        let mut groups: BTreeMap<Vec<usize>, (Cand, Vec<EntityId>)> = BTreeMap::new();
        for cand in inner.values() {
            if let (true, Some((_, prefix))) = (cand.id.is_mesh_entity(), cand.order.split_last()) {
                groups
                    .entry(prefix.to_vec())
                    .or_insert_with(|| (cand.clone(), Vec::new()))
                    .1
                    .push(cand.id);
            }
        }
        let mut out = CandSet::new();
        for (template, seeds) in groups.into_values() {
            let Some(entities) = doc.container(template.container) else {
                continue;
            };
            for id in flood_fill(entities.mesh(), &seeds) {
                if let Some(cand) = self.sibling(&template, id) {
                    out.insert(cand.order.clone(), cand);
                }
            }
        }
        out
    }

    /// `bounded-by(q)`: the edges of every matched face.
    fn bounded_by(&mut self, inner: &CandSet) -> CandSet {
        let doc = self.doc;
        let mut out = CandSet::new();
        for cand in inner.values() {
            let EntityId::Face(f) = cand.id else {
                continue;
            };
            let Some(Ok(edges)) = doc
                .container(cand.container)
                .map(|entities| entities.mesh().face_edges(f))
            else {
                continue;
            };
            for e in edges {
                if let Some(edge) = self.sibling(cand, EntityId::Edge(e)) {
                    out.insert(edge.order.clone(), edge);
                }
            }
        }
        out
    }

    /// `inside(q)`: context entities whose bounds lie inside the union of the
    /// matches' bounds (the matches themselves excluded).
    fn inside(&mut self, inner: &CandSet) -> CandSet {
        let union = inner
            .values()
            .fold(Aabb::EMPTY, |acc, c| acc.union(&self.bounds(c)));
        if union.is_empty() {
            return CandSet::new();
        }
        let region = union.expanded(self.tol.length);
        self.universe()
            .into_iter()
            .filter(|(key, cand)| {
                let b = self.bounds(cand);
                !inner.contains_key(key) && !b.is_empty() && region.contains_aabb(&b)
            })
            .collect()
    }

    /// Bounds of an occurrence in the context frame (empty when it has none).
    fn bounds(&self, cand: &Cand) -> Aabb {
        let Some(entities) = self.doc.container(cand.container) else {
            return Aabb::EMPTY;
        };
        let mesh = entities.mesh();
        let points = |ps: Vec<DVec3>| Aabb::from_points(ps.into_iter().map(|p| cand.to_context(p)));
        match cand.id {
            EntityId::Edge(e) => mesh
                .edge_segment(e)
                .map_or(Aabb::EMPTY, |s| points(vec![s.a, s.b])),
            EntityId::Face(f) => mesh.face_vertices(f).map_or(Aabb::EMPTY, |vs| {
                points(
                    vs.into_iter()
                        .filter_map(|v| mesh.position(v).ok())
                        .collect(),
                )
            }),
            EntityId::Group(_) | EntityId::Instance(_) => {
                let (Some(def), Some(t)) = (
                    self.definition_of(entities, cand.id),
                    entities.transform_of(cand.id),
                ) else {
                    return Aabb::EMPTY;
                };
                let bb = def
                    .entities
                    .local_aabb(&self.doc.definitions, false)
                    .transformed(&t);
                match &cand.local {
                    Some(l) => bb.transformed(l),
                    None => bb,
                }
            }
            EntityId::Image(i) => entities.image(i).map_or(Aabb::EMPTY, |image| {
                let (w, h) = (image.width.as_mm(), image.height.as_mm());
                points(
                    [
                        DVec3::ZERO,
                        DVec3::new(w, 0.0, 0.0),
                        DVec3::new(w, h, 0.0),
                        DVec3::new(0.0, h, 0.0),
                    ]
                    .into_iter()
                    .map(|c| image.transform.transform_point3(c))
                    .collect(),
                )
            }),
            EntityId::Guide(g) => match entities.guide(g) {
                Some(Guide::Point { pos }) => points(vec![*pos]),
                Some(Guide::Line {
                    origin,
                    dir,
                    finite: Some((t0, t1)),
                }) => points(vec![*origin + *dir * *t0, *origin + *dir * *t1]),
                _ => Aabb::EMPTY,
            },
            EntityId::Dimension(_) | EntityId::Text(_) | EntityId::SectionPlane(_) => Aabb::EMPTY,
        }
    }

    fn definition_of(&self, entities: &Entities, id: EntityId) -> Option<&'a ComponentDefinition> {
        let doc = self.doc;
        entities.definition_of(id).and_then(|d| doc.definition(d))
    }

    /// Own hidden flag or an invisible tag (docs/04 §5).
    fn is_hidden(&self, entities: &Entities, id: EntityId) -> bool {
        let tag = entities
            .tag_of(id)
            .unwrap_or_else(|| self.doc.tags.untagged());
        entities.is_hidden(id) || !self.doc.tags.is_visible(tag)
    }

    fn test(&self, node: &Node, cand: &Cand, produced: &[CandSet]) -> bool {
        let Some(entities) = self.doc.container(cand.container) else {
            return false;
        };
        let id = cand.id;
        match node {
            Node::Always => true,
            Node::Never => false,
            Node::Kind(kind) => id.kind() == *kind,
            Node::Pid(pid) => entities.pid_of(id) == Some(*pid),
            Node::Member(i) => produced
                .get(*i)
                .is_some_and(|s| s.contains_key(&cand.order)),
            Node::Selected => self
                .selected
                .as_ref()
                .is_some_and(|s| s.contains(&cand.order)),
            Node::Last => entities
                .pid_of(id)
                .is_some_and(|pid| self.last.contains(&pid)),
            Node::Solid => self
                .definition_of(entities, id)
                .is_some_and(|d| d.entities.mesh().is_solid()),
            Node::Hidden => self.is_hidden(entities, id),
            Node::Visible => !self.is_hidden(entities, id),
            Node::Locked => entities.is_locked(id),
            Node::Tag(tag) => {
                entities
                    .tag_of(id)
                    .unwrap_or_else(|| self.doc.tags.untagged())
                    == *tag
            }
            Node::Material(material) => self.has_material(entities, id, *material),
            Node::Name(glob) => self
                .names(entities, id)
                .into_iter()
                .any(|name| glob_match(glob, name)),
            Node::Definition(name) => self
                .definition_of(entities, id)
                .is_some_and(|d| d.name == *name),
            Node::Attr { dict, key, value } => {
                attr_of(entities, id, dict, key).is_some_and(|v| match value {
                    None => true,
                    Some(want) => attr_equals(v, want, &self.tol),
                })
            }
            Node::AttrCmp {
                dict,
                key,
                op,
                value,
            } => attr_of(entities, id, dict, key)
                .and_then(attr_number)
                .is_some_and(|a| op.holds(a, *value, self.tol.length)),
            Node::Plane(plane) => {
                face_plane(entities, cand).is_some_and(|p| self.tol.is_coplanar(&p, plane))
            }
            Node::Normal(dir) => {
                face_plane(entities, cand).is_some_and(|p| self.tol.same_direction(p.normal, *dir))
            }
            Node::Measure { what, op, value } => self
                .measure(entities, cand, *what)
                .is_some_and(|a| op.holds(a, *value, measure_eps(*what, a, *value, &self.tol))),
            Node::Not(inner) => !self.test(inner, cand, produced),
            Node::And(items) => items.iter().all(|n| self.test(n, cand, produced)),
            Node::Or(items) => items.iter().any(|n| self.test(n, cand, produced)),
        }
    }

    /// Faces whose front or back resolves to `material`; instances overriding with it.
    fn has_material(&self, entities: &Entities, id: EntityId, material: MaterialId) -> bool {
        let materials = &self.doc.materials;
        match id {
            EntityId::Face(f) => entities.mesh().face(f).is_some_and(|face| {
                materials.resolve(face.sides.material_front, FrontOrBack::Front) == material
                    || materials.resolve(face.sides.material_back, FrontOrBack::Back) == material
            }),
            EntityId::Instance(i) => entities
                .instance(i)
                .is_some_and(|inst| inst.material_override == Some(material)),
            _ => false,
        }
    }

    /// Names `name:` matches: the group/instance name and its definition's name.
    fn names<'e>(&self, entities: &'e Entities, id: EntityId) -> Vec<&'e str>
    where
        'a: 'e,
    {
        let own = match id {
            EntityId::Group(g) => entities.group(g).map(|g| g.name.as_str()),
            EntityId::Instance(i) => entities.instance(i).and_then(|i| i.name.as_deref()),
            _ => return Vec::new(),
        };
        let definition = self.definition_of(entities, id).map(|d| d.name.as_str());
        own.into_iter()
            .chain(definition)
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// `area`, `length`, `volume` (context frame) or `id` of an occurrence.
    fn measure(&self, entities: &Entities, cand: &Cand, what: Measure) -> Option<f64> {
        let mesh = entities.mesh();
        match (what, cand.id) {
            (Measure::Length, EntityId::Edge(e)) => {
                let s = mesh.edge_segment(e).ok()?;
                Some(cand.to_context(s.a).distance(cand.to_context(s.b)))
            }
            (Measure::Area, EntityId::Face(f)) => match &cand.local {
                None => mesh.face_area(f).ok(),
                Some(_) => {
                    let snapshot = mesh.face_snapshot(f).ok()?;
                    let mapped = |ps: &[DVec3]| -> Vec<DVec3> {
                        ps.iter().map(|p| cand.to_context(*p)).collect()
                    };
                    let holes: f64 = snapshot
                        .holes
                        .iter()
                        .map(|h| polygon_area(&mapped(h)))
                        .sum();
                    Some(polygon_area(&mapped(&snapshot.outer)) - holes)
                }
            },
            (Measure::Volume, EntityId::Group(_) | EntityId::Instance(_)) => {
                let def_mesh = self.definition_of(entities, cand.id)?.entities.mesh();
                if !def_mesh.is_solid() {
                    return None;
                }
                let t = entities.transform_of(cand.id)?;
                let scale = cand.local.map_or(1.0, |l| l.matrix3.determinant().abs());
                Some(def_mesh.volume().ok()?.abs() * t.matrix3.determinant().abs() * scale)
            }
            (Measure::Id, _) => entities.pid_of(cand.id).map(|pid| pid.raw() as f64),
            _ => None,
        }
    }
}

fn compile_compare(key: &QueryKey, op: CmpOp, value: f64) -> Result<Node, QueryError> {
    if !value.is_finite() {
        return Err(QueryError::Eval(format!(
            "`{}` needs a finite number",
            key.source_text()
        )));
    }
    let measure = |what| Node::Measure { what, op, value };
    Ok(match key {
        QueryKey::Area => measure(Measure::Area),
        QueryKey::Length => measure(Measure::Length),
        QueryKey::Volume => measure(Measure::Volume),
        QueryKey::Id => measure(Measure::Id),
        QueryKey::Attr { dict, key } => Node::AttrCmp {
            dict: dict.clone(),
            key: key.clone(),
            op,
            value,
        },
        other => {
            return Err(QueryError::Eval(format!(
                "`{}` cannot be compared with a number",
                other.source_text()
            )));
        }
    })
}

/// Tolerance of `=` (and of the strictness of `<`/`>`) per measure: the length
/// tolerance, and its effect on a square of the same area or a cube of the
/// same volume.
fn measure_eps(what: Measure, a: f64, b: f64, tol: &Tolerances) -> f64 {
    let size = a.abs().max(b.abs());
    match what {
        Measure::Length => tol.length,
        Measure::Area => tol.length_sq + 2.0 * tol.length * size.sqrt(),
        Measure::Volume => {
            tol.length_sq * tol.length + 3.0 * tol.length * libm::pow(size, 2.0 / 3.0)
        }
        Measure::Id => 0.5,
    }
}

/// `plane:xy@1200` → the plane Z = 1200 (offset defaults to 0).
fn parse_plane_value(value: &str) -> Result<Plane, QueryError> {
    let (axes, offset) = match value.split_once('@') {
        Some((axes, offset)) => (axes, Some(offset)),
        None => (value, None),
    };
    let normal = match axes.to_ascii_lowercase().as_str() {
        "xy" | "yx" => DVec3::Z,
        "xz" | "zx" => DVec3::Y,
        "yz" | "zy" => DVec3::X,
        _ => {
            return Err(QueryError::Eval(format!(
                "plane `{value}`: expected `xy`, `xz` or `yz`, optionally `@<offset>`"
            )));
        }
    };
    let offset = match offset {
        Some(text) => parse_length(text, LengthUnit::Millimeter)
            .map_err(|e| QueryError::Eval(format!("plane offset `{text}`: {e}")))?
            .as_mm(),
        None => 0.0,
    };
    Ok(Plane::new(normal, -offset))
}

/// `normal:z`, `normal:-x`, `normal:+y`.
fn parse_normal_value(value: &str) -> Result<DVec3, QueryError> {
    let lower = value.to_ascii_lowercase();
    let (sign, axis) = match lower.strip_prefix('-') {
        Some(axis) => (-1.0, axis),
        None => (1.0, lower.strip_prefix('+').unwrap_or(&lower)),
    };
    let dir = match axis {
        "x" => DVec3::X,
        "y" => DVec3::Y,
        "z" => DVec3::Z,
        _ => {
            return Err(QueryError::Eval(format!(
                "normal `{value}`: expected `x`, `y`, `z`, `-x`, `-y` or `-z`"
            )));
        }
    };
    Ok(dir * sign)
}

/// A face's plane in the context frame.
fn face_plane(entities: &Entities, cand: &Cand) -> Option<Plane> {
    let EntityId::Face(f) = cand.id else {
        return None;
    };
    let plane = entities.mesh().face(f)?.plane;
    Some(match &cand.local {
        Some(t) => plane.transformed(t),
        None => plane,
    })
}

/// Area of a planar polygon in 3D (Newell's method).
fn polygon_area(points: &[DVec3]) -> f64 {
    let n = points.len();
    let sum = (0..n).fold(DVec3::ZERO, |acc, i| {
        acc + points[i].cross(points[(i + 1) % n])
    });
    sum.length() * 0.5
}

/// Edges and faces reachable from `seeds` through shared vertices. A face
/// touches every vertex of its loops (so holes connect to the outer loop).
fn flood_fill(mesh: &HalfEdgeMesh, seeds: &[EntityId]) -> BTreeSet<EntityId> {
    fn reach_face(
        mesh: &HalfEdgeMesh,
        f: crate::ids::FaceId,
        reached: &mut BTreeSet<EntityId>,
        queue: &mut Vec<VertexId>,
    ) {
        if reached.insert(EntityId::Face(f)) {
            for e in mesh.face_edges(f).unwrap_or_default() {
                if let Ok((a, b)) = mesh.edge_endpoints(e) {
                    queue.extend([a, b]);
                }
            }
        }
    }
    let mut reached = BTreeSet::new();
    let mut queue = Vec::new();
    for seed in seeds {
        match *seed {
            EntityId::Edge(e) => {
                if let Ok((a, b)) = mesh.edge_endpoints(e) {
                    reached.insert(EntityId::Edge(e));
                    queue.extend([a, b]);
                }
            }
            EntityId::Face(f) => reach_face(mesh, f, &mut reached, &mut queue),
            _ => {}
        }
    }
    let mut visited = BTreeSet::new();
    while let Some(v) = queue.pop() {
        if !visited.insert(v) {
            continue;
        }
        for &h in mesh.vertex_star(v) {
            let Some(he) = mesh.half_edge(h) else {
                continue;
            };
            reached.insert(EntityId::Edge(he.edge));
            if let Some(f) = he.face {
                reach_face(mesh, f, &mut reached, &mut queue);
            }
            if let Some(twin) = mesh.half_edge(he.twin) {
                queue.push(twin.origin);
                if let Some(f) = twin.face {
                    reach_face(mesh, f, &mut reached, &mut queue);
                }
            }
        }
    }
    reached
}

/// `*` matches any run of characters (including none); everything else is literal.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(c) if *c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p.get(pi..)
        .is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

/// An entity's attribute (side table first, then an instance's own attributes).
fn attr_of<'e>(entities: &'e Entities, id: EntityId, dict: &str, key: &str) -> Option<&'e Value> {
    entities
        .attrs(id)
        .and_then(|a| a.get(dict, key))
        .or_else(|| match id {
            EntityId::Instance(i) => entities
                .instance(i)
                .and_then(|inst| inst.dynamic_attrs.get(dict, key)),
            _ => None,
        })
}

/// Numeric attribute value (lengths in mm).
fn attr_number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Length(l) => Some(l.as_mm()),
        _ => None,
    }
}

/// `attr.d.k:value`: lengths compare numerically (the value is parsed with
/// the length grammar), everything else by its text form.
fn attr_equals(value: &Value, want: &str, tol: &Tolerances) -> bool {
    if let Value::Length(l) = value
        && let Ok(w) = parse_length(want, LengthUnit::Millimeter)
    {
        return tol.approx_eq_len(l.as_mm(), w.as_mm());
    }
    value_text(value) == want
}

/// Text form of an attribute value for `attr.d.k:value` matching.
fn value_text(value: &Value) -> String {
    match value {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Length(l) => l.to_string(),
        Value::Str(s) => s.clone(),
        Value::Bytes(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        Value::Vec3(v) => format!("{},{},{}", v.x, v.y, v.z),
        Value::Color(c) => c.to_string(),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(value_text).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Map(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}:{}", value_text(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::EntityRef(pid) => pid.to_string(),
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

    // ----- parser ------------------------------------------------------------------

    fn kind(k: EntityKind) -> QueryExpr {
        QueryExpr::Atom(QueryAtom::Kind(k))
    }

    fn face() -> QueryExpr {
        kind(EntityKind::Face)
    }

    fn edge() -> QueryExpr {
        kind(EntityKind::Edge)
    }

    fn group() -> QueryExpr {
        kind(EntityKind::Group)
    }

    fn not(e: QueryExpr) -> QueryExpr {
        QueryExpr::Not(Box::new(e))
    }

    fn matches_(key: QueryKey, value: &str) -> QueryExpr {
        QueryExpr::Atom(QueryAtom::Match {
            key,
            value: value.to_string(),
        })
    }

    fn compare(source: &str) -> (QueryKey, CmpOp, f64) {
        match parse_query(source) {
            Ok(QueryExpr::Atom(QueryAtom::Compare { key, op, value })) => (key, op, value),
            other => panic!("{source}: {other:?}"),
        }
    }

    fn syntax_pos(source: &str) -> usize {
        match parse_query(source) {
            Err(QueryError::Syntax { pos, .. }) => pos,
            other => panic!("{source}: expected a syntax error, got {other:?}"),
        }
    }

    #[test]
    fn precedence_or_and_not() {
        use QueryExpr::{And, Or};
        assert_eq!(
            parse_query("face edge | group"),
            Ok(Or(vec![And(vec![face(), edge()]), group()]))
        );
        assert_eq!(
            parse_query("face | edge & group"),
            Ok(Or(vec![face(), And(vec![edge(), group()])]))
        );
        assert_eq!(
            parse_query("!face & edge"),
            Ok(And(vec![not(face()), edge()]))
        );
        assert_eq!(
            parse_query("!(face | edge)"),
            Ok(not(Or(vec![face(), edge()])))
        );
        assert_eq!(
            parse_query("face & (edge | group)"),
            Ok(And(vec![face(), Or(vec![edge(), group()])]))
        );
        assert_eq!(parse_query("face edge"), parse_query("face & edge"));
        assert_eq!(
            parse_query("face&edge|group"),
            parse_query("face edge | group")
        );
        assert_eq!(
            parse_query("face | edge | group"),
            Ok(Or(vec![face(), edge(), group()]))
        );
        // Parentheses keep their structure (no flattening).
        assert_eq!(
            parse_query("(face & edge) & group"),
            Ok(And(vec![And(vec![face(), edge()]), group()]))
        );
        assert_eq!(parse_query("!!face"), Ok(not(not(face()))));
        assert_eq!(parse_query("  ( ( face ) )  "), Ok(face()));
    }

    #[test]
    fn atoms_keys_and_keywords() {
        assert_eq!(parse_query("section"), Ok(kind(EntityKind::SectionPlane)));
        assert_eq!(
            parse_query("@42"),
            Ok(QueryExpr::Atom(QueryAtom::Pid(Pid(42))))
        );
        assert_eq!(
            parse_query("#Model/Kitchen/Cabinet[2]/@42 face"),
            Ok(QueryExpr::And(vec![
                QueryExpr::Atom(QueryAtom::Path("#Model/Kitchen/Cabinet[2]/@42".into())),
                face(),
            ]))
        );
        assert_eq!(
            parse_query("#Model/\"My Kitchen\"/@3|face"),
            Ok(QueryExpr::Or(vec![
                QueryExpr::Atom(QueryAtom::Path("#Model/\"My Kitchen\"/@3".into())),
                face(),
            ]))
        );
        assert_eq!(
            parse_query("layer:Walls"),
            Ok(matches_(QueryKey::Tag, "Walls"))
        );
        assert_eq!(
            parse_query("tag : Walls"),
            Ok(matches_(QueryKey::Tag, "Walls"))
        );
        assert_eq!(
            parse_query("tag:\"Wall A\""),
            Ok(matches_(QueryKey::Tag, "Wall A"))
        );
        assert_eq!(
            parse_query(r#"name:"a\"b\\c""#),
            Ok(matches_(QueryKey::Name, "a\"b\\c"))
        );
        assert_eq!(
            parse_query("name:Cab*"),
            Ok(matches_(QueryKey::Name, "Cab*"))
        );
        assert_eq!(
            parse_query("plane:xy@1200"),
            Ok(matches_(QueryKey::Plane, "xy@1200"))
        );
        assert_eq!(
            parse_query("normal:-z"),
            Ok(matches_(QueryKey::Normal, "-z"))
        );
        let attr = QueryKey::Attr {
            dict: "ext.demo".into(),
            key: "kind".into(),
        };
        assert_eq!(
            parse_query("attr.ext.demo.kind"),
            Ok(matches_(attr.clone(), ""))
        );
        assert_eq!(
            parse_query("attr.ext.demo.kind:floor"),
            Ok(matches_(attr.clone(), "floor"))
        );
        assert_eq!(compare("attr.ext.demo.kind>=2m"), (attr, CmpOp::Ge, 2000.0));
        for (word, keyword) in [
            ("all", QueryKeyword::All),
            ("selection", QueryKeyword::Selection),
            ("last", QueryKeyword::Last),
            ("solid", QueryKeyword::Solid),
            ("hidden", QueryKeyword::Hidden),
            ("visible", QueryKeyword::Visible),
            ("locked", QueryKeyword::Locked),
        ] {
            assert_eq!(
                parse_query(word),
                Ok(QueryExpr::Atom(QueryAtom::Keyword(keyword)))
            );
        }
        assert_eq!(
            parse_query("children (group) & connected(face | edge)"),
            Ok(QueryExpr::And(vec![
                QueryExpr::Atom(QueryAtom::Keyword(QueryKeyword::Children(
                    Box::new(group())
                ))),
                QueryExpr::Atom(QueryAtom::Keyword(QueryKeyword::Connected(Box::new(
                    QueryExpr::Or(vec![face(), edge()])
                )))),
            ]))
        );
        assert_eq!(
            parse_query("bounded-by(face) inside(group)"),
            Ok(QueryExpr::And(vec![
                QueryExpr::Atom(QueryAtom::Keyword(QueryKeyword::BoundedBy(
                    Box::new(face())
                ))),
                QueryExpr::Atom(QueryAtom::Keyword(QueryKeyword::Inside(Box::new(group())))),
            ]))
        );
    }

    #[test]
    fn numbers_carry_units() {
        let close = |source: &str, key: QueryKey, op: CmpOp, want: f64| {
            let (k, o, v) = compare(source);
            assert_eq!((k, o), (key, op), "{source}");
            assert!(
                (v - want).abs() <= want.abs() * 1e-12,
                "{source}: {v} != {want}"
            );
        };
        close("area>40m2", QueryKey::Area, CmpOp::Gt, 40e6);
        close("area>=1.5m²", QueryKey::Area, CmpOp::Ge, 1.5e6);
        close("area<1500cm2", QueryKey::Area, CmpOp::Lt, 150_000.0);
        close(
            "area<=12ft2",
            QueryKey::Area,
            CmpOp::Le,
            12.0 * 304.8 * 304.8,
        );
        close("area=40", QueryKey::Area, CmpOp::Eq, 40.0);
        close("area > 40mm2", QueryKey::Area, CmpOp::Gt, 40.0);
        close("length>=1.2m", QueryKey::Length, CmpOp::Ge, 1200.0);
        close("length<4'", QueryKey::Length, CmpOp::Lt, 1219.2);
        close("length>\"4' 6\\\"\"", QueryKey::Length, CmpOp::Gt, 1371.6);
        close("length=-5", QueryKey::Length, CmpOp::Eq, -5.0);
        close("length<150cm", QueryKey::Length, CmpOp::Lt, 1500.0);
        close("volume<2m3", QueryKey::Volume, CmpOp::Lt, 2e9);
        close("volume<2m³", QueryKey::Volume, CmpOp::Lt, 2e9);
        close("volume>1000cm3", QueryKey::Volume, CmpOp::Gt, 1e6);
        close("id=42", QueryKey::Id, CmpOp::Eq, 42.0);
        for bad in [
            "area>abc",
            "area>40m",
            "area>40m3",
            "area>40km2",
            "area>1e5",
            "volume>2m2",
            "length>40m2",
            "length>1,5",
            "id>1.5",
            "id>-1",
            "id>4m",
        ] {
            let text = bad.split(['>', '=']).next_back().unwrap_or_default();
            assert_eq!(
                parse_query(bad),
                Err(QueryError::InvalidNumber(text.to_string())),
                "{bad}"
            );
        }
    }

    #[test]
    fn syntax_errors_report_positions() {
        assert_eq!(syntax_pos(""), 0);
        assert_eq!(syntax_pos("   "), 3);
        assert_eq!(syntax_pos("face &"), 6);
        assert_eq!(syntax_pos("face |"), 6);
        assert_eq!(syntax_pos("(face"), 5);
        assert_eq!(syntax_pos("face)"), 4);
        assert_eq!(syntax_pos("face & )"), 7);
        assert_eq!(syntax_pos("tag:"), 4);
        assert_eq!(syntax_pos("tag:\"open"), 4);
        assert_eq!(syntax_pos("face(edge)"), 4);
        assert_eq!(syntax_pos("connected face"), 10);
        assert_eq!(syntax_pos("connected(face"), 14);
        assert_eq!(syntax_pos("@"), 1);
        assert_eq!(syntax_pos("@0"), 0);
        assert_eq!(syntax_pos("@12x"), 3);
        assert_eq!(syntax_pos("face && edge"), 6);
        assert_eq!(syntax_pos("tag>5"), 3);
        assert_eq!(syntax_pos("area:5"), 4);
        assert_eq!(syntax_pos("area"), 4);
        assert_eq!(syntax_pos("face ~"), 5);
        assert_eq!(syntax_pos("attr.x:1"), 0);
        assert_eq!(syntax_pos("attr:1"), 0);
        assert_eq!(syntax_pos("face #Mode/@1"), 6);
        assert_eq!(syntax_pos("#Model//@1"), 7);
        assert_eq!(syntax_pos("#Model/\"x"), 7);
        assert_eq!(syntax_pos(&"(".repeat(300)), 257);
        assert_eq!(
            parse_query("faces"),
            Err(QueryError::UnknownKind("faces".into()))
        );
        assert_eq!(
            parse_query("face | box"),
            Err(QueryError::UnknownKeyword("box".into()))
        );
        assert_eq!(
            parse_query("colour:red"),
            Err(QueryError::UnknownKey("colour".into()))
        );
        assert_eq!(
            parse_query("weight>5"),
            Err(QueryError::UnknownKey("weight".into()))
        );
    }

    // ----- unparser ----------------------------------------------------------------

    #[test]
    fn to_source_is_canonical() {
        for (source, canonical) in [
            ("face edge|group", "face & edge | group"),
            ("!(face|edge)", "!(face | edge)"),
            ("(face & edge) & group", "(face & edge) & group"),
            ("face & (edge | group)", "face & (edge | group)"),
            ("(face | edge) | group", "(face | edge) | group"),
            ("!(face edge)", "!(face & edge)"),
            ("!!face", "!!face"),
            ("layer:Walls", "tag:Walls"),
            ("tag:\"Wall A\"", "tag:\"Wall A\""),
            ("name:\"\"", "name:\"\""),
            (r#"name:"a\"b""#, r#"name:"a\"b""#),
            ("attr.ext.demo.kind:\"\"", "attr.ext.demo.kind"),
            ("attr.ext.demo.kind : x", "attr.ext.demo.kind:x"),
            ("area>40m2", "area>40000000"),
            ("length >= 1.5m", "length>=1500"),
            ("id=7", "id=7"),
            ("connected( face )", "connected(face)"),
            (
                "children(face|edge) hidden",
                "children(face | edge) & hidden",
            ),
            (
                "#Model/\"My Kitchen\"/@3 @4",
                "#Model/\"My Kitchen\"/@3 & @4",
            ),
        ] {
            let parsed = parse_query(source).unwrap();
            assert_eq!(parsed.to_source(), canonical, "{source}");
            assert_eq!(parse_query(canonical), Ok(parsed), "{canonical}");
        }
        // Degenerate (non-parser) shapes still print something sensible.
        assert_eq!(QueryExpr::And(vec![]).to_source(), "all");
        assert_eq!(QueryExpr::Or(vec![]).to_source(), "!all");
        assert_eq!(QueryExpr::And(vec![face()]).to_source(), "face");
        assert_eq!(
            QueryExpr::And(vec![QueryExpr::Or(vec![]), edge()]).to_source(),
            "!all & edge"
        );
    }

    fn arb_text() -> impl Strategy<Value = String> {
        "[ -~é²]{0,8}"
    }

    fn arb_dict() -> impl Strategy<Value = String> {
        "[a-z0-9_.-]{1,8}"
    }

    fn arb_key_word() -> impl Strategy<Value = String> {
        "[a-z0-9_-]{1,6}"
    }

    fn arb_number() -> impl Strategy<Value = f64> {
        prop_oneof![
            any::<f64>().prop_filter("finite", |v| v.is_finite()),
            -1e7..1e7f64,
            Just(0.0),
        ]
    }

    fn arb_op() -> impl Strategy<Value = CmpOp> {
        prop::sample::select(vec![CmpOp::Gt, CmpOp::Lt, CmpOp::Ge, CmpOp::Le, CmpOp::Eq])
    }

    fn arb_atom() -> impl Strategy<Value = QueryAtom> {
        let match_key = prop_oneof![
            Just(QueryKey::Tag),
            Just(QueryKey::Material),
            Just(QueryKey::Name),
            Just(QueryKey::Definition),
            Just(QueryKey::Plane),
            Just(QueryKey::Normal),
            (arb_dict(), arb_key_word()).prop_map(|(dict, key)| QueryKey::Attr { dict, key }),
        ];
        let measured =
            prop::sample::select(vec![QueryKey::Area, QueryKey::Length, QueryKey::Volume]);
        prop_oneof![
            prop::sample::select(EntityKind::ALL.to_vec()).prop_map(QueryAtom::Kind),
            (1u64..1_000_000).prop_map(|n| QueryAtom::Pid(Pid(n))),
            prop::sample::select(vec![
                "#Model/@1",
                "#Model/Kitchen/Cabinet[2]/@42",
                "#Model/\"My Kitchen\"/@7",
                "#Model/\"a\\\"b|c\"[1]/12",
            ])
            .prop_map(|s| QueryAtom::Path(s.to_string())),
            (match_key, arb_text()).prop_map(|(key, value)| QueryAtom::Match { key, value }),
            (measured, arb_op(), arb_number()).prop_map(|(key, op, value)| QueryAtom::Compare {
                key,
                op,
                value
            }),
            (arb_dict(), arb_key_word(), arb_op(), arb_number()).prop_map(
                |(dict, key, op, value)| QueryAtom::Compare {
                    key: QueryKey::Attr { dict, key },
                    op,
                    value,
                }
            ),
            (arb_op(), 0u64..(1u64 << 53)).prop_map(|(op, n)| QueryAtom::Compare {
                key: QueryKey::Id,
                op,
                value: n as f64,
            }),
            prop::sample::select(vec![
                QueryKeyword::All,
                QueryKeyword::Selection,
                QueryKeyword::Last,
                QueryKeyword::Solid,
                QueryKeyword::Hidden,
                QueryKeyword::Visible,
                QueryKeyword::Locked,
            ])
            .prop_map(QueryAtom::Keyword),
        ]
    }

    fn arb_expr() -> impl Strategy<Value = QueryExpr> {
        arb_atom()
            .prop_map(QueryExpr::Atom)
            .prop_recursive(4, 48, 4, |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 2..4).prop_map(QueryExpr::Or),
                    prop::collection::vec(inner.clone(), 2..4).prop_map(QueryExpr::And),
                    inner.clone().prop_map(|e| QueryExpr::Not(Box::new(e))),
                    (0..4u8, inner).prop_map(|(f, e)| {
                        let e = Box::new(e);
                        QueryExpr::Atom(QueryAtom::Keyword(match f {
                            0 => QueryKeyword::Connected(e),
                            1 => QueryKeyword::BoundedBy(e),
                            2 => QueryKeyword::Inside(e),
                            _ => QueryKeyword::Children(e),
                        }))
                    }),
                ]
            })
    }

    /// Query-like text: random ASCII, or a soup of query tokens (which parses
    /// far more often).
    fn arb_source() -> impl Strategy<Value = String> {
        let tokens = prop::sample::select(vec![
            "face",
            "edge",
            "group",
            " ",
            " ",
            "|",
            "&",
            "!",
            "(",
            ")",
            "tag",
            "layer",
            ":",
            "x",
            "\"a b\"",
            "\"",
            "\\",
            "area",
            "length",
            "id",
            ">",
            "<",
            "=",
            ">=",
            "40m2",
            "1.5m",
            "-2",
            "4'",
            "@3",
            "@",
            "#Model/@1",
            "#Model/\"K x\"[1]/@2",
            "connected(",
            "children(",
            "inside",
            "attr.d.k",
            "attr.",
            "all",
            "selection",
            "plane",
            "xy@12",
            "name",
            "Cab*",
            "é",
            "²",
        ]);
        prop_oneof![
            "[ -~]{0,24}",
            prop::collection::vec(tokens, 0..14).prop_map(|parts| parts.concat()),
        ]
    }

    proptest! {
        #[test]
        fn to_source_round_trips(expr in arb_expr()) {
            let text = expr.to_source();
            prop_assert_eq!(parse_query(&text), Ok(expr), "{}", text);
        }

        #[test]
        fn parser_never_panics(source in arb_source()) {
            if let Ok(expr) = parse_query(&source) {
                prop_assert_eq!(parse_query(&expr.to_source()), Ok(expr));
            }
        }
    }

    // ----- evaluation fixture --------------------------------------------------------

    use proptest::prelude::*;

    use crate::document::attributes::AttributeDictionaries;
    use crate::document::entities::{ComponentInstance, Group};
    use crate::document::hierarchy::EntityRecord;
    use crate::document::materials::{Material, Rgba8};
    use crate::document::tags::Tag;
    use crate::geom::NoDeltas;
    use crate::ids::{DefinitionId, FaceId};
    use crate::units::Length;

    /// Model: a 1000 × 2000 rectangle at Z = 0 (four edges and a face tagged
    /// `Walls`, front material `Brick`, attribute `ext.demo/kind = floor`), a
    /// hidden 500 mm dangling edge, a guide point inside the rectangle, the
    /// locked group `Kitchen` (scaled ×2, holding a 100 mm cube and four
    /// `Cabinet` instances, the last named `Pantry`), the instance `Left` of
    /// `Cabinet` on the invisible tag `Secret` with a `Brick` override, and a
    /// 100 mm edge without a pid. `Cabinet` holds one 600 mm edge.
    struct Fixture {
        doc: Document,
        rect_edges: Vec<Pid>,
        face: Pid,
        dangling: Pid,
        guide: Pid,
        kitchen: Pid,
        left: Pid,
        loose: EntityId,
        cube_faces: Vec<Pid>,
        cabinets: Vec<Pid>,
        cabinet_edge: Pid,
    }

    fn p(x: f64, y: f64, z: f64) -> DVec3 {
        DVec3::new(x, y, z)
    }

    fn instance(
        definition: DefinitionId,
        transform: DAffine3,
        name: Option<&str>,
    ) -> ComponentInstance {
        ComponentInstance {
            definition,
            transform,
            name: name.map(str::to_string),
            material_override: None,
            glued_to: None,
            dynamic_attrs: AttributeDictionaries::default(),
        }
    }

    fn add_cube(entities: &mut Entities, size: f64, tol: &Tolerances) {
        let c = |i: u8| {
            p(
                if i & 1 != 0 { size } else { 0.0 },
                if i & 2 != 0 { size } else { 0.0 },
                if i & 4 != 0 { size } else { 0.0 },
            )
        };
        for (a, b) in [
            (0, 1),
            (1, 3),
            (3, 2),
            (2, 0),
            (4, 5),
            (5, 7),
            (7, 6),
            (6, 4),
            (0, 4),
            (1, 5),
            (3, 7),
            (2, 6),
        ] {
            entities
                .mesh_mut()
                .add_edge(c(a), c(b), tol, &mut NoDeltas)
                .unwrap();
        }
    }

    fn fixture() -> Fixture {
        let mut doc = Document::new();
        let tol = *doc.tolerances();
        let walls = doc
            .tags
            .insert(Tag::new("Walls", Rgba8::rgb(200, 0, 0)))
            .unwrap();
        let mut secret = Tag::new("Secret", Rgba8::BLACK);
        secret.visible = false;
        let secret = doc.tags.insert(secret).unwrap();
        let brick = doc
            .materials
            .insert(Material::solid("Brick", Rgba8::rgb(150, 60, 40)))
            .unwrap();

        // The rectangle.
        let corners = [
            p(0.0, 0.0, 0.0),
            p(1000.0, 0.0, 0.0),
            p(1000.0, 2000.0, 0.0),
            p(0.0, 2000.0, 0.0),
        ];
        let mut edges = Vec::new();
        let mut face = None;
        for i in 0..4 {
            let r = doc
                .model
                .mesh_mut()
                .add_edge(corners[i], corners[(i + 1) % 4], &tol, &mut NoDeltas)
                .unwrap();
            edges.extend(r.edges);
            face = face.or(r.new_faces.first().copied());
        }
        let mut rect_edges = Vec::new();
        for e in edges {
            let pid = doc.allocate_pid();
            doc.model.bind_pid(EntityId::Edge(e), pid, None).unwrap();
            rect_edges.push(pid);
        }
        let face = face.unwrap();
        let face_id = EntityId::Face(face);
        let face_pid = doc.allocate_pid();
        doc.model.bind_pid(face_id, face_pid, None).unwrap();
        let mut sides = doc.model.mesh().face(face).unwrap().sides.clone();
        sides.material_front = Some(brick);
        doc.model
            .mesh_mut()
            .set_face_sides(face, sides, &mut NoDeltas)
            .unwrap();
        doc.model.set_tag(face_id, Some(walls)).unwrap();
        doc.model
            .attrs_mut(face_id)
            .unwrap()
            .set("ext.demo", "kind", Value::Str("floor".into()));

        // The hidden dangling edge, the guide and the edge without a pid.
        let r = doc
            .model
            .mesh_mut()
            .add_edge(
                p(3000.0, 0.0, 0.0),
                p(3000.0, 0.0, 500.0),
                &tol,
                &mut NoDeltas,
            )
            .unwrap();
        let dangling_id = EntityId::Edge(r.edges[0]);
        let dangling = doc.allocate_pid();
        doc.model.bind_pid(dangling_id, dangling, None).unwrap();
        doc.model.set_hidden(dangling_id, true).unwrap();
        let guide = doc.allocate_pid();
        doc.model
            .insert(
                EntityRecord::Guide(Guide::Point {
                    pos: p(500.0, 1000.0, 0.0),
                }),
                guide,
            )
            .unwrap();
        let r = doc
            .model
            .mesh_mut()
            .add_edge(
                p(5000.0, 0.0, 0.0),
                p(5000.0, 100.0, 0.0),
                &tol,
                &mut NoDeltas,
            )
            .unwrap();
        let loose = EntityId::Edge(r.edges[0]);

        // Cabinet.
        let cabinet = doc.add_definition(ComponentDefinition::new(uuid::Uuid::nil(), "Cabinet"));
        let r = doc.definitions[cabinet]
            .entities
            .mesh_mut()
            .add_edge(p(0.0, 0.0, 0.0), p(600.0, 0.0, 0.0), &tol, &mut NoDeltas)
            .unwrap();
        let cabinet_edge = doc.allocate_pid();
        doc.definitions[cabinet]
            .entities
            .bind_pid(EntityId::Edge(r.edges[0]), cabinet_edge, None)
            .unwrap();

        // Kitchen.
        let mut kitchen_def = ComponentDefinition::new(uuid::Uuid::nil(), "Kitchen#1");
        kitchen_def.is_internal_group = true;
        let kitchen_def = doc.add_definition(kitchen_def);
        add_cube(&mut doc.definitions[kitchen_def].entities, 100.0, &tol);
        let cube: Vec<FaceId> = doc.definitions[kitchen_def]
            .entities
            .mesh()
            .faces()
            .map(|(f, _)| f)
            .collect();
        let mut cube_faces = Vec::new();
        for f in cube {
            let pid = doc.allocate_pid();
            doc.definitions[kitchen_def]
                .entities
                .bind_pid(EntityId::Face(f), pid, None)
                .unwrap();
            cube_faces.push(pid);
        }
        let mut cabinets = Vec::new();
        for (i, name) in [None, None, None, Some("Pantry")].into_iter().enumerate() {
            let pid = doc.allocate_pid();
            let at = DAffine3::from_translation(p(0.0, 1000.0 * i as f64, 0.0));
            doc.definitions[kitchen_def]
                .entities
                .insert(EntityRecord::Instance(instance(cabinet, at, name)), pid)
                .unwrap();
            cabinets.push(pid);
        }
        let kitchen = doc.allocate_pid();
        let kitchen_id = doc
            .model
            .insert(
                EntityRecord::Group(Group {
                    definition: kitchen_def,
                    transform: DAffine3::from_translation(p(10_000.0, 0.0, 0.0))
                        * DAffine3::from_scale(DVec3::splat(2.0)),
                    name: "Kitchen".into(),
                }),
                kitchen,
            )
            .unwrap();
        doc.model.set_locked(kitchen_id, true).unwrap();
        doc.model.attrs_mut(kitchen_id).unwrap().set(
            "ext.demo",
            "area",
            Value::Length(Length::mm(1200.0)),
        );

        // Left.
        let left = doc.allocate_pid();
        let mut left_instance = instance(
            cabinet,
            DAffine3::from_translation(p(0.0, 5000.0, 0.0)),
            Some("Left"),
        );
        left_instance.material_override = Some(brick);
        let left_id = doc
            .model
            .insert(EntityRecord::Instance(left_instance), left)
            .unwrap();
        doc.model.set_tag(left_id, Some(secret)).unwrap();

        Fixture {
            doc,
            rect_edges,
            face: face_pid,
            dangling,
            guide,
            kitchen,
            left,
            loose,
            cube_faces,
            cabinets,
            cabinet_edge,
        }
    }

    impl Fixture {
        /// The model paths of `pids`.
        fn model(&self, pids: &[Pid]) -> Vec<EntityPath> {
            pids.iter()
                .map(|pid| EntityPath::in_model(EntityKey::Pid(*pid)))
                .collect()
        }

        fn loose_path(&self) -> EntityPath {
            EntityPath::in_model(EntityKey::Id(self.loose))
        }

        fn kitchen_path(&self) -> InstancePath {
            InstancePath::root().child(EntityKey::Pid(self.kitchen))
        }

        fn eval_with(
            &self,
            source: &str,
            context: &EditContext,
            selection: &Selection,
            last: &[Pid],
        ) -> Result<Vec<EntityPath>, QueryError> {
            let env = QueryEnv {
                selection,
                last,
                context,
            };
            let expr = parse_query(source).unwrap_or_else(|e| panic!("{source}: {e}"));
            Ok(expr.eval(&self.doc, &env)?.iter().cloned().collect())
        }

        fn eval(&self, source: &str) -> Vec<EntityPath> {
            self.eval_with(source, &EditContext::model(), &Selection::new(), &[])
                .unwrap_or_else(|e| panic!("{source}: {e}"))
        }

        /// Every model entity in container order.
        fn universe(&self) -> Vec<EntityPath> {
            let mut all = self.model(&self.rect_edges);
            all.extend(self.model(&[
                self.face,
                self.dangling,
                self.guide,
                self.kitchen,
                self.left,
            ]));
            all.push(self.loose_path());
            all
        }
    }

    // ----- evaluation ----------------------------------------------------------------

    #[test]
    fn kinds_pids_and_container_order() {
        let fx = fixture();
        let mut edges = fx.model(&fx.rect_edges);
        edges.extend(fx.model(&[fx.dangling]));
        edges.push(fx.loose_path());
        assert_eq!(fx.eval("edge"), edges);
        assert_eq!(fx.eval("face"), fx.model(&[fx.face]));
        assert_eq!(fx.eval("group"), fx.model(&[fx.kitchen]));
        assert_eq!(fx.eval("instance"), fx.model(&[fx.left]));
        assert_eq!(fx.eval("guide"), fx.model(&[fx.guide]));
        assert!(fx.eval("vertex").is_empty());
        assert!(fx.eval("text | dimension | image | section").is_empty());
        assert_eq!(fx.eval("all"), fx.universe());
        assert_eq!(
            fx.eval("!edge"),
            fx.model(&[fx.face, fx.guide, fx.kitchen, fx.left])
        );
        assert!(fx.eval("edge face").is_empty());
        // Order follows the container, not the query.
        assert_eq!(fx.eval("guide | face"), fx.model(&[fx.face, fx.guide]));
        assert_eq!(
            fx.eval(&format!("@{}", fx.face.raw())),
            fx.model(&[fx.face])
        );
        assert!(fx.eval("@999").is_empty());
        assert_eq!(
            fx.eval(&format!("#Model/@{}", fx.face.raw())),
            fx.model(&[fx.face])
        );
    }

    #[test]
    fn tags_materials_names_and_definitions() {
        let fx = fixture();
        assert_eq!(fx.eval("tag:Walls"), fx.model(&[fx.face]));
        assert_eq!(fx.eval("layer:Walls"), fx.model(&[fx.face]));
        assert!(fx.eval("tag:Nope").is_empty());
        let mut untagged = fx.model(&fx.rect_edges);
        untagged.extend(fx.model(&[fx.dangling, fx.guide, fx.kitchen]));
        untagged.push(fx.loose_path());
        assert_eq!(fx.eval("tag:Untagged"), untagged);
        assert_eq!(fx.eval("tag:Secret"), fx.model(&[fx.left]));

        assert_eq!(fx.eval("material:Brick"), fx.model(&[fx.face, fx.left]));
        assert_eq!(fx.eval("material:\"Default Back\""), fx.model(&[fx.face]));
        assert!(fx.eval("material:Default").is_empty());
        assert!(fx.eval("material:Nope").is_empty());

        assert_eq!(fx.eval("name:Kit*"), fx.model(&[fx.kitchen]));
        assert_eq!(fx.eval("name:Cabinet"), fx.model(&[fx.left]));
        assert_eq!(fx.eval("name:L*t"), fx.model(&[fx.left]));
        assert_eq!(fx.eval("name:*"), fx.model(&[fx.kitchen, fx.left]));
        assert_eq!(fx.eval("name:\"Kitchen#1\""), fx.model(&[fx.kitchen]));
        assert!(fx.eval("name:kitchen").is_empty());
        assert!(fx.eval("name:Kit").is_empty());
        assert_eq!(fx.eval("definition:Cabinet"), fx.model(&[fx.left]));
        assert!(fx.eval("definition:Cab").is_empty());
    }

    #[test]
    fn attributes() {
        let fx = fixture();
        assert_eq!(fx.eval("attr.ext.demo.kind"), fx.model(&[fx.face]));
        assert_eq!(fx.eval("attr.ext.demo.kind:floor"), fx.model(&[fx.face]));
        assert!(fx.eval("attr.ext.demo.kind:wall").is_empty());
        assert!(fx.eval("attr.ext.other.kind").is_empty());
        assert_eq!(fx.eval("attr.ext.demo.area:1200"), fx.model(&[fx.kitchen]));
        assert_eq!(fx.eval("attr.ext.demo.area:1.2m"), fx.model(&[fx.kitchen]));
        assert_eq!(
            fx.eval("attr.ext.demo.area:1200mm"),
            fx.model(&[fx.kitchen])
        );
        assert_eq!(fx.eval("attr.ext.demo.area>1m"), fx.model(&[fx.kitchen]));
        assert!(fx.eval("attr.ext.demo.area<1m").is_empty());
        assert!(fx.eval("attr.ext.demo.kind>1").is_empty());
        assert_eq!(
            value_text(&Value::Array(vec![Value::Int(1), Value::Bool(true)])),
            "[1,true]"
        );
    }

    #[test]
    fn planes_normals_and_measures() {
        let fx = fixture();
        for q in [
            "plane:xy",
            "plane:xy@0",
            "plane:YX@0mm",
            "normal:z",
            "normal:+Z",
        ] {
            assert_eq!(fx.eval(q), fx.model(&[fx.face]), "{q}");
        }
        for q in [
            "plane:xy@1200",
            "plane:xz",
            "plane:yz",
            "normal:-z",
            "normal:x",
        ] {
            assert!(fx.eval(q).is_empty(), "{q}");
        }
        assert_eq!(fx.eval("area=2m2"), fx.model(&[fx.face]));
        assert_eq!(fx.eval("area>1m2"), fx.model(&[fx.face]));
        assert_eq!(fx.eval("area<=2000000"), fx.model(&[fx.face]));
        assert!(fx.eval("area<2000000").is_empty());
        assert!(fx.eval("area>2m²").is_empty());
        let (e0, e1, e2, e3) = (
            fx.rect_edges[0],
            fx.rect_edges[1],
            fx.rect_edges[2],
            fx.rect_edges[3],
        );
        assert_eq!(fx.eval("length=1000"), fx.model(&[e0, e2]));
        assert_eq!(fx.eval("length>=2m"), fx.model(&[e1, e3]));
        let mut short = fx.model(&[fx.dangling]);
        short.push(fx.loose_path());
        assert_eq!(fx.eval("length<600"), short);
        assert_eq!(fx.eval("length=0.5m"), fx.model(&[fx.dangling]));
        // 100 mm cube scaled by 2.
        assert_eq!(fx.eval("volume=8000000"), fx.model(&[fx.kitchen]));
        assert_eq!(fx.eval("volume>1000cm3"), fx.model(&[fx.kitchen]));
        assert!(fx.eval("volume>1m3").is_empty());
        assert_eq!(
            fx.eval(&format!("id={}", fx.face.raw())),
            fx.model(&[fx.face])
        );
        assert_eq!(
            fx.eval(&format!("id>{}", fx.cabinets[3].raw())),
            fx.model(&[fx.kitchen, fx.left])
        );
        assert_eq!(fx.eval("id>0").len(), 9);
    }

    #[test]
    fn state_keywords() {
        let fx = fixture();
        assert_eq!(fx.eval("solid"), fx.model(&[fx.kitchen]));
        assert_eq!(fx.eval("locked"), fx.model(&[fx.kitchen]));
        assert_eq!(fx.eval("hidden"), fx.model(&[fx.dangling, fx.left]));
        let mut visible = fx.model(&fx.rect_edges);
        visible.extend(fx.model(&[fx.face, fx.guide, fx.kitchen]));
        visible.push(fx.loose_path());
        assert_eq!(fx.eval("visible"), visible);

        let face_id = fx.doc.model.id_of(fx.face).unwrap();
        let deep_face = EntityPath::new(fx.kitchen_path(), EntityKey::Pid(fx.cube_faces[0]));
        let selection = Selection::from_iter([
            deep_face.clone(),
            fx.loose_path(),
            // Id-addressed items are normalized to their pid.
            EntityPath::in_model(EntityKey::Id(face_id)),
        ]);
        let model = EditContext::model();
        let mut expected = fx.model(&[fx.face]);
        expected.push(fx.loose_path());
        assert_eq!(
            fx.eval_with("selection", &model, &selection, &[]).unwrap(),
            expected
        );
        assert_eq!(
            fx.eval_with("children(group) & selection", &model, &selection, &[])
                .unwrap(),
            vec![deep_face]
        );
        assert!(
            fx.eval_with("selection", &model, &Selection::new(), &[])
                .unwrap()
                .is_empty()
        );
        let last = [fx.kitchen, fx.guide, Pid(999)];
        assert_eq!(
            fx.eval_with("last", &model, &Selection::new(), &last)
                .unwrap(),
            fx.model(&[fx.guide, fx.kitchen])
        );
        assert_eq!(
            fx.eval_with("last & group", &model, &Selection::new(), &last)
                .unwrap(),
            fx.model(&[fx.kitchen])
        );
    }

    #[test]
    fn connected_bounded_by_and_inside() {
        let fx = fixture();
        let mut rect = fx.model(&fx.rect_edges);
        assert_eq!(fx.eval("bounded-by(face)"), rect);
        assert!(fx.eval("bounded-by(edge)").is_empty());
        rect.extend(fx.model(&[fx.face]));
        assert_eq!(fx.eval("connected(face)"), rect);
        assert_eq!(
            fx.eval(&format!("connected(@{})", fx.rect_edges[2].raw())),
            rect
        );
        assert_eq!(
            fx.eval(&format!("connected(@{})", fx.dangling.raw())),
            fx.model(&[fx.dangling])
        );
        assert!(fx.eval("connected(group)").is_empty());
        let mut mesh = rect.clone();
        mesh.extend(fx.model(&[fx.dangling]));
        mesh.push(fx.loose_path());
        assert_eq!(fx.eval("connected(edge)"), mesh);
        assert_eq!(fx.eval("connected(face) & edge"), fx.model(&fx.rect_edges));

        let mut inside_face = fx.model(&fx.rect_edges);
        inside_face.extend(fx.model(&[fx.guide]));
        assert_eq!(fx.eval("inside(face)"), inside_face);
        assert_eq!(fx.eval("inside(edge)"), fx.model(&[fx.face, fx.guide]));
        assert!(fx.eval("inside(group)").is_empty());
        assert!(fx.eval("inside(text)").is_empty());
    }

    #[test]
    fn children_go_one_level_deeper() {
        let fx = fixture();
        let kitchen = fx.kitchen_path();
        let children = fx.eval("children(group)");
        assert_eq!(children.len(), 6 + 4 + 12);
        assert!(children.iter().all(|p| p.instances == kitchen));
        let deep = |pid: Pid| EntityPath::new(kitchen.clone(), EntityKey::Pid(pid));
        assert_eq!(
            children[..10],
            fx.cube_faces
                .iter()
                .chain(&fx.cabinets)
                .map(|pid| deep(*pid))
                .collect::<Vec<_>>()[..]
        );
        assert!(
            children[10..]
                .iter()
                .all(|p| matches!(p.target, EntityKey::Id(EntityId::Edge(_))))
        );
        // Atoms filter the occurrences `children` brings into view.
        let faces: Vec<EntityPath> = fx.cube_faces.iter().map(|pid| deep(*pid)).collect();
        assert_eq!(fx.eval("children(group) & face"), faces);
        assert_eq!(fx.eval("children(name:Kitchen) & area=40000"), faces);
        assert!(fx.eval("children(group) & area=10000").is_empty());
        assert_eq!(fx.eval("children(group) & edge & length=200").len(), 12);
        assert_eq!(fx.eval("children(group) & solid").len(), 0);
        let top = fx.eval("children(group) & plane:xy@200");
        assert_eq!(top.len(), 1);
        assert!(fx.eval("children(group) & plane:xy@100").is_empty());
        assert_eq!(fx.eval("children(group) & (normal:z | normal:-z)").len(), 2);
        assert_eq!(
            fx.eval("children(group) & instance & name:Pantry"),
            vec![deep(fx.cabinets[3])]
        );
        assert_eq!(fx.eval("children(group) & instance & name:Cab*").len(), 4);
        // `!` complements within the domain: deeper occurrences drop out.
        assert_eq!(fx.eval("!children(group)"), fx.universe());
        // Pre-order: the group's contents come right after it.
        let mixed = fx.eval("children(group) | face | instance");
        assert_eq!(mixed.len(), 2 + 22);
        assert_eq!(mixed[0], fx.model(&[fx.face])[0]);
        assert_eq!(mixed[1].instances, kitchen);
        assert_eq!(mixed[23], fx.model(&[fx.left])[0]);

        let grandchildren = fx.eval("children(children(group))");
        let expected: Vec<EntityPath> = fx
            .cabinets
            .iter()
            .map(|c| {
                EntityPath::new(
                    kitchen.child(EntityKey::Pid(*c)),
                    EntityKey::Pid(fx.cabinet_edge),
                )
            })
            .collect();
        assert_eq!(grandchildren, expected);
        assert_eq!(fx.eval("children(children(group)) & length=1200"), expected);
        assert_eq!(
            fx.eval("children(instance)"),
            vec![EntityPath::new(
                InstancePath::root().child(EntityKey::Pid(fx.left)),
                EntityKey::Pid(fx.cabinet_edge)
            )]
        );
        assert_eq!(fx.eval("connected(children(children(group)))"), expected);
        // A `#path` brings its occurrence into view.
        let path = format!(
            "#Model/Kitchen/Cabinet[1]/@{} | face",
            fx.cabinet_edge.raw()
        );
        assert_eq!(
            fx.eval(&path),
            vec![fx.model(&[fx.face])[0].clone(), expected[1].clone()]
        );
        assert!(fx.eval("#Model/Nope/@1").is_empty());
        assert!(fx.eval("#Model/Kitchen/Cabinet[7]").is_empty());
    }

    #[test]
    fn evaluation_follows_the_edit_context() {
        let fx = fixture();
        let kitchen = fx.kitchen_path();
        let context = EditContext {
            path: kitchen.clone(),
        };
        let none = Selection::new();
        let run = |q: &str| fx.eval_with(q, &context, &none, &[]).unwrap();
        let deep = |pid: Pid| EntityPath::new(kitchen.clone(), EntityKey::Pid(pid));
        assert_eq!(
            run("face"),
            fx.cube_faces.iter().map(|p| deep(*p)).collect::<Vec<_>>()
        );
        assert_eq!(
            run("instance"),
            fx.cabinets.iter().map(|p| deep(*p)).collect::<Vec<_>>()
        );
        assert_eq!(run("edge").len(), 12);
        assert_eq!(run("area=10000").len(), 6);
        assert_eq!(run("plane:xy@100").len(), 1);
        assert_eq!(run("children(instance)").len(), 4);
        assert_eq!(run("name:Pantry"), vec![deep(fx.cabinets[3])]);
        assert_eq!(run("connected(face)").len(), 18);
        // The bottom face's own edges, nothing else.
        assert_eq!(run("inside(face & plane:xy@0)").len(), 4);

        let selection =
            Selection::from_iter([fx.model(&[fx.face])[0].clone(), deep(fx.cube_faces[2])]);
        assert_eq!(
            fx.eval_with("selection", &context, &selection, &[])
                .unwrap(),
            vec![deep(fx.cube_faces[2])]
        );

        let left = EditContext {
            path: InstancePath::root().child(EntityKey::Pid(fx.left)),
        };
        assert_eq!(
            fx.eval_with("all", &left, &none, &[]).unwrap(),
            vec![EntityPath::new(
                left.path.clone(),
                EntityKey::Pid(fx.cabinet_edge)
            )]
        );
        let bad = EditContext {
            path: InstancePath::root().child(EntityKey::Pid(Pid(999))),
        };
        assert!(matches!(
            fx.eval_with("all", &bad, &none, &[]),
            Err(QueryError::PathNotFound(_))
        ));
    }

    #[test]
    fn malformed_values_fail_evaluation() {
        let fx = fixture();
        let env_selection = Selection::new();
        let context = EditContext::model();
        let env = QueryEnv {
            selection: &env_selection,
            last: &[],
            context: &context,
        };
        for q in [
            "plane:bogus",
            "plane:xy@abc",
            "normal:q",
            "face | normal:zz",
        ] {
            assert!(
                matches!(
                    parse_query(q).unwrap().eval(&fx.doc, &env),
                    Err(QueryError::Eval(_))
                ),
                "{q}"
            );
        }
        let atoms = [
            QueryAtom::Match {
                key: QueryKey::Area,
                value: "5".into(),
            },
            QueryAtom::Compare {
                key: QueryKey::Tag,
                op: CmpOp::Eq,
                value: 1.0,
            },
            QueryAtom::Compare {
                key: QueryKey::Length,
                op: CmpOp::Gt,
                value: f64::NAN,
            },
            QueryAtom::Path("#Model/\"open".into()),
        ];
        for atom in atoms {
            assert!(
                matches!(
                    QueryExpr::Atom(atom.clone()).eval(&fx.doc, &env),
                    Err(QueryError::Eval(_))
                ),
                "{atom:?}"
            );
        }
    }

    // ----- paths -----------------------------------------------------------------------

    #[test]
    fn paths_parse_names_indices_and_pids() {
        let fx = fixture();
        let doc = &fx.doc;
        let kitchen = fx.kitchen_path();
        let edge_in = |cabinet: usize| {
            EntityPath::new(
                kitchen.child(EntityKey::Pid(fx.cabinets[cabinet])),
                EntityKey::Pid(fx.cabinet_edge),
            )
        };
        let e = fx.cabinet_edge.raw();
        assert_eq!(
            EntityPath::parse(&format!("#Model/Kitchen/Cabinet[2]/@{e}"), doc),
            Ok(edge_in(2))
        );
        assert_eq!(
            EntityPath::parse(&format!("Model/Kitchen/Cabinet/@{e}"), doc),
            Ok(edge_in(0))
        );
        assert_eq!(
            EntityPath::parse(&format!("#Model/Kitchen/Cabinet[0]/{e}"), doc),
            Ok(edge_in(0))
        );
        assert_eq!(
            EntityPath::parse(&format!("#Model/Kitchen/Pantry/@{e}"), doc),
            Ok(edge_in(3))
        );
        assert_eq!(
            EntityPath::parse(
                &format!("#Model/@{}/{}/@{e}", fx.kitchen.raw(), fx.cabinets[1].raw()),
                doc
            ),
            Ok(edge_in(1))
        );
        assert_eq!(
            EntityPath::parse("#Model/\"Kitchen\"/Cabinet[1]", doc),
            Ok(EntityPath::new(
                kitchen.clone(),
                EntityKey::Pid(fx.cabinets[1])
            ))
        );
        assert_eq!(
            EntityPath::parse("#Model/Kitchen", doc),
            Ok(EntityPath::in_model(EntityKey::Pid(fx.kitchen)))
        );
        assert_eq!(
            EntityPath::parse("#Model/Left", doc),
            Ok(EntityPath::in_model(EntityKey::Pid(fx.left)))
        );

        let syntax_at = |text: &str| match EntityPath::parse(text, doc) {
            Err(QueryError::Syntax { pos, .. }) => pos,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(syntax_at("#Mode/x"), 1);
        assert_eq!(syntax_at("#Model"), 6);
        assert_eq!(syntax_at("#Modelx"), 6);
        assert_eq!(syntax_at("#Model//x"), 7);
        assert_eq!(syntax_at("#Model/x/"), 9);
        assert_eq!(syntax_at("#Model/@0"), 7);
        assert_eq!(syntax_at("#Model/@x"), 7);
        assert_eq!(syntax_at("#Model/Cab[x]"), 11);
        assert_eq!(syntax_at("#Model/[1]"), 7);
        assert_eq!(syntax_at("#Model/a\"b"), 8);
        assert_eq!(syntax_at("#Model/\"open"), 7);
        assert_eq!(syntax_at("#Model/\"a\"b"), 10);
        assert_eq!(syntax_at("#Model/\"a\"[1"), 10);

        for text in [
            "#Model/Kitchen/Cabinet[3]".to_string(),
            "#Model/Nope".to_string(),
            "#Model/@999".to_string(),
            format!("#Model/@{}/@1", fx.face.raw()),
            format!("#Model/Kitchen/@{}", fx.face.raw()),
        ] {
            assert!(
                matches!(
                    EntityPath::parse(&text, doc),
                    Err(QueryError::PathNotFound(_))
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn paths_display_with_names_and_round_trip() {
        let fx = fixture();
        let doc = &fx.doc;
        let kitchen = fx.kitchen_path();
        let e = fx.cabinet_edge.raw();
        let edge_in = |cabinet: usize| {
            EntityPath::new(
                kitchen.child(EntityKey::Pid(fx.cabinets[cabinet])),
                EntityKey::Pid(fx.cabinet_edge),
            )
        };
        assert_eq!(
            edge_in(0).display_with(doc),
            format!("#Model/Kitchen/Cabinet/@{e}")
        );
        assert_eq!(
            edge_in(2).display_with(doc),
            format!("#Model/Kitchen/Cabinet[2]/@{e}")
        );
        assert_eq!(
            edge_in(3).display_with(doc),
            format!("#Model/Kitchen/Pantry/@{e}")
        );
        assert_eq!(
            EntityPath::in_model(EntityKey::Pid(fx.left)).display_with(doc),
            "#Model/Left"
        );
        assert_eq!(
            EntityPath::in_model(EntityKey::Pid(fx.face)).display_with(doc),
            format!("#Model/@{}", fx.face.raw())
        );
        assert!(
            fx.loose_path()
                .display_with(doc)
                .starts_with("#Model/EdgeId#")
        );
        assert_eq!(
            EntityPath::new(
                InstancePath::root().child(EntityKey::Pid(Pid(999))),
                EntityKey::Pid(Pid(5))
            )
            .display_with(doc),
            "#Model/@999/@5"
        );

        // Every pid-addressed occurrence three levels deep round-trips.
        let all = fx.eval("all | children(all) | children(children(all))");
        assert_eq!(all.len(), 10 + (22 + 1) + 4);
        for path in all {
            if matches!(path.target, EntityKey::Pid(_)) {
                let text = path.display_with(doc);
                assert_eq!(EntityPath::parse(&text, doc), Ok(path), "{text}");
            }
        }
    }

    #[test]
    fn path_names_with_special_characters_are_quoted() {
        let mut doc = Document::new();
        let names = [
            "Living room",
            "Living room",
            "42",
            "@home",
            "a/b",
            "x[1]",
            "",
            "q\"t",
        ];
        let mut pids = Vec::new();
        for name in names {
            let mut def = ComponentDefinition::new(uuid::Uuid::nil(), "");
            def.is_internal_group = true;
            let definition = doc.add_definition(def);
            let pid = doc.allocate_pid();
            doc.model
                .insert(
                    EntityRecord::Group(Group {
                        definition,
                        transform: DAffine3::IDENTITY,
                        name: name.to_string(),
                    }),
                    pid,
                )
                .unwrap();
            pids.push(pid);
        }
        let shown: Vec<String> = pids
            .iter()
            .map(|pid| EntityPath::in_model(EntityKey::Pid(*pid)).display_with(&doc))
            .collect();
        assert_eq!(
            shown,
            [
                "#Model/\"Living room\"".to_string(),
                "#Model/\"Living room\"[1]".to_string(),
                "#Model/\"42\"".to_string(),
                "#Model/\"@home\"".to_string(),
                "#Model/\"a/b\"".to_string(),
                "#Model/\"x[1]\"".to_string(),
                format!("#Model/@{}", pids[6].raw()),
                "#Model/\"q\\\"t\"".to_string(),
            ]
        );
        for (text, pid) in shown.iter().zip(&pids) {
            assert_eq!(
                EntityPath::parse(text, &doc),
                Ok(EntityPath::in_model(EntityKey::Pid(*pid))),
                "{text}"
            );
            // Paths embed in queries as they are displayed.
            let selection = Selection::new();
            let context = EditContext::model();
            let env = QueryEnv {
                selection: &selection,
                last: &[],
                context: &context,
            };
            let found = parse_query(&format!("{text} | text"))
                .unwrap()
                .eval(&doc, &env)
                .unwrap();
            assert_eq!(
                found.iter().cloned().collect::<Vec<_>>(),
                vec![EntityPath::in_model(EntityKey::Pid(*pid))],
                "{text}"
            );
        }
        // An unquoted name may contain spaces outside queries.
        assert_eq!(
            EntityPath::parse("#Model/Living room[1]", &doc),
            Ok(EntityPath::in_model(EntityKey::Pid(pids[1])))
        );
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*", ""));
        assert!(glob_match("Cab*", "Cabinet"));
        assert!(glob_match("*net", "Cabinet"));
        assert!(glob_match("C*b*t", "Cabinet"));
        assert!(glob_match("**", "x"));
        assert!(!glob_match("Cab", "Cabinet"));
        assert!(!glob_match("C*x", "Cabinet"));
        assert!(glob_match("Cabinet", "Cabinet"));
        assert!(glob_match("a*a*a", "aaaa"));
        assert!(!glob_match("a*a*a", "aa"));
    }
}
