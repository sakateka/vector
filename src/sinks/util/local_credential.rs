use std::{collections::BTreeMap, sync::Arc};

use reqwest::Url;
use serde_json::Value;
use vector_lib::configurable::configurable_component;

fn default_ticket_path() -> String {
    "/v2/tickets".to_owned()
}

/// Fetches short-lived credentials from a local HTTP issuer and injects them into outbound requests.
///
/// The issuer is expected to accept `GET` requests with `self` and `dsts` query parameters and
/// return JSON shaped like `{ "<destination>": { "ticket": "<credential>" } }`.
#[configurable_component]
#[derive(Clone, Debug)]
pub struct LocalCredentialConfig {
    /// Base URL of the local credential issuer (scheme, host, and port).
    ///
    /// # Examples
    ///
    /// - `http://127.0.0.1:8080`
    /// - `http://localhost:8080`
    #[configurable(metadata(docs::examples = "http://127.0.0.1:8080"))]
    pub endpoint: String,

    /// HTTP path on the issuer used to request credentials.
    #[serde(default = "default_ticket_path")]
    pub path: String,

    /// Source identity sent as the `self` query parameter.
    pub source: String,

    /// Destination identity sent as the `dsts` query parameter and used as the top-level JSON key
    /// when reading the response.
    pub destination: String,

    /// Request metadata / header name used to attach the fetched credential.
    #[configurable(metadata(docs::examples = "x-custom-auth"))]
    pub header: String,

    /// Optional environment variable whose value is sent as the `Authorization` header when
    /// requesting credentials from the issuer.
    pub authorization_env: Option<String>,

    /// Additional HTTP headers to send when requesting credentials from the issuer.
    #[serde(default)]
    #[configurable(metadata(
        docs::additional_props_description = "An HTTP header name and value."
    ))]
    pub issuer_request_headers: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct LocalCredentialProvider {
    client: reqwest::Client,
    request_url: String,
    destination: String,
    header_name: String,
    grpc_metadata_key: tonic::metadata::AsciiMetadataKey,
    authorization_env: Option<String>,
    issuer_request_headers: reqwest::header::HeaderMap,
}

impl LocalCredentialProvider {
    pub fn try_from_config(config: &LocalCredentialConfig) -> crate::Result<Self> {
        let mut url = Url::parse(&config.endpoint)
            .map_err(|e| format!("invalid local credential issuer endpoint: {e}"))?;
        url.set_path(&config.path);
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("self", &config.source);
            pairs.append_pair("dsts", &config.destination);
        }

        http::header::HeaderName::from_bytes(config.header.as_bytes()).map_err(|e| {
            format!("invalid credential header name {:?}: {e}", config.header)
        })?;

        let header_lower = config.header.to_lowercase();
        let grpc_metadata_key =
            tonic::metadata::AsciiMetadataKey::from_bytes(header_lower.as_bytes()).map_err(|e| {
                format!(
                    "credential header {:?} is not valid gRPC metadata: {e}",
                    config.header
                )
            })?;

        let mut issuer_request_headers = reqwest::header::HeaderMap::new();
        for (name, value) in &config.issuer_request_headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                format!("invalid issuer request header name {name:?}: {e}")
            })?;
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                format!("invalid issuer request header value for {name:?}: {e}")
            })?;
            issuer_request_headers.insert(name, value);
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .map_err(|e| format!("failed to build HTTP client for local credential issuer: {e}"))?;

        Ok(Self {
            client,
            request_url: url.to_string(),
            destination: config.destination.clone(),
            header_name: config.header.clone(),
            grpc_metadata_key,
            authorization_env: config.authorization_env.clone(),
            issuer_request_headers,
        })
    }

    pub fn header_name(&self) -> &str {
        &self.header_name
    }

    /// Fetches a credential and returns it as gRPC metadata.
    ///
    /// The metadata key is validated at sink build time; only the value is validated here because
    /// it is issued at runtime.
    pub async fn fetch_grpc_metadata(
        &self,
    ) -> crate::Result<(
        tonic::metadata::AsciiMetadataKey,
        tonic::metadata::AsciiMetadataValue,
    )> {
        let value = self.fetch().await?;
        let metadata_value = tonic::metadata::AsciiMetadataValue::try_from(value.as_str())
            .map_err(|e| {
                format!(
                    "credential value for header {:?} is not valid ASCII gRPC metadata: {e}",
                    self.header_name
                )
            })?;
        Ok((self.grpc_metadata_key.clone(), metadata_value))
    }

    pub async fn fetch(&self) -> crate::Result<String> {
        let mut request = self
            .client
            .get(&self.request_url)
            .headers(self.issuer_request_headers.clone());

        if let Some(env_name) = &self.authorization_env {
            match std::env::var(env_name) {
                Ok(value) => {
                    request = request.header("Authorization", value);
                }
                Err(err) => {
                    return Err(format!(
                        "local credential issuer authorization env var {env_name:?} is not set: {err}"
                    )
                    .into());
                }
            }
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("failed to request credential from local issuer: {e}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "local credential issuer returned HTTP {}",
                response.status()
            )
            .into());
        }

        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse local credential issuer response: {e}"))?;

        body.get(&self.destination)
            .and_then(|entry| entry.get("ticket"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                format!(
                    "local credential issuer response is missing \
                     destination {:?} ticket field",
                    self.destination
                )
                .into()
            })
    }
}

pub type SharedLocalCredentialProvider = Arc<LocalCredentialProvider>;

/// HTTP stream sinks implement this on their per-request metadata type so a local credential can be
/// injected before the request is built.
pub trait LocalCredentialRequestMetadata: Send {
    fn insert_request_header(&mut self, name: &str, value: String);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_request_url() {
        let provider = LocalCredentialProvider::try_from_config(&LocalCredentialConfig {
            endpoint: "http://127.0.0.1:8080".to_owned(),
            path: "/v2/tickets".to_owned(),
            source: "my-service".to_owned(),
            destination: "remote-service".to_owned(),
            header: "x-service-credential".to_owned(),
            authorization_env: None,
            issuer_request_headers: BTreeMap::new(),
        })
        .unwrap();

        assert_eq!(
            provider.request_url,
            "http://127.0.0.1:8080/v2/tickets?self=my-service&dsts=remote-service"
        );
    }

    #[test]
    fn parses_ticket_from_response() {
        let destination = "remote-service";
        let body: Value = serde_json::json!({
            destination: {
                "ticket": "abc123"
            }
        });

        let ticket = body
            .get(destination)
            .and_then(|entry| entry.get("ticket"))
            .and_then(Value::as_str)
            .unwrap();

        assert_eq!(ticket, "abc123");
    }
}
