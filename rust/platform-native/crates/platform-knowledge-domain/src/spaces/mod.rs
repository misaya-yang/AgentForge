//! Knowledge spaces: a space binds one hidden dataset and its items stay in this context.
mod authorize;
mod index;
mod markdown;
mod model;
mod repository;

pub use index::{IndexPlan, IndexSnapshot, connector_acl, document_metadata, plan, stamp_hit, viewer_can_retrieve};
pub use markdown::{
    blocks_to_html, blocks_to_markdown, html_to_blocks, line_diff, markdown_to_blocks, markdown_to_html,
    project, read_zip_pages,
};
pub use model::{
    ContentPut, ImportBody, Item, ItemContent, ItemFilter, ItemPage, ItemPatch, ItemView, Member,
    RevisionMeta,
    MemberInput, MemberPut, NewItem, NewSpace, SharePut, Space, SpaceError, SpacePatch, SpaceResult,
    bounded_name, content_digest, page_limit, parse_view,
};
pub use repository::{RetrievalRef, Spaces, decode_cursor, retrieval_refs};
