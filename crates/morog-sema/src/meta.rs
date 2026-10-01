//! Typed STA frontmatter (plan §2.1–2.3).
//!
//! [`parse_note_meta`] converts the span-preserving [`Frontmatter`] node
//! that morg delivers into the strict per-kind schemas. The conversion is
//! explicit (the YAML value is `saphyr::YamlOwned`, not serde) and
//! **lenient**: it always produces a best-effort [`NoteMeta`] plus a list of
//! [`MetaError`]s carrying the spans of the offending keys and values, so
//! every schema violation becomes a diagnostic instead of aborting the file.
//!
//! Unknown top-level keys are errors (the `deny_unknown_fields` behaviour of
//! §2.1) except under the user-owned `extra:` map. A file with no `kind` key
//! is a plain morg note and is not validated at all; an unknown `kind` value
//! is an error.

use morg_parser::saphyr::YamlOwned;
use morg_parser::{Frontmatter, FrontmatterNode, FrontmatterNodeKind, Span};

use crate::types::{
    ArgumentForm, ArgumentStatus, BloomOp, NoteId, NoteKind, NoteUuid, SourceStatus, Stance,
};

/// Keys shared by all three STA kinds.
const COMMON_KEYS: &[&str] = &["id", "uuid", "kind", "title", "created", "publish", "extra"];
const SOURCE_KEYS: &[&str] = &["status"];
const TOPIC_KEYS: &[&str] = &["aliases", "related"];
const ARGUMENT_KEYS: &[&str] = &["form", "thesis", "relations", "status"];
const RELATION_KEYS: &[&str] = &["to", "stance", "op"];

/// User-owned metadata under `extra:`, preserved in document order and
/// never interpreted by morog.
pub type Extra = Vec<(String, YamlOwned)>;

/// The typed frontmatter of one note, by kind.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteMeta {
    /// No `kind:` key — a plain morg note; no STA rule applies.
    Plain,
    Source(SourceMeta),
    Topic(TopicMeta),
    Argument(ArgumentMeta),
}

impl NoteMeta {
    pub fn kind(&self) -> Option<NoteKind> {
        match self {
            Self::Plain => None,
            Self::Source(_) => Some(NoteKind::Source),
            Self::Topic(_) => Some(NoteKind::Topic),
            Self::Argument(_) => Some(NoteKind::Argument),
        }
    }

    /// The note id, when declared.
    pub fn id(&self) -> Option<&NoteId> {
        match self {
            Self::Plain => None,
            Self::Source(m) => m.id.as_ref(),
            Self::Topic(m) => m.id.as_ref(),
            Self::Argument(m) => m.id.as_ref(),
        }
    }
}

/// Frontmatter of a `kind: source` note (plan §2.2).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SourceMeta {
    /// Required; `None` only when the file is in error (reported).
    pub id: Option<NoteId>,
    pub uuid: Option<NoteUuid>,
    /// Display title; derived from bibrat if omitted.
    pub title: Option<String>,
    pub status: Option<SourceStatus>,
    pub created: Option<String>,
    pub publish: bool,
    pub extra: Extra,
}

/// Frontmatter of a `kind: topic` note (plan §2.2).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TopicMeta {
    pub id: Option<NoteId>,
    pub uuid: Option<NoteUuid>,
    pub title: Option<String>,
    pub aliases: Vec<String>,
    /// Untyped topic → topic relations.
    pub related: Vec<RelatedDecl>,
    pub created: Option<String>,
    pub publish: bool,
    pub extra: Extra,
}

/// Frontmatter of a `kind: argument` note (plan §2.2).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArgumentMeta {
    pub id: Option<NoteId>,
    pub uuid: Option<NoteUuid>,
    pub title: Option<String>,
    pub form: ArgumentForm,
    pub thesis: Option<String>,
    pub relations: Vec<RelationDecl>,
    pub status: Option<ArgumentStatus>,
    pub created: Option<String>,
    pub publish: bool,
    pub extra: Extra,
}

/// One entry of an argument's `relations:` list.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationDecl {
    /// The raw `to:` value, e.g. `circles#evidence-2`.
    pub to: String,
    /// Target note id (the part before `#`).
    pub target_id: String,
    /// Target block anchor (the part after `#`), if any.
    pub target_anchor: Option<String>,
    pub stance: Stance,
    pub op: Option<BloomOp>,
    /// Span of this entry's `to:` target text (the whole entry when the
    /// target is somehow spanless).
    pub span: Span,
}

/// One entry of a topic's `related:` list.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedDecl {
    pub to: String,
    pub span: Span,
}

/// A schema violation found while typing the frontmatter, with the span of
/// the offending key or value when one is known.
#[derive(Debug, Clone, PartialEq)]
pub struct MetaError {
    pub kind: MetaErrorKind,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetaErrorKind {
    /// `kind:` carries a value outside the closed vocabulary.
    UnknownKind { value: String },
    /// A top-level (or relation) key outside the schema for this kind.
    UnknownKey { key: String, kind: NoteKind },
    /// A required field is absent (`id`, `relations[N].to`, …).
    MissingField { field: String },
    /// A value has the wrong YAML type.
    TypeMismatch {
        field: String,
        expected: &'static str,
    },
    /// A value is outside a closed enum vocabulary.
    BadEnumValue {
        field: String,
        value: String,
        allowed: &'static [&'static str],
    },
    /// `uuid:` does not parse as a UUID.
    BadUuid { value: String },
}

impl MetaError {
    /// Whether this error concerns the note's identity (`sta::id`) rather
    /// than its schema (`sta::kind`).
    pub fn is_id_error(&self) -> bool {
        matches!(&self.kind, MetaErrorKind::MissingField { field } if field == "id")
    }
}

impl std::fmt::Display for MetaErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKind { value } => {
                write!(
                    f,
                    "unknown kind `{value}` (expected source, topic, or argument)"
                )
            }
            Self::UnknownKey { key, kind } => {
                write!(
                    f,
                    "unknown frontmatter key `{key}` for kind `{}`",
                    kind.as_str()
                )
            }
            Self::MissingField { field } => write!(f, "missing required field `{field}`"),
            Self::TypeMismatch { field, expected } => {
                write!(f, "field `{field}` must be {expected}")
            }
            Self::BadEnumValue {
                field,
                value,
                allowed,
            } => write!(
                f,
                "invalid value `{value}` for `{field}` (expected one of: {})",
                allowed.join(", ")
            ),
            Self::BadUuid { value } => write!(f, "`{value}` is not a valid uuid"),
        }
    }
}

/// Convert a parsed frontmatter node into typed STA metadata.
///
/// Spans come straight off the parser's [`FrontmatterNode`] tree, so
/// entries nested inside `relations:` and `related:` lists (and their
/// individual keys and values) are located exactly.
pub fn parse_note_meta(fm: Option<&Frontmatter>) -> (NoteMeta, Vec<MetaError>) {
    let Some(fm) = fm else {
        return (NoteMeta::Plain, Vec::new());
    };
    let Some(kind_value) = fm.data.as_mapping_get("kind") else {
        // No `kind` key: a plain morg note, exempt from all STA rules.
        return (NoteMeta::Plain, Vec::new());
    };

    let mut cx = Cx {
        fm,
        errors: Vec::new(),
    };

    let Some(kind) = kind_value.as_str().and_then(NoteKind::parse) else {
        let value = kind_value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| yaml_type_name(kind_value).to_owned());
        cx.push(MetaErrorKind::UnknownKind { value }, cx.value_span("kind"));
        return (NoteMeta::Plain, cx.errors);
    };

    cx.check_unknown_keys(kind);

    let meta = match kind {
        NoteKind::Source => NoteMeta::Source(SourceMeta {
            id: cx.id_field(),
            uuid: cx.uuid_field(),
            title: cx.str_field("title"),
            status: cx.enum_field("status", SourceStatus::parse, &SourceStatus::ALL),
            created: cx.str_field("created"),
            publish: cx.bool_field("publish"),
            extra: cx.extra_field(),
        }),
        NoteKind::Topic => NoteMeta::Topic(TopicMeta {
            id: cx.id_field(),
            uuid: cx.uuid_field(),
            title: cx.str_field("title"),
            aliases: cx.str_list_field("aliases"),
            related: cx.related_field(),
            created: cx.str_field("created"),
            publish: cx.bool_field("publish"),
            extra: cx.extra_field(),
        }),
        NoteKind::Argument => NoteMeta::Argument(ArgumentMeta {
            id: cx.id_field(),
            uuid: cx.uuid_field(),
            title: cx.str_field("title"),
            form: cx
                .enum_field("form", ArgumentForm::parse, &ArgumentForm::ALL)
                .unwrap_or_default(),
            thesis: cx.str_field("thesis"),
            relations: cx.relations_field(),
            status: cx.enum_field("status", ArgumentStatus::parse, &ArgumentStatus::ALL),
            created: cx.str_field("created"),
            publish: cx.bool_field("publish"),
            extra: cx.extra_field(),
        }),
    };

    (meta, cx.errors)
}

/// Split a relation target into `(id, anchor)` at the first `#`.
pub fn split_target(to: &str) -> (String, Option<String>) {
    match to.split_once('#') {
        Some((id, anchor)) if !anchor.is_empty() => (id.to_owned(), Some(anchor.to_owned())),
        Some((id, _)) => (id.to_owned(), None),
        None => (to.to_owned(), None),
    }
}

/// Conversion context: the frontmatter node plus error accumulation.
struct Cx<'a> {
    fm: &'a Frontmatter,
    errors: Vec<MetaError>,
}

impl Cx<'_> {
    fn push(&mut self, kind: MetaErrorKind, span: Option<Span>) {
        self.errors.push(MetaError { kind, span });
    }

    fn value_span(&self, key: &str) -> Option<Span> {
        self.fm.entry(key).map(|e| e.value_span)
    }

    fn key_span(&self, key: &str) -> Option<Span> {
        self.fm.entry(key).map(|e| e.key_span)
    }

    fn value(&self, key: &str) -> Option<&YamlOwned> {
        self.fm.data.as_mapping_get(key)
    }

    /// The span-annotated node of a top-level value.
    fn node(&self, key: &str) -> Option<&FrontmatterNode> {
        self.fm.entry(key).map(|e| &e.value)
    }

    /// Spans of a top-level sequence's items, in document order.
    fn item_spans(&self, key: &str) -> Vec<Span> {
        self.node(key)
            .and_then(FrontmatterNode::items)
            .map(|items| items.iter().map(|n| n.span).collect())
            .unwrap_or_default()
    }

    /// Flag any top-level key outside the schema for `kind`.
    fn check_unknown_keys(&mut self, kind: NoteKind) {
        let extra_keys: &[&str] = match kind {
            NoteKind::Source => SOURCE_KEYS,
            NoteKind::Topic => TOPIC_KEYS,
            NoteKind::Argument => ARGUMENT_KEYS,
        };
        let mut unknown = Vec::new();
        for entry in &self.fm.entries {
            let key = entry.key.as_str();
            if !COMMON_KEYS.contains(&key) && !extra_keys.contains(&key) {
                unknown.push((key.to_owned(), entry.key_span));
            }
        }
        for (key, span) in unknown {
            self.push(MetaErrorKind::UnknownKey { key, kind }, Some(span));
        }
    }

    fn id_field(&mut self) -> Option<NoteId> {
        match self.value("id") {
            Some(v) => match v.as_str() {
                Some(s) => Some(NoteId::new(s)),
                None => {
                    self.push(
                        MetaErrorKind::TypeMismatch {
                            field: "id".into(),
                            expected: "a string",
                        },
                        self.value_span("id"),
                    );
                    None
                }
            },
            None => {
                // Point at the `kind:` entry, which is what demands an id.
                let span = self.key_span("kind");
                self.push(MetaErrorKind::MissingField { field: "id".into() }, span);
                None
            }
        }
    }

    fn uuid_field(&mut self) -> Option<NoteUuid> {
        let raw = self.str_field("uuid")?;
        match raw.parse() {
            Ok(uuid) => Some(uuid),
            Err(_) => {
                self.push(
                    MetaErrorKind::BadUuid { value: raw },
                    self.value_span("uuid"),
                );
                None
            }
        }
    }

    fn str_field(&mut self, key: &str) -> Option<String> {
        let v = self.value(key)?;
        match v.as_str() {
            Some(s) => Some(s.to_owned()),
            None => {
                self.push(
                    MetaErrorKind::TypeMismatch {
                        field: key.into(),
                        expected: "a string",
                    },
                    self.value_span(key),
                );
                None
            }
        }
    }

    fn bool_field(&mut self, key: &str) -> bool {
        let Some(v) = self.value(key) else {
            return false;
        };
        match v.as_bool() {
            Some(b) => b,
            None => {
                self.push(
                    MetaErrorKind::TypeMismatch {
                        field: key.into(),
                        expected: "a boolean",
                    },
                    self.value_span(key),
                );
                false
            }
        }
    }

    fn enum_field<T>(
        &mut self,
        key: &str,
        parse: fn(&str) -> Option<T>,
        allowed: &'static [&'static str],
    ) -> Option<T> {
        let raw = self.str_field(key)?;
        match parse(&raw) {
            Some(v) => Some(v),
            None => {
                self.push(
                    MetaErrorKind::BadEnumValue {
                        field: key.into(),
                        value: raw,
                        allowed,
                    },
                    self.value_span(key),
                );
                None
            }
        }
    }

    fn str_list_field(&mut self, key: &str) -> Vec<String> {
        let Some(v) = self.value(key) else {
            return Vec::new();
        };
        let Some(seq) = v.as_sequence() else {
            self.push(
                MetaErrorKind::TypeMismatch {
                    field: key.into(),
                    expected: "a list of strings",
                },
                self.value_span(key),
            );
            return Vec::new();
        };
        let items: Vec<Option<String>> = seq
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect();
        let spans = self.item_spans(key);
        let fallback = self.value_span(key);
        let mut out = Vec::new();
        for (i, item) in items.into_iter().enumerate() {
            match item {
                Some(s) => out.push(s),
                None => {
                    self.push(
                        MetaErrorKind::TypeMismatch {
                            field: format!("{key}[{i}]"),
                            expected: "a string",
                        },
                        spans.get(i).copied().or(fallback),
                    );
                }
            }
        }
        out
    }

    fn extra_field(&mut self) -> Extra {
        let Some(v) = self.value("extra") else {
            return Vec::new();
        };
        let Some(map) = v.as_mapping() else {
            self.push(
                MetaErrorKind::TypeMismatch {
                    field: "extra".into(),
                    expected: "a mapping",
                },
                self.value_span("extra"),
            );
            return Vec::new();
        };
        map.iter()
            .filter_map(|(k, v)| Some((k.as_str()?.to_owned(), v.clone())))
            .collect()
    }

    fn related_field(&mut self) -> Vec<RelatedDecl> {
        let Some(v) = self.value("related") else {
            return Vec::new();
        };
        let Some(seq) = v.as_sequence() else {
            self.push(
                MetaErrorKind::TypeMismatch {
                    field: "related".into(),
                    expected: "a list of note ids",
                },
                self.value_span("related"),
            );
            return Vec::new();
        };
        let items: Vec<Option<String>> = seq
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect();
        let spans = self.item_spans("related");
        let fallback = self.value_span("related").unwrap_or(self.fm.span);
        let mut out = Vec::new();
        for (i, item) in items.into_iter().enumerate() {
            let span = spans.get(i).copied().unwrap_or(fallback);
            match item {
                Some(to) => out.push(RelatedDecl { to, span }),
                None => {
                    self.push(
                        MetaErrorKind::TypeMismatch {
                            field: format!("related[{i}]"),
                            expected: "a note id",
                        },
                        Some(span),
                    );
                }
            }
        }
        out
    }

    fn relations_field(&mut self) -> Vec<RelationDecl> {
        let Some(v) = self.value("relations") else {
            return Vec::new();
        };
        let Some(seq) = v.as_sequence() else {
            self.push(
                MetaErrorKind::TypeMismatch {
                    field: "relations".into(),
                    expected: "a list of { to, stance, op } entries",
                },
                self.value_span("relations"),
            );
            return Vec::new();
        };
        let seq: Vec<_> = seq.to_vec();
        let whole = self.value_span("relations").unwrap_or(self.fm.span);

        // Per-entry node spans: the whole entry, its `to` / `stance` / `op`
        // values, and the key spans of any keys outside the schema.
        struct EntrySpans {
            item: Span,
            to: Option<Span>,
            stance: Option<Span>,
            op: Option<Span>,
            unknown_keys: Vec<(String, Span)>,
        }
        let entry_spans: Vec<EntrySpans> = self
            .node("relations")
            .and_then(FrontmatterNode::items)
            .map(|items| {
                items
                    .iter()
                    .map(|item| EntrySpans {
                        item: item.span,
                        to: item.entry("to").map(|e| e.value.span),
                        stance: item.entry("stance").map(|e| e.value.span),
                        op: item.entry("op").map(|e| e.value.span),
                        unknown_keys: match &item.kind {
                            FrontmatterNodeKind::Map(entries) => entries
                                .iter()
                                .filter(|e| !RELATION_KEYS.contains(&e.key.as_str()))
                                .map(|e| (e.key.clone(), e.key_span))
                                .collect(),
                            _ => Vec::new(),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut out = Vec::new();
        for (i, item) in seq.iter().enumerate() {
            let spans = entry_spans.get(i);
            let item_span = spans.map_or(whole, |s| s.item);
            if item.as_mapping().is_none() {
                self.push(
                    MetaErrorKind::TypeMismatch {
                        field: format!("relations[{i}]"),
                        expected: "a mapping with `to` and `stance`",
                    },
                    Some(item_span),
                );
                continue;
            }

            let unknown_keys = spans.map(|s| s.unknown_keys.clone()).unwrap_or_default();
            for (k, key_span) in unknown_keys {
                self.push(
                    MetaErrorKind::UnknownKey {
                        key: format!("relations[{i}].{k}"),
                        kind: NoteKind::Argument,
                    },
                    Some(key_span),
                );
            }

            let to_span = spans.and_then(|s| s.to).unwrap_or(item_span);
            let to = match item.as_mapping_get("to") {
                Some(v) => match v.as_str() {
                    Some(s) => Some(s.to_owned()),
                    None => {
                        self.push(
                            MetaErrorKind::TypeMismatch {
                                field: format!("relations[{i}].to"),
                                expected: "a string",
                            },
                            Some(to_span),
                        );
                        None
                    }
                },
                None => {
                    self.push(
                        MetaErrorKind::MissingField {
                            field: format!("relations[{i}].to"),
                        },
                        Some(item_span),
                    );
                    None
                }
            };

            // The entry's representative span is its `to:` target text.
            let span = to_span;

            let stance_span = spans.and_then(|s| s.stance).unwrap_or(item_span);
            let stance = match item.as_mapping_get("stance").and_then(|v| v.as_str()) {
                Some(s) => match Stance::parse(s) {
                    Some(stance) => Some(stance),
                    None => {
                        self.push(
                            MetaErrorKind::BadEnumValue {
                                field: format!("relations[{i}].stance"),
                                value: s.to_owned(),
                                allowed: &Stance::ALL,
                            },
                            Some(stance_span),
                        );
                        None
                    }
                },
                None => {
                    self.push(
                        MetaErrorKind::MissingField {
                            field: format!("relations[{i}].stance"),
                        },
                        Some(item_span),
                    );
                    None
                }
            };

            let op_span = spans.and_then(|s| s.op).unwrap_or(item_span);
            let op = match item.as_mapping_get("op") {
                Some(v) => match v.as_str().and_then(BloomOp::parse) {
                    Some(op) => Some(op),
                    None => {
                        self.push(
                            MetaErrorKind::BadEnumValue {
                                field: format!("relations[{i}].op"),
                                value: v.as_str().unwrap_or("<non-string>").to_owned(),
                                allowed: &BloomOp::ALL,
                            },
                            Some(op_span),
                        );
                        None
                    }
                },
                None => None,
            };

            if let (Some(to), Some(stance)) = (to, stance) {
                let (target_id, target_anchor) = split_target(&to);
                out.push(RelationDecl {
                    to,
                    target_id,
                    target_anchor,
                    stance,
                    op,
                    span,
                });
            }
        }
        out
    }
}

fn yaml_type_name(v: &YamlOwned) -> &'static str {
    use morg_parser::saphyr::ScalarOwned;
    match v {
        YamlOwned::Value(ScalarOwned::Null) => "<null>",
        YamlOwned::Value(ScalarOwned::Boolean(_)) => "<boolean>",
        YamlOwned::Value(ScalarOwned::Integer(_)) => "<integer>",
        YamlOwned::Value(ScalarOwned::FloatingPoint(_)) => "<float>",
        YamlOwned::Value(ScalarOwned::String(_)) => "<string>",
        YamlOwned::Sequence(_) => "<sequence>",
        YamlOwned::Mapping(_) => "<mapping>",
        _ => "<value>",
    }
}
