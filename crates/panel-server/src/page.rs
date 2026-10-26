const PAGE_TEMPLATE: &str = include_str!("page.html");
const TIMELINE_PAGE_TEMPLATE: &str = include_str!("timeline_page.html");

/// Where the timeline panel's page is served.
pub const TIMELINE_PAGE_PATH: &str = "/timeline-panel";
/// Where the timeline page sends its seeks (`?at=<fraction>`) and play requests (`?play`).
pub const TIMELINE_REQUEST_PATH: &str = "/timeline";
/// Where the timeline page reads the viewer's time cursor: an image whose width encodes it.
pub const TIMELINE_POSITION_PATH: &str = "/timeline/position";

/// View names become URL path segments and page titles, so they are restricted to characters
/// that need no escaping in either.
pub fn is_valid_view_name(view_name: &str) -> bool {
    !view_name.is_empty()
        && view_name.len() <= 64
        && view_name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

pub fn view_page(view_name: &str) -> String {
    PAGE_TEMPLATE
        .replace("${VIEW_NAME}", view_name)
        .replace("${FRAME_URL}", &format!("/views/{view_name}/frame.png"))
}

pub fn timeline_page() -> String {
    TIMELINE_PAGE_TEMPLATE
        .replace("${TIMELINE_URL}", TIMELINE_REQUEST_PATH)
        .replace("${POSITION_URL}", TIMELINE_POSITION_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_points_at_the_view_frame_url() {
        let page = view_page("time_series");
        assert!(page.contains("var frameUrl = \"/views/time_series/frame.png\";"));
        assert!(page.contains("<title>time_series</title>"));
        assert!(!page.contains("${"));
    }

    #[test]
    fn page_loads_frames_as_plain_images() {
        let page = view_page("map");
        assert!(
            page.contains(
                "preloader.src = frameUrl + \"?n=\" + pageToken + \"-\" + requestNumber;"
            )
        );
        assert!(page.contains("shownElement.src = preloader.src;"));
        assert!(
            page.contains("preloader.decode().then(showPreloadedFrame, showPreloadedFrame);"),
            "a frame is decoded before it is shown"
        );
        assert!(page.contains("<img id=\"frame\" alt=\"map\" decoding=\"sync\">"));
        assert_eq!(
            page.matches("<img").count(),
            1,
            "one image element: the QAROS panel renderer draws every image, hidden or not"
        );
        for unsupported_in_embedded_browsers in ["fetch(", "Blob", "createObjectURL"] {
            assert!(
                !page.contains(unsupported_in_embedded_browsers),
                "{unsupported_in_embedded_browsers}"
            );
        }
    }

    /// Runs `tests/page_retry.mjs` on the view page with Node; skipped where `node` is missing.
    #[test]
    fn the_page_retries_with_back_off_and_resumes_when_frames_load_again() {
        run_page_check(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/page_retry.mjs"),
            &view_page("camera"),
            "page retry: ok\n",
        );
    }

    /// Runs `tests/page_scrub.mjs` on the timeline page with Node; skipped where `node` is
    /// missing.
    #[test]
    fn the_timeline_page_sends_thinned_seeks_and_play_for_mouse_and_touch() {
        run_page_check(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/page_scrub.mjs"),
            &timeline_page(),
            "page scrub: ok\n",
        );
    }

    /// Runs `tests/page_position.mjs` on the timeline page with Node; skipped where `node` is
    /// missing.
    #[test]
    fn the_timeline_thumb_follows_the_polled_position_except_under_the_finger() {
        run_page_check(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/page_position.mjs"),
            &timeline_page(),
            "page position: ok\n",
        );
    }

    #[test]
    fn the_view_fills_its_page_without_a_slider() {
        let page = view_page("map");
        for slider_part in ["id=\"track\"", "id=\"thumb\"", "id=\"play\"", "timelineUrl"] {
            assert!(!page.contains(slider_part), "{slider_part}");
        }
        let frame_style = css_rule(&page, "#frame");
        assert!(frame_style.contains("width: 100%;"), "{frame_style}");
        assert!(frame_style.contains("height: 100%;"), "{frame_style}");
    }

    #[test]
    fn the_slider_and_play_button_fill_the_timeline_page_edge_to_edge() {
        let page = timeline_page();
        assert!(page.contains("var timelineUrl = \"/timeline\";"));
        assert!(page.contains("var positionUrl = \"/timeline/position\";"));
        assert!(!page.contains("${"));
        assert!(
            !css_rule(&page, "#thumb").contains("visibility"),
            "the thumb always shows the viewer's time"
        );
        assert_eq!(
            page.matches("<img").count(),
            0,
            "no view on the timeline page"
        );
        let scrubber_style = css_rule(&page, "#scrubber");
        for edge in ["left: 0;", "top: 0;", "width: 100%;", "height: 100%;"] {
            assert!(scrubber_style.contains(edge), "{edge} in {scrubber_style}");
        }
        let track_style = css_rule(&page, "#track");
        for edge in ["left: 0;", "right: 20%;", "top: 0;", "bottom: 0;"] {
            assert!(track_style.contains(edge), "{edge} in {track_style}");
        }
        let play_style = css_rule(&page, "#play");
        for edge in ["right: 0;", "top: 0;", "width: 20%;", "height: 100%;"] {
            assert!(play_style.contains(edge), "{edge} in {play_style}");
        }
        for unsupported_in_embedded_browsers in ["fetch(", "XMLHttpRequest", "Blob"] {
            assert!(
                !page.contains(unsupported_in_embedded_browsers),
                "{unsupported_in_embedded_browsers}"
            );
        }
    }

    fn css_rule<'page>(page: &'page str, selector: &str) -> &'page str {
        page.split(&format!("{selector} {{"))
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .unwrap_or_else(|| panic!("a {selector} rule"))
    }

    fn run_page_check(script: &str, page: &str, expected_output: &str) {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let spawned = Command::new("node")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(mut node) = spawned else {
            eprintln!("skipped: no `node` to run {script}");
            return;
        };
        node.stdin
            .take()
            .expect("node's stdin")
            .write_all(page.as_bytes())
            .expect("page written to node");
        let output = node.wait_with_output().expect("node finished");
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected_output);
    }

    #[test]
    fn view_names_are_url_safe() {
        assert!(is_valid_view_name("image-2d_0"));
        assert!(!is_valid_view_name(""));
        assert!(!is_valid_view_name("a/b"));
        assert!(!is_valid_view_name("<script>"));
        assert!(!is_valid_view_name(&"x".repeat(65)));
    }
}
