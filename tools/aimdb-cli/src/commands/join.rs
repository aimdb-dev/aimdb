//! Join Command — admit this machine to an AimDB deployment
//!
//! Speaks the open provisioning format of design 043 (`POST /v1/join`):
//! authenticate (GitHub device flow, or a pre-issued claim token), pass the
//! deployment's `app` metadata through untouched, and write the returned
//! station profile as `station.toml`. Generic by construction — the endpoint
//! URL is an argument and no deployment-specific types appear here.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use clap::Args;
use serde::{Deserialize, Serialize};

use crate::error::CliResult;

/// Public OAuth client ID of the flagship's GitHub App — public by design,
/// nothing secret ships in the CLI (design 042 §6.1). Deployments building
/// their own binaries bake their own app in at build time:
/// `AIMDB_GITHUB_CLIENT_ID=… cargo build`.
const GITHUB_CLIENT_ID: Option<&str> = option_env!("AIMDB_GITHUB_CLIENT_ID");

const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_USER_URL: &str = "https://api.github.com/user";

/// The station profile format this CLI understands (design 043 §3/§5).
const PROFILE_VERSION: u64 = 1;

/// `app` fields collected via prompts (design 043 §2.2). The values are
/// passed through untouched; the service validates and echoes the final
/// values back in the profile.
const APP_PROMPTS: &[(&str, &str)] = &[("name", "Station name"), ("location", "Location (city)")];

/// Join an AimDB deployment via its provisioning endpoint
#[derive(Debug, Args)]
pub struct JoinCommand {
    /// Base URL of the provisioning endpoint (the CLI POSTs to <URL>/v1/join)
    provisioning_url: String,

    /// Pre-issued claim token — selects the `claim-token` auth kind and skips
    /// the GitHub device flow (for deployments without public self-serve)
    #[arg(long)]
    token: Option<String>,

    /// Where to write the station profile (contains the broker credential;
    /// written with mode 0600)
    #[arg(long, default_value = "station.toml")]
    out: PathBuf,
}

/// Identity envelope of the join request (design 043 §2.1).
#[derive(Debug, Serialize)]
struct AuthEnvelope {
    kind: &'static str,
    token: String,
}

/// The station profile returned on success (design 043 §3.1). Unknown fields
/// are ignored on both ends so services can extend the format without a
/// version bump (043 §5).
#[derive(Debug, Serialize, Deserialize)]
struct StationProfile {
    profile_version: u64,
    station_id: String,
    broker: BrokerProfile,
    app: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BrokerProfile {
    url: String,
    username: String,
    password: String,
}

impl JoinCommand {
    pub async fn execute(self) -> CliResult<()> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("aimdb-cli/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;

        let auth = match self.token {
            Some(token) => AuthEnvelope {
                kind: "claim-token",
                token,
            },
            None => AuthEnvelope {
                kind: "github",
                token: github_device_flow(&client).await?,
            },
        };

        let mut app = serde_json::Map::new();
        for (key, label) in APP_PROMPTS {
            app.insert((*key).to_string(), prompt(label)?.into());
        }

        let profile = request_join(&client, &self.provisioning_url, &auth, &app).await?;

        if profile.profile_version > PROFILE_VERSION {
            eprintln!(
                "⚠ profile_version {} is newer than this CLI understands ({}); writing it anyway",
                profile.profile_version, PROFILE_VERSION
            );
        }

        write_profile(&self.out, &profile)?;

        let name = profile
            .app
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("station");
        println!(
            "✓ {} \"{}\" — credentials written to {}",
            profile.station_id,
            name,
            self.out.display()
        );
        Ok(())
    }
}

/// GitHub OAuth device flow (design 042 §6.1): print a short code, the user
/// confirms it at github.com/login/device, poll for the access token. No
/// scopes are requested — public identity only.
async fn github_device_flow(client: &reqwest::Client) -> CliResult<String> {
    let client_id = GITHUB_CLIENT_ID.ok_or_else(|| {
        anyhow!(
            "this build has no GitHub OAuth client ID compiled in\n  \
             Hint: use --token <t> for claim-token deployments, or rebuild with \
             AIMDB_GITHUB_CLIENT_ID=<id> for GitHub self-serve"
        )
    })?;

    #[derive(Deserialize)]
    struct DeviceCode {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_in: u64,
        interval: u64,
    }

    let device: DeviceCode = client
        .post(GITHUB_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({ "client_id": client_id }))
        .send()
        .await
        .context("device-code request to GitHub failed")?
        .error_for_status()
        .context("device-code request to GitHub failed")?
        .json()
        .await
        .context("unexpected device-code response from GitHub")?;

    println!(
        "→ To authorize, enter code {} at {}",
        device.user_code, device.verification_uri
    );

    #[derive(Deserialize)]
    struct TokenPoll {
        access_token: Option<String>,
        error: Option<String>,
        interval: Option<u64>,
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(device.expires_in);
    let mut interval = device.interval.max(1);
    let token = loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if std::time::Instant::now() > deadline {
            return Err(
                anyhow!("device code expired before authorization — run join again").into(),
            );
        }

        let poll: TokenPoll = client
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": client_id,
                "device_code": device.device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }))
            .send()
            .await
            .context("access-token poll to GitHub failed")?
            .json()
            .await
            .context("unexpected access-token response from GitHub")?;

        if let Some(token) = poll.access_token {
            break token;
        }
        match poll.error.as_deref() {
            Some("authorization_pending") => {}
            // GitHub asks us to back off; it may supply the new interval.
            Some("slow_down") => interval = poll.interval.unwrap_or(interval + 5),
            Some("expired_token") => {
                return Err(
                    anyhow!("device code expired before authorization — run join again").into(),
                )
            }
            Some("access_denied") => {
                return Err(anyhow!("authorization was denied on GitHub").into())
            }
            other => {
                return Err(anyhow!(
                    "GitHub device flow failed: {}",
                    other.unwrap_or("no error code in response")
                )
                .into())
            }
        }
    };

    // Cosmetic only — the service re-verifies the identity server-side.
    #[derive(Deserialize)]
    struct GithubUser {
        login: String,
    }
    if let Ok(resp) = client
        .get(GITHUB_USER_URL)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(&token)
        .send()
        .await
    {
        if let Ok(user) = resp.json::<GithubUser>().await {
            println!("✓ authorized as @{}", user.login);
        }
    }

    Ok(token)
}

/// `POST <base>/v1/join` and interpret the response per design 043.
async fn request_join(
    client: &reqwest::Client,
    base_url: &str,
    auth: &AuthEnvelope,
    app: &serde_json::Map<String, serde_json::Value>,
) -> CliResult<StationProfile> {
    let url = format!("{}/v1/join", base_url.trim_end_matches('/'));
    let response = client
        .post(&url)
        .json(&serde_json::json!({ "auth": auth, "app": app }))
        .send()
        .await
        .with_context(|| format!("request to {url} failed"))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read response from {url}"))?;
    parse_join_response(status, &body)
}

/// Interpret a `/v1/join` response (design 043 §3). On 403/409/503 the body's
/// `message` is surfaced as-is — the service owns the failure UX text.
fn parse_join_response(status: u16, body: &str) -> CliResult<StationProfile> {
    #[derive(Deserialize)]
    struct ErrorBody {
        message: String,
    }

    match status {
        200 => serde_json::from_str(body)
            .context("malformed station profile in join response")
            .map_err(Into::into),
        403 | 409 | 503 => {
            let message = serde_json::from_str::<ErrorBody>(body)
                .map(|e| e.message)
                .unwrap_or_else(|_| format!("join rejected with status {status}"));
            Err(anyhow!("{message}").into())
        }
        other => Err(anyhow!(
            "unexpected response from provisioning endpoint (status {other}): {}",
            body.trim()
        )
        .into()),
    }
}

/// Write the profile as TOML (design 043 §4), mode 0600 — it carries the
/// broker credential.
fn write_profile(path: &PathBuf, profile: &StationProfile) -> CliResult<()> {
    let toml =
        toml::to_string_pretty(profile).context("failed to serialize station profile as TOML")?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(toml.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Prompt on stdin for one `app` field; re-asks until non-empty.
fn prompt(label: &str) -> CliResult<String> {
    loop {
        print!("{label}: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let value = line.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The design 043 §3.1 sample response.
    const PROFILE_JSON: &str = r#"{
        "profile_version": 1,
        "station_id": "slot-17",
        "broker": {
            "url": "mqtts://xxxx.eu-central-1.emqx.cloud:8883",
            "username": "station-17",
            "password": "s3cret"
        },
        "app": { "name": "graz-balcony", "lat": 47.07, "lon": 15.44 }
    }"#;

    #[test]
    fn profile_survives_json_to_toml_round_trip() {
        let profile = parse_join_response(200, PROFILE_JSON).unwrap();
        let toml_str = toml::to_string_pretty(&profile).unwrap();

        let reread: StationProfile = toml::from_str(&toml_str).unwrap();
        assert_eq!(reread.profile_version, 1);
        assert_eq!(reread.station_id, "slot-17");
        assert_eq!(reread.broker.username, "station-17");
        assert_eq!(reread.broker.password, "s3cret");
        assert_eq!(
            reread.app.get("name").and_then(|v| v.as_str()),
            Some("graz-balcony")
        );
        assert_eq!(reread.app.get("lat").and_then(|v| v.as_f64()), Some(47.07));
    }

    #[test]
    fn unknown_profile_fields_are_ignored() {
        // 043 §5: services may add fields without a version bump.
        let body = PROFILE_JSON.replacen(
            "\"profile_version\": 1,",
            "\"profile_version\": 1, \"issued_at\": \"2026-07-09\",",
            1,
        );
        let profile = parse_join_response(200, &body).unwrap();
        assert_eq!(profile.station_id, "slot-17");
    }

    #[test]
    fn rejection_surfaces_service_message_as_is() {
        for status in [403, 409, 503] {
            let err = parse_join_response(
                status,
                r#"{ "message": "The mesh is full (64/64 slots) — try again later." }"#,
            )
            .unwrap_err();
            assert_eq!(
                err.to_string(),
                "The mesh is full (64/64 slots) — try again later."
            );
        }
    }

    #[test]
    fn rejection_without_json_body_still_reports_status() {
        let err = parse_join_response(503, "upstream timeout").unwrap_err();
        assert!(err.to_string().contains("503"), "got: {err}");
    }

    #[test]
    fn unexpected_status_is_an_error() {
        let err = parse_join_response(500, "oops").unwrap_err();
        assert!(err.to_string().contains("500"), "got: {err}");
    }

    #[test]
    fn malformed_success_body_is_an_error() {
        let err = parse_join_response(200, r#"{ "hello": 1 }"#).unwrap_err();
        assert!(err.to_string().contains("profile"), "got: {err}");
    }

    #[test]
    fn profile_written_with_owner_only_permissions() {
        let profile = parse_join_response(200, PROFILE_JSON).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("station.toml");
        write_profile(&path, &profile).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let reread: StationProfile =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reread.station_id, "slot-17");
    }

    /// One-shot HTTP server: accepts a single connection, reads the request,
    /// answers with the canned status/body, then closes.
    async fn mock_join_server(status_line: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Drain the request headers + body far enough to respond politely.
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn join_request_round_trips_against_mock_server() {
        let base = mock_join_server("200 OK", PROFILE_JSON).await;
        let client = reqwest::Client::new();
        let auth = AuthEnvelope {
            kind: "claim-token",
            token: "t".into(),
        };
        let profile = request_join(&client, &base, &auth, &serde_json::Map::new())
            .await
            .unwrap();
        assert_eq!(profile.station_id, "slot-17");
    }

    #[tokio::test]
    async fn join_request_surfaces_mock_rejection_message() {
        let base = mock_join_server(
            "409 Conflict",
            r#"{ "message": "Your account already runs graz-balcony (slot-17)." }"#,
        )
        .await;
        let client = reqwest::Client::new();
        let auth = AuthEnvelope {
            kind: "claim-token",
            token: "t".into(),
        };
        let err = request_join(&client, &base, &auth, &serde_json::Map::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Your account already runs graz-balcony (slot-17)."
        );
    }
}
