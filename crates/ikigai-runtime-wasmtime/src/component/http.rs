//! `wasi:http` routed to the kernel's `urn:http:*` (R-D3's host-rule row, R-D5): a component's
//! outgoing request is issued as ikigai-http's endpoint for its method, under the session's
//! capability, so ikigai-http's own host rules, globs, redirect re-checks and the caller's grant
//! apply. A socket check sees an IP, never a host name; this sees the URL. wasmtime's own HTTP
//! client (`default-send-request`) is not compiled in at all.
//!
//! The mapping, request side: the method picks the endpoint and verb through ikigai-http's
//! `Method` table; the URL is `url=`; `Accept`, `Authorization`, `Range` and `If-Range` are their
//! named arguments, `Content-Type` is `content_type=`, every other header goes to `headers=` as
//! `Name: Value` lines, and a body is `content=`. Response side: a representation is a 200 with
//! its media type as `Content-Type`; a `NotFound` is a 404 and a `Conflict` a 409 (answers, not
//! failures, to an HTTP client); a `Denied` is `HTTP-request-denied`; `Timeout` and `Unavailable`
//! are the connection codes; anything else is an internal error carrying the message.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use ikigai_core::{ArgRef, Capability, Error as CoreError, Iri, Request};
use ikigai_http::Method;
use ikigai_runtime::HostResolver;
use wasmtime_wasi_http::{Error as HttpError, RequestOptions, WasiBody, WasiHttpHooks};

/// The `wasi:http` hooks of one call: the kernel seam and the capability the call runs under.
pub(crate) struct KernelHttp {
    pub(crate) host: Arc<dyn HostResolver>,
    pub(crate) capability: Capability,
}

type SendResult = Result<
    (
        http::Response<WasiBody>,
        Box<dyn std::future::Future<Output = Result<(), HttpError>> + Send>,
    ),
    HttpError,
>;

impl WasiHttpHooks for KernelHttp {
    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        _options: Option<RequestOptions>,
        _fut: Box<dyn std::future::Future<Output = Result<(), HttpError>> + Send>,
    ) -> Box<dyn std::future::Future<Output = SendResult> + Send> {
        let host = Arc::clone(&self.host);
        let capability = self.capability.clone();
        Box::new(async move {
            let (parts, body) = request.into_parts();
            let method = Method::ALL
                .into_iter()
                .find(|m| m.as_str() == parts.method.as_str())
                .ok_or(HttpError::HttpRequestMethodInvalid)?;
            let body = body.collect().await?.to_bytes();
            let target =
                Iri::parse(method.iri()).map_err(|_| HttpError::HttpRequestMethodInvalid)?;
            let mut kernel_request = Request::new(method.verb(), target)
                .with_arg("url", inline(parts.uri.to_string().into_bytes()));
            let mut extra = String::new();
            for (name, value) in &parts.headers {
                let Ok(value) = value.to_str() else {
                    return Err(HttpError::HttpRequestHeaderSize {
                        field_name: Some(name.as_str().to_string()),
                        field_size: None,
                    });
                };
                match name.as_str() {
                    "accept" | "authorization" | "range" => {
                        kernel_request = kernel_request
                            .with_arg(name.as_str(), inline(value.as_bytes().to_vec()));
                    }
                    "if-range" => {
                        kernel_request =
                            kernel_request.with_arg("if_range", inline(value.as_bytes().to_vec()));
                    }
                    "content-type" => {
                        kernel_request = kernel_request
                            .with_arg("content_type", inline(value.as_bytes().to_vec()));
                    }
                    // The URL carries the authority; the length is the body's.
                    "host" | "content-length" => {}
                    other => {
                        extra.push_str(other);
                        extra.push_str(": ");
                        extra.push_str(value);
                        extra.push('\n');
                    }
                }
            }
            if !extra.is_empty() {
                kernel_request = kernel_request.with_arg("headers", inline(extra.into_bytes()));
            }
            if !body.is_empty() {
                kernel_request = kernel_request.with_arg("content", inline(body.to_vec()));
            }
            let response = match host.resolve(kernel_request, &capability).await {
                Ok(repr) if method == Method::Head => {
                    status(if repr.bytes == b"true" { 200 } else { 404 }, None)
                }
                Ok(repr) => {
                    let content_type = repr.repr_type.canonical();
                    status(200, Some((content_type, Bytes::from(repr.bytes))))
                }
                Err(CoreError::NotFound(_)) => status(404, None),
                Err(CoreError::Conflict(_)) => status(409, None),
                Err(CoreError::Denied(_)) => return Err(HttpError::HttpRequestDenied),
                Err(CoreError::Timeout(_)) => return Err(HttpError::ConnectionTimeout),
                Err(CoreError::Unavailable(_)) => return Err(HttpError::DestinationUnavailable),
                Err(other) => return Err(HttpError::InternalError(Some(other.to_string()))),
            };
            let done: Box<dyn std::future::Future<Output = Result<(), HttpError>> + Send> =
                Box::new(async { Ok(()) });
            Ok((response, done))
        })
    }
}

fn inline(bytes: Vec<u8>) -> ArgRef {
    ArgRef::Inline(bytes)
}

fn status(code: u16, body: Option<(String, Bytes)>) -> http::Response<WasiBody> {
    let mut builder = http::Response::builder().status(code);
    let bytes = match body {
        Some((content_type, bytes)) => {
            builder = builder.header("content-type", content_type);
            bytes
        }
        None => Bytes::new(),
    };
    builder
        .body(
            Full::new(bytes)
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .expect("a status and an optional content type always build")
}
