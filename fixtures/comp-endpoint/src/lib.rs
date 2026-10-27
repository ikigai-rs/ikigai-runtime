//! An `endpoint`-world component (R-D2): it serves its own endpoints through `endpoint-api` and
//! reaches the kernel only through the `kernel` import.

use ikigai_runtime_guest::component::{kernel, Arg, Error, Guest, Representation, Request};

struct Fixture;

/// Core `Description` JSON for the endpoints this guest serves (`describe()`).
const DESCRIBE: &str = r#"[
  {"id":"hello","title":"hello","summary":"Greet the host's name, read through the kernel.","verbs":["Source"],"inputs":[],"outputs":["text/plain"]},
  {"id":"echo","title":"echo","summary":"Answer the content argument.","verbs":["Source"],"inputs":[{"name":"content","summary":"the bytes","required":true}],"outputs":["application/octet-stream"]},
  {"id":"note","title":"note","summary":"Write a note through the kernel, then read it back.","verbs":["Source"],"inputs":[{"name":"content","summary":"the bytes","required":true}],"outputs":["text/plain"]},
  {"id":"spin","title":"spin","summary":"Loop forever.","verbs":["Source"],"inputs":[],"outputs":["text/plain"]},
  {"id":"balloon","title":"balloon","summary":"Allocate without bound.","verbs":["Source"],"inputs":[],"outputs":["text/plain"]},
  {"id":"boom","title":"boom","summary":"Panic.","verbs":["Source"],"inputs":[],"outputs":["text/plain"]},
  {"id":"clock","title":"clock","summary":"Read the wall clock.","verbs":["Source"],"inputs":[],"outputs":["text/plain"]}
]"#;

fn arg<'a>(req: &'a Request, name: &str) -> Option<&'a [u8]> {
    req.args
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.value.as_slice())
}

fn text(body: impl Into<Vec<u8>>) -> Representation {
    Representation {
        media_type: "text/plain; charset=utf-8".into(),
        bytes: body.into(),
    }
}

impl Guest for Fixture {
    fn describe() -> String {
        DESCRIBE.to_string()
    }

    fn invoke(req: Request) -> Result<Representation, Error> {
        match req.endpoint.as_str() {
            "hello" => {
                let name = kernel::source("urn:host:name", &[])?;
                let mut greeting = b"hello, ".to_vec();
                greeting.extend_from_slice(&name.bytes);
                Ok(text(greeting))
            }
            "echo" => match arg(&req, "content") {
                Some(bytes) => Ok(Representation {
                    media_type: "application/octet-stream".into(),
                    bytes: bytes.to_vec(),
                }),
                None => Err(Error::InvalidArgument("content: missing".into())),
            },
            "note" => {
                let content = arg(&req, "content").unwrap_or_default().to_vec();
                kernel::sink(
                    "urn:host:note",
                    &[Arg {
                        name: "content".into(),
                        value: content,
                    }],
                )?;
                let back = kernel::source("urn:host:note", &[])?;
                Ok(text(back.bytes))
            }
            "spin" => {
                let mut i: u64 = 0;
                loop {
                    i = i.wrapping_add(1);
                    core::hint::black_box(i);
                }
            }
            "balloon" => {
                let mut held: Vec<Vec<u8>> = Vec::new();
                loop {
                    held.push(vec![0u8; 1 << 16]);
                    core::hint::black_box(held.len());
                }
            }
            "boom" => panic!("boom"),
            "clock" => {
                // The endpoint world's clock is fixed: the same call answers the same bytes on
                // every machine.
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(u64::MAX);
                Ok(text(format!("{secs}")))
            }
            other => Err(Error::NotFound(format!("no endpoint `{other}`"))),
        }
    }
}

ikigai_runtime_guest::component::export_endpoint!(Fixture);
