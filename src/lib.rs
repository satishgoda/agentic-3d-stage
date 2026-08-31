//! thinner-floor document store, mailbox, and spatial look queries.

pub mod beauty;
pub mod catalog;
pub mod camera;
pub mod capabilities;
pub mod cycles_stream;
pub mod cycles_xml;
pub mod document;
pub mod export;
pub mod feed;
pub mod hud;
pub mod geom;
pub mod layout;
pub mod light;
pub mod mailbox;
pub mod mcp;
pub mod play;
pub mod rtx;
pub mod query;

pub use document::{
    ApplyChange, ApplyRequest, ApplyResult, Document, DocumentStore, Entity, Material, MeshRecipe,
    Scene, Transform, DEFAULT_DOCUMENT_PATH, DEFAULT_MAILBOX_ADDR, DEFAULT_TOKEN_PATH,
};
pub use feed::MailboxFeed;
pub use mailbox::{
    HistoryAction, MailboxClient, MailboxRequest, MailboxResponse, MailboxServer, ProjectAction,
    DEFAULT_LOCAL_NAME,
};
pub use query::{ColorQuery, QueryHit, QueryResult, QuerySpec};
