//! The regions of the throne room, and the views routed inside it.

mod browser_view;
mod city_rail;
mod conversation;
mod diff_view;
mod file_tree;
mod map;
mod markdown;
mod note_composer;
mod prompt_bar;
mod proposal;
mod resizer;
pub mod review_drawer;
mod review_notes;
mod sidebar;
mod source_view;

pub use browser_view::BrowserView;
pub use city_rail::CityRail;
pub use conversation::Conversation;
pub use diff_view::DiffView;
pub use file_tree::FileTree;
pub use map::KingdomMap;
pub use markdown::Prose;
pub use prompt_bar::PromptBar;
pub use proposal::ProposalCard;
pub use review_drawer::ReviewDrawer;
pub use review_notes::ReviewMargin;
pub use sidebar::Sidebar;
pub use source_view::SourceView;
