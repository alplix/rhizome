# Test certificates

Used only by `#[cfg(test)]` code in `src/identity.rs` and `src/connection.rs`,
to test SASL `EXTERNAL` against a real mutual-TLS handshake. Not secrets —
regenerate with:

```bash
openssl ecparam -genkey -name prime256v1 -noout -out ca.key
openssl req -x509 -new -key ca.key -sha256 -days 3650 -out ca.crt -subj "/CN=Rhizome Test CA"

openssl ecparam -genkey -name prime256v1 -noout -out server.key
openssl req -new -key server.key -out server.csr -subj "/CN=localhost"
printf "subjectAltName=DNS:localhost,IP:127.0.0.1\n" > server.ext
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 3650 -sha256 -extfile server.ext

openssl ecparam -genkey -name prime256v1 -noout -out client.key
openssl req -new -key client.key -out client.csr -subj "/CN=alp-test-client"
openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 3650 -sha256

# A client certificate from an entirely different, untrusted CA.
openssl ecparam -genkey -name prime256v1 -noout -out other.key
openssl req -x509 -new -key other.key -sha256 -days 3650 -out other.crt -subj "/CN=untrusted-other-client"
```
