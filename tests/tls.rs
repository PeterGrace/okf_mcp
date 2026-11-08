//! TLS to Postgres (M7): with `sslmode=verify-full` the server connects
//! encrypted to a database whose certificate its CA signed, and refuses one
//! another CA signed. Needs Docker, like every database test. Certificates
//! are generated here, never checked in.

use std::io::Write;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::{ContainerAsync, ContainerRequest, ImageExt};

// Only the Postgres helper: `#[path]` compiles that one file as a module,
// without the HTTP server helpers in `support/mod.rs`.
#[path = "support/pg.rs"]
mod pg;

/// Installs the copied key and certificate as the `postgres` user's
/// (Postgres refuses a key it does not own, and the copy is root's), then
/// hands over to the image's entrypoint with TLS on. `exec` keeps Postgres
/// as the container's main process.
///
/// The image's `docker-entrypoint.sh` runs any command that is not
/// `postgres` (or a `-flag`) as given, as root, so this script gets root for
/// `install -o postgres`; the nested `docker-entrypoint.sh postgres` then
/// initializes the database and drops to the `postgres` user itself.
const START_WITH_TLS: &str = "install -o postgres -g postgres -m 600 /tmp/server.key \
     /var/lib/postgresql/server.key \
     && install -o postgres -g postgres -m 644 /tmp/server.crt \
     /var/lib/postgresql/server.crt \
     && exec docker-entrypoint.sh postgres -c ssl=on \
     -c ssl_cert_file=/var/lib/postgresql/server.crt \
     -c ssl_key_file=/var/lib/postgresql/server.key";

/// A certificate authority: its certificate (PEM) and what signs with it.
struct Ca {
    /// The CA certificate, PEM-encoded: what a client trusts.
    pem: String,
    /// The CA's parameters and private key, which sign server certificates.
    /// `'static` is a Rust lifetime: the issuer owns its parameters rather
    /// than borrowing them, so it can live as long as the `Ca` does.
    issuer: Issuer<'static, KeyPair>,
}

/// Generates a self-signed CA whose subject's common name is `name`.
///
/// # Arguments
/// * `name` - the CA's common name; distinct per CA, so a failure names
///   which one it saw.
///
/// # Returns
/// The CA's certificate and its issuer.
fn new_ca(name: &str) -> Ca {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("no names is valid");
    params.distinguished_name.push(DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate().expect("a CA key");
    let pem = params.self_signed(&key).expect("a CA certificate").pem();
    Ca {
        pem,
        issuer: Issuer::new(params, key),
    }
}

/// A certificate for `127.0.0.1` signed by `ca`, and its key.
///
/// # Arguments
/// * `ca` - the CA that signs the certificate.
///
/// # Returns
/// `(certificate, key)`, both PEM. rcgen turns the IP-literal name into an
/// IP subject alternative name, which `verify-full` checks against the host.
fn server_certificate(ca: &Ca) -> (String, String) {
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_string()]).expect("an IP name");
    params
        .distinguished_name
        .push(DnType::CommonName, "okf-mcp test server");
    let key = KeyPair::generate().expect("a server key");
    let certificate = params
        .signed_by(&key, &ca.issuer)
        .expect("a signed certificate");
    (certificate.pem(), key.serialize_pem())
}

/// A request for `postgres:17-alpine` with TLS on, serving `certificate`
/// and `key`.
///
/// # Arguments
/// * `certificate` - the server certificate (PEM).
/// * `key` - its private key (PEM).
fn tls_request(certificate: &str, key: &str) -> ContainerRequest<Postgres> {
    pg::postgres_17()
        .with_copy_to("/tmp/server.crt", certificate.as_bytes().to_vec())
        .with_copy_to("/tmp/server.key", key.as_bytes().to_vec())
        .with_cmd(["sh", "-c", START_WITH_TLS])
}

/// Starts `postgres:17-alpine` with TLS on, serving `certificate` and `key`,
/// through the shared helper, so a failed start leaves no container behind.
///
/// # Arguments
/// * `certificate` - the server certificate (PEM).
/// * `key` - its private key (PEM).
///
/// # Returns
/// The container (dropping it stops the container) and its host port.
async fn postgres_with_tls(certificate: &str, key: &str) -> (ContainerAsync<Postgres>, u16) {
    let container = pg::start_with_retry(|| tls_request(certificate, key)).await;
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("the container's port");
    (container, port)
}

/// Writes `pem` to a temporary file, which is removed when the handle drops
/// (Rust runs the cleanup in `Drop`, like a context manager's `__exit__`).
fn pem_file(pem: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("a temporary file");
    file.write_all(pem.as_bytes()).expect("write the CA");
    file
}

/// A URL for the container on `port` with `sslmode=verify-full`, trusting
/// the CA certificate in the file `root`.
fn verify_full_url(port: u16, root: &std::path::Path) -> String {
    format!(
        "postgres://postgres:postgres@127.0.0.1:{port}/postgres\
         ?sslmode=verify-full&sslrootcert={}",
        root.display()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_full_connects_encrypted_to_a_server_its_ca_signed() {
    let ca = new_ca("okf-mcp test CA");
    let (certificate, key) = server_certificate(&ca);
    let (_container, port) = postgres_with_tls(&certificate, &key).await;
    let root = pem_file(&ca.pem);

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&verify_full_url(port, root.path()))
        .await
        .expect("connect with verify-full");
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .expect("read pg_stat_ssl");

    assert!(encrypted, "the connection is not encrypted");
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_full_refuses_a_server_another_ca_signed() {
    let ca = new_ca("okf-mcp test CA");
    let stranger = new_ca("okf-mcp stranger CA");
    let (certificate, key) = server_certificate(&ca);
    let (_container, port) = postgres_with_tls(&certificate, &key).await;
    let root = pem_file(&stranger.pem);

    let refused = PgPoolOptions::new()
        .max_connections(1)
        .connect(&verify_full_url(port, root.path()))
        .await;

    // Refused for the certificate, not for any reason: a refused or dropped
    // connection must not pass. sqlx reports rustls's verdict as an I/O
    // error during the handshake, so the check reads rustls's name for it.
    // `{error:?}` is the `Debug` form (like Python's `repr`): if a dependency
    // bump changes the wording, the failure shows the whole error chain.
    let error = refused.expect_err("connected to a server the CA did not sign");
    assert!(
        error.to_string().contains("UnknownIssuer"),
        "refused for another reason than the certificate's issuer: {error} ({error:?})"
    );
}

/// A customized start (copied files, a command) that times out leaves no
/// container behind: Docker created it and copied the files before the
/// timeout fired, so without the cleanup it would leak.
#[tokio::test(flavor = "multi_thread")]
async fn a_timed_out_tls_start_leaves_no_container() {
    use testcontainers_modules::testcontainers::core::error::WaitContainerError;
    use testcontainers_modules::testcontainers::TestcontainersError;

    let ca = new_ca("okf-mcp timeout CA");
    let (certificate, key) = server_certificate(&ca);
    let label = pg::unique_start_label();
    let request = tls_request(&certificate, &key).with_startup_timeout(std::time::Duration::ZERO);

    let started = pg::start_labelled(request, &label).await;

    assert!(matches!(
        started,
        Err(TestcontainersError::WaitContainer(
            WaitContainerError::StartupTimeout
        ))
    ));
    assert!(pg::labelled_containers(&label).await.unwrap().is_empty());
}
