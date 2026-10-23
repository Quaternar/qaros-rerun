//! Route B for 2D views as QAROS GUI panels: the viewer renders a view off screen, this server
//! hands the latest frame to a minimal web page, and a QAROS panel navigates to that page
//! (`gui-panels.mdx` L14, L119: panel content is a URI).
//!
//! Each view gets a page at `/views/<name>` that keeps fetching `/views/<name>/frame.png` and
//! swaps it in once loaded. `/views` lists the published views as JSON.

mod page;
mod server;
mod test_pattern;

pub use server::{PanelServer, PanelServerError, PublishedFrame, ViewFrames};
pub use test_pattern::TestPattern;
