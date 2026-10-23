//! Serves an animated test pattern at `/views/test_pattern` for a few seconds, the stand-in
//! for an off-screen-rendered Rerun 2D view. Open the printed URL in a browser, or point a
//! QAROS GUI panel at it with `qar_gui_panels_navigate_to_uri`.
//!
//! `cargo run -p panel-server --example serve_test_pattern -- [seconds] [address]`

use std::time::{Duration, Instant};

use panel_server::{PanelServer, TestPattern};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let seconds: u64 = arguments.next().map_or(Ok(30), |text| text.parse())?;
    let address = arguments
        .next()
        .unwrap_or_else(|| "127.0.0.1:9877".to_owned());

    let server = PanelServer::start(&address)?;
    let view_name = "test_pattern";
    println!("serving {}", server.view_url(view_name).unwrap_or_default());

    let pattern = TestPattern {
        width: 640,
        height: 360,
    };
    let frame_interval = Duration::from_millis(33);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frame_index = 0;
    while Instant::now() < deadline {
        let frame = pattern.frame_rgba8(frame_index);
        server
            .frames()
            .publish_rgba8(view_name, pattern.width, pattern.height, &frame)?;
        frame_index += 1;
        std::thread::sleep(frame_interval);
    }
    println!("published {frame_index} frames");
    Ok(())
}
