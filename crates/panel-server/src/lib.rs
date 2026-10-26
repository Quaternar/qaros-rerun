//! Route B for 2D views as QAROS GUI panels: the viewer renders a view off screen, this server
//! hands the latest frame to a minimal web page, and a QAROS panel navigates to that page
//! (`gui-panels.mdx` L14, L119: panel content is a URI).
//!
//! Each view gets a page at `/views/<name>` that keeps fetching `/views/<name>/frame.png` and
//! swaps it in once loaded. `/views` lists the published views as JSON.
//!
//! The timeline panel's page at `/timeline-panel` holds a slider and a Play button: dragging the
//! slider sends `/timeline?at=<fraction>`, and Play `/timeline?play`, which the viewer takes
//! from [`TimelineRequests`]. The viewer publishes its time cursor to [`TimelinePosition`], and
//! the page polls it from `/timeline/position` so the thumb follows playback and the desktop
//! time bar.

mod page;
mod server;
mod test_pattern;
mod timeline;

pub use page::TIMELINE_PAGE_PATH;
pub use server::{PanelServer, PanelServerError, PublishedFrame, ViewFrames};
pub use test_pattern::TestPattern;
pub use timeline::{TimelinePosition, TimelineRequest, TimelineRequests};
