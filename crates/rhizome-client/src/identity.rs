//! A TLS client certificate, for SASL `EXTERNAL`.
//!
//! `EXTERNAL` authenticates by the certificate presented during the TLS
//! handshake itself, so nothing secret ever crosses the wire in the SASL
//! exchange — see [`rhizome_proto::sasl`]. This module only reads the
//! certificate and its key from PEM text; presenting it is the connector's
//! job, in `connection.rs`.

use std::io;

use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// A certificate chain and the private key that matches it, ready to present
/// during a TLS handshake.
pub struct ClientCert {
    pub(crate) chain: Vec<CertificateDer<'static>>,
    pub(crate) key: PrivateKeyDer<'static>,
}

impl Clone for ClientCert {
    fn clone(&self) -> ClientCert {
        ClientCert {
            chain: self.chain.clone(),
            key: self.key.clone_key(),
        }
    }
}

impl std::fmt::Debug for ClientCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `PrivateKeyDer`'s own `Debug` already elides the key material; the
        // certificates are not secret, but printing their raw DER bytes has
        // never been useful in a log, so only the count is shown.
        f.debug_struct("ClientCert")
            .field("certificates", &self.chain.len())
            .field("key", &self.key)
            .finish()
    }
}

impl ClientCert {
    /// Reads a certificate chain and its private key from PEM text: however
    /// many `-----BEGIN...-----` blocks it holds, in whatever order, cert and
    /// key together in one file (as a network's SASL `CertFP` instructions
    /// usually hand out) or concatenated from two. Every certificate is kept,
    /// in the order it appeared — some networks expect an intermediate
    /// alongside the leaf — and the first private key found is the one used.
    pub fn from_pem(pem: &[u8]) -> io::Result<ClientCert> {
        let chain: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut io::Cursor::new(pem)).collect::<Result<_, _>>()?;
        if chain.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no certificate found in the file",
            ));
        }
        let key = rustls_pemfile::private_key(&mut io::Cursor::new(pem))?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "no private key found in the file",
            )
        })?;
        Ok(ClientCert { chain, key })
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    //! A self-signed test CA and a certificate it issued to a client, for
    //! this module's own parsing tests. `tests/sasl_external.rs` needs a
    //! server certificate and an untrusted client certificate too, from the
    //! same generated set, but keeps its own copies: a `#[cfg(test)]` item
    //! here does not exist for that separately compiled integration test
    //! crate to import.

    pub(crate) const CA_CERT: &str = include_str!("../testdata/ca.crt");
    pub(crate) const CLIENT_CERT: &str = include_str!("../testdata/client.crt");
    pub(crate) const CLIENT_KEY: &str = include_str!("../testdata/client.key");
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::*;
    use super::*;

    #[test]
    fn reads_a_combined_pem_of_cert_and_key_in_either_order() {
        let combined = format!("{CLIENT_CERT}\n{CLIENT_KEY}");
        let cert = ClientCert::from_pem(combined.as_bytes()).unwrap();
        assert_eq!(cert.chain.len(), 1);

        // Key before cert is just as valid: nothing here assumes an order.
        let reversed = format!("{CLIENT_KEY}\n{CLIENT_CERT}");
        let cert = ClientCert::from_pem(reversed.as_bytes()).unwrap();
        assert_eq!(cert.chain.len(), 1);
    }

    #[test]
    fn keeps_every_certificate_for_an_intermediate_chain() {
        let combined = format!("{CLIENT_CERT}\n{CA_CERT}\n{CLIENT_KEY}");
        let cert = ClientCert::from_pem(combined.as_bytes()).unwrap();
        assert_eq!(cert.chain.len(), 2, "the leaf and the intermediate/CA");
    }

    #[test]
    fn a_file_with_no_certificate_is_an_error() {
        let err = ClientCert::from_pem(CLIENT_KEY.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("certificate"), "{err}");
    }

    #[test]
    fn a_file_with_no_private_key_is_an_error() {
        let err = ClientCert::from_pem(CLIENT_CERT.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("private key"), "{err}");
    }

    #[test]
    fn nonsense_input_is_an_error_not_a_panic() {
        assert!(ClientCert::from_pem(b"this is not a PEM file at all").is_err());
        assert!(ClientCert::from_pem(b"").is_err());
    }

    #[test]
    fn debug_output_never_contains_the_key_material() {
        let combined = format!("{CLIENT_CERT}\n{CLIENT_KEY}");
        let cert = ClientCert::from_pem(combined.as_bytes()).unwrap();
        let shown = format!("{cert:?}");
        // The key, base64-encoded, would be a long run of characters from
        // its PEM block; none of it should turn up in Debug output.
        for line in CLIENT_KEY.lines().filter(|l| !l.starts_with("-----")) {
            assert!(!shown.contains(line), "key material leaked: {shown}");
        }
    }

    #[test]
    fn cloning_does_not_require_the_key_type_to_implement_clone_itself() {
        let combined = format!("{CLIENT_CERT}\n{CLIENT_KEY}");
        let cert = ClientCert::from_pem(combined.as_bytes()).unwrap();
        let cloned = cert.clone();
        assert_eq!(cloned.chain, cert.chain);
    }
}
