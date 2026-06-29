/// A field's logical type, rendered to a concrete Rust (per binding) or Python type.
///
/// A few types render differently per language on purpose: a `Count` is `u32` in Node
/// (so it stays a plain JS `number` rather than a `BigInt`) but `u64` in Python.
#[derive(Clone, Copy)]
pub enum Ty {
    /// `String` / `str`.
    Str,
    /// `Option<String>` / `Optional[str]`.
    OptStr,
    /// `bool`.
    Bool,
    /// `u32`: a small count or attempt number.
    U32,
    /// `f64`: epoch milliseconds or a duration in seconds.
    F64,
    /// `Option<f64>`.
    OptF64,
    /// A count: `u32` in Node (avoids JS `BigInt`), `u64` in Python.
    Count,
    /// A byte size: `f64` in Node, `u64` in Python.
    Size,
    /// `Vec<String>` / `list[str]`.
    VecStr,
    /// `HashMap<String, String>` / `dict[str, str]`.
    Map,
    /// `Vec<Dto>` / `list[Dto]`, referencing another DTO by its logical name.
    VecOf(&'static str),
}

impl Ty {
    /// The Rust type for the Node (`napi`) binding.
    pub fn node(self) -> String {
        match self {
            Ty::Str => "String".into(),
            Ty::OptStr => "Option<String>".into(),
            Ty::Bool => "bool".into(),
            Ty::U32 => "u32".into(),
            Ty::F64 => "f64".into(),
            Ty::OptF64 => "Option<f64>".into(),
            Ty::Count => "u32".into(),
            Ty::Size => "f64".into(),
            Ty::VecStr => "Vec<String>".into(),
            Ty::Map => "HashMap<String, String>".into(),
            Ty::VecOf(name) => format!("Vec<{}>", node_name(name)),
        }
    }

    /// The Rust type for the Python (`pyo3`) binding.
    pub fn py(self) -> String {
        match self {
            Ty::Str => "String".into(),
            Ty::OptStr => "Option<String>".into(),
            Ty::Bool => "bool".into(),
            Ty::U32 => "u32".into(),
            Ty::F64 => "f64".into(),
            Ty::OptF64 => "Option<f64>".into(),
            Ty::Count => "u64".into(),
            Ty::Size => "u64".into(),
            Ty::VecStr => "Vec<String>".into(),
            Ty::Map => "HashMap<String, String>".into(),
            Ty::VecOf(name) => format!("Vec<{}>", py_name(name)),
        }
    }

    /// The Python type for the `.pyi` stub.
    pub fn pyi(self) -> String {
        match self {
            Ty::Str => "str".into(),
            Ty::OptStr => "Optional[str]".into(),
            Ty::Bool => "bool".into(),
            Ty::U32 | Ty::Count | Ty::Size => "int".into(),
            Ty::F64 => "float".into(),
            Ty::OptF64 => "Optional[float]".into(),
            Ty::VecStr => "list[str]".into(),
            Ty::Map => "dict[str, str]".into(),
            Ty::VecOf(name) => format!("list[{}]", py_name(name)),
        }
    }
}

/// One field of a DTO.
pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
    /// Doc comment (without `///`); empty for none.
    pub doc: &'static str,
}

const fn field(name: &'static str, ty: Ty, doc: &'static str) -> Field {
    Field { name, ty, doc }
}

/// One cross-language DTO: a value type returned across the FFI boundary.
pub struct Dto {
    /// Logical name, used to cross-reference from `Ty::VecOf`.
    pub name: &'static str,
    /// The Node struct name (napi renders it as the TS interface name).
    pub node_name: &'static str,
    /// The Python `#[pyclass]` name.
    pub py_name: &'static str,
    /// Whether the Python struct needs `#[derive(Clone)]` (it is nested inside a page).
    pub clone: bool,
    /// Struct-level doc comment (without `///`).
    pub doc: &'static str,
    pub fields: &'static [Field],
}

/// Resolve a logical DTO name to its Node struct name.
pub fn node_name(logical: &str) -> &'static str {
    dto(logical).node_name
}

/// Resolve a logical DTO name to its Python struct name.
pub fn py_name(logical: &str) -> &'static str {
    dto(logical).py_name
}

fn dto(logical: &str) -> &'static Dto {
    SCHEMA
        .iter()
        .find(|d| d.name == logical)
        .unwrap_or_else(|| panic!("unknown DTO referenced in schema: {logical}"))
}

use Ty::*;

pub static SCHEMA: &[Dto] = &[
    Dto {
        name: "Job",
        node_name: "JsJob",
        py_name: "Job",
        clone: false,
        doc: "A leased job. Settle it with ack/nack/heartbeat using the opaque, \
              delivery-unique `receipt` (not `id`, which is stable across redeliveries \
              and is the natural idempotency key).",
        fields: &[
            field("id", Str, ""),
            field(
                "receipt",
                Str,
                "Delivery-unique handle for ack/nack/heartbeat (SQS ReceiptHandle).",
            ),
            field("payload", Str, ""),
            field("attempt", U32, ""),
            field("max_attempts", U32, ""),
            field("leased_until_ms", F64, ""),
            field("queue", Str, ""),
        ],
    },
    Dto {
        name: "Decision",
        node_name: "JsDecision",
        py_name: "Decision",
        clone: false,
        doc: "A rate-limit decision (maps onto the IETF RateLimit header fields).",
        fields: &[
            field("allowed", Bool, ""),
            field("limit", U32, ""),
            field("remaining", U32, ""),
            field(
                "reset_after_seconds",
                F64,
                "Seconds until the limit fully resets (the IETF `RateLimit-Reset` value).",
            ),
            field("retry_after_seconds", OptF64, ""),
        ],
    },
    Dto {
        name: "ApiKey",
        node_name: "JsApiKey",
        py_name: "ApiKey",
        clone: false,
        doc: "A freshly minted API key. `secret` is shown exactly once.",
        fields: &[
            field("id", Str, ""),
            field("secret", Str, ""),
            field("label", Str, ""),
            field("created_at_ms", F64, ""),
        ],
    },
    Dto {
        name: "QueueDepth",
        node_name: "JsQueueDepth",
        py_name: "QueueDepth",
        clone: false,
        doc: "Approximate queue depth (SQS ApproximateNumberOfMessages{,NotVisible,Delayed}).",
        fields: &[
            field("visible", Count, ""),
            field("in_flight", Count, ""),
            field("delayed", Count, ""),
        ],
    },
    Dto {
        name: "ScanPage",
        node_name: "JsScanPage",
        py_name: "ScanPage",
        clone: false,
        doc: "One page of a kv scan: the keys plus an opaque next-page `cursor` \
              (absent when iteration is complete).",
        fields: &[
            field("keys", VecStr, ""),
            field("cursor", OptStr, ""),
        ],
    },
    Dto {
        name: "BlobInfo",
        node_name: "JsBlobInfo",
        py_name: "BlobInfo",
        clone: true,
        doc: "Object metadata (S3 HeadObject). `last_modified_ms` is epoch milliseconds.",
        fields: &[
            field("key", Str, ""),
            field("size", Size, ""),
            field("content_type", Str, ""),
            field("etag", Str, ""),
            field("last_modified_ms", F64, ""),
            field("metadata", Map, ""),
        ],
    },
    Dto {
        name: "BlobPage",
        node_name: "JsBlobPage",
        py_name: "BlobListPage",
        clone: false,
        doc: "One page of a blob list: the objects plus an opaque next-page `cursor` \
              (absent when iteration is complete).",
        fields: &[
            field("items", VecOf("BlobInfo"), ""),
            field("cursor", OptStr, ""),
        ],
    },
    Dto {
        name: "ScheduleInfo",
        node_name: "JsScheduleInfo",
        py_name: "ScheduleInfo",
        clone: true,
        doc: "A registered schedule. `kind` is \"cron\" or \"at\"; `cron_expr` is set \
              only for crons. Times are epoch milliseconds.",
        fields: &[
            field("name", Str, ""),
            field("kind", Str, ""),
            field("cron_expr", OptStr, ""),
            field("queue", Str, ""),
            field("next_run_ms", F64, ""),
            field("last_run_ms", OptF64, ""),
        ],
    },
    Dto {
        name: "SchedulePage",
        node_name: "JsSchedulePage",
        py_name: "SchedulePage",
        clone: false,
        doc: "One page of a schedule list: the schedules plus an opaque next-page \
              `cursor` (absent when iteration is complete).",
        fields: &[
            field("items", VecOf("ScheduleInfo"), ""),
            field("cursor", OptStr, ""),
        ],
    },
    Dto {
        name: "Session",
        node_name: "JsSession",
        py_name: "SessionInfo",
        clone: false,
        doc: "A validated session's metadata. Times are epoch milliseconds.",
        fields: &[
            field("user_id", Str, ""),
            field("created_at_ms", F64, ""),
            field("expires_at_ms", F64, ""),
        ],
    },
    Dto {
        name: "ApiKeyInfo",
        node_name: "JsApiKeyInfo",
        py_name: "ApiKeyInfo",
        clone: false,
        doc: "Non-secret API-key metadata.",
        fields: &[
            field("id", Str, ""),
            field("owner_id", Str, ""),
            field("label", Str, ""),
        ],
    },
    Dto {
        name: "BackendInfo",
        node_name: "JsBackendInfo",
        py_name: "BackendInfo",
        clone: false,
        doc: "One line of a backend report: which provider powers a primitive.",
        fields: &[
            field("primitive", Str, ""),
            field("provider", Str, ""),
            field("durable", Bool, ""),
            field("caveats", Str, ""),
        ],
    },
];
