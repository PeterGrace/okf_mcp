//! OAuth clients (spec "Client ID Metadata Documents", "`POST /oauth/register`"):
//! dynamic registration (RFC 7591), the CIMD fetcher, and [`resolve`], which
//! turns an authorize request's `client_id` into a [`ClientRecord`] either
//! way. Every client is public: no secret, PKCE required.
//!
//! The fetcher is the server's one outbound request driven by a stranger's
//! input (a `client_id` URL), so it is hardened against server-side request
//! forgery: HTTPS only, no redirects, no proxy, a 5-second deadline, a
//! 16 KiB body cap, literal IP hosts checked before sending, and a DNS
//! resolver that refuses non-public addresses. reqwest connects to exactly
//! the addresses that resolver returns, so the check cannot be raced by DNS
//! rebinding: the connection is pinned to the addresses that were checked.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, SubsecRound, TimeDelta, Utc};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use url::{Host, Url};

use crate::app::App;
use crate::error::OkfError;
use crate::keyed_lock::KeyedLocks;
use crate::oauth::error::OAuthError;
use crate::oauth::OAuth;
use crate::store::oauth::clients::{self as client_rows, ClientKind, ClientRecord, NewDcrClient};

/// The whole-request deadline of a CIMD fetch, body included.
pub const CIMD_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest CIMD document read, in bytes.
pub const CIMD_MAX_BYTES: usize = 16 * 1024;
/// The shortest a fetched document is trusted before a refetch.
pub const CACHE_FLOOR: Duration = Duration::from_secs(5 * 60);
/// The longest a fetched document is trusted, and how long after its fetch
/// a cached one may stand in for a failed refetch.
pub const CACHE_CEILING: Duration = Duration::from_secs(24 * 60 * 60);
/// Registration is refused while this many DCR clients have no grant.
pub const MAX_UNBOUND_DCR: i64 = 500;
/// The most redirect URIs one client may register.
pub const MAX_REDIRECT_URIS: usize = 10;
/// The longest redirect URI accepted, in characters.
pub const MAX_URI_CHARS: usize = 2000;
/// The longest `client_name` accepted, in characters.
pub const MAX_CLIENT_NAME_CHARS: usize = 200;
/// The largest registration request body.
const REGISTER_BODY_LIMIT: usize = 16 * 1024;

/// Is `ip` an ordinary public unicast address, safe for the server to
/// connect to on a stranger's behalf? Refuses loopback, private, link-local,
/// unique-local, shared (CGNAT), documentation, benchmarking, multicast,
/// broadcast, reserved and unspecified space, and judges IPv4-mapped, NAT64
/// and 6to4 IPv6 forms by the IPv4 address inside.
///
/// # Arguments
/// * `ip` - a resolved or literal address.
///
/// # Returns
/// `true` only for an address the fetcher may connect to.
pub fn is_public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// [`is_public_address`] for IPv4. Core's `is_global` is unstable, so the
/// special ranges are written out.
fn is_public_v4(ip: Ipv4Addr) -> bool {
    // `octets()` is the address as four bytes; the array pattern names them.
    let [a, b, c, _] = ip.octets();
    let special = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0 // 0.0.0.0/8 "this network"
        || (a == 100 && (64..=127).contains(&b)) // 100.64.0.0/10 shared (CGNAT)
        || (a == 192 && b == 0 && c == 0) // 192.0.0.0/24 protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // 198.18.0.0/15 benchmarking
        || a >= 240; // 240.0.0.0/4 reserved (and broadcast)
    !special
}

/// [`is_public_address`] for IPv6.
fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let segments = ip.segments();
    // The IPv4 address in two 16-bit segments.
    let embedded = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    // 64:ff9b::/96, the NAT64 well-known prefix: the last 32 bits are IPv4.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return is_public_v4(embedded(segments[6], segments[7]));
    }
    // 2002::/16, 6to4: the next 32 bits are IPv4.
    if segments[0] == 0x2002 {
        return is_public_v4(embedded(segments[1], segments[2]));
    }
    let special = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unique_local() // fc00::/7
        || ip.is_unicast_link_local() // fe80::/10
        || segments[..6] == [0; 6] // ::/96, deprecated IPv4-compatible
        || (segments[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local (deprecated)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8) // 2001:db8::/32 documentation
        || segments[..4] == [0x100, 0, 0, 0]; // 100::/64 discard
    !special
}

/// Is this a loopback host, as written in a URL?
fn is_loopback_host(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// Checks one redirect URI: `https` with a host, or `http` on a loopback
/// host (RFC 8252 native clients); no fragment, no user information, at
/// most [`MAX_URI_CHARS`] characters. OAuth 2.1 compares redirect URIs
/// exactly, so nothing here normalizes.
///
/// # Arguments
/// * `uri` - the URI as the client sent it.
///
/// # Returns
/// `Ok(())` for an acceptable URI.
///
/// # Errors
/// [`OAuthError::InvalidRedirectUri`] naming what is wrong.
pub fn validate_redirect_uri(uri: &str) -> Result<(), OAuthError> {
    let refuse = |why: &str| Err(OAuthError::InvalidRedirectUri(format!("{uri:?}: {why}")));
    if uri.chars().count() > MAX_URI_CHARS {
        return refuse("too long");
    }
    // `let ... else`: bind on success, otherwise run the block (which must
    // leave the function), like `if (m := parse(uri)) is None: return ...`.
    let Ok(parsed) = Url::parse(uri) else {
        return refuse("not an absolute URL");
    };
    if parsed.fragment().is_some() {
        return refuse("a redirect URI must not have a fragment");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return refuse("a redirect URI must not carry user information");
    }
    match (parsed.scheme(), parsed.host()) {
        ("https", Some(_)) => Ok(()),
        ("http", Some(host)) if is_loopback_host(&host) => Ok(()),
        ("http", Some(_)) => refuse("http is allowed only on a loopback host"),
        _ => refuse("must be https (or http on a loopback host)"),
    }
}

/// Client metadata after validation: what both kinds of client keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMetadata {
    /// Its display name, if it gave one.
    pub client_name: Option<String>,
    /// Its redirect URIs, each checked by [`validate_redirect_uri`].
    pub redirect_uris: Vec<String>,
    /// The grant types it asked for that this server supports:
    /// `authorization_code`, and optionally `refresh_token`.
    pub grant_types: Vec<String>,
    /// `["code"]`.
    pub response_types: Vec<String>,
}

/// The fields of RFC 7591 client metadata this server reads. Unknown fields
/// (`logo_uri`, `scope`, ...) are ignored, as RFC 7591 section 2 allows.
#[derive(Debug, Deserialize)]
struct ClientMetadata {
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
    // Not an RFC 7591 field, but clients that can authenticate several ways
    // publish it in their metadata document: ChatGPT lists `none` here while
    // declaring `private_key_jwt` above (issue #273).
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// The grant types this server issues tokens for.
const SUPPORTED_GRANT_TYPES: [&str; 2] = ["authorization_code", "refresh_token"];

/// Validates client metadata, from a registration request or a CIMD
/// document.
///
/// # Arguments
/// * `document` - the JSON as received.
///
/// # Returns
/// The fields the server keeps, with RFC 7591's defaults filled in
/// (`grant_types` `["authorization_code"]`, `response_types` `["code"]`).
///
/// # Errors
/// [`OAuthError::InvalidClientMetadata`] for anything but a JSON object, or a
/// shape, auth method, grant or response type this server does not support;
/// [`OAuthError::InvalidRedirectUri`] for a bad redirect URI.
pub fn validate_metadata(document: &Value) -> Result<ValidatedMetadata, OAuthError> {
    let invalid = |why: String| OAuthError::InvalidClientMetadata(why);
    // RFC 7591 metadata is a JSON object. Checked first because serde's
    // derived struct deserializer would also accept an array holding the
    // fields in declaration order.
    if !document.is_object() {
        return Err(invalid("client metadata must be a JSON object".to_string()));
    }
    // `T::deserialize(&Value)` reads a `Value` already in memory, without
    // re-serializing it (serde_json implements `Deserializer` for `&Value`).
    let metadata = ClientMetadata::deserialize(document)
        .map_err(|error| invalid(format!("client metadata: {error}")))?;
    if metadata.redirect_uris.is_empty() || metadata.redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err(invalid(format!(
            "redirect_uris must list 1 to {MAX_REDIRECT_URIS} URIs"
        )));
    }
    metadata
        .redirect_uris
        .iter()
        .try_for_each(|uri| validate_redirect_uri(uri))?;
    // Only public clients: the client must be able to use `none`, either as
    // its declared method or among the methods it says it supports. One that
    // also supports another method reads this server's metadata, which offers
    // only `none`, and authenticates that way; it is treated exactly as a
    // public client (PKCE, an exact redirect URI, no client secret).
    let offers_none = metadata
        .token_endpoint_auth_methods_supported
        .as_ref()
        .is_some_and(|methods| methods.iter().any(|method| method == "none"));
    match metadata.token_endpoint_auth_method.as_deref() {
        None | Some("none") => {}
        Some(_) if offers_none => {}
        Some(other) => {
            return Err(invalid(format!(
                "token_endpoint_auth_method {other:?} is not supported; \
                 only public clients (\"none\")"
            )))
        }
    }
    // RFC 7591 section 2 lets the server drop grant types it does not
    // support. Keep the supported ones, in the order given; the client must
    // still be able to start with an authorization code.
    let grant_types: Vec<String> = metadata
        .grant_types
        .unwrap_or_else(|| vec!["authorization_code".to_string()])
        .into_iter()
        .filter(|grant| SUPPORTED_GRANT_TYPES.contains(&grant.as_str()))
        .collect();
    if !grant_types
        .iter()
        .any(|grant| grant == "authorization_code")
    {
        return Err(invalid(
            "grant_types must include authorization_code (refresh_token is also supported)"
                .to_string(),
        ));
    }
    let response_types = metadata
        .response_types
        .unwrap_or_else(|| vec!["code".to_string()]);
    if response_types != ["code"] {
        return Err(invalid("response_types must be [\"code\"]".to_string()));
    }
    if let Some(name) = &metadata.client_name {
        if name.chars().count() > MAX_CLIENT_NAME_CHARS || name.trim().is_empty() {
            return Err(invalid(format!(
                "client_name must be 1 to {MAX_CLIENT_NAME_CHARS} characters"
            )));
        }
    }
    Ok(ValidatedMetadata {
        client_name: metadata.client_name,
        redirect_uris: metadata.redirect_uris,
        grant_types,
        response_types,
    })
}

/// Parses a CIMD `client_id`: an `https` URL (or `http`, on a test rig that
/// allows it) in canonical form, with a path, without a fragment or user
/// information. Canonical form means the text equals the parser's own
/// serialization, which rules out dot segments, an upper-case host and
/// other spellings of the same URL; the document must name the identical
/// string.
///
/// # Arguments
/// * `client_id` - the `client_id` an authorize request carried.
/// * `allow_http` - also accept `http` (test rigs only).
///
/// # Returns
/// The parsed URL.
///
/// # Errors
/// [`OAuthError::InvalidClient`].
pub fn cimd_url(client_id: &str, allow_http: bool) -> Result<Url, OAuthError> {
    let refuse = |why: &str| {
        Err(OAuthError::InvalidClient(format!(
            "client_id {client_id:?} is not a valid metadata document URL: {why}"
        )))
    };
    let Ok(url) = Url::parse(client_id) else {
        return refuse("not a URL");
    };
    if url.as_str() != client_id {
        return refuse("not in canonical form");
    }
    match url.scheme() {
        "https" => {}
        "http" if allow_http => {}
        _ => return refuse("must be https"),
    }
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return refuse("must not have a fragment or user information");
    }
    if url.path() == "/" || url.path().is_empty() {
        return refuse("must have a path");
    }
    Ok(url)
}

/// Validates a fetched Client ID Metadata Document.
///
/// # Arguments
/// * `url` - the URL it was fetched from (the `client_id`).
/// * `document` - its JSON.
///
/// # Returns
/// Its metadata.
///
/// # Errors
/// [`OAuthError::InvalidClient`] when its `client_id` is not exactly `url`,
/// or for any metadata problem (the authorize request names the client, so
/// a broken document is an invalid client, not a registration error).
pub fn validate_cimd_document(
    url: &str,
    document: &Value,
) -> Result<ValidatedMetadata, OAuthError> {
    if document.get("client_id").and_then(Value::as_str) != Some(url) {
        return Err(OAuthError::InvalidClient(format!(
            "the metadata document at {url:?} does not name itself as its client_id"
        )));
    }
    validate_metadata(document).map_err(|error| {
        OAuthError::InvalidClient(format!("the metadata document at {url:?}: {error}"))
    })
}

/// How long to trust a fetched document: its `Cache-Control: max-age`,
/// clamped to [[`CACHE_FLOOR`], [`CACHE_CEILING`]]. `no-store` or `no-cache`,
/// a missing header or an unreadable one means the floor.
///
/// # Arguments
/// * `headers` - the response headers.
///
/// # Returns
/// The lifetime.
pub fn cache_lifetime(headers: &HeaderMap) -> Duration {
    let mut max_age = None;
    // A response may carry several `Cache-Control` headers; directives are
    // comma-separated and case-insensitive (RFC 9111 section 5.2).
    for value in headers.get_all(header::CACHE_CONTROL) {
        let Ok(text) = value.to_str() else { continue };
        for directive in text.split(',').map(str::trim) {
            let lower = directive.to_ascii_lowercase();
            if lower == "no-store" || lower == "no-cache" {
                return CACHE_FLOOR;
            }
            if let Some(seconds) = lower.strip_prefix("max-age=") {
                max_age = seconds.trim_matches('"').parse::<u64>().ok();
            }
        }
    }
    max_age
        .map(Duration::from_secs)
        .unwrap_or(CACHE_FLOOR)
        .clamp(CACHE_FLOOR, CACHE_CEILING)
}

/// Why a CIMD fetch failed. Shown to the person on the authorize error page
/// (inside an [`OAuthError::InvalidClient`]), so no variant carries more
/// than a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CimdError {
    /// The host resolved to, or is, a non-public address.
    #[error("the metadata document's host is not a public address")]
    PrivateAddress,
    /// The host has no address.
    #[error("the metadata document's host does not resolve")]
    Unresolvable,
    /// Anything but 200 (redirects included: they are not followed).
    #[error("the metadata document answered HTTP {0}")]
    Status(u16),
    /// Over [`CIMD_MAX_BYTES`].
    #[error("the metadata document is larger than {CIMD_MAX_BYTES} bytes")]
    TooLarge,
    /// Not JSON.
    #[error("the metadata document is not JSON")]
    NotJson,
    /// The deadline passed.
    #[error("the metadata document took longer than {} seconds", CIMD_TIMEOUT.as_secs())]
    Timeout,
    /// Any other transport failure (TLS, connection refused, a plain-http
    /// URL when only https is allowed).
    #[error("the metadata document could not be fetched")]
    Transport,
}

/// The resolver's refusal of a non-public address; recognized again in
/// reqwest's error chain.
#[derive(Debug, thiserror::Error)]
#[error("refused a non-public address")]
struct RefusedAddress;

/// The resolver's report that a name has no address.
#[derive(Debug, thiserror::Error)]
#[error("the name has no address")]
struct NoAddress;

/// A future resolving one host name to its addresses (any port).
// `Pin<Box<dyn Future ...>>`: a heap-allocated future of a type erased at
// compile time, so different lookups fit one function-pointer type.
type LookupFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;

/// How the guarded resolver looks a name up: the system resolver in
/// production, a fixed answer in tests.
type Lookup = fn(String) -> LookupFuture;

/// The system resolver (`getaddrinfo`, through tokio).
fn system_lookup(host: String) -> LookupFuture {
    Box::pin(async move {
        // Port 0: reqwest substitutes the URL's port.
        Ok(tokio::net::lookup_host((host.as_str(), 0)).await?.collect())
    })
}

/// The fetcher's DNS resolver: a lookup, then a refusal if any address is
/// non-public (unless private addresses are allowed). Refusing on *any* bad
/// address, rather than filtering, means a name that mixes public and
/// private addresses is never fetched at all. reqwest connects only to the
/// addresses this returns, which pins the connection to the checked ones.
#[derive(Debug, Clone, Copy)]
struct GuardedResolver {
    allow_private: bool,
    lookup: Lookup,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        // Copied out of `self`: the future must own what it uses
        // (`'static`), since reqwest may poll it after this call returns.
        let GuardedResolver {
            allow_private,
            lookup,
        } = *self;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = lookup(host).await.unwrap_or_default();
            if addrs.is_empty() {
                return Err(NoAddress.into());
            }
            if !allow_private && addrs.iter().any(|addr| !is_public_address(addr.ip())) {
                return Err(RefusedAddress.into());
            }
            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// A fetched document and how long to trust it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    /// The parsed JSON.
    pub document: Value,
    /// From `Cache-Control`, clamped ([`cache_lifetime`]).
    pub lifetime: Duration,
}

/// Fetches Client ID Metadata Documents, one fetch per URL at a time.
pub struct CimdFetcher {
    client: reqwest::Client,
    allow_private: bool,
    allow_http: bool,
    // Single-flight: concurrent resolves of one URL queue here, and all but
    // the first find the fresh cache row when they get the lock.
    in_flight: KeyedLocks,
}

// Written by hand: `reqwest::Client`'s own `Debug` is long and says nothing
// about the guard settings.
impl std::fmt::Debug for CimdFetcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CimdFetcher")
            .field("allow_private", &self.allow_private)
            .field("allow_http", &self.allow_http)
            .finish_non_exhaustive()
    }
}

impl CimdFetcher {
    /// Builds the hardened HTTP client.
    ///
    /// # Arguments
    /// * `allow_private` - accept non-public addresses (`cimd_allow_private`,
    ///   test rigs only).
    /// * `allow_http` - accept `http` URLs (only with `cimd_allow_private` and
    ///   a loopback `http` `public_url`; see `OAuth::new`).
    ///
    /// # Returns
    /// The fetcher.
    ///
    /// # Errors
    /// [`OkfError::Config`] if the TLS backend cannot be initialized.
    pub fn new(allow_private: bool, allow_http: bool) -> Result<Self, OkfError> {
        Self::with_lookup(allow_private, allow_http, system_lookup)
    }

    /// [`CimdFetcher::new`] with a chosen name lookup (tests pin names to
    /// fixed addresses with it).
    fn with_lookup(
        allow_private: bool,
        allow_http: bool,
        lookup: Lookup,
    ) -> Result<Self, OkfError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CIMD_TIMEOUT)
            // A proxy would resolve and connect to the host itself, around
            // the guard (and `HTTPS_PROXY` comes from the environment).
            .no_proxy()
            .https_only(!allow_http)
            .dns_resolver(GuardedResolver {
                allow_private,
                lookup,
            })
            .user_agent(concat!("okf-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| OkfError::Config {
                path: "<oauth>".to_string(),
                message: format!("building the CIMD client: {error}"),
            })?;
        Ok(Self {
            client,
            allow_private,
            allow_http,
            in_flight: KeyedLocks::new(),
        })
    }

    /// Whether `http` CIMD URLs are accepted (test rigs only).
    ///
    /// # Returns
    /// The `allow_http` it was built with.
    pub fn allows_http(&self) -> bool {
        self.allow_http
    }

    /// Fetches one document. Does not validate it ([`validate_cimd_document`]).
    ///
    /// # Arguments
    /// * `url` - a URL [`cimd_url`] accepted.
    ///
    /// # Returns
    /// The JSON and its cache lifetime.
    ///
    /// # Errors
    /// A [`CimdError`] for each way the fetch can fail.
    pub async fn fetch(&self, url: &Url) -> Result<Fetched, CimdError> {
        // A literal IP host is connected to directly, never resolved, so the
        // guard in the resolver would not see it.
        let literal = match url.host() {
            Some(Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            Some(Host::Domain(_)) | None => None,
        };
        if literal.is_some_and(|ip| !self.allow_private && !is_public_address(ip)) {
            return Err(CimdError::PrivateAddress);
        }
        let mut response = self
            .client
            .get(url.clone())
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(classify_transport)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(CimdError::Status(response.status().as_u16()));
        }
        // `usize` to `u64` is lossless on every platform Rust supports.
        if response
            .content_length()
            .is_some_and(|length| length > CIMD_MAX_BYTES as u64)
        {
            return Err(CimdError::TooLarge);
        }
        let lifetime = cache_lifetime(response.headers());
        // Read chunk by chunk and stop at the cap, so a server that omits its
        // length still cannot make us buffer more.
        let mut body = Vec::with_capacity(CIMD_MAX_BYTES.min(4096));
        while let Some(chunk) = response.chunk().await.map_err(classify_transport)? {
            if body.len() + chunk.len() > CIMD_MAX_BYTES {
                return Err(CimdError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let document = serde_json::from_slice(&body).map_err(|_| CimdError::NotJson)?;
        Ok(Fetched { document, lifetime })
    }
}

/// Maps a reqwest failure to a [`CimdError`], finding the resolver's own
/// errors in the error's source chain.
fn classify_transport(error: reqwest::Error) -> CimdError {
    if error.is_timeout() {
        return CimdError::Timeout;
    }
    // `std::iter::successors` walks `source()` links: Python's
    // `while e := e.__cause__`.
    let chain = std::iter::successors(
        Some(&error as &(dyn std::error::Error + 'static)),
        |current| current.source(),
    );
    for cause in chain {
        if cause.is::<RefusedAddress>() {
            return CimdError::PrivateAddress;
        }
        if cause.is::<NoAddress>() {
            return CimdError::Unresolvable;
        }
    }
    CimdError::Transport
}

/// The single-flight key for a URL: the first 8 bytes of its SHA-256. A
/// collision only makes two URLs take turns.
fn flight_key(url: &str) -> i64 {
    let digest = crate::keys::hash(url);
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(first)
}

/// Turns an authorize request's `client_id` into its client: an `https`
/// URL is a CIMD client (fetched, or from the cache while fresh); anything
/// else must be a registered DCR client.
///
/// # Arguments
/// * `app` - the application (the database and clock).
/// * `oauth` - the OAuth state (the fetcher).
/// * `client_id` - as the request carried it.
///
/// # Returns
/// The client.
///
/// # Errors
/// [`OAuthError::InvalidClient`] for an unknown DCR id, a bad CIMD URL, a
/// failed fetch with no usable cached copy, or an invalid document;
/// [`OAuthError::Server`] for a database failure.
pub async fn resolve(
    app: &App,
    oauth: &OAuth,
    client_id: &str,
) -> Result<ClientRecord, OAuthError> {
    let looks_like_url = client_id.starts_with("https://")
        || (oauth.cimd.allows_http() && client_id.starts_with("http://"));
    if looks_like_url {
        return resolve_cimd(app, oauth, client_id).await;
    }
    match client_rows::find(app.pool(), client_id).await? {
        Some(client) if client.kind == ClientKind::Dcr => Ok(client),
        Some(_) | None => Err(OAuthError::InvalidClient(format!(
            "unknown client_id {client_id:?}"
        ))),
    }
}

/// Is a cached CIMD row still fresh at `now`?
fn is_fresh(client: &ClientRecord, now: DateTime<Utc>) -> bool {
    client.cache_expires_at.is_some_and(|expires| now < expires)
}

/// May a cached CIMD row stand in for a failed refetch at `now`? Up to
/// [`CACHE_CEILING`] after it was fetched.
fn is_usable_stale(client: &ClientRecord, now: DateTime<Utc>) -> bool {
    let ceiling = TimeDelta::from_std(CACHE_CEILING).unwrap_or(TimeDelta::MAX);
    client
        .fetched_at
        .is_some_and(|fetched| now < fetched + ceiling)
}

/// The CIMD half of [`resolve`].
async fn resolve_cimd(
    app: &App,
    oauth: &OAuth,
    client_id: &str,
) -> Result<ClientRecord, OAuthError> {
    let url = cimd_url(client_id, oauth.cimd.allows_http())?;
    let cached = client_rows::find(app.pool(), client_id).await?;
    if let Some(client) = &cached {
        if is_fresh(client, app.clock().now()) {
            return Ok(client.clone());
        }
    }
    // Held until this function returns: other resolves of the same URL wait.
    let _flight = oauth.cimd.in_flight.lock(flight_key(client_id)).await;
    // Another resolve may have refreshed it while this one waited.
    let cached = match client_rows::find(app.pool(), client_id).await? {
        Some(client) if is_fresh(&client, app.clock().now()) => return Ok(client),
        other => other.or(cached),
    };
    // Whole microseconds, as Postgres stores them, so the record returned
    // equals the one a later cache hit reads back.
    let now = app.clock().now().trunc_subsecs(6);
    let fetched = match oauth.cimd.fetch(&url).await {
        Ok(fetched) => fetched,
        Err(error) => {
            return match cached {
                Some(client) if is_usable_stale(&client, now) => {
                    tracing::warn!(
                        client = %client_id,
                        %error,
                        "CIMD refetch failed; using the cached document"
                    );
                    Ok(client)
                }
                Some(_) | None => Err(OAuthError::InvalidClient(error.to_string())),
            };
        }
    };
    let metadata = validate_cimd_document(client_id, &fetched.document)?;
    let lifetime = TimeDelta::from_std(fetched.lifetime).unwrap_or(TimeDelta::MAX);
    let client = ClientRecord {
        client_id: client_id.to_string(),
        kind: ClientKind::Cimd,
        client_name: metadata.client_name,
        redirect_uris: metadata.redirect_uris,
        metadata: fetched.document,
        fetched_at: Some(now),
        cache_expires_at: Some(now + lifetime),
        // `Option::map_or`: the cached row's first sighting, or now.
        created_at: cached.map_or(now, |client| client.created_at),
    };
    client_rows::upsert_cimd(app.pool(), &client).await?;
    Ok(client)
}

/// The registration route. Mounted with the other OAuth routes; the handler
/// answers 404 when DCR is off (`dcr_enabled = false`), as if the route did
/// not exist.
///
/// # Returns
/// `POST /oauth/register`, with a 16 KiB body limit.
pub fn routes() -> Router<Arc<App>> {
    Router::new().route(
        "/oauth/register",
        post(register).layer(DefaultBodyLimit::max(REGISTER_BODY_LIMIT)),
    )
}

/// `POST /oauth/register` (RFC 7591).
///
/// The body is parsed by hand rather than with the `Json` extractor, so a
/// malformed body is an RFC 7591 `invalid_client_metadata` error, not
/// axum's plain-text 400 or 422. `Cache-Control: no-store` comes from the
/// layer in [`crate::oauth::routes`].
async fn register(State(app): State<Arc<App>>, body: Bytes) -> Result<Response, OAuthError> {
    let Some(oauth) = app.oauth().filter(|oauth| oauth.settings.dcr_enabled) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    if !oauth.register_limit.try_take(Instant::now()) {
        return Err(OAuthError::SlowDown);
    }
    let document: Value = serde_json::from_slice(&body).map_err(|_| {
        OAuthError::InvalidClientMetadata("the body must be a JSON object".to_string())
    })?;
    let metadata = validate_metadata(&document)?;
    let now = app.clock().now();
    // The metadata as registered, which the response echoes; `client_name`
    // only when the client sent one.
    let mut stored = Map::new();
    if let Some(name) = &metadata.client_name {
        stored.insert("client_name".to_string(), json!(name));
    }
    stored.insert("redirect_uris".to_string(), json!(metadata.redirect_uris));
    stored.insert("grant_types".to_string(), json!(metadata.grant_types));
    stored.insert("response_types".to_string(), json!(metadata.response_types));
    stored.insert("token_endpoint_auth_method".to_string(), json!("none"));
    let stored = Value::Object(stored);
    // `None`: the cap of unbound clients is reached (checked atomically with
    // the insert, so concurrent registrations cannot pass it).
    let Some(client) = client_rows::insert_dcr(
        app.pool(),
        &NewDcrClient {
            client_name: metadata.client_name.as_deref(),
            redirect_uris: &metadata.redirect_uris,
            metadata: &stored,
        },
        now,
        MAX_UNBOUND_DCR,
    )
    .await?
    else {
        tracing::warn!("DCR refused: {MAX_UNBOUND_DCR} registered clients have no grant");
        return Err(OAuthError::SlowDown);
    };
    let mut answer = stored;
    // `as_object_mut` borrows the JSON object's map mutably; it is always an
    // object here, since it was built from a `Map` just above.
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("client_id".to_string(), Value::String(client.client_id));
        fields.insert("client_id_issued_at".to_string(), json!(now.timestamp()));
    }
    Ok((StatusCode::CREATED, Json(answer)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use axum::response::Redirect;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use crate::config::{Config, OAuthConfig};
    use crate::mcp::testing::{self, TestApp};

    fn v4(text: &str) -> IpAddr {
        IpAddr::V4(text.parse::<Ipv4Addr>().expect("v4"))
    }

    fn v6(text: &str) -> IpAddr {
        IpAddr::V6(text.parse::<Ipv6Addr>().expect("v6"))
    }

    #[test]
    fn only_public_addresses_are_public() {
        // Documentation ranges are refused too, so these "public" examples
        // use ordinary unicast space that no test ever connects to.
        for public in ["8.8.4.4", "1.1.1.1"] {
            assert!(is_public_address(v4(public)), "{public}");
        }
        for public in [
            "2606:4700::1111",
            "::ffff:8.8.4.4",
            "64:ff9b::808:404",
            "2002:808:404::1",
        ] {
            assert!(is_public_address(v6(public)), "{public}");
        }
        for refused in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!is_public_address(v4(refused)), "{refused}");
        }
        for refused in [
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "100::1",
            // IPv4-mapped, NAT64, 6to4 and IPv4-compatible forms of private addresses.
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::1",
            "::7f00:1",
        ] {
            assert!(!is_public_address(v6(refused)), "{refused}");
        }
    }

    #[test]
    fn redirect_uris_are_https_or_loopback_http() {
        for good in [
            "https://client.example.com/callback",
            "https://client.example.com:8443/cb?x=1",
            "http://127.0.0.1:33418/callback",
            "http://localhost/callback",
            "http://[::1]:9000/cb",
        ] {
            assert!(validate_redirect_uri(good).is_ok(), "{good}");
        }
        let long = format!("https://client.example.com/{}", "a".repeat(MAX_URI_CHARS));
        for bad in [
            "http://client.example.com/callback",
            "https://client.example.com/cb#frag",
            "https://user:pw@client.example.com/cb",
            "javascript:alert(1)",
            "cursor://callback",
            "/relative",
            "",
            &long,
        ] {
            assert!(
                matches!(
                    validate_redirect_uri(bad),
                    Err(OAuthError::InvalidRedirectUri(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn max_age_is_clamped_to_five_minutes_and_a_day() {
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CACHE_CONTROL, value.parse().expect("header"));
            headers
        };

        assert_eq!(cache_lifetime(&HeaderMap::new()), CACHE_FLOOR);
        assert_eq!(
            cache_lifetime(&header("max-age=3600")),
            Duration::from_secs(3600)
        );
        assert_eq!(
            cache_lifetime(&header("public, MAX-AGE=7200")),
            Duration::from_secs(7200)
        );
        assert_eq!(cache_lifetime(&header("max-age=10")), CACHE_FLOOR);
        assert_eq!(cache_lifetime(&header("max-age=999999")), CACHE_CEILING);
        assert_eq!(cache_lifetime(&header("no-store")), CACHE_FLOOR);
        assert_eq!(
            cache_lifetime(&header("no-cache, max-age=3600")),
            CACHE_FLOOR
        );
        assert_eq!(cache_lifetime(&header("max-age=junk")), CACHE_FLOOR);
    }

    #[test]
    fn a_client_that_also_offers_none_is_accepted_as_public() {
        // ChatGPT's own document (https://chatgpt.com/oauth/client.json,
        // 2026-11-11), verbatim: it declares private_key_jwt but lists `none`
        // among the methods it supports, and asks for one grant this server
        // does not offer (issue #273).
        let chatgpt = json!({
            "client_id": "https://chatgpt.com/oauth/client.json",
            "client_uri": "https://chatgpt.com/",
            "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"],
            "token_endpoint_auth_method": "private_key_jwt",
            "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
            "grant_types": [
                "authorization_code",
                "refresh_token",
                "urn:ietf:params:oauth:grant-type:jwt-bearer"
            ],
            "response_types": ["code"],
            "client_name": "ChatGPT",
            "logo_uri": "https://persistent.oaistatic.com/sonic/misc/openai-logo.png",
            "token_endpoint_auth_signing_alg": "RS256",
            "jwks_uri": "https://chatgpt.com/oauth/jwks.json"
        });

        let accepted = validate_cimd_document("https://chatgpt.com/oauth/client.json", &chatgpt)
            .expect("ChatGPT's document is a public client");

        assert_eq!(accepted.client_name.as_deref(), Some("ChatGPT"));
        // Only the grants this server supports are kept.
        assert_eq!(
            accepted.grant_types,
            ["authorization_code", "refresh_token"]
        );

        let cb = "https://client.example.com/cb";
        for (bad, why) in [
            (
                json!({
                    "redirect_uris": [cb],
                    "token_endpoint_auth_method": "private_key_jwt",
                    "token_endpoint_auth_methods_supported": ["private_key_jwt"]
                }),
                "does not offer none",
            ),
            (
                json!({
                    "redirect_uris": [cb],
                    "token_endpoint_auth_method": "private_key_jwt",
                    "token_endpoint_auth_methods_supported": "none"
                }),
                "supported methods not a list",
            ),
            (
                json!({
                    "redirect_uris": [cb],
                    "grant_types": ["refresh_token", "urn:ietf:params:oauth:grant-type:jwt-bearer"]
                }),
                "no authorization_code",
            ),
        ] {
            assert!(
                matches!(
                    validate_metadata(&bad),
                    Err(OAuthError::InvalidClientMetadata(_))
                ),
                "{why}: {bad}"
            );
        }
    }

    #[test]
    fn metadata_validation_accepts_public_clients_only() {
        let valid = validate_metadata(&json!({
            "client_name": "Example",
            "redirect_uris": ["https://client.example.com/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "logo_uri": "https://client.example.com/logo.png"
        }))
        .expect("valid");
        assert_eq!(valid.client_name.as_deref(), Some("Example"));
        assert_eq!(valid.grant_types, ["authorization_code", "refresh_token"]);

        let defaults = validate_metadata(&json!({
            "redirect_uris": ["https://client.example.com/callback"]
        }))
        .expect("defaults");
        assert_eq!(defaults.grant_types, ["authorization_code"]);
        assert_eq!(defaults.response_types, ["code"]);
        assert_eq!(defaults.client_name, None);

        let too_many: Vec<String> = (0..=MAX_REDIRECT_URIS)
            .map(|n| format!("https://client.example.com/cb{n}"))
            .collect();
        let cb = "https://client.example.com/cb";
        for (bad, why) in [
            (json!({}), "no redirect_uris"),
            (json!({ "redirect_uris": [] }), "empty redirect_uris"),
            (
                json!({ "redirect_uris": too_many }),
                "too many redirect_uris",
            ),
            (json!({ "redirect_uris": cb }), "not a list"),
            (
                json!({ "redirect_uris": [cb], "token_endpoint_auth_method": "client_secret_basic" }),
                "confidential",
            ),
            (
                json!({ "redirect_uris": [cb], "grant_types": ["client_credentials"] }),
                "grant type",
            ),
            (
                json!({ "redirect_uris": [cb], "grant_types": [] }),
                "no grant type",
            ),
            (
                json!({ "redirect_uris": [cb], "response_types": ["token"] }),
                "response type",
            ),
            (
                json!({ "redirect_uris": [cb], "client_name": "x".repeat(MAX_CLIENT_NAME_CHARS + 1) }),
                "long name",
            ),
            (
                json!({ "redirect_uris": [cb], "client_name": "  " }),
                "blank name",
            ),
            (json!(["not", "an", "object"]), "not an object"),
        ] {
            assert!(
                matches!(
                    validate_metadata(&bad),
                    Err(OAuthError::InvalidClientMetadata(_))
                ),
                "{why}"
            );
        }
        let bad_uri =
            validate_metadata(&json!({ "redirect_uris": ["http://client.example.com/cb"] }));
        assert!(matches!(bad_uri, Err(OAuthError::InvalidRedirectUri(_))));
    }

    /// Client metadata as a well-typed positional array, in `ClientMetadata`'s
    /// field order: serde's derived struct deserializer accepts that shape.
    fn positional_metadata() -> Value {
        json!([
            "Example",
            ["https://client.example.com/cb"],
            null,
            null,
            null
        ])
    }

    #[test]
    fn metadata_must_be_a_json_object() {
        let positional = positional_metadata();

        assert!(
            matches!(
                validate_metadata(&positional),
                Err(OAuthError::InvalidClientMetadata(_))
            ),
            "registration metadata"
        );
        assert!(
            matches!(
                validate_cimd_document("https://client.example.com/c.json", &positional),
                Err(OAuthError::InvalidClient(_))
            ),
            "a metadata document"
        );
    }

    #[test]
    fn cimd_urls_must_be_canonical_https_with_a_path() {
        assert!(cimd_url("https://client.example.com/oauth/client.json", false).is_ok());
        assert!(cimd_url("http://127.0.0.1:9000/client.json", true).is_ok());
        for bad in [
            "http://client.example.com/client.json",
            "https://client.example.com",
            "https://client.example.com/",
            "https://client.example.com/a/../client.json",
            "https://CLIENT.example.com/client.json",
            "https://client.example.com/client.json#x",
            "https://u:p@client.example.com/client.json",
            "not a url",
        ] {
            assert!(
                matches!(cimd_url(bad, false), Err(OAuthError::InvalidClient(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_cimd_document_must_name_its_own_url() {
        let url = "https://client.example.com/oauth/client.json";
        let document = json!({
            "client_id": url,
            "client_name": "Example",
            "redirect_uris": ["https://client.example.com/callback"]
        });
        assert!(validate_cimd_document(url, &document).is_ok());

        for bad in [
            json!({
                "client_id": "https://client.example.com/other.json",
                "redirect_uris": ["https://client.example.com/callback"]
            }),
            json!({ "redirect_uris": ["https://client.example.com/callback"] }),
            json!({ "client_id": url, "redirect_uris": [] }),
            json!({
                "client_id": url,
                "redirect_uris": ["https://client.example.com/callback"],
                "token_endpoint_auth_method": "private_key_jwt"
            }),
        ] {
            assert!(
                matches!(
                    validate_cimd_document(url, &bad),
                    Err(OAuthError::InvalidClient(_))
                ),
                "{bad}"
            );
        }
    }

    /// A local CIMD host: its base URL, and counters the tests read.
    struct CimdHost {
        base: String,
        port: u16,
        hits: Arc<AtomicUsize>,
        failing: Arc<AtomicBool>,
    }

    /// Serves `/client.json` (a valid document naming its own URL, cached
    /// for an hour, counted, 500 while `failing`), `/mismatch.json` (names
    /// another URL), `/big.json` (over the cap, with a `Content-Length`),
    /// `/text` (not JSON) and `/redirect` (307 to `/client.json`).
    async fn cimd_host() -> CimdHost {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("address").port();
        let base = format!("http://127.0.0.1:{port}");
        let hits = Arc::new(AtomicUsize::new(0));
        let failing = Arc::new(AtomicBool::new(false));
        let client_url = format!("{base}/client.json");
        let (counter, failure) = (Arc::clone(&hits), Arc::clone(&failing));
        let router = Router::new()
            .route(
                "/client.json",
                // `move` closures own their captures; each request clones the
                // `Arc`s it needs, as Python closures would share them.
                get(move || {
                    let (counter, failure, client_url) = (
                        Arc::clone(&counter),
                        Arc::clone(&failure),
                        client_url.clone(),
                    );
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        // Slow enough that concurrent resolves overlap.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        if failure.load(Ordering::SeqCst) {
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                        (
                            [(header::CACHE_CONTROL, "max-age=3600")],
                            Json(json!({
                                "client_id": client_url,
                                "client_name": "Example Client",
                                "redirect_uris": ["https://client.example.com/callback"]
                            })),
                        )
                            .into_response()
                    }
                }),
            )
            .route(
                "/mismatch.json",
                get(|| async {
                    Json(json!({
                        "client_id": "https://client.example.com/other.json",
                        "redirect_uris": ["https://client.example.com/callback"]
                    }))
                }),
            )
            .route("/big.json", get(|| async { padded_json(CIMD_MAX_BYTES) }))
            .route("/text", get(|| async { "not json" }))
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/client.json") }),
            );
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        CimdHost {
            base,
            port,
            hits,
            failing,
        }
    }

    /// A valid JSON object longer than `size` bytes.
    fn padded_json(size: usize) -> String {
        format!("{{\"pad\": \"{}\"}}", "x".repeat(size))
    }

    /// A raw HTTP/1.1 server for responses axum will not send: it writes
    /// `head`, waits `delay`, writes `body` and closes, once per connection.
    /// The head carries no `Content-Length`, so the body runs until close.
    async fn raw_host(body: String, delay: Duration) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    // Read the request head first: closing a socket with
                    // unread input sends a reset, not the response.
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                                connection: close\r\n\r\n";
                    // Write errors only mean the client hung up first.
                    let _ = socket.write_all(head.as_bytes()).await;
                    tokio::time::sleep(delay).await;
                    let _ = socket.write_all(body.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        base
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("url")
    }

    /// Every name is 127.0.0.1 (reqwest fills in the URL's port).
    fn loopback_lookup(_host: String) -> LookupFuture {
        Box::pin(async { Ok(vec![SocketAddr::from(([127, 0, 0, 1], 0))]) })
    }

    /// Every name is 127.0.0.1 and a public address.
    fn mixed_lookup(_host: String) -> LookupFuture {
        Box::pin(async {
            Ok(vec![
                SocketAddr::from(([127, 0, 0, 1], 0)),
                SocketAddr::from(([8, 8, 4, 4], 0)),
            ])
        })
    }

    /// No name has an address.
    fn empty_lookup(_host: String) -> LookupFuture {
        Box::pin(async { Ok(Vec::new()) })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_reads_a_document_and_its_lifetime() {
        let host = cimd_host().await;
        let fetcher = CimdFetcher::new(true, true).expect("fetcher");

        let fetched = fetcher
            .fetch(&url(&format!("{}/client.json", host.base)))
            .await
            .expect("fetched");

        assert_eq!(fetched.document["client_name"], "Example Client");
        assert_eq!(fetched.lifetime, Duration::from_secs(3600));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_refuses_redirects_oversize_and_non_json() {
        let host = cimd_host().await;
        let streamed = raw_host(padded_json(CIMD_MAX_BYTES), Duration::ZERO).await;
        let fetcher = CimdFetcher::new(true, true).expect("fetcher");

        let redirected = fetcher
            .fetch(&url(&format!("{}/redirect", host.base)))
            .await;
        let big = fetcher
            .fetch(&url(&format!("{}/big.json", host.base)))
            .await;
        // No `Content-Length`: only the cap on the bytes read stops it.
        let big_streamed = fetcher.fetch(&url(&format!("{streamed}/c.json"))).await;
        let text = fetcher.fetch(&url(&format!("{}/text", host.base))).await;
        let missing = fetcher.fetch(&url(&format!("{}/nothing", host.base))).await;

        assert_eq!(redirected, Err(CimdError::Status(307)));
        assert_eq!(
            host.hits.load(Ordering::SeqCst),
            0,
            "the redirect was not followed"
        );
        assert_eq!(big, Err(CimdError::TooLarge));
        assert_eq!(big_streamed, Err(CimdError::TooLarge));
        assert_eq!(text, Err(CimdError::NotJson));
        assert_eq!(missing, Err(CimdError::Status(404)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_gives_up_after_its_deadline() {
        // The head arrives at once and the body after the deadline: the
        // deadline covers reading the body too, not just the response head.
        let slow = raw_host("{}".to_string(), CIMD_TIMEOUT + Duration::from_secs(2)).await;
        let fetcher = CimdFetcher::new(true, true).expect("fetcher");
        let started = std::time::Instant::now();

        let fetched = fetcher.fetch(&url(&format!("{slow}/c.json"))).await;

        assert_eq!(fetched, Err(CimdError::Timeout));
        assert!(started.elapsed() < CIMD_TIMEOUT + Duration::from_secs(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_refuses_private_addresses_by_name_and_literal() {
        let host = cimd_host().await;
        let port = host.port;
        let guarded = CimdFetcher::new(false, true).expect("fetcher");
        let mixed = CimdFetcher::with_lookup(false, true, mixed_lookup).expect("fetcher");

        let by_name = guarded
            .fetch(&url(&format!("http://localhost:{port}/client.json")))
            .await;
        let by_literal = guarded
            .fetch(&url(&format!("http://127.0.0.1:{port}/client.json")))
            .await;
        let by_mapped = guarded
            .fetch(&url(&format!(
                "http://[::ffff:127.0.0.1]:{port}/client.json"
            )))
            .await;
        // One private address among public ones refuses the name outright.
        let by_mixed_name = mixed
            .fetch(&url(&format!(
                "http://client.example.com:{port}/client.json"
            )))
            .await;

        assert_eq!(by_name, Err(CimdError::PrivateAddress));
        assert_eq!(by_literal, Err(CimdError::PrivateAddress));
        assert_eq!(by_mapped, Err(CimdError::PrivateAddress));
        assert_eq!(by_mixed_name, Err(CimdError::PrivateAddress));
        assert_eq!(
            host.hits.load(Ordering::SeqCst),
            0,
            "nothing reached the host"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_connects_only_to_the_addresses_its_resolver_checked() {
        let host = cimd_host().await;
        // `.invalid` never resolves in the system resolver (RFC 6761), so a
        // fetch that succeeds connected to the guarded resolver's answer.
        let pinned = CimdFetcher::with_lookup(true, true, loopback_lookup).expect("fetcher");
        let unresolvable = CimdFetcher::with_lookup(false, true, empty_lookup).expect("fetcher");
        let target = format!("http://client.invalid:{}/client.json", host.port);

        let fetched = pinned.fetch(&url(&target)).await;
        let nowhere = unresolvable.fetch(&url(&target)).await;

        assert!(fetched.is_ok(), "{fetched:?}");
        assert_eq!(host.hits.load(Ordering::SeqCst), 1);
        assert_eq!(nowhere, Err(CimdError::Unresolvable));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_allow_http_plain_http_is_refused() {
        let host = cimd_host().await;
        let fetcher = CimdFetcher::new(true, false).expect("fetcher");

        let fetched = fetcher
            .fetch(&url(&format!("{}/client.json", host.base)))
            .await;

        assert_eq!(fetched, Err(CimdError::Transport));
        assert_eq!(host.hits.load(Ordering::SeqCst), 0, "nothing was sent");
        assert!(cimd_url(&format!("{}/client.json", host.base), false).is_err());
    }

    fn rig_config(dcr_enabled: bool) -> Config {
        let mut config = crate::store::oauth::testing::oauth_config("http://127.0.0.1:8080");
        config.oauth = OAuthConfig {
            cimd_allow_private: true,
            dcr_enabled,
            ..OAuthConfig::default()
        };
        config
    }

    /// An app with OAuth on at a loopback `http` `public_url` and
    /// `cimd_allow_private`, which also allows `http` CIMD URLs.
    async fn rig(dcr_enabled: bool) -> TestApp {
        testing::app_with(&[("main", &[])], rig_config(dcr_enabled)).await
    }

    fn oauth(test: &TestApp) -> &OAuth {
        test.app.oauth().expect("OAuth is on")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_fetches_once_then_serves_the_cache() {
        let host = cimd_host().await;
        let test = rig(true).await;
        let client_id = format!("{}/client.json", host.base);

        let first = resolve(&test.app, oauth(&test), &client_id)
            .await
            .expect("first");
        let second = resolve(&test.app, oauth(&test), &client_id)
            .await
            .expect("second");

        assert_eq!(first.kind, ClientKind::Cimd);
        assert_eq!(first.client_name.as_deref(), Some("Example Client"));
        assert_eq!(first.redirect_uris, ["https://client.example.com/callback"]);
        let fetched = first.fetched_at.expect("fetched");
        assert_eq!(
            first.cache_expires_at,
            Some(fetched + TimeDelta::seconds(3600)),
            "max-age=3600"
        );
        assert_eq!(second, first);
        assert_eq!(host.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_resolves_of_one_url_fetch_once() {
        let host = cimd_host().await;
        let test = rig(true).await;
        let client_id = format!("{}/client.json", host.base);

        // `tokio::join!` polls all four futures at once on this task.
        let (a, b, c, d) = tokio::join!(
            resolve(&test.app, oauth(&test), &client_id),
            resolve(&test.app, oauth(&test), &client_id),
            resolve(&test.app, oauth(&test), &client_id),
            resolve(&test.app, oauth(&test), &client_id),
        );

        for result in [a, b, c, d] {
            assert!(result.is_ok(), "{result:?}");
        }
        assert_eq!(host.hits.load(Ordering::SeqCst), 1, "single-flight");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_cache_refetches_and_a_failed_refetch_serves_stale() {
        let host = cimd_host().await;
        let test = rig(true).await;
        let client_id = format!("{}/client.json", host.base);
        resolve(&test.app, oauth(&test), &client_id)
            .await
            .expect("first");
        let expire = || {
            sqlx::query("UPDATE oauth_clients SET cache_expires_at = now() - interval '1 second'")
                .execute(test.app.pool())
        };

        expire().await.expect("expire");
        resolve(&test.app, oauth(&test), &client_id)
            .await
            .expect("refetched");
        assert_eq!(host.hits.load(Ordering::SeqCst), 2);

        expire().await.expect("expire");
        host.failing.store(true, Ordering::SeqCst);
        let stale = resolve(&test.app, oauth(&test), &client_id).await;
        assert!(stale.is_ok(), "stale within a day: {stale:?}");
        assert_eq!(host.hits.load(Ordering::SeqCst), 3);

        sqlx::query("UPDATE oauth_clients SET fetched_at = now() - interval '25 hours'")
            .execute(test.app.pool())
            .await
            .expect("age");
        let too_old = resolve(&test.app, oauth(&test), &client_id).await;
        assert!(
            matches!(too_old, Err(OAuthError::InvalidClient(_))),
            "{too_old:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_refuses_mismatched_documents_and_unknown_ids() {
        let host = cimd_host().await;
        let test = rig(true).await;
        let mismatch_url = format!("{}/mismatch.json", host.base);

        let mismatch = resolve(&test.app, oauth(&test), &mismatch_url).await;
        let unknown = resolve(&test.app, oauth(&test), "no-such-client").await;

        assert!(
            matches!(mismatch, Err(OAuthError::InvalidClient(_))),
            "{mismatch:?}"
        );
        assert!(
            matches!(unknown, Err(OAuthError::InvalidClient(_))),
            "{unknown:?}"
        );
        assert!(
            client_rows::find(test.app.pool(), &mismatch_url)
                .await
                .expect("find")
                .is_none(),
            "an invalid document is not cached"
        );
    }

    /// Sends `POST /oauth/register` through every OAuth route, as
    /// `http::router` mounts them.
    async fn register_request(test: &TestApp, body: &str) -> (StatusCode, HeaderMap, Value) {
        let response = crate::oauth::routes()
            .with_state(Arc::clone(&test.app))
            .oneshot(
                Request::post("/oauth/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, json)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_stores_a_public_client_and_resolve_finds_it() {
        let test = rig(true).await;

        let (status, headers, body) = register_request(
            &test,
            r#"{"client_name": "Example", "redirect_uris": ["https://client.example.com/cb"],
                "grant_types": ["authorization_code", "refresh_token"]}"#,
        )
        .await;
        let (_, _, unnamed) = register_request(
            &test,
            r#"{"redirect_uris": ["https://client.example.com/cb"]}"#,
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(body["client_name"], "Example");
        assert_eq!(
            body["grant_types"],
            json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(body["token_endpoint_auth_method"], "none");
        assert!(body.get("client_secret").is_none());
        assert!(body["client_id_issued_at"].is_i64());
        assert!(unnamed.get("client_name").is_none(), "{unnamed}");
        let client_id = body["client_id"].as_str().expect("client_id");
        let found = resolve(&test.app, oauth(&test), client_id)
            .await
            .expect("found");
        assert_eq!(found.kind, ClientKind::Dcr);
        assert_eq!(found.client_name.as_deref(), Some("Example"));
        assert_eq!(found.redirect_uris, ["https://client.example.com/cb"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_refuses_bad_metadata_with_rfc_7591_errors() {
        let test = rig(true).await;

        for (body, code) in [
            ("not json", "invalid_client_metadata"),
            (r#"{"redirect_uris": []}"#, "invalid_client_metadata"),
            (
                r#"{"redirect_uris": ["https://client.example.com/cb"],
                    "token_endpoint_auth_method": "client_secret_post"}"#,
                "invalid_client_metadata",
            ),
            (
                r#"{"redirect_uris": ["http://client.example.com/cb"]}"#,
                "invalid_redirect_uri",
            ),
        ] {
            let (status, headers, json) = register_request(&test, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(json["error"], code, "{body}");
            assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{body}");
        }
        assert_eq!(
            client_rows::count_unbound_dcr(test.app.pool())
                .await
                .expect("count"),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_is_capped_at_500_unbound_clients() {
        let test = rig(true).await;
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             SELECT gen_random_uuid()::text, 'dcr', ARRAY['https://client.example.com/cb'], \
                    '{}'::jsonb, now() FROM generate_series(1, 499)",
        )
        .execute(test.app.pool())
        .await
        .expect("fill");
        let body = r#"{"redirect_uris": ["https://client.example.com/cb"]}"#;

        let (last, _, _) = register_request(&test, body).await;
        let (status, _, json) = register_request(&test, body).await;

        assert_eq!(last, StatusCode::CREATED, "the 500th is accepted");
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json["error"], "slow_down");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_registrations_never_pass_the_cap() {
        let test = rig(true).await;
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             SELECT gen_random_uuid()::text, 'dcr', ARRAY['https://client.example.com/cb'], \
                    '{}'::jsonb, now() FROM generate_series(1, 499)",
        )
        .execute(test.app.pool())
        .await
        .expect("fill");
        let body = r#"{"redirect_uris": ["https://client.example.com/cb"]}"#;

        // Eight requests at once (within the burst of 10), each on its own
        // tokio task, so their database round trips overlap.
        let mut requests = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let router = crate::oauth::routes().with_state(Arc::clone(&test.app));
            requests.spawn(async move {
                let request = Request::post("/oauth/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("request");
                router.oneshot(request).await.expect("infallible").status()
            });
        }
        let statuses = requests.join_all().await;

        let created = statuses
            .iter()
            .filter(|status| **status == StatusCode::CREATED)
            .count();
        assert_eq!(created, 1, "{statuses:?}");
        assert!(
            statuses.iter().all(|status| matches!(
                *status,
                StatusCode::CREATED | StatusCode::TOO_MANY_REQUESTS
            )),
            "{statuses:?}"
        );
        assert_eq!(
            client_rows::count_unbound_dcr(test.app.pool())
                .await
                .expect("count"),
            MAX_UNBOUND_DCR
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_is_rate_limited() {
        let test = rig(true).await;
        let body = r#"{"redirect_uris": ["https://client.example.com/cb"]}"#;

        let mut statuses = Vec::new();
        for _ in 0..11 {
            statuses.push(register_request(&test, body).await.0);
        }

        // Task 2's register bucket: a burst of 10, then one every 6 seconds.
        assert_eq!(statuses[..10], [StatusCode::CREATED; 10]);
        assert_eq!(statuses[10], StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_refuses_metadata_that_is_not_an_object() {
        let test = rig(true).await;

        let (status, _, json) = register_request(&test, &positional_metadata().to_string()).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["error"], "invalid_client_metadata");
        assert_eq!(
            client_rows::count_unbound_dcr(test.app.pool())
                .await
                .expect("count"),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_bodies_over_16_kib_are_refused() {
        let test = rig(true).await;
        let body = format!(
            r#"{{"redirect_uris": ["https://client.example.com/cb"], "pad": "{}"}}"#,
            "x".repeat(REGISTER_BODY_LIMIT)
        );

        let (status, _, _) = register_request(&test, &body).await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            client_rows::count_unbound_dcr(test.app.pool())
                .await
                .expect("count"),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_dcr_registration_is_404() {
        let test = rig(false).await;

        let (status, _, _) = register_request(
            &test,
            r#"{"redirect_uris": ["https://client.example.com/cb"]}"#,
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
