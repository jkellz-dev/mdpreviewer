//! The HTTPS client Typst uses to download `@preview` packages.
//!
//! typst-kit's own downloader links OpenSSL through `native-tls`. This one is
//! ureq on rustls, so no C TLS library is linked on any platform.

use std::any::Any;
use std::io::{self, Read};

use typst_kit::downloader::Downloader;
use ureq::tls::{RootCerts, TlsConfig};

/// Fetches package archives and the package index. Certificates are checked
/// against the operating system's trust store, as the typst CLI does, so a
/// CA installed for a corporate proxy works. ureq reads the proxy from the
/// usual environment variables.
pub struct HttpsDownloader(ureq::Agent);

impl Default for HttpsDownloader {
    fn default() -> Self {
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .user_agent(concat!("mdpreviewer/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        HttpsDownloader(agent)
    }
}

impl Downloader for HttpsDownloader {
    fn stream(&self, _key: &dyn Any, url: &str) -> io::Result<(Option<usize>, Box<dyn Read>)> {
        let response = self.0.get(url).call().map_err(|err| match err {
            // The trait asks for `NotFound` here: typst-kit reports it as
            // "package not found" rather than as a network failure.
            ureq::Error::StatusCode(404) => io::Error::new(io::ErrorKind::NotFound, err),
            err => io::Error::other(err),
        })?;
        let body = response.into_body();
        let size = body
            .content_length()
            .and_then(|len| usize::try_from(len).ok());
        Ok((size, Box::new(body.into_reader())))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read};
    use std::thread;

    use tiny_http::{Response, Server};
    use typst_kit::downloader::Downloader;

    use super::HttpsDownloader;

    /// A plain-HTTP server answering `/ok` with a body and anything else
    /// with a 404. The status mapping does not depend on TLS.
    fn serve() -> String {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        thread::spawn(move || {
            for request in server.incoming_requests() {
                let response = if request.url() == "/ok" {
                    Response::from_string("package bytes")
                } else {
                    Response::from_string("missing").with_status_code(404)
                };
                let _ = request.respond(response);
            }
        });
        url
    }

    #[test]
    fn a_download_streams_the_body_with_its_size() {
        let url = serve();
        let (size, mut reader) = HttpsDownloader::default()
            .stream(&(), &format!("{url}/ok"))
            .unwrap();
        let mut body = String::new();
        reader.read_to_string(&mut body).unwrap();
        assert_eq!(body, "package bytes");
        assert_eq!(size, Some(body.len()));
    }

    #[test]
    fn a_404_is_not_found() {
        let url = serve();
        let err = HttpsDownloader::default()
            .stream(&(), &format!("{url}/gone"))
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn other_failures_are_not_not_found() {
        // Nothing listens on port 1.
        let err = HttpsDownloader::default()
            .stream(&(), "http://127.0.0.1:1/x")
            .err()
            .unwrap();
        assert_ne!(err.kind(), ErrorKind::NotFound);
    }
}
