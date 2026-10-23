use std::collections::BTreeMap;
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use tiny_http::{Header, Method, Request, Response, Server};

use crate::page::{is_valid_view_name, view_page};

#[derive(Debug)]
pub enum PanelServerError {
    Bind(String),
    InvalidViewName(String),
    InvalidFrame(String),
    Encode(String),
}

impl std::fmt::Display for PanelServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(reason) => write!(formatter, "cannot bind panel server: {reason}"),
            Self::InvalidViewName(name) => write!(formatter, "invalid view name {name:?}"),
            Self::InvalidFrame(reason) => write!(formatter, "invalid frame: {reason}"),
            Self::Encode(reason) => write!(formatter, "cannot encode frame: {reason}"),
        }
    }
}

impl std::error::Error for PanelServerError {}

#[derive(Clone, Debug)]
pub struct PublishedFrame {
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub png: Arc<Vec<u8>>,
}

/// Latest frame per view, shared between the publisher and the HTTP thread, and when each
/// view's page last asked for its page or a frame.
#[derive(Clone, Default)]
pub struct ViewFrames {
    frames: Arc<Mutex<BTreeMap<String, PublishedFrame>>>,
    page_requests: Arc<Mutex<BTreeMap<String, Instant>>>,
}

impl ViewFrames {
    /// Encodes an RGBA8 frame as PNG and makes it the view's latest frame.
    pub fn publish_rgba8(
        &self,
        view_name: &str,
        width: u32,
        height: u32,
        rgba8: &[u8],
    ) -> Result<u64, PanelServerError> {
        if !is_valid_view_name(view_name) {
            return Err(PanelServerError::InvalidViewName(view_name.to_owned()));
        }
        let expected_length = width as usize * height as usize * 4;
        if rgba8.len() != expected_length || width == 0 || height == 0 {
            return Err(PanelServerError::InvalidFrame(format!(
                "{} bytes for {width}x{height}",
                rgba8.len()
            )));
        }
        let mut png = Vec::new();
        image::write_buffer_with_format(
            &mut Cursor::new(&mut png),
            rgba8,
            width,
            height,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(|error| PanelServerError::Encode(error.to_string()))?;

        let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        let sequence = frames
            .get(view_name)
            .map_or(0, |previous| previous.sequence + 1);
        frames.insert(
            view_name.to_owned(),
            PublishedFrame {
                sequence,
                width,
                height,
                png: Arc::new(png),
            },
        );
        Ok(sequence)
    }

    pub fn latest(&self, view_name: &str) -> Option<PublishedFrame> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(view_name)
            .cloned()
    }

    /// When this server last got a request for the view's page or one of its frames, counted
    /// once the view is published. A page that loaded keeps asking for frames; one whose
    /// document never loaded asks for nothing.
    pub fn last_page_request(&self, view_name: &str) -> Option<Instant> {
        self.page_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(view_name)
            .copied()
    }

    /// Whether any published view's page or frame was asked for since this server started.
    pub fn any_page_request(&self) -> bool {
        !self
            .page_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Notes requests for published views only: any LAN client can ask for any valid name, and
    /// the record must not grow with what it invents.
    fn note_page_request(&self, view_name: &str) {
        let published = self
            .frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(view_name);
        if !published {
            return;
        }
        self.page_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(view_name.to_owned(), Instant::now());
    }

    pub fn view_names(&self) -> Vec<String> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }
}

/// Serves `/views` (JSON list), `/views/<name>` (the page) and `/views/<name>/frame.png`
/// from a background thread; dropped, it stops the thread.
pub struct PanelServer {
    server: Arc<Server>,
    frames: ViewFrames,
    thread: Option<JoinHandle<()>>,
}

impl PanelServer {
    /// Binds to `address`; port 0 picks a free port, see [`PanelServer::local_address`].
    pub fn start(address: &str) -> Result<Self, PanelServerError> {
        let server = Arc::new(
            Server::http(address).map_err(|error| PanelServerError::Bind(error.to_string()))?,
        );
        let frames = ViewFrames::default();
        let thread = {
            let server = Arc::clone(&server);
            let frames = frames.clone();
            std::thread::Builder::new()
                .name("panel-server".to_owned())
                .spawn(move || {
                    for request in server.incoming_requests() {
                        respond(request, &frames);
                    }
                })
                .map_err(|error| PanelServerError::Bind(error.to_string()))?
        };
        Ok(Self {
            server,
            frames,
            thread: Some(thread),
        })
    }

    /// Binds to `address`, or to a free port on the same host when that port is taken (a second
    /// instance of a demo asks for the same fixed port). Returns why the asked port was refused.
    pub fn start_or_any_free_port(
        address: &str,
    ) -> Result<(Self, Option<PanelServerError>), PanelServerError> {
        match Self::start(address) {
            Ok(server) => Ok((server, None)),
            Err(refusal) => {
                let Some((host, port)) = address.rsplit_once(':') else {
                    return Err(refusal);
                };
                if port == "0" {
                    return Err(refusal);
                }
                let server = Self::start(&format!("{host}:0"))?;
                Ok((server, Some(refusal)))
            }
        }
    }

    pub fn frames(&self) -> &ViewFrames {
        &self.frames
    }

    pub fn local_address(&self) -> Option<SocketAddr> {
        self.server.server_addr().to_ip()
    }

    pub fn view_url(&self, view_name: &str) -> Option<String> {
        self.local_address()
            .map(|address| format!("http://{address}/views/{view_name}"))
    }
}

impl Drop for PanelServer {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            eprintln!("panel server thread panicked");
        }
    }
}

enum Route<'a> {
    ViewList,
    Page(&'a str),
    Frame {
        view_name: &'a str,
        after: Option<u64>,
        /// The page names every frame request uniquely (`n=`), so that response may be cached:
        /// the page shows a frame by pointing its image at the URL its preloader just loaded.
        uniquely_named: bool,
    },
    NotFound,
}

fn route(url: &str) -> Route<'_> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match segments[..] {
        ["views"] => Route::ViewList,
        ["views", view_name] if is_valid_view_name(view_name) => Route::Page(view_name),
        ["views", view_name, "frame.png"] if is_valid_view_name(view_name) => Route::Frame {
            view_name,
            after: query
                .split('&')
                .find_map(|pair| pair.strip_prefix("after="))
                .and_then(|value| value.parse().ok()),
            uniquely_named: query.split('&').any(|pair| pair.starts_with("n=")),
        },
        _ => Route::NotFound,
    }
}

fn header(name: &str, value: &str) -> Option<Header> {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).ok()
}

fn respond(request: Request, frames: &ViewFrames) {
    let response = if *request.method() == Method::Get {
        build_response(request.url(), frames)
    } else {
        Response::from_string("method not allowed").with_status_code(405)
    };
    if let Err(error) = request.respond(response) {
        eprintln!("panel server: cannot send response: {error}");
    }
}

fn build_response(url: &str, frames: &ViewFrames) -> Response<Cursor<Vec<u8>>> {
    let no_store = header("Cache-Control", "no-store");
    match route(url) {
        Route::ViewList => {
            let names: Vec<String> = frames
                .view_names()
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect();
            Response::from_string(format!("[{}]", names.join(",")))
                .with_optional_header(header("Content-Type", "application/json"))
                .with_optional_header(no_store)
        }
        Route::Page(view_name) => {
            frames.note_page_request(view_name);
            Response::from_string(view_page(view_name))
                .with_optional_header(header("Content-Type", "text/html; charset=utf-8"))
                .with_optional_header(no_store)
        }
        Route::Frame {
            view_name,
            after,
            uniquely_named,
        } => {
            frames.note_page_request(view_name);
            frame_response(frames, view_name, after, uniquely_named)
        }
        Route::NotFound => Response::from_string("not found").with_status_code(404),
    }
}

fn frame_response(
    frames: &ViewFrames,
    view_name: &str,
    after: Option<u64>,
    uniquely_named: bool,
) -> Response<Cursor<Vec<u8>>> {
    let no_store = header("Cache-Control", "no-store");
    match frames.latest(view_name) {
        Some(frame) if after.is_none_or(|seen| frame.sequence > seen) => {
            let caching = if uniquely_named {
                header("Cache-Control", "private, max-age=60, immutable")
            } else {
                no_store
            };
            Response::from_data(frame.png.as_ref().clone())
                .with_optional_header(header("Content-Type", "image/png"))
                .with_optional_header(header("X-Frame-Sequence", &frame.sequence.to_string()))
                .with_optional_header(header("Access-Control-Expose-Headers", "X-Frame-Sequence"))
                .with_optional_header(caching)
        }
        _ => Response::from_data(Vec::new())
            .with_status_code(204)
            .with_optional_header(no_store),
    }
}

trait OptionalHeader {
    fn with_optional_header(self, header: Option<Header>) -> Self;
}

impl OptionalHeader for Response<Cursor<Vec<u8>>> {
    fn with_optional_header(mut self, header: Option<Header>) -> Self {
        if let Some(header) = header {
            self.add_header(header);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::*;

    struct HttpReply {
        status: u16,
        headers: String,
        body: Vec<u8>,
    }

    fn http_get(address: SocketAddr, path: &str) -> HttpReply {
        let mut stream = TcpStream::connect(address).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status = headers.split(' ').nth(1).unwrap().parse().unwrap();
        HttpReply {
            status,
            headers,
            body: raw[split + 4..].to_vec(),
        }
    }

    #[test]
    fn a_taken_port_falls_back_to_a_free_one_on_the_same_host() {
        let first = PanelServer::start("127.0.0.1:0").unwrap();
        let taken = first.local_address().unwrap();

        let (second, refusal) = PanelServer::start_or_any_free_port(&taken.to_string()).unwrap();
        let fallback = second.local_address().unwrap();

        assert!(matches!(refusal, Some(PanelServerError::Bind(_))));
        assert_eq!(fallback.ip(), taken.ip());
        assert_ne!(fallback.port(), taken.port());
        assert_eq!(http_get(taken, "/views").status, 200);
        assert_eq!(http_get(fallback, "/views").status, 200);
    }

    /// Another test may bind the probed port between the probe and the start, so a refusal
    /// is retried with a fresh port; only a free port used elsewhere than asked fails.
    #[test]
    fn a_free_port_is_used_as_asked() {
        for _ in 0..20 {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let free = probe.local_addr().unwrap();
            drop(probe);

            let (server, refusal) = PanelServer::start_or_any_free_port(&free.to_string()).unwrap();

            if refusal.is_none() {
                assert_eq!(server.local_address().unwrap(), free);
                return;
            }
        }
        panic!("every probed port was taken before the server started");
    }

    #[test]
    fn serves_page_and_latest_frame() {
        let server = PanelServer::start("127.0.0.1:0").unwrap();
        let address = server.local_address().unwrap();

        let empty = http_get(address, "/views/plot/frame.png");
        assert_eq!(empty.status, 204);

        let red_pixels = [255, 0, 0, 255].repeat(6);
        let sequence = server
            .frames()
            .publish_rgba8("plot", 3, 2, &red_pixels)
            .unwrap();
        assert_eq!(sequence, 0);

        let page = http_get(address, "/views/plot");
        assert_eq!(page.status, 200);
        assert!(String::from_utf8_lossy(&page.body).contains("/views/plot/frame.png"));

        let frame = http_get(address, "/views/plot/frame.png");
        assert_eq!(frame.status, 200);
        assert!(frame.headers.contains("X-Frame-Sequence: 0"));
        assert!(frame.headers.contains("Cache-Control: no-store"));
        let named_frame = http_get(address, "/views/plot/frame.png?n=1760000000000-7");
        assert_eq!(named_frame.status, 200);
        assert!(
            named_frame
                .headers
                .contains("Cache-Control: private, max-age=60, immutable"),
            "the page's uniquely named requests can be served from the cache: {}",
            named_frame.headers
        );
        let decoded = image::load_from_memory(&frame.body).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(2, 1).0, [255, 0, 0, 255]);

        let list = http_get(address, "/views");
        assert_eq!(String::from_utf8_lossy(&list.body), "[\"plot\"]");
    }

    #[test]
    fn frame_requests_after_the_latest_sequence_get_no_content() {
        let server = PanelServer::start("127.0.0.1:0").unwrap();
        let address = server.local_address().unwrap();
        let pixels = [0, 0, 0, 255].repeat(4);
        server
            .frames()
            .publish_rgba8("image", 2, 2, &pixels)
            .unwrap();
        let second = server
            .frames()
            .publish_rgba8("image", 2, 2, &pixels)
            .unwrap();
        assert_eq!(second, 1);

        assert_eq!(
            http_get(address, "/views/image/frame.png?after=1").status,
            204
        );
        let fresh = http_get(address, "/views/image/frame.png?after=0");
        assert_eq!(fresh.status, 200);
        assert!(fresh.headers.contains("X-Frame-Sequence: 1"));
    }

    #[test]
    fn rejects_bad_names_frames_and_paths() {
        let server = PanelServer::start("127.0.0.1:0").unwrap();
        let address = server.local_address().unwrap();
        assert!(matches!(
            server.frames().publish_rgba8("../etc", 1, 1, &[0; 4]),
            Err(PanelServerError::InvalidViewName(_))
        ));
        assert!(matches!(
            server.frames().publish_rgba8("ok", 2, 2, &[0; 4]),
            Err(PanelServerError::InvalidFrame(_))
        ));
        assert_eq!(http_get(address, "/views/a%2Fb").status, 404);
        assert_eq!(http_get(address, "/elsewhere").status, 404);
    }

    #[test]
    fn page_and_frame_requests_are_noted_per_published_view() {
        let server = PanelServer::start("127.0.0.1:0").unwrap();
        let address = server.local_address().unwrap();
        let pixels = [0, 0, 0, 255].repeat(4);
        http_get(address, "/views/camera");
        http_get(address, "/views/camera/frame.png?n=1-1");
        assert_eq!(
            server.frames().last_page_request("camera"),
            None,
            "not published yet"
        );
        http_get(address, "/views");
        assert!(
            !server.frames().any_page_request(),
            "neither an unpublished view nor the view list counts"
        );

        server
            .frames()
            .publish_rgba8("camera", 2, 2, &pixels)
            .unwrap();
        let before_page = Instant::now();
        http_get(address, "/views/camera");
        let after_page = server.frames().last_page_request("camera").unwrap();
        assert!(after_page >= before_page);
        assert!(server.frames().any_page_request());

        http_get(address, "/views/camera/frame.png?n=1-2");
        let after_frame = server.frames().last_page_request("camera").unwrap();
        assert!(after_frame >= after_page, "a frame request counts too");

        for invented in ["map", "a1", "b2", "c3"] {
            http_get(address, &format!("/views/{invented}"));
            http_get(address, &format!("/views/{invented}/frame.png"));
        }
        http_get(address, "/elsewhere");
        assert_eq!(server.frames().last_page_request("map"), None);
        assert_eq!(
            server
                .frames()
                .page_requests
                .lock()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["camera"],
            "names no view publishes are not recorded"
        );
        assert_eq!(server.frames().view_names(), ["camera"]);
    }

    #[test]
    fn view_url_uses_the_bound_port() {
        let server = PanelServer::start("127.0.0.1:0").unwrap();
        let port = server.local_address().unwrap().port();
        assert_eq!(
            server.view_url("plot").unwrap(),
            format!("http://127.0.0.1:{port}/views/plot")
        );
    }
}
