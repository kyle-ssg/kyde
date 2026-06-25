//! Minimal HTTP client wired into gpui so the Markdown preview can load remote
//! (http/https) images. Compiled only under the `remote-images` Cargo feature —
//! it pulls in ureq + rustls, which we keep out of the default build to protect
//! the binary/RAM footprint (see CONTRIBUTING.md: heavy, optional things go behind
//! a Cargo feature). Local-file images need none of this and always work.
//!
//! gpui's image asset loader calls `HttpClient::get` for `Resource::Uri` sources;
//! the default `NullHttpClient` bails with "No HttpClient available", so without a
//! real client a remote `<img>` simply renders nothing (no panic). This supplies
//! one: a thin async wrapper over blocking `ureq`, run on a scratch thread so it
//! never blocks gpui's executor.

use std::io::Read;

use futures::channel::oneshot;
use futures::future::BoxFuture;
use gpui::http_client::{http::HeaderValue, AsyncBody, HttpClient, Request, Response, Url};

pub struct UreqClient {
    agent: ureq::Agent,
    user_agent: HeaderValue,
}

impl UreqClient {
    pub fn new() -> Self {
        Self {
            agent: ureq::Agent::new_with_defaults(),
            user_agent: HeaderValue::from_static(concat!("Kyde/", env!("CARGO_PKG_VERSION"))),
        }
    }
}

impl HttpClient for UreqClient {
    fn type_name(&self) -> &'static str {
        "UreqClient"
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let agent = self.agent.clone();
        let method = req.method().as_str().to_string();
        let uri = req.uri().to_string();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            // ureq is blocking — run it off the async executor on a scratch thread.
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<(u16, Vec<u8>)> {
                    // ureq 3 uses method-specific builders; this client only ever serves
                    // gpui's image GETs (HEAD kept for completeness).
                    let resp = match method.as_str() {
                        "GET" => agent.get(uri.as_str()).call()?,
                        "HEAD" => agent.head(uri.as_str()).call()?,
                        other => anyhow::bail!("UreqClient supports only GET/HEAD (got {other})"),
                    };
                    let status = resp.status().as_u16();
                    let mut body = Vec::new();
                    // Cap the read so a hostile/huge URL can't exhaust memory.
                    resp.into_body()
                        .into_reader()
                        .take(64 * 1024 * 1024)
                        .read_to_end(&mut body)?;
                    Ok((status, body))
                })();
                let _ = tx.send(result);
            });
            let (status, body) = rx
                .await
                .map_err(|_| anyhow::anyhow!("image request thread dropped"))??;
            Ok(Response::builder()
                .status(status)
                .body(AsyncBody::from(body))?)
        })
    }
}
