use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use anyhow::Result;
use mime_guess::MimeGuess;
use once_cell::sync::OnceCell;
use tiny_http::{Header, Response, Server};
use tracing::warn;

#[derive(Debug)]
pub struct PreviewServer {
    pub port: u16,
}

static SERVER: OnceCell<Arc<PreviewServer>> = OnceCell::new();

pub fn ensure_server(viewer_root: PathBuf, beatmaps_root: PathBuf) -> Result<Arc<PreviewServer>> {
    SERVER
        .get_or_try_init(|| start_server(viewer_root, beatmaps_root))
        .cloned()
}

fn start_server(viewer_root: PathBuf, beatmaps_root: PathBuf) -> Result<Arc<PreviewServer>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let viewer_root = Arc::new(viewer_root);
    let beatmaps_root = Arc::new(beatmaps_root);
    let server = Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let path = request
                .url()
                .split('?')
                .next()
                .unwrap_or("/")
                .to_string();
            if path == "/" {
                let index = viewer_root.join("index.html");
                let _ = serve_path(request, &index, MimeGuess::from_ext("html"));
                continue;
            }
            if let Some(rest) = path.strip_prefix("/viewer/") {
                let _ = serve_from_root(request, &viewer_root, rest);
                continue;
            }
            if let Some(rest) = path.strip_prefix("/beatmaps/") {
                let _ = serve_from_root(request, &beatmaps_root, rest);
                continue;
            }
            let _ = request.respond(Response::empty(404));
        }
    });
    Ok(Arc::new(PreviewServer { port }))
}

fn serve_from_root(
    request: tiny_http::Request,
    root: &Path,
    rel: &str,
) -> std::io::Result<()> {
    if rel.contains("..") {
        return request.respond(Response::empty(403));
    }
    let target = root.join(rel);
    let mime = mime_guess::from_path(&target);
    serve_path(request, &target, mime)
}

fn serve_path(
    request: tiny_http::Request,
    path: &Path,
    mime: MimeGuess,
) -> std::io::Result<()> {
    if !path.exists() || !path.is_file() {
        return request.respond(Response::empty(404));
    }
    let file = std::fs::File::open(path)?;
    let mut response = Response::from_file(file);
    if let Some(mt) = mime.first_raw() {
        if let Ok(header) = Header::from_bytes(&b"Content-Type"[..], mt.as_bytes()) {
            response = response.with_header(header);
        }
    }
    if let Err(err) = request.respond(response) {
        warn!("Falha ao responder preview: {err}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use tempfile::tempdir;

    #[test]
    fn serves_beatmap_files() {
        let viewer = tempdir().unwrap();
        let beatmaps = tempdir().unwrap();
        std::fs::write(viewer.path().join("index.html"), "viewer").unwrap();
        std::fs::write(beatmaps.path().join("sample.txt"), "beatmap").unwrap();

        let server =
            ensure_server(viewer.path().to_path_buf(), beatmaps.path().to_path_buf()).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        write!(
            stream,
            "GET /beatmaps/sample.txt HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();
        let mut buf = [0u8; 256];
        let len = stream.read(&mut buf).unwrap();
        let body = String::from_utf8_lossy(&buf[..len]);
        assert!(body.contains("beatmap"));
    }
}
