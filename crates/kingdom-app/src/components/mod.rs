//! The regions of the throne room, and the views routed inside it.

mod browser_view;
mod city_rail;
mod conversation;
mod diff_view;
mod file_tree;
mod markdown;
mod note_composer;
mod ports_badge;
mod prompt_bar;
mod proposal;
mod resizer;
pub mod review_drawer;
mod review_notes;
mod sidebar;
mod source_view;
mod terminal_view;
mod wells;

pub use browser_view::BrowserView;
pub use city_rail::CityRail;
pub use conversation::Conversation;
pub use diff_view::DiffView;
pub use file_tree::FileTree;
pub use markdown::Prose;
pub use ports_badge::PortsBadge;
pub use prompt_bar::PromptBar;
pub use proposal::ProposalCard;
pub use review_drawer::ReviewDrawer;
pub use review_notes::ReviewMargin;
pub use sidebar::Sidebar;
pub use source_view::SourceView;
pub use terminal_view::TerminalView;
pub use wells::SharedResourcesView;
