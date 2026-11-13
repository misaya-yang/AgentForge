//! Space and item shapes. Block JSON is stored as given; markdown is the projection.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub enum SpaceError {
    NotFound,
    Conflict,
    Invalid(&'static str),
    SchemaUnavailable,
    Unavailable,
    Precondition,
}

pub type SpaceResult<T> = Result<T, SpaceError>;

#[derive(Debug, Clone, Serialize)]
pub struct Space {
    pub space_id: String,
    pub name: String,
    pub icon: String,
    pub kind: String,
    pub visibility: String,
    pub dataset_id: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub item_id: String,
    pub space_id: String,
    pub space_name: String,
    pub space_icon: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub icon: String,
    pub position: String,
    pub source: String,
    pub created_by: String,
    pub updated_by: String,
    pub content_format: String,
    pub current_revision: i64,
    pub starred: bool,
    pub shared: bool,
    pub last_activity_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemPage {
    pub items: Vec<Item>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub principal_type: String,
    pub principal_id: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemContent {
    pub item_id: String,
    pub revision: i64,
    pub content_format: String,
    pub blocks: Value,
    pub markdown: String,
    pub content_sha256: String,
}

#[derive(Debug, Deserialize)]
pub struct NewSpace {
    pub name: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default = "personal")]
    pub kind: String,
    #[serde(default = "private_visibility")]
    pub visibility: String,
}

#[derive(Debug, Deserialize)]
pub struct SpacePatch {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub kind: Option<String>,
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewItem {
    pub title: String,
    #[serde(default = "page_kind")]
    pub kind: String,
    #[serde(default)]
    pub icon: String,
    pub parent_id: Option<String>,
    pub position: Option<String>,
    #[serde(default = "markdown_format")]
    pub content_format: String,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemPatch {
    pub title: Option<String>,
    pub icon: Option<String>,
    pub parent_id: Option<String>,
    pub clear_parent: Option<bool>,
    pub position: Option<String>,
    pub space_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContentPut {
    pub content_format: String,
    #[serde(default = "null_blocks")]
    pub blocks: Value,
    #[serde(default)]
    pub markdown: String,
}

#[derive(Debug, Deserialize)]
pub struct MemberPut {
    pub members: Vec<MemberInput>,
}

#[derive(Debug, Deserialize)]
pub struct MemberInput {
    pub principal_type: String,
    pub principal_id: String,
    pub role: String,
}

#[derive(Debug, Deserialize)]
pub struct SharePut {
    pub shares: Vec<MemberInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevisionMeta {
    pub revision: i64,
    pub author: String,
    pub created_at: String,
    pub content_sha256: String,
}

#[derive(Debug, Deserialize)]
pub struct ImportBody {
    pub format: String,
    #[serde(default)]
    pub filename: String,
    pub content_base64: String,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemView {
    All,
    Recent,
    Starred,
    Mine,
    Shared,
    Trash,
}

#[derive(Debug, Clone)]
pub struct ItemFilter {
    pub view: ItemView,
    pub kind: Option<String>,
    pub query: Option<String>,
    pub space_id: Option<String>,
    pub cursor: Option<(String, String)>,
    pub limit: i64,
}

fn personal() -> String {
    "personal".into()
}
fn private_visibility() -> String {
    "private".into()
}
fn page_kind() -> String {
    "page".into()
}
fn markdown_format() -> String {
    "markdown".into()
}
fn null_blocks() -> Value {
    Value::Null
}

pub fn parse_view(value: Option<&str>, trash: bool) -> SpaceResult<ItemView> {
    if trash {
        return Ok(ItemView::Trash);
    }
    match value.unwrap_or("all") {
        "all" => Ok(ItemView::All),
        "recent" => Ok(ItemView::Recent),
        "starred" => Ok(ItemView::Starred),
        "mine" => Ok(ItemView::Mine),
        "shared" => Ok(ItemView::Shared),
        _ => Err(SpaceError::Invalid("invalid item view")),
    }
}

pub fn bounded_name(value: &str) -> SpaceResult<String> {
    let value = value.trim();
    if !(1..=200).contains(&value.chars().count()) || value.chars().any(char::is_control) {
        return Err(SpaceError::Invalid("invalid space name"));
    }
    Ok(value.to_owned())
}

pub fn bounded_title(value: &str) -> SpaceResult<String> {
    let value = value.trim();
    if !(1..=500).contains(&value.chars().count()) || value.chars().any(char::is_control) {
        return Err(SpaceError::Invalid("invalid item title"));
    }
    Ok(value.to_owned())
}

pub fn bounded_icon(value: &str) -> SpaceResult<String> {
    if value.chars().count() > 32 || value.chars().any(char::is_control) {
        return Err(SpaceError::Invalid("invalid icon"));
    }
    Ok(value.to_owned())
}

pub fn space_kind(value: &str) -> SpaceResult<String> {
    if ["personal", "team", "tenant"].contains(&value) {
        Ok(value.to_owned())
    } else {
        Err(SpaceError::Invalid("invalid space kind"))
    }
}

pub fn item_kind(value: &str) -> SpaceResult<String> {
    if [
        "page", "folder", "file", "link", "slide", "site", "image", "external",
    ]
    .contains(&value)
    {
        Ok(value.to_owned())
    } else {
        Err(SpaceError::Invalid("invalid item kind"))
    }
}

pub fn visibility(value: &str) -> SpaceResult<String> {
    if ["private", "members", "tenant"].contains(&value) {
        Ok(value.to_owned())
    } else {
        Err(SpaceError::Invalid("invalid visibility"))
    }
}

pub fn content_format(value: &str) -> SpaceResult<String> {
    if ["blocknote_json", "markdown", "binary"].contains(&value) {
        Ok(value.to_owned())
    } else {
        Err(SpaceError::Invalid("invalid content format"))
    }
}

pub fn position(value: &str) -> SpaceResult<String> {
    if (1..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(value.to_owned())
    } else {
        Err(SpaceError::Invalid("invalid position"))
    }
}

pub fn principal(kind: &str, id: &str) -> SpaceResult<(String, String)> {
    if !["user", "group", "role"].contains(&kind) || id.trim().is_empty() || id.len() > 255 {
        return Err(SpaceError::Invalid("invalid principal"));
    }
    Ok((kind.to_owned(), id.trim().to_owned()))
}

pub fn member_role(value: &str) -> SpaceResult<String> {
    if ["owner", "editor", "commenter", "viewer"].contains(&value) {
        Ok(value.to_owned())
    } else {
        Err(SpaceError::Invalid("invalid member role"))
    }
}

pub fn source_label(value: &str) -> SpaceResult<String> {
    let value = if value.is_empty() { "native" } else { value };
    let connector = value
        .strip_prefix("connector:")
        .is_some_and(|rest| !rest.is_empty() && rest.len() <= 80 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-')));
    if value == "native" || value == "upload" || connector {
        Ok(value.to_owned())
    } else {
        Err(SpaceError::Invalid("invalid item source"))
    }
}

pub fn like_pattern(query: &str) -> SpaceResult<String> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 200 {
        return Err(SpaceError::Invalid("invalid search"));
    }
    let mut escaped = String::new();
    for ch in query.chars() {
        if ch.is_control() {
            return Err(SpaceError::Invalid("invalid search"));
        }
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    Ok(format!("%{escaped}%"))
}

pub fn content_digest(format: &str, blocks: &Value, markdown: &str) -> SpaceResult<String> {
    if markdown.len() > 1_000_000 || blocks.to_string().len() > 1_000_000 {
        return Err(SpaceError::Invalid("content is too large"));
    }
    let mut hasher = Sha256::new();
    hasher.update(format.as_bytes());
    hasher.update([0]);
    hasher.update(markdown.as_bytes());
    hasher.update([0]);
    hasher.update(blocks.to_string().as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn page_limit(value: Option<i64>) -> SpaceResult<i64> {
    match value.unwrap_or(50) {
        limit if (1..=200).contains(&limit) => Ok(limit),
        _ => Err(SpaceError::Invalid("invalid page limit")),
    }
}

