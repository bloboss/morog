//! Core newtypes and closed vocabularies of the STA document model
//! (plan §2.1 and §2.3).

use std::fmt;
use std::str::FromStr;

pub use morog_bib::CiteKey;

/// Human-readable note identifier: the required `id:` frontmatter field.
///
/// Ids are slug-shaped (see [`is_slug`]); the shape is checked by the
/// `sta::id` lint rather than at construction, so a malformed id can still
/// be carried through extraction and reported with its span.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NoteId(Box<str>);

impl NoteId {
    pub fn new(id: impl Into<Box<str>>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the id is slug-shaped.
    pub fn is_slug(&self) -> bool {
        is_slug(&self.0)
    }
}

impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::ops::Deref for NoteId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

/// Slug shape shared by note ids and block anchors: ASCII alphanumeric
/// start, then alphanumerics, `-`, and `_`.
pub fn is_slug(s: &str) -> bool {
    let mut bytes = s.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Permanent note identity: the `uuid:` frontmatter field. Generated once,
/// never edited; public IRIs are minted from it (plan §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoteUuid(uuid::Uuid);

impl NoteUuid {
    pub fn new(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }
}

impl FromStr for NoteUuid {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(s).map(Self)
    }
}

impl fmt::Display for NoteUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The three strictly enforced STA note kinds. Closed in v1 (plan §0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteKind {
    /// Anchor to raw material: citation-shaped title, DOI/PDF, mostly empty.
    Source,
    /// Everything known about one narrow topic, every block sourced.
    Topic,
    /// Synthesis in the author's own words; carries the thesis.
    Argument,
}

impl NoteKind {
    pub const ALL: [&'static str; 3] = ["source", "topic", "argument"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Topic => "topic",
            Self::Argument => "argument",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "source" => Some(Self::Source),
            "topic" => Some(Self::Topic),
            "argument" => Some(Self::Argument),
            _ => None,
        }
    }
}

/// Relation polarity (plan §2.3): discourse-graph *supports*/*opposes* plus
/// Toulmin's qualifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stance {
    Supports,
    Opposes,
    Qualifies,
}

impl Stance {
    pub const ALL: [&'static str; 3] = ["supports", "opposes", "qualifies"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Opposes => "opposes",
            Self::Qualifies => "qualifies",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "supports" => Some(Self::Supports),
            "opposes" => Some(Self::Opposes),
            "qualifies" => Some(Self::Qualifies),
            _ => None,
        }
    }
}

/// What the argument does with the target: the six revised-Bloom levels as
/// verbs (plan §2.3). Ordered, so an argument's depth is `max(op)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BloomOp {
    Recall,
    Explain,
    Apply,
    Analyze,
    Evaluate,
    Create,
}

impl BloomOp {
    pub const ALL: [&'static str; 6] = [
        "recall", "explain", "apply", "analyze", "evaluate", "create",
    ];

    /// The Bloom ordinal (0–5), as stored by the index.
    pub fn ordinal(self) -> u8 {
        match self {
            Self::Recall => 0,
            Self::Explain => 1,
            Self::Apply => 2,
            Self::Analyze => 3,
            Self::Evaluate => 4,
            Self::Create => 5,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recall => "recall",
            Self::Explain => "explain",
            Self::Apply => "apply",
            Self::Analyze => "analyze",
            Self::Evaluate => "evaluate",
            Self::Create => "create",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "recall" => Some(Self::Recall),
            "explain" => Some(Self::Explain),
            "apply" => Some(Self::Apply),
            "analyze" => Some(Self::Analyze),
            "evaluate" => Some(Self::Evaluate),
            "create" => Some(Self::Create),
            _ => None,
        }
    }
}

/// The kinds of relation morog records (plan §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationKind {
    /// Inline `[@key]` from a topic block to a source.
    Cites,
    /// Untyped topic → topic `related:` entry.
    Related,
    /// Plain `id:` link in prose.
    Mentions,
    /// Typed argument relation declared in frontmatter.
    Argues { stance: Stance, op: Option<BloomOp> },
}

/// Reading status of a source note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceStatus {
    Unread,
    Skimmed,
    Read,
    Processed,
}

impl SourceStatus {
    pub const ALL: [&'static str; 4] = ["unread", "skimmed", "read", "processed"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Skimmed => "skimmed",
            Self::Read => "read",
            Self::Processed => "processed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unread" => Some(Self::Unread),
            "skimmed" => Some(Self::Skimmed),
            "read" => Some(Self::Read),
            "processed" => Some(Self::Processed),
            _ => None,
        }
    }
}

/// Maturity status of an argument note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgumentStatus {
    Draft,
    Developing,
    Stable,
}

impl ArgumentStatus {
    pub const ALL: [&'static str; 3] = ["draft", "developing", "stable"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Developing => "developing",
            Self::Stable => "stable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "developing" => Some(Self::Developing),
            "stable" => Some(Self::Stable),
            _ => None,
        }
    }
}

/// Whether an argument asserts a claim or poses an open question
/// (discourse-graph `QUE` as an attribute, not a fourth kind — plan §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ArgumentForm {
    #[default]
    Claim,
    Question,
}

impl ArgumentForm {
    pub const ALL: [&'static str; 2] = ["claim", "question"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Question => "question",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claim" => Some(Self::Claim),
            "question" => Some(Self::Question),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_shapes() {
        assert!(is_slug("specific-leaf-area"));
        assert!(is_slug("paszke_pytorch_2019"));
        assert!(is_slug("shabanovSTA"));
        assert!(is_slug("b1"));
        assert!(!is_slug(""));
        assert!(!is_slug("-leading-dash"));
        assert!(!is_slug("has space"));
        assert!(!is_slug("café"));
        assert!(!is_slug("a#b"));
    }

    #[test]
    fn bloom_op_is_ordered() {
        assert!(BloomOp::Recall < BloomOp::Create);
        assert!(BloomOp::Analyze < BloomOp::Evaluate);
        assert_eq!(BloomOp::Create.ordinal(), 5);
    }

    #[test]
    fn enum_round_trips() {
        for s in NoteKind::ALL {
            assert_eq!(NoteKind::parse(s).unwrap().as_str(), s);
        }
        for s in Stance::ALL {
            assert_eq!(Stance::parse(s).unwrap().as_str(), s);
        }
        for s in BloomOp::ALL {
            assert_eq!(BloomOp::parse(s).unwrap().as_str(), s);
        }
        for s in SourceStatus::ALL {
            assert_eq!(SourceStatus::parse(s).unwrap().as_str(), s);
        }
        for s in ArgumentStatus::ALL {
            assert_eq!(ArgumentStatus::parse(s).unwrap().as_str(), s);
        }
        for s in ArgumentForm::ALL {
            assert_eq!(ArgumentForm::parse(s).unwrap().as_str(), s);
        }
    }

    #[test]
    fn note_uuid_parses() {
        let u: NoteUuid = "0192f3c4-7a11-7bbb-8f55-111111111111".parse().unwrap();
        assert_eq!(u.to_string(), "0192f3c4-7a11-7bbb-8f55-111111111111");
        assert!("not-a-uuid".parse::<NoteUuid>().is_err());
    }
}
