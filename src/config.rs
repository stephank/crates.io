use anyhow::{anyhow, Context};
use ipnetwork::IpNetwork;

use crate::publish_rate_limit::PublishRateLimit;
use crate::{env, env_optional, uploaders::Uploader, Env};

mod balance_capacity;
mod base;
mod database_pools;

pub use self::base::Base;
pub use self::database_pools::{DatabasePools, DbPoolConfig};
pub use crate::config::balance_capacity::BalanceCapacityConfig;
use http::HeaderValue;
use std::collections::HashSet;
use std::time::Duration;

const DEFAULT_VERSION_ID_CACHE_SIZE: u64 = 10_000;
const DEFAULT_VERSION_ID_CACHE_TTL: u64 = 5 * 60; // 5 minutes

pub struct Server {
    pub base: Base,
    pub db: DatabasePools,
    pub session_key: cookie::Key,
    pub gh_client_id: String,
    pub gh_client_secret: String,
    pub gh_base_url: String,
    pub max_upload_size: u64,
    pub max_unpack_size: u64,
    pub publish_rate_limit: PublishRateLimit,
    pub new_version_rate_limit: Option<u32>,
    pub blocked_traffic: Vec<(String, Vec<String>)>,
    pub max_allowed_page_offset: u32,
    pub page_offset_ua_blocklist: Vec<String>,
    pub page_offset_cidr_blocklist: Vec<IpNetwork>,
    pub excluded_crate_names: Vec<String>,
    pub domain_name: String,
    pub allowed_origins: AllowedOrigins,
    pub downloads_persist_interval_ms: usize,
    pub ownership_invitations_expiration_days: u64,
    pub metrics_authorization_token: Option<String>,
    pub use_test_database_pool: bool,
    pub instance_metrics_log_every_seconds: Option<u64>,
    pub force_unconditional_redirects: bool,
    pub blocked_routes: HashSet<String>,
    pub version_id_cache_size: u64,
    pub version_id_cache_ttl: Duration,
    pub cdn_user_agent: String,
    pub balance_capacity: BalanceCapacityConfig,
}

impl Default for Server {
    /// Returns a default value for the application's config
    ///
    /// Sets the following default values:
    ///
    /// - `Config::max_upload_size`: 10MiB
    /// - `Config::ownership_invitations_expiration_days`: 30
    ///
    /// Pulls values from the following environment variables:
    ///
    /// - `SESSION_KEY`: The key used to sign and encrypt session cookies.
    /// - `GH_CLIENT_ID`: The client ID of the associated GitHub application.
    /// - `GH_CLIENT_SECRET`: The client secret of the associated GitHub application.
    /// - `BLOCKED_TRAFFIC`: A list of headers and environment variables to use for blocking
    ///   traffic. See the `block_traffic` module for more documentation.
    /// - `DOWNLOADS_PERSIST_INTERVAL_MS`: how frequent to persist download counts (in ms).
    /// - `METRICS_AUTHORIZATION_TOKEN`: authorization token needed to query metrics. If missing,
    ///   querying metrics will be completely disabled.
    /// - `WEB_MAX_ALLOWED_PAGE_OFFSET`: Page offsets larger than this value are rejected. Defaults
    ///   to 200.
    /// - `WEB_PAGE_OFFSET_UA_BLOCKLIST`: A comma seperated list of user-agent substrings that will
    ///   be blocked if `WEB_MAX_ALLOWED_PAGE_OFFSET` is exceeded. Including an empty string in the
    ///   list will block *all* user-agents exceeding the offset. If not set or empty, no blocking
    ///   will occur.
    /// - `WEB_PAGE_OFFSET_CIDR_BLOCKLIST`: A comma separated list of CIDR blocks that will be used
    ///   to block IP addresses given in the `X-Real-Ip` HTTP header, e.g. `192.168.1.0/24`.
    ///   If not set or empty, no blocking will occur.
    /// - `INSTANCE_METRICS_LOG_EVERY_SECONDS`: How frequently should instance metrics be logged.
    ///   If the environment variable is not present instance metrics are not logged.
    /// - `FORCE_UNCONDITIONAL_REDIRECTS`: Whether to force unconditional redirects in the download
    ///   endpoint even with a healthy database pool.
    /// - `BLOCKED_ROUTES`: A comma separated list of HTTP route patterns that are manually blocked
    ///   by an operator (e.g. `/crates/:crate_id/:version/download`).
    ///
    /// # Panics
    ///
    /// This function panics if the Server configuration is invalid.
    fn default() -> Self {
        let allowed_origins = AllowedOrigins::from_default_env();
        let page_offset_ua_blocklist = match env_optional::<String>("WEB_PAGE_OFFSET_UA_BLOCKLIST")
        {
            None => vec![],
            Some(s) if s.is_empty() => vec![],
            Some(s) => s.split(',').map(String::from).collect(),
        };
        let page_offset_cidr_blocklist =
            match env_optional::<String>("WEB_PAGE_OFFSET_CIDR_BLOCKLIST") {
                None => vec![],
                Some(s) if s.is_empty() => vec![],
                Some(s) => s
                    .split(',')
                    .map(parse_cidr_block)
                    .collect::<Result<_, _>>()
                    .unwrap(),
            };

        let base = Base::from_environment();
        let excluded_crate_names = match env_optional::<String>("EXCLUDED_CRATE_NAMES") {
            None => vec![],
            Some(s) if s.is_empty() => vec![],
            Some(s) => s.split(',').map(String::from).collect(),
        };
        Server {
            db: DatabasePools::full_from_environment(&base),
            base,
            session_key: cookie::Key::derive_from(env("SESSION_KEY").as_bytes()),
            gh_client_id: env("GH_CLIENT_ID"),
            gh_client_secret: env("GH_CLIENT_SECRET"),
            gh_base_url: "https://api.github.com".to_string(),
            max_upload_size: 10 * 1024 * 1024, // 10 MB default file upload size limit
            max_unpack_size: 512 * 1024 * 1024, // 512 MB max when decompressed
            publish_rate_limit: Default::default(),
            new_version_rate_limit: env_optional("MAX_NEW_VERSIONS_DAILY"),
            blocked_traffic: blocked_traffic(),
            max_allowed_page_offset: env_optional("WEB_MAX_ALLOWED_PAGE_OFFSET").unwrap_or(200),
            page_offset_ua_blocklist,
            page_offset_cidr_blocklist,
            excluded_crate_names,
            domain_name: domain_name(),
            allowed_origins,
            downloads_persist_interval_ms: dotenv::var("DOWNLOADS_PERSIST_INTERVAL_MS")
                .map(|interval| {
                    interval
                        .parse()
                        .expect("invalid DOWNLOADS_PERSIST_INTERVAL_MS")
                })
                .unwrap_or(60_000), // 1 minute
            ownership_invitations_expiration_days: 30,
            metrics_authorization_token: dotenv::var("METRICS_AUTHORIZATION_TOKEN").ok(),
            use_test_database_pool: false,
            instance_metrics_log_every_seconds: env_optional("INSTANCE_METRICS_LOG_EVERY_SECONDS"),
            force_unconditional_redirects: dotenv::var("FORCE_UNCONDITIONAL_REDIRECTS").is_ok(),
            blocked_routes: env_optional("BLOCKED_ROUTES")
                .map(|routes: String| routes.split(',').map(|s| s.into()).collect())
                .unwrap_or_else(HashSet::new),
            version_id_cache_size: env_optional("VERSION_ID_CACHE_SIZE")
                .unwrap_or(DEFAULT_VERSION_ID_CACHE_SIZE),
            version_id_cache_ttl: Duration::from_secs(
                env_optional("VERSION_ID_CACHE_TTL").unwrap_or(DEFAULT_VERSION_ID_CACHE_TTL),
            ),
            cdn_user_agent: dotenv::var("WEB_CDN_USER_AGENT")
                .unwrap_or_else(|_| "Amazon CloudFront".into()),
            balance_capacity: BalanceCapacityConfig::from_environment(),
        }
    }
}

impl Server {
    pub fn env(&self) -> Env {
        self.base.env
    }

    pub fn uploader(&self) -> &Uploader {
        self.base.uploader()
    }
}

pub(crate) fn domain_name() -> String {
    dotenv::var("DOMAIN_NAME").unwrap_or_else(|_| "crates.io".into())
}

/// Parses a CIDR block string to a valid `IpNetwork` struct.
///
/// The purpose is to be able to block IP ranges that overload the API that uses pagination.
///
/// The minimum number of bits for a host prefix must be
///
/// * at least 16 for IPv4 based CIDRs.
/// * at least 64 for IPv6 based CIDRs
///
fn parse_cidr_block(block: &str) -> anyhow::Result<IpNetwork> {
    let cidr = block
        .parse()
        .context("WEB_PAGE_OFFSET_CIDR_BLOCKLIST must contain IPv4 or IPv6 CIDR blocks.")?;

    let host_prefix = match cidr {
        IpNetwork::V4(_) => 16,
        IpNetwork::V6(_) => 64,
    };

    if cidr.prefix() < host_prefix {
        return Err(anyhow!("WEB_PAGE_OFFSET_CIDR_BLOCKLIST only allows CIDR blocks with a host prefix of at least 16 bits (IPv4) or 64 bits (IPv6)."));
    }

    Ok(cidr)
}

fn blocked_traffic() -> Vec<(String, Vec<String>)> {
    let pattern_list = dotenv::var("BLOCKED_TRAFFIC").unwrap_or_default();
    parse_traffic_patterns(&pattern_list)
        .map(|(header, value_env_var)| {
            let value_list = dotenv::var(value_env_var).unwrap_or_default();
            let values = value_list.split(',').map(String::from).collect();
            (header.into(), values)
        })
        .collect()
}

fn parse_traffic_patterns(patterns: &str) -> impl Iterator<Item = (&str, &str)> {
    patterns.split_terminator(',').map(|pattern| {
        if let Some(idx) = pattern.find('=') {
            (&pattern[..idx], &pattern[(idx + 1)..])
        } else {
            panic!(
                "BLOCKED_TRAFFIC must be in the form HEADER=VALUE_ENV_VAR, \
                 got invalid pattern {pattern}"
            )
        }
    })
}

#[derive(Clone, Debug, Default)]
pub struct AllowedOrigins(Vec<String>);

impl AllowedOrigins {
    pub fn from_default_env() -> Self {
        let allowed_origins = env("WEB_ALLOWED_ORIGINS")
            .split(',')
            .map(ToString::to_string)
            .collect();

        Self(allowed_origins)
    }

    pub fn contains(&self, value: &HeaderValue) -> bool {
        self.0.iter().any(|it| it == value)
    }
}

#[test]
fn parse_traffic_patterns_splits_on_comma_and_looks_for_equal_sign() {
    let pattern_string_1 = "Foo=BAR,Bar=BAZ";
    let pattern_string_2 = "Baz=QUX";
    let pattern_string_3 = "";

    let patterns_1 = parse_traffic_patterns(pattern_string_1).collect::<Vec<_>>();
    assert_eq!(vec![("Foo", "BAR"), ("Bar", "BAZ")], patterns_1);

    let patterns_2 = parse_traffic_patterns(pattern_string_2).collect::<Vec<_>>();
    assert_eq!(vec![("Baz", "QUX")], patterns_2);

    assert_none!(parse_traffic_patterns(pattern_string_3).next());
}

#[test]
fn parse_cidr_block_list_successfully() {
    assert_ok_eq!(
        parse_cidr_block("127.0.0.1/24"),
        "127.0.0.1/24".parse::<IpNetwork>().unwrap()
    );
    assert_ok_eq!(
        parse_cidr_block("192.168.0.1/31"),
        "192.168.0.1/31".parse::<IpNetwork>().unwrap()
    );
}

#[test]
fn parse_cidr_blocks_panics_when_host_ipv4_prefix_is_too_low() {
    assert_err!(parse_cidr_block("127.0.0.1/8"));
}

#[test]
fn parse_cidr_blocks_panics_when_host_ipv6_prefix_is_too_low() {
    assert_err!(parse_cidr_block(
        "2001:0db8:0123:4567:89ab:cdef:1234:5678/56"
    ));
}

#[test]
fn parse_ipv6_based_cidr_blocks() {
    assert_ok_eq!(
        parse_cidr_block("2002::1234:abcd:ffff:c0a8:101/64"),
        "2002::1234:abcd:ffff:c0a8:101/64"
            .parse::<IpNetwork>()
            .unwrap()
    );
    assert_ok_eq!(
        parse_cidr_block("2001:0db8:0123:4567:89ab:cdef:1234:5678/92"),
        "2001:0db8:0123:4567:89ab:cdef:1234:5678/92"
            .parse::<IpNetwork>()
            .unwrap()
    );
}
