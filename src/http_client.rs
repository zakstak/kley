//! HTTPS clients with bundled Mozilla roots for container-safe trust.

/// Build an asynchronous HTTPS client that augments platform roots with the
/// bundled Mozilla WebPKI root store. This keeps TLS verification enabled when
/// a minimal Linux or container image has no system CA bundle.
pub fn client() -> reqwest::Client {
    add_bundled_roots(reqwest::Client::builder())
        .build()
        .expect("bundled WebPKI root certificates must build a valid client")
}

/// Build a blocking HTTPS client with the same bundled root store.
pub fn blocking_client() -> reqwest::blocking::Client {
    add_bundled_blocking_roots(reqwest::blocking::Client::builder())
        .build()
        .expect("bundled WebPKI root certificates must build a valid client")
}

/// Start an asynchronous client builder with bundled Mozilla roots.
pub fn client_builder() -> reqwest::ClientBuilder {
    add_bundled_roots(reqwest::Client::builder())
}

/// Start a blocking client builder with bundled Mozilla roots.
pub fn blocking_client_builder() -> reqwest::blocking::ClientBuilder {
    add_bundled_blocking_roots(reqwest::blocking::Client::builder())
}

fn add_bundled_roots(mut builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    for certificate in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_der(certificate.as_ref())
                .expect("bundled WebPKI root certificate must be valid DER"),
        );
    }
    builder
}

fn add_bundled_blocking_roots(
    mut builder: reqwest::blocking::ClientBuilder,
) -> reqwest::blocking::ClientBuilder {
    for certificate in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_der(certificate.as_ref())
                .expect("bundled WebPKI root certificate must be valid DER"),
        );
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::{blocking_client, client};

    #[test]
    fn clients_build_with_bundled_webpki_roots() {
        let _ = client();
        let _ = blocking_client();
    }
}
