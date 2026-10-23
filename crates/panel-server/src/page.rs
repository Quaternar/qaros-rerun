const PAGE_TEMPLATE: &str = include_str!("page.html");

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

    /// Runs `tests/page_retry.mjs` on the page with Node; skipped where `node` is missing.
    #[test]
    fn the_page_retries_with_back_off_and_resumes_when_frames_load_again() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/page_retry.mjs");
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
            .write_all(view_page("camera").as_bytes())
            .expect("page written to node");
        let output = node.wait_with_output().expect("node finished");
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "page retry: ok\n");
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
