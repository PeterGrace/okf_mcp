//! The write tools (design "Write tools"): the content tools here (Task 8),
//! the bookkeeping tools below them (Task 9). Each goes through
//! [`crate::write::apply`], so it acts on the changeset its required
//! `changeset` handle names (in that changeset's catalog), the write rules
//! apply, and nothing is visible to anyone else until `publish`.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::okf::ops::{self, LogKind, NewConcept, SourceEntry, StampField};
use crate::write::{apply, WriteReport};

/// `create_concept`'s arguments. Doc comments are the parameters'
/// descriptions in the tool's input schema.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct CreateConceptParams {
    /// The new concept's path, e.g. `/infra/ntp.md` (the `.md` is optional); it must not exist.
    pub path: String,
    /// Frontmatter keys as a JSON object; `type` is required, and `generated`, `verified` and `reviewed` are refused (the server writes `generated`).
    pub frontmatter: serde_json::Map<String, serde_json::Value>,
    /// The markdown body after the frontmatter.
    pub body: String,
    /// List the concept in its folder's index.md with this summary, creating folder indexes as needed.
    pub index_summary: Option<String>,
    /// The index.md section to list it under (default: where `okf.py index` puts it); needs `index_summary`.
    pub index_section: Option<String>,
    /// Who wrote it, for the `generated` stamp (default: the key's actor).
    pub by: Option<String>,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `edit_document`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct EditDocumentParams {
    /// A full path with `.md`: a concept or an index.md, e.g. `/infra/ntp.md` (`/infra/ntp` is refused).
    pub path: String,
    /// The exact text to replace; without `replace_all` it must occur exactly once. The edit may not add or change frontmatter.
    pub old_str: String,
    /// The replacement text.
    pub new_str: String,
    /// Replace every occurrence instead of exactly one.
    #[serde(default)]
    pub replace_all: bool,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `replace_body`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ReplaceBodyParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// The new body; the frontmatter is kept byte for byte, and the body may not add any.
    pub body: String,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `set_frontmatter`'s arguments.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetFrontmatterParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// A descriptive key such as `title`, `status` or `tags`; the stamps, `open_questions` and `sources` have their own tools.
    pub key: String,
    /// The new value as JSON; required, and an explicit null deletes the key (`type` cannot be deleted, and must be a non-blank string).
    // No `#[serde(default)]`: a missing `value` is a parameter error, so
    // forgetting the argument can never delete the key. `serde_json::Value`
    // is not an `Option`, so serde treats the field as required.
    pub value: serde_json::Value,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// Creates a concept (and, with `index_summary`, its listing).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::InvalidPath`] (taken, reserved,
/// machinery) or [`OkfError::Refused`] (a missing `type`, a stamp key, a
/// listing it cannot make).
pub(crate) async fn create_concept(
    ctx: &ToolContext,
    params: CreateConceptParams,
) -> Result<WriteReport, OkfError> {
    // Destructuring moves each field into its own variable, so the closure
    // can take the ones it needs while `changeset` and `by` stay here.
    let CreateConceptParams {
        path,
        frontmatter,
        body,
        index_summary,
        index_section,
        by,
        changeset,
    } = params;
    apply(ctx, &changeset, by.as_deref(), move |ws, op_ctx| {
        let new = NewConcept {
            path: &path,
            frontmatter: &frontmatter,
            body: &body,
            index_summary: index_summary.as_deref(),
            index_section: index_section.as_deref(),
        };
        ops::create_concept(ws, op_ctx, &new)
    })
    .await
}

/// Replaces exact text in a concept or an `index.md`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the document (a full `.md` path) and the replacement.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; [`OkfError::EditMismatch`] when `old_str` is found 0 or
/// (without `replace_all`) more than 1 times; [`OkfError::InvalidPath`] for
/// `log.md`, machinery or a path without `.md`; [`OkfError::Refused`] for an
/// edit that adds or changes frontmatter.
pub(crate) async fn edit_document(
    ctx: &ToolContext,
    params: EditDocumentParams,
) -> Result<WriteReport, OkfError> {
    let EditDocumentParams {
        path,
        old_str,
        new_str,
        replace_all,
        changeset,
    } = params;
    apply(ctx, &changeset, None, move |ws, _| {
        ops::edit_document(ws, &path, &old_str, &new_str, replace_all)
    })
    .await
}

/// Replaces a concept's body, keeping its frontmatter byte for byte.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept reference and its new body.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::NotFound`], [`OkfError::InvalidPath`]
/// or [`OkfError::Refused`] (a body that adds frontmatter).
pub(crate) async fn replace_body(
    ctx: &ToolContext,
    params: ReplaceBodyParams,
) -> Result<WriteReport, OkfError> {
    let ReplaceBodyParams {
        path,
        body,
        changeset,
    } = params;
    apply(ctx, &changeset, None, move |ws, _| {
        ops::replace_body(ws, &path, &body)
    })
    .await
}

/// Sets or deletes one descriptive frontmatter key, as a textual edit.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept reference, the key and the value (null deletes).
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's refusal of a dedicated key, of deleting `type`
/// or of a `type` that is not a non-blank string.
pub(crate) async fn set_frontmatter(
    ctx: &ToolContext,
    params: SetFrontmatterParams,
) -> Result<WriteReport, OkfError> {
    let SetFrontmatterParams {
        path,
        key,
        value,
        changeset,
    } = params;
    // The op reads a JSON null as a delete, so the value passes as it is.
    apply(ctx, &changeset, None, move |ws, _| {
        ops::set_frontmatter(ws, &path, &key, Some(&value))
    })
    .await
}

// The content tools; `OkfServer::tools` joins `writes_router()` with the rest.
#[tool_router(router = writes_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `create_concept` tool.
    #[tool(
        title = "Create Concept",
        description = "Create a concept in the changeset `changeset` names (open one with \
                       open_changeset); nothing is visible to others until `publish`. `path` is \
                       the new concept's path, `.md` optional (e.g. /infra/ntp). Needs `type` in \
                       `frontmatter`; the server writes `generated: { by, at }` itself. With \
                       `index_summary`, also lists it in its folder's index.md (creating folder \
                       indexes as needed). Write access.",
        annotations(
            title = "Create Concept",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CreateConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(create_concept(&ctx, params).await)
    }

    /// The `edit_document` tool.
    #[tool(
        title = "Edit Document",
        description = "Replace exact text in a concept or an index.md, in the changeset \
                       `changeset` names (open one with open_changeset). `path` is a full path \
                       with .md (e.g. /infra/ntp.md or /infra/index.md). `old_str` must occur \
                       exactly once unless `replace_all`; the edit may not add or change \
                       frontmatter. log.md and machinery are refused (use `log`). Write access.",
        annotations(
            title = "Edit Document",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn edit_document(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<EditDocumentParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(edit_document(&ctx, params).await)
    }

    /// The `replace_body` tool.
    #[tool(
        title = "Replace Concept Body",
        description = "Replace a concept's body, keeping its frontmatter byte for byte, in the \
                       changeset `changeset` names (open one with open_changeset). `path` is a \
                       concept reference, .md optional (/infra/ntp or /infra/ntp.md). Write \
                       access.",
        annotations(
            title = "Replace Concept Body",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn replace_body(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ReplaceBodyParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(replace_body(&ctx, params).await)
    }

    /// The `set_frontmatter` tool.
    #[tool(
        title = "Set Frontmatter",
        description = "Set (or, with a null value, delete) one descriptive frontmatter key of a \
                       concept, in the changeset `changeset` names (open one with \
                       open_changeset). `path` is a concept reference, .md optional (/infra/ntp \
                       or /infra/ntp.md). The stamps, open_questions and sources have their own \
                       tools; `type` cannot be deleted. Write access.",
        annotations(
            title = "Set Frontmatter",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn set_frontmatter(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SetFrontmatterParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(set_frontmatter(&ctx, params).await)
    }
}

// ---------------------------------------------------------------------------
// The bookkeeping tools (Task 9): ports of okf.py's concept commands, `log`
// and `index`. A block of its own (`bookkeeping_router`), so the content
// tools above and these `okf.py` ports stay two groups, as in the design.
// ---------------------------------------------------------------------------

/// A queued question, by 1-based position or by its exact text.
// `untagged`: serde tries each variant in order, so `2` is a number and
// `"Who owns it?"` is text, with no wrapper object in the JSON.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum QuestionRef {
    /// Its 1-based position, as `open_questions` numbers it.
    Number(u64),
    /// Its exact text (a string of digits is read as a position, as `okf.py` does).
    Text(String),
}

impl QuestionRef {
    /// The argument `okf.py unquestion` takes: a number as its digits, or the
    /// text.
    ///
    /// # Returns
    /// The string the op parses (digits are a 1-based position).
    fn into_argument(self) -> String {
        match self {
            QuestionRef::Number(number) => number.to_string(),
            QuestionRef::Text(text) => text,
        }
    }
}

/// `stamp`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct StampParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// `generated` (content written) or `verified` (checked against the real system).
    pub field: StampField,
    /// Who did it (default: the key's actor); say `human:<name>` only when a person really did.
    pub by: Option<String>,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `review`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ReviewParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// The outcome: verified, updated, partial or unverifiable.
    pub outcome: String,
    /// What was and wasn't checked.
    pub note: String,
    /// Questions to queue in `open_questions` (exact duplicates are skipped).
    pub questions: Option<Vec<String>>,
    /// Who reviewed (default: the key's actor).
    pub by: Option<String>,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `question`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct QuestionParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// The self-contained question to queue in `open_questions`.
    pub question: String,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `unquestion`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct UnquestionParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// The question to remove: its 1-based number in `open_questions`, or its exact text.
    pub question: QuestionRef,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `add_source`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct AddSourceParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// The entry's id; an entry with this id must not exist.
    pub id: String,
    /// What was consulted: a URI, a path or a description.
    pub resource: String,
    /// Its title.
    pub title: Option<String>,
    /// Its author, e.g. `human:pgrace`.
    pub author: Option<String>,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `remove_source`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct RemoveSourceParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// The id of the entry to remove; no footnote in the body may still cite it.
    pub id: String,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `log`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct LogParams {
    /// The entry; runs of whitespace become single spaces.
    pub text: String,
    /// Update, Creation, Change or Import (default Update).
    pub kind: Option<LogKind>,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `index`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct IndexParams {
    /// The concept to list, with or without `.md`, e.g. `/infra/ntp`; its folder's index.md must exist.
    pub path: String,
    /// The text after the link's ` - `.
    pub summary: String,
    /// The heading to list it under, e.g. `Build infrastructure` (default: `Concepts`).
    pub section: Option<String>,
    /// The link text (default: the concept's frontmatter title).
    pub title: Option<String>,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `okf.py stamp`: sets `generated`, or appends to `verified`, with
/// `{ by, at: now }`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, the field and `by`.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::NotFound`] or
/// [`OkfError::InvalidPath`] for the reference, [`OkfError::Refused`] for a
/// concept without frontmatter, [`OkfError::FrontmatterParse`] or
/// [`OkfError::Shape`].
pub(crate) async fn stamp(ctx: &ToolContext, params: StampParams) -> Result<WriteReport, OkfError> {
    let StampParams {
        path,
        field,
        by,
        changeset,
    } = params;
    apply(ctx, &changeset, by.as_deref(), move |ws, op_ctx| {
        ops::stamp(ws, op_ctx, &path, field)
    })
    .await
}

/// `okf.py review`: replaces `reviewed` and queues any questions.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, outcome, note, questions and `by`.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`]; [`OkfError::Refused`] for an unknown outcome.
pub(crate) async fn review(
    ctx: &ToolContext,
    params: ReviewParams,
) -> Result<WriteReport, OkfError> {
    let ReviewParams {
        path,
        outcome,
        note,
        questions,
        by,
        changeset,
    } = params;
    apply(ctx, &changeset, by.as_deref(), move |ws, op_ctx| {
        // `unwrap_or_default` gives an empty list when no questions were sent.
        let questions = questions.unwrap_or_default();
        ops::review(ws, op_ctx, &path, &outcome, &note, &questions)
    })
    .await
}

/// `okf.py question`: queues a question in `open_questions`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and the question.
///
/// # Returns
/// The write's report; nothing touched if the question is already queued.
///
/// # Errors
/// As [`stamp`].
pub(crate) async fn question(
    ctx: &ToolContext,
    params: QuestionParams,
) -> Result<WriteReport, OkfError> {
    let QuestionParams {
        path,
        question,
        changeset,
    } = params;
    apply(ctx, &changeset, None, move |ws, op_ctx| {
        ops::question(ws, op_ctx, &path, &question)
    })
    .await
}

/// `okf.py unquestion`: removes a queued question by number or text,
/// deleting `open_questions` when none are left.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and the question.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`]; [`OkfError::Refused`] when there is no such question.
pub(crate) async fn unquestion(
    ctx: &ToolContext,
    params: UnquestionParams,
) -> Result<WriteReport, OkfError> {
    let UnquestionParams {
        path,
        question,
        changeset,
    } = params;
    let question = question.into_argument();
    apply(ctx, &changeset, None, move |ws, op_ctx| {
        ops::unquestion(ws, op_ctx, &path, &question)
    })
    .await
}

/// `okf.py source`: appends an entry to `sources`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and the entry.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`]; [`OkfError::Refused`] for a duplicate id.
pub(crate) async fn add_source(
    ctx: &ToolContext,
    params: AddSourceParams,
) -> Result<WriteReport, OkfError> {
    let AddSourceParams {
        path,
        id,
        resource,
        title,
        author,
        changeset,
    } = params;
    apply(ctx, &changeset, None, move |ws, op_ctx| {
        let entry = SourceEntry {
            id: &id,
            resource: &resource,
            title: title.as_deref(),
            author: author.as_deref(),
        };
        ops::source(ws, op_ctx, &path, &entry)
    })
    .await
}

/// Removes an entry from `sources` by id (the server's inverse of
/// `okf.py source`), deleting the key when none are left.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and the entry's id.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`]; [`OkfError::Refused`] when no entry has the id, or a
/// footnote in the body still cites it.
pub(crate) async fn remove_source(
    ctx: &ToolContext,
    params: RemoveSourceParams,
) -> Result<WriteReport, OkfError> {
    let RemoveSourceParams {
        path,
        id,
        changeset,
    } = params;
    apply(ctx, &changeset, None, move |ws, op_ctx| {
        ops::remove_source(ws, op_ctx, &path, &id)
    })
    .await
}

/// `okf.py log`: adds a bullet under today's (UTC) heading in `log.md`.
/// Its replay op is stored on the `log.md` entry.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the text and kind (default [`LogKind::Update`]).
///
/// # Returns
/// The write's report; nothing touched if the bullet is already there today.
///
/// # Errors
/// As [`apply`]; [`OkfError::NotFound`] for a catalog without `log.md`.
pub(crate) async fn log(ctx: &ToolContext, params: LogParams) -> Result<WriteReport, OkfError> {
    let LogParams {
        text,
        kind,
        changeset,
    } = params;
    let kind = kind.unwrap_or_default();
    apply(ctx, &changeset, None, move |ws, op_ctx| {
        ops::log(ws, op_ctx, kind, &text)
    })
    .await
}

/// `okf.py index`: lists a concept in its folder's `index.md`. Its replay
/// op is stored on the `index.md` entry.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, summary, section and title.
///
/// # Returns
/// The write's report; nothing touched if the index already links it.
///
/// # Errors
/// As [`stamp`]; [`OkfError::Refused`] for a folder without `index.md`, a
/// concept without a title or a missing `section` heading.
pub(crate) async fn index(ctx: &ToolContext, params: IndexParams) -> Result<WriteReport, OkfError> {
    let IndexParams {
        path,
        summary,
        section,
        title,
        changeset,
    } = params;
    apply(ctx, &changeset, None, move |ws, op_ctx| {
        ops::index(
            ws,
            op_ctx,
            &path,
            &summary,
            section.as_deref(),
            title.as_deref(),
        )
    })
    .await
}

// The bookkeeping tools; `OkfServer::tools` joins `bookkeeping_router()`.
#[tool_router(router = bookkeeping_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `stamp` tool.
    #[tool(
        title = "Stamp Concept",
        description = "okf.py stamp: set a concept's `generated` (content written) or append to \
                       its `verified` (checked against the real system) with { by, at: now }, in \
                       the changeset `changeset` names (open one with open_changeset). `by` \
                       defaults to the key's actor. Write access.",
        annotations(
            title = "Stamp Concept",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn stamp(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<StampParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(stamp(&ctx, params).await)
    }

    /// The `review` tool.
    #[tool(
        title = "Record Review",
        description = "okf.py review: replace a concept's `reviewed` with { by, at: now, outcome, \
                       note } and queue any `questions` in open_questions, in the changeset \
                       `changeset` names (open one with open_changeset). `by` defaults to the \
                       key's actor. Write access.",
        annotations(
            title = "Record Review",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn review(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ReviewParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(review(&ctx, params).await)
    }

    /// The `question` tool.
    #[tool(
        title = "Add Question",
        description = "okf.py question: queue one question in a concept's open_questions without \
                       recording a review, in the changeset `changeset` names (open one with \
                       open_changeset). Write access.",
        annotations(
            title = "Add Question",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn question(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<QuestionParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(question(&ctx, params).await)
    }

    /// The `unquestion` tool.
    #[tool(
        title = "Resolve Question",
        description = "okf.py unquestion: remove one queued question, by its 1-based number in \
                       open_questions or its exact text, deleting the key when none are left, in \
                       the changeset `changeset` names (open one with open_changeset). Write \
                       access.",
        annotations(
            title = "Resolve Question",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn unquestion(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<UnquestionParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(unquestion(&ctx, params).await)
    }

    /// The `add_source` tool.
    #[tool(
        title = "Add Source",
        description = "okf.py source: append an entry (id, resource, title, author) to a \
                       concept's `sources`, refusing a duplicate id, in the changeset `changeset` \
                       names (open one with open_changeset). Write access.",
        annotations(
            title = "Add Source",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn add_source(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<AddSourceParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(add_source(&ctx, params).await)
    }

    /// The `remove_source` tool.
    #[tool(
        title = "Remove Source",
        description = "Remove the entry with `id` from a concept's `sources`, deleting the key \
                       when none are left, in the changeset `changeset` names (open one with \
                       open_changeset). Refused while a footnote in the body still cites it (its \
                       label is the id, or its definition names the id, title or resource): \
                       remove the citation first. Write access.",
        annotations(
            title = "Remove Source",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn remove_source(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<RemoveSourceParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(remove_source(&ctx, params).await)
    }

    /// The `log` tool.
    #[tool(
        title = "Add Log Entry",
        description = "okf.py log: add `* **<kind>:** <text>` under today's UTC date in log.md, \
                       the only way to write log.md, in the changeset `changeset` names (open one \
                       with open_changeset). Replayed at publish onto a log.md that changed \
                       meanwhile, so concurrent entries never conflict. Write access.",
        annotations(
            title = "Add Log Entry",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn log(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<LogParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(log(&ctx, params).await)
    }

    /// The `index` tool.
    #[tool(
        title = "Add Index Entry",
        description = "okf.py index: list a concept in its folder's index.md as `* \
                       [Title](name.md) - summary`, in the changeset `changeset` names (open one \
                       with open_changeset); nothing happens if it is already linked. Replayed at \
                       publish onto an index.md that changed meanwhile, unless a free edit of \
                       that index.md follows. Write access.",
        annotations(
            title = "Add Index Entry",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn index(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<IndexParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(index(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, changeset_row, open, writer, TestApp};
    use crate::store::changesets;

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n\nOld text.\n";

    async fn pg() -> TestApp {
        app_with(
            &[(
                "pg",
                &[
                    (
                        "index.md",
                        "# Catalog\n\n# Sections\n\n* [Infra](infra/index.md)\n",
                    ),
                    ("a.md", NOTE),
                    ("infra/index.md", "# Infra\n"),
                    ("log.md", "# Log\n"),
                ],
            )],
            Config::default(),
        )
        .await
    }

    /// The text of `path` as reads through the changeset `handle` see it.
    async fn seen(ctx: &ToolContext, handle: &str, path: &str) -> String {
        let state = ctx.app().catalog("pg").unwrap();
        let pinned = ctx.pin(Some(handle)).await.unwrap();
        let view = ctx.view(&state, pinned.as_ref()).await.unwrap();
        view.snapshot()
            .get(path)
            .map(|doc| doc.text().to_string())
            .unwrap_or_default()
    }

    fn edit(handle: &str, path: &str, old_str: &str, new_str: &str) -> EditDocumentParams {
        EditDocumentParams {
            path: path.to_string(),
            old_str: old_str.to_string(),
            new_str: new_str.to_string(),
            replace_all: false,
            changeset: handle.to_string(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_content_tools_edit_through_the_overlay() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let frontmatter =
            serde_json::json!({"type": "System", "title": "NTP", "description": "Hosts sync."});

        let created = create_concept(
            &ctx,
            CreateConceptParams {
                path: "/infra/ntp.md".to_string(),
                frontmatter: frontmatter.as_object().unwrap().clone(),
                body: "# NTP\n\nSee [A](/a.md).\n".to_string(),
                index_summary: None,
                index_section: None,
                by: None,
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        let edited = edit_document(&ctx, edit(&handle, "/a.md", "Old text.", "New text."))
            .await
            .unwrap();
        replace_body(
            &ctx,
            ReplaceBodyParams {
                path: "/infra/ntp".to_string(),
                body: "# NTP\n\nRewritten.\n".to_string(),
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        set_frontmatter(
            &ctx,
            SetFrontmatterParams {
                path: "/a.md".to_string(),
                key: "status".to_string(),
                value: serde_json::json!("draft"),
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        let mismatch = edit_document(&ctx, edit(&handle, "/a.md", "not there", "x"))
            .await
            .unwrap_err();

        assert_eq!(created.touched, ["/infra/ntp.md"]);
        assert_eq!(edited.changeset, created.changeset);
        let ntp = seen(&ctx, &handle, "infra/ntp.md").await;
        // `by` defaulted to the key's actor in the `generated` stamp.
        assert!(
            ntp.contains("generated:") && ntp.contains("writer-a"),
            "{ntp}"
        );
        assert!(ntp.ends_with("# NTP\n\nRewritten.\n"), "{ntp}");
        let a = seen(&ctx, &handle, "a.md").await;
        assert!(
            a.contains("status: draft") && a.contains("New text."),
            "{a}"
        );
        assert!(matches!(
            mismatch,
            OkfError::EditMismatch { matches: 0, .. }
        ));
        let entries = changesets::entries(test.app.pool(), changeset_row(&test, &handle).await.id)
            .await
            .unwrap();
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "infra/ntp.md"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn edit_document_refuses_log_md() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;

        let log = edit_document(&ctx, edit(&handle, "/log.md", "# Log", "# Mine"))
            .await
            .unwrap_err();
        // A concept reference without `.md` is not a document path here.
        let bare = edit_document(&ctx, edit(&handle, "/a", "Old text.", "New."))
            .await
            .unwrap_err();

        assert_eq!(log.kind(), "InvalidPath");
        assert_eq!(bare.kind(), "InvalidPath");
    }

    #[test]
    fn set_frontmatter_requires_value_and_only_an_explicit_null_deletes() {
        let missing = serde_json::json!({"path": "/a", "key": "status", "changeset": "h"});
        let null =
            serde_json::json!({"path": "/a", "key": "status", "value": null, "changeset": "h"});

        let refused = serde_json::from_value::<SetFrontmatterParams>(missing);
        let delete = serde_json::from_value::<SetFrontmatterParams>(null).unwrap();
        let tools = OkfServer::writes_router().list_all();
        let tool = tools
            .iter()
            .find(|tool| tool.name == "set_frontmatter")
            .unwrap();

        let error = refused.unwrap_err().to_string();
        assert!(error.contains("missing field `value`"), "{error}");
        assert_eq!(delete.value, serde_json::Value::Null);
        let required = tool.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|name| name == "value"), "{required:?}");
    }

    #[test]
    fn the_write_tools_have_object_output_schemas_and_say_who_by_defaults_to() {
        let tools = OkfServer::writes_router().list_all();

        for name in [
            "create_concept",
            "edit_document",
            "replace_body",
            "set_frontmatter",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name}"));
            let output = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(output["type"], "object", "{name}");
        }
        let create = tools
            .iter()
            .find(|tool| tool.name == "create_concept")
            .unwrap();
        let by = create.input_schema["properties"]["by"]["description"]
            .as_str()
            .unwrap();
        // The phrase sits on one doc-comment line (schemars keeps line breaks).
        assert!(by.contains("default: the key's actor"), "{by}");
        // The catalog comes from the changeset (M8 Decision 5).
        for tool in &tools {
            let required = tool.input_schema["required"].as_array().unwrap();
            let inputs = tool.input_schema["properties"].as_object().unwrap();
            assert!(
                required.iter().any(|name| name == "changeset") && !inputs.contains_key("catalog"),
                "{}",
                tool.name
            );
        }
    }
}

#[cfg(test)]
mod bookkeeping_tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, changeset_row, open, writer, TestApp};
    use crate::store::changesets::{self, EntryRow};

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
    const NTP: &str = "---\ntype: System\ntitle: NTP\ndescription: Hosts sync.\n---\n# NTP\n";

    async fn pg() -> TestApp {
        app_with(
            &[(
                "pg",
                &[
                    ("a.md", NOTE),
                    (
                        "infra/index.md",
                        "# Infra\n\n* [Time server](time.md) - the GPS clock\n",
                    ),
                    ("infra/time.md", NOTE),
                    ("infra/ntp.md", NTP),
                    ("log.md", "# Log\n"),
                ],
            )],
            Config::default(),
        )
        .await
    }

    /// The entry for `path` of the changeset `handle` names.
    async fn entry(test: &TestApp, handle: &str, path: &str) -> EntryRow {
        let row = changeset_row(test, handle).await;
        changesets::entries(test.app.pool(), row.id)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("no entry for {path}"))
    }

    /// The text of `path` as reads through the changeset `handle` see it.
    async fn seen(ctx: &ToolContext, handle: &str, path: &str) -> String {
        let state = ctx.app().catalog("pg").unwrap();
        let pinned = ctx.pin(Some(handle)).await.unwrap();
        let view = ctx.view(&state, pinned.as_ref()).await.unwrap();
        view.snapshot()
            .get(path)
            .map(|doc| doc.text().to_string())
            .unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn log_records_its_replay_on_the_log_md_entry() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;

        let report = log(
            &ctx,
            LogParams {
                text: "hello   world".to_string(),
                kind: None,
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();

        assert_eq!(report.touched, ["/log.md"]);
        let stored = entry(&test, &report.changeset, "log.md").await;
        let replay = stored.replay.expect("a log entry is replayable");
        // `kind` defaulted to Update; okf.py collapses the whitespace.
        assert_eq!(replay[0]["op"], "log");
        assert_eq!(replay[0]["bullet"], "* **Update:** hello world");
        assert!(seen(&ctx, &handle, "log.md")
            .await
            .contains("* **Update:** hello world"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_free_edit_of_an_index_after_index_clears_its_replay() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;

        let listed = index(
            &ctx,
            IndexParams {
                path: "/infra/ntp.md".to_string(),
                summary: "how hosts sync".to_string(),
                section: None,
                title: None,
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        let after_index = entry(&test, &listed.changeset, "infra/index.md").await;
        edit_document(
            &ctx,
            EditDocumentParams {
                path: "/infra/index.md".to_string(),
                old_str: "the GPS clock".to_string(),
                new_str: "the GPS stratum 1 clock".to_string(),
                replace_all: false,
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        let after_edit = entry(&test, &listed.changeset, "infra/index.md").await;

        assert_eq!(
            after_index.replay.expect("index is replayable")[0]["op"],
            "list_bullet"
        );
        assert_eq!(after_edit.replay, None);
        let text = seen(&ctx, &handle, "infra/index.md").await;
        assert!(
            text.contains("[NTP](ntp.md) - how hosts sync") && text.contains("stratum 1"),
            "{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_concept_commands_edit_frontmatter_as_free_edits() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let path = || "/a.md".to_string();
        let ask = |question: &str| QuestionParams {
            path: path(),
            question: question.to_string(),
            changeset: handle.clone(),
        };

        stamp(
            &ctx,
            StampParams {
                path: path(),
                field: StampField::Verified,
                by: Some("human:pgrace".to_string()),
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        question(&ctx, ask("Who owns it?")).await.unwrap();
        question(&ctx, ask("Is it backed up?")).await.unwrap();
        // A number is the question's 1-based position, as `open_questions` numbers it.
        unquestion(
            &ctx,
            UnquestionParams {
                path: path(),
                question: QuestionRef::Number(1),
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        review(
            &ctx,
            ReviewParams {
                path: path(),
                outcome: "verified".to_string(),
                note: "checked".to_string(),
                questions: None,
                by: None,
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();
        for (id, resource) in [
            ("runbook", "https://example.com/runbook"),
            ("spare", "https://example.com/spare"),
        ] {
            add_source(
                &ctx,
                AddSourceParams {
                    path: path(),
                    id: id.to_string(),
                    resource: resource.to_string(),
                    title: Some("Runbook".to_string()),
                    author: None,
                    changeset: handle.clone(),
                },
            )
            .await
            .unwrap();
        }
        let last = remove_source(
            &ctx,
            RemoveSourceParams {
                path: path(),
                id: "spare".to_string(),
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap();

        let text = seen(&ctx, &handle, "a.md").await;
        assert!(
            text.contains("verified:") && text.contains("human:pgrace"),
            "{text}"
        );
        assert!(
            text.contains("reviewed:") && text.contains("writer-a"),
            "{text}"
        );
        assert!(
            text.contains("Is it backed up?") && !text.contains("Who owns it?"),
            "{text}"
        );
        assert!(text.contains("https://example.com/runbook"), "{text}");
        assert!(!text.contains("spare"), "{text}");
        // Frontmatter edits carry no replay: they conflict on a stale base.
        assert_eq!(last.touched, ["/a.md"]);
        assert_eq!(entry(&test, &last.changeset, "a.md").await.replay, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_op_is_a_tool_error_and_stores_nothing() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;

        let outcome = review(
            &ctx,
            ReviewParams {
                path: "/a.md".to_string(),
                outcome: "current".to_string(),
                note: "checked".to_string(),
                questions: None,
                by: None,
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap_err();
        // The fixture has no root index.md, so a root concept cannot be listed.
        let unlisted = index(
            &ctx,
            IndexParams {
                path: "/a.md".to_string(),
                summary: "a note".to_string(),
                section: None,
                title: None,
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap_err();
        // `a.md` has no open_questions, so any number is refused.
        let none_queued = unquestion(
            &ctx,
            UnquestionParams {
                path: "/a.md".to_string(),
                question: QuestionRef::Number(3),
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap_err();

        // `a.md` has no sources, so no id is there to remove.
        let no_source = remove_source(
            &ctx,
            RemoveSourceParams {
                path: "/a.md".to_string(),
                id: "runbook".to_string(),
                changeset: handle.clone(),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(outcome.kind(), "Refused", "{outcome}");
        assert_eq!(unlisted.kind(), "Refused", "{unlisted}");
        assert_eq!(none_queued.kind(), "Refused", "{none_queued}");
        assert_eq!(no_source.kind(), "Refused", "{no_source}");
        // `apply` rolls back on an op error: the changeset holds nothing.
        let row = changeset_row(&test, &handle).await;
        assert!(changesets::entries(test.app.pool(), row.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unquestion_takes_a_number_or_text_and_log_takes_okf_py_kinds() {
        let number = serde_json::json!({"path": "/a", "question": 2, "changeset": "h"});
        let text = serde_json::json!({"path": "/a", "question": "Who owns it?", "changeset": "h"});
        let lower = serde_json::json!({"text": "x", "kind": "update", "changeset": "h"});

        let number = serde_json::from_value::<UnquestionParams>(number).unwrap();
        let text = serde_json::from_value::<UnquestionParams>(text).unwrap();
        let change = serde_json::from_value::<LogParams>(serde_json::json!({
            "text": "x",
            "kind": "Change",
            "changeset": "h"
        }))
        .unwrap();
        let refused = serde_json::from_value::<LogParams>(lower);

        assert_eq!(number.question.into_argument(), "2");
        assert_eq!(text.question.into_argument(), "Who owns it?");
        assert_eq!(change.kind, Some(LogKind::Change));
        // okf.py's argparse `choices` are case-sensitive, and so is this.
        assert!(refused.is_err());
    }

    #[test]
    fn the_bookkeeping_tools_are_listed_with_their_defaults() {
        let tools = OkfServer::bookkeeping_router().list_all();
        // `&*` borrows the name (a `Cow<str>`) as a plain `&str`.
        let names: Vec<&str> = tools.iter().map(|tool| &*tool.name).collect();
        let log = tools.iter().find(|tool| tool.name == "log").unwrap();
        let kind = log.input_schema["properties"]["kind"]["description"]
            .as_str()
            .unwrap();

        for name in [
            "stamp",
            "review",
            "question",
            "unquestion",
            "add_source",
            "remove_source",
            "log",
            "index",
        ] {
            assert!(names.contains(&name), "{name} is registered");
        }
        assert_eq!(names.len(), 8, "{names:?}");
        assert!(kind.contains("default Update"), "{kind}");
        for tool in &tools {
            let output = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(output["type"], "object", "{}", tool.name);
            // No tool takes a timestamp: every one comes from the server clock.
            let properties = tool.input_schema["properties"].as_object().unwrap();
            assert!(
                !properties.keys().any(|key| key == "at" || key == "now"),
                "{}",
                tool.name
            );
        }
    }
}
