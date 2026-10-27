//! The probe: each export tries one authority and answers `ok: …` or `err: …`.

use std::io::{Read, Write};

wit_bindgen::generate!({
    path: "wit",
    world: "ikigai:probe/probe-app",
});

use exports::ikigai::probe::probe::Guest;

struct Probe;

fn io<T: std::fmt::Debug>(r: std::io::Result<T>) -> String {
    match r {
        Ok(v) => format!("ok: {v:?}"),
        Err(e) => format!("err: {:?}", e.kind()),
    }
}

impl Guest for Probe {
    fn args() -> String {
        format!("ok: {:?}", std::env::args().collect::<Vec<_>>())
    }

    fn env(name: String) -> String {
        match std::env::var(&name) {
            Ok(v) => format!("ok: {v}"),
            Err(e) => format!("err: {e}"),
        }
    }

    fn read_root() -> String {
        io(std::fs::read_dir("/").map(|d| d.count()))
    }

    fn read_data() -> String {
        io(std::fs::read_to_string("/data/x.txt"))
    }

    fn read_path(path: String) -> String {
        io(std::fs::read_to_string(path))
    }

    fn write_data(text: String) -> String {
        io(std::fs::File::create("/data/out.txt").and_then(|mut f| f.write_all(text.as_bytes())))
    }

    fn tcp(addr: String) -> String {
        io(std::net::TcpStream::connect(addr.as_str()).map(|_| ()))
    }

    fn dns(host: String) -> String {
        io(std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), 80)).map(|a| a.count()))
    }

    fn spawn() -> String {
        io(std::process::Command::new("ls").status().map(|_| ()))
    }

    fn clock() -> String {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => format!("ok: {}", d.as_secs()),
            Err(_) => "err: before the epoch".to_string(),
        }
    }

    fn random() -> String {
        let bytes = wasip2::random::random::get_random_bytes(8);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        format!("ok: {hex}")
    }

    fn print(text: String) -> String {
        println!("{text}");
        "ok: printed".to_string()
    }

    fn http_get(url: String) -> String {
        use wasip2::http::outgoing_handler::handle;
        use wasip2::http::types::{Fields, Method, OutgoingRequest, Scheme};

        let Some((scheme, rest)) = url.split_once("://") else {
            return "err: not a URL".to_string();
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let request = OutgoingRequest::new(Fields::new());
        let _ = request.set_method(&Method::Get);
        let _ = request.set_scheme(Some(&if scheme == "http" {
            Scheme::Http
        } else {
            Scheme::Https
        }));
        let _ = request.set_authority(Some(authority));
        let _ = request.set_path_with_query(Some(path));
        let future = match handle(request, None) {
            Ok(f) => f,
            Err(e) => return format!("err: {e:?}"),
        };
        future.subscribe().block();
        let response = match future.get() {
            Some(Ok(Ok(r))) => r,
            Some(Ok(Err(e))) => return format!("err: {e:?}"),
            other => return format!("err: {other:?}"),
        };
        let status = response.status();
        let mut body = Vec::new();
        if let Ok(incoming) = response.consume() {
            if let Ok(stream) = incoming.stream() {
                loop {
                    match stream.blocking_read(4096) {
                        Ok(chunk) if !chunk.is_empty() => body.extend_from_slice(&chunk),
                        _ => break,
                    }
                }
            }
        }
        let mut text = String::new();
        let _ = (&body[..]).read_to_string(&mut text);
        format!("ok: {status} {text}")
    }
}

export!(Probe);
