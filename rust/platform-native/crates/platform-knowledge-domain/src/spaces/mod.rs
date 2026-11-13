//! Knowledge spaces: a space binds one hidden dataset and its items stay in this context.
mod authorize;
mod model;
mod repository;

pub use model::{
    ContentPut, Item, ItemContent, ItemFilter, ItemPage, ItemPatch, ItemView, Member, MemberInput,
    MemberPut, NewItem, NewSpace, SharePut, Space, SpaceError, SpacePatch, SpaceResult,
    bounded_name, content_digest, page_limit, parse_view,
};
pub use repository::{Spaces, decode_cursor};
